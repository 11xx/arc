use super::common::*;
use predicates::prelude::*;
use std::os::unix::fs::PermissionsExt;

fn recorded_journal_event(repo: &Repo) -> serde_json::Value {
    let dir = stdout(repo.arc(&repo.root).args(["journal", "dir"]));
    let events = fs::read_to_string(Path::new(dir.trim()).join("events.jsonl")).unwrap();
    serde_json::from_str(events.lines().last().unwrap()).unwrap()
}

fn opened_event(repo: &Repo, change_id: &str) -> serde_json::Value {
    let path = fs::read_dir(event_dir(repo, change_id))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

/// `arc env`'s line for a session the harness's own store backs.
fn corroborated(harness: &str) -> String {
    format!(
        "unset ARC_SESSION_LINK\n# session corroborated: the {harness} session store resolved a recording for this id\n"
    )
}

/// `arc env`'s line for a session id the harness's own store does not hold.
fn uncorroborated(harness: &str) -> String {
    format!(
        "unset ARC_SESSION_LINK\n# session uncorroborated: the {harness} session store resolved no recording for this id\n"
    )
}

fn unresolved(harness: &str) -> String {
    format!(
        "unset ARC_SESSION_LINK\n# session unresolved: the {harness} session store could not establish whether this id has a readable recording\n"
    )
}

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
        .stdout(env_output(format!(
            "export ARC_HARNESS='codex' ARC_SESSION='thread-123'\nunset ARC_MODEL\n{}",
            uncorroborated("codex")
        )));
}

#[test]
fn env_exports_only_a_well_formed_link_for_the_acting_claude_harness() {
    let repo = Repo::new();
    for (id, expected) in [
        ("session_01abc", "https://claude.ai/code/session_01abc"),
        (
            "session_a_staging_b",
            "https://claude-ai.staging.ant.dev/code/session_a_staging_b",
        ),
        (
            "session_a_local_b",
            "http://localhost:4000/code/session_a_local_b",
        ),
    ] {
        let output = stdout(
            repo.arc(&repo.root)
                .arg("env")
                .env("CLAUDE_CODE_SESSION_ID", "claude-session")
                .env("CLAUDE_CODE_BRIDGE_SESSION_ID", id),
        );
        assert!(
            output.contains(&format!("export ARC_SESSION_LINK='{expected}'\n")),
            "{output}"
        );
    }
    for id in [
        None,
        Some("session_"),
        Some("session_abc/other"),
        Some("wrong_abc"),
    ] {
        let mut command = repo.arc(&repo.root);
        command
            .arg("env")
            .env("CLAUDE_CODE_SESSION_ID", "claude-session");
        if let Some(id) = id {
            command.env("CLAUDE_CODE_BRIDGE_SESSION_ID", id);
        }
        assert!(stdout(&mut command).contains("unset ARC_SESSION_LINK\n"));
    }
    let output = stdout(
        repo.arc(&repo.root)
            .arg("env")
            .env("CODEX_THREAD_ID", "codex-session")
            .env("CLAUDE_CODE_BRIDGE_SESSION_ID", "session_01abc"),
    );
    assert!(output.contains("unset ARC_SESSION_LINK\n"), "{output}");
}

#[test]
fn session_link_round_trips_in_ledger_and_journal_events_only_when_supplied() {
    let repo = Repo::new();
    let link = "https://claude.ai/code/session_fixture123";
    let linked = opened_change_id(&stdout(
        repo.arc(&repo.root)
            .args(["begin", "linked-session"])
            .env("ARC_SESSION_LINK", link),
    ));
    let event = opened_event(&repo, &linked);
    assert_eq!(event["session_link"], link);
    assert_eq!(
        json_stdout(repo.arc(&repo.root).args(["show", &linked, "--json"]))["opened_session_link"],
        link
    );
    let flagged = opened_change_id(&stdout(
        repo.arc(&repo.root)
            .args(["begin", "flagged-session", "--session-link", link])
            .env("ARC_SESSION_LINK", "https://example.invalid/overridden"),
    ));
    assert_eq!(opened_event(&repo, &flagged)["session_link"], link);

    let unlinked = opened_change_id(&stdout(
        repo.arc(&repo.root).args(["begin", "plain-session"]),
    ));
    let old_event = opened_event(&repo, &unlinked);
    assert!(old_event.get("session_link").is_none(), "{old_event}");
    assert!(
        json_stdout(repo.arc(&repo.root).args(["show", &unlinked, "--json"]))
            .get("opened_session_link")
            .is_none()
    );

    repo.arc(&repo.root)
        .args(["journal", "note", "linked-note", "--title", "Linked"])
        .env("ARC_SESSION_LINK", link)
        .assert()
        .success();
    assert_eq!(recorded_journal_event(&repo)["session_link"], link);
    repo.arc(&repo.root)
        .args(["journal", "note", "plain-note", "--title", "Plain"])
        .assert()
        .success();
    assert!(recorded_journal_event(&repo).get("session_link").is_none());
    let events = stdout(repo.arc(&repo.root).args(["journal", "events"]));
    assert!(events.contains(link));
}

#[test]
fn session_link_stays_out_of_git_messages_trailers_and_changelog() {
    let repo = Repo::new();
    let link = "https://claude.ai/code/session_privatefixture";
    let slug = "private-provenance";
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let worktree = repo.home.join(".worktrees/repo-private-provenance");
    let base = git_out(&worktree, &["rev-parse", "HEAD"]);
    repo.commit(&worktree, "private.txt", "content\n", "feat: content");

    let rewrite = repo
        .arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--from",
            &base,
            "--append",
            "Implemented-by: fixture",
            "--no-sign",
            "--dry-run",
        ])
        .env("ARC_SESSION_LINK", link)
        .output()
        .unwrap();
    assert!(rewrite.status.success(), "{rewrite:?}");
    assert!(!String::from_utf8_lossy(&rewrite.stdout).contains(link));
    assert!(!String::from_utf8_lossy(&rewrite.stderr).contains(link));

    repo.arc(&worktree)
        .args(["changelog", slug, "--category", "added", "--body-file", "-"])
        .write_stdin("- Added content\n")
        .env("ARC_SESSION_LINK", link)
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["snapshot", slug])
        .env("ARC_SESSION_LINK", link)
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .env("ARC_SESSION_LINK", link)
        .assert()
        .success();
    let dry_run = stdout(
        repo.arc(&repo.root)
            .args(["integrate", slug, "--dry-run"])
            .env("ARC_SESSION_LINK", link),
    );
    assert!(dry_run.contains("merge message:"), "{dry_run}");
    assert!(!dry_run.contains(link), "{dry_run}");
    repo.arc(&repo.root)
        .args(["integrate", slug])
        .env("ARC_SESSION_LINK", link)
        .assert()
        .success();
    assert!(!git_out(&repo.root, &["log", "-1", "--format=%B"]).contains(link));

    let projection = stdout(
        repo.arc(&repo.root)
            .args(["changelog", "--json"])
            .env("ARC_SESSION_LINK", link),
    );
    assert!(!projection.contains(link), "{projection}");
    fs::write(
        repo.root.join("CHANGELOG.md"),
        "# Changelog\n\n## [Unreleased]\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .env("ARC_SESSION_LINK", link)
        .assert()
        .success();
    assert!(!fs::read_to_string(repo.root.join("CHANGELOG.md"))
        .unwrap()
        .contains(link));
}

#[test]
fn resume_json_uses_arc_resume_schema() {
    let repo = Repo::new();
    let output = stdout(repo.arc(&repo.root).args(["begin", "contextual"]));
    let change_id = opened_change_id(&output);
    let worktree = repo.home.join(".worktrees/repo-contextual");
    let output = stdout(repo.arc(&worktree).args(["resume", "--json"]));
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();

    assert_eq!(value["schema"], "arc-resume/8");
    assert_eq!(value["status"]["schema"], "arc-status/27");
    assert!(value["status"]["policy_sources"].is_object());
    assert_eq!(value["status"]["change_id"], change_id);
}

