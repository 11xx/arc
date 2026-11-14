//! Project-owned build preparation with isolated or persistent output directories.

use crate::perf::Counted;
use crate::project_command::Command;
use crate::{gitio, process_group};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

mod stable;

const CONFIG: &str = ".arc/worktrees.toml";

#[derive(Debug)]
pub(crate) struct SourceMutation(String);

impl std::fmt::Display for SourceMutation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "worktree preparation changed source, index, HEAD or untracked source paths; keeping checkout for inspection")?;
        if !self.0.is_empty() {
            write!(formatter, ": {}", self.0)?;
        }
        Ok(())
    }
}

impl std::error::Error for SourceMutation {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    prepare: Option<Hook>,
    slots: Option<crate::slots::Declaration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hook {
    command: Option<String>,
    timeout: Option<String>,
    #[serde(default)]
    mode: Mode,
    directory: Option<PathBuf>,
}

#[derive(Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    #[default]
    SourceBound,
    PathStable,
}

#[derive(Clone)]
pub(crate) struct CommandStorage {
    build: PathBuf,
    cache: PathBuf,
    owned_build: bool,
}
impl CommandStorage {
    pub(crate) fn owned(&self) -> Vec<PathBuf> {
        std::iter::once(self.cache.clone())
            .chain(self.owned_build.then(|| self.build.clone()))
            .collect()
    }
    pub(crate) fn external(&self) -> Option<&Path> {
        (!self.owned_build).then_some(self.build.as_path())
    }
}

/// Keeps a configured command's mutable build storage exclusively leased.
#[derive(Default)]
pub struct CommandLease {
    files: Vec<File>,
    build_lease: Option<PathBuf>,
    storage: Option<CommandStorage>,
}

