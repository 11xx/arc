use super::common::*;
use predicates::prelude::*;
use std::os::unix::fs::PermissionsExt;

fn begin(repo: &Repo, slug: &str) -> (String, PathBuf) {
    let output = stdout(repo.arc(&repo.root).args(["begin", slug]));
    (
        opened_change_id(&output),
        repo.home.join(".worktrees").join(format!("repo-{slug}")),
    )
}

fn claim_from_dead_session(repo: &Repo, slug: &str) {
    claim_from_session(repo, slug, "dead-harness", "dead-session");
}

fn claim_from_session(repo: &Repo, slug: &str, harness: &str, session: &str) {
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "dead actor")
        .env("ARC_HARNESS", harness)
        .env("ARC_SESSION", session)
        .args(["claim", slug, "--stage-budget", "launch=1s"])
        .assert()
        .success();
}

/// A `PATH` with only `git` on it, so nothing named `tapes` is reachable.
fn path_without_tapes(repo: &Repo) -> PathBuf {
    let without_tapes = repo.home.join("without-tapes-bin");
    fs::create_dir_all(&without_tapes).unwrap();
    let git = std::env::split_paths(&std::env::var_os("PATH").expect("PATH is set"))
        .map(|dir| dir.join("git"))
        .find_map(|path| path.is_file().then(|| fs::canonicalize(path).unwrap()))
        .expect("git must be available on PATH");
    let link = without_tapes.join("git");
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(git, link).unwrap();
    without_tapes
}

#[test]
fn stale_foreign_claim_is_abandoned_and_reports_owner() {
    let repo = Repo::new();
    let (change_id, worktree) = begin(&repo, "stale-rescue");
    claim_from_dead_session(&repo, "stale-rescue");
    age_event(&repo, &change_id, "claim-set", 5);

    repo.arc(&worktree)
        .arg("rescue")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Owner: dead actor via dead-harness/dead-session",
        ))
        .stdout(predicate::str::contains("State: stale"))
        .stdout(predicate::str::contains("Abandoned: yes"));
}

#[test]
fn fresh_foreign_claim_is_not_abandoned() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "fresh-rescue");
    claim_from_dead_session(&repo, "fresh-rescue");

    repo.arc(&worktree)
        .arg("rescue")
        .assert()
        .success()
        .stdout(predicate::str::contains("State: active"))
        .stdout(predicate::str::contains("Abandoned: no"));
}

#[test]
fn rescue_reports_dirty_and_clean_worktrees() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "dirty-rescue");

    repo.arc(&worktree)
        .arg("rescue")
        .assert()
        .success()
        .stdout(predicate::str::contains("Uncommitted edits: absent"));
    fs::write(worktree.join("uncommitted.txt"), "work\n").unwrap();
    repo.arc(&worktree)
        .arg("rescue")
        .assert()
        .success()
        .stdout(predicate::str::contains("Uncommitted edits: present"));
}

#[test]
fn rescue_reports_missing_patchset_without_head_drift() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "no-patchset-rescue");

    repo.arc(&worktree)
        .arg("rescue")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Branch head: no patchset recorded",
        ))
        .stdout(predicate::str::contains("moved past").not());
}

#[test]
fn take_transfers_a_stale_claim_and_records_displaced_owner() {
    let repo = Repo::new();
    let (change_id, worktree) = begin(&repo, "take-rescue");
    claim_from_dead_session(&repo, "take-rescue");
    age_event(&repo, &change_id, "claim-set", 5);

    repo.arc(&worktree)
        .args(["rescue", "--take"])
        .assert()
        .success();
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&worktree).arg("status"))).unwrap();
    assert_eq!(status["claim"]["owner"]["session"], "session-a");
    let claims = stdout(repo.arc(&worktree).args([
        "events",
        "--change",
        "take-rescue",
        "--type",
        "claim-set",
    ]));
    let takeover: serde_json::Value = serde_json::from_str(claims.lines().last().unwrap()).unwrap();
    assert_eq!(takeover["displaced"]["actor"], "dead actor");
    assert_eq!(takeover["displaced"]["stage"], "launch");
}

