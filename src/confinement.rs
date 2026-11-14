//! Kernel-enforced writable roots for commands supplied by a project.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    #[serde(default)]
    pub writes: Vec<String>,
    #[serde(default = "required")]
    pub required: bool,
}

fn required() -> bool {
    true
}

/// Operator paths supplement the project, and either layer can require enforcement.
pub fn combine(destination: &mut Option<Declaration>, layer: Option<Declaration>) {
    if let Some(mut layer) = layer {
        if let Some(current) = destination {
            current.writes.append(&mut layer.writes);
            current.writes.sort();
            current.writes.dedup();
            current.required |= layer.required;
        } else {
            layer.writes.sort();
            layer.writes.dedup();
            *destination = Some(layer);
        }
    }
}

thread_local! {
    static DECLARATION: RefCell<Option<Option<Declaration>>> = const { RefCell::new(None) };
}

/// Carries destination authority through scratch population and preparation.
pub struct Scope(Option<Option<Declaration>>);
impl Scope {
    pub fn new(declaration: &Option<Declaration>) -> Self {
        Self(DECLARATION.with(|current| current.replace(Some(declaration.clone()))))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        DECLARATION.with(|current| current.replace(self.0.take()));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub confined: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abi: Option<u32>,
    pub writes: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

impl Receipt {
    pub fn describe(&self) -> String {
        if self.confined {
            format!(
                "ran confined (Landlock ABI {}); writable roots: {}",
                self.abi.unwrap_or(0),
                self.writes
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            format!(
                "ran unconfined (required = false): {}",
                self.unavailable
                    .as_deref()
                    .unwrap_or("Landlock unavailable")
            )
        }
    }
    pub fn failed(&self) {
        eprintln!("{}", self.describe());
    }
}

pub(crate) fn declaration(checkout: &Path) -> Result<Option<Declaration>> {
    if let Some(declaration) = DECLARATION.with(|current| current.borrow().clone()) {
        return Ok(declaration);
    }
    // Checkout hooks and slot commands also answer to an associated change's destination.
    let top = match crate::gitio::toplevel(checkout) {
        Ok(top) => top,
        Err(_) => return Ok(None),
    };
    let root = crate::store::Store::resolve_root(&top)?;
    if let Some(store) = crate::store::Store::open_at_for(&root, &top)? {
        if let Some(branch) = crate::gitio::current_branch(&top)? {
            let states = store.readable_states_shared()?;
            let owners = states
                .values()
                .filter(|s| !s.is_closed() && s.branch == branch)
                .collect::<Vec<_>>();
            if owners.len() > 1 {
                bail!("ambiguous confinement destination for branch {branch}");
            }
            if let Some(state) = owners.first() {
                let target = crate::declarations::authorizing_revision(&top, state, None)?;
                let mut destination = at(&top, &target)?;
                let head = crate::gitio::branch_head(&top, &state.branch)?;
                let own = at(&top, &head)?;
                if let Some(destination) = &mut destination {
                    destination.required |= own.as_ref().is_some_and(|own| own.required);
                } else {
                    destination = own;
                }
                return Ok(destination);
            }
        }
    }
    Ok(crate::gates::inspect(&top)?.confine)
}

#[derive(Deserialize)]
struct ConfineFile {
    confine: Option<Declaration>,
}

/// Read only confinement when opening a checkout, before gate validation is due.
pub(crate) fn at(cwd: &Path, revision: &str) -> Result<Option<Declaration>> {
    let text = crate::gitio::file_at(cwd, revision, ".arc/gates.toml")?;
    let mut declaration = text
        .map(|text| toml::from_str::<ConfineFile>(&text).map(|file| file.confine))
        .transpose()?
        .flatten();
    let operator = crate::policy::operator_path(cwd)?;
    match fs::read_to_string(operator) {
        Ok(text) => combine(
            &mut declaration,
            toml::from_str::<ConfineFile>(&text)?.confine,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(declaration)
}

/// Attach a prepared ruleset before exec; the parent retains its own authority.
pub fn apply(command: &mut Command, checkout: &Path) -> Result<Option<Receipt>> {
    let Some(declaration) = declaration(checkout)? else {
        return Ok(None);
    };
    apply_declared(command, checkout, &declaration)
}

pub fn apply_declared(
    command: &mut Command,
    checkout: &Path,
    declaration: &Declaration,
) -> Result<Option<Receipt>> {
    apply_with_abi(command, checkout, declaration, abi_version())
}

fn apply_with_abi(
    command: &mut Command,
    checkout: &Path,
    declaration: &Declaration,
    abi: Result<u32>,
) -> Result<Option<Receipt>> {
    let abi = match abi.and_then(check_abi) {
        Ok(abi) => abi,
        Err(error) => {
            if declaration.required {
                return Err(error).context("write confinement required; refusing project command");
            }
            let receipt = Receipt {
                confined: false,
                identity: None,
                abi: None,
                writes: Vec::new(),
                unavailable: Some(error.to_string()),
            };
            eprintln!("{}", receipt.describe());
            return Ok(Some(receipt));
        }
    };
    let checkout = fs::canonicalize(checkout)?;
    let storage = command
        .get_envs()
        .filter_map(|(key, value)| {
            matches!(
                key.to_str(),
                Some("ARC_WORKTREE_BUILD" | "ARC_WORKTREE_CACHE")
            )
            .then_some(value)
            .flatten()
            .map(PathBuf::from)
        })
        .collect::<Vec<_>>();
    for root in &storage {
        fs::create_dir_all(root)?;
    }
    let mut roots = storage.clone();
    for declared in &declaration.writes {
        let expanded = expand(declared)?;
        let path = if expanded.is_absolute() {
            expanded
        } else {
            checkout.join(expanded)
        };
        let resolved = resolve_missing(&path)?;
        if !resolved.exists() {
            if !resolved.starts_with(&checkout)
                && !storage.iter().any(|root| resolved.starts_with(root))
            {
                bail!(
                    "confinement writable root {} does not exist; external roots must exist",
                    resolved.display()
                );
            }
            fs::create_dir_all(&resolved)?;
        }
        roots.push(fs::canonicalize(resolved)?);
    }
    roots.push(PathBuf::from("/tmp"));
    roots.push(std::env::temp_dir());
    if let Some(tmp) = command
        .get_envs()
        .find_map(|(key, value)| (key == "TMPDIR").then_some(value).flatten())
    {
        roots.push(PathBuf::from(tmp));
    }
    for device in [
        "/dev/null",
        "/dev/zero",
        "/dev/random",
        "/dev/urandom",
        "/dev/tty",
        "/dev/pts",
    ] {
        if Path::new(device).exists() {
            roots.push(device.into());
        }
    }
    let mut roots = roots
        .into_iter()
        .map(fs::canonicalize)
        .collect::<std::io::Result<Vec<_>>>()?;
    roots.sort();
    roots.dedup();
    let ruleset = match ruleset(abi, &roots) {
        Ok(ruleset) => ruleset,
        Err(error) if !declaration.required => {
            let receipt = Receipt {
                confined: false,
                identity: None,
                abi: Some(abi),
                writes: roots,
                unavailable: Some(format!("{error:#}")),
            };
            eprintln!("{}", receipt.describe());
            return Ok(Some(receipt));
        }
        Err(error) => {
            return Err(error).context("cannot prepare write confinement; refusing project command")
        }
    };
    let identity = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&(abi, &roots))?))
    );
    // SAFETY: the child only invokes async-signal-safe syscalls with an already
    // prepared descriptor. No locks, allocation or filesystem traversal occur after fork.
    unsafe {
        command.pre_exec(move || restrict(&ruleset));
    }
    Ok(Some(Receipt {
        confined: true,
        identity: Some(identity),
        abi: Some(abi),
        writes: roots,
        unavailable: None,
    }))
}

fn expand(value: &str) -> Result<PathBuf> {
    if value.is_empty() {
        bail!("confinement writable root must not be empty");
    }
    let value = if value == "~" || value.starts_with("~/") {
        format!(
            "{}{}",
            std::env::var("HOME").context("HOME is unset")?,
            &value[1..]
        )
    } else {
        value.to_string()
    };
    let mut expanded = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '$' {
            expanded.push(ch);
            continue;
        }
        let braced = chars.peek() == Some(&'{');
        if braced {
            chars.next();
        }
        let mut name = String::new();
        while chars
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
        {
            name.push(chars.next().unwrap());
        }
        if name.is_empty() || (braced && chars.next() != Some('}')) {
            bail!("invalid environment expansion in confinement writable root");
        }
        expanded.push_str(
            &std::env::var(&name)
                .with_context(|| format!("confinement variable {name} is unset"))?,
        );
    }
    if expanded.is_empty() {
        bail!("confinement writable root expanded to an empty path");
    }
    Ok(expanded.into())
}

