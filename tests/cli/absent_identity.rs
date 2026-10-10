//! A harness, session or actor nobody established is recorded as absent. arc
//! never writes a word of its own in its place, so a reader can tell a value
//! somebody declared from one nobody had.

use super::common::*;

pub(crate) fn journal_dir(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim())
}

/// `arc` with no harness and no session, as a caller that never learned
/// either would run it.
pub(crate) fn without_session(repo: &Repo, cwd: &Path) -> AssertCommand {
    let mut command = repo.arc(cwd);
    command.env_remove("ARC_HARNESS").env_remove("ARC_SESSION");
    command
}

pub(crate) fn refusal(cmd: &mut AssertCommand) -> String {
    let output = cmd.output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_journal_write_without_harness_or_session_omits_both_keys() {
    let repo = Repo::new();
    without_session(&repo, &repo.root)
        .args(["journal", "log", "absent", "nobody established who"])
        .assert()
        .success();

    let dir = journal_dir(&repo);
    let log = journal_event_log(&dir);
    let event = log.last().unwrap();
    assert_eq!(event["schema"], "journal-events/2", "{event}");
    assert_eq!(event["actor"], "tester", "{event}");
    for key in ["harness", "session"] {
        assert!(event.get(key).is_none(), "{key} is recorded: {event}");
    }
    let raw = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert!(!raw.contains("unknown"), "{raw}");

    let streamed = stdout(without_session(&repo, &repo.root).args(["journal", "events"]));
    let read: serde_json::Value = serde_json::from_str(streamed.lines().last().unwrap()).unwrap();
    assert_eq!(read["message"], "nobody established who", "{read}");
    assert!(read.get("harness").is_none() && read.get("session").is_none());
}

#[test]
fn a_declared_harness_and_session_are_still_recorded() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "declared", "somebody established who"])
        .assert()
        .success();
    let log = journal_event_log(&journal_dir(&repo));
    let event = log.last().unwrap();
    assert_eq!(event["schema"], "journal-events/2", "{event}");
    assert_eq!(event["harness"], "test", "{event}");
    assert_eq!(event["session"], "session-a", "{event}");
}

/// The recorded line of an event an earlier arc wrote, field order included.
const LEGACY_LOG: &str = r#"{"schema":"journal-events/1","ts":"2026-01-01T00:00:00Z","harness":"unknown","session":"unknown","topic":"legacy","event":"log","message":"recorded before absence was explicit"}"#;

/// A `journal-events/1` event is relayed as recorded, so the line a peer or an
/// older reader holds is the line arc streams, never a reshaped copy.
#[test]
fn a_version_one_event_naming_unknown_is_relayed_unchanged() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "legacy", "written by an older arc"])
        .assert()
        .success();
    let events = journal_dir(&repo).join("events.jsonl");
    let mut text = fs::read_to_string(&events).unwrap();
    text.push_str(LEGACY_LOG);
    text.push('\n');
    fs::write(&events, text).unwrap();

    let streamed = stdout(repo.arc(&repo.root).args(["journal", "events"]));
    assert!(
        streamed.lines().any(|line| line == LEGACY_LOG),
        "the recorded line is not relayed: {streamed}"
    );
}

/// What the event records as `unknown` is absent to every reader.
#[test]
fn a_version_one_event_naming_unknown_reads_as_absent() {
    let repo = Repo::new();
    let (dir, file) = journal_artifact(&repo, "legacy-note", "note", "# Legacy\n");
    let events = dir.join("events.jsonl");
    let rewritten: Vec<String> = fs::read_to_string(&events)
        .unwrap()
        .lines()
        .map(|line| {
            let mut event: serde_json::Value = serde_json::from_str(line).unwrap();
            if event["file"] == file {
                event["schema"] = "journal-events/1".into();
                event["harness"] = "unknown".into();
                event["session"] = "unknown".into();
            }
            event.to_string()
        })
        .collect();
    fs::write(&events, rewritten.join("\n") + "\n").unwrap();

    let shown: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["journal", "show", &file, "--json"]),
    ))
    .unwrap();
    assert_eq!(shown["recorded_by"]["actor"], "tester", "{shown}");
    for key in ["harness", "session"] {
        assert!(shown["recorded_by"][key].is_null(), "{key}: {shown}");
    }
}

