//! Open-item counts from the intakes the operator configures, for `arc catchup`.
//!
//! An intake is a place outside arc where a person leaves items for agents to
//! triage. Catchup says only how many are open: it never reads an item,
//! prints one, or starts a triage. Each `[intake.<name>]` command runs as
//! argv from the current directory, with a null stdin and the environment arc
//! was given, and must answer within [`TIMEOUT`] with JSON whose top-level
//! integer `total` is the count. Any other answer leaves that intake
//! unavailable with a reason, and never fails catchup.

use crate::config::IntakeCommand;
use serde::Serialize;
use std::path::Path;
use std::thread::JoinHandle;
use std::time::Duration;

/// How long an intake command may take to answer.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(2);

/// One configured intake's answer: a count, or why there is none.
#[derive(Debug, Serialize)]
pub(crate) struct IntakeCount {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    open: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable: Option<String>,
}

impl IntakeCount {
    fn new(name: String, answer: Result<u64, String>) -> Self {
        let (open, unavailable) = match answer {
            Ok(open) => (Some(open), None),
            Err(reason) => (None, Some(reason)),
        };
        Self {
            name,
            open,
            unavailable,
        }
    }

    /// The catchup line: `intake: <name>: <n> open items`, or
    /// `intake: <name>: unavailable (<reason>)`.
    pub(crate) fn line(&self) -> String {
        match (self.open, &self.unavailable) {
            (Some(1), _) => format!("intake: {}: 1 open item", self.name),
            (Some(open), _) => format!("intake: {}: {open} open items", self.name),
            (None, reason) => format!(
                "intake: {}: unavailable ({})",
                self.name,
                reason.as_deref().unwrap_or("no answer")
            ),
        }
    }
}

/// Counts being taken, one thread per configured intake, so they run beside
/// catchup's own reads and a slow intake costs at most one timeout however
/// many are configured.
///
/// Dropped unread, as when catchup fails before reporting, it still waits
/// for every count: each ends within its timeout, and an intake command left
/// running would outlive arc in its own process group.
pub(crate) struct Pending(Vec<(String, JoinHandle<IntakeCount>)>);

/// Start counting every configured intake from `cwd`.
///
/// A configuration that cannot be loaded counts nothing: catchup already
/// names that error on stderr when it measures the worktree root.
pub(crate) fn start(cwd: &Path) -> Pending {
    let configured = crate::config::load()
        .map(|config| config.intake)
        .unwrap_or_default();
    Pending(
        configured
            .into_iter()
            .map(|(name, command)| {
                let cwd = cwd.to_path_buf();
                let thread_name = name.clone();
                let handle = std::thread::spawn(move || {
                    let answer = count(&thread_name, &command, &cwd);
                    IntakeCount::new(thread_name, answer)
                });
                (name, handle)
            })
            .collect(),
    )
}

impl Pending {
    /// Every intake's answer, in configuration order.
    pub(crate) fn finish(mut self) -> Vec<IntakeCount> {
        std::mem::take(&mut self.0)
            .into_iter()
            .map(|(name, handle)| {
                handle.join().unwrap_or_else(|_| {
                    IntakeCount::new(name, Err("the count panicked".to_string()))
                })
            })
            .collect()
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        for (_, handle) in self.0.drain(..) {
            let _ = handle.join();
        }
    }
}

fn count(name: &str, command: &IntakeCommand, cwd: &Path) -> Result<u64, String> {
    if !crate::journal::valid_intake_name(name) {
        return Err("the name does not match [a-z][a-z0-9-]{0,31}".to_string());
    }
    if command.argv.is_empty() {
        return Err("argv is empty".to_string());
    }
    let stdout = crate::changelog_render::run("intake command", &command.argv, cwd, None, TIMEOUT)
        .map_err(|error| format!("{error:#}"))?
        .map_err(|refusal| refusal.cause.to_string())?;
    total(&stdout).ok_or_else(|| "output has no integer total".to_string())
}

/// The top-level integer `total` of a JSON answer.
fn total(stdout: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(stdout)
        .ok()?
        .get("total")?
        .as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a top-level non-negative integer `total` is a count. Rejects
    /// reading a count out of a string, a fraction, a nested field, or text
    /// that is not JSON.
    #[test]
    fn only_a_top_level_integer_total_is_a_count() {
        assert_eq!(
            total(r#"{"schema": "tray-list/1", "items": [], "total": 3}"#),
            Some(3)
        );
        assert_eq!(total(r#"{"total": 0}"#), Some(0));
        for garbage in [
            "",
            "3",
            "not json",
            r#"{"total": "3"}"#,
            r#"{"total": 2.5}"#,
            r#"{"total": -1}"#,
            r#"{"items": {"total": 3}}"#,
        ] {
            assert_eq!(total(garbage), None, "{garbage}");
        }
    }

    /// The catchup line says the count, or that there is none and why.
    #[test]
    fn the_line_names_the_intake_and_its_count_or_reason() {
        let line = |answer| IntakeCount::new("tray".to_string(), answer).line();
        assert_eq!(line(Ok(3)), "intake: tray: 3 open items");
        assert_eq!(line(Ok(1)), "intake: tray: 1 open item");
        assert_eq!(
            line(Err("exited with status 1".to_string())),
            "intake: tray: unavailable (exited with status 1)"
        );
    }
}
