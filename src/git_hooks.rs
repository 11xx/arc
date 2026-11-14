//! Forward Git-fired hooks through the same destination authority as project programs.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Mutex, OnceLock};

pub const ENTRY: &str = "__project-hook";
const AUTHORITY: &str = "ARC_PROJECT_HOOK_AUTHORITY";
const NAMES: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "post-index-change",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
];

#[derive(Clone)]
struct Inventory {
    common: PathBuf,
    directory: PathBuf,
    names: Vec<&'static str>,
    configured_hooks: Vec<String>,
    watched: Vec<Stamp>,
}

#[derive(Serialize, Deserialize)]
struct Authority {
    scope: Option<crate::confinement::Authority>,
}

pub(crate) struct Hooks {
    directory: PathBuf,
}

fn forwarder_executable() -> Result<PathBuf> {
    #[cfg(test)]
    {
        if let Some(binary) = std::env::var_os("ARC_TEST_BIN") {
            return Ok(binary.into());
        }
        return Ok(std::env::current_exe()?
            .parent()
            .and_then(Path::parent)
            .context("test executable has no Cargo profile directory")?
            .join("arc"));
    }
    #[cfg(not(test))]
    std::env::current_exe().map_err(Into::into)
}
impl Drop for Hooks {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

impl Hooks {
    pub(crate) fn hold_for(self, child: &mut Child) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: the unreaped child retains its PID until this descriptor is opened.
            let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0) };
            if descriptor < 0 {
                let error = std::io::Error::last_os_error();
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            // SAFETY: pidfd_open returned an owned descriptor.
            let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor as i32) };
            std::thread::spawn(move || {
                let _hooks = self;
                let mut event = libc::pollfd {
                    fd: descriptor.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: poll uses one initialized descriptor and wakes when the child exits.
                while unsafe { libc::poll(&mut event, 1, -1) } < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                }
            });
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = child.kill();
            let _ = child.wait();
            Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
        }
    }
}

fn executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Global options end before the subcommand; their values are never verbs.
fn subcommand_index(arguments: &[OsString]) -> Result<usize> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        let value = argument.to_string_lossy();
        if !value.starts_with('-') {
            return Ok(index);
        }
        if matches!(
            value.as_ref(),
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
        ) {
            if arguments.get(index + 1).is_none() {
                bail!("Git global option {value} requires a value");
            }
            index += 2;
        } else if matches!(
            value.as_ref(),
            "--bare"
                | "--no-replace-objects"
                | "--no-lazy-fetch"
                | "--no-optional-locks"
                | "--no-advice"
                | "-p"
                | "--paginate"
                | "-P"
                | "--no-pager"
                | "--literal-pathspecs"
                | "--glob-pathspecs"
                | "--noglob-pathspecs"
                | "--icase-pathspecs"
                | "--no-implicit-dot"
                | "--version"
                | "-v"
                | "--help"
                | "-h"
                | "--exec-path"
                | "--html-path"
                | "--man-path"
                | "--info-path"
        ) || [
            "-C",
            "-c",
            "--git-dir=",
            "--work-tree=",
            "--namespace=",
            "--config-env=",
            "--exec-path=",
            "--attr-source=",
            "--super-prefix=",
        ]
        .iter()
        .any(|prefix| value.starts_with(prefix))
        {
            index += 1;
        } else {
            bail!("cannot forward Git hooks after unsupported global option {value:?}; refusing Git invocation");
        }
    }
    Ok(index)
}

fn working_directory(cwd: &Path, global: &[OsString]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let mut cwd = cwd.to_path_buf();
    let mut index = 0;
    while let Some(argument) = global.get(index) {
        let value = argument.to_string_lossy();
        if value == "-C" {
            if let Some(path) = global.get(index + 1).filter(|path| !path.is_empty()) {
                cwd = cwd.join(path);
            }
            index += 2;
        } else if value.starts_with("-C") {
            cwd = cwd.join(OsStr::from_bytes(&argument.as_bytes()[2..]));
            index += 1;
        } else {
            index += if matches!(
                value.as_ref(),
                "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
            ) {
                2
            } else {
                1
            };
        }
    }
    cwd
}

