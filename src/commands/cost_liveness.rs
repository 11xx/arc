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
    let mut child = Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| UNAVAILABLE)?;
    let stdout = child.stdout.take().ok_or(UNAVAILABLE)?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = crate::process_group::kill_process_group(child.id());
                let _ = child.wait();
                return Err(TIMED_OUT);
            }
            Err(_) => {
                let _ = crate::process_group::kill_process_group(child.id());
                let _ = child.wait();
                return Err(FAILED);
            }
        }
    };
    // The group leader exited; anything it left holding the pipe goes too.
    let _ = crate::process_group::kill_process_group(child.id());
    let bytes = crate::process_group::join_before_deadline(reader, Some(deadline), "agent-status")
        .ok()
        .flatten()
        .ok_or(TIMED_OUT)?
        .map_err(|_| UNREADABLE)?;
    if !status.success() {
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
