//! Whether a session is still running, from agent-status's snapshot.
//!
//! A recording cannot show whether its session has ended, so the cost view
//! asks agent-status once per read. Its bare invocation prints
//! `{"threads": [{"id", "harness", "state", ...}], "owner": {...}}`. A thread
//! listed in any state is live. A session of a harness agent-status reports
//! on that an owned snapshot does not list has ended. Everything else is
//! unknown with a fixed reason: no program, a failure, no answer within the
//! deadline, unreadable output, a snapshot no registry owner answered, which
//! reports nothing, or a harness agent-status does not report on. Raw output
//! and error text never reach the view.

use std::collections::BTreeSet;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The program asked, from `ARC_AGENT_STATUS` when set.
const PROGRAM_VAR: &str = "ARC_AGENT_STATUS";
const DEADLINE: Duration = Duration::from_secs(2);
/// Bound on the snapshot read; a longer one is unreadable.
const OUTPUT_LIMIT: u64 = 8 << 20;
/// The harnesses agent-status has adapters for.
const COVERED: [&str; 3] = ["claude", "codex", "pi"];

const UNAVAILABLE: &str = "agent-status is unavailable";
const FAILED: &str = "agent-status failed";
const TIMED_OUT: &str = "agent-status did not answer in time";
const UNREADABLE: &str = "agent-status output is unreadable";
const OWNERLESS: &str = "agent-status has no registry owner, so its snapshot reports nothing";
const UNCOVERED: &str = "agent-status does not report on this harness";

/// One session's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Liveness {
    Live,
    Ended,
    Unknown(&'static str),
}

impl Liveness {
    /// The answer as retained facts record it.
    pub(super) fn answer(self) -> &'static str {
        match self {
            Liveness::Live => "live",
            Liveness::Ended => "ended",
            Liveness::Unknown(_) => "unknown",
        }
    }

    pub(super) fn reason(self) -> Option<&'static str> {
        match self {
            Liveness::Unknown(reason) => Some(reason),
            _ => None,
        }
    }
}

/// One agent-status snapshot: the sessions it lists, or why there is none.
pub(super) enum Snapshot {
    Answered(BTreeSet<(String, String)>),
    Unknown(&'static str),
}

impl Snapshot {
    /// Ask agent-status once, within the deadline.
    pub(super) fn read() -> Self {
        let program = std::env::var_os(PROGRAM_VAR).unwrap_or_else(|| "agent-status".into());
        match run(&program) {
            Ok(text) => Self::parse(&text),
            Err(reason) => Snapshot::Unknown(reason),
        }
    }

    fn parse(text: &str) -> Self {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return Snapshot::Unknown(UNREADABLE);
        };
        let Some(threads) = value["threads"].as_array() else {
            return Snapshot::Unknown(UNREADABLE);
        };
        let listed: Option<BTreeSet<(String, String)>> = threads
            .iter()
            .map(|thread| {
                Some((
                    thread["harness"].as_str()?.to_string(),
                    thread["id"].as_str()?.to_string(),
                ))
            })
            .collect();
        match listed {
            None => Snapshot::Unknown(UNREADABLE),
            Some(_) if !value["owner"].is_object() => Snapshot::Unknown(OWNERLESS),
            Some(listed) => Snapshot::Answered(listed),
        }
    }

    pub(super) fn of(&self, harness: &str, session: &str) -> Liveness {
        match self {
            Snapshot::Unknown(reason) => Liveness::Unknown(reason),
            Snapshot::Answered(listed)
                if listed.contains(&(harness.to_string(), session.to_string())) =>
            {
                Liveness::Live
            }
            Snapshot::Answered(_) if COVERED.contains(&harness) => Liveness::Ended,
            Snapshot::Answered(_) => Liveness::Unknown(UNCOVERED),
        }
    }
}

fn run(program: &std::ffi::OsStr) -> Result<String, &'static str> {
    run_within(program, DEADLINE)
}

/// `program`'s output, if it answers within `deadline`.
fn run_within(program: &std::ffi::OsStr, deadline: Duration) -> Result<String, &'static str> {
    run_command_within(crate::child_command::command(program), deadline)
}