#[test]
fn resume_reports_worktree_state_and_head_drift() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "resume-worktree"]));
    let worktree = repo.home.join(".worktrees/repo-resume-worktree");

    repo.arc(&worktree)
        .arg("resume")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Branch head: no patchset recorded",
        ))
        .stdout(predicate::str::contains("Uncommitted edits: absent"));

    repo.commit(&worktree, "first.txt", "first\n", "test: add first");
    stdout(repo.arc(&worktree).arg("snapshot"));

    repo.arc(&worktree)
        .arg("resume")
        .assert()
        .success()
        .stdout(predicate::str::contains("## Worktree"))
        .stdout(predicate::str::contains(
            "Branch head: matches the newest approved/snapshotted head",
        ))
        .stdout(predicate::str::contains("Uncommitted edits: absent"));

    repo.commit(&worktree, "second.txt", "second\n", "test: add second");
    repo.arc(&worktree)
        .arg("resume")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Branch head: has moved past the newest patchset",
        ))
        .stdout(predicate::str::contains("Uncommitted edits: absent"));
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
    let project = repo.home.join(".claude/projects/-home-user");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{session}.jsonl")),
        concat!(
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-opus-4-8\"}}\n",
            "not a json line\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-fable-5\"}}\n",
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
        .stdout(env_output(format!(
            "export ARC_HARNESS='claude' ARC_SESSION='{session}' ARC_MODEL='claude-fable-5'\n{}",
            corroborated("claude")
        )));
}

#[test]
fn env_detects_claude_effort_and_skips_synthetic_entries() {
    let repo = Repo::new();
    let session = "11111111-2222-3333-4444-555555555555";
    let project = repo.home.join(".claude/projects/-home-user");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{session}.jsonl")),
        concat!(
            "{\"type\":\"assistant\",\"effort\":\"medium\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-opus-4-8\"}}\n",
            "{\"type\":\"assistant\",\"effort\":\"medium\",\"perTurnEffort\":\"high\",",
            "\"message\":{\"role\":\"assistant\",\"model\":\"claude-fable-5\"}}\n",
            "{\"type\":\"assistant\",\"isApiErrorMessage\":true,",
            "\"message\":{\"role\":\"assistant\",\"model\":\"<synthetic>\"}}\n",
        ),
    )
    .unwrap();

    // The turn's own effort beats the session-wide one, and an API-error
    // entry at the tail names no model.
    repo.arc(&repo.root)
        .arg("env")
        .env("CLAUDE_SESSION_ID", session)
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .env_remove("PI_SESSION_ID")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='claude' ARC_SESSION='{session}' ARC_MODEL='claude-fable-5#high'\n{}",
            corroborated("claude")
        )));
}

#[test]
fn env_falls_back_to_the_claude_session_effort() {
    let repo = Repo::new();
    let session = "11111111-2222-3333-4444-555555555555";
    let project = repo.home.join(".claude/projects/-home-user");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{session}.jsonl")),
        concat!(
            "{\"type\":\"assistant\",\"perTurnEffort\":\"high\",",
            "\"message\":{\"role\":\"assistant\",\"model\":\"claude-opus-4-8\"}}\n",
            "{\"type\":\"assistant\",\"effort\":\"low\",\"perTurnEffort\":null,",
            "\"message\":{\"role\":\"assistant\",\"model\":\"claude-fable-5\"}}\n",
        ),
    )
    .unwrap();

    // Without a turn effort, the newest entry's session effort stands; an
    // older entry's turn effort does not leak into it.
    repo.arc(&repo.root)
        .arg("env")
        .env("CLAUDE_SESSION_ID", session)
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .env_remove("PI_SESSION_ID")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='claude' ARC_SESSION='{session}' ARC_MODEL='claude-fable-5#low'\n{}",
            corroborated("claude")
        )));
}

#[test]
fn env_resolves_the_claude_store_under_its_config_dir_override() {
    let repo = Repo::new();
    let session = "22222222-3333-4444-5555-666666666666";
    let relocated = repo.home.join("relocated-claude");
    let project = relocated.join("projects/-home-user");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{session}.jsonl")),
        "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-fable-5\"}}\n",
    )
    .unwrap();
    // The default store holds the same session under a different model, so a
    // store consulted in addition to the override would answer with it: the
    // override replaces the configuration directory rather than extending it.
    let default = repo.home.join(".claude/projects/-home-user");
    fs::create_dir_all(&default).unwrap();
    fs::write(
        default.join(format!("{session}.jsonl")),
        "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-elsewhere\"}}\n",
    )
    .unwrap();

    repo.arc(&repo.root)
        .arg("env")
        .env("CLAUDE_SESSION_ID", session)
        .env("CLAUDE_CONFIG_DIR", &relocated)
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='claude' ARC_SESSION='{session}' ARC_MODEL='claude-fable-5'\n{}",
            corroborated("claude")
        )));
}

#[test]
fn env_takes_the_newest_claude_recording_across_project_directories() {
    let repo = Repo::new();
    let session = "33333333-4444-5555-6666-777777777777";
    let projects = repo.home.join(".claude/projects");
    // The stale recording's directory is created first, which is the order
    // this fixture's directory enumeration offers them in. The rule is the
    // newest recording, not the first one offered.
    let stale = projects.join("a-project");
    let live = projects.join("b-project");
    fs::create_dir_all(&stale).unwrap();
    let stale_file = stale.join(format!("{session}.jsonl"));
    fs::write(
        &stale_file,
        "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-stale\"}}\n",
    )
    .unwrap();
    fs::create_dir_all(&live).unwrap();
    let live_file = live.join(format!("{session}.jsonl"));
    fs::write(
        &live_file,
        "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-live\"}}\n",
    )
    .unwrap();
    set_modified(&stale_file, 1_700_000_000);
    set_modified(&live_file, 1_800_000_000);

    repo.arc(&repo.root)
        .arg("env")
        .env("CLAUDE_SESSION_ID", session)
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .env_remove("PI_SESSION_ID")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='claude' ARC_SESSION='{session}' ARC_MODEL='claude-live'\n{}",
            corroborated("claude")
        )));
}

fn set_modified(path: &Path, seconds: u64) {
    let time = std::time::UNIX_EPOCH + Duration::from_secs(seconds);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

#[test]
fn env_detects_claude_code_session_variable() {
    let repo = Repo::new();
    let session = "66666666-7777-8888-9999-000000000000";
    let project = repo.home.join(".claude/projects/-home-user");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{session}.jsonl")),
        "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-fable-5\"}}\n",
    )
    .unwrap();

    // Claude Code exports `CLAUDE_CODE_SESSION_ID`, not `CLAUDE_SESSION_ID`;
    // the same session store lookup serves either spelling.
    repo.arc(&repo.root)
        .arg("env")
        .env_remove("CLAUDE_SESSION_ID")
        .env("CLAUDE_CODE_SESSION_ID", session)
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='claude' ARC_SESSION='{session}' ARC_MODEL='claude-fable-5'\n{}",
            corroborated("claude")
        )));
}

#[test]
fn env_prefers_hand_set_claude_session_over_ambient() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .arg("env")
        .env("CLAUDE_SESSION_ID", "hand-set")
        .env("CLAUDE_CODE_SESSION_ID", "ambient")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='claude' ARC_SESSION='hand-set'\nunset ARC_MODEL\n{}",
            uncorroborated("claude")
        )));
}

