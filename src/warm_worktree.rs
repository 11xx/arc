//! Project-owned build preparation with source-bound output directories.

use crate::{gitio, process_group};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CONFIG: &str = ".arc/worktrees.toml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    prepare: Hook,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hook {
    command: String,
    timeout: Option<String>,
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
    fn observe(path: &Path) -> Result<Self> {
        Ok(Self {
            revision: gitio::head(path)?,
            tree: gitio::git(path, &["rev-parse", "HEAD^{tree}"])?,
            tracked: source_bytes(path, &["diff", "--binary", "HEAD", "--"])?,
            staged: source_bytes(path, &["diff", "--cached", "--binary", "HEAD", "--"])?,
            untracked: source_bytes(path, &["ls-files", "--others", "--exclude-standard", "-z"])?,
            index: fs::read(gitio::worktree_admin_dir(path)?.join("index"))?,
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

fn source_bytes(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = gitio::git_command()
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(path)
        .output()?;
    if !output.status.success() {
        bail!(
            "cannot observe worktree preparation source: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}

/// Prepare an existing checkout. Cache failures leave it cold; a hook that
/// changes its source, index or HEAD makes the checkout unsafe to continue.
pub fn prepare(path: &Path) -> Result<()> {
    let text = match fs::read_to_string(path.join(CONFIG)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            cold(&error.to_string());
            return Ok(());
        }
    };
    let hook = match configuration(&text) {
        Ok(hook) => hook,
        Err(error) => {
            cold(&format!("{error:#}"));
            return Ok(());
        }
    };
    let path = fs::canonicalize(path).context("cannot resolve preparation checkout")?;
    let before = Source::observe(&path)?;
    let outcome = run(&path, &before, &hook);
    if Source::observe(&path)? != before {
        bail!("worktree preparation changed source, index, HEAD or untracked source paths; keeping checkout for inspection");
    }
    if let Err(error) = outcome {
        cold_storage(&path, &before, &hook)?;
        cold(&format!("{error:#}"));
    } else {
        eprintln!("worktree preparation: prepared source tree {}", before.tree);
    }
    Ok(())
}

fn configuration(text: &str) -> Result<(Hook, u64)> {
    let config: Configuration = toml::from_str(text).context("invalid .arc/worktrees.toml")?;
    if config.prepare.command.trim().is_empty() {
        bail!("worktree preparation command is empty");
    }
    let timeout = config
        .prepare
        .timeout
        .as_deref()
        .map(crate::commands::parse_duration)
        .transpose()?
        .unwrap_or(30);
    if timeout == 0 {
        bail!("worktree preparation timeout must be positive");
    }
    Ok((config.prepare, timeout))
}

fn cold(reason: &str) {
    eprintln!("warning: worktree preparation unavailable; continuing cold: {reason}");
}

/// Give gates and their environment probes the same source-bound storage
/// paths as preparation. Computing these paths writes nothing.
pub fn configure_command(command: &mut Command, path: &Path) {
    let context = (|| -> Result<Vec<(&'static str, OsString)>> {
        let text = fs::read_to_string(path.join(CONFIG))?;
        let (hook, timeout) = configuration(&text)?;
        let path = fs::canonicalize(path)?;
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
        Ok(environment(
            &path,
            &source.revision,
            &source.tree,
            &cache,
            &build,
        ))
    })();
    if let Ok(context) = context {
        command
            .envs(context)
            .env_remove("CARGO_TARGET_DIR")
            .env("GIT_OPTIONAL_LOCKS", "0");
    }
}

fn cache_key(hook: &Hook, timeout: u64) -> String {
    hex::encode(Sha256::digest(
        format!("{}\n{timeout}", hook.command).as_bytes(),
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
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(cache.join("lease"))?;
    if !file.metadata()?.file_type().is_file() {
        bail!("preparation cache lease is not a regular file");
    }
    // SAFETY: the descriptor belongs to `file`, which holds the lease until
    // preparation has finished and all owned children have been collected.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            bail!("preparation cache lease is busy");
        }
        return Err(error).context("cannot lease preparation cache");
    }
    Ok(file)
}

struct PreparationChild(Child);

impl Drop for PreparationChild {
    fn drop(&mut self) {
        let _ = process_group::kill_process_group(self.0.id());
        let _ = self.0.wait();
    }
}

fn exit_without_reaping(child: &Child) -> Result<Option<bool>> {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: waitid initializes `info`. WNOWAIT keeps the leader's PID
    // reserved until the process group is stopped and the leader is reaped.
    if unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("cannot observe worktree preparation");
    }
    // SAFETY: waitid returned success and initialized the zeroed structure.
    let info = unsafe { info.assume_init() };
    // SAFETY: waitid's successful WEXITED result supplies these child fields.
    Ok(unsafe {
        (info.si_pid() != 0).then(|| info.si_code == libc::CLD_EXITED && info.si_status() == 0)
    })
}

fn run(path: &Path, source: &Source, (hook, timeout): &(Hook, u64)) -> Result<()> {
    let key = cache_key(hook, *timeout);
    let common = gitio::common_dir(path)?;
    let cache = owned_cache(&common, &key)?;
    let _lease = lease(&cache)?;
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
    let identity = preparation_identity(source);
    if already_prepared(&build.join("prepared"), &identity)? {
        return Ok(());
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(*timeout))
        .context("worktree preparation timeout is too large")?;
    let child = Command::new("sh")
        .args(["-c", &hook.command])
        .current_dir(&build)
        .envs(environment(
            path,
            &source.revision,
            &source.tree,
            &cache,
            &build,
        ))
        .env_remove("CARGO_TARGET_DIR")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("cannot start worktree preparation")?;
    let mut child = PreparationChild(child);
    let stdout = child
        .0
        .stdout
        .take()
        .context("preparation stdout unavailable")?;
    let stderr = child
        .0
        .stderr
        .take()
        .context("preparation stderr unavailable")?;
    let out = std::thread::spawn(move || {
        process_group::read_tail(stdout, process_group::STDERR_TAIL_BYTES)
    });
    let err = std::thread::spawn(move || {
        process_group::read_tail(stderr, process_group::STDERR_TAIL_BYTES)
    });
    let mut status = None;
    let mut timed_out = false;
    loop {
        if status.is_none() {
            status = exit_without_reaping(&child.0)?;
        }
        if status.is_some() && out.is_finished() && err.is_finished() {
            break;
        }
        if Instant::now() >= deadline {
            process_group::kill_process_group(child.0.id())
                .context("cannot stop overrunning worktree preparation")?;
            timed_out = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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
    if status != Some(true) {
        bail!("preparation failed: {}{}", out.trim(), err.trim());
    }
    drop(child);
    if Source::observe(path)? != *source {
        bail!("worktree preparation changed source, index, HEAD or untracked source paths");
    }
    record_prepared(&build, &identity)?;
    Ok(())
}