#[test]
fn take_claims_an_expired_foreign_claim_and_records_displacement() {
    let repo = Repo::new();
    let (change_id, worktree) = begin(&repo, "expired-rescue");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "dead actor")
        .env("ARC_HARNESS", "dead-harness")
        .env("ARC_SESSION", "dead-session")
        .args(["claim", "expired-rescue", "--ttl", "1s"])
        .assert()
        .success();
    age_event(&repo, &change_id, "claim-set", 5);

    repo.arc(&worktree)
        .args(["rescue", "--take"])
        .assert()
        .success();
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&worktree).arg("status"))).unwrap();
    assert_eq!(status["claim"]["owner"]["session"], "session-a");
    let claims = stdout(repo.arc(&worktree).args([
        "events",
        "--change",
        "expired-rescue",
        "--type",
        "claim-set",
    ]));
    let takeover: serde_json::Value = serde_json::from_str(claims.lines().last().unwrap()).unwrap();
    assert_eq!(takeover["displaced"]["actor"], "dead actor");
    assert_eq!(takeover["displaced"]["harness"], "dead-harness");
    assert_eq!(takeover["displaced"]["session"], "dead-session");
    assert_eq!(takeover["displaced"]["stage"], "launch");
    assert!(takeover["displaced"]["claim_id"].is_string());
}

#[test]
fn take_refuses_a_fresh_claim_without_changing_owner() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "refuse-rescue");
    claim_from_dead_session(&repo, "refuse-rescue");

    repo.arc(&worktree)
        .args(["rescue", "--take"])
        .assert()
        .code(8)
        .stderr(predicate::str::contains("not yet stale"));
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&worktree).arg("status"))).unwrap();
    assert_eq!(status["claim"]["owner"]["session"], "dead-session");
}

#[test]
fn rescue_json_uses_versioned_schema() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "json-rescue");
    let output = stdout(repo.arc(&worktree).args(["rescue", "--json"]));
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();

    assert_eq!(value["schema"], "arc-rescue/4");
    assert!(value.get("transcript").is_none());
}

/// One recording, read through the linked tapes library. Nothing named
/// `tapes` is on `PATH`, and the answer does not depend on whether it is:
/// location and shape knowledge travel in the binary's own dependencies.
#[test]
fn transcript_reads_a_recording_through_the_linked_library() {
    let repo = Repo::new();
    let session = "library-dead-session";
    let (change_id, worktree) = begin(&repo, "library-transcript");
    claim_from_session(&repo, "library-transcript", "claude", session);
    let project = repo.home.join(".claude/projects/-test-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{session}.jsonl")),
        concat!(
            "{\"type\":\"user\",\"timestamp\":\"1\",\"message\":{\"role\":\"user\",\"content\":\"the question\"}}\n",
            "{\"type\":\"system\",\"timestamp\":\"2\",\"message\":{\"role\":\"system\",\"content\":\"kept out of the operator view\"}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"3\",\"message\":{\"role\":\"assistant\",\"content\":\"the answer\"}}\n",
        ),
    )
    .unwrap();

    let without_tapes = path_without_tapes(&repo);
    let read = |path: Option<&Path>| -> serde_json::Value {
        let mut command = repo.arc(&worktree);
        if let Some(path) = path {
            command.env("PATH", path);
        }
        let output = stdout(command.args(["rescue", change_id.as_str(), "--transcript", "--json"]));
        serde_json::from_str(&output).unwrap()
    };

    let inherited = read(None);
    let restricted = read(Some(&without_tapes));
    assert_eq!(inherited["transcript"]["source"], "tapes");
    assert_eq!(inherited["transcript"]["count"], 2);
    assert_eq!(inherited["transcript"]["turns"][0]["text"], "the question");
    assert_eq!(inherited["transcript"]["turns"][1]["text"], "the answer");
    assert_eq!(inherited, restricted);

    repo.arc(&worktree)
        .env("PATH", &without_tapes)
        .args(["rescue", change_id.as_str(), "--transcript"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Source: tapes"))
        .stdout(predicate::str::contains("Turns: 2"));
}

#[test]
fn claude_transcript_returns_newest_window_oldest_first() {
    let repo = Repo::new();
    let session = "claude-dead-session";
    let (_, worktree) = begin(&repo, "claude-transcript");
    claim_from_session(&repo, "claude-transcript", "claude", session);
    let project = repo.home.join(".claude/projects/-test-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{session}.jsonl")),
        concat!(
            "{\"type\":\"user\",\"timestamp\":\"1\",\"message\":{\"role\":\"user\",\"content\":\"first\"}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"intermediate\"}]}}\n",
            "{\"type\":\"user\",\"timestamp\":\"3\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"second\"}]}}\n",
            "{\"type\":\"user\",\"timestamp\":\"4\",\"message\":{\"role\":\"user\",\"content\":\"third\"}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"5\",\"message\":{\"role\":\"assistant\",\"content\":\"final answer\"}}\n",
        ),
    )
    .unwrap();

    let output =
        stdout(
            repo.arc(&worktree)
                .args(["rescue", "--transcript", "--tail", "3", "--json"]),
        );
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    let turns = value["transcript"]["turns"].as_array().unwrap();
    assert_eq!(value["transcript"]["count"], 3);
    assert_eq!(turns[0]["text"], "second");
    assert_eq!(turns[1]["text"], "third");
    assert_eq!(turns[2]["text"], "final answer");
}