#[test]
fn env_detects_codex_model_and_effort_from_rollout() {
    let repo = Repo::new();
    let session = "019f0000-0000-7000-8000-000000000002";
    let codex_home = repo.home.join("custom-codex-state");
    let day = codex_home.join("sessions/2026/07/20");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join(format!("rollout-2026-07-20T00-00-00-{session}.jsonl")),
        concat!(
            "{\"type\":\"session_meta\",\"timestamp\":\"1\",",
            "\"payload\":{\"id\":\"019f0000-0000-7000-8000-000000000002\"}}\n",
            "{\"type\":\"turn_context\",\"timestamp\":\"2\",\"payload\":{\"model\":\"gpt-5.5\"}}\n",
            "{\"type\":\"turn_context\",\"timestamp\":\"3\",\"payload\":{\"model\":\"gpt-5.6-sol\",",
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
        .stdout(env_output(format!(
            "export ARC_HARNESS='codex' ARC_SESSION='{session}' ARC_MODEL='gpt-5.6-sol#high'\n{}",
            corroborated("codex")
        )));
}

#[test]
fn env_requires_exact_codex_identity_not_year_substring() {
    let repo = Repo::new();
    let codex_home = repo.home.join("codex-state");
    let day = codex_home.join("sessions/2026/09/25");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join("rollout-2026-09-25T10-00-00-00000000-0000-4000-8000-000000000001.jsonl"),
        concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"different-session\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-fixture\",\"effort\":\"high\"}}\n",
        ),
    )
    .unwrap();

    repo.arc(&repo.root)
        .args(["env"])
        .env("CODEX_THREAD_ID", "2026")
        .env("CODEX_HOME", &codex_home)
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OPENCODE_SESSION")
        .env_remove("PI_SESSION_ID")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='codex' ARC_SESSION='2026'\nunset ARC_MODEL\n{}",
            uncorroborated("codex")
        )));
}

#[test]
fn env_requires_the_canonical_codex_id_before_using_its_model() {
    let repo = Repo::new();
    let full = "019f0000-0000-7000-8000-000000000002";
    let prefix = "019f0000";
    codex_recording(&repo, full, "gpt-fixture");
    let codex_home = repo.home.join("codex-state");

    let exact = stdout(
        repo.arc(&repo.root)
            .arg("env")
            .env("CODEX_HOME", &codex_home)
            .env("CODEX_THREAD_ID", full),
    );
    assert!(exact.contains("ARC_MODEL='gpt-fixture#high'"), "{exact}");
    assert!(exact.contains("session corroborated"), "{exact}");

    let partial = stdout(
        repo.arc(&repo.root)
            .arg("env")
            .env("CODEX_HOME", &codex_home)
            .env("CODEX_THREAD_ID", prefix),
    );
    assert!(partial.contains("ARC_SESSION='019f0000'"), "{partial}");
    assert!(partial.contains("unset ARC_MODEL"), "{partial}");
    assert!(partial.contains("session uncorroborated"), "{partial}");
    assert!(!partial.contains("gpt-fixture"), "{partial}");
}

#[test]
fn ambiguous_codex_prefix_keeps_session_resolution_unknown() {
    let repo = Repo::new();
    let prefix = "019f0000";
    codex_recording(&repo, "019f0000-0000-7000-8000-000000000002", "first-model");
    codex_recording(
        &repo,
        "019f0000-1234-4444-8888-111111111111",
        "second-model",
    );
    let codex_home = repo.home.join("codex-state");

    let output = stdout(
        repo.arc(&repo.root)
            .arg("env")
            .env("CODEX_HOME", &codex_home)
            .env("CODEX_THREAD_ID", prefix),
    );
    assert!(output.contains("unset ARC_MODEL"), "{output}");
    assert!(output.contains("session unresolved"), "{output}");
    assert!(!output.contains("session uncorroborated"), "{output}");

    enable_identity_detection(&repo);
    let opened = stdout(
        repo.arc(&repo.root)
            .args(["begin", "ambiguous-codex-id"])
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_SESSION")
            .env("CODEX_HOME", &codex_home)
            .env("CODEX_THREAD_ID", prefix),
    );
    let event = opened_event(&repo, &opened_change_id(&opened));
    assert_eq!(event["session_resolution"], "unresolved", "{event}");
    assert!(event.get("model").is_none(), "{event}");
    assert_eq!(event["schema_version"], 7, "{event}");
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 5, "{config}");
    let bundle = repo.home.join("unresolved-bundle.json");
    repo.arc(&repo.root)
        .args([
            "export",
            "ambiguous-codex-id",
            "--output",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();
    let exported: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    assert_eq!(exported["schema"], "arc-bundle/6", "{exported}");
    assert_eq!(exported["store_format"], 7, "{exported}");

    let recipient = Repo::new();
    recipient
        .arc(&recipient.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let imported = opened_event(&recipient, &opened_change_id(&opened));
    assert_eq!(imported["session_resolution"], "unresolved", "{imported}");
    let recipient_config: serde_json::Value =
        serde_json::from_slice(&fs::read(recipient.root.join(".git/arc/config.json")).unwrap())
            .unwrap();
    assert_eq!(recipient_config["schema_version"], 5, "{recipient_config}");
}

#[test]
fn unreadable_codex_recording_does_not_corroborate_a_model() {
    let repo = Repo::new();
    let session = "019f0000-0000-7000-8000-000000000002";
    let path = codex_recording(&repo, session, "gpt-fixture");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let output = stdout(
        repo.arc(&repo.root)
            .arg("env")
            .env("CODEX_HOME", repo.home.join("codex-state"))
            .env("CODEX_THREAD_ID", session),
    );
    assert!(output.contains("unset ARC_MODEL"), "{output}");
    assert!(output.contains("session unresolved"), "{output}");
    assert!(!output.contains("session uncorroborated"), "{output}");
    assert!(!output.contains("gpt-fixture"), "{output}");
}

#[test]
fn env_does_not_use_an_opencode_listing_model_when_the_read_fails() {
    let repo = Repo::new();
    let session = "ses_test123";
    let data_home = repo.home.join("data");
    let store = data_home.join("opencode/opencode.db");
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    // tapes reads the stable OpenCode store through sqlite3, so the fixture
    // supplies a database carrying the one header byte sequence that marks a
    // SQLite file and a deterministic reader on PATH, rather than depending
    // on a host SQLite installation.
    fs::write(&store, b"SQLite format 3\0").unwrap();

    let bin = repo.home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let sqlite = bin.join("sqlite3");
    fs::write(
        &sqlite,
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' 'row'\n",
            "printf '%s\\n' '{\"id\":\"ses_test123\",\"model\":\"{\\\"id\\\":\\\"kimi-k3\\\",",
            "\\\"providerID\\\":\\\"opencode-go\\\",\\\"variant\\\":\\\"max\\\"}\"}'\n",
        ),
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
        .stdout(env_output(format!(
            "export ARC_HARNESS='opencode' ARC_SESSION='{session}'\nunset ARC_MODEL\n# export ARC_MODEL=<model[#effort]>  # unavailable: the session store could not resolve or read this id\n{}",
            unresolved("opencode")
        )));
}

#[test]
fn env_detects_pi_model_and_thinking_level_from_session_store() {
    let repo = Repo::new();
    let session = "019f0000-0000-7000-8000-000000000001";
    let sessions = repo.home.join("pi-sessions/project");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join(format!("2026-07-18T12-07-52Z_{session}.jsonl")),
        concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"019f0000-0000-7000-8000-000000000001\",",
            "\"timestamp\":\"2026-07-18T12:07:52Z\",\"cwd\":\"/fixture\"}\n",
            "{\"type\":\"model_change\",\"id\":\"model-1\",\"parentId\":null,",
            "\"timestamp\":\"2026-07-18T12:07:53Z\",\"provider\":\"openai-codex\",",
            "\"modelId\":\"gpt-5.6-sol\"}\n",
            "{\"type\":\"thinking_level_change\",\"id\":\"thinking-1\",\"parentId\":\"model-1\",",
            "\"timestamp\":\"2026-07-18T12:07:54Z\",\"thinkingLevel\":\"medium\"}\n",
            "{\"type\":\"message\",\"id\":\"assistant-1\",\"parentId\":\"thinking-1\",",
            "\"timestamp\":\"2026-07-18T12:07:55Z\",\"message\":{\"role\":\"assistant\",",
            "\"timestamp\":1767261604000,\"provider\":\"openai-codex\",\"model\":\"gpt-5.6-sol\",",
            "\"content\":[{\"type\":\"text\",\"text\":\"The recording's answer.\"}]}}\n",
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
        .stdout(env_output(format!(
            "export ARC_HARNESS='pi' ARC_SESSION='{session}' ARC_MODEL='gpt-5.6-sol#medium'\n{}",
            corroborated("pi")
        )));
}

#[test]
fn env_detects_opencode2_by_terminal_variable_and_leaves_the_session_unset() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .arg("env")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .env_remove("PI_SESSION_ID")
        .env_remove("OPENCODE_TERMINAL")
        .env("OPENCODE_TERMINAL", "1")
        .assert()
        .success()
        .stdout("export ARC_HARNESS='opencode'\nunset ARC_SESSION ARC_MODEL ARC_SESSION_LINK\n# export ARC_SESSION=<session-id>  # unavailable: opencode does not export a session variable; set it by hand\n");
}

#[test]
fn env_detects_opencode2_by_process_ancestry() {
    let repo = Repo::new();
    // A shell whose comm is `opencode2` playing the harness, so the binary
    // under test descends from a matching PPID chain in /proc.
    let shell = repo.home.join("bin");
    fs::create_dir_all(&shell).unwrap();
    let harness = shell.join("opencode2");
    // No exec: the wrapper must survive as arc's parent for the comm to
    // appear in the PPID chain the detection walks. The trailing no-op stops
    // dash from exec-optimizing the last command into the wrapper's own
    // process, which would leave arc parented to the test runner instead.
    fs::write(&harness, "#!/bin/sh\n\"$@\"\n:\n").unwrap();
    fs::set_permissions(&harness, fs::Permissions::from_mode(0o755)).unwrap();

    // assert_cmd execs the binary directly, which would leave the test runner
    // as the parent; spawn through the wrapper so the ancestry is real.
    let mut wrapped = fixture_arc(&repo, &harness);
    wrapped.arg(assert_cmd::cargo_bin!("arc")).arg("env");
    let output = output_past_busy_text(&mut wrapped);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "export ARC_HARNESS='opencode'\nunset ARC_SESSION ARC_MODEL ARC_SESSION_LINK\n# export ARC_SESSION=<session-id>  # unavailable: opencode does not export a session variable; set it by hand\n"
    );
}

