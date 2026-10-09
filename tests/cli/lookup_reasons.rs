use crate::common::*;
use crate::model_resolution::{acting, assistant, prompt, recording};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;

const CODEX_SESSION: &str = "019f0000-0000-7000-8000-000000000002";
const UNRESOLVED_MODEL: &str =
    "# export ARC_MODEL=<model[#effort]>  # unavailable: the session store could not resolve or read this id: ";
const UNRESOLVED_SESSION: &str = "# session unresolved: the codex session store could not establish whether this id has a readable recording: ";

/// `arc env` in a process whose Codex thread is `session`.
fn codex_env(repo: &Repo, session: &str) -> String {
    stdout(
        repo.arc(&repo.root)
            .arg("env")
            .env("CODEX_HOME", repo.home.join("codex-state"))
            .env("CODEX_THREAD_ID", session),
    )
}

/// The opening event of `slug`, read back from the ledger.
fn opened(repo: &Repo, slug: &str) -> Value {
    let events =
        stdout(
            repo.arc(&repo.root)
                .args(["events", "--change", slug, "--type", "change-opened"]),
        );
    serde_json::from_str(events.trim()).unwrap()
}

/// Open `slug` with the Codex harness and `session` declared, not detected.
fn begin_declared_codex(repo: &Repo, slug: &str, session: &str) -> Value {
    stdout(
        repo.arc(&repo.root)
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", session)
            .env("CODEX_HOME", repo.home.join("codex-state"))
            .args(["begin", slug]),
    );
    opened(repo, slug)
}

/// Open `slug` with identity detection supplying the Codex session.
fn begin_detected_codex(repo: &Repo, slug: &str, session: &str) -> Value {
    stdout(
        repo.arc(&repo.root)
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_SESSION")
            .env("CODEX_HOME", repo.home.join("codex-state"))
            .env("CODEX_THREAD_ID", session)
            .args(["begin", slug]),
    );
    opened(repo, slug)
}

/// The text after `prefix` on the one `arc env` line that starts with it.
fn reason_after<'a>(output: &'a str, prefix: &str) -> &'a str {
    let lines: Vec<&str> = output
        .lines()
        .filter_map(|line| line.strip_prefix(prefix))
        .collect();
    assert_eq!(lines.len(), 1, "one line starts with {prefix:?}:\n{output}");
    lines[0]
}

#[test]
fn an_unreadable_recording_names_its_permission_error_in_env_and_on_events() {
    let repo = Repo::new();
    enable_identity_detection(&repo);
    let path = codex_recording(&repo, CODEX_SESSION, "gpt-fixture");

    let readable = codex_env(&repo, CODEX_SESSION);
    assert!(readable.contains("# session corroborated"), "{readable}");
    for event in [
        begin_declared_codex(&repo, "readable-declared", CODEX_SESSION),
        begin_detected_codex(&repo, "readable-detected", CODEX_SESSION),
    ] {
        assert_eq!(event["session_resolution"], "corroborated", "{event}");
        assert!(event.get("session_resolution_reason").is_none(), "{event}");
        assert_eq!(event["model"], "gpt-fixture#high", "{event}");
    }

    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = codex_env(&repo, CODEX_SESSION);
    let reason = reason_after(&unreadable, UNRESOLVED_SESSION);
    assert!(
        reason.ends_with("open file failed (PermissionDenied)"),
        "{unreadable}"
    );
    assert_eq!(reason_after(&unreadable, UNRESOLVED_MODEL), reason);
    assert!(!unreadable.contains("gpt-fixture"), "{unreadable}");
    for event in [
        begin_declared_codex(&repo, "unreadable-declared", CODEX_SESSION),
        begin_detected_codex(&repo, "unreadable-detected", CODEX_SESSION),
    ] {
        assert_eq!(event["session_resolution"], "unresolved", "{event}");
        assert_eq!(event["session_resolution_reason"], reason, "{event}");
        assert!(event.get("model").is_none(), "{event}");
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn a_prefix_match_is_unresolved_and_names_both_ids() {
    let repo = Repo::new();
    let prefix = "019f0000";
    codex_recording(&repo, CODEX_SESSION, "gpt-fixture");
    let expected =
        format!("session {prefix} resolves to the recording of a different id, {CODEX_SESSION}");

    let output = codex_env(&repo, prefix);
    assert_eq!(reason_after(&output, UNRESOLVED_SESSION), expected);
    assert_eq!(reason_after(&output, UNRESOLVED_MODEL), expected);
    assert!(!output.contains("uncorroborated"), "{output}");
    assert!(!output.contains("gpt-fixture"), "{output}");

    let event = begin_declared_codex(&repo, "prefix-declared", prefix);
    assert_eq!(event["session"], prefix, "{event}");
    assert_eq!(event["session_resolution"], "unresolved", "{event}");
    assert_eq!(event["session_resolution_reason"], expected, "{event}");
    assert!(event.get("model").is_none(), "{event}");
}

#[test]
fn declared_identity_records_the_store_verdict_on_every_write() {
    let repo = Repo::new();

    // The fixture declares a harness arc reads no store for.
    stdout(repo.arc(&repo.root).args(["begin", "storeless-harness"]));
    let event = opened(&repo, "storeless-harness");
    assert_eq!(event["harness"], "test", "{event}");
    assert_eq!(event["session_resolution"], "uncorroborated", "{event}");
    assert_eq!(
        event["session_resolution_reason"], "arc reads no session store for the test harness",
        "{event}"
    );

    let event = begin_declared_codex(&repo, "missing-recording", CODEX_SESSION);
    assert_eq!(event["session_resolution"], "uncorroborated", "{event}");
    assert_eq!(
        event["session_resolution_reason"], "no recording for this session in the configured store",
        "{event}"
    );

    recording(
        &repo,
        &[
            prompt("u1", "2026-01-01T10:00:00Z"),
            assistant("a1", "2026-01-01T10:00:01Z", "high"),
        ],
    );
    let opened_claude = stdout(acting(&repo).args(["begin", "corroborated-claude"]));
    let change = opened_change_id(&opened_claude);
    acting(&repo)
        .args(["comment", &change, "--body", "declared and corroborated"])
        .assert()
        .success();
    let events: Vec<Value> = stdout(acting(&repo).args(["events", "--change", &change]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 2, "{events:?}");
    for event in events {
        assert_eq!(event["harness"], "claude", "{event}");
        assert_eq!(event["session_resolution"], "corroborated", "{event}");
        assert!(event.get("session_resolution_reason").is_none(), "{event}");
        assert_eq!(event["model"], "m#high", "{event}");
    }
}
