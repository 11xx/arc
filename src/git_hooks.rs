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
    names: Vec<String>,
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
                && arguments[index + 1].to_string_lossy().contains('.')
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

fn validate_hook_command(arguments: &[OsString], index: usize) -> Result<()> {
    if arguments
        .get(index)
        .is_none_or(|argument| argument != "hook")
    {
        return Ok(());
    }
    match arguments
        .get(index + 1)
        .and_then(|argument| argument.to_str())
    {
        Some("list") => return Ok(()),
        Some("run") => {}
        _ => bail!("cannot forward unsupported Git hook action; refusing Git invocation"),
    }
    let mut cursor = index + 2;
    while let Some(argument) = arguments.get(cursor) {
        let value = argument.to_string_lossy();
        if value == "--" {
            cursor += 1;
            break;
        }
        if matches!(
            value.as_ref(),
            "--ignore-missing"
                | "--no-ignore-missing"
                | "--allow-unknown-hook-name"
                | "--no-allow-unknown-hook-name"
                | "--no-to-stdin"
                | "--no-jobs"
        ) {
            cursor += 1;
        } else if matches!(value.as_ref(), "--to-stdin" | "--jobs" | "-j") {
            if arguments.get(cursor + 1).is_none() {
                bail!("Git hook option {value} requires a value");
            }
            cursor += 2;
        } else if value.starts_with("--to-stdin=")
            || value.starts_with("--jobs=")
            || (value.starts_with("-j") && value.len() > 2)
        {
            cursor += 1;
        } else if value.starts_with('-') {
            bail!("cannot forward unsupported Git hook option {value:?}; refusing Git invocation");
        } else {
            break;
        }
    }
    let name = arguments
        .get(cursor)
        .context("Git hook run requires one filename")?;
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.as_encoded_bytes().contains(&b'/')
        || name.as_encoded_bytes().contains(&0)
    {
        bail!("Git hook name {name:?} must be one filename; refusing Git invocation");
    }
    Ok(())
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
        .and_then(|(_, value)| value.map(OsStr::to_os_string))
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
    query.env_clear();
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

struct Configuration {
    hook_path: Option<PathBuf>,
    hooks: Vec<String>,
    origins: Vec<PathBuf>,
    includes: Vec<PathBuf>,
}

fn configuration(
    command: &Command,
    cwd: &Path,
    global: &[OsString],
    file: Option<&Path>,
) -> Result<Configuration> {
    let mut options = vec!["config"];
    if let Some(file) = file {
        options.extend([
            "--file",
            file.to_str()
                .context("Git configuration path is not UTF-8")?,
            "--no-includes",
        ]);
    }
    let mut arguments = options.clone();
    arguments.extend(["--null", "--show-origin", "--list"]);
    let text = query(command, cwd, global, &arguments)?.unwrap_or_default();
    let records = text.split_terminator('\0').collect::<Vec<_>>();
    if records.len() % 2 != 0 {
        bail!("invalid Git hook configuration records; refusing Git invocation");
    }
    let mut result = Configuration {
        hook_path: None,
        hooks: Vec::new(),
        origins: Vec::new(),
        includes: Vec::new(),
    };
    let workdir = working_directory(cwd, global);
    let mut expanded = HashMap::new();
    for record in records.chunks_exact(2) {
        let (key, value) = record[1].split_once('\n').unwrap_or((record[1], ""));
        if (key == "core.hookspath"
            || key == "include.path"
            || (key.starts_with("includeif.") && key.ends_with(".path")))
            && (value.starts_with('~') || value.starts_with("%("))
            && !expanded.contains_key(key)
        {
            let mut arguments = options.clone();
            arguments.extend(["--null", "--path", "--get-all", key]);
            let values = query(command, cwd, global, &arguments)?
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
        let path = expanded
            .get_mut(key)
            .map(|values| {
                values
                    .pop_front()
                    .context("Git configuration paths changed during discovery")
            })
            .transpose()?
            .unwrap_or_else(|| PathBuf::from(value));
        if let Some(origin) = &origin {
            result.origins.push(origin.clone());
        }
        if key == "core.hookspath" {
            result.hook_path = Some(path.clone());
        }
        if key.starts_with("hook.") && key.ends_with(".command") {
            result.hooks.push(key.to_string());
        }
        if key == "include.path" || (key.starts_with("includeif.") && key.ends_with(".path")) {
            result.includes.push(if path.is_absolute() {
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
    Ok(result)
}

fn discover(command: &Command, cwd: &Path, global: &[OsString]) -> Result<Option<Inventory>> {
    let workdir = working_directory(cwd, global);
    let active = configuration(command, cwd, global, None)?;
    let configured = active.hook_path;
    let mut configured_hooks = active.hooks;
    let mut paths = active.origins;
    let mut seen = paths
        .iter()
        .map(fs::canonicalize)
        .collect::<std::io::Result<std::collections::HashSet<_>>>()?;
    let mut pending = active.includes;
    while let Some(path) = pending.pop() {
        paths.push(path.clone());
        if Stamp::read(path.clone())?.value.is_none() {
            continue;
        }
        if !seen.insert(fs::canonicalize(&path)?) {
            continue;
        }
        let included = match configuration(command, cwd, global, Some(&path)) {
            Ok(included) => included,
            Err(error) => {
                if crate::confinement::declaration("included Git hook configuration", cwd)?
                    .is_some()
                {
                    return Err(error).with_context(|| format!("cannot inspect included Git hook configuration {}; refusing Git invocation", path.display()));
                }
                return Ok(None);
            }
        };
        configured_hooks.extend(included.hooks);
        paths.extend(included.origins);
        pending.extend(included.includes);
    }
    configured_hooks.sort();
    configured_hooks.dedup();
    let pointer = workdir.join(".git");
    if pointer.is_file() {
        paths.push(pointer);
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
            "--git-path",
            "HEAD",
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
    let [directory, common, worktree_config, head] = locations.as_slice() else {
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
        PathBuf::from(head),
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
    let mut paths = paths
        .into_iter()
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                workdir.join(path)
            }
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    let watched = paths
        .into_iter()
        .map(Stamp::read)
        .collect::<Result<Vec<_>>>()?;
    let mut names = if configured_path {
        NAMES
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    match fs::read_dir(&actual_directory) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if executable(&entry.path()) {
                    names.push(entry.file_name().into_string().map_err(|_| {
                        anyhow::anyhow!("Git hook filename is not UTF-8; refusing Git invocation")
                    })?);
                }
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) => {}
        Err(error) => {
            return Err(error).context("cannot inspect Git hook directory; refusing Git invocation")
        }
    }
    names.sort();
    names.dedup();
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

pub(crate) fn prepare(
    command: &mut Command,
    args: &[OsString],
    inherit_environment: bool,
) -> Result<Option<Hooks>> {
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
    let cwd = if cwd.is_absolute() {
        cwd
    } else {
        std::env::current_dir()?.join(cwd)
    };
    if let Err(error) = validate_hook_command(args, index) {
        if crate::confinement::declaration("native Git hook arguments", &cwd)?.is_none() {
            command.args(args);
            return Ok(None);
        }
        return Err(error);
    }
    let authority = Authority {
        scope: crate::confinement::current(),
    };
    command.env(AUTHORITY, serde_json::to_string(&authority)?);
    let mut effective = if inherit_environment {
        std::env::vars_os().collect::<std::collections::BTreeMap<_, _>>()
    } else {
        std::collections::BTreeMap::new()
    };
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            effective.insert(key.to_os_string(), value.to_os_string());
        } else {
            effective.remove(key);
        }
    }
    command.env_clear().envs(effective);
    static INVENTORIES: OnceLock<Mutex<HashMap<Vec<OsString>, Inventory>>> = OnceLock::new();
    let mut key = vec![
        command.get_program().to_os_string(),
        cwd.as_os_str().to_os_string(),
    ];
    key.extend(args[..index].iter().cloned());
    key.push(OsString::new());
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
            let inventory = match discover(command, &cwd, &args[..index]) {
                Ok(inventory) => inventory,
                Err(error) => {
                    if crate::confinement::declaration("Git hook discovery", &cwd)?.is_none() {
                        command.args(args);
                        return Ok(None);
                    }
                    return Err(error);
                }
            };
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
        let target = inventory.directory.join(&name);
        let script = format!(
            "#!/bin/sh\nexec {executable} {ENTRY} {} \"$@\"\n",
            crate::context::shell_quote(&target.display().to_string())
        );
        let path = hooks.directory.join(name);
        fs::write(&path, script)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
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

    /// Claim I12: unknown Git verbs and config actions forward hooks; global
    /// option values are not verbs. Rejects word scanning or invalid legacy keys.
    #[test]
    fn forwarding_defaults_closed_and_parses_global_option_values() {
        for (args, expected, free) in [
            (vec!["--version"], 1, true),
            (vec!["config", "future-action"], 0, false),
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

    /// Claim I12: changing HEAD invalidates branch-conditioned classic hook paths.
    /// Rejects caching a hook-free path while a later branch selects project hooks.
    #[test]
    fn cached_discovery_tracks_branch_conditioned_hook_configuration() {
        if !supported() {
            return;
        }
        let scratch = fixture();
        let root = scratch.path();
        let include = root.join(".git/branch-config");
        let decoy = root.join("decoy");
        fs::write(&decoy, "retained").unwrap();
        fs::create_dir(root.join(".branch-hooks")).unwrap();
        let hook = root.join(".branch-hooks/post-checkout");
        fs::write(
            &hook,
            format!("#!/bin/sh\nprintf escaped > '{}'\n", decoy.display()),
        )
        .unwrap();
        fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
        git(root, &["branch", "other"]);
        git(
            root,
            &["config", "includeIf.onbranch:other.path", "branch-config"],
        );
        git(
            root,
            &[
                "config",
                "--file",
                include.to_str().unwrap(),
                "core.hooksPath",
                ".branch-hooks",
            ],
        );
        let _scope = crate::confinement::Scope::new(&Some(crate::confinement::Declaration {
            writes: vec!["target".into()],
            required: true,
        }));
        assert!(command(root, &["status", "--porcelain"])
            .output()
            .unwrap()
            .status
            .success());
        git(root, &["symbolic-ref", "HEAD", "refs/heads/other"]);
        let output = command(
            root,
            &["restore", "--source", "HEAD", "--worktree", "--", "."],
        )
        .output()
        .unwrap();
        assert_eq!(
            fs::read_to_string(decoy).unwrap(),
            "retained",
            "cached branch hook escaped"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("ran confined"));
    }

    /// Claim I12: inactive conditional includes cannot hide configured hook
    /// commands until a mutation activates them. Rejects inspecting only active config.
    #[test]
    fn conditional_hook_entries_are_refused_before_activation() {
        if !supported() {
            return;
        }
        let scratch = fixture();
        let root = scratch.path();
        let include = root.join(".git/inactive-config");
        let decoy = root.join("decoy");
        fs::write(&decoy, "retained").unwrap();
        git(
            root,
            &["config", "includeIf.onbranch:other.path", "inactive-config"],
        );
        git(
            root,
            &[
                "config",
                "--file",
                include.to_str().unwrap(),
                "hook.inactive.command",
                &format!("printf escaped > '{}'", decoy.display()),
            ],
        );
        git(
            root,
            &[
                "config",
                "--file",
                include.to_str().unwrap(),
                "hook.inactive.event",
                "post-checkout",
            ],
        );
        let _scope = crate::confinement::Scope::new(&Some(crate::confinement::Declaration {
            writes: vec!["target".into()],
            required: true,
        }));
        let result = command(root, &["status", "--porcelain"]).output();
        assert!(
            result.is_err(),
            "inactive configuration hook was not refused: {result:?}"
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("hook.inactive.command"));
        assert_eq!(fs::read_to_string(decoy).unwrap(), "retained");
    }

    /// Claim I12: a missing relative global configuration is watched at Git's
    /// effective directory. Rejects a cached empty inventory after its creation.
    #[test]
    fn cached_discovery_tracks_relative_global_configuration() {
        if !supported() {
            return;
        }
        let scratch = fixture();
        let root = scratch.path();
        let configuration = root.join("global-config");
        let decoy = root.join("decoy");
        fs::write(&decoy, "retained").unwrap();
        let _scope = crate::confinement::Scope::new(&Some(crate::confinement::Declaration {
            writes: vec!["target".into()],
            required: true,
        }));
        let make = |args: &[&str]| {
            let mut arguments = vec!["-C", root.to_str().unwrap()];
            arguments.extend_from_slice(args);
            let mut command = command(root.parent().unwrap(), &arguments);
            command.env("GIT_CONFIG_GLOBAL", "global-config");
            command
        };
        assert!(make(&["status", "--porcelain"])
            .output()
            .unwrap()
            .status
            .success());
        git(
            root,
            &[
                "config",
                "--file",
                configuration.to_str().unwrap(),
                "hook.global.command",
                &format!("printf escaped > '{}'", decoy.display()),
            ],
        );
        git(
            root,
            &[
                "config",
                "--file",
                configuration.to_str().unwrap(),
                "hook.global.event",
                "post-checkout",
            ],
        );
        let result = make(&["restore", "--source", "HEAD", "--worktree", "--", "."]).output();
        assert!(
            result.is_err(),
            "cached global discovery admitted a configuration hook: {result:?}"
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("hook.global.command"));
        assert_eq!(fs::read_to_string(decoy).unwrap(), "retained");
    }

    /// Claim I12: discovery uses Git's actual inherited or cleared environment.
    /// Rejects stale --config-env values and inheriting variables after env_clear.
    #[test]
    fn discovery_matches_inherited_and_cleared_environments() {
        const MODE: &str = "ARC_GIT_ENVIRONMENT_CASE";
        let Some(mode) = std::env::var_os(MODE) else {
            let mut failures = Vec::new();
            for mode in ["inherit", "clear"] {
                let output = crate::child_command::command(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "git_hooks::tests::discovery_matches_inherited_and_cleared_environments",
                        "--nocapture",
                    ])
                    .env(MODE, mode)
                    .output()
                    .unwrap();
                if !output.status.success() {
                    failures.push(format!(
                        "{mode}: {} {}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
            }
            assert!(failures.is_empty(), "{}", failures.join("\n"));
            return;
        };
        if !supported() {
            return;
        }
        let scratch = fixture();
        let root = scratch.path();
        let configured = root.join("environment-config");
        let decoy = root.join("decoy");
        fs::write(&decoy, "retained").unwrap();
        git(
            root,
            &[
                "config",
                "--file",
                configured.to_str().unwrap(),
                "hook.environment.command",
                &format!("printf escaped > '{}'", decoy.display()),
            ],
        );
        git(
            root,
            &[
                "config",
                "--file",
                configured.to_str().unwrap(),
                "hook.environment.event",
                "post-checkout",
            ],
        );
        let _scope = crate::confinement::Scope::new(&Some(crate::confinement::Declaration {
            writes: vec!["target".into()],
            required: true,
        }));
        if mode == "inherit" {
            let empty = root.join("empty-config");
            fs::write(&empty, "").unwrap();
            std::env::set_var("ARC_GIT_SELECTED_INCLUDE", &empty);
            let make = |args: &[&str]| {
                let mut arguments = vec!["--config-env=include.path=ARC_GIT_SELECTED_INCLUDE"];
                arguments.extend_from_slice(args);
                command(root, &arguments)
            };
            assert!(make(&["status", "--porcelain"])
                .output()
                .unwrap()
                .status
                .success());
            std::env::set_var("ARC_GIT_SELECTED_INCLUDE", &configured);
            let result = make(&["restore", "--source", "HEAD", "--worktree", "--", "."]).output();
            assert!(
                result.is_err(),
                "changed inherited configuration was admitted: {result:?}"
            );
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("hook.environment.command"));
        } else {
            std::env::set_var("GIT_CONFIG_GLOBAL", &configured);
            let home = root.join("empty-home");
            fs::create_dir(&home).unwrap();
            let result = command(root, &["status", "--porcelain"])
                .env_clear()
                .env("HOME", home)
                .env("PATH", "/usr/bin:/bin")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output();
            assert!(
                result.as_ref().is_ok_and(|output| output.status.success()),
                "cleared environment inherited configuration: {result:?}"
            );
        }
        assert_eq!(fs::read_to_string(decoy).unwrap(), "retained");
    }

    /// Claim I12: executable hook files outside the standard event inventory
    /// still pass through confinement. Rejects a second allowlist of hook names.
    #[test]
    fn custom_hook_files_are_forwarded() {
        if !supported() {
            return;
        }
        let scratch = fixture();
        let root = scratch.path();
        let decoy = root.join("decoy");
        fs::write(&decoy, "retained").unwrap();
        fs::create_dir_all(root.join(".git/hooks")).unwrap();
        let hook = root.join(".git/hooks/custom-command");
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
        let output = custom_hook_command(root, "custom-command")
            .output()
            .unwrap();
        assert_eq!(
            fs::read_to_string(decoy).unwrap(),
            "retained",
            "custom hook escaped"
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("ran confined"));
    }

    /// Claim I12: a native hook name cannot traverse out of forwarding controls.
    /// Rejects treating an arbitrary relative path as a hook's filename.
    #[test]
    fn native_hook_names_cannot_traverse_directories() {
        if !supported() {
            return;
        }
        let scratch = fixture();
        let root = scratch.path();
        let decoy = root.join("decoy");
        fs::write(&decoy, "retained").unwrap();
        fs::create_dir_all(root.join(".git/hooks")).unwrap();
        let hook = root.join(".git/escaped-command");
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
        let result = custom_hook_command(root, "../escaped-command").output();
        assert_eq!(
            fs::read_to_string(decoy).unwrap(),
            "retained",
            "traversing hook escaped"
        );
        assert!(
            result.is_err(),
            "traversing hook name was admitted: {result:?}"
        );
        assert!(result.unwrap_err().to_string().contains("one filename"));
    }

    /// Claim: a destination without confinement keeps native hook authority
    /// when the hook directory is executable but not readable. Rejects imposing
    /// discovery requirements on a destination that grants ordinary authority.
    #[test]
    fn unconfined_destinations_keep_unreadable_hook_directories() {
        // SAFETY: geteuid takes no arguments and has no side effects.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("SKIP hook-directory permissions: root bypasses read permissions");
            return;
        }
        let scratch = fixture();
        let root = scratch.path();
        let decoy = root.join("decoy");
        fs::write(&decoy, "retained").unwrap();
        let directory = root.join(".git/hooks");
        fs::create_dir_all(&directory).unwrap();
        let hook = directory.join("post-checkout");
        fs::write(
            &hook,
            format!("#!/bin/sh\nprintf ordinary > '{}'\n", decoy.display()),
        )
        .unwrap();
        fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
        let permissions = fs::metadata(&directory).unwrap().permissions();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o111)).unwrap();
        let _scope = crate::confinement::Scope::at(root, "HEAD");
        let result = command(
            root,
            &["restore", "--source", "HEAD", "--worktree", "--", "."],
        )
        .output();
        fs::set_permissions(directory, permissions).unwrap();
        assert!(
            result.as_ref().is_ok_and(|output| output.status.success()),
            "ordinary hook was refused: {result:?}"
        );
        assert_eq!(fs::read_to_string(decoy).unwrap(), "ordinary");
    }

    fn custom_hook_command(root: &Path, name: &str) -> crate::project_command::Command {
        let help = crate::child_command::command("git")
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["hook", "run", "-h"])
            .output()
            .unwrap();
        let mut arguments = vec!["hook", "run"];
        if String::from_utf8_lossy(&help.stdout).contains("allow-unknown-hook-name")
            || String::from_utf8_lossy(&help.stderr).contains("allow-unknown-hook-name")
        {
            arguments.push("--allow-unknown-hook-name");
        }
        arguments.push(name);
        command(root, &arguments)
    }
}