/// Only object/ref queries and configuration reads bypass hook discovery.
fn hook_free(arguments: &[OsString], index: usize) -> bool {
    if index == arguments.len()
        && arguments.iter().any(|argument| {
            matches!(
                argument.to_str(),
                Some(
                    "--version"
                        | "-v"
                        | "--exec-path"
                        | "--html-path"
                        | "--man-path"
                        | "--info-path"
                )
            )
        })
    {
        return true;
    }
    match arguments.get(index).and_then(|value| value.to_str()) {
        // Object reads do not update the index, worktree or refs.
        Some("cat-file" | "diff-tree" | "ls-tree" | "merge-base" | "rev-list" | "log" | "show") => {
            true
        }
        // Tracked-file enumeration reads the index without changing it.
        Some("ls-files") => true,
        // Ref and revision queries do not change the repository.
        Some("check-ref-format" | "for-each-ref" | "rev-parse" | "show-ref") => true,
        // Object insertion does not update the index, worktree or refs.
        Some("hash-object") => true,
        // Version reporting cannot run repository hooks.
        Some("version") => true,
        // Listing checkouts does not create, populate or remove one.
        Some("worktree") => arguments
            .get(index + 1)
            .is_some_and(|argument| argument == "list"),
        // A single ref with only query flags cannot create, delete or retarget it.
        Some("symbolic-ref") => {
            let tail = &arguments[index + 1..];
            tail.iter()
                .filter(|argument| !argument.to_string_lossy().starts_with('-'))
                .count()
                == 1
                && tail.iter().all(|argument| {
                    !argument.to_string_lossy().starts_with('-')
                        || matches!(argument.to_str(), Some("-q" | "--quiet" | "--short"))
                })
        }
        // Configuration writes remain subject to discovery and cache invalidation.
        Some("config") => {
            arguments.len() == index + 2
                && !arguments[index + 1].to_string_lossy().starts_with('-')
                && !matches!(
                    arguments[index + 1].to_str(),
                    Some("set" | "unset" | "rename-section" | "remove-section" | "edit")
                )
                || arguments.get(index + 1).is_some_and(|argument| {
                    matches!(
                        argument.to_str(),
                        Some(
                            "--get"
                                | "--get-all"
                                | "--get-regexp"
                                | "--get-urlmatch"
                                | "--list"
                                | "-l"
                                | "get"
                                | "list"
                        )
                    )
                })
        }
        _ => false,
    }
}

type FileIdentity = (u64, u64, u64, i64, i64, i64, i64, u32);

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    path: PathBuf,
    value: Option<FileIdentity>,
}
impl Stamp {
    fn read(path: PathBuf) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let value = match fs::metadata(&path) {
            Ok(metadata) => Some((
                metadata.dev(),
                metadata.ino(),
                metadata.size(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec(),
                metadata.mode(),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("cannot inspect Git hook configuration {}", path.display())
                })
            }
        };
        Ok(Self { path, value })
    }
}

fn environment(command: &Command, key: &str) -> Option<OsString> {
    command
        .get_envs()
        .find(|(name, _)| *name == key)
        .map_or_else(
            || std::env::var_os(key),
            |(_, value)| value.map(OsStr::to_os_string),
        )
}

#[allow(
    clippy::disallowed_methods,
    reason = "Hook discovery runs only read-only Git path and configuration queries without recursive forwarding."
)]
fn query(
    command: &Command,
    cwd: &Path,
    global: &[OsString],
    arguments: &[&str],
) -> Result<Option<String>> {
    use crate::perf::Counted;
    let mut query = crate::child_command::command(command.get_program());
    query.args(global);
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            query.env(key, value);
        } else {
            query.env_remove(key);
        }
    }
    let output = query.current_dir(cwd).args(arguments).counted_output()?;
    if !output.status.success() {
        if arguments.contains(&"rev-parse") {
            return Ok(None);
        }
        bail!(
            "cannot read Git hook configuration; refusing Git invocation: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(Some(
        String::from_utf8(output.stdout)?
            .trim_end_matches('\n')
            .to_string(),
    ))
}

