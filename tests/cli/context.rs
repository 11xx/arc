use super::common::*;
use predicates::prelude::*;

#[test]
fn no_arg_snapshot_stage_and_show_work_inside_change_worktree() {
    let repo = Repo::new();
    let output = stdout(repo.arc(&repo.root).args(["begin", "contextual"]));
    let change_id = opened_change_id(&output);
    let worktree = repo.home.join(".worktrees/repo-contextual");

    repo.arc(&worktree).arg("claim").assert().success();
    repo.arc(&worktree)
        .args(["stage", "started"])
        .assert()
        .success();
    repo.commit(&worktree, "context.txt", "context\n", "test: add context");
    repo.arc(&worktree).arg("snapshot").assert().success();
    repo.arc(&worktree)
        .arg("show")
        .assert()
        .success()
        .stdout(predicate::str::contains(change_id));
}

#[test]
fn no_arg_command_outside_change_worktree_lists_candidates_and_demands_explicit_change() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "contextual"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .arg("show")
        .assert()
        .failure()
        .stderr(predicate::str::contains("candidates: (none)"))
        .stderr(predicate::str::contains("pass CHANGE explicitly"));
}

#[test]
fn explicit_change_still_wins_over_worktree_context() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "first-context"])
        .assert()
        .success();
    let output = stdout(repo.arc(&repo.root).args(["begin", "second-context"]));
    let second_id = opened_change_id(&output);
    let first_worktree = repo.home.join(".worktrees/repo-first-context");

    repo.arc(&first_worktree)
        .args(["show", "second-context"])
        .assert()
        .success()
        .stdout(predicate::str::contains(second_id));
}

#[test]
fn env_detects_codex_thread_and_prints_exports() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .arg("env")
        .env_remove("CLAUDE_SESSION_ID")
        .env("CODEX_THREAD_ID", "thread-123")
        .env_remove("OPENCODE_SESSION")
        .assert()
        .success()
        .stdout("export ARC_HARNESS='codex' ARC_SESSION='thread-123'\n");
}

#[test]
fn resume_json_uses_arc_resume_schema() {
    let repo = Repo::new();
    let output = stdout(repo.arc(&repo.root).args(["begin", "contextual"]));
    let change_id = opened_change_id(&output);
    let worktree = repo.home.join(".worktrees/repo-contextual");
    let output = stdout(repo.arc(&worktree).args(["resume", "--json"]));
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();

    assert_eq!(value["schema"], "arc-resume/1");
    assert_eq!(value["status"]["change_id"], change_id);
}

#[test]
fn prompt_is_empty_outside_change_worktree() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "contextual"])
        .assert()
        .success();

    repo.arc(&repo.home)
        .arg("prompt")
        .assert()
        .success()
        .stdout("");
}

#[test]
fn env_detects_claude_model_from_transcript() {
    let repo = Repo::new();
    let session = "11111111-2222-3333-4444-555555555555";
    let project = repo.home.join(".claude/projects/-home-lobo");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{session}.jsonl")),
        concat!(
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\"}}\n",
            "not a json line\n",
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-fable-5\"}}\n",
        ),
    )
    .unwrap();

    // The newest assistant model wins; malformed lines are skipped.
    repo.arc(&repo.root)
        .arg("env")
        .env("CLAUDE_SESSION_ID", session)
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .assert()
        .success()
        .stdout(format!(
            "export ARC_HARNESS='claude' ARC_SESSION='{session}' ARC_MODEL='claude-fable-5'\n"
        ));
}

#[test]
fn env_detects_codex_model_and_effort_from_rollout() {
    let repo = Repo::new();
    let session = "019f7890-5c01-7ec1-9240-2eba1613e5d2";
    let day = repo.home.join(".codex/sessions/2026/07/20");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join(format!("rollout-2026-07-20T00-00-00-{session}.jsonl")),
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"x\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.5\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-sol\",",
            "\"reasoning_effort\":\"high\"}}\n",
        ),
    )
    .unwrap();

    // Last turn_context wins; model and effort combine as model#effort.
    repo.arc(&repo.root)
        .arg("env")
        .env_remove("CLAUDE_SESSION_ID")
        .env("CODEX_THREAD_ID", session)
        .env_remove("OPENCODE_SESSION")
        .assert()
        .success()
        .stdout(format!(
            "export ARC_HARNESS='codex' ARC_SESSION='{session}' ARC_MODEL='gpt-5.6-sol#high'\n"
        ));
}

#[test]
fn env_omits_model_when_no_session_store_matches() {
    let repo = Repo::new();
    // A codex session id with no rollout file: harness/session exports only.
    repo.arc(&repo.root)
        .arg("env")
        .env_remove("CLAUDE_SESSION_ID")
        .env("CODEX_THREAD_ID", "no-such-thread")
        .env_remove("OPENCODE_SESSION")
        .assert()
        .success()
        .stdout("export ARC_HARNESS='codex' ARC_SESSION='no-such-thread'\n");

    // Nothing detected at all: the fallback comment names ARC_MODEL too.
    repo.arc(&repo.root)
        .arg("env")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .assert()
        .failure()
        .stdout(predicate::str::contains("ARC_MODEL=<model[#effort]>"));
}