/// One harness in a synthetic ancestry: its wrapper name, the session variable
/// it exports, and the session that variable names.
type HarnessLink<'a> = (&'a str, &'a str, &'a str);

/// A `/bin/sh` process named `name` that exports its own session variable and
/// runs `inner` as a child, the way a harness hands its session to the tool
/// shells it starts. The trailing no-op keeps the wrapper alive as its child's
/// parent instead of exec-optimizing itself away.
fn harness_link(repo: &Repo, name: &str, variable: &str, session: &str, inner: &Path) -> PathBuf {
    let bin = repo.home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join(name);
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport {variable}='{session}'\n\"{}\" \"$@\"\n:\n",
            inner.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    wrapper
}

/// A command for the fixture binary with this suite's own actor and harness
/// variables removed, so only what a test sets reaches the binary under test.
fn fixture_arc(repo: &Repo, program: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(&repo.root)
        .env("HOME", &repo.home)
        .env("ARC_SANDBOX", &repo.home)
        .envs(NO_EDITOR)
        .env_remove("ARC_ACTOR")
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION")
        .env_remove("ARC_MODEL")
        .env_remove("ARC_SESSION_LINK")
        .env_remove("ARC_ON_BEHALF_OF")
        .env_remove("ARC_DATA_DIR")
        .env_remove("ARC_DATA_ROOT")
        .env_remove("ARC_WORKTREES_DIR")
        .env_remove("AI_HOME")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_CODE_BRIDGE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .env_remove("OPENCODE_TERMINAL")
        .env_remove("PI_SESSION_ID")
        .env_remove("PI_SESSION_FILE")
        .env_remove("PI_MODEL")
        .env_remove("PI_REASONING_LEVEL");
    command
}

/// Run the fixture binary under `chain`, outermost harness first. Each link is
/// a real process that exports its own session variable, as a harness does for
/// the tool shells it starts; no real harness is needed.
fn nested_arc(repo: &Repo, chain: &[HarnessLink], args: &[&str]) -> std::process::Output {
    let mut program = assert_cmd::cargo_bin!("arc").to_path_buf();
    for (name, variable, session) in chain.iter().rev() {
        program = harness_link(repo, name, variable, session, &program);
    }
    let mut command = fixture_arc(repo, &program);
    command.args(args);
    output_past_busy_text(&mut command)
}

/// Every nested pairing the harness variables can make: the outer harness
/// starts the inner one, and the inner one starts arc.
const NESTED_PAIRS: [(&str, &str, &str, &str); 4] = [
    ("claude", "CLAUDE_CODE_SESSION_ID", "pi", "PI_SESSION_ID"),
    (
        "claude",
        "CLAUDE_CODE_SESSION_ID",
        "codex",
        "CODEX_THREAD_ID",
    ),
    (
        "codex",
        "CODEX_THREAD_ID",
        "claude",
        "CLAUDE_CODE_SESSION_ID",
    ),
    ("codex", "CODEX_THREAD_ID", "pi", "PI_SESSION_ID"),
];

/// The harness that owns the process is the shell that exported the session
/// id into it, not the first harness a fixed list happens to name.
#[test]
fn env_reports_the_nested_harness_that_owns_the_process() {
    for (outer, outer_variable, inner, inner_variable) in NESTED_PAIRS {
        let repo = Repo::new();
        let outer_session = format!("{outer}-outer-session");
        let inner_session = format!("{inner}-inner-session");
        let chain = [
            (outer, outer_variable, outer_session.as_str()),
            (inner, inner_variable, inner_session.as_str()),
        ];
        let output = nested_arc(&repo, &chain, &["env"]);
        assert!(
            output.status.success(),
            "{inner} under {outer} failed: {output:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!(
                "export ARC_HARNESS='{inner}' ARC_SESSION='{inner_session}'\nunset ARC_MODEL\n{}",
                uncorroborated(inner)
            ),
            "{inner} under {outer} reported the wrong owner"
        );
    }
}

/// Corroboration is not ownership. Both stores can hold a recording for their
/// own session, and the harness that exported the nearest one still owns the
/// process, with its own model and its store's verdict reported together.
#[test]
fn nested_detection_prefers_the_owner_over_a_corroborated_outer_session() {
    let repo = Repo::new();
    let outer_session = "11111111-2222-3333-4444-555555555555";
    let inner_session = "019f0000-0000-7000-8000-000000000001";
    let project = repo.home.join(".claude/projects/-test-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join(format!("{outer_session}.jsonl")),
        "{\"type\":\"assistant\",\"timestamp\":\"1\",\"message\":{\"role\":\"assistant\",\"model\":\"claude-outer\"}}\n",
    )
    .unwrap();
    let sessions = repo.home.join("pi-sessions/project");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join(format!("2026-07-18T12-07-52Z_{inner_session}.jsonl")),
        concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"019f0000-0000-7000-8000-000000000001\",",
            "\"timestamp\":\"2026-07-18T12:07:52Z\",\"cwd\":\"/fixture\"}\n",
            "{\"type\":\"thinking_level_change\",\"id\":\"thinking-1\",\"parentId\":null,",
            "\"timestamp\":\"2026-07-18T12:07:53Z\",\"thinkingLevel\":\"medium\"}\n",
            "{\"type\":\"message\",\"id\":\"assistant-1\",\"parentId\":\"thinking-1\",",
            "\"timestamp\":\"2026-07-18T12:07:54Z\",\"message\":{\"role\":\"assistant\",",
            "\"timestamp\":1767261604000,\"provider\":\"openai-codex\",\"model\":\"gpt-5.6-sol\",",
            "\"content\":[{\"type\":\"text\",\"text\":\"The recording's answer.\"}]}}\n",
        ),
    )
    .unwrap();

    let mut program = assert_cmd::cargo_bin!("arc").to_path_buf();
    program = harness_link(&repo, "pi", "PI_SESSION_ID", inner_session, &program);
    program = harness_link(
        &repo,
        "claude",
        "CLAUDE_CODE_SESSION_ID",
        outer_session,
        &program,
    );
    let mut command = fixture_arc(&repo, &program);
    command
        .env("PI_CODING_AGENT_SESSION_DIR", repo.home.join("pi-sessions"))
        .arg("env");
    let output = output_past_busy_text(&mut command);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        without_observation(&String::from_utf8_lossy(&output.stdout)),
        format!(
            "export ARC_HARNESS='pi' ARC_SESSION='{inner_session}' ARC_MODEL='gpt-5.6-sol#medium'\n{}",
            corroborated("pi")
        )
    );
}

