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

/// A `PATH` with a stub `tapes` first that prints `document`, so one recording
/// can be read through both readers without a real tapes installation.
fn tapes_path(repo: &Repo, document: &str) -> String {
    let document_path = repo.home.join("tapes-document.json");
    fs::write(&document_path, document).unwrap();
    let tapes_bin = repo.home.join("tapes-bin");
    fs::create_dir_all(&tapes_bin).unwrap();
    let tapes = tapes_bin.join("tapes");
    fs::write(
        &tapes,
        format!("#!/bin/sh\ncat '{}'\n", document_path.display()),
    )
    .unwrap();
    fs::set_permissions(&tapes, fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        tapes_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// A `PATH` with only `git` on it, so no `tapes` reader is reachable.
fn native_only_path(repo: &Repo) -> PathBuf {
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

    assert_eq!(value["schema"], "arc-rescue/3");
    assert!(value.get("transcript").is_none());
}

#[test]
fn rescue_transcript_prefers_tapes() {
    let repo = Repo::new();
    let session = "opencode-dead-session";
    let (change_id, worktree) = begin(&repo, "tapes-transcript");
    claim_from_session(&repo, "tapes-transcript", "opencode", session);

    let path_with_tapes = tapes_path(
        &repo,
        concat!(
            "{\"schema\":\"tapes-session/1\",\"session\":{},\"turns\":[",
            "{\"role\":\"user\",\"text\":\"fake question\",\"ts\":\"1\"},",
            "{\"role\":\"system\",\"text\":\"ignored\",\"ts\":\"2\"},",
            "{\"role\":\"assistant\",\"text\":\"fake answer\",\"ts\":\"3\"}",
            "],\"truncated\":false}",
        ),
    );

    let output = stdout(repo.arc(&worktree).env("PATH", path_with_tapes).args([
        "rescue",
        change_id.as_str(),
        "--transcript",
        "--json",
    ]));
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["transcript"]["source"], "tapes");
    assert_eq!(value["transcript"]["count"], 2);
    assert_eq!(value["transcript"]["turns"][0]["text"], "fake question");
    assert_eq!(value["transcript"]["turns"][1]["text"], "fake answer");

    let path_without_tapes = native_only_path(&repo);

    let output = repo
        .arc(&worktree)
        .env("PATH", &path_without_tapes)
        .args(["rescue", change_id.as_str(), "--transcript", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["transcript"]["count"], 0);
    assert!(value["transcript"]["turns"]
        .as_array()
        .is_some_and(Vec::is_empty));

    repo.arc(&worktree)
        .env("PATH", &path_without_tapes)
        .args(["rescue", change_id.as_str(), "--transcript"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Unavailable: no transcript for the claimed session in tapes or on disk",
        ));
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
    let session = "codex-dead-session";
    let (_, worktree) = begin(&repo, "codex-transcript");
    claim_from_session(&repo, "codex-transcript", "codex", session);
    let codex_home = repo.home.join("codex-state");
    let day = codex_home.join("sessions/2026/07/24");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join(format!("rollout-{session}.jsonl")),
        concat!(
            "{\"type\":\"response_item\",\"timestamp\":\"1\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"do the work\"}]}}\n",
            "{\"type\":\"response_item\",\"timestamp\":\"2\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"work done\"}]}}\n",
        ),
    )
    .unwrap();

    let output = stdout(repo.arc(&worktree).env("CODEX_HOME", &codex_home).args([
        "rescue",
        "--transcript",
        "--json",
    ]));
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["transcript"]["turns"][0]["text"], "do the work");
    assert_eq!(value["transcript"]["turns"][1]["text"], "work done");
}

#[test]
fn missing_transcript_is_reported_without_failure() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "missing-transcript");
    claim_from_session(&repo, "missing-transcript", "claude", "missing-session");

    repo.arc(&worktree)
        .env("PATH", native_only_path(&repo))
        .args(["rescue", "--transcript"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Unavailable: no transcript for the claimed session in tapes or on disk",
        ));

    let output = stdout(
        repo.arc(&worktree)
            .env("PATH", native_only_path(&repo))
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

/// One recording answers the same through a stub `tapes` and through arc's
/// own reader, and `--tail` counts the same projection on both paths.
#[test]
fn transcript_readers_agree_on_one_recording() {
    let repo = Repo::new();
    let session = "agree-dead-session";
    let (_, worktree) = begin(&repo, "agreement-transcript");
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
    let with_tapes = tapes_path(
        &repo,
        concat!(
            "{\"schema\":\"tapes-session/1\",\"session\":{},\"turns\":[",
            "{\"role\":\"user\",\"text\":\"q1\",\"ts\":\"1\"},",
            "{\"role\":\"assistant\",\"text\":\"a1\",\"ts\":\"2\"},",
            "{\"role\":\"user\",\"text\":\"q2\",\"ts\":\"3\"},",
            "{\"role\":\"assistant\",\"text\":\"a2\",\"ts\":\"4\"},",
            "{\"role\":\"user\",\"text\":\"q3\",\"ts\":\"5\"},",
            "{\"role\":\"assistant\",\"text\":\"a3\",\"ts\":\"6\"}",
            "],\"truncated\":false}",
        ),
    );
    let without_tapes = native_only_path(&repo);
    let read = |path: &Path, tail: Option<&str>| -> serde_json::Value {
        let mut args = vec!["rescue", "--transcript", "--json"];
        if let Some(tail) = tail {
            args.extend(["--tail", tail]);
        }
        let output = stdout(repo.arc(&worktree).env("PATH", path).args(&args));
        serde_json::from_str(&output).unwrap()
    };

    let through_tapes = read(Path::new(&with_tapes), None);
    let through_native = read(&without_tapes, None);
    assert_eq!(through_tapes["transcript"]["source"], "tapes");
    assert_eq!(through_native["transcript"]["source"], "native");
    assert_eq!(through_tapes["transcript"]["count"], 4);
    assert_eq!(
        through_tapes["transcript"]["turns"],
        through_native["transcript"]["turns"]
    );
    let texts: Vec<&str> = through_native["transcript"]["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|turn| turn["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["q1", "q2", "q3", "a3"]);

    let tapes_tail = read(Path::new(&with_tapes), Some("3"));
    let native_tail = read(&without_tapes, Some("3"));
    assert_eq!(tapes_tail["transcript"]["count"], 3);
    assert_eq!(
        tapes_tail["transcript"]["turns"],
        native_tail["transcript"]["turns"]
    );

    repo.arc(&worktree)
        .env("PATH", Path::new(&with_tapes))
        .args(["rescue", "--transcript"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Source: tapes (native: not consulted)",
        ));
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

    let native_only = native_only_path(&repo);
    let output = stdout(repo.arc(&worktree).env("PATH", &native_only).args([
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
        .env("PATH", &native_only)
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
        .env("PATH", native_only_path(&repo))
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
