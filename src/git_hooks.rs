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
}

#[derive(Serialize, Deserialize)]
struct Authority {
    scope: Option<crate::confinement::Authority>,
}

pub(crate) struct Hooks {
    directory: PathBuf,
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

fn hook_capable(arguments: &[OsString]) -> bool {
    arguments.iter().any(|argument| {
        matches!(
            argument.to_str(),
            Some(
                "switch"
                    | "checkout"
                    | "worktree"
                    | "commit"
                    | "rebase"
                    | "merge"
                    | "cherry-pick"
                    | "revert"
                    | "update-ref"
                    | "fetch"
                    | "pull"
                    | "push"
                    | "receive-pack"
                    | "read-tree"
                    | "reset"
                    | "replay"
                    | "am"
                    | "apply"
                    | "gc"
                    | "maintenance"
                    | "hook"
                    | "status"
                    | "diff"
                    | "add"
                    | "update-index"
                    | "rm"
                    | "mv"
                    | "stash"
                    | "branch"
                    | "tag"
                    | "notes"
                    | "reflog"
                    | "pack-refs"
            )
        )
    })
}

fn ordinary_empty_hooks(cwd: &Path) -> bool {
    if std::env::var_os("GIT_CONFIG_NOSYSTEM").as_deref() != Some(OsStr::new("1")) {
        return false;
    }
    let git = cwd.join(".git");
    if !git.is_dir()
        || fs::read_dir(git.join("hooks")).is_ok_and(|mut entries| {
            entries.any(|entry| entry.is_ok_and(|entry| executable(&entry.path())))
        })
    {
        return false;
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let global = std::env::var_os("GIT_CONFIG_GLOBAL")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".gitconfig"));
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    [
        git.join("config"),
        git.join("config.worktree"),
        global,
        xdg.join("git/config"),
    ]
    .iter()
    .all(|path| match fs::read_to_string(path) {
        Ok(text) => {
            let text = text.to_ascii_lowercase();
            !text.contains("hookspath") && !text.contains("include")
        }
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    })
}

#[allow(
    clippy::disallowed_methods,
    reason = "Hook discovery runs only Git's read-only configuration and path queries without recursive hook forwarding."
)]
fn query(command: &Command, cwd: &Path, arguments: &[&str]) -> Result<Option<String>> {
    let mut query = crate::child_command::command(command.get_program());
    let args = command.get_args().collect::<Vec<_>>();
    let mut index = 0;
    while index < args.len() && args[index].to_string_lossy().starts_with('-') {
        let argument = args[index];
        query.arg(argument);
        index += 1;
        if matches!(
            argument.to_str(),
            Some("-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env")
        ) && index < args.len()
        {
            query.arg(args[index]);
            index += 1;
        }
    }
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            query.env(key, value);
        } else {
            query.env_remove(key);
        }
    }
    let output = query.current_dir(cwd).args(arguments).output()?;
    Ok(output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string()
    }))
}

pub(crate) fn prepare(command: &mut Command) -> Result<Option<Hooks>> {
    let args = command
        .get_args()
        .map(OsStr::to_os_string)
        .collect::<Vec<_>>();
    if !hook_capable(&args) {
        return Ok(None);
    }
    // Explicitly unconfined destinations retain Git's ordinary hook execution.
    if crate::confinement::unconfined() {
        return Ok(None);
    }
    let cwd = command
        .get_current_dir()
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_dir()?);
    let overrides = command.get_envs().any(|(key, value)| {
        key.to_string_lossy().starts_with("GIT_CONFIG")
            && value.map(OsStr::to_os_string) != std::env::var_os(key)
    });
    if !overrides
        && !args
            .iter()
            .any(|arg| arg.to_string_lossy().starts_with('-'))
        && ordinary_empty_hooks(&cwd)
    {
        return Ok(None);
    }
    static INVENTORIES: OnceLock<Mutex<HashMap<Vec<OsString>, Option<Inventory>>>> =
        OnceLock::new();
    let key = std::iter::once(cwd.as_os_str().to_os_string())
        .chain(
            args.iter()
                .take_while(|argument| argument.to_string_lossy().starts_with('-'))
                .cloned(),
        )
        .collect::<Vec<_>>();
    let inventories = INVENTORIES.get_or_init(Default::default);
    let cached = inventories
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned();
    let inventory = match cached {
        Some(inventory) => inventory,
        None => {
            let directory = query(
                command,
                &cwd,
                &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
            )?;
            let inventory = if let Some(directory) = directory {
                let configured = query(
                    command,
                    &cwd,
                    &["config", "--path", "--get", "core.hooksPath"],
                )?;
                let configured_path = configured.is_some();
                let directory = configured
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(directory));
                let names = NAMES
                    .iter()
                    .copied()
                    .filter(|name| {
                        configured_path
                            || !directory.is_absolute()
                            || executable(&directory.join(name))
                    })
                    .collect::<Vec<_>>();
                if names.is_empty() {
                    None
                } else {
                    let common = query(command, &cwd, &["rev-parse", "--git-common-dir"])?
                        .context("hook command has no Git common directory")?;
                    let common = PathBuf::from(common);
                    Some(Inventory {
                        directory,
                        names,
                        common: fs::canonicalize(if common.is_absolute() {
                            common
                        } else {
                            cwd.join(common)
                        })?,
                    })
                }
            } else {
                None
            };
            inventories
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(key, inventory.clone());
            inventory
        }
    };
    let Some(inventory) = inventory else {
        return Ok(None);
    };
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
    let executable = crate::context::shell_quote(&std::env::current_exe()?.display().to_string());
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
    // Preserve stdio and pre-exec setup on the existing command by using command-scoped configuration.
    let mut count = command
        .get_envs()
        .find_map(|(key, value)| (key == "GIT_CONFIG_COUNT").then_some(value).flatten())
        .and_then(|value| value.to_str())
        .unwrap_or("0")
        .parse::<usize>()?;
    command
        .env(format!("GIT_CONFIG_KEY_{count}"), "core.hooksPath")
        .env(format!("GIT_CONFIG_VALUE_{count}"), &hooks.directory);
    count += 1;
    command.env("GIT_CONFIG_COUNT", count.to_string());
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
