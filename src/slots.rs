//! Leased fixed-path checkouts a repository keeps across changes.
//!
//! A pool is a directory holding `pool.json` (the owning repository's Git
//! common directory and a random pool id), `pool.lock`, and per slot a state
//! file `slot-NN.json`, a use lock `slot-NN.use.lock` and the checkout
//! `slot-NN`. The pool lock is innermost: it is held only around reading and
//! writing slot state files, and nothing is acquired while it is held.
//!
//! A slot's use lock is held shared by every command arc runs in the
//! checkout (gates, probes, preparation) and exclusively, without blocking,
//! by every operation that changes what the checkout holds. Commands run
//! under a holder process that keeps the lock until its last descendant
//! has exited, so killing the arc process that launched a command does not
//! free the slot while the command's processes live.

use crate::process_group::{self, OwnedGroup};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::gitio;

pub const POOL_SCHEMA: &str = "arc-slot-pool/1";
pub const SLOT_SCHEMA: &str = "arc-slot/1";
pub const STATUS_SCHEMA: &str = "arc-slots/1";
const POOL_FILE: &str = "pool.json";
const POOL_LOCK: &str = "pool.lock";
const MAX_SIZE: u32 = 32;
/// The argument that runs arc as a slot's use holder.
pub const HOLDER: &str = "__slot-hold";
const STATUS_FD: &str = "ARC_SLOT_STATUS_FD";
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(120);
const INVALIDATE_TIMEOUT: Duration = Duration::from_secs(600);

/// `[slots]` in `.arc/worktrees.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub size: u32,
    pub root: Option<String>,
    #[serde(default = "default_documentation")]
    pub documentation: Vec<String>,
    pub identity: Option<String>,
    #[serde(default)]
    pub retain: Vec<String>,
    pub invalidate: Option<String>,
    #[serde(default)]
    pub default: bool,
    /// What `arc slots warm` runs in a stale free slot.
    pub warm: Option<String>,
    /// Whether a successful integration starts one detached warm.
    #[serde(default)]
    pub warm_after_integrate: bool,
}

fn default_documentation() -> Vec<String> {
    vec!["*.md".to_string()]
}

impl Declaration {
    pub fn validated(self) -> Result<Self> {
        if !(1..=MAX_SIZE).contains(&self.size) {
            bail!("[slots] size must be between 1 and {MAX_SIZE}");
        }
        for (key, value) in [
            ("root", &self.root),
            ("identity", &self.identity),
            ("invalidate", &self.invalidate),
            ("warm", &self.warm),
        ] {
            if value.as_ref().is_some_and(|value| value.trim().is_empty()) {
                bail!("[slots] {key} is empty");
            }
        }
        if self.warm_after_integrate && self.warm.is_none() {
            bail!("[slots] warm_after_integrate needs a warm command");
        }
        for pattern in self.documentation.iter().chain(&self.retain) {
            if pattern.is_empty()
                || pattern.starts_with('/')
                || pattern
                    .split('/')
                    .any(|part| part == ".." || part.is_empty())
            {
                bail!("[slots] pattern {pattern:?} must be a nonempty relative path");
            }
        }
        Ok(self)
    }
}

/// Whether `path` matches `pattern`: `*` and `?` stay within one path
/// component, a pattern without `/` matches a file name at any depth, and
/// one with `/` matches from the checkout root.
pub fn matches(pattern: &str, path: &str) -> bool {
    if pattern.contains('/') {
        component_glob(pattern.as_bytes(), path.as_bytes())
    } else {
        path.rsplit('/')
            .next()
            .is_some_and(|name| component_glob(pattern.as_bytes(), name.as_bytes()))
    }
}

fn component_glob(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            component_glob(&pattern[1..], text)
                || (text.first().is_some_and(|c| *c != b'/') && component_glob(pattern, &text[1..]))
        }
        (Some(b'?'), Some(c)) if *c != b'/' => component_glob(&pattern[1..], &text[1..]),
        (Some(p), Some(c)) if p == c => component_glob(&pattern[1..], &text[1..]),
        _ => false,
    }
}

/// Whether ignored `path` lies at or beneath a path a `retain` pattern names.
fn retained(retain: &[String], path: &str) -> bool {
    let path = path.trim_end_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    (1..=parts.len()).any(|end| {
        let prefix = parts[..end].join("/");
        retain.iter().any(|pattern| matches(pattern, &prefix))
    })
}

#[derive(Serialize, Deserialize)]
struct PoolRecord {
    schema: String,
    repository: String,
    id: String,
    size: u32,
}

/// Whether the pool at `root` exists and belongs to the repository whose
/// checkout is `cwd`. A slot record imported from another replica names a
/// pool this repository does not own; reading that takes no lock.
pub fn owned_here(cwd: &Path, root: &Path) -> bool {
    let Ok(common) = gitio::common_dir(cwd).and_then(|dir| Ok(fs::canonicalize(dir)?)) else {
        return false;
    };
    owned_by(root, &common)
}

fn owned_by(root: &Path, common: &Path) -> bool {
    let Ok(root) = fs::canonicalize(root) else {
        return false;
    };
    let Ok(bytes) = fs::read(root.join(POOL_FILE)) else {
        return false;
    };
    serde_json::from_slice::<PoolRecord>(&bytes)
        .is_ok_and(|record| record.schema == POOL_SCHEMA && Path::new(&record.repository) == common)
}

/// A slot pool owned by this repository.
pub struct Pool {
    pub root: PathBuf,
    pub id: String,
    pub size: u32,
}

/// The pool root a declaration names for the repository at `cwd`.
pub fn declared_root(cwd: &Path, declaration: &Declaration) -> Result<PathBuf> {
    match &declaration.root {
        Some(root) => {
            let root = crate::config::expand_tilde(root)?;
            if !root.is_absolute() {
                bail!("[slots] root must be absolute or start with ~/");
            }
            Ok(root)
        }
        None => {
            let primary = gitio::primary_worktree(cwd)?;
            let name = primary
                .file_name()
                .context("cannot determine repository name")?
                .to_string_lossy()
                .into_owned();
            Ok(crate::config::load()?
                .worktrees_dir
                .join(format!("{name}-slots")))
        }
    }
}

impl Pool {
    /// The declared pool, created when absent. A root another repository
    /// owns, or one holding anything but a pool, refuses.
    pub fn declared(cwd: &Path, declaration: &Declaration) -> Result<Self> {
        let root = declared_root(cwd, declaration)?;
        fs::create_dir_all(&root)
            .with_context(|| format!("cannot create slot pool {}", root.display()))?;
        Self::open(cwd, &root, Some(declaration.size))
    }

    /// An existing pool at `root`, which this repository must own.
    pub fn existing(cwd: &Path, root: &Path) -> Result<Self> {
        Self::open(cwd, root, None)
    }