fn resolve_missing(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(fs::canonicalize(path)?);
    }
    let parent = path.parent().context("writable root has no parent")?;
    let name = path.file_name().context("invalid writable root")?;
    Ok(resolve_missing(parent)?.join(name))
}

fn check_abi(abi: u32) -> Result<u32> {
    if abi < 3 {
        bail!("Landlock ABI {abi} lacks write confinement's required TRUNCATE right (ABI 3)");
    }
    Ok(abi)
}

#[cfg(target_os = "linux")]
fn abi_version() -> Result<u32> {
    // SAFETY: the version query takes no pointers and creates no descriptor.
    let version = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0,
            1,
        )
    };
    if version < 0 {
        return Err(std::io::Error::last_os_error()).context("Landlock unavailable");
    }
    Ok(version as u32)
}

#[repr(C, packed)]
struct PathRule {
    access: u64,
    parent: i32,
}

#[cfg(target_os = "linux")]
fn ruleset(abi: u32, roots: &[PathBuf]) -> Result<OwnedFd> {
    // WRITE_FILE, REMOVE_*, MAKE_*, REFER and TRUNCATE. Reads remain unrestricted.
    let rights: u64 = (1 << 1) | (0x7ff << 4) | if abi >= 5 { 1 << 15 } else { 0 };
    // SAFETY: the kernel reads the u64-sized v1 ruleset attribute.
    let fd = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, &rights, 8, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: a successful create_ruleset returns an owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    for root in roots {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(root)?;
        let device = root.starts_with("/dev");
        let access = if device || !file.metadata()?.is_dir() {
            rights & ((1 << 1) | (1 << 14) | (1 << 15))
        } else {
            rights
        };
        let rule = PathRule {
            access,
            parent: file.as_raw_fd(),
        };
        // SAFETY: PathRule matches the packed Linux path-beneath attribute.
        if unsafe { libc::syscall(libc::SYS_landlock_add_rule, fd.as_raw_fd(), 1, &rule, 0) } != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("cannot allow {}", root.display()));
        }
    }
    Ok(fd)
}

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
unsafe fn restrict(ruleset: &OwnedFd) -> std::io::Result<()> {
    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
        || libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0) != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn abi_version() -> Result<u32> {
    bail!("Linux Landlock unavailable on this platform")
}
#[cfg(not(target_os = "linux"))]
fn ruleset(_: u32, _: &[PathBuf]) -> Result<OwnedFd> {
    bail!("Linux Landlock unavailable on this platform")
}
#[cfg(not(target_os = "linux"))]
unsafe fn restrict(_: &OwnedFd) -> std::io::Result<()> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}
