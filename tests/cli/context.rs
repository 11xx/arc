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
