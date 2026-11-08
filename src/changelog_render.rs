//! Runs a configured program that answers on stdout: the changelog renderer
//! a project declares in `.arc/changelog.toml`, and the intake count commands
//! the operator declares in arc's configuration.
//!
//! The program runs with the invoking user's authority the way a gate is. It
//! runs as argv, never through a shell, from the directory its caller names,
//! as the leader of its own process group, with the environment arc was
//! given. It reads the request document its caller gives it on stdin, or
//! gets a null stdin when there is none, and answers on stdout. Every way it
//! can fail to answer — it cannot start, it exits non-zero, it overruns its
//! deadline, it prints more than `MAX_OUTPUT_BYTES`, or it prints bytes that
//! are not UTF-8 — is a refusal carrying the tail of its stderr, and the
//! caller writes nothing.

use crate::process_group::{read_tail, OwnedGroup, STDERR_TAIL_BYTES};
use anyhow::{Context, Result};
use std::fmt;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The most stdout a program may print. A changelog is text a person reads
/// and a count is a few bytes; anything larger is a program gone wrong, and
/// holding it would cost memory without bound.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// How long a program's stdout and stderr may stay open after its group was
/// killed at the deadline. A descendant that left the group can hold a pipe
/// open indefinitely, and the refusal must not wait on it.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// Why a program produced no usable answer.
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

/// A program that did not answer, with what it said on stderr.
#[derive(Debug)]
pub struct Refusal {
    pub cause: Cause,
    pub stderr_tail: String,
}

