//! Kernel-enforced writable roots for commands supplied by a project.

use crate::perf::Counted;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::ffi::{CString, OsStr};
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const CARGO_CACHEDIR_TAG: &[u8] = b"\
Signature: 8a477f597d28d172789f06886806bc55
# Created by arc write confinement; does not authorize artifact cleanup alone.
# For information about cache directory tags see https://bford.info/cachedir/
";

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

#[derive(Debug, Clone)]
enum Authority {
    Resolved(Option<Declaration>),
    Destination {
        repository: PathBuf,
        revision: String,
    },
}
thread_local! {
    static DECLARATION: RefCell<Option<Authority>> = const { RefCell::new(None) };
}

/// Carries destination authority through scratch population and preparation.
pub struct Scope(Option<Authority>);
impl Scope {
    pub fn new(declaration: &Option<Declaration>) -> Self {
        Self::captured(Authority::Resolved(declaration.clone()))
    }
    pub(crate) fn at(repository: &Path, revision: &str) -> Self {
        Self::captured(Authority::Destination {
            repository: repository.to_path_buf(),
            revision: revision.to_string(),
        })
    }
    fn captured(authority: Authority) -> Self {
        Self(DECLARATION.with(|current| current.replace(Some(authority))))
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

pub(crate) fn declaration(site: &str, checkout: &Path) -> Result<Option<Declaration>> {
    let current = DECLARATION.with(|current| current.borrow().clone());
    match current.with_context(|| format!("project command at {site} has no destination confinement scope for {}; refusing to read the checkout's declaration", checkout.display()))? {
        Authority::Resolved(declaration) => Ok(declaration),
        Authority::Destination { repository, revision } => at(&repository, &revision),
    }
}

/// Resolve the destination explicitly at a project command entry point.
pub(crate) fn for_checkout(checkout: &Path) -> Result<Option<Declaration>> {
    let top = crate::gitio::toplevel(checkout)?;
    let root = crate::store::Store::resolve_root(&top)?;
    let store = crate::store::Store::open_at_for(&root, &top)?;
    let states = store
        .as_ref()
        .map(|store| store.readable_states_shared())
        .transpose()?;
    let owner = |branch: &str| -> Result<Option<Option<Declaration>>> {
        let Some(states) = &states else {
            return Ok(None);
        };
        let owners = states
            .values()
            .filter(|state| state.branch == branch)
            .collect::<Vec<_>>();
        let open = owners
            .iter()
            .copied()
            .filter(|state| !state.is_closed())
            .collect::<Vec<_>>();
        let owners = if open.is_empty() { owners } else { open };
        if owners.len() > 1 {
            bail!("ambiguous destination for {branch}; run from the destination checkout");
        }
        let Some(state) = owners.first() else {
            return Ok(None);
        };
        if state.is_closed() {
            return Ok(Some(at(
                &top,
                &crate::gitio::branch_head(&top, &state.target_branch)?,
            )?));
        }
        let declarations = crate::declarations::for_change(&top, state)?;
        if declarations.target_unreadable {
            bail!("destination of {} is unreadable", state.change_id);
        }
        Ok(Some(declarations.gates.confine))
    };
    let branch = crate::gitio::current_branch(&top)?;
    if let Some(branch) = &branch {
        if let Some(declaration) = owner(branch)? {
            return Ok(declaration);
        }
    }
    if let Some(primary) = crate::gitio::primary_worktree_branch(&top)? {
        if let Some(declaration) = owner(&primary)? {
            return Ok(declaration);
        }
        if branch.as_deref() == Some(primary.as_str()) {
            return at(&top, &crate::gitio::branch_head(&top, &primary)?);
        }
    }
    bail!("checkout {} has no established confinement destination; run `arc gates measure` or `arc gates init` from the destination checkout, or use `arc verify <change>` for a recorded change", top.display())
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
#[cfg(test)]
pub fn apply(command: &mut Command, checkout: &Path) -> Result<Option<Receipt>> {
    let Some(declaration) = declaration("unscoped project command", checkout)? else {
        return Ok(None);
    };
    apply_declared(command, checkout, &declaration)
}

#[cfg(test)]
pub fn apply_declared(
    command: &mut Command,
    checkout: &Path,
    declaration: &Declaration,
) -> Result<Option<Receipt>> {
    apply_with_storage(command, checkout, declaration, None)
}

pub(crate) fn apply_with_storage(
    command: &mut Command,
    checkout: &Path,
    declaration: &Declaration,
    storage: Option<&crate::warm_worktree::CommandStorage>,
) -> Result<Option<Receipt>> {
    apply_with_abi_and_storage(command, checkout, declaration, abi_version(), storage)
}

#[cfg(test)]
fn apply_with_abi(
    command: &mut Command,
    checkout: &Path,
    declaration: &Declaration,
    abi: Result<u32>,
) -> Result<Option<Receipt>> {
    apply_with_abi_and_storage(command, checkout, declaration, abi, None)
}

fn apply_with_abi_and_storage(
    command: &mut Command,
    checkout: &Path,
    declaration: &Declaration,
    abi: Result<u32>,
    storage: Option<&crate::warm_worktree::CommandStorage>,
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
    let owned = storage.map(|storage| storage.owned()).unwrap_or_default();
    for root in &owned {
        fs::create_dir_all(root)?;
    }
    let common = crate::gitio::common_dir(&checkout).ok();
    let home = fs::canonicalize(std::env::var_os("HOME").context("HOME is unset")?)?;
    let mut resolved_writes = Vec::new();
    for declared in &declaration.writes {
        let expanded = expand(declared)?;
        let resolved = resolve_declared(&checkout, &expanded)?;
        validate_root(
            &resolved,
            &checkout,
            &home,
            common.as_deref(),
            &owned,
            false,
        )?;
        if !resolved.exists()
            && !resolved.starts_with(&checkout)
            && !owned.iter().any(|root| resolved.starts_with(root))
        {
            bail!(
                "confinement writable root {} does not exist; external roots must exist",
                resolved.display()
            );
        }
        resolved_writes.push(resolved);
    }
    let mut owned_canonical = Vec::with_capacity(owned.len());
    for root in owned {
        let resolved = fs::canonicalize(root)?;
        if checkout.starts_with(&resolved)
            || home.starts_with(&resolved)
            || common
                .as_ref()
                .is_some_and(|common| common.starts_with(&resolved))
        {
            bail!("arc-owned storage contains protected checkout, home or Git administration");
        }
        owned_canonical.push(resolved);
    }
    let tmp_root = fs::canonicalize("/tmp").unwrap_or_else(|_| PathBuf::from("/tmp"));
    let tmp = match command.get_envs().find(|(key, _)| *key == "TMPDIR") {
        Some((_, value)) => value.map(PathBuf::from),
        None => std::env::var_os("TMPDIR").map(PathBuf::from),
    };
    let extra_tmp = if let Some(tmp) = tmp {
        let tmp = resolve_missing(&if tmp.is_absolute() {
            tmp
        } else {
            checkout.join(tmp)
        })?;
        if tmp != Path::new("/tmp") && tmp != tmp_root {
            validate_root(&tmp, &checkout, &home, common.as_deref(), &[], true)?;
        }
        Some(tmp)
    } else {
        None
    };
    // Collect all roots that this confined command will be allowed to write so
    // target staging never uses a directory writable by the confined command.
    let mut writable_roots = Vec::new();
    writable_roots.push(tmp_root.clone());
    if let Some(ref extra_tmp) = extra_tmp {
        writable_roots.push(extra_tmp.clone());
    }
    writable_roots.extend(owned_canonical.iter().cloned());
    writable_roots.extend(
        resolved_writes
            .iter()
            .map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.clone())),
    );

    let mut roots = Vec::new();
    for resolved in resolved_writes {
        if !resolved.exists() {
            if resolved.starts_with(&checkout) {
                create_checkout_root(
                    command,
                    &checkout,
                    &resolved,
                    common.as_deref(),
                    &writable_roots,
                )?;
            } else {
                fs::create_dir_all(&resolved)?;
            }
        }
        roots.push(fs::canonicalize(resolved)?);
    }
    if let Some(storage) = storage {
        if let Some(external) = storage.external() {
            let external = resolve_missing(external)?;
            if !roots.iter().any(|root| external.starts_with(root)) {
                bail!("project-selected preparation directory {} is not covered by destination [confine] writes", external.display());
            }
        }
    } else if command.get_envs().any(|(key, value)| {
        matches!(
            key.to_str(),
            Some("ARC_WORKTREE_BUILD" | "ARC_WORKTREE_CACHE")
        ) && value.is_some()
    }) {
        bail!(
            "project command supplied unverified build storage; only arc-owned storage is implicit"
        );
    }
    roots.extend(owned_canonical);
    roots.push(PathBuf::from("/tmp"));
    if let Some(tmp) = extra_tmp {
        roots.push(tmp);
    }
    for device in [
        "/dev/null",
        "/dev/zero",
        "/dev/random",
        "/dev/urandom",
        "/dev/tty",
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
        let mut expression = String::new();
        if braced {
            let mut depth = 1;
            for ch in chars.by_ref() {
                if ch == '{' {
                    depth += 1;
                }
                if ch == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                expression.push(ch);
            }
            if depth != 0 {
                bail!("invalid environment expansion in confinement writable root");
            }
        } else {
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                expression.push(chars.next().unwrap());
            }
        }
        let (name, default) = expression
            .split_once(":-")
            .map_or((expression.as_str(), None), |(name, default)| {
                (name, Some(default))
            });
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!("invalid environment expansion in confinement writable root");
        }
        match std::env::var(name).ok().filter(|value| !value.is_empty()) {
            Some(value) => expanded.push_str(&value),
            None => match default {
                Some(default) => expanded.push_str(&expand(default)?.to_string_lossy()),
                None => bail!("confinement variable {name} is unset"),
            },
        }
    }
    if expanded.is_empty() {
        bail!("confinement writable root expanded to an empty path");
    }
    Ok(expanded.into())
}

