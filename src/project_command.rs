//! Typed launch boundary for project programs and hook-capable Git operations.

use crate::confinement::{Declaration, Receipt};
use crate::perf::Counted;
use anyhow::Result;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Output, Stdio};

pub struct Command {
    inner: std::process::Command,
    kind: Kind,
    git_args: Option<Vec<OsString>>,
    storage: Option<crate::warm_worktree::CommandStorage>,
    receipt: Option<Receipt>,
    hooks: Option<crate::git_hooks::Hooks>,
    started: bool,
    inherit_environment: bool,
}

enum Kind {
    Project {
        checkout: PathBuf,
        declaration: Option<Declaration>,
    },
    Git,
    Operator,
}

/// Construct a project command only with an explicit destination scope.
pub fn new(program: impl AsRef<OsStr>, checkout: &Path, site: &'static str) -> Result<Command> {
    let declaration = crate::confinement::declaration(site, checkout)?;
    Ok(make(
        program,
        Kind::Project {
            checkout: checkout.to_path_buf(),
            declaration,
        },
    ))
}

pub(crate) fn git(program: impl AsRef<OsStr>) -> Command {
    make(program, Kind::Git)
}
pub(crate) fn operator(program: impl AsRef<OsStr>) -> Command {
    make(program, Kind::Operator)
}

#[allow(
    clippy::disallowed_methods,
    reason = "The typed launch boundary owns the environment-sanitized raw command."
)]
fn make(program: impl AsRef<OsStr>, kind: Kind) -> Command {
    Command {
        inner: crate::child_command::command(program),
        git_args: matches!(kind, Kind::Git).then(Vec::new),
        kind,
        storage: None,
        receipt: None,
        hooks: None,
        started: false,
        inherit_environment: true,
    }
}

impl std::ops::Deref for Command {
    type Target = std::process::Command;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Command {
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        if let Some(arguments) = &mut self.git_args {
            arguments.push(arg.as_ref().to_os_string());
        } else {
            self.inner.arg(arg);
        }
        self
    }
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for argument in args {
            self.arg(argument);
        }
        self
    }
    pub fn get_args(&self) -> impl Iterator<Item = &OsStr> {
        self.git_args
            .iter()
            .flatten()
            .map(OsString::as_os_str)
            .chain(self.inner.get_args())
    }
    pub fn current_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.inner.current_dir(path);
        self
    }
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.inner.env(key, value);
        self
    }
    pub fn envs<I, K, V>(&mut self, values: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.inner.envs(values);
        self
    }
    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.inner.env_remove(key);
        self
    }
    pub fn env_clear(&mut self) -> &mut Self {
        self.inherit_environment = false;
        self.inner.env_clear();
        self
    }
    pub fn stdin(&mut self, value: impl Into<Stdio>) -> &mut Self {
        self.inner.stdin(value);
        self
    }
    pub fn stdout(&mut self, value: impl Into<Stdio>) -> &mut Self {
        self.inner.stdout(value);
        self
    }
    pub fn stderr(&mut self, value: impl Into<Stdio>) -> &mut Self {
        self.inner.stderr(value);
        self
    }
    pub fn process_group(&mut self, group: i32) -> &mut Self {
        self.inner.process_group(group);
        self
    }
    pub unsafe fn pre_exec<F>(&mut self, callback: F) -> &mut Self
    where
        F: FnMut() -> io::Result<()> + Send + Sync + 'static,
    {
        self.inner.pre_exec(callback);
        self
    }
    pub(crate) fn storage(&mut self, storage: crate::warm_worktree::CommandStorage) {
        self.storage = Some(storage);
    }
    pub fn confinement(&self) -> Option<Receipt> {
        self.receipt.clone()
    }

    fn prepare(&mut self) -> io::Result<()> {
        if self.started {
            return Err(io::Error::other("a typed command can only be started once"));
        }
        self.started = true;
        match &self.kind {
            Kind::Project {
                checkout,
                declaration: Some(declaration),
            } => {
                self.receipt = crate::confinement::apply_with_storage(
                    &mut self.inner,
                    checkout,
                    declaration,
                    self.storage.as_ref(),
                )
                .map_err(io::Error::other)?;
            }
            Kind::Project { .. } | Kind::Operator => {}
            Kind::Git => {
                if self.hooks.is_none() {
                    let arguments = self.get_args().map(OsStr::to_os_string).collect::<Vec<_>>();
                    self.git_args = None;
                    self.hooks = crate::git_hooks::prepare(
                        &mut self.inner,
                        &arguments,
                        self.inherit_environment,
                    )
                    .map_err(io::Error::other)?;
                }
            }
        }
        Ok(())
    }

    pub fn spawn(&mut self) -> io::Result<Child> {
        self.counted_spawn()
    }
    pub fn output(&mut self) -> io::Result<Output> {
        self.counted_output()
    }
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        let status = self.spawn()?.wait()?;
        self.failed(status);
        Ok(status)
    }
    fn failed(&self, status: ExitStatus) {
        if !status.success() {
            if let Some(receipt) = &self.receipt {
                if receipt.confined {
                    receipt.failed();
                }
            }
        }
    }
}

impl Counted for Command {
    fn counted_output(&mut self) -> io::Result<Output> {
        self.prepare()?;
        let output = self.inner.counted_output()?;
        if matches!(self.kind, Kind::Git) && output.status.success() {
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            if diagnostic.contains("ran confined (")
                || diagnostic.contains("destination confinement scope")
            {
                eprint!("{diagnostic}");
            }
        }
        self.failed(output.status);
        Ok(output)
    }
    fn counted_spawn(&mut self) -> io::Result<Child> {
        self.prepare()?;
        let mut child = self.inner.counted_spawn()?;
        if let Some(hooks) = self.hooks.take() {
            hooks.hold_for(&mut child)?;
        }
        Ok(child)
    }
    fn confinement(&self) -> Option<Receipt> {
        self.receipt.clone()
    }
}
