use super::common::*;
use predicates::prelude::*;
use std::os::unix::fs::PermissionsExt;

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
        .env_remove("PI_SESSION_ID")
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
        .env_remove("PI_SESSION_ID")
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
    let codex_home = repo.home.join("custom-codex-state");
    let day = codex_home.join("sessions/2026/07/20");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join(format!("rollout-2026-07-20T00-00-00-{session}.jsonl")),
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"x\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.5\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-sol\",",
            "\"effort\":\"high\"}}\n",
        ),
    )
    .unwrap();

    // Last turn_context wins; model and effort combine as model#effort.
    repo.arc(&repo.root)
        .arg("env")
        .env_remove("CLAUDE_SESSION_ID")
        .env("CODEX_THREAD_ID", session)
        .env("CODEX_HOME", &codex_home)
        .env_remove("OPENCODE_SESSION")
        .env_remove("PI_SESSION_ID")
        .assert()
        .success()
        .stdout(format!(
            "export ARC_HARNESS='codex' ARC_SESSION='{session}' ARC_MODEL='gpt-5.6-sol#high'\n"
        ));
}

#[test]
fn env_detects_opencode_model_and_variant_from_session_store() {
    let repo = Repo::new();
    let session = "ses_test123";
    let data_home = repo.home.join("data");
    let store = data_home.join("opencode/opencode-next.db");
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    fs::write(&store, "test placeholder").unwrap();

    // Detection deliberately shells only to sqlite3, so the test supplies a
    // deterministic reader without depending on a host SQLite installation.
    let bin = repo.home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let sqlite = bin.join("sqlite3");
    fs::write(
        &sqlite,
        "#!/bin/sh\nprintf '%s\\n' '{\"id\":\"kimi-k3\",\"providerID\":\"opencode-go\",\"variant\":\"max\"}'\n",
    )
    .unwrap();
    fs::set_permissions(&sqlite, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    repo.arc(&repo.root)
        .arg("env")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env("OPENCODE_SESSION", session)
        .env_remove("PI_SESSION_ID")
        .env("XDG_DATA_HOME", &data_home)
        .env("PATH", path)
        .assert()
        .success()
        .stdout(format!(
            "export ARC_HARNESS='opencode' ARC_SESSION='{session}' ARC_MODEL='kimi-k3#max'\n"
        ));
}

#[test]
fn env_detects_pi_model_and_thinking_level_from_session_store() {
    let repo = Repo::new();
    let session = "019f7520-3278-7736-a3d9-2442c7a51fa0";
    let sessions = repo.home.join("pi-sessions/project");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join(format!("2026-07-18T12-07-52Z_{session}.jsonl")),
        concat!(
            "{\"type\":\"session\",\"id\":\"x\"}\n",
            "{\"type\":\"model_change\",\"modelId\":\"gpt-5.6-sol\"}\n",
            "{\"type\":\"thinking_level_change\",\"thinkingLevel\":\"medium\"}\n",
        ),
    )
    .unwrap();

    repo.arc(&repo.root)
        .arg("env")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .env("PI_SESSION_ID", session)
        .env("PI_CODING_AGENT_SESSION_DIR", repo.home.join("pi-sessions"))
        .assert()
        .success()
        .stdout(format!(
            "export ARC_HARNESS='pi' ARC_SESSION='{session}' ARC_MODEL='gpt-5.6-sol#medium'\n"
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
        .env_remove("PI_SESSION_ID")
        .assert()
        .success()
        .stdout("export ARC_HARNESS='codex' ARC_SESSION='no-such-thread'\n");

    // Nothing detected at all: the fallback comment names ARC_MODEL too.
    repo.arc(&repo.root)
        .arg("env")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .env_remove("PI_SESSION_ID")
        .assert()
        .failure()
        .stdout(predicate::str::contains("ARC_MODEL=<model[#effort]>"));
}