impl CommandLease {
    fn inherit(&self, command: &mut Command) {
        if let Some(storage) = &self.storage {
            command.storage(storage.clone());
        }
        if let Some(path) = &self.build_lease {
            command.env("ARC_WORKTREE_BUILD_LEASE", path).env(
                "ARC_WORKTREE_BUILD_LEASE_FD",
                self.files[0].as_raw_fd().to_string(),
            );
        }
        let descriptors: Vec<_> = self.files.iter().map(AsRawFd::as_raw_fd).collect();
        // SAFETY: fcntl is async-signal-safe. Only the owned child clears
        // CLOEXEC; every parent descriptor remains closed across unrelated execs.
        unsafe {
            command.pre_exec(move || {
                for descriptor in &descriptors {
                    let flags = libc::fcntl(*descriptor, libc::F_GETFD);
                    if flags < 0
                        || libc::fcntl(*descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
}

#[derive(PartialEq, Eq)]
struct Source {
    revision: String,
    tree: String,
    tracked: Vec<u8>,
    staged: Vec<u8>,
    untracked: Vec<u8>,
    index: Vec<u8>,
}

impl Source {
    fn changed_fields(&self, other: &Self) -> String {
        [
            ("revision", self.revision != other.revision),
            ("tree", self.tree != other.tree),
            ("tracked source", self.tracked != other.tracked),
            ("staged source", self.staged != other.staged),
            ("untracked paths", self.untracked != other.untracked),
            ("index", self.index != other.index),
        ]
        .into_iter()
        .filter_map(|(name, changed)| changed.then_some(name))
        .collect::<Vec<_>>()
        .join(", ")
    }
    fn observe(path: &Path) -> Result<Self> {
        Ok(Self {
            revision: gitio::fresh_read(|| gitio::head(path))?,
            tree: gitio::git(path, &["rev-parse", "HEAD^{tree}"])?,
            tracked: source_bytes(path, &["diff", "--binary", "HEAD", "--"])?,
            staged: source_bytes(path, &["diff", "--cached", "--binary", "HEAD", "--"])?,
            untracked: source_bytes(path, &["ls-files", "--others", "--exclude-standard", "-z"])?,
            index: index_entries(path)?,
        })
    }

    fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        for bytes in [
            self.revision.as_bytes(),
            self.tree.as_bytes(),
            &self.tracked,
            &self.staged,
            &self.untracked,
            &self.index,
        ] {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        hex::encode(digest.finalize())
    }
}

fn index_entries(path: &Path) -> Result<Vec<u8>> {
    let mut entries = Vec::new();
    for flag in ["-v", "-f"] {
        let observed = source_bytes(path, &["ls-files", "--stage", flag, "-z"])?;
        entries.extend_from_slice(&(observed.len() as u64).to_le_bytes());
        entries.extend_from_slice(&observed);
    }
    Ok(entries)
}

fn source_bytes(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = gitio::git_command()
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(path)
        .counted_output()?;
    if !output.status.success() {
        bail!(
            "cannot observe worktree preparation source: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}

/// Prepare a checkout. Source-bound cache failures continue cold; persistent
/// storage failures and source, index or HEAD mutations refuse further use.
pub fn prepare(path: &Path) -> Result<()> {
    let text = match fs::read_to_string(path.join(CONFIG)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("cannot read .arc/worktrees.toml"),
    };
    let hook = match configuration(&text) {
        Ok(Some(hook)) => hook,
        Ok(None) => return Ok(()),
        Err(error) => {
            if explicit_mode(&text)? {
                return Err(error);
            }
            cold(&format!("{error:#}"));
            return Ok(());
        }
    };
    let path = fs::canonicalize(path).context("cannot resolve preparation checkout")?;
    if hook.0.mode == Mode::PathStable {
        if stable::prepare(&path, &hook)? {
            crate::slots::record_built(&path)?;
        }
        return Ok(());
    }
    let before = Source::observe(&path)?;
    let outcome = run(&path, &before, &hook);
    if Source::observe(&path)? != before {
        return Err(SourceMutation(String::new()).into());
    }
    if let Err(error) = outcome {
        cold_storage(&path, &before, &hook)?;
        cold(&format!("{error:#}"));
    } else {
        eprintln!("worktree preparation: prepared source tree {}", before.tree);
        crate::slots::record_built(&path)?;
    }
    Ok(())
}

/// Where a checkout's prepared outputs live, for a slot whose toolchain
/// identity changed.
pub(crate) enum Storage {
    /// Outputs live in the checkout or arc-owned storage keyed by it.
    Owned,
    /// A project directory outside the checkout that arc does not own.
    External(PathBuf),
}

/// Classify the storage `.arc/worktrees.toml` in `path` declares.
pub(crate) fn storage(path: &Path) -> Result<Storage> {
    let text = match fs::read_to_string(path.join(CONFIG)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Storage::Owned),
        Err(error) => return Err(error).context("cannot read .arc/worktrees.toml"),
    };
    Ok(match configuration(&text)? {
        Some((hook, _)) if hook.mode == Mode::PathStable => match &hook.directory {
            Some(directory) => Storage::External(directory.clone()),
            None => Storage::Owned,
        },
        _ => Storage::Owned,
    })
}

/// Remove the arc-owned prepared outputs keyed by checkout `path`: its
/// source-bound receipts and builds, and its path-stable storage under the
/// Git common directory. A busy path-stable lease refuses.
pub(crate) fn clear_owned_storage(path: &Path) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    let admin = gitio::worktree_admin_dir(path)?;
    for name in ["arc-warm-build", "arc-cold-build"] {
        let directory = admin.join(name);
        if fs::symlink_metadata(&directory).is_ok() {
            fs::remove_dir_all(&directory)
                .with_context(|| format!("cannot remove {}", directory.display()))?;
            removed.push(directory);
        }
    }
    let common = fs::canonicalize(gitio::common_dir(path)?)?;
    let build = common
        .join("arc/cache/path-stable-worktrees")
        .join(source_path_key(&fs::canonicalize(path)?));
    if fs::symlink_metadata(&build).is_ok() {
        let _lease = acquire_lease(&stable::lease_path(&build)?, "path-stable build")?;
        fs::remove_dir_all(&build).with_context(|| format!("cannot remove {}", build.display()))?;
        let incomplete = stable::incomplete_path(&build)?;
        if fs::symlink_metadata(&incomplete).is_ok() {
            fs::remove_file(&incomplete)?;
        }
        removed.push(build);
    }
    Ok(removed)
}

/// Configure a slot's invalidation command with the gate environment,
/// without preparing or requiring complete preparation.
pub(crate) fn configure_invalidation(command: &mut Command, path: &Path) -> Result<CommandLease> {
    configure(command, path, Use::Invalidate)
}

fn explicit_mode(text: &str) -> Result<bool> {
    let value: toml::Value = toml::from_str(text).context("invalid .arc/worktrees.toml")?;
    Ok(value
        .get("prepare")
        .and_then(|prepare| prepare.get("mode"))
        .is_some_and(|mode| mode.as_str() != Some("source-bound")))
}

/// The slot pool `.arc/worktrees.toml` in `checkout` declares, if any. A
/// declared `[slots]` table that is malformed refuses: a pool is never
/// guessed. A file that is not TOML at all refuses only when `strict`.
pub fn slot_declaration(
    checkout: &Path,
    strict: bool,
) -> Result<Option<crate::slots::Declaration>> {
    let text = match fs::read_to_string(checkout.join(CONFIG)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot read .arc/worktrees.toml"),
    };
    let value: toml::Value = match toml::from_str(&text) {
        Ok(value) => value,
        Err(error) if strict => {
            return Err(error).context("invalid .arc/worktrees.toml");
        }
        Err(_) => return Ok(None),
    };
    if value.get("slots").is_none() {
        return Ok(None);
    }
    let config: Configuration = toml::from_str(&text).context("invalid .arc/worktrees.toml")?;
    config
        .slots
        .map(crate::slots::Declaration::validated)
        .transpose()
}

/// Whether `.arc/worktrees.toml` in `checkout` names a `[prepare]` command,
/// which runs before every gate in the evaluated checkout.
pub fn declares_prepare_command(checkout: &Path) -> bool {
    fs::read_to_string(checkout.join(CONFIG))
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
        .and_then(|value| {
            value
                .get("prepare")?
                .get("command")?
                .as_str()
                .map(str::to_owned)
        })
        .is_some_and(|command| !command.trim().is_empty())
}

fn configuration(text: &str) -> Result<Option<(Hook, u64)>> {
    let config: Configuration = toml::from_str(text).context("invalid .arc/worktrees.toml")?;
    let Some(prepare) = config.prepare else {
        return Ok(None);
    };
    if prepare
        .command
        .as_ref()
        .is_some_and(|command| command.trim().is_empty())
    {
        bail!("worktree preparation command is empty");
    }
    if prepare.mode == Mode::SourceBound {
        if prepare.command.is_none() {
            bail!("source-bound worktree preparation requires a command");
        }
        if prepare.directory.is_some() {
            bail!("worktree preparation directory requires path-stable mode");
        }
    }
    if prepare
        .directory
        .as_ref()
        .is_some_and(|directory| directory.as_os_str().is_empty())
    {
        bail!("worktree preparation directory is empty");
    }
    let timeout = prepare
        .timeout
        .as_deref()
        .map(crate::commands::parse_duration)
        .transpose()?
        .unwrap_or(30);
    if timeout == 0 {
        bail!("worktree preparation timeout must be positive");
    }
    Ok(Some((prepare, timeout)))
}

fn cold(reason: &str) {
    eprintln!("warning: worktree preparation unavailable; continuing cold: {reason}");
}

/// Prepare and configure a gate while retaining its build-directory lease.
pub fn prepare_command(command: &mut Command, path: &Path) -> Result<CommandLease> {
    configure(command, path, Use::Gate)
}

/// Configure a probe without running preparation, retaining its storage lease.
pub fn configure_command(command: &mut Command, path: &Path) -> Result<CommandLease> {
    configure(command, path, Use::Probe)
}

/// What a configured command does with prepared storage.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    /// Prepare first, then use the outputs.
    Gate,
    /// Use outputs only when preparation completed.
    Probe,
    /// Discard outputs, whatever their state.
    Invalidate,
}

fn configure(command: &mut Command, path: &Path, usage: Use) -> Result<CommandLease> {
    let prepare_first = usage == Use::Gate;
    let text = match fs::read_to_string(path.join(CONFIG)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CommandLease::default())
        }
        Err(error) => return Err(error).context("cannot read .arc/worktrees.toml"),
    };
    let (hook, timeout) = match configuration(&text) {
        Ok(Some(configuration)) => configuration,
        Ok(None) => return Ok(CommandLease::default()),
        Err(error) => {
            if explicit_mode(&text)? {
                return Err(error);
            }
            cold(&format!("{error:#}"));
            return Ok(CommandLease::default());
        }
    };
    let path = fs::canonicalize(path)?;
    if hook.mode == Mode::PathStable {
        return stable::configure(command, &path, &(hook, timeout), usage);
    }
    if prepare_first {
        prepare(&path)?;
    }
    let key = cache_key(&hook, timeout);
    let source = Source::observe(&path)?;
    let cache = gitio::common_dir(&path)?
        .join("arc/cache/worktrees")
        .join(&key);
    let admin = gitio::worktree_admin_dir(&path)?;
    let source_path = source_path_key(&path);
    let mut build = admin
        .join("arc-warm-build")
        .join(&key)
        .join(&source_path)
        .join(&source.tree);
    let identity = preparation_identity(&source);
    if !already_prepared(&build.join("prepared"), &identity).unwrap_or(false) {
        build = admin
            .join("arc-cold-build")
            .join(invocation_key())
            .join(&key)
            .join(source_path)
            .join(&source.tree);
    }
    command.storage(CommandStorage {
        build: build.clone(),
        cache: cache.clone(),
        owned_build: true,
    });
    command
        .envs(environment(
            &path,
            &source.revision,
            &source.tree,
            &cache,
            &build,
        ))
        .env_remove("CARGO_TARGET_DIR")
        .env("GIT_OPTIONAL_LOCKS", "0");
    Ok(CommandLease::default())
}

fn cache_key(hook: &Hook, timeout: u64) -> String {
    hex::encode(Sha256::digest(
        format!("{}\n{timeout}", hook.command.as_deref().unwrap_or_default()).as_bytes(),
    ))
}

fn source_path_key(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    hex::encode(Sha256::digest(path.as_os_str().as_bytes()))
}

fn preparation_identity(source: &Source) -> String {
    format!("arc-worktree-prepared/1\n{}\n", source.fingerprint())
}

fn invocation_key() -> &'static str {
    static KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    KEY.get_or_init(|| ulid::Ulid::new().to_string())
}

fn cold_storage(path: &Path, source: &Source, (hook, timeout): &(Hook, u64)) -> Result<PathBuf> {
    let admin = gitio::worktree_admin_dir(path)?;
    let root = private_directory(&admin, "arc-cold-build")?;
    let invocation = private_directory(&root, invocation_key())?;
    let config = private_directory(&invocation, &cache_key(hook, *timeout))?;
    let source_path = private_directory(&config, &source_path_key(path))?;
    private_directory(&source_path, &source.tree)
}

fn environment(
    path: &Path,
    revision: &str,
    tree: &str,
    cache: &Path,
    build: &Path,
) -> Vec<(&'static str, OsString)> {
    vec![
        ("ARC_WORKTREE_PATH", path.as_os_str().to_owned()),
        ("ARC_WORKTREE_REVISION", revision.into()),
        ("ARC_WORKTREE_TREE", tree.into()),
        ("ARC_WORKTREE_CACHE", cache.as_os_str().to_owned()),
        ("ARC_WORKTREE_BUILD", build.as_os_str().to_owned()),
    ]
}

fn private_directory(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    match fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| format!("cannot create {}", path.display()))
        }
    }
    if !fs::symlink_metadata(&path)?.file_type().is_dir() {
        bail!(
            "preparation directory {} is not a real directory",
            path.display()
        );
    }
    Ok(path)
}