fn discover(command: &Command, cwd: &Path, global: &[OsString]) -> Result<Option<Inventory>> {
    let workdir = working_directory(cwd, global);
    let configuration = query(
        command,
        cwd,
        global,
        &["config", "--null", "--show-origin", "--list"],
    )?
    .unwrap_or_default();
    let records = configuration.split_terminator('\0').collect::<Vec<_>>();
    if records.len() % 2 != 0 {
        bail!("invalid Git hook configuration records; refusing Git invocation");
    }
    let mut configured = None;
    let mut configured_hooks = Vec::new();
    let pointer = workdir.join(".git");
    let mut paths = if pointer.is_file() {
        vec![pointer]
    } else {
        Vec::new()
    };
    let mut expanded = HashMap::new();
    for record in records.chunks_exact(2) {
        let (key, value) = record[1].split_once('\n').unwrap_or((record[1], ""));
        if (key == "core.hookspath"
            || key == "include.path"
            || (key.starts_with("includeif.") && key.ends_with(".path")))
            && value.starts_with('~')
            && !expanded.contains_key(key)
        {
            let values = query(
                command,
                cwd,
                global,
                &["config", "--null", "--path", "--get-all", key],
            )?
            .context("Git configuration path disappeared during discovery")?;
            expanded.insert(
                key,
                values
                    .split_terminator('\0')
                    .map(PathBuf::from)
                    .collect::<std::collections::VecDeque<_>>(),
            );
        }
    }
    for record in records.chunks_exact(2) {
        let origin = record[0]
            .strip_prefix("file:")
            .map(PathBuf::from)
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    workdir.join(path)
                }
            });
        let (key, value) = record[1].split_once('\n').unwrap_or((record[1], ""));
        let path_value = expanded
            .get_mut(key)
            .map(|values| {
                values
                    .pop_front()
                    .context("Git configuration paths changed during discovery")
            })
            .transpose()?
            .unwrap_or_else(|| PathBuf::from(value));
        if let Some(origin) = &origin {
            paths.push(origin.clone());
        }
        if key == "core.hookspath" {
            configured = Some(path_value.clone());
        }
        if key.starts_with("hook.") && key.ends_with(".command") {
            configured_hooks.push(key.to_string());
        }
        if key == "include.path" || (key.starts_with("includeif.") && key.ends_with(".path")) {
            let path = path_value;
            paths.push(if path.is_absolute() {
                path
            } else {
                origin
                    .as_deref()
                    .and_then(Path::parent)
                    .unwrap_or(&workdir)
                    .join(path)
            });
        }
    }
    let Some(locations) = query(
        command,
        cwd,
        global,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "hooks",
            "--git-common-dir",
            "--git-path",
            "config.worktree",
        ],
    )?
    else {
        if crate::confinement::declaration("Git hook discovery outside a repository", cwd)?
            .is_some()
        {
            bail!("cannot locate Git hook controls outside a repository; refusing confined Git invocation");
        }
        return Ok(None);
    };
    let locations = locations.split('\n').collect::<Vec<_>>();
    let [directory, common, worktree_config] = locations.as_slice() else {
        bail!("Git hook discovery returned invalid administration paths; refusing Git invocation");
    };
    let common = fs::canonicalize(common)?;
    let configured_path = configured.is_some();
    let directory = configured.unwrap_or_else(|| PathBuf::from(directory));
    let actual_directory = if directory.is_absolute() {
        directory.clone()
    } else {
        workdir.join(&directory)
    };
    paths.extend([
        actual_directory.clone(),
        common.join("config"),
        PathBuf::from(worktree_config),
    ]);
    if environment(command, "GIT_CONFIG_NOSYSTEM").as_deref() != Some(OsStr::new("1")) {
        paths.push(
            environment(command, "GIT_CONFIG_SYSTEM")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/etc/gitconfig".into()),
        );
    }
    let home = environment(command, "HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    paths.push(
        environment(command, "GIT_CONFIG_GLOBAL")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".gitconfig")),
    );
    paths.push(
        environment(command, "XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("git/config"),
    );
    paths.sort();
    paths.dedup();
    let watched = paths
        .into_iter()
        .map(Stamp::read)
        .collect::<Result<Vec<_>>>()?;
    let names = NAMES
        .iter()
        .copied()
        .filter(|name| configured_path || executable(&actual_directory.join(name)))
        .collect();
    Ok(Some(Inventory {
        common,
        directory,
        names,
        configured_hooks,
        watched,
    }))
}

fn refuses_configured_hook(entry: &str, cwd: &Path) -> Result<bool> {
    use std::cell::Cell;
    thread_local! { static RESOLVING: Cell<bool> = const { Cell::new(false) }; }
    if RESOLVING.with(|value| value.replace(true)) {
        bail!("configured Git hook {entry:?} requires recursive destination resolution; refusing Git invocation");
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            RESOLVING.with(|value| value.set(false));
        }
    }
    let _reset = Reset;
    Ok(crate::confinement::declaration("configuration-defined Git hook", cwd)?.is_some())
}