/// An event written with no declared identity carries the harness that owns
/// the process, its own session, the store's verdict, and the derived actor —
/// everything a later reader needs to reach the thread that did the work.
#[test]
fn nested_detection_records_the_owning_harness_on_an_undeclared_event() {
    let repo = Repo::new();
    enable_identity_detection(&repo);
    let outer_session = "22222222-3333-4444-5555-666666666666";
    let inner_session = "019f0000-0000-7000-8000-000000000001";
    let sessions = repo.home.join("pi-sessions/project");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join(format!("2026-07-18T12-07-52Z_{inner_session}.jsonl")),
        concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"019f0000-0000-7000-8000-000000000001\",",
            "\"timestamp\":\"2026-07-18T12:07:52Z\",\"cwd\":\"/fixture\"}\n",
            "{\"type\":\"message\",\"id\":\"assistant-1\",\"parentId\":null,",
            "\"timestamp\":\"2026-07-18T12:07:53Z\",\"message\":{\"role\":\"assistant\",",
            "\"timestamp\":1767261603000,\"provider\":\"openai-codex\",\"model\":\"gpt-5.6-sol\",",
            "\"content\":[{\"type\":\"text\",\"text\":\"The recording's answer.\"}]}}\n",
        ),
    )
    .unwrap();

    let mut program = assert_cmd::cargo_bin!("arc").to_path_buf();
    program = harness_link(&repo, "pi", "PI_SESSION_ID", inner_session, &program);
    program = harness_link(
        &repo,
        "claude",
        "CLAUDE_CODE_SESSION_ID",
        outer_session,
        &program,
    );
    let mut command = fixture_arc(&repo, &program);
    command
        .env("PI_CODING_AGENT_SESSION_DIR", repo.home.join("pi-sessions"))
        .args(["begin", "nested-detect"]);
    let output = output_past_busy_text(&mut command);
    assert!(output.status.success(), "{output:?}");

    let event = opened_event(
        &repo,
        &opened_change_id(&String::from_utf8_lossy(&output.stdout)),
    );
    assert_eq!(event["harness"], "pi", "{event}");
    assert_eq!(event["session"], inner_session, "{event}");
    assert_eq!(event["session_resolution"], "corroborated", "{event}");
    assert_eq!(event["actor"], format!("pi:{inner_session}"), "{event}");
    assert_eq!(event["actor_source"], "derived", "{event}");
    assert_ne!(event["session"], outer_session, "{event}");
}

/// Several harnesses' session variables in a process whose ancestry names no
/// owner is an ambiguity arc reports instead of resolving by list position.
#[test]
fn env_reports_ambiguity_when_no_ancestor_names_the_owner() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .arg("env")
        .env("CLAUDE_CODE_SESSION_ID", "outer-session")
        .env("PI_SESSION_ID", "inner-session")
        .assert()
        .code(1)
        .stdout(concat!(
            "# export ARC_HARNESS=<claude|codex|opencode|pi> ARC_SESSION=<session-id> ",
            "ARC_MODEL=<model[#effort]> ARC_SESSION_LINK=<url>\n",
            "# ambiguous: CLAUDE_CODE_SESSION_ID (claude) and PI_SESSION_ID (pi); ",
            "set ARC_HARNESS and ARC_SESSION by hand\n",
            "unset ARC_HARNESS ARC_SESSION ARC_MODEL ARC_SESSION_LINK\n"
        ));
}

/// The same ambiguity leaves an undeclared event's identity unset: nothing is
/// recorded that a later reader could mistake for the owning thread.
#[test]
fn ambiguous_detection_records_no_identity_on_an_undeclared_event() {
    let repo = Repo::new();
    enable_identity_detection(&repo);
    let output = stdout(
        repo.arc(&repo.root)
            .args(["begin", "ambiguous-detect"])
            .env_remove("ARC_ACTOR")
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_SESSION")
            .env("CLAUDE_CODE_SESSION_ID", "outer-session")
            .env("PI_SESSION_ID", "inner-session"),
    );
    let event = opened_event(&repo, &opened_change_id(&output));
    assert!(event["harness"].is_null(), "{event}");
    assert!(event["session"].is_null(), "{event}");
    assert!(event["model"].is_null(), "{event}");
    assert!(event.get("session_resolution").is_none(), "{event}");
    assert_eq!(event["actor"], "Tester", "{event}");
    assert_eq!(event["actor_source"], "git-fallback", "{event}");
}

/// A Pi recording in the fixture's store: the header, the thinking level in
/// effect, and one assistant turn naming the model. Returns the file, so a
/// caller can also name it as the live recording.
fn pi_recording(repo: &Repo, session: &str, model: &str, level: &str) -> PathBuf {
    let sessions = repo.home.join("pi-sessions/project");
    fs::create_dir_all(&sessions).unwrap();
    let path = sessions.join(format!("2026-07-18T12-07-52Z_{session}.jsonl"));
    fs::write(
        &path,
        format!(
            concat!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{session}\",",
                "\"timestamp\":\"2026-07-18T12:07:52Z\",\"cwd\":\"/fixture\"}}\n",
                "{{\"type\":\"thinking_level_change\",\"id\":\"thinking-1\",\"parentId\":null,",
                "\"timestamp\":\"2026-07-18T12:07:53Z\",\"thinkingLevel\":\"{level}\"}}\n",
                "{{\"type\":\"message\",\"id\":\"assistant-1\",\"parentId\":\"thinking-1\",",
                "\"timestamp\":\"2026-07-18T12:07:54Z\",\"message\":{{\"role\":\"assistant\",",
                "\"timestamp\":1767261604000,\"provider\":\"openai-codex\",\"model\":\"{model}\",",
                "\"content\":[{{\"type\":\"text\",\"text\":\"The recording's answer.\"}}]}}}}\n",
            ),
            session = session,
            model = model,
            level = level,
        ),
    )
    .unwrap();
    path
}

