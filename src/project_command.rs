//! Typed launch boundary for project programs and the operator programs beside them.

use crate::confinement::{Declaration, Receipt};
use crate::perf::Counted;
use anyhow::Result;
use std::ffi::OsStr;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Output, Stdio};

pub struct Command {
    inner: std::process::Command,
    kind: Kind,
    storage: Option<crate::warm_worktree::CommandStorage>,
    receipt: Option<Receipt>,
    started: bool,
}

enum Kind {
    Project {
        checkout: PathBuf,
        declaration: Option<Declaration>,
    },
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
        kind,
        storage: None,
        receipt: None,
        started: false,
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
        self.inner.arg(arg);
        self
    }
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.inner.args(args);
        self
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
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        let status = self.counted_spawn()?.wait()?;
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
        self.failed(output.status);
        Ok(output)
    }
    fn counted_spawn(&mut self) -> io::Result<Child> {
        self.prepare()?;
        self.inner.counted_spawn()
    }
    fn confinement(&self) -> Option<Receipt> {
        self.receipt.clone()
    }
}