    fn open(cwd: &Path, root: &Path, size: Option<u32>) -> Result<Self> {
        let common = fs::canonicalize(gitio::common_dir(cwd)?)?;
        let root = fs::canonicalize(root)
            .with_context(|| format!("slot pool {} is unavailable", root.display()))?;
        if root.starts_with(&common) || common.starts_with(&root) {
            bail!(
                "slot pool {} must lie outside the Git directory",
                root.display()
            );
        }
        if let Ok(primary) = gitio::primary_worktree(cwd) {
            if root.starts_with(fs::canonicalize(&primary).unwrap_or(primary)) {
                bail!(
                    "slot pool {} must lie outside the repository's checkout",
                    root.display()
                );
            }
        }
        let _lock = lock_file(&root.join(POOL_LOCK), true)?;
        let path = root.join(POOL_FILE);
        let record = match fs::read(&path) {
            Ok(bytes) => {
                let record: PoolRecord = serde_json::from_slice(&bytes)
                    .with_context(|| format!("cannot read {}", path.display()))?;
                if record.schema != POOL_SCHEMA {
                    bail!(
                        "{} has unsupported schema {}",
                        path.display(),
                        record.schema
                    );
                }
                if Path::new(&record.repository) != common {
                    bail!(
                        "slot pool {} belongs to the repository at {}, not {}; give this repository its own [slots] root",
                        root.display(),
                        record.repository,
                        common.display()
                    );
                }
                if size.is_some_and(|size| size != record.size) {
                    bail!(
                        "slot pool {} has size {}; a pool's size is fixed once it exists",
                        root.display(),
                        record.size
                    );
                }
                record
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(size) = size else {
                    bail!("{} is not a slot pool", root.display());
                };
                for entry in fs::read_dir(&root)? {
                    let name = entry?.file_name();
                    if name != POOL_LOCK {
                        bail!(
                            "slot pool root {} holds {}; a new pool needs an empty directory",
                            root.display(),
                            name.to_string_lossy()
                        );
                    }
                }
                let record = PoolRecord {
                    schema: POOL_SCHEMA.into(),
                    repository: common.display().to_string(),
                    id: crate::ids::new_identifier(),
                    size,
                };
                write_json(&path, &record)?;
                record
            }
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()))
            }
        };
        Ok(Self {
            root,
            id: record.id,
            size: record.size,
        })
    }

    pub fn names(&self) -> Vec<String> {
        (1..=self.size)
            .map(|index| format!("slot-{index:02}"))
            .collect()
    }

    pub fn name_of(&self, reference: &str) -> Result<String> {
        let name = if let Ok(index) = reference.parse::<u32>() {
            format!("slot-{index:02}")
        } else {
            reference.to_string()
        };
        if !self.names().contains(&name) {
            bail!(
                "{reference} is not a slot of the pool at {}",
                self.root.display()
            );
        }
        Ok(name)
    }

    pub fn checkout(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn state_path(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.json"))
    }

    pub fn lock(&self) -> Result<File> {
        lock_file(&self.root.join(POOL_LOCK), true)
    }

    /// A slot's state; a slot with no state file is free.
    pub fn read(&self, name: &str) -> Result<SlotState> {
        let path = self.state_path(name);
        match fs::read(&path) {
            Ok(bytes) => {
                let state: SlotState = serde_json::from_slice(&bytes)
                    .with_context(|| format!("cannot read {}", path.display()))?;
                if state.schema != SLOT_SCHEMA || state.name != name {
                    bail!("{} does not describe {name}", path.display());
                }
                Ok(state)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SlotState::free(name)),
            Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    fn write(&self, state: &SlotState) -> Result<()> {
        write_json(&self.state_path(&state.name), state)
    }

    /// Change a leased slot's state under the pool lock, refusing a token
    /// that no longer holds its lease.
    pub fn update(
        &self,
        name: &str,
        token: &str,
        change: impl FnOnce(&mut SlotState),
    ) -> Result<SlotState> {
        let _lock = self.lock()?;
        let mut state = self.read(name)?;
        if state.lease.as_ref().map(|lease| lease.token.as_str()) != Some(token) {
            bail!("token {token} does not hold the lease on {name}");
        }
        change(&mut state);
        self.write(&state)?;
        Ok(state)
    }

    /// Free a slot whose lease `token` holds.
    pub fn free(&self, name: &str, token: &str) -> Result<()> {
        self.update(name, token, |state| {
            state.lease = None;
            state.phase = Phase::Free;
        })
        .map(|_| ())
    }

    fn owner_id(&self, name: &str) -> String {
        format!("slot:{}:{name}", self.id)
    }

    /// Refuse a checkout this repository did not register at the slot's path
    /// with the administrative directory and owner token recorded for it.
    pub fn require_owned(&self, cwd: &Path, state: &SlotState) -> Result<PathBuf> {
        let path = self.checkout(&state.name);
        let owner = state.owner.as_ref().with_context(|| {
            format!(
                "ownership of {} is unknown; arc reuses only a checkout it created there",
                path.display()
            )
        })?;
        if !gitio::worktree_admin_matches(cwd, &path, Path::new(&owner.admin))
            || !gitio::worktree_owner_matches(
                &path,
                &self.owner_id(&state.name),
                Some(&owner.token),
            )
        {
            bail!(
                "{} is not the checkout this repository registered for {}; arc reuses only a checkout it owns",
                path.display(),
                state.name
            );
        }
        Ok(path)
    }

    /// Whether a command still runs in the slot or an operation holds it.
    pub fn in_use(&self, name: &str) -> bool {
        let checkout = self.checkout(name);
        if held(&checkout).is_some() {
            return true;
        }
        match lock_file_nonblocking(&use_lock_path(&checkout), libc::LOCK_EX) {
            Ok(Some(_)) => false,
            Ok(None) | Err(_) => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Free,
    Leasing,
    Switching,
    Recorded,
    Warming,
    Detaching,
    Releasing,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Free => "free",
            Phase::Leasing => "leasing",
            Phase::Switching => "switching",
            Phase::Recorded => "recorded",
            Phase::Warming => "warming",
            Phase::Detaching => "detaching",
            Phase::Releasing => "releasing",
        }
    }
}

/// A slot's lease: to a change and its branch, or, with neither, to a warm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub token: String,
    #[serde(default)]
    pub change_id: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    pub target: String,
    pub leased_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Owner {
    pub admin: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Built {
    pub revision: String,
    pub identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotState {
    pub schema: String,
    pub name: String,
    pub phase: Phase,
    #[serde(default)]
    pub lease: Option<Lease>,
    #[serde(default)]
    pub owner: Option<Owner>,
    /// The identity digest the slot's checkout last ran under.
    #[serde(default)]
    pub identity: Option<String>,
    /// Set only by a successful preparation in this slot.
    #[serde(default)]
    pub built: Option<Built>,
}

impl SlotState {
    fn free(name: &str) -> Self {
        Self {
            schema: SLOT_SCHEMA.into(),
            name: name.into(),
            phase: Phase::Free,
            lease: None,
            owner: None,
            identity: None,
            built: None,
        }
    }

    pub fn is_free(&self) -> bool {
        self.lease.is_none() && self.phase == Phase::Free
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    crate::commands::write_atomically(path, &bytes, crate::commands::FileMode::Create(0o600))
}

fn open_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    if !file.metadata()?.file_type().is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok(file)
}

fn lock_file(path: &Path, exclusive: bool) -> Result<File> {
    let file = open_lock(path)?;
    let operation = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    loop {
        // SAFETY: flock takes a descriptor `file` owns.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            gitio::forget_answers();
            return Ok(file);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error).with_context(|| format!("cannot lock {}", path.display()));
        }
    }
}

/// The lock, or `None` when another holder has it.
fn lock_file_nonblocking(path: &Path, operation: i32) -> Result<Option<File>> {
    let file = open_lock(path)?;
    // SAFETY: flock takes a descriptor `file` owns.
    if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } == 0 {
        gitio::forget_answers();
        return Ok(Some(file));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(None);
    }
    Err(error).with_context(|| format!("cannot lock {}", path.display()))
}

fn use_lock_path(checkout: &Path) -> PathBuf {
    let name = checkout
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    checkout.with_file_name(format!("{name}.use.lock"))
}

/// The slot checkout `path` is, judged by the pool layout around it.
fn slot_checkout(path: &Path) -> Option<PathBuf> {
    let path = fs::canonicalize(path).ok()?;
    let name = path.file_name()?.to_str()?;
    let index = name.strip_prefix("slot-")?;
    if index.len() != 2 || !index.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    path.parent()?.join(POOL_FILE).is_file().then_some(path)
}

struct Held {
    checkout: PathBuf,
    file: Weak<File>,
    exclusive: bool,
}

static HELD: Mutex<Vec<Held>> = Mutex::new(Vec::new());

/// This process's live hold on a slot's use lock, if any.
fn held(checkout: &Path) -> Option<(Arc<File>, bool)> {
    let mut held = HELD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.retain(|entry| entry.file.strong_count() > 0);
    held.iter()
        .find(|entry| entry.checkout == checkout)
        .and_then(|entry| entry.file.upgrade().map(|file| (file, entry.exclusive)))
}

/// A slot's use lock. One process holds it once: a nested acquisition
/// shares the outer hold, which then passes to every holder it starts.
#[derive(Clone)]
pub struct UseLease {
    file: Arc<File>,
}

impl UseLease {
    /// Exclusive use without waiting, or `None` while anything uses the slot.
    pub fn exclusive(checkout: &Path) -> Result<Option<Self>> {
        if let Some((file, exclusive)) = held(checkout) {
            return Ok(exclusive.then_some(Self { file }));
        }
        Ok(
            lock_file_nonblocking(&use_lock_path(checkout), libc::LOCK_EX)?
                .map(|file| Self::register(checkout, file, true)),
        )
    }

    /// Shared use, waiting for an operation that changes the checkout.
    fn shared(checkout: &Path) -> Result<Self> {
        if let Some((file, _)) = held(checkout) {
            return Ok(Self { file });
        }
        let file = lock_file(&use_lock_path(checkout), false)?;
        Ok(Self::register(checkout, file, false))
    }

    fn register(checkout: &Path, file: File, exclusive: bool) -> Self {
        let file = Arc::new(file);
        HELD.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Held {
                checkout: checkout.to_path_buf(),
                file: Arc::downgrade(&file),
                exclusive,
            });
        Self { file }
    }
}