pub(crate) fn prepare(command: &mut Command, args: &[OsString]) -> Result<Option<Hooks>> {
    if crate::confinement::unconfined() {
        command.args(args);
        return Ok(None);
    }
    let index = subcommand_index(args)?;
    if hook_free(args, index) {
        command.args(args);
        return Ok(None);
    }
    let cwd = command
        .get_current_dir()
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_dir()?);
    static INVENTORIES: OnceLock<Mutex<HashMap<Vec<OsString>, Inventory>>> = OnceLock::new();
    let mut key = vec![
        command.get_program().to_os_string(),
        cwd.as_os_str().to_os_string(),
    ];
    key.extend(args[..index].iter().cloned());
    key.push(OsString::new());
    for name in [
        "HOME",
        "PATH",
        "XDG_CONFIG_HOME",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_NOSYSTEM",
    ] {
        key.push(name.into());
        key.push(environment(command, name).unwrap_or_default());
    }
    for (name, value) in command.get_envs() {
        key.push(name.to_os_string());
        key.push(value.unwrap_or_default().to_os_string());
    }
    let inventories = INVENTORIES.get_or_init(Default::default);
    let cached = inventories
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned();
    let inventory = match cached.filter(|inventory| {
        inventory
            .watched
            .iter()
            .all(|stamp| Stamp::read(stamp.path.clone()).is_ok_and(|current| current == *stamp))
    }) {
        Some(inventory) => Some(inventory),
        None => {
            let inventory = discover(command, &cwd, &args[..index])?;
            if let Some(inventory) = &inventory {
                inventories
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(key, inventory.clone());
            }
            inventory
        }
    };
    let Some(inventory) = inventory else {
        command.args(args);
        return Ok(None);
    };
    if let Some(entry) = inventory.configured_hooks.first() {
        if refuses_configured_hook(entry, &cwd)? {
            bail!("configured Git hook {entry:?} cannot be forwarded reliably; refusing Git invocation");
        }
        command.args(args);
        return Ok(None);
    }
    if inventory.names.is_empty() {
        command.args(args);
        return Ok(None);
    }
    let parent = inventory.common.join("arc/command-hooks");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&parent)?;
    if fs::symlink_metadata(&parent)?.file_type().is_symlink() {
        bail!("Git hook control directory is a symlink");
    }
    let directory = parent.join(crate::ids::new_identifier());
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let hooks = Hooks { directory };
    let executable = crate::context::shell_quote(&forwarder_executable()?.display().to_string());
    for name in inventory.names {
        let target = inventory.directory.join(name);
        let script = format!(
            "#!/bin/sh\nexec {executable} {ENTRY} {} \"$@\"\n",
            crate::context::shell_quote(&target.display().to_string())
        );
        let path = hooks.directory.join(name);
        fs::write(&path, script)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let authority = Authority {
        scope: crate::confinement::current(),
    };
    command.env(AUTHORITY, serde_json::to_string(&authority)?);
    // The last command-line value overrides both environment and earlier global options.
    command
        .args(&args[..index])
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.directory.display()))
        .args(&args[index..]);
    Ok(Some(hooks))
}