#[test]
fn codex_rollout_yields_operator_turns() {
    let repo = Repo::new();
    let session = "019f7890-5c01-7ec1-9240-2eba1613e5d2";
    let (_, worktree) = begin(&repo, "codex-transcript");
    claim_from_session(&repo, "codex-transcript", "codex", session);
    let codex_home = repo.home.join("codex-state");
    let day = codex_home.join("sessions/2026/07/24");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join(format!("rollout-2026-07-24T00-00-00-{session}.jsonl")),
        concat!(
            "{\"type\":\"session_meta\",\"timestamp\":\"2026-07-24T00:00:00Z\",",
            "\"payload\":{\"id\":\"019f7890-5c01-7ec1-9240-2eba1613e5d2\"}}\n",
            "{\"type\":\"response_item\",\"timestamp\":\"2026-07-24T00:00:01Z\",",
            "\"payload\":{\"type\":\"message\",\"role\":\"user\",",
            "\"content\":[{\"type\":\"input_text\",\"text\":\"do the work\"}]}}\n",
            "{\"type\":\"response_item\",\"timestamp\":\"2026-07-24T00:00:02Z\",",
            "\"payload\":{\"type\":\"message\",\"role\":\"assistant\",",
            "\"content\":[{\"type\":\"output_text\",\"text\":\"work done\"}]}}\n",
        ),
    )
    .unwrap();

    let output = stdout(repo.arc(&worktree).env("CODEX_HOME", &codex_home).args([
        "rescue",
        "--transcript",
        "--json",
    ]));
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["transcript"]["source"], "tapes");
    assert_eq!(value["transcript"]["turns"][0]["text"], "do the work");
    assert_eq!(value["transcript"]["turns"][1]["text"], "work done");
}

#[test]
fn codex_rescue_requires_the_canonical_session_id() {
    let repo = Repo::new();
    let full = "019f7890-5c01-7ec1-9240-2eba1613e5d2";
    codex_recording(&repo, full, "gpt-fixture");
    let codex_home = repo.home.join("codex-state");
    let (_, exact_worktree) = begin(&repo, "exact-codex-read");
    claim_from_session(&repo, "exact-codex-read", "codex", full);
    let exact: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&exact_worktree)
            .env("CODEX_HOME", &codex_home)
            .args(["rescue", "--transcript", "--json"]),
    ))
    .unwrap();
    assert_eq!(exact["transcript"]["count"], 2, "{exact}");
    assert_eq!(
        exact["transcript"]["turns"][0]["text"],
        "private full-id prompt"
    );

    let (_, prefix_worktree) = begin(&repo, "prefix-codex-read");
    claim_from_session(&repo, "prefix-codex-read", "codex", "019f7890");
    let prefix: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&prefix_worktree)
            .env("CODEX_HOME", &codex_home)
            .args(["rescue", "--transcript", "--json"]),
    ))
    .unwrap();
    assert_eq!(prefix["transcript"]["count"], 0, "{prefix}");
    assert_eq!(prefix["transcript"]["cause"], "no-recording", "{prefix}");
    assert!(prefix["transcript"]["turns"].as_array().unwrap().is_empty());
    assert!(
        !prefix.to_string().contains("private full-id prompt"),
        "{prefix}"
    );
}