/// Exclusive use of the slot checkout holding `cwd`, for an operation that
/// changes its source; `None` outside slots. A busy slot refuses.
pub fn exclusive_in(cwd: &Path) -> Result<Option<UseLease>> {
    let Some(checkout) = gitio::toplevel(cwd)
        .ok()
        .and_then(|top| slot_checkout(&top))
    else {
        return Ok(None);
    };
    match UseLease::exclusive(&checkout)? {
        Some(lease) => Ok(Some(lease)),
        None => bail!(
            "slot checkout {} is in use by a running command; retry when it finishes",
            checkout.display()
        ),
    }
}

/// A shell that runs a command in `cwd`. In a slot checkout the shell runs
/// under a use holder that keeps the slot in use until its last descendant
/// has exited; elsewhere it is plain `sh`.
pub struct Shell {
    pub command: crate::project_command::Command,
    report: Option<(OwnedFd, OwnedFd)>,
    control: Option<(OwnedFd, OwnedFd)>,
    _lease: Option<UseLease>,
    _exclusive: Option<File>,
}

pub fn shell(cwd: &Path) -> Result<Shell> {
    let Some(checkout) = slot_checkout(cwd) else {
        return Ok(Shell {
            command: crate::project_command::new("sh", cwd, "slot shell")?,
            report: None,
            control: None,
            _lease: None,
            _exclusive: None,
        });
    };
    held_shell(cwd, Some(UseLease::shared(&checkout)?), None, false)
}

/// A command holder that accounts for the command's entire descendant tree.
pub fn measured_shell(cwd: &Path) -> Result<(Shell, u64)> {
    measured_shell_with_lock(cwd, None)
}

pub fn measured_shell_with_lock(cwd: &Path, exclusive: Option<File>) -> Result<(Shell, u64)> {
    let (lease, wait_ms) = match slot_checkout(cwd) {
        Some(checkout) => {
            let waiting = Instant::now();
            let lease = UseLease::shared(&checkout)?;
            (Some(lease), waiting.elapsed().as_millis() as u64)
        }
        None => (None, 0),
    };
    let shell = held_shell(cwd, lease, exclusive, true)?;
    Ok((shell, wait_ms))
}

fn pipe_pair() -> Result<(OwnedFd, OwnedFd)> {
    let mut descriptors = [0; 2];
    // SAFETY: pipe2 writes two descriptors into the array on success.
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot create a holder pipe");
    }
    // SAFETY: pipe2 succeeded, so both descriptors are open and owned here.
    Ok(unsafe {
        (
            OwnedFd::from_raw_fd(descriptors[0]),
            OwnedFd::from_raw_fd(descriptors[1]),
        )
    })
}

const STOP_FD: &str = "ARC_COMMAND_STOP_FD";
const STOP_ON_DISCONNECT: &str = "ARC_COMMAND_STOP_ON_DISCONNECT";