fn resolve_declared(checkout: &Path, declared: &Path) -> Result<PathBuf> {
    if declared.is_absolute() {
        if let Ok(relative) = declared.strip_prefix(checkout) {
            return resolve_declared(checkout, relative);
        }
        return resolve_missing(declared);
    }
    let mut resolved = checkout.to_path_buf();
    for component in declared.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::Normal(name) => {
                resolved.push(name);
                if resolved.exists() {
                    resolved = fs::canonicalize(resolved)?;
                }
            }
            _ => bail!("invalid relative writable root"),
        }
        if !resolved.starts_with(checkout) {
            bail!(
                "relative confinement root {} escapes the checkout",
                declared.display()
            );
        }
    }
    Ok(resolved)
}

fn validate_root(
    root: &Path,
    checkout: &Path,
    home: &Path,
    common: Option<&Path>,
    owned: &[PathBuf],
    temporary: bool,
) -> Result<()> {
    let kind = if temporary {
        "TMPDIR"
    } else {
        "confinement writable root"
    };
    if temporary && root.starts_with(home) {
        bail!("TMPDIR {} overlaps the home directory; choose a temporary directory outside home, the checkout and Git administration", root.display());
    }
    if home.starts_with(root) || root.parent() == Some(home) {
        bail!(
            "{kind} {} grants the home directory's top level or an ancestor",
            root.display()
        );
    }
    if checkout.starts_with(root) || (temporary && root.starts_with(checkout)) {
        bail!(
            "{kind} {} grants the checkout or overlaps it",
            root.display()
        );
    }
    if root == checkout.join(".git")
        || common.is_some_and(|common| {
            common.starts_with(root)
                || (root.starts_with(common) && !owned.iter().any(|owned| root.starts_with(owned)))
        })
    {
        bail!(
            "{kind} {} grants Git administration outside arc-owned build storage",
            root.display()
        );
    }
    Ok(())
}