#[test]
fn a_claim_refuses_each_half_that_is_absent_with_its_own_message() {
    let repo = Repo::new();
    let (_, file) = journal_artifact(&repo, "claimed", "todo", "# Queued\n");

    let stderr = refusal(
        repo.arc(&repo.root)
            .env_remove("ARC_ACTOR")
            .args(["claim", &file]),
    );
    assert!(
        stderr.contains("claiming an artifact requires a declared actor (ARC_ACTOR or --actor)"),
        "{stderr}"
    );
    let stderr = refusal(
        repo.arc(&repo.root)
            .env_remove("ARC_HARNESS")
            .args(["claim", &file]),
    );
    assert!(
        stderr.contains(
            "claiming an artifact requires a declared harness (ARC_HARNESS or --harness)"
        ),
        "{stderr}"
    );
    let stderr = refusal(
        repo.arc(&repo.root)
            .env_remove("ARC_SESSION")
            .args(["claim", &file]),
    );
    assert!(
        stderr.contains(
            "claiming an artifact requires a declared session (ARC_SESSION or --session)"
        ),
        "{stderr}"
    );
    repo.arc(&repo.root)
        .args(["claim", &file])
        .assert()
        .success();
}

#[test]
fn a_lane_refuses_an_absent_session_or_harness() {
    let repo = Repo::new();
    let stderr = refusal(
        repo.arc(&repo.root)
            .env_remove("ARC_SESSION")
            .args(["journal", "lane", "open", "work-a"]),
    );
    assert!(stderr.contains("requires a session identity"), "{stderr}");
    let stderr = refusal(
        repo.arc(&repo.root)
            .env_remove("ARC_HARNESS")
            .args(["journal", "lane", "open", "work-a"]),
    );
    assert!(stderr.contains("requires a harness identity"), "{stderr}");
}

fn outbox_files(repo: &Repo) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(repo.root.join(".arc/outbox"))
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// A spooled write keeps the identity it was made under, absence included,
/// and a spool written by an earlier arc is filed the same way.
#[test]
fn a_spooled_write_without_harness_or_session_is_filed_without_them() {
    let repo = Repo::new();
    for topic in ["parked", "older"] {
        without_session(&repo, &repo.root)
            .args(["journal", "todo", topic, "--body-file", "-", "--spool"])
            .write_stdin("deferred\n")
            .assert()
            .success();
    }
    let spooled = outbox_files(&repo);
    assert_eq!(spooled.len(), 2);
    for path in &spooled {
        let file: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(file["schema"], "arc-journal-spool/2", "{file}");
        for key in ["harness", "session"] {
            assert!(file["event"].get(key).is_none(), "{key}: {file}");
        }
    }
    let mut older: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&spooled[1]).unwrap()).unwrap();
    older["schema"] = "arc-journal-spool/1".into();
    fs::write(&spooled[1], older.to_string()).unwrap();

    let promoted =
        stdout(without_session(&repo, &repo.root).args(["journal", "spool", "--promote"]));
    assert!(promoted.contains("promoted 2, kept 0"), "{promoted}");
    for event in journal_event_log(&journal_dir(&repo)) {
        for key in ["harness", "session"] {
            assert!(event.get(key).is_none(), "{key}: {event}");
        }
        assert_eq!(event["promoted_by"], "tester", "{event}");
    }
}

/// A claim names a holder by actor, harness and session. A record that
/// leaves any of them out names nobody, so it holds nothing.
#[test]
fn a_claim_event_naming_no_holder_claims_nothing() {
    let repo = Repo::new();
    let (dir, file) = journal_artifact(&repo, "ghost", "todo", "# Queued\n");
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let events = dir.join("events.jsonl");
    let mut text = fs::read_to_string(&events).unwrap();
    for (harness, session) in [("unknown", "unknown"), ("test", "unknown")] {
        let claim = serde_json::json!({
            "schema": "journal-events/1", "ts": now, "harness": harness,
            "session": session, "actor": "ghost", "topic": "ghost",
            "event": "claim-set", "file": file, "claim_id": format!("claim-{harness}"),
        });
        text.push_str(&format!("{claim}\n"));
    }
    fs::write(&events, text).unwrap();

    repo.arc(&repo.root)
        .args(["claim", &file])
        .assert()
        .success();
}