fn owned_cache(common: &Path, key: &str) -> Result<PathBuf> {
    let arc = private_directory(common, "arc")?;
    let cache = private_directory(&arc, "cache")?;
    let root = private_directory(&cache, "worktrees")?;
    let cache = private_directory(&root, key)?;
    let marker = format!("arc-worktree-cache/1\n{key}\n");
    owned_marker(&cache.join("owner"), &marker)?;
    Ok(cache)
}

fn owned_marker(path: &Path, marker: &str) -> Result<()> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(marker.as_bytes())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(path)?.file_type().is_file()
                || fs::read_to_string(path)? != marker
            {
                bail!(
                    "preparation cache ownership does not match at {}",
                    path.display()
                );
            }
        }
        Err(error) => return Err(error).context("cannot mark preparation cache ownership"),
    }
    Ok(())
}

fn already_prepared(path: &Path, identity: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let text = fs::read_to_string(path)?;
            if !text.starts_with("arc-worktree-prepared/1\n") {
                bail!(
                    "preparation receipt ownership does not match at {}",
                    path.display()
                );
            }
            Ok(text == identity)
        }
        Ok(_) => bail!(
            "preparation receipt {} is not a regular file",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("cannot read preparation receipt"),
    }
}

fn record_prepared(build: &Path, identity: &str) -> Result<()> {
    use std::io::Write;

    let temporary = build.join(format!("prepared-{}", ulid::Ulid::new()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        file.write_all(identity.as_bytes())?;
        fs::rename(&temporary, build.join("prepared"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn lease(cache: &Path) -> Result<File> {
    acquire_lease(&cache.join("lease"), "preparation cache")
}

fn acquire_lease(path: &Path, kind: &str) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        bail!("{kind} lease is not a regular file");
    }
    // SAFETY: the descriptor belongs to `file`, which holds the lease until
    // preparation has finished and all owned children have been collected.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            bail!("{kind} lease is busy");
        }
        return Err(error).with_context(|| format!("cannot lease {kind}"));
    }
    gitio::forget_answers();
    Ok(file)
}

fn run(path: &Path, source: &Source, (hook, timeout): &(Hook, u64)) -> Result<()> {
    let key = cache_key(hook, *timeout);
    let common = gitio::common_dir(path)?;
    let cache = owned_cache(&common, &key)?;
    let lease = CommandLease {
        files: vec![lease(&cache)?],
        build_lease: None,
        storage: None,
    };
    let admin = gitio::worktree_admin_dir(path)?;
    let build_root = private_directory(&admin, "arc-warm-build")?;
    let builds = private_directory(&build_root, &key)?;
    let sources = private_directory(&builds, &source_path_key(path))?;
    let build = private_directory(&sources, &source.tree)?;
    owned_marker(
        &build.join("source"),
        &format!(
            "arc-worktree-build/1\n{}\n{}\n{key}\n",
            path.display(),
            source.tree
        ),
    )?;
    let mut lease = lease;
    lease.storage = Some(CommandStorage {
        build: build.clone(),
        cache: cache.clone(),
        owned_build: true,
    });
    let identity = preparation_identity(source);
    if already_prepared(&build.join("prepared"), &identity)? {
        return Ok(());
    }
    run_hook(
        path,
        source,
        hook.command
            .as_deref()
            .context("preparation command unavailable")?,
        *timeout,
        &cache,
        &build,
        &lease,
    )?;
    record_prepared(&build, &identity)?;
    Ok(())
}

fn run_hook(
    path: &Path,
    source: &Source,
    hook_command: &str,
    timeout: u64,
    cache: &Path,
    build: &Path,
    lease: &CommandLease,
) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(timeout))
        .context("worktree preparation timeout is too large")?;
    let (mut shell, _) = crate::slots::measured_shell(path)?;
    let command = &mut shell.command;
    command
        .args(["-c", hook_command])
        .current_dir(build)
        .envs(environment(
            path,
            &source.revision,
            &source.tree,
            cache,
            build,
        ))
        .env_remove("CARGO_TARGET_DIR")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    lease.inherit(command);
    let mut child = shell.spawn().context("cannot start worktree preparation")?;
    let stdout = child
        .child()
        .stdout
        .take()
        .context("preparation stdout unavailable")?;
    let stderr = child
        .child()
        .stderr
        .take()
        .context("preparation stderr unavailable")?;
    let out = std::thread::spawn(move || {
        process_group::read_tail(stdout, process_group::STDERR_TAIL_BYTES)
    });
    let err = std::thread::spawn(move || {
        process_group::read_tail(stderr, process_group::STDERR_TAIL_BYTES)
    });
    let mut exited = false;
    let mut timed_out = false;
    loop {
        if !exited {
            exited = child
                .leader_exited()
                .context("cannot observe worktree preparation")?;
        }
        if exited && out.is_finished() && err.is_finished() {
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child
        .stop()
        .context("cannot stop worktree preparation's process group")?;
    crate::gate_work::record_tree_cpu(&child.observation.timing);
    let out =
        process_group::join_before_deadline(out, Some(deadline), "preparation stdout reader")?
            .transpose()?
            .unwrap_or_default();
    let err =
        process_group::join_before_deadline(err, Some(deadline), "preparation stderr reader")?
            .transpose()?
            .unwrap_or_default();
    if timed_out {
        bail!("preparation exceeded {timeout}s; its process group was stopped");
    }
    if !(exited && status.success()) {
        bail!("preparation failed: {}{}", out.trim(), err.trim());
    }
    let after = Source::observe(path)?;
    if after != *source {
        bail!(
            "worktree preparation changed source, index, HEAD or untracked source paths: {}",
            source.changed_fields(&after)
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "Lease witnesses use inert operator programs under fixture ownership."
)]
mod tests {
    use super::*;

    /// A lease belongs to the owned child only. Clearing CLOEXEC in the
    /// parent lets a concurrent unrelated exec keep mutable storage busy.
    #[test]
    fn lease_inheritance_is_scoped_to_the_owned_command() {
        let directory = tempfile::tempdir().unwrap();
        let guard = CommandLease {
            files: vec![lease(directory.path()).unwrap()],
            build_lease: None,
            storage: None,
        };
        let descriptor = guard.files[0].as_raw_fd();
        let mut owned = crate::project_command::operator("sh");
        owned
            .args(["-c", "test -e \"/proc/self/fd/$LEASE_FD\""])
            .env("LEASE_FD", descriptor.to_string());
        guard.inherit(&mut owned);
        let mut unrelated = crate::project_command::operator("sh");
        unrelated
            .args(["-c", "test ! -e \"/proc/self/fd/$LEASE_FD\""])
            .env("LEASE_FD", descriptor.to_string());
        std::thread::scope(|scope| {
            let owned = scope.spawn(move || owned.status().unwrap());
            let unrelated = scope.spawn(move || unrelated.status().unwrap());
            assert!(
                owned.join().unwrap().success(),
                "owned child lost its lease descriptor"
            );
            assert!(
                unrelated.join().unwrap().success(),
                "unrelated child inherited a foreign lease"
            );
        });
        // SAFETY: guard retains this regular file descriptor.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "parent descriptor became inheritable"
        );
    }
}