/// A Claude session that has spawned one subagent, with the subagent's own
/// recording beside it. `completed` decides whether the parent recorded the
/// subagent as finished.
fn claude_with_subagent(repo: &Repo, session: &str, agent: &str, completed: bool) {
    let project = repo.home.join(".claude/projects/-test-repo");
    fs::create_dir_all(&project).unwrap();
    let mut parent = vec![
        format!(
            "{{\"type\":\"user\",\"sessionId\":\"{session}\",\"uuid\":\"u1\",\"parentUuid\":null,\"timestamp\":\"2026-01-01T10:00:00Z\",\"message\":{{\"role\":\"user\",\"content\":\"work\"}}}}"
        ),
        format!(
            "{{\"type\":\"assistant\",\"sessionId\":\"{session}\",\"uuid\":\"a1\",\"parentUuid\":\"u1\",\"timestamp\":\"2026-01-01T10:00:01Z\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-parent\",\"content\":[{{\"type\":\"text\",\"text\":\"starting\"}}]}}}}"
        ),
        format!(
            "{{\"type\":\"assistant\",\"sessionId\":\"{session}\",\"uuid\":\"a2\",\"parentUuid\":\"a1\",\"timestamp\":\"2026-01-01T10:00:02Z\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-parent\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"tool-agent-1\",\"name\":\"Agent\",\"input\":{{\"subagent_type\":\"Explore\",\"description\":\"Inspect the fixture.\"}}}}]}}}}"
        ),
    ];
    if completed {
        parent.push(format!(
            "{{\"type\":\"user\",\"sessionId\":\"{session}\",\"uuid\":\"r1\",\"parentUuid\":\"a2\",\"timestamp\":\"2026-01-01T10:00:04Z\",\"toolUseResult\":{{\"status\":\"completed\",\"agentId\":\"{agent}\",\"agentType\":\"Explore\"}},\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"tool-agent-1\",\"content\":\"Subagent complete.\"}}]}}}}"
        ));
    }
    fs::write(
        project.join(format!("{session}.jsonl")),
        parent.join("\n") + "\n",
    )
    .unwrap();

    let directory = project.join(session).join("subagents");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join(format!("agent-{agent}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"sessionId\":\"{session}\",\"uuid\":\"s1\",\"parentUuid\":null,\"timestamp\":\"2026-01-01T10:00:03Z\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-subagent\",\"content\":[{{\"type\":\"text\",\"text\":\"subagent work\"}}]}}}}\n"
        ),
    )
    .unwrap();
    if completed {
        fs::write(
            directory.join(format!("agent-{agent}.meta.json")),
            "{\"agentType\":\"Explore\",\"description\":\"Inspect the fixture.\",\"toolUseId\":\"tool-agent-1\",\"spawnDepth\":1,\"model\":\"sonnet\"}\n",
        )
        .unwrap();
    }
}

/// Pi re-sets its model and reasoning level for every tool call, so while
/// `PI_SESSION_ID` is the acting session those live values answer and the
/// recording is the fallback. A level without a live model names no model.
#[test]
fn env_reports_the_live_pi_values_for_the_acting_session() {
    let repo = Repo::new();
    let session = "019f0000-0000-7000-8000-000000000001";
    pi_recording(&repo, session, "gpt-5.6-recorded", "low");

    repo.arc(&repo.root)
        .arg("env")
        .env("PI_SESSION_ID", session)
        .env("PI_CODING_AGENT_SESSION_DIR", repo.home.join("pi-sessions"))
        .env("PI_MODEL", "gpt-5.6-live")
        .env("PI_REASONING_LEVEL", "high")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='pi' ARC_SESSION='{session}' ARC_MODEL='gpt-5.6-live#high'\n{}",
            corroborated("pi")
        )));

    repo.arc(&repo.root)
        .arg("env")
        .env("PI_SESSION_ID", session)
        .env("PI_CODING_AGENT_SESSION_DIR", repo.home.join("pi-sessions"))
        .env("PI_REASONING_LEVEL", "high")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='pi' ARC_SESSION='{session}' ARC_MODEL='gpt-5.6-recorded#low'\n{}",
            corroborated("pi")
        )));
}

/// A session opened with `--session`/`--session-dir` lives outside the
/// configured store; `PI_SESSION_FILE` names its recording, so the session is
/// corroborated from there.
#[test]
fn env_corroborates_a_pi_session_recorded_at_the_live_file() {
    let repo = Repo::new();
    let session = "019f0000-0000-7000-8000-000000000001";
    let recorded = pi_recording(&repo, session, "gpt-5.6-recorded", "medium");
    let elsewhere = repo.home.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();

    repo.arc(&repo.root)
        .arg("env")
        .env("PI_SESSION_ID", session)
        .env("PI_SESSION_FILE", &recorded)
        .env("PI_CODING_AGENT_SESSION_DIR", &elsewhere)
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='pi' ARC_SESSION='{session}' ARC_MODEL='gpt-5.6-recorded#medium'\n{}",
            corroborated("pi")
        )));

    // Without the live file the recording is outside every configured root.
    repo.arc(&repo.root)
        .arg("env")
        .env("PI_SESSION_ID", session)
        .env("PI_CODING_AGENT_SESSION_DIR", &elsewhere)
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='pi' ARC_SESSION='{session}'\nunset ARC_MODEL\n{}",
            uncorroborated("pi")
        )));
}

/// A subagent's tool shell carries its parent's session id, and the store does
/// not say which subagent a shell belongs to. While the session has not
/// recorded a subagent as finished, `arc env` names no model and says why
/// rather than report the parent's.
#[test]
fn env_withholds_the_model_while_a_subagent_recording_is_live() {
    let repo = Repo::new();
    let session = "4074d881-1111-2222-3333-444444444444";
    claude_with_subagent(&repo, session, "aef42352478f33196", false);

    repo.arc(&repo.root)
        .arg("env")
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .env_remove("PI_SESSION_ID")
        .assert()
        .success()
        .stdout(env_output(format!(
            concat!(
                "export ARC_HARNESS='claude' ARC_SESSION='{session}'\n",
                "unset ARC_MODEL\n",
                "# export ARC_MODEL=<model[#effort]>  # unavailable: a subagent recording ",
                "is newer than the session's last turn, so the session's model need not ",
                "be the acting one\n",
                "{}"
            ),
            corroborated("claude"),
            session = session
        )));

    // A subagent the session recorded as finished leaves the session's own
    // model in place.
    claude_with_subagent(&repo, session, "aef42352478f33196", true);
    repo.arc(&repo.root)
        .arg("env")
        .env("CLAUDE_CODE_SESSION_ID", session)
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .env_remove("PI_SESSION_ID")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='claude' ARC_SESSION='{session}' ARC_MODEL='claude-parent'\n{}",
            corroborated("claude")
        )));
}