#[test]
fn ambiguous_codex_rescue_names_the_unreadable_lookup() {
    let repo = Repo::new();
    codex_recording(&repo, "019f7890-5c01-7ec1-9240-2eba1613e5d2", "first-model");
    codex_recording(
        &repo,
        "019f7890-1234-4444-8888-111111111111",
        "second-model",
    );
    let (_, worktree) = begin(&repo, "ambiguous-codex-read");
    claim_from_session(&repo, "ambiguous-codex-read", "codex", "019f7890");
    let codex_home = repo.home.join("codex-state");
    let output = stdout(repo.arc(&worktree).env("CODEX_HOME", &codex_home).args([
        "rescue",
        "--transcript",
        "--json",
    ]));
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["transcript"]["count"], 0, "{value}");
    assert_eq!(value["transcript"]["cause"], "ambiguous", "{value}");
    assert!(value["transcript"]["reason"]
        .as_str()
        .unwrap()
        .contains("ambiguous"));
    assert!(!output.contains("private full-id prompt"));
    repo.arc(&worktree)
        .env("CODEX_HOME", &codex_home)
        .args(["rescue", "--transcript"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ambiguous"));
}

#[test]
fn unreadable_codex_rescue_has_a_cause_and_reason() {
    let repo = Repo::new();
    let session = "019f7890-5c01-7ec1-9240-2eba1613e5d2";
    let path = codex_recording(&repo, session, "gpt-fixture");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let (_, worktree) = begin(&repo, "unreadable-codex-read");
    claim_from_session(&repo, "unreadable-codex-read", "codex", session);
    let codex_home = repo.home.join("codex-state");

    let output = stdout(repo.arc(&worktree).env("CODEX_HOME", &codex_home).args([
        "rescue",
        "--transcript",
        "--json",
    ]));
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["transcript"]["count"], 0, "{value}");
    assert_eq!(value["transcript"]["cause"], "unreadable", "{value}");
    let reason = value["transcript"]["reason"].as_str().unwrap();
    assert!(!reason.is_empty());
    assert!(!output.contains("private full-id prompt"));
    repo.arc(&worktree)
        .env("CODEX_HOME", &codex_home)
        .args(["rescue", "--transcript"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Unreadable"))
        .stdout(predicate::str::contains(reason));
}

#[test]
fn missing_transcript_is_reported_without_failure() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "missing-transcript");
    claim_from_session(&repo, "missing-transcript", "claude", "missing-session");

    repo.arc(&worktree)
        .env("PATH", path_without_tapes(&repo))
        .args(["rescue", "--transcript"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Unavailable: no recording of the claimed session in its harness's store",
        ));

    let output = stdout(
        repo.arc(&worktree)
            .env("PATH", path_without_tapes(&repo))
            .args(["rescue", "--transcript", "--json"]),
    );
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["transcript"]["count"], 0);
    assert_eq!(value["transcript"]["cause"], "no-recording");
}

#[test]
fn unknown_claim_identity_is_reported_without_failure() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "unknown-transcript");
    claim_from_session(
        &repo,
        "unknown-transcript",
        "unknown-harness",
        "unknown-session",
    );

    repo.arc(&worktree)
        .args(["rescue", "--transcript"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Unavailable: claim harness/session is unknown",
        ));

    let output = stdout(
        repo.arc(&worktree)
            .args(["rescue", "--transcript", "--json"]),
    );
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["transcript"]["count"], 0);
    assert_eq!(value["transcript"]["cause"], "unknown-identity");
}

#[test]
fn malformed_transcript_lines_are_skipped() {
    let repo = Repo::new();
    let session = "malformed-session";
    let (_, worktree) = begin(&repo, "malformed-transcript");
    claim_from_session(&repo, "malformed-transcript", "claude", session);
    let project = repo.home.join(".claude/projects/-test-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{session}.jsonl")),
        concat!(
            "not json\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"kept\"}}\n",
            "{\"broken\":\n",
        ),
    )
    .unwrap();

    let output = stdout(
        repo.arc(&worktree)
            .args(["rescue", "--transcript", "--json"]),
    );
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["transcript"]["count"], 1);
    assert_eq!(value["transcript"]["turns"][0]["text"], "kept");
}

/// The operator projection keeps every operator turn and the assistant's last
/// word, and `--tail` counts turns of that projection rather than records of
/// the recording.
#[test]
fn transcript_projects_operator_turns_before_tail() {
    let repo = Repo::new();
    let session = "agree-dead-session";
    let (change_id, worktree) = begin(&repo, "agreement-transcript");
    claim_from_session(&repo, "agreement-transcript", "claude", session);
    let project = repo.home.join(".claude/projects/-test-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{session}.jsonl")),
        concat!(
            "{\"type\":\"user\",\"timestamp\":\"1\",\"message\":{\"role\":\"user\",\"content\":\"q1\"}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2\",\"message\":{\"role\":\"assistant\",\"content\":\"a1\"}}\n",
            "{\"type\":\"user\",\"timestamp\":\"3\",\"message\":{\"role\":\"user\",\"content\":\"q2\"}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"4\",\"message\":{\"role\":\"assistant\",\"content\":\"a2\"}}\n",
            "{\"type\":\"user\",\"timestamp\":\"5\",\"message\":{\"role\":\"user\",\"content\":\"q3\"}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"6\",\"message\":{\"role\":\"assistant\",\"content\":\"a3\"}}\n",
        ),
    )
    .unwrap();
    let without_tapes = path_without_tapes(&repo);
    let read = |path: Option<&Path>, tail: Option<&str>| -> serde_json::Value {
        let mut args = vec!["rescue", change_id.as_str(), "--transcript", "--json"];
        if let Some(tail) = tail {
            args.extend(["--tail", tail]);
        }
        let mut command = repo.arc(&worktree);
        if let Some(path) = path {
            command.env("PATH", path);
        }
        let output = stdout(command.args(&args));
        serde_json::from_str(&output).unwrap()
    };

    let whole = read(None, None);
    assert_eq!(whole["transcript"]["source"], "tapes");
    assert_eq!(whole["transcript"]["count"], 4);
    let texts: Vec<&str> = whole["transcript"]["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|turn| turn["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["q1", "q2", "q3", "a3"]);

    let tail = read(None, Some("3"));
    assert_eq!(tail["transcript"]["count"], 3);
    let tail_texts: Vec<&str> = tail["transcript"]["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|turn| turn["text"].as_str().unwrap())
        .collect();
    assert_eq!(tail_texts, ["q2", "q3", "a3"]);
    assert_eq!(read(Some(&without_tapes), None), whole);
    assert_eq!(read(Some(&without_tapes), Some("3")), tail);
}