pub(crate) fn main() -> ! {
    let result = (|| -> Result<i32> {
        let mut arguments = std::env::args_os().skip(2);
        let hook = PathBuf::from(arguments.next().context("missing Git hook path")?);
        let cwd = std::env::current_dir()?;
        let hook = if hook.is_absolute() {
            hook
        } else {
            cwd.join(hook)
        };
        if !executable(&hook) {
            return Ok(0);
        }
        let authority: Authority = serde_json::from_str(
            &std::env::var(AUTHORITY).context("Git hook has no destination authority")?,
        )?;
        let scope = authority.scope.with_context(|| {
            format!(
                "project command at Git hook {} has no destination confinement scope",
                hook.display()
            )
        })?;
        let _scope = crate::confinement::Scope::captured(scope);
        let checkout = crate::gitio::invocation_directory(&cwd)?;
        let status = crate::project_command::new(&hook, &checkout, "Git-fired hook")?
            .args(arguments)
            .current_dir(&checkout)
            .env_remove(AUTHORITY)
            .status()?;
        Ok(status.code().unwrap_or(1))
    })();
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claim I12: unknown Git verbs forward hooks and global option values are
    /// not verbs. Rejects an allowlist of hook-capable words or argument scanning.
    #[test]
    fn forwarding_defaults_closed_and_parses_global_option_values() {
        for (args, expected, free) in [
            (vec!["--version"], 1, true),
            (vec!["restore", "--source", "rev-parse"], 0, false),
            (vec!["future-command", "cat-file"], 0, false),
            (
                vec!["-C", "restore", "-c", "alias.x=commit", "rev-parse", "HEAD"],
                4,
                true,
            ),
            (
                vec![
                    "--git-dir",
                    "restore",
                    "--work-tree=commit",
                    "future-command",
                ],
                3,
                false,
            ),
            (vec!["--no-pager", "-cfoo.bar=restore", "restore"], 2, false),
            (vec!["config", "set", "example.value", "get"], 0, false),
            (vec!["symbolic-ref", "HEAD", "refs/heads/other"], 0, false),
            (vec!["worktree", "add", "list"], 0, false),
        ] {
            let args = args.into_iter().map(OsString::from).collect::<Vec<_>>();
            let index = subcommand_index(&args).unwrap();
            assert_eq!(index, expected, "{args:?}");
            assert_eq!(hook_free(&args, index), free, "{args:?}");
        }
        assert!(subcommand_index(&["--future-global-option".into(), "rev-parse".into()]).is_err());
    }

    fn fixture() -> tempfile::TempDir {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/confinement-fixtures");
        fs::create_dir_all(&base).unwrap();
        let scratch = tempfile::tempdir_in(base).unwrap();
        git(scratch.path(), &["init", "--template=", "-q"]);
        fs::write(scratch.path().join("file"), "content").unwrap();
        git(scratch.path(), &["add", "file"]);
        git(
            scratch.path(),
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.org",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "fixture",
            ],
        );
        scratch
    }
    fn git(path: &Path, args: &[&str]) {
        let output = crate::child_command::command("git")
            .args(args)
            .current_dir(path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn supported() -> bool {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: the ABI query takes no pointers or descriptors.
            if unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, 0, 0, 1) } >= 3 {
                return true;
            }
        }
        eprintln!("SKIP Git hook confinement: Landlock ABI 3 unavailable");
        false
    }
    fn command(path: &Path, args: &[&str]) -> crate::project_command::Command {
        let mut command = crate::gitio::git_command();
        command
            .args(args)
            .current_dir(path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }

    /// Claim I12: command-line hooksPath cannot override forwarding on restore.
    /// Rejects redirecting through lower-precedence environment configuration.
    #[test]
    fn command_line_hook_paths_cannot_override_forwarding() {
        if !supported() {
            return;
        }
        let scratch = fixture();
        let root = scratch.path();
        let decoy = root.join("decoy");
        fs::write(&decoy, "retained").unwrap();
        fs::create_dir(root.join(".hooks")).unwrap();
        let hook = root.join(".hooks/post-checkout");
        fs::write(
            &hook,
            format!("#!/bin/sh\nprintf escaped > '{}'\n", decoy.display()),
        )
        .unwrap();
        fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
        let _scope = crate::confinement::Scope::new(&Some(crate::confinement::Declaration {
            writes: vec!["target".into()],
            required: true,
        }));
        let output = command(
            root,
            &[
                "-c",
                "core.hooksPath=.hooks",
                "restore",
                "--source",
                "HEAD",
                "--worktree",
                "--",
                ".",
            ],
        )
        .output()
        .unwrap();
        assert_eq!(
            fs::read_to_string(decoy).unwrap(),
            "retained",
            "command-line hook escaped"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("ran confined"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Claim I12: cached discovery notices a command added through an initially
    /// empty include after -C changes Git's working directory. Rejects reusing
    /// an empty inventory after configuration changes or watching the wrong file.
    #[test]
    fn cached_discovery_tracks_empty_configuration_includes() {
        if !supported() {
            return;
        }
        let scratch = fixture();
        let root = scratch.path();
        let include = root.join(".git/extra-config");
        fs::write(&include, "").unwrap();
        git(root, &["config", "include.path", "extra-config"]);
        let decoy = root.join("decoy");
        fs::write(&decoy, "retained").unwrap();
        let _scope = crate::confinement::Scope::new(&Some(crate::confinement::Declaration {
            writes: vec!["target".into()],
            required: true,
        }));
        assert!(command(
            root.parent().unwrap(),
            &["-C", root.to_str().unwrap(), "status", "--porcelain"]
        )
        .output()
        .unwrap()
        .status
        .success());
        git(
            root,
            &[
                "config",
                "--file",
                include.to_str().unwrap(),
                "hook.cached.command",
                &format!("printf escaped > '{}'", decoy.display()),
            ],
        );
        git(
            root,
            &[
                "config",
                "--file",
                include.to_str().unwrap(),
                "hook.cached.event",
                "post-checkout",
            ],
        );
        let result = command(
            root.parent().unwrap(),
            &[
                "-C",
                root.to_str().unwrap(),
                "restore",
                "--source",
                "HEAD",
                "--worktree",
                "--",
                ".",
            ],
        )
        .output();
        assert!(
            result.is_err(),
            "cached discovery admitted a configuration hook: {result:?}"
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("hook.cached.command"));
        assert_eq!(fs::read_to_string(decoy).unwrap(), "retained");
    }
}