/// Every identity field `arc env` cannot establish is unset, so evaluating
/// its output never leaves a stale value beside a fresh one.
#[test]
fn env_unsets_the_identity_fields_it_cannot_establish() {
    let repo = Repo::new();
    let binary = assert_cmd::cargo_bin!("arc");
    let read_after_eval = |extra: &[(&str, &str)]| -> String {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(format!(
                "eval \"$('{}' env)\"; printf '%s\\n' \
                 \"ARC_HARNESS=${{ARC_HARNESS-<unset>}}\" \
                 \"ARC_SESSION=${{ARC_SESSION-<unset>}}\" \
                 \"ARC_MODEL=${{ARC_MODEL-<unset>}}\" \
                 \"ARC_SESSION_LINK=${{ARC_SESSION_LINK-<unset>}}\"",
                binary.display()
            ))
            .current_dir(&repo.root)
            .env("HOME", &repo.home)
            .env("ARC_SANDBOX", &repo.home)
            .envs(NO_EDITOR)
            .env("ARC_SESSION", "old-session")
            .env("ARC_MODEL", "old-model")
            .env("ARC_SESSION_LINK", "old-link")
            .env_remove("ARC_ACTOR")
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_ROLE")
            .env_remove("ARC_ON_BEHALF_OF")
            .env_remove("ARC_DATA_DIR")
            .env_remove("ARC_DATA_ROOT")
            .env_remove("ARC_WORKTREES_DIR")
            .env_remove("AI_HOME")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("OPENCODE_SESSION")
            .env_remove("OPENCODE_TERMINAL")
            .env_remove("PI_SESSION_ID")
            .env_remove("PI_SESSION_FILE")
            .env_remove("PI_MODEL")
            .env_remove("PI_REASONING_LEVEL");
        for (key, value) in extra {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    // A harness recognized without a session variable keeps its export and
    // clears the fields it cannot establish.
    assert_eq!(
        read_after_eval(&[("OPENCODE_TERMINAL", "1")]),
        "ARC_HARNESS=opencode\nARC_SESSION=<unset>\nARC_MODEL=<unset>\nARC_SESSION_LINK=<unset>\n"
    );
    // Nothing detected at all clears every field.
    assert_eq!(
        read_after_eval(&[]),
        "ARC_HARNESS=<unset>\nARC_SESSION=<unset>\nARC_MODEL=<unset>\nARC_SESSION_LINK=<unset>\n"
    );
}

/// A detected model belongs to the session detection resolved. Filling it
/// beside a different acting session would record one session's identity with
/// another's model.
#[test]
fn ambient_fill_pairs_a_model_only_with_the_session_it_answers_for() {
    let repo = Repo::new();
    enable_identity_detection(&repo);
    let detected = "019f0000-0000-7000-8000-000000000002";
    let day = repo.home.join(".codex/sessions/2026/07/24");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join(format!("rollout-2026-07-24T00-00-00-{detected}.jsonl")),
        concat!(
            "{\"type\":\"session_meta\",\"timestamp\":\"1\",",
            "\"payload\":{\"id\":\"019f0000-0000-7000-8000-000000000002\"}}\n",
            "{\"type\":\"turn_context\",\"timestamp\":\"2\",",
            "\"payload\":{\"model\":\"gpt-5.6-sol\",\"effort\":\"low\"}}\n",
        ),
    )
    .unwrap();

    let output = stdout(
        repo.arc(&repo.root)
            .args(["--session", "hand-set-session", "begin", "model-pairing"])
            .env_remove("ARC_ACTOR")
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_SESSION")
            .env_remove("ARC_MODEL")
            .env("CODEX_THREAD_ID", detected)
            .env("CODEX_HOME", repo.home.join(".codex")),
    );
    let event = opened_event(&repo, &opened_change_id(&output));
    assert_eq!(event["harness"], "codex", "{event}");
    assert_eq!(event["session"], "hand-set-session", "{event}");
    assert!(event["model"].is_null(), "{event}");
}

/// Pi's active conversation is the ancestry of the last entry, so a model on
/// an abandoned branch is not the session's model even when it is the last
/// one written.
#[test]
fn env_reports_the_active_pi_branch_s_model() {
    let repo = Repo::new();
    let session = "019f0000-0000-7000-8000-000000000001";
    let sessions = repo.home.join("pi-sessions/project");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join(format!("2026-07-18T12-07-52Z_{session}.jsonl")),
        concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"019f0000-0000-7000-8000-000000000001\",\"timestamp\":\"2026-07-18T12:07:52Z\",\"cwd\":\"/fixture\"}\n",
            "{\"type\":\"model_change\",\"id\":\"m1\",\"parentId\":null,\"timestamp\":\"2026-07-18T12:07:53Z\",\"provider\":\"p\",\"modelId\":\"pi-model-a\"}\n",
            "{\"type\":\"thinking_level_change\",\"id\":\"t1\",\"parentId\":\"m1\",\"timestamp\":\"2026-07-18T12:07:54Z\",\"thinkingLevel\":\"low\"}\n",
            "{\"type\":\"message\",\"id\":\"u1\",\"parentId\":\"t1\",\"timestamp\":\"2026-07-18T12:07:55Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"work\"}]}}\n",
            "{\"type\":\"model_change\",\"id\":\"m2\",\"parentId\":\"m1\",\"timestamp\":\"2026-07-18T12:07:56Z\",\"provider\":\"p\",\"modelId\":\"pi-abandoned\"}\n",
            "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"timestamp\":\"2026-07-18T12:07:57Z\",\"message\":{\"role\":\"assistant\",\"provider\":\"p\",\"model\":\"pi-model-a\",\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]}}\n",
        ),
    )
    .unwrap();

    repo.arc(&repo.root)
        .arg("env")
        .env("PI_SESSION_ID", session)
        .env("PI_CODING_AGENT_SESSION_DIR", repo.home.join("pi-sessions"))
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("OPENCODE_SESSION")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='pi' ARC_SESSION='{session}' ARC_MODEL='pi-model-a#low'\n{}",
            corroborated("pi")
        )));
}

/// A listed row without a readable transcript establishes neither a model nor
/// a corroborated session identity.
#[test]
fn env_marks_an_unreadable_opencode_recording_unresolved() {
    let repo = Repo::new();
    let session = "ses_test123";
    let data_home = repo.home.join("data");
    let store = data_home.join("opencode/opencode.db");
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    fs::write(&store, b"SQLite format 3\0").unwrap();
    let bin = repo.home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let sqlite = bin.join("sqlite3");
    fs::write(
        &sqlite,
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' 'row'\n",
            "printf '%s\\n' '{\"id\":\"ses_test123\",\"model\":null}'\n",
        ),
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
        .stdout(env_output(format!(
            concat!(
                "export ARC_HARNESS='opencode' ARC_SESSION='{session}'\n",
                "unset ARC_MODEL\n",
                "# export ARC_MODEL=<model[#effort]>  # unavailable: the session store ",
                "could not resolve or read this id\n",
                "{}"
            ),
            unresolved("opencode"),
            session = session
        )));
}

/// An effort belongs to the turn that wrote it: a later `turn_context` that
/// changes model without one does not inherit the earlier effort.
#[test]
fn env_does_not_carry_a_codex_effort_across_a_model_change() {
    let repo = Repo::new();
    let session = "019f0000-0000-7000-8000-000000000002";
    let codex_home = repo.home.join("codex-state");
    let day = codex_home.join("sessions/2026/07/20");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join(format!("rollout-2026-07-20T00-00-00-{session}.jsonl")),
        concat!(
            "{\"type\":\"session_meta\",\"timestamp\":\"1\",",
            "\"payload\":{\"id\":\"019f0000-0000-7000-8000-000000000002\"}}\n",
            "{\"type\":\"turn_context\",\"timestamp\":\"2\",",
            "\"payload\":{\"model\":\"gpt-5.5-a\",\"effort\":\"high\"}}\n",
            "{\"type\":\"turn_context\",\"timestamp\":\"3\",",
            "\"payload\":{\"model\":\"gpt-5.5-b\"}}\n",
        ),
    )
    .unwrap();

    repo.arc(&repo.root)
        .arg("env")
        .env_remove("CLAUDE_SESSION_ID")
        .env("CODEX_THREAD_ID", session)
        .env("CODEX_HOME", &codex_home)
        .env_remove("OPENCODE_SESSION")
        .env_remove("PI_SESSION_ID")
        .assert()
        .success()
        .stdout(env_output(format!(
            "export ARC_HARNESS='codex' ARC_SESSION='{session}' ARC_MODEL='gpt-5.5-b'\n{}",
            corroborated("codex")
        )));
}