/// A recording whose newest operator turn lies before the read window reports
/// the turns the window holds, the bound the answer rests on, and a cause a
/// machine consumer can read.
#[test]
fn transcript_reports_text_outside_the_read_window() {
    let repo = Repo::new();
    let session = "bound-dead-session";
    let (_, worktree) = begin(&repo, "bound-transcript");
    claim_from_session(&repo, "bound-transcript", "claude", session);
    let project = repo.home.join(".claude/projects/-test-repo");
    fs::create_dir_all(&project).unwrap();

    let mut recording = String::from(
        "{\"type\":\"user\",\"timestamp\":\"1\",\"message\":{\"role\":\"user\",\"content\":\"the newest operator turn\"}}\n",
    );
    let filler = format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"0\",\"message\":{{\"role\":\"tool\",\"content\":\"{}\"}}}}\n",
        "x".repeat(512)
    );
    while recording.len() < 5 * 1024 * 1024 {
        recording.push_str(&filler);
    }
    fs::write(project.join(format!("{session}.jsonl")), recording).unwrap();

    let without_tapes = path_without_tapes(&repo);
    let output = stdout(repo.arc(&worktree).env("PATH", &without_tapes).args([
        "rescue",
        "--transcript",
        "--json",
    ]));
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["transcript"]["count"], 0);
    assert_eq!(value["transcript"]["cause"], "outside-read-bound");
    assert_eq!(value["transcript"]["bound"]["kind"], "file-tail");
    assert_eq!(
        value["transcript"]["bound"]["window_bytes"],
        4 * 1024 * 1024
    );
    assert!(
        value["transcript"]["bound"]["skipped_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(value["transcript"]["turns"].as_array().unwrap().is_empty());

    repo.arc(&worktree)
        .env("PATH", &without_tapes)
        .args(["rescue", "--transcript"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Bound:"))
        .stdout(predicate::str::contains("Outside read bound:"))
        .stdout(predicate::str::contains("earlier bytes were not read"));
}

/// A recording that fits the read window produces no bound-related message.
#[test]
fn short_recording_reports_no_bound() {
    let repo = Repo::new();
    let session = "short-dead-session";
    let (_, worktree) = begin(&repo, "short-transcript");
    claim_from_session(&repo, "short-transcript", "claude", session);
    let project = repo.home.join(".claude/projects/-test-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{session}.jsonl")),
        "{\"type\":\"user\",\"timestamp\":\"1\",\"message\":{\"role\":\"user\",\"content\":\"only turn\"}}\n",
    )
    .unwrap();

    repo.arc(&worktree)
        .env("PATH", path_without_tapes(&repo))
        .args(["rescue", "--transcript"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Turns: 1"))
        .stdout(predicate::str::contains("Bound:").not())
        .stdout(predicate::str::contains("Outside read bound:").not());
}

/// An artifact keeps checkpoints rather than a claimed session, so
/// `--transcript` has nothing to read there. The refusal says so in one
/// readable line.
#[test]
fn rescue_refuses_a_transcript_of_an_artifact_in_one_line() {
    let repo = Repo::new();
    let (_, file) = journal_artifact(&repo, "no-transcript", "todo", "# Queued\n");

    let out = repo
        .arc(&repo.root)
        .args(["rescue", &file, "--transcript"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(stderr.contains("its checkpoints"), "{stderr}");
    assert!(
        !stderr.contains("   "),
        "refusal must not carry a run of spaces: {stderr}"
    );
}