fn held_shell(
    cwd: &Path,
    lease: Option<UseLease>,
    exclusive: Option<File>,
    measured: bool,
) -> Result<Shell> {
    let (read, write) = pipe_pair()?;
    let control = (measured || exclusive.is_some())
        .then(pipe_pair)
        .transpose()?;
    let mut command = crate::project_command::new(
        std::env::current_exe().context("cannot locate arc")?,
        cwd,
        "slot command holder",
    )?;
    command
        .arg(HOLDER)
        .arg("sh")
        .env_remove(STOP_FD)
        .env(
            STOP_ON_DISCONNECT,
            if exclusive.is_some() { "1" } else { "0" },
        )
        .env(STATUS_FD, write.as_raw_fd().to_string());
    let mut inherited = vec![write.as_raw_fd()];
    if let Some(lease) = &lease {
        inherited.push(lease.file.as_raw_fd());
    }
    if let Some(lock) = &exclusive {
        inherited.push(lock.as_raw_fd());
    }
    if let Some((read, _)) = &control {
        inherited.push(read.as_raw_fd());
        command.env(STOP_FD, read.as_raw_fd().to_string());
    }
    // SAFETY: fcntl is async-signal-safe. Only the holder clears CLOEXEC, so
    // no unrelated exec inherits the use lock or the report pipe.
    unsafe {
        command.pre_exec(move || {
            for &descriptor in &inherited {
                let flags = libc::fcntl(descriptor, libc::F_GETFD);
                if flags < 0
                    || libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok(Shell {
        command,
        report: Some((read, write)),
        control,
        _lease: lease,
        _exclusive: exclusive,
    })
}

impl Shell {
    /// Spawn the shell as the leader of its own process group.
    pub fn spawn(&mut self) -> std::io::Result<OwnedGroup> {
        let mut group = OwnedGroup::spawn(&mut self.command)?;
        if let Some((read, write)) = self.report.take() {
            drop(write);
            group.report_through(read)?;
        }
        if let Some((read, write)) = self.control.take() {
            drop(read);
            group.stop_through(write);
        }
        Ok(group)
    }
}

/// The use holder: run the command it was given in this process group,
/// report its exit, then keep the inherited use lock until no descendant is
/// left. As a child subreaper it inherits every descendant whose parent
/// exits, including ones that detach into another session.
pub fn holder_main() -> ! {
    use crate::verification_timing::{host_at_start, monotonic_ns, wait_usage, CommandReport};
    use std::io::Write;
    let arguments: Vec<OsString> = std::env::args_os().skip(2).collect();
    let report = std::env::var(STATUS_FD)
        .ok()
        .and_then(|value| value.parse::<i32>().ok());
    let stop_on_disconnect = std::env::var(STOP_ON_DISCONNECT).is_ok_and(|v| v == "1");
    let control = std::env::var(STOP_FD)
        .ok()
        .and_then(|value| value.parse::<i32>().ok());
    // SAFETY: prctl and signal take no borrowed memory. The holder must
    // survive a closed report pipe once its launcher is gone.
    let subreaper = unsafe {
        let result = libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        if let Some(report) = report {
            libc::fcntl(report, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        if let Some(control) = control {
            libc::fcntl(control, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        result == 0
    };
    if control.is_some() && !subreaper {
        std::process::exit(127);
    }
    let mut report = report.map(|fd| {
        // SAFETY: the launcher passed ownership of this report descriptor.
        unsafe { File::from_raw_fd(fd) }
    });
    let mut send = |message: &CommandReport| {
        if let Some(report) = &mut report {
            if let Ok(mut bytes) = serde_json::to_vec(message) {
                bytes.push(b'\n');
                let _ = report.write_all(&bytes);
            }
        }
    };
    let Some((program, rest)) = arguments.split_first() else {
        std::process::exit(2);
    };
    let mut observation = CommandReport {
        timing: crate::verification_timing::VerificationTiming {
            host_cpu: crate::gate_work::Host::read().map(Box::new),
            ..host_at_start()
        },
        started_ns: monotonic_ns(),
        ..Default::default()
    };
    #[allow(
        clippy::disallowed_methods,
        reason = "The command holder inherits its typed launcher's kernel restriction."
    )]
    let mut command = crate::child_command::command(program);
    command
        .args(rest)
        .env_remove(STATUS_FD)
        .env_remove(STOP_FD)
        .env_remove(STOP_ON_DISCONNECT);
    if control.is_some() {
        command.process_group(0);
    }
    let host_sample = crate::gate_work::HostSample::start();
    let pending_counter = crate::instruction_counter::prepare(&mut command);
    let spawned = command.spawn();
    let counter = if spawned.is_ok() {
        pending_counter.and_then(|pending| pending.receive())
    } else {
        None
    };
    send(&observation);
    // The launcher reads the command's output to its end; only the command
    // and its descendants may hold those pipes open.
    // SAFETY: open and dup2 take a NUL-terminated literal and descriptors.
    unsafe {
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if null >= 0 {
            for target in 0..3 {
                libc::dup2(null, target);
            }
            if null > 2 {
                libc::close(null);
            }
        }
    }
    if let (Some(control), Ok(child)) = (control, &spawned) {
        let pid = child.id();
        let stop = std::thread::spawn(move || {
            // SAFETY: the launcher passed ownership of this pipe descriptor.
            let mut pipe = unsafe { File::from_raw_fd(control) };
            let mut byte = [0];
            let explicit = loop {
                match std::io::Read::read(&mut pipe, &mut byte) {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Ok(n) => break n > 0,
                    Err(_) => break false,
                }
            };
            // The command remains unreaped until this thread has used its group ID.
            if explicit || stop_on_disconnect {
                let _ = process_group::kill_process_group(pid);
            }
            explicit
        });
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: waitid initializes info and leaves the command's PID reserved.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        } == 0
        {
            // SAFETY: waitid initialized info.
            let info = unsafe { info.assume_init() };
            let status = unsafe { info.si_status() };
            crate::gate_work::check_gate_affinity(&mut observation.timing, pid);
            observation.status = Some(if info.si_code == libc::CLD_EXITED {
                status << 8
            } else {
                status
            });
            send(&observation);
        } else if let Some(host) = &mut observation.timing.host_cpu {
            host.unrestricted = false;
        }
        let _ = stop.join();
    }
    let command_group = spawned.as_ref().ok().map(std::process::Child::id);
    let (raw, mut user, mut system, mut complete) =
        match spawned.and_then(|child| wait_usage(child.id() as i32, 0)) {
            Ok(Some((raw, user, system))) => (raw, user, system, subreaper),
            _ => (127 << 8, 0, 0, false),
        };
    observation.status = Some(raw);
    // Reap adopted children already done before publishing the leader's exit.
    // A complete report accompanies that exit whenever no descendant remains.
    let mut flags = libc::WNOHANG;
    loop {
        match wait_usage(-1, flags) {
            Ok(Some((_, child_user, child_system))) => {
                user += child_user;
                system += child_system;
                flags = libc::WNOHANG;
            }
            Ok(None) => {
                let escaped_only = command_group.is_some_and(|group| {
                    std::fs::read_to_string("/proc/thread-self/children")
                        .ok()
                        .is_some_and(|children| {
                            let pids = children
                                .split_whitespace()
                                .filter_map(|p| p.parse::<i32>().ok())
                                .collect::<Vec<_>>();
                            !pids.is_empty()
                                && pids.iter().all(|pid| {
                                    crate::gate_work::check_gate_affinity(
                                        &mut observation.timing,
                                        *pid as u32,
                                    );
                                    // SAFETY: the adopted, unreaped child still owns this PID.
                                    unsafe { libc::getpgid(*pid) != group as i32 }
                                })
                        })
                });
                if escaped_only {
                    observation.stopped = control.is_some();
                    send(&observation);
                }
                flags = 0;
            }
            Err(error) => {
                complete &= error.raw_os_error() == Some(libc::ECHILD);
                break;
            }
        }
    }
    if complete {
        observation.timing.cpu_user_ms = Some(user / 1000);
        observation.timing.cpu_system_ms = Some(system / 1000);
        observation.timing.tree_cpu_ms = counter
            .as_ref()
            .and_then(|counter| counter.cpu_ms())
            .map(Box::new);
        observation.timing.instructions = counter.and_then(|counter| counter.finish());
        // A clock below waited user CPU alone cannot have followed the tree.
        // Smaller gaps are judged against the reference's, where both are
        // known: wait also reports process teardown the clock never sees.
        if observation
            .timing
            .tree_cpu_ms
            .as_deref()
            .is_some_and(|cpu| cpu.saturating_add(2) < user / 1000)
        {
            observation.timing.instructions = None;
            observation.timing.tree_cpu_ms = None;
        }
        if let Some(sample) = host_sample {
            sample.finish(&mut observation.timing);
        }
    }
    #[cfg(debug_assertions)]
    if let Ok(path) = std::env::var("ARC_TEST_HOLDER_FINAL_SOCKET") {
        if let Ok(mut socket) = std::os::unix::net::UnixStream::connect(path) {
            let _ = socket.write_all(b"+");
            let _ = std::io::Read::read_exact(&mut socket, &mut [0_u8]);
        }
    }
    observation.stopped = control.is_some();
    send(&observation);
    let code = if libc::WIFEXITED(raw) {
        libc::WEXITSTATUS(raw)
    } else {
        128 + libc::WTERMSIG(raw)
    };
    std::process::exit(code)
}

/// Run `script` in `cwd`, returning its exit code and its output.
fn captured(
    shell: &mut Shell,
    cwd: &Path,
    script: &str,
    timeout: Duration,
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    let deadline = Instant::now() + timeout;
    shell
        .command
        .args(["-c", script])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut group = shell.spawn().context("cannot start command")?;
    let mut stdout = group.child().stdout.take().context("stdout unavailable")?;
    let mut stderr = group.child().stderr.take().context("stderr unavailable")?;
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut bytes).map(|_| bytes)
    });
    let err = std::thread::spawn(move || {
        process_group::read_tail(&mut stderr, process_group::STDERR_TAIL_BYTES)
    });
    let mut exited = false;
    loop {
        if !exited {
            exited = group.leader_exited()?;
        }
        if exited && out.is_finished() && err.is_finished() {
            break;
        }
        if Instant::now() >= deadline {
            group.stop()?;
            bail!("`{script}` exceeded {}s and was stopped", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = group.stop()?;
    let out = process_group::join_before_deadline(out, Some(deadline), "stdout reader")?
        .transpose()?
        .unwrap_or_default();
    let err = process_group::join_before_deadline(err, Some(deadline), "stderr reader")?
        .transpose()?
        .unwrap_or_default();
    let code = status.code().unwrap_or_else(|| {
        128 + std::os::unix::process::ExitStatusExt::signal(&status).unwrap_or(0)
    });
    Ok((code, out, err.into_bytes()))
}

/// The digest of the declared identity command's output in `checkout`. A
/// pool without one has a single fixed identity.
pub fn identity(checkout: &Path, declaration: &Declaration) -> Result<String> {
    let Some(script) = &declaration.identity else {
        return Ok(hex::encode(Sha256::digest(b"arc-slot-identity/1\nnone\n")));
    };
    let mut shell = Shell {
        command: crate::project_command::new("sh", checkout, "slot identity")?,
        report: None,
        control: None,
        _lease: None,
        _exclusive: None,
    };
    let (code, out, err) = captured(&mut shell, checkout, script, IDENTITY_TIMEOUT)
        .context("slot identity command failed")?;
    if code != 0 {
        bail!(
            "slot identity command exited {code}: {}",
            String::from_utf8_lossy(&err).trim()
        );
    }
    let mut digest = Sha256::new();
    digest.update(b"arc-slot-identity/1\n");
    digest.update((out.len() as u64).to_le_bytes());
    digest.update(&out);
    Ok(hex::encode(digest.finalize()))
}

/// Record a successful preparation in slot checkout `path` as its built
/// revision, under the identity the checkout runs with now. Outside slots
/// nothing is recorded.
pub fn record_built(path: &Path) -> Result<()> {
    let Some(checkout) = slot_checkout(path) else {
        return Ok(());
    };
    let Some(declaration) = crate::warm_worktree::slot_declaration(&checkout, false)? else {
        return Ok(());
    };
    let root = checkout.parent().context("slot has no pool")?;
    let pool = Pool::existing(&checkout, root)?;
    let name = checkout
        .file_name()
        .and_then(|name| name.to_str())
        .context("slot name is not UTF-8")?
        .to_string();
    let identity = identity(&checkout, &declaration)?;
    let _lock = pool.lock()?;
    let revision = gitio::fresh_read(|| gitio::head(&checkout))?;
    let mut state = pool.read(&name)?;
    state.built = Some(Built { revision, identity });
    pool.write(&state)
}

/// Where a free slot stands for a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    /// Paths that differ between its built revision and the target,
    /// documentation excluded.
    Known(usize),
    /// Built under this identity, at a revision that cannot be compared.
    Unknown,
    /// No matching build, or no checkout.
    Cold,
}

pub fn rank(
    pool: &Pool,
    cwd: &Path,
    state: &SlotState,
    target: &str,
    declaration: &Declaration,
    identity: &str,
) -> Rank {
    if !pool.checkout(&state.name).exists() {
        return Rank::Cold;
    }
    let Some(built) = state
        .built
        .as_ref()
        .filter(|built| built.identity == identity)
    else {
        return Rank::Cold;
    };
    match gitio::git(
        cwd,
        &["diff", "--name-only", "-z", &built.revision, target, "--"],
    ) {
        Ok(names) => Rank::Known(
            names
                .split('\0')
                .filter(|name| !name.is_empty())
                .filter(|name| {
                    !declaration
                        .documentation
                        .iter()
                        .any(|pattern| matches(pattern, name))
                })
                .count(),
        ),
        Err(_) => Rank::Unknown,
    }
}

/// What a lease asks of the pool: a change and its branch, or, with
/// neither, a warm.
pub struct LeaseRequest<'a> {
    pub change_id: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub target: &'a str,
    pub actor: Option<&'a str>,
    pub session: Option<&'a str>,
}

/// A slot leased for one change or one warm, held exclusively until it is
/// dropped.
pub struct Leased {
    pub pool: Pool,
    pub name: String,
    pub token: String,
    pub path: PathBuf,
    _use: UseLease,
}

/// Lease the best-ranked free slot whose use lock is free. A slot whose
/// state cannot be read is never handed out; an exhausted pool refuses with
/// its table.
pub fn lease(
    cwd: &Path,
    pool: Pool,
    declaration: &Declaration,
    request: &LeaseRequest<'_>,
) -> Result<Leased> {
    let identity = identity(&gitio::toplevel(cwd)?, declaration)?;
    match select(cwd, pool, declaration, request, &identity, |_, _| true)? {
        Selected::Leased(leased) => Ok(leased),
        Selected::None { pool, table, .. } => bail!(
            "no free slot in the pool at {} (size {}):\n  {}\n\
             inspect it with `arc slots status`; free a lease whose holder is gone with \
             `arc slots release <slot> --token <token>`, or begin with --worktree",
            pool.root.display(),
            pool.size,
            table.join("\n  ")
        ),
    }
}

/// Whether a slot holds a build of `target` under `identity`: its checkout
/// exists and its `built` record names both.
pub fn current(pool: &Pool, state: &SlotState, target: &str, identity: &str) -> bool {
    pool.checkout(&state.name).exists()
        && state
            .built
            .as_ref()
            .is_some_and(|built| built.revision == target && built.identity == identity)
}

/// What selecting a slot found.
pub enum Selected {
    Leased(Leased),
    /// No eligible free slot: the table of the others, and how many free
    /// slots the filter passed over.
    None {
        pool: Pool,
        table: Vec<String>,
        passed_over: usize,
    },
}

/// Lease the best-ranked free slot `eligible` accepts and whose use lock is
/// free, under the pool lock, then take its use lock exclusively.
pub fn select(
    cwd: &Path,
    pool: Pool,
    declaration: &Declaration,
    request: &LeaseRequest<'_>,
    identity: &str,
    eligible: impl Fn(&Pool, &SlotState) -> bool,
) -> Result<Selected> {
    let mut busy: Vec<String> = Vec::new();
    loop {
        let (name, token) = {
            let _lock = pool.lock()?;
            let mut candidates = Vec::new();
            let mut table = Vec::new();
            let mut passed_over = 0;
            for name in pool.names() {
                match pool.read(&name) {
                    Ok(state) if state.is_free() && !busy.contains(&name) => {
                        if eligible(&pool, &state) {
                            let rank =
                                rank(&pool, cwd, &state, request.target, declaration, identity);
                            candidates.push((rank, state));
                        } else {
                            passed_over += 1;
                        }
                    }
                    Ok(state) => table.push(describe(&state, busy.contains(&name))),
                    Err(error) => table.push(format!("{name}  unreadable: {error:#}")),
                }
            }
            candidates.sort_by_key(|(rank, _)| *rank);
            let Some((_, mut state)) = candidates.into_iter().next() else {
                drop(_lock);
                return Ok(Selected::None {
                    pool,
                    table,
                    passed_over,
                });
            };
            let token = crate::ids::new_identifier();
            state.phase = Phase::Leasing;
            state.lease = Some(Lease {
                token: token.clone(),
                change_id: request.change_id.map(Into::into),
                branch: request.branch.map(Into::into),
                target: request.target.into(),
                leased_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                actor: request.actor.map(Into::into),
                session: request.session.map(Into::into),
            });
            pool.write(&state)?;
            (state.name, token)
        };
        let path = pool.checkout(&name);
        if let Some(lease) = UseLease::exclusive(&path)? {
            return Ok(Selected::Leased(Leased {
                pool,
                name,
                token,
                path,
                _use: lease,
            }));
        }
        pool.free(&name, &token)?;
        busy.push(name);
    }
}

fn describe(state: &SlotState, busy: bool) -> String {
    match &state.lease {
        Some(Lease {
            change_id: Some(change_id),
            branch,
            leased_at,
            ..
        }) => format!(
            "{}  {}  change {change_id} on {} since {leased_at}",
            state.name,
            state.phase.label(),
            branch.as_deref().unwrap_or("?"),
        ),
        Some(lease) => format!(
            "{}  {}  warming {} since {}",
            state.name,
            state.phase.label(),
            lease.target,
            lease.leased_at
        ),
        None if busy => format!("{}  free, in use by a running command", state.name),
        None => format!("{}  {}", state.name, state.phase.label()),
    }
}

impl Leased {
    fn phase(&self, phase: Phase) -> Result<SlotState> {
        self.pool
            .update(&self.name, &self.token, |state| state.phase = phase)
    }

    /// Put the change's branch at `base` in the slot, then prepare the
    /// checkout.
    pub fn switch(
        &self,
        cwd: &Path,
        declaration: &Declaration,
        base: &str,
        open_change: Option<&str>,
    ) -> Result<()> {
        self.place(cwd, declaration, base, open_change)?;
        crate::warm_worktree::prepare(&self.path)?;
        Ok(())
    }

    /// Put the slot at `base`, on the lease's branch or, for a warm,
    /// detached: create the checkout when absent, else switch a clean,
    /// detached, owned checkout holding no open change and no ignored path
    /// outside `retain`. On a changed identity, discard the outputs built
    /// under the previous one.
    fn place(
        &self,
        cwd: &Path,
        declaration: &Declaration,
        base: &str,
        open_change: Option<&str>,
    ) -> Result<()> {
        let state = self.phase(Phase::Switching)?;
        let lease = state.lease.as_ref().context("slot lease vanished")?;
        let branch = lease.branch.as_deref();
        let path = &self.path;
        let fresh = !path.exists();
        if fresh {
            match branch {
                Some(branch) => {
                    if gitio::branch_exists(cwd, branch) {
                        bail!("branch {branch:?} already exists");
                    }
                    gitio::add_worktree_new_branch(cwd, path, branch, base)?;
                }
                None => {
                    gitio::git(
                        cwd,
                        &[
                            "worktree",
                            "add",
                            "--no-checkout",
                            "--detach",
                            path.to_str().context("non-UTF8 slot path")?,
                            base,
                        ],
                    )?;
                }
            }
            let admin = gitio::worktree_admin_dir(path)?;
            let token = crate::ids::new_identifier();
            gitio::write_worktree_owner_with_token(
                &admin,
                &self.pool.owner_id(&self.name),
                &token,
            )?;
            self.pool.update(&self.name, &self.token, |state| {
                state.owner = Some(Owner {
                    admin: admin.display().to_string(),
                    token,
                });
                state.identity = None;
                state.built = None;
            })?;
            gitio::populate_checkout(path)?;
        } else {
            self.pool.require_owned(cwd, &state)?;
            if let Some(change) = open_change {
                bail!(
                    "{} at {} still holds open change {change}",
                    self.name,
                    path.display()
                );
            }
            if let Some(branch) = gitio::fresh_read(|| gitio::current_branch(path))? {
                bail!(
                    "{} at {} has branch {branch} checked out; a free slot is detached",
                    self.name,
                    path.display()
                );
            }
            require_reusable(path, &self.name, &declaration.retain)?;
            match branch {
                Some(branch) => {
                    gitio::git(path, &["switch", "--no-guess", "-c", branch, base])?;
                }
                None => {
                    gitio::git(path, &["switch", "--detach", base])?;
                }
            }
        }
        let identity = identity(path, declaration)?;
        let recorded = self.pool.read(&self.name)?.identity;
        if recorded.as_deref() != Some(identity.as_str()) && !(fresh && recorded.is_none()) {
            invalidate(path, declaration, cwd)?;
        }
        self.pool.update(&self.name, &self.token, |state| {
            state.identity = Some(identity)
        })?;
        Ok(())
    }

    /// Warm the slot at `target`: detach it there, then run `script` through
    /// the preparation and environment gates use, under a holder that
    /// shares this lease's exclusive use. A successful run that leaves the
    /// checkout reusable records `built`, and the lease is freed.
    pub fn warm(
        &self,
        cwd: &Path,
        declaration: &Declaration,
        script: &str,
        open_change: Option<&str>,
    ) -> Result<()> {
        let target = self
            .pool
            .read(&self.name)?
            .lease
            .context("slot lease vanished")?
            .target;
        self.place(cwd, declaration, &target, open_change)?;
        self.phase(Phase::Warming)?;
        let mut shell = shell(&self.path)?;
        shell
            .command
            .args(["-c", script])
            .current_dir(&self.path)
            .stdin(Stdio::null())
            .process_group(0);
        let _storage = crate::warm_worktree::prepare_command(&mut shell.command, &self.path)?;
        let _watch =
            crate::interruption::Watch::start().context("cannot watch for interruption")?;
        crate::interruption::check()?;
        let mut group = shell.spawn().context("cannot start the warm command")?;
        loop {
            if group.leader_exited()? {
                break;
            }
            if let Err(error) = crate::interruption::check() {
                group.stop()?;
                return Err(error.into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let status = group.stop()?;
        if !status.success() {
            bail!(
                "slot warm command exited {}",
                status.code().map_or_else(
                    || format!(
                        "on signal {}",
                        std::os::unix::process::ExitStatusExt::signal(&status).unwrap_or(0)
                    ),
                    |code| code.to_string()
                )
            );
        }
        if gitio::fresh_read(|| gitio::head(&self.path))? != target
            || gitio::fresh_read(|| gitio::current_branch(&self.path))?.is_some()
        {
            bail!(
                "the warm command moved {} off {target}",
                self.path.display()
            );
        }
        require_reusable(&self.path, &self.name, &declaration.retain)?;
        record_built(&self.path)?;
        self.pool.free(&self.name, &self.token)
    }

    /// Hold `commit` detached in the slot for a landing's gate run: create
    /// the checkout when absent, else switch a clean, detached, owned
    /// checkout holding no open change and no ignored path outside `retain`.
    /// On a changed identity, discard the outputs built under the previous
    /// one; then prepare the checkout. The exclusive use lock is let go, so
    /// gates take it shared like any command run in a slot; the lease stays
    /// until [`release_held`].
    pub fn hold_detached(
        self,
        cwd: &Path,
        declaration: &Declaration,
        commit: &str,
        open_change: Option<&str>,
    ) -> Result<HeldSlot> {
        let held = (|| -> Result<()> {
            self.phase(Phase::Switching)?;
            let path = &self.path;
            let fresh = !path.exists();
            if fresh {
                gitio::git(
                    cwd,
                    &[
                        "worktree",
                        "add",
                        "--no-checkout",
                        "--detach",
                        path.to_str().context("non-UTF8 worktree path")?,
                        commit,
                    ],
                )?;
                let admin = gitio::worktree_admin_dir(path)?;
                let token = crate::ids::new_identifier();
                gitio::write_worktree_owner_with_token(
                    &admin,
                    &self.pool.owner_id(&self.name),
                    &token,
                )?;
                self.pool.update(&self.name, &self.token, |state| {
                    state.owner = Some(Owner {
                        admin: admin.display().to_string(),
                        token,
                    });
                    state.identity = None;
                    state.built = None;
                })?;
                gitio::populate_checkout(path)?;
            } else {
                let state = self.pool.read(&self.name)?;
                self.pool.require_owned(cwd, &state)?;
                if let Some(change) = open_change {
                    bail!(
                        "{} at {} still holds open change {change}",
                        self.name,
                        path.display()
                    );
                }
                if let Some(branch) = gitio::fresh_read(|| gitio::current_branch(path))? {
                    bail!(
                        "{} at {} has branch {branch} checked out; a free slot is detached",
                        self.name,
                        path.display()
                    );
                }
                require_reusable(path, &self.name, &declaration.retain)?;
                gitio::git(path, &["switch", "--detach", commit])?;
            }
            let identity = identity(path, declaration)?;
            let recorded = self.pool.read(&self.name)?.identity;
            if recorded.as_deref() != Some(identity.as_str()) && !(fresh && recorded.is_none()) {
                invalidate(path, declaration, cwd)?;
            }
            self.pool.update(&self.name, &self.token, |state| {
                state.identity = Some(identity)
            })?;
            crate::warm_worktree::prepare(path)?;
            self.phase(Phase::Recorded).map(|_| ())
        })();
        if let Err(error) = held {
            return Err(error.context(self.kept()));
        }
        Ok(HeldSlot {
            pool: self.pool,
            name: self.name,
            token: self.token,
            path: self.path,
        })
    }

    /// Mark the lease recorded on its change.
    pub fn recorded(&self) -> Result<()> {
        self.phase(Phase::Recorded).map(|_| ())
    }

    /// The recovery a failed begin or warm leaves to its operator.
    pub fn kept(&self) -> String {
        format!(
            "{} at {} keeps its lease for inspection; once resolved, run \
             `arc slots recover {} --token {}`",
            self.name,
            self.path.display(),
            self.name,
            self.token
        )
    }
}

/// Refuse a checkout carrying source changes or ignored paths outside
/// `retain`, naming the first offender.
fn require_reusable(path: &Path, name: &str, retain: &[String]) -> Result<()> {
    let status = gitio::git(
        path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--ignored=traditional",
            "--untracked-files=normal",
        ],
    )?;
    for entry in status.split('\0').filter(|entry| entry.len() > 3) {
        let (code, file) = entry.split_at(3);
        if code != "!! " {
            bail!(
                "{name} at {} carries {} ({}); resolve it before the slot is reused",
                path.display(),
                file,
                code.trim()
            );
        }
        if retained(retain, file)
            || file.trim_end_matches('/') == ".arc/outbox" && empty_outbox(path)
        {
            continue;
        }
        bail!(
            "{name} at {} carries ignored {file}, which [slots] retain does not list; \
             remove it or declare it retained before the slot is reused",
            path.display()
        );
    }
    Ok(())
}

fn empty_outbox(path: &Path) -> bool {
    let outbox = path.join(".arc/outbox");
    let Ok(entries) = fs::read_dir(&outbox) else {
        return false;
    };
    let entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.len() == 1
        && entries[0].file_name() == ".gitignore"
        && fs::read(entries[0].path()).is_ok_and(|bytes| bytes == b"*\n")
}

/// Discard outputs built under another identity: the project's
/// `invalidate` command with the gate environment, or without one the
/// retained paths and arc-owned storage keyed by the checkout.
fn invalidate(path: &Path, declaration: &Declaration, cwd: &Path) -> Result<()> {
    if let Some(script) = &declaration.invalidate {
        let lease = UseLease::exclusive(path)?.context("slot is in use")?;
        let mut shell = held_shell(path, Some(lease), None, false)?;
        let _storage = crate::warm_worktree::configure_invalidation(&mut shell.command, path)?;
        let (code, _, err) = captured(&mut shell, path, script, INVALIDATE_TIMEOUT)?;
        if code != 0 {
            bail!(
                "slot invalidate command exited {code}: {}",
                String::from_utf8_lossy(&err).trim()
            );
        }
        println!("slot: identity changed; ran invalidate, the next build is cold");
        return Ok(());
    }
    if let crate::warm_worktree::Storage::External(directory) = crate::warm_worktree::storage(path)?
    {
        bail!(
            "slot identity changed and path-stable storage {} is not arc's to clear; \
             declare [slots] invalidate to discard it",
            directory.display()
        );
    }
    let _ = cwd;
    let mut removed = Vec::new();
    let status = gitio::git(
        path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--ignored=traditional",
            "--untracked-files=normal",
        ],
    )?;
    for entry in status.split('\0').filter(|entry| entry.starts_with("!! ")) {
        let file = entry[3..].trim_end_matches('/');
        if retained(&declaration.retain, file) {
            let target = path.join(file);
            let metadata = fs::symlink_metadata(&target)?;
            if metadata.is_dir() {
                fs::remove_dir_all(&target)
            } else {
                fs::remove_file(&target)
            }
            .with_context(|| format!("cannot remove {}", target.display()))?;
            removed.push(target);
        }
    }
    removed.extend(crate::warm_worktree::clear_owned_storage(path)?);
    println!(
        "slot: identity changed; removed {} retained path(s), the next build is cold",
        removed.len()
    );
    Ok(())
}

/// A slot leased with no change recorded on it, its checkout holding one
/// commit detached.
pub struct HeldSlot {
    pub pool: Pool,
    pub name: String,
    pub token: String,
    pub path: PathBuf,
}

/// Free a held slot, its checkout left detached where it is. A checkout a
/// command still runs in, or one left dirty, keeps its lease, and the
/// returned line names the release to run once it is resolved.
pub fn release_held(held: &HeldSlot, cwd: &Path) -> Result<Option<String>> {
    match detach(&held.pool, cwd, &held.name, &held.token, None, None)? {
        Finished::Detached(_use) => {
            held.pool.free(&held.name, &held.token)?;
            Ok(None)
        }
        Finished::Kept(reason) => Ok(Some(format!(
            "{reason}; {} keeps its lease: once resolved, run `arc slots release {} --token {}`",
            held.name, held.name, held.token
        ))),
    }
}

/// What finishing a lease did with the checkout.
pub enum Finished {
    /// Detached and ready to free once the change records the release.
    Detached(UseLease),
    /// Kept leased, with the reason.
    Kept(String),
}

/// Detach a leased slot so its lease can be released: exclusive use,
/// ownership and a clean checkout are required, and a refusal keeps the
/// lease. `landed` is where to detach (the checkout's `HEAD` otherwise);
/// `merged` names a branch to delete with `git branch -d` from `cwd`.
pub fn detach(
    pool: &Pool,
    cwd: &Path,
    name: &str,
    token: &str,
    landed: Option<&str>,
    merged: Option<&str>,
) -> Result<Finished> {
    let path = pool.checkout(name);
    let Some(lease) = UseLease::exclusive(&path)? else {
        return Ok(Finished::Kept(format!(
            "a command still runs in {}",
            path.display()
        )));
    };
    let state = pool.read(name)?;
    if state.lease.as_ref().map(|lease| lease.token.as_str()) != Some(token) {
        bail!("token {token} does not hold the lease on {name}");
    }
    if path.exists() {
        pool.require_owned(cwd, &state)?;
        if !gitio::is_clean(&path)? {
            return Ok(Finished::Kept(format!(
                "{} has uncommitted or untracked work",
                path.display()
            )));
        }
        pool.update(name, token, |state| state.phase = Phase::Detaching)?;
        let at = match landed {
            Some(commit) => commit.to_string(),
            None => gitio::fresh_read(|| gitio::head(&path))?,
        };
        gitio::git(&path, &["switch", "--detach", &at])?;
    } else {
        pool.update(name, token, |state| state.phase = Phase::Detaching)?;
    }
    if let Some(branch) = merged {
        match gitio::git(cwd, &["branch", "-d", branch]) {
            Ok(_) => println!("deleted branch {branch}"),
            Err(error) => println!(
                "kept branch {branch}: Git refused: {}",
                format!("{error:#}")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        }
    }
    pool.update(name, token, |state| state.phase = Phase::Releasing)?;
    Ok(Finished::Detached(lease))
}

/// One slot as `arc slots status` reports it.
#[derive(Serialize)]
pub struct SlotView {
    pub name: String,
    pub path: String,
    pub phase: Option<String>,
    pub lease: Option<Lease>,
    pub head: Option<String>,
    pub checkout: &'static str,
    pub in_use: bool,
    pub built: Option<Built>,
    pub identity_matches: Option<bool>,
    pub rank: Option<String>,
    /// The change a lease names, as the ledger knows it.
    pub holder: Option<Holder>,
    pub error: Option<String>,
}

/// The change holding a lease: `state` is `open`, its closure outcome,
/// `absent` when the ledger does not hold it, or `unreadable`.
#[derive(Serialize)]
pub struct Holder {
    pub change_id: String,
    pub slug: Option<String>,
    pub state: String,
}

pub fn view(
    pool: &Pool,
    cwd: &Path,
    declaration: &Declaration,
    identity: Option<&str>,
    target: Option<&str>,
) -> Vec<SlotView> {
    pool.names()
        .into_iter()
        .map(|name| {
            let path = pool.checkout(&name);
            let (checkout, head) = if !path.exists() {
                ("absent", None)
            } else {
                match gitio::head(&path) {
                    Ok(head) => ("present", Some(head)),
                    Err(_) => ("unreadable", None),
                }
            };
            let in_use = pool.in_use(&name);
            match pool.read(&name) {
                Ok(state) => SlotView {
                    rank: match (state.is_free(), identity, target) {
                        (true, Some(identity), Some(target)) => Some(
                            match rank(pool, cwd, &state, target, declaration, identity) {
                                Rank::Known(paths) => format!("{paths} path(s) from target"),
                                Rank::Unknown => "unknown".into(),
                                Rank::Cold => "cold".into(),
                            },
                        ),
                        _ => None,
                    },
                    identity_matches: identity.and_then(|identity| {
                        state.built.as_ref().map(|built| built.identity == identity)
                    }),
                    name: name.clone(),
                    path: path.display().to_string(),
                    phase: Some(state.phase.label().into()),
                    lease: state.lease,
                    head,
                    checkout,
                    in_use,
                    built: state.built,
                    holder: None,
                    error: None,
                },
                Err(error) => SlotView {
                    name: name.clone(),
                    path: path.display().to_string(),
                    phase: None,
                    lease: None,
                    head,
                    checkout,
                    in_use,
                    built: None,
                    identity_matches: None,
                    rank: None,
                    holder: None,
                    error: Some(format!("{error:#}")),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pool is this repository's only when its record names this Git
    /// common directory; a missing root, a root without a pool record, and
    /// another repository's pool are not. Rejects releasing a slot record
    /// that arrived from another replica against a pool it does not own.
    #[test]
    fn a_pool_is_owned_only_by_the_repository_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let common = fs::canonicalize(dir.path()).unwrap().join("common");
        fs::create_dir_all(&common).unwrap();
        let root = dir.path().join("pool");
        assert!(!owned_by(&root, &common), "a missing root");
        fs::create_dir_all(&root).unwrap();
        assert!(!owned_by(&root, &common), "a root without a pool record");
        let record = |repository: &Path| {
            serde_json::json!({"schema": POOL_SCHEMA, "repository": repository, "id": "p", "size": 1})
                .to_string()
        };
        fs::write(root.join(POOL_FILE), record(&dir.path().join("elsewhere"))).unwrap();
        assert!(!owned_by(&root, &common), "another repository's pool");
        fs::write(root.join(POOL_FILE), record(&common)).unwrap();
        assert!(owned_by(&root, &common), "this repository's pool");
    }

    /// Documentation patterns without a slash match file names at any depth;
    /// a wildcard never crosses a directory.
    #[test]
    fn patterns_match_names_and_rooted_paths() {
        assert!(matches("*.md", "README.md"));
        assert!(matches("*.md", "docs/guide/intro.md"));
        assert!(!matches("*.md", "src/main.rs"));
        assert!(matches("docs/*.txt", "docs/a.txt"));
        assert!(!matches("docs/*.txt", "docs/deep/a.txt"));
        assert!(!matches("docs/*", "other/docs/a"));
    }

    /// An ignored entry is retained when it or an ancestor matches; an
    /// unlisted sibling is not.
    #[test]
    fn retain_covers_paths_beneath_a_listed_directory() {
        let retain = vec!["target".to_string()];
        assert!(retained(&retain, "target/"));
        assert!(retained(&retain, "target/debug/build"));
        assert!(retained(&retain, "crates/a/target/"));
        assert!(!retained(&retain, "cabal.project.local"));
        assert!(!retained(&retain, "targets/"));
    }
}