/// The guide and help are the command contract: a stale command or a stale
/// exit-status claim sends a cold session down a path the CLI does not accept.
#[test]
fn guide_and_help_teach_current_debt_and_partial_opencode_identity() {
    let repo = Repo::new();
    let normalize = |text: String| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let integrate = normalize(stdout(repo.arc(&repo.root).args(["integrate", "--help"])));
    assert!(integrate.contains("--debt <REASON>"), "{integrate}");
    assert!(integrate.contains("arc query --debt"), "{integrate}");
    assert!(!integrate.contains("audit-debt"), "{integrate}");

    let guide = normalize(stdout(&mut repo.arc(&repo.root)));
    assert!(!guide.contains("audit-debt"), "{guide}");
    assert!(
        guide.contains("OpenCode v2 (`opencode2`) is recognized without one"),
        "{guide}"
    );
    assert!(
        guide.contains("`arc env` exits 1 and prints the export template"),
        "{guide}"
    );
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
        .stdout(env_output(format!(
            "export ARC_HARNESS='codex' ARC_SESSION='no-such-thread'\nunset ARC_MODEL\n{}",
            uncorroborated("codex")
        )));

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

#[test]
fn ambient_identity_detection_is_off_without_config() {
    let repo = Repo::new();
    let output = stdout(
        repo.arc(&repo.root)
            .args(["begin", "detect-off"])
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_SESSION")
            .env_remove("ARC_MODEL")
            .env_remove("CLAUDE_SESSION_ID")
            .env("CODEX_THREAD_ID", "ambient-thread")
            .env_remove("OPENCODE_SESSION")
            .env_remove("PI_SESSION_ID"),
    );
    let event = opened_event(&repo, &opened_change_id(&output));

    assert!(event["harness"].is_null());
    assert!(event["session"].is_null());
}

#[test]
fn ambient_identity_detection_fills_harness_session_and_model() {
    let repo = Repo::new();
    enable_identity_detection(&repo);
    let session = "019f0000-0000-7000-8000-000000000002";
    let day = repo.home.join(".codex/sessions/2026/07/24");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join(format!("rollout-2026-07-24T00-00-00-{session}.jsonl")),
        concat!(
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-sol\",",
            "\"effort\":\"low\"}}\n",
        ),
    )
    .unwrap();

    repo.arc(&repo.root)
        .args(["journal", "log", "detect-on", "event"])
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION")
        .env_remove("ARC_MODEL")
        .env_remove("CLAUDE_SESSION_ID")
        .env("CODEX_THREAD_ID", session)
        .env("CODEX_HOME", repo.home.join(".codex"))
        .env_remove("OPENCODE_SESSION")
        .env_remove("PI_SESSION_ID")
        .assert()
        .success();
    let event = recorded_journal_event(&repo);

    assert_eq!(event["harness"], "codex");
    assert_eq!(event["session"], session);
    assert_eq!(event["model"], "gpt-5.6-sol#low");
}

#[test]
fn explicit_environment_identity_wins_over_detection() {
    let repo = Repo::new();
    enable_identity_detection(&repo);
    let output = stdout(
        repo.arc(&repo.root)
            .args(["begin", "explicit-identity"])
            .env("ARC_HARNESS", "explicit")
            .env("ARC_SESSION", "explicit-session")
            .env_remove("ARC_MODEL")
            .env_remove("CLAUDE_SESSION_ID")
            .env("CODEX_THREAD_ID", "ambient-thread")
            .env_remove("OPENCODE_SESSION")
            .env_remove("PI_SESSION_ID"),
    );
    let event = opened_event(&repo, &opened_change_id(&output));

    assert_eq!(event["harness"], "explicit");
    assert_eq!(event["session"], "explicit-session");
}

#[test]
fn differing_explicit_harness_suppresses_detected_session() {
    let repo = Repo::new();
    enable_identity_detection(&repo);
    let output = stdout(
        repo.arc(&repo.root)
            .args(["--harness", "explicit", "begin", "different-harness"])
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_SESSION")
            .env_remove("ARC_MODEL")
            .env_remove("CLAUDE_SESSION_ID")
            .env("CODEX_THREAD_ID", "ambient-thread")
            .env_remove("OPENCODE_SESSION")
            .env_remove("PI_SESSION_ID"),
    );
    let event = opened_event(&repo, &opened_change_id(&output));

    assert_eq!(event["harness"], "explicit");
    assert!(event["session"].is_null());
}

/// Bare `arc` is the orientation surface: it replaces a separate workflow
/// document, so the guide must carry the lifecycle, the profiles, and the
/// rules that change what a session does — not just a command list.
#[test]
fn bare_arc_prints_the_workflow_guide() {
    let repo = Repo::new();
    let guide = stdout(&mut repo.arc(&repo.root));
    let flat = guide.split_whitespace().collect::<Vec<_>>().join(" ");
    for expected in [
        "An event's effective author is its `--on-behalf-of` subject when set, otherwise its actor.",
        "A patchset's effective contributors are its recorded set when nonempty, otherwise its effective author alone.",
        "`check` reports `iterating` and suppresses `no-valid-approval`; every other blocker still applies.",
        "baseline evidence fails at that brief's base and final evidence passes at the patchset's head",
        "the newest run for each phase at its required revision decides",
        "A brief with no base, or with a base equal to the patchset head, cannot discharge a probe.",
        "one passing verification event per required gate, each prerequisite's satisfying closure, the empty blocking-finding and hold vectors",
        "A debt declaration is recorded in the basis only when its waiver supplied the approval or let it stand.",
        "if readiness fails or the basis differs, nothing is written",
        "any passing evidence for that gate at the counted tree (or revision when the tree is unresolved) names a falsification",
        "A later pass without one does not retract it.",
    ] {
        assert!(flat.contains(expected), "guide missing {expected:?}:\n{guide}");
    }
    for expected in [
        "arc catchup",
        "arc fork <slug>",
        "unintegrated by",
        "arc journal open",
        "arc journal verified <file> [--note <text>]",
        "SETTLE A QUESTION",
        "arc journal note <topic> --kind discussion --body-file -",
        "arc journal position <file> --body-file -",
        "--stance <for|against|amend>",
        "arc journal question <file> --placement opening|closing --option <a> --option <b> --body-file -",
        "arc journal answer <file> --question <id> --option <choice> --body-file -",
        "arc journal discussion",
        "arc journal consume <file> --outcome done --decision <decision>",
        "arc begin <slug> --from-journal <file>",
        "Resolve a discussion as done, or promote the still-open discussion to work",
        "never both",
        "position <file> --body-file - --question <id> --option <opt>",
        "RUN A CHANGE",
        "PROFILES",
        "direct",
        "release",
        "binds to the exact approved patchset head",
        "arc config --check-writable",
        "arc watch <change> --until stalled",
        "arc holds no routing opinion",
    ] {
        assert!(
            guide.contains(expected),
            "guide missing {expected:?}:\n{guide}"
        );
    }
}

#[test]
fn help_points_at_the_guide_and_at_live_state() {
    let repo = Repo::new();
    let help = stdout(repo.arc(&repo.root).arg("--help"));
    assert!(
        help.contains("no arguments for the workflow guide"),
        "{help}"
    );
    assert!(help.contains("arc catchup"), "{help}");
}

/// Making the subcommand optional must not turn a mistyped invocation into a
/// silent success: only the genuinely empty argument list is a guide request.
#[test]
fn optional_subcommand_still_rejects_usage_errors() {
    let repo = Repo::new();
    repo.arc(&repo.root).assert().code(0);
    repo.arc(&repo.root).arg("nosuchcommand").assert().code(2);
    repo.arc(&repo.root).arg("--nosuchflag").assert().code(2);
    repo.arc(&repo.root)
        .args(["inbox", "--nosuchflag"])
        .assert()
        .code(2);
}

#[test]
fn inference_failure_names_the_open_changes_and_their_worktrees() {
    let repo = Repo::new();
    let output = stdout(repo.arc(&repo.root).args(["begin", "elsewhere"]));
    let change_id = opened_change_id(&output);

    repo.arc(&repo.root)
        .args(["status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "name one of these, or cd to its worktree",
        ))
        .stderr(predicate::str::contains(change_id))
        .stderr(predicate::str::contains(".worktrees/repo-elsewhere"));
}

#[test]
fn inference_failure_with_nothing_open_points_at_the_backlog() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("arc catchup"));
}

/// Shell exports remain exact while observation comments have their own fixtures.
fn env_output(expected: String) -> impl predicates::Predicate<str> {
    predicates::function::function(move |actual: &str| without_observation(actual) == expected)
}

fn without_observation(actual: &str) -> String {
    actual
        .lines()
        .filter(|line| !line.starts_with("# observed:"))
        .map(|line| format!("{line}\n"))
        .collect()
}