fn run_command_within(mut command: Command, deadline: Duration) -> Result<String, &'static str> {
    let mut group = crate::process_group::OwnedGroup::spawn(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0),
    )
    .map_err(|_| UNAVAILABLE)?;
    let stdout = group.child().stdout.take().ok_or(UNAVAILABLE)?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + deadline;
    loop {
        match group.leader_exited() {
            Ok(true) => break,
            Ok(false) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(false) => return Err(TIMED_OUT),
            Err(_) => return Err(FAILED),
        }
    }
    // The group leader exited; anything it left holding the pipe goes too.
    let _ = group.kill();
    let bytes = crate::process_group::join_before_deadline(reader, Some(deadline), "agent-status")
        .ok()
        .flatten()
        .ok_or(TIMED_OUT)?
        .map_err(|_| UNREADABLE)?;
    if !group.stop().map_err(|_| FAILED)?.success() {
        return Err(FAILED);
    }
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err(UNREADABLE);
    }
    String::from_utf8(bytes).map_err(|_| UNREADABLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status command's group ID stays reserved by its unreaped leader
    /// until arc has signalled the group. Rejects reaping the leader as soon
    /// as it exits: with the group's only other member escaped into its own
    /// session, the reaped ID is free for reuse, and the kill sent to clear
    /// the leader's pipe would reach whatever process group next takes that
    /// number. Fixture scripts are read by the shell so a writable descriptor
    /// inherited by another fork cannot prevent the status process from starting.
    #[test]
    fn the_status_leader_stays_reserved_until_its_group_is_stopped() {
        use crate::process_group::test_support::{leader_script, LeaderObservation, HANG_BOUND};
        use std::os::unix::fs::PermissionsExt;
        let cwd = tempfile::tempdir().unwrap();
        let program = cwd.path().join("agent-status");
        std::fs::write(
            &program,
            format!("#!/bin/sh\ncd \"$(dirname \"$0\")\"\n{}\n", leader_script()),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&program)
            .unwrap();
        use std::os::fd::{AsRawFd, FromRawFd};
        let directory = std::ffi::CString::new(cwd.path().to_str().unwrap()).unwrap();
        // SAFETY: inotify owns its descriptor and borrows the NUL-terminated path.
        let mut notifications = unsafe {
            let fd = libc::inotify_init1(libc::IN_CLOEXEC);
            assert!(fd >= 0);
            assert!(libc::inotify_add_watch(fd, directory.as_ptr(), libc::IN_CLOSE_WRITE) >= 0);
            std::fs::File::from_raw_fd(fd)
        };
        let (completed, completion) = std::os::unix::net::UnixStream::pair().unwrap();
        let path = program.clone();
        let status = std::thread::spawn(move || {
            let mut command = Command::new("sh");
            command.arg(&path);
            let answer = run_command_within(command, HANG_BOUND);
            drop(completion);
            answer
        });
        let mut status = Some(status);
        let mut finished = false;
        loop {
            if std::fs::read_to_string(cwd.path().join("leader.pid"))
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
                .is_some()
            {
                break;
            }
            assert!(
                !finished,
                "status fixture ended before writing its PID: {:?}",
                status.take().unwrap().join().unwrap()
            );
            let mut ready = [
                libc::pollfd {
                    fd: notifications.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: completed.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: poll borrows two initialized descriptors with a deadlock watchdog.
            let observed =
                unsafe { libc::poll(ready.as_mut_ptr(), 2, HANG_BOUND.as_millis() as i32) };
            if observed < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            assert!(observed > 0, "status fixture never became ready");
            if ready[0].revents != 0 {
                let count = notifications.read(&mut [0; 4096]).unwrap();
                assert!(count > 0, "status readiness watcher closed");
            }
            finished = ready[1].revents != 0;
        }
        let observation = LeaderObservation::new(cwd.path());
        let reserved = observation.reserved_after_exit();
        observation.kill_escaped();
        let answer = status.take().unwrap().join().unwrap();
        assert!(
            reserved,
            "status leader was reaped while its group could still be signalled"
        );
        assert_eq!(answer.as_deref(), Ok(""));
    }

    const OWNER: &str = r#""owner": {"pid": 1}"#;

    #[test]
    fn a_listed_thread_is_live_in_any_state_and_an_absent_covered_one_ended() {
        let snapshot = Snapshot::parse(&format!(
            r#"{{"threads": [{{"id": "s1", "harness": "claude", "state": "completed"}}], {OWNER}}}"#
        ));
        assert_eq!(snapshot.of("claude", "s1"), Liveness::Live);
        assert_eq!(snapshot.of("claude", "s2"), Liveness::Ended);
        assert_eq!(snapshot.of("codex", "s1"), Liveness::Ended);
        assert_eq!(snapshot.of("pi", "s3"), Liveness::Ended);
    }

    /// Claim: absence answers only for a harness agent-status reports on.
    /// Rejects reading an opencode session as ended because no adapter
    /// could have listed it.
    #[test]
    fn absence_for_a_harness_agent_status_does_not_cover_is_unknown() {
        let snapshot = Snapshot::parse(&format!(r#"{{"threads": [], {OWNER}}}"#));
        assert_eq!(snapshot.of("opencode", "s1"), Liveness::Unknown(UNCOVERED));
        assert_eq!(snapshot.of("test", "s1"), Liveness::Unknown(UNCOVERED));
    }

    /// Claim: an empty snapshot no owner answered reports nothing. Rejects
    /// reading every session as ended when no follower is running.
    #[test]
    fn an_ownerless_or_malformed_snapshot_is_unknown() {
        for text in [
            r#"{"threads": []}"#,
            "not json",
            r#"{"threads": {}}"#,
            r#"{"threads": [{"id": 1, "harness": "pi"}], "owner": {}}"#,
        ] {
            assert!(
                matches!(Snapshot::parse(text).of("pi", "s1"), Liveness::Unknown(_)),
                "{text}"
            );
        }
    }
}
