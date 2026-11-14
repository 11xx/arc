//! Named host admission, shared by every project in an account.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

pub const WAITER: &str = "--arc-exclusive-wait";

#[derive(Clone)]
pub struct Request {
    pub name: String,
    pub admission: Duration,
}

impl Request {
    pub fn for_gate(gate: &crate::gates::Gate) -> Option<Self> {
        gate.exclusive.as_ref().map(|name| Self {
            name: name.clone(),
            admission: gate.admission.unwrap_or(Duration::from_secs(60)),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Owner<'a> {
    project: String,
    change: &'a str,
    gate: &'a str,
    started_ns: Option<u64>,
}

struct Waiter(Child);

impl Drop for Waiter {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Acquire one name, or return without running a command at the deadline.
pub fn acquire(
    request: &Request,
    cwd: &Path,
    change: &str,
    gate: &str,
) -> Result<(Option<File>, u64)> {
    // Lock names share one account namespace even when projects relocate their ledgers.
    let home = crate::config::sandbox()?.unwrap_or(crate::config::real_home()?);
    let root = crate::config::ai_home_under(&home).join("arc/exclusive");
    fs::create_dir_all(&root)?;
    let path = root.join(format!(
        "{}.lock",
        hex::encode(Sha256::digest(request.name.as_bytes()))
    ));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "exclusive lock is not a regular file"
    );
    let started = Instant::now();
    // SAFETY: flock borrows a descriptor file owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error).context("cannot take exclusive lock");
        }
        let mut bytes = Vec::new();
        File::open(&path)?.take(65536).read_to_end(&mut bytes)?;
        if let Ok(owner) = serde_json::from_slice::<Owner<'_>>(&bytes) {
            let held_ms = owner
                .started_ns
                .zip(crate::verification_timing::monotonic_ns())
                .map(|(start, end)| end.saturating_sub(start) / 1_000_000)
                .unwrap_or(0);
            eprintln!(
                "waiting for exclusive {:?}: project {}, change {}, gate {}, held for {:.3}s",
                request.name,
                owner.project,
                owner.change,
                owner.gate,
                held_ms as f64 / 1000.0
            );
        } else {
            eprintln!(
                "waiting for exclusive {:?}: holder details pending",
                request.name
            );
        }
        let fd = file.as_raw_fd();
        let parent = std::process::id();
        let mut command = crate::child_command::command(std::env::current_exe()?);
        command
            .arg(WAITER)
            .arg(fd.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // SAFETY: only async-signal-safe calls run before exec. The helper cannot
        // remain blocked after its launcher dies, including a death during spawn.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() != parent as i32
                    || libc::fcntl(fd, libc::F_SETFD, 0) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut waiter = Waiter(command.spawn().context("cannot start exclusive wait")?);
        let ready = waiter
            .0
            .stdout
            .as_ref()
            .context("exclusive wait pipe unavailable")?;
        let remaining = request.admission.saturating_sub(started.elapsed());
        if !wait_readable(ready.as_raw_fd(), Some(remaining))? {
            eprintln!(
                "exclusive {:?}: not-admitted after {:.3}s",
                request.name,
                started.elapsed().as_secs_f64()
            );
            return Ok((None, started.elapsed().as_millis() as u64));
        }
        anyhow::ensure!(waiter.0.wait()?.success(), "exclusive lock wait failed");
    }
    let owner = Owner {
        project: crate::gitio::primary_worktree(cwd)?.display().to_string(),
        change,
        gate,
        started_ns: crate::verification_timing::monotonic_ns(),
    };
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    serde_json::to_writer(&mut file, &owner)?;
    file.flush()?;
    Ok((Some(file), started.elapsed().as_millis() as u64))
}

/// Wait for pipe data or EOF, with no recurring wakeup.
pub(crate) fn wait_readable(fd: i32, timeout: Option<Duration>) -> std::io::Result<bool> {
    let deadline = timeout
        .map(|timeout| {
            Instant::now().checked_add(timeout).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "admission duration is too large",
                )
            })
        })
        .transpose()?;
    loop {
        let mut ready = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let milliseconds = deadline
            .map(|deadline| {
                let remaining = deadline.saturating_duration_since(Instant::now());
                remaining
                    .as_millis()
                    .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0))
                    .min(i32::MAX as u128) as i32
            })
            .unwrap_or(-1);
        // SAFETY: poll borrows one initialized pollfd.
        let result = unsafe { libc::poll(&mut ready, 1, milliseconds) };
        if result > 0 {
            return Ok(true);
        }
        if result == 0 {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(false);
            }
            continue;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
        crate::interruption::check().map_err(std::io::Error::other)?;
    }
}

/// Internal exec entry point: the parent owns the same open file description.
pub fn waiter_main() -> ! {
    let Some(fd) = std::env::args()
        .nth(2)
        .and_then(|raw| raw.parse::<i32>().ok())
    else {
        std::process::exit(2);
    };
    loop {
        // SAFETY: the parent explicitly inherited this lock descriptor.
        if unsafe { libc::flock(fd, libc::LOCK_EX) } == 0 {
            std::process::exit(0);
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            std::process::exit(1);
        }
    }
}