pub(crate) fn require_external(checkout: &Path, external: &Path) -> Result<()> {
    let Some(declaration) = declaration("path-stable preparation directory", checkout)? else {
        return Ok(());
    };
    let checkout = fs::canonicalize(checkout)?;
    let external = resolve_missing(external)?;
    let home = fs::canonicalize(std::env::var_os("HOME").context("HOME is unset")?)?;
    let common = crate::gitio::common_dir(&checkout)?;
    let mut covered = false;
    for root in &declaration.writes {
        let root = resolve_declared(&checkout, &expand(root)?)?;
        validate_root(&root, &checkout, &home, Some(&common), &[], false)?;
        covered |= external.starts_with(&root);
    }
    if !covered {
        bail!("project-selected preparation directory {} is not covered by destination [confine] writes", external.display());
    }
    Ok(())
}

fn resolve_missing(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(fs::canonicalize(path)?);
    }
    let parent = path.parent().context("writable root has no parent")?;
    let name = path.file_name().context("invalid writable root")?;
    Ok(resolve_missing(parent)?.join(name))
}

fn create_checkout_root(
    command: &Command,
    checkout: &Path,
    resolved: &Path,
    common: Option<&Path>,
    writable_roots: &[PathBuf],
) -> Result<()> {
    let relative = resolved.strip_prefix(checkout).with_context(|| {
        format!(
            "{} is outside checkout {}",
            resolved.display(),
            checkout.display()
        )
    })?;
    let checkout_fd = open_dir_path(checkout)?;
    let first_is_root_target = matches!(
        relative.components().next(),
        Some(std::path::Component::Normal(name)) if name == OsStr::new("target")
    );
    // Only the checkout root's `target` directory is Cargo's default build root
    // when neither environment nor Cargo config overrides `target-dir`.
    let should_mark_target = first_is_root_target
        && is_regular_file_at(&checkout_fd, c"Cargo.toml")?
        && cargo_uses_default_root_target(command, checkout)
        && common.is_some()
        && is_git_ignored(checkout, Path::new("target/CACHEDIR.TAG"))?;

    let mut current = checkout_fd;
    let mut current_path = checkout.to_path_buf();
    for (depth, component) in relative.components().enumerate() {
        let std::path::Component::Normal(name) = component else {
            bail!(
                "invalid component in confinement writable root {}",
                resolved.display()
            );
        };
        let c_name = CString::new(name.as_bytes()).with_context(|| {
            format!(
                "invalid NUL byte in confinement writable root {}",
                resolved.display()
            )
        })?;
        current_path.push(name);
        if depth == 0 && should_mark_target {
            publish_tagged_target_at(&current, common, writable_roots, &c_name, &current_path)?;
        } else {
            create_untagged_dir_at(&current, &c_name, &current_path)?;
        }
        // Use `O_PATH | O_DIRECTORY | O_NOFOLLOW` so write+search-only (0300)
        // directories remain traversable without following symlinks.
        current = open_dir_at(&current, &c_name, &current_path, libc::O_PATH)?;
    }
    Ok(())
}

