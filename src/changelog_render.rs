//! Runs the changelog renderer a project declares in `.arc/changelog.toml`.
//!
//! The renderer is the project's program, run with the invoking user's
//! authority the way a gate is. It runs as argv, never through a shell, from
//! the repository root, as the leader of its own process group, with the
//! environment arc was given. It reads one request document on stdin and
//! answers on stdout. Every way it can fail to answer — it cannot start, it
//! exits non-zero, it overruns its deadline, it prints more than
//! `MAX_OUTPUT_BYTES`, or it prints bytes that are not UTF-8 — is a refusal
//! carrying the tail of its stderr, and the caller writes nothing.

use crate::process_group::{kill_process_group, read_tail, STDERR_TAIL_BYTES};
use anyhow::{Context, Result};
use std::fmt;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The most stdout a renderer may print. A changelog is text a person reads;
/// anything larger is a renderer gone wrong, and holding it would cost memory
/// without bound.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// How long a renderer's stdout and stderr may stay open after its group was
/// killed at the deadline. A descendant that left the group can hold a pipe
/// open indefinitely, and the refusal must not wait on it.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// Why a renderer produced no usable answer.
#[derive(Debug)]
pub enum Cause {
    Start(std::io::Error),
    Exit(Option<i32>),
    TimedOut(Duration),
    Oversized,
    NotUtf8,
}

impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cause::Start(error) => write!(f, "could not start: {error}"),
            Cause::Exit(Some(code)) => write!(f, "exited with status {code}"),
            Cause::Exit(None) => write!(f, "was killed by a signal"),
            Cause::TimedOut(timeout) => {
                write!(f, "did not finish within {}s", timeout.as_secs())
            }
            Cause::Oversized => write!(
                f,
                "printed more than {} MiB",
                MAX_OUTPUT_BYTES / (1024 * 1024)
            ),
            Cause::NotUtf8 => write!(f, "printed output that is not UTF-8"),
        }
    }
}

enum Stream {
    Stdout,
    Stderr,
}

/// A renderer that did not answer, with what it said on stderr.
#[derive(Debug)]
pub struct Refusal {
    pub cause: Cause,
    pub stderr_tail: String,
}

/// Run `argv` from `cwd` with `request` on stdin and hand back its stdout.
///
/// The outer error is arc failing to supervise the child; the inner one is
/// the renderer failing to answer.
pub fn run(
    argv: &[String],
    cwd: &Path,
    request: &[u8],
    timeout: Duration,
) -> Result<std::result::Result<String, Refusal>> {
    let (program, arguments) = argv
        .split_first()
        .context("changelog renderer command is empty")?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("changelog renderer timeout is too large")?;
    let mut child = match Command::new(program)
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return Ok(Err(Refusal {
                cause: Cause::Start(error),
                stderr_tail: String::new(),
            }))
        }
    };
    let mut stdin = child
        .stdin
        .take()
        .context("changelog renderer stdin pipe unavailable")?;
    let stdout = child
        .stdout
        .take()
        .context("changelog renderer stdout pipe unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("changelog renderer stderr pipe unavailable")?;

    // A renderer may answer without reading its whole request, so a pipe it
    // closed is its choice, and one that never got its request fails on its
    // own terms. The writer is never joined: a descendant holding stdin open
    // without reading must not stall arc.
    let request = request.to_vec();
    std::thread::spawn(move || {
        let _ = stdin.write_all(&request);
    });

    let (done_tx, done_rx) = mpsc::sync_channel(2);
    let stdout_done = done_tx.clone();
    let stdout_reader = std::thread::spawn(move || {
        let captured = read_capped(stdout, MAX_OUTPUT_BYTES);
        let _ = stdout_done.send(Stream::Stdout);
        captured
    });
    let stderr_reader = std::thread::spawn(move || {
        let tail = read_tail(stderr, STDERR_TAIL_BYTES);
        let _ = done_tx.send(Stream::Stderr);
        tail
    });

    let mut status = None;
    let (mut stdout_finished, mut stderr_finished) = (false, false);
    let mut timed_out = false;
    let mut drain_until = None;
    loop {
        if status.is_none() {
            status = child
                .try_wait()
                .context("failed to wait for changelog renderer")?;
        }
        while let Ok(stream) = done_rx.try_recv() {
            match stream {
                Stream::Stdout => stdout_finished = true,
                Stream::Stderr => stderr_finished = true,
            }
        }
        if status.is_some() && stdout_finished && stderr_finished {
            break;
        }
        if drain_until.is_some_and(|until| Instant::now() >= until) {
            break;
        }
        if drain_until.is_none() && Instant::now() >= deadline {
            kill_process_group(child.id())
                .context("failed to kill overrunning changelog renderer")?;
            if status.is_none() {
                status = Some(
                    child
                        .wait()
                        .context("failed to reap overrunning changelog renderer")?,
                );
            }
            timed_out = true;
            drain_until = Some(Instant::now() + DRAIN_GRACE);
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let stderr_tail = if stderr_finished {
        stderr_reader
            .join()
            .map_err(|_| anyhow::anyhow!("changelog renderer stderr reader panicked"))?
            .context("failed to read changelog renderer stderr")?
    } else {
        String::new()
    };
    let refuse = |cause| {
        Ok(Err(Refusal {
            cause,
            stderr_tail: stderr_tail.clone(),
        }))
    };
    if timed_out {
        return refuse(Cause::TimedOut(timeout));
    }
    let (stdout, oversized) = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("changelog renderer stdout reader panicked"))?
        .context("failed to read changelog renderer stdout")?;
    if oversized {
        return refuse(Cause::Oversized);
    }
    let status = status.context("changelog renderer exited without an observed status")?;
    if !status.success() {
        return refuse(Cause::Exit(status.code()));
    }
    match String::from_utf8(stdout) {
        Ok(stdout) => Ok(Ok(stdout)),
        Err(_) => refuse(Cause::NotUtf8),
    }
}

/// Up to `limit` bytes of a reader, and whether it held more. The excess is
/// read and discarded, so the writer is never blocked on a full pipe.
fn read_capped(mut reader: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut captured = Vec::new();
    let mut oversized = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            return Ok((captured, oversized));
        }
        if oversized {
            continue;
        }
        if captured.len() + read > limit {
            oversized = true;
            captured = Vec::new();
            continue;
        }
        captured.extend_from_slice(&chunk[..read]);
    }
}