/// Run `argv` from `cwd` with `request` on stdin, or a null stdin without
/// one, and hand back its stdout. `role` names the program in arc's own
/// supervision errors.
///
/// The outer error is arc failing to supervise the child; the inner one is
/// the program failing to answer.
pub fn run(
    role: &str,
    argv: &[String],
    cwd: &Path,
    request: Option<&[u8]>,
    timeout: Duration,
) -> Result<std::result::Result<String, Refusal>> {
    let (program, arguments) = argv
        .split_first()
        .with_context(|| format!("{role} command is empty"))?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .with_context(|| format!("{role} timeout is too large"))?;
    let mut group = match OwnedGroup::spawn(
        Command::new(program)
            .args(arguments)
            .current_dir(cwd)
            .stdin(if request.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0),
    ) {
        Ok(group) => group,
        Err(error) => {
            return Ok(Err(Refusal {
                cause: Cause::Start(error),
                stderr_tail: String::new(),
            }))
        }
    };
    let child = group.child();
    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("{role} stdout pipe unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .with_context(|| format!("{role} stderr pipe unavailable"))?;

    // A program may answer without reading its whole request, so a pipe it
    // closed is its choice, and one that never got its request fails on its
    // own terms. The writer is never joined: a descendant holding stdin open
    // without reading must not stall arc.
    if let Some(request) = request {
        let mut stdin = child
            .stdin
            .take()
            .with_context(|| format!("{role} stdin pipe unavailable"))?;
        let request = request.to_vec();
        std::thread::spawn(move || {
            let _ = crate::process_group::write_input(&mut stdin, &request);
        });
    }

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

    let mut exited = false;
    let (mut stdout_finished, mut stderr_finished) = (false, false);
    let mut timed_out = false;
    let mut drain_until = None;
    loop {
        if !exited {
            exited = group
                .leader_exited()
                .with_context(|| format!("failed to wait for {role}"))?;
        }
        while let Ok(stream) = done_rx.try_recv() {
            match stream {
                Stream::Stdout => stdout_finished = true,
                Stream::Stderr => stderr_finished = true,
            }
        }
        if exited && stdout_finished && stderr_finished {
            break;
        }
        if drain_until.is_some_and(|until| Instant::now() >= until) {
            break;
        }
        if drain_until.is_none() && Instant::now() >= deadline {
            group
                .kill()
                .with_context(|| format!("failed to kill overrunning {role}"))?;
            timed_out = true;
            drain_until = Some(Instant::now() + DRAIN_GRACE);
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let stderr_tail = if stderr_finished {
        stderr_reader
            .join()
            .map_err(|_| anyhow::anyhow!("{role} stderr reader panicked"))?
            .with_context(|| format!("failed to read {role} stderr"))?
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
        .map_err(|_| anyhow::anyhow!("{role} stdout reader panicked"))?
        .with_context(|| format!("failed to read {role} stdout"))?;
    if oversized {
        return refuse(Cause::Oversized);
    }
    let status = group
        .stop()
        .with_context(|| format!("failed to reap {role}"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A command renderer that answers without reading its whole request is
    /// judged by its own exit status and output. Rejects the request writer
    /// meeting the pipe the renderer closed and raising SIGPIPE, which under
    /// the disposition `main` restores ends `arc changelog` silently.
    #[test]
    fn a_renderer_that_skips_its_request_answers_on_its_own_terms() {
        crate::process_group::test_support::under_default_sigpipe(
            "changelog_render::tests::a_renderer_that_skips_its_request_answers_on_its_own_terms",
            || {
                let cwd = tempfile::tempdir().unwrap();
                let request = vec![b'x'; 1024 * 1024];
                let render = |script: &str| {
                    run(
                        "changelog renderer",
                        &["sh".into(), "-c".into(), script.into()],
                        cwd.path(),
                        Some(&request),
                        crate::process_group::test_support::HANG_BOUND,
                    )
                    .unwrap()
                };
                assert_eq!(render("printf rendered").unwrap(), "rendered");
                let refusal = render("exit 3").unwrap_err();
                assert!(matches!(refusal.cause, Cause::Exit(Some(3))));
                // The request writer is never joined; give a signal it would
                // raise the time to land.
                std::thread::sleep(Duration::from_millis(200));
            },
        );
    }

    /// A renderer that answers leaves nothing running in its process group.
    /// Rejects reaping a renderer that exited and leaving the descendants it
    /// started in its group running after `arc changelog` returns.
    #[test]
    fn a_renderer_that_answers_leaves_nothing_in_its_group() {
        use crate::process_group::test_support::{ends_within, read_pid, HANG_BOUND};
        let cwd = tempfile::tempdir().unwrap();
        let answer = run(
            "changelog renderer",
            &[
                "sh".into(),
                "-c".into(),
                format!(
                    "sleep {} >/dev/null 2>&1 & echo $! > descendant.pid; printf rendered",
                    crate::process_group::test_support::OUTLIVING_SECS
                ),
            ],
            cwd.path(),
            Some(b""),
            HANG_BOUND,
        )
        .unwrap();
        let descendant = read_pid(cwd.path(), "descendant.pid");
        let ended = ends_within(descendant, HANG_BOUND);
        if !ended {
            crate::process_group::test_support::kill(descendant);
        }
        assert_eq!(answer.unwrap(), "rendered");
        assert!(
            ended,
            "renderer descendant {descendant} outlived the renderer"
        );
    }

    /// The renderer's group ID stays reserved by its unreaped leader until
    /// arc has signalled the group. Rejects reaping the leader as soon as it
    /// exits: with the group's only other member escaped into its own
    /// session, the reaped ID is free for reuse, and a later kill of the
    /// group would reach whatever process group next takes that number.
    #[test]
    fn the_renderer_leader_stays_reserved_until_its_group_is_stopped() {
        use crate::process_group::test_support::{leader_script, LeaderObservation, HANG_BOUND};
        let cwd = tempfile::tempdir().unwrap();
        let path = cwd.path().to_owned();
        let render = std::thread::spawn(move || {
            run(
                "changelog renderer",
                &["sh".into(), "-c".into(), leader_script()],
                &path,
                Some(b""),
                HANG_BOUND,
            )
        });
        let observation = LeaderObservation::new(cwd.path());
        let reserved = observation.reserved_after_exit();
        observation.kill_escaped();
        let answer = render.join().unwrap().unwrap();
        assert!(
            reserved,
            "renderer leader was reaped while its group could still be signalled"
        );
        assert_eq!(answer.unwrap(), "");
    }
}