fn create_untagged_dir_at(parent: &OwnedFd, name: &CString, display: &Path) -> Result<()> {
    // SAFETY: `parent` is an open directory fd and `name` is a valid NUL-terminated path component.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o777) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error).with_context(|| format!("cannot create {}", display.display()));
        }
    }
    Ok(())
}

fn fd_device(fd: &OwnedFd) -> Result<libc::dev_t> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fd` is open (including `O_PATH`) and `stat` is valid for writes.
    let rc = unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error()).context("cannot stat directory handle");
    }
    // SAFETY: `fstat` succeeded and initialized `stat`.
    Ok(unsafe { stat.assume_init() }.st_dev)
}

/// Choose a staging parent directory on the same filesystem as `checkout_fd`,
/// using `<git-common-dir>/arc` only when it is withheld from the confined
/// command's writable roots (including automatic `/tmp` and `TMPDIR` grants) so
/// sibling checkout processes cannot replace staging entries before
/// `renameat2(RENAME_NOREPLACE)`.
fn staging_parent_for(
    checkout_fd: &OwnedFd,
    common: Option<&Path>,
    writable_roots: &[PathBuf],
) -> Result<Option<OwnedFd>> {
    let Some(common) = common else {
        return Ok(None);
    };
    let Ok(canonical_common) = fs::canonicalize(common) else {
        return Ok(None);
    };
    let canonical_arc = canonical_common.join("arc");
    if writable_roots
        .iter()
        .any(|root| canonical_arc.starts_with(root))
    {
        return Ok(None);
    }
    let Ok(common_fd) = open_dir_path(&canonical_common) else {
        return Ok(None);
    };
    // SAFETY: `common_fd` is an open directory fd and `"arc"` is a static C string.
    if unsafe { libc::mkdirat(common_fd.as_raw_fd(), c"arc".as_ptr(), 0o777) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Ok(None);
        }
    }
    let Ok(arc_fd) = open_dir_at(&common_fd, c"arc", &canonical_arc, libc::O_PATH) else {
        return Ok(None);
    };
    if fd_device(&arc_fd)? == fd_device(checkout_fd)? {
        Ok(Some(arc_fd))
    } else {
        Ok(None)
    }
}

/// Atomically publish a new directory already carrying `CACHEDIR.TAG` using
/// `renameat2(RENAME_NOREPLACE)`. All staging operations use `O_PATH` directory
/// descriptors (working even under `umask 0477`) inside `<git-common-dir>/arc`
/// when withheld from the command's writable roots on the same mount; writing,
/// publication, and cleanup stay bound to the opened handles. If
/// `<git-common-dir>/arc` is writable by the command, unavailable, or across a
/// mount boundary (`EXDEV`), creates the directory unmarked instead. If `name`
/// already exists, the staged directory is removed without touching it.
fn publish_tagged_target_at(
    checkout_fd: &OwnedFd,
    common: Option<&Path>,
    writable_roots: &[PathBuf],
    name: &CString,
    display: &Path,
) -> Result<()> {
    let Some(stage_root) = staging_parent_for(checkout_fd, common, writable_roots)? else {
        return create_untagged_dir_at(checkout_fd, name, display);
    };
    publish_tagged_target_from(checkout_fd, &stage_root, name, display)
}

fn publish_tagged_target_from(
    checkout_fd: &OwnedFd,
    stage_root: &OwnedFd,
    name: &CString,
    display: &Path,
) -> Result<()> {
    let container_name = format!(".arc-target-stage-{}", crate::ids::new_identifier());
    let c_container =
        CString::new(container_name.as_bytes()).context("invalid staging container name")?;
    // Create the staging container with mode 0o700 so other users cannot traverse it;
    // the inner `target` directory uses 0o777 to respect the caller's umask.
    // SAFETY: `stage_root` is an open directory fd and `c_container` is a valid NUL-terminated name.
    if unsafe { libc::mkdirat(stage_root.as_raw_fd(), c_container.as_ptr(), 0o700) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("cannot stage {}", display.display()));
    }
    let stage_result = (|| -> Result<()> {
        let container_fd = open_dir_at(stage_root, &c_container, display, libc::O_PATH)?;
        // SAFETY: `container_fd` is an open directory fd and `name` is a valid NUL-terminated name.
        if unsafe { libc::mkdirat(container_fd.as_raw_fd(), name.as_ptr(), 0o777) } != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("cannot create staged {}", display.display()));
        }
        let target_fd = open_dir_at(&container_fd, name, display, libc::O_PATH)?;
        if let Err(error) = write_cachedir_tag_in_dir(&target_fd, display) {
            cleanup_staged_inner(&container_fd, &target_fd, name);
            return Err(error);
        }
        // SAFETY: `container_fd` and `checkout_fd` are open directory fds.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                container_fd.as_raw_fd(),
                name.as_ptr(),
                checkout_fd.as_raw_fd(),
                name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        cleanup_staged_inner(&container_fd, &target_fd, name);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Ok(());
        }
        if error.raw_os_error() == Some(libc::EXDEV) {
            return create_untagged_dir_at(checkout_fd, name, display);
        }
        Err(error).with_context(|| format!("cannot publish {}", display.display()))
    })();
    // Always remove the staging container using the same `stage_root` handle.
    // SAFETY: `stage_root` is an open directory fd and `c_container` is a valid NUL-terminated name.
    unsafe {
        libc::unlinkat(
            stage_root.as_raw_fd(),
            c_container.as_ptr(),
            libc::AT_REMOVEDIR,
        );
    }
    stage_result
}

fn cleanup_staged_inner(container_fd: &OwnedFd, target_fd: &OwnedFd, name: &CString) {
    // SAFETY: `target_fd` is the pinned staged target descriptor, never reopened by path.
    unsafe {
        libc::unlinkat(target_fd.as_raw_fd(), c"CACHEDIR.TAG".as_ptr(), 0);
        libc::unlinkat(container_fd.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR);
    }
}

fn effective_env_value(command: &Command, key: &str) -> Option<std::ffi::OsString> {
    if let Some((_, value)) = command.get_envs().find(|(k, _)| *k == OsStr::new(key)) {
        return value.map(std::ffi::OsStr::to_os_string);
    }
    std::env::var_os(key)
}

/// Returns `true` only when neither command/process environment nor Cargo
/// configuration files redirect Cargo's target directory away from
/// `<checkout>/target`. Honors Cargo's precedence (`.cargo/config` before
/// `.cargo/config.toml` in each directory) and fails closed on unreadable or
/// unparseable Cargo configuration.
fn cargo_uses_default_root_target(command: &Command, checkout: &Path) -> bool {
    let default_target = checkout.join("target");
    for var in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"] {
        if let Some(value) = effective_env_value(command, var) {
            if !value.is_empty() {
                let candidate = PathBuf::from(&value);
                let resolved = if candidate.is_absolute() {
                    candidate
                } else {
                    checkout.join(candidate)
                };
                let normalized = resolve_missing(&resolved).unwrap_or(resolved);
                if normalized != default_target {
                    return false;
                }
            }
        }
    }
    let mut config_dirs: Vec<PathBuf> = checkout
        .ancestors()
        .map(|ancestor| ancestor.join(".cargo"))
        .collect();
    if let Some(cargo_home) = effective_env_value(command, "CARGO_HOME").filter(|v| !v.is_empty()) {
        let candidate = PathBuf::from(cargo_home);
        // Preserve the lexical config directory so relative `build.target-dir`
        // resolves from `dir.parent()` even when `CARGO_HOME` is a symlink.
        let dir = if candidate.is_absolute() {
            candidate
        } else {
            checkout.join(candidate)
        };
        config_dirs.push(dir);
    } else if let Some(home) = effective_env_value(command, "HOME").filter(|v| !v.is_empty()) {
        let candidate = PathBuf::from(home).join(".cargo");
        let dir = if candidate.is_absolute() {
            candidate
        } else {
            checkout.join(candidate)
        };
        config_dirs.push(dir);
    }
    for dir in config_dirs {
        // Cargo checks `config` before `config.toml` in each directory and
        // ignores `config.toml` when `config` is present in the same directory.
        let config_path = dir.join("config");
        let toml_path = dir.join("config.toml");
        let path = if fs::symlink_metadata(&config_path).is_ok() {
            config_path
        } else if fs::symlink_metadata(&toml_path).is_ok() {
            toml_path
        } else {
            continue;
        };
        let Some(contents) = read_regular_file_nonblocking(&path) else {
            return false;
        };
        let Ok(parsed) = contents.parse::<toml::Value>() else {
            return false;
        };
        // Conservatively skip tagging whenever a selected Cargo config uses
        // `include`, since included files can redirect `build.target-dir`.
        if parsed.get("include").is_some() {
            return false;
        }
        if let Some(value) = parsed
            .get("build")
            .and_then(|build| build.get("target-dir"))
        {
            let Some(target_dir) = value.as_str() else {
                return false;
            };
            // Cargo resolves relative `build.target-dir` from the lexical parent
            // of the config directory regardless of whether it is named `.cargo`.
            let base = dir.parent().unwrap_or(checkout);
            let candidate = Path::new(target_dir);
            let resolved = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                base.join(candidate)
            };
            let normalized = resolve_missing(&resolved).unwrap_or(resolved);
            return normalized == default_target;
        }
    }
    true
}

/// Open `path` without blocking on FIFOs/special files, verify the opened
/// descriptor is a regular file, and read it as UTF-8.
fn read_regular_file_nonblocking(path: &Path) -> Option<String> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c_path` is a valid NUL-terminated path; fd is checked for `< 0`.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` is a freshly opened valid descriptor owned by this File.
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;
    Some(contents)
}

fn open_dir_path(path: &Path) -> Result<OwnedFd> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("invalid path {}", path.display()))?;
    // SAFETY: `c_path` is a valid NUL-terminated path; the returned fd is checked for `< 0`.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("cannot open directory {}", path.display()));
    }
    // SAFETY: `fd` is a freshly opened valid descriptor owned by this function.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn open_dir_at(
    parent: &OwnedFd,
    name: &std::ffi::CStr,
    display: &Path,
    access: libc::c_int,
) -> Result<OwnedFd> {
    // SAFETY: `parent` is an open directory fd and `name` is a valid NUL-terminated entry name.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            access | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "confinement writable root {} is not a real directory",
                display.display()
            )
        });
    }
    // SAFETY: `fd` is a freshly opened valid descriptor owned by this function.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn is_regular_file_at(parent: &OwnedFd, name: &std::ffi::CStr) -> Result<bool> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `parent` is an open directory fd, `name` is NUL-terminated, and `stat` is valid for writes.
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(false);
        }
        return Err(error).context("cannot inspect Cargo manifest in checkout");
    }
    // SAFETY: `fstatat` succeeded and initialized `stat`.
    let stat = unsafe { stat.assume_init() };
    Ok((stat.st_mode & libc::S_IFMT) == libc::S_IFREG)
}

fn write_cachedir_tag_in_dir(dir: &OwnedFd, dir_path: &Path) -> Result<()> {
    // SAFETY: `dir` is a pinned directory fd and `"CACHEDIR.TAG"` is a static C string.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c"CACHEDIR.TAG".as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o644,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Ok(());
        }
        return Err(error)
            .with_context(|| format!("cannot create {}/CACHEDIR.TAG", dir_path.display()));
    }
    // SAFETY: `fd` is a freshly opened valid descriptor owned by this File.
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    file.write_all(CARGO_CACHEDIR_TAG)
        .with_context(|| format!("cannot write {}/CACHEDIR.TAG", dir_path.display()))
}

fn is_git_ignored(checkout: &Path, relative: &Path) -> Result<bool> {
    let literal = Path::new(".").join(relative);
    let output = crate::gitio::git_command()
        .args(["check-ignore", "-q", "--"])
        .arg(&literal)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(checkout)
        .counted_output()?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "cannot check ignore rules for {}: {}",
            relative.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
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
        let access = if !file.metadata()?.is_dir() {
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
