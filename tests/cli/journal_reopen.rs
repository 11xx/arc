use crate::common::*;

fn journal_dir(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim())
}

fn global_journal_dir(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "--global", "dir"])).trim())
}

fn write_artifact(repo: &Repo, topic: &str, kind: &str, body: &str) -> String {
    let out = stdout(
        repo.arc(&repo.root)
            .args(["journal", "note", topic, "--kind", kind, "--body-file", "-"])
            .write_stdin(body),
    );
    Path::new(out.lines().last().unwrap().trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn consume_artifact(repo: &Repo, file: &str, outcome: &str) {
    repo.arc(&repo.root)
        .args(["journal", "consume", file, "--outcome", outcome])
        .assert()
        .success();
}

fn events_in(dir: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn output_text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn doctor_json(repo: &Repo) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["journal", "doctor", "--json"]))
}

/// Claim: reopening consumed artifacts is append-only under the event lock and
/// returns actionable items, memories, decisions, and discussions to their
/// pre-consumption state across all journal views.
#[test]
fn reopen_single_and_multi_file_restores_all_views_append_only() {
    let repo = Repo::new();
    let todo = write_artifact(&repo, "fix-cache", "todo", "# Fix cache\n\nNeeds fix.\n");
    let memory = write_artifact(
        &repo,
        "keep-fact",
        "memory",
        "# Keep fact\n\nDurable rule.\n",
    );
    let decision = write_artifact(
        &repo,
        "choose-path",
        "decision",
        "# Choose path\n\nWe chose A.\n",
    );
    let discussion = write_artifact(
        &repo,
        "debate-topic",
        "discussion",
        "# Debate topic\n\nOpen question.\n",
    );

    consume_artifact(&repo, &todo, "done");
    consume_artifact(&repo, &memory, "discarded");
    consume_artifact(&repo, &decision, "superseded");
    consume_artifact(&repo, &discussion, "done");

    let open_before = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert!(open_before["open"].as_array().unwrap().is_empty());

    let disc_before =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &discussion, "--json"]),
        );
    assert_eq!(disc_before["resolution"]["outcome"], "done");

    let log_path = journal_dir(&repo).join("events.jsonl");
    let log_before = fs::read_to_string(&log_path).unwrap();

    // Reopen a single artifact and confirm append-only preservation.
    let single_out = stdout(repo.arc(&repo.root).args([
        "journal",
        "reopen",
        &todo,
        "--reason",
        "closed by wrong list",
    ]));
    assert!(
        single_out.contains(&format!("reopened: {todo} [was done]")),
        "{single_out}"
    );
    let log_after_single = fs::read_to_string(&log_path).unwrap();
    assert!(
        log_after_single.starts_with(&log_before),
        "events.jsonl must be strictly append-only"
    );
    let appended: serde_json::Value =
        serde_json::from_str(log_after_single[log_before.len()..].trim()).unwrap();
    assert_eq!(appended["schema"], "journal-events/2");
    assert_eq!(appended["event"], "reopened");
    assert_eq!(appended["file"], todo);
    assert_eq!(appended["note"], "closed by wrong list");

    // Reopened todo returns to open, list, show, catchup, filed, and accepts evidence.
    let open_after = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert!(open_after["open"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["file"] == todo));

    let list_after = json_stdout(repo.arc(&repo.root).args(["journal", "list", "--json"]));
    let todo_row = list_after["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["file"] == todo)
        .unwrap();
    assert!(todo_row["consumed"].is_null(), "{todo_row}");

    let shown = stdout(repo.arc(&repo.root).args(["journal", "show", &todo]));
    assert!(shown.contains("Needs fix."), "{shown}");

    let catchup = json_stdout(repo.arc(&repo.root).args(["journal", "catchup", "--json"]));
    let catchup_todo = catchup["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["file"] == todo)
        .unwrap();
    assert!(catchup_todo["resolution"].is_null(), "{catchup_todo}");
    assert_eq!(catchup_todo["availability"], "available", "{catchup_todo}");
    assert!(catchup["journal_tail"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap_or_default().contains("reopened")));

    let filed = json_stdout(repo.arc(&repo.root).args(["journal", "filed", "--json"]));
    let filed_todo = filed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["items"].as_array().unwrap())
        .find(|item| item["file"] == todo)
        .unwrap();
    assert_eq!(filed_todo["fate"], "open", "{filed_todo}");

    repo.arc(&repo.root)
        .args(["journal", "evidence", &todo, "--body-file", "-"])
        .write_stdin("Fresh note after reopen.\n")
        .assert()
        .success();

    // Multi-file reopen restores memory, decision, and discussion together.
    let multi_out = stdout(repo.arc(&repo.root).args([
        "journal",
        "reopen",
        &memory,
        &decision,
        &discussion,
        "--reason",
        "undo mistaken bulk consume",
    ]));
    assert!(multi_out.contains(&format!("reopened: {memory} [was discarded]")));
    assert!(multi_out.contains(&format!("reopened: {decision} [was superseded]")));
    assert!(multi_out.contains(&format!("reopened: {discussion} [was done]")));

    let memories = json_stdout(repo.arc(&repo.root).args(["journal", "memories", "--json"]));
    assert!(memories["memories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["file"] == memory));

    let top_catchup = json_stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
    assert!(top_catchup["journal"]["memories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["file"] == memory));

    let disc_after =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &discussion, "--json"]),
        );
    assert!(disc_after["resolution"].is_null(), "{disc_after}");
    assert_eq!(
        disc_after["resolution_status"], "unresolved",
        "{disc_after}"
    );
    assert_eq!(disc_after["resolution_basis"], "explicit", "{disc_after}");
    repo.arc(&repo.root)
        .args([
            "journal",
            "position",
            &discussion,
            "--stance",
            "for",
            "--body-file",
            "-",
        ])
        .write_stdin("Reopened argument.\n")
        .assert()
        .success();

    let doctor = doctor_json(&repo);
    assert!(
        doctor["problems"].as_array().unwrap().is_empty(),
        "{doctor}"
    );
}

/// Claim: repeated consume -> reopen -> consume -> reopen -> consume cycles
/// preserve full history and derive the latest standing state in every view.
#[test]
fn reopen_supports_repeated_consume_reopen_consume_cycles() {
    let repo = Repo::new();
    let todo = write_artifact(&repo, "cycled", "todo", "# Cycled\n\nWork.\n");

    consume_artifact(&repo, &todo, "done");
    repo.arc(&repo.root)
        .args(["journal", "reopen", &todo, "--reason", "first reversal"])
        .assert()
        .success();

    consume_artifact(&repo, &todo, "discarded");
    let list_mid = json_stdout(repo.arc(&repo.root).args(["journal", "list", "--json"]));
    let mid_row = list_mid["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["file"] == todo)
        .unwrap();
    assert_eq!(mid_row["consumed"], "discarded", "{mid_row}");

    let inv_mid = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "inventory", &todo, "--json"]),
    );
    assert_eq!(inv_mid["items"][0]["resolution"], "discarded", "{inv_mid}");

    repo.arc(&repo.root)
        .args(["journal", "reopen", &todo, "--reason", "second reversal"])
        .assert()
        .success();

    let open_mid = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert!(open_mid["open"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["file"] == todo));

    consume_artifact(&repo, &todo, "superseded");
    let filed = json_stdout(repo.arc(&repo.root).args(["journal", "filed", "--json"]));
    let filed_row = filed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["items"].as_array().unwrap())
        .find(|item| item["file"] == todo)
        .unwrap();
    assert_eq!(filed_row["fate"], "superseded", "{filed_row}");

    let history = events_in(&journal_dir(&repo));
    let kinds: Vec<&str> = history
        .iter()
        .filter(|event| event["file"] == todo)
        .map(|event| event["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec!["note", "consumed", "reopened", "consumed", "reopened", "consumed"]
    );

    let doctor = doctor_json(&repo);
    assert!(
        doctor["problems"].as_array().unwrap().is_empty(),
        "{doctor}"
    );
}

/// Claim: explicit reopen validates `--reason`, mode combinations, and
/// non-consumed states atomically without writing partial events.
#[test]
fn reopen_validates_arguments_and_refuses_non_consumed_states_atomically() {
    let repo = Repo::new();
    let open_todo = write_artifact(&repo, "still-open", "todo", "# Still open\n");
    let live_decision = write_artifact(&repo, "live-dec", "decision", "# Live decision\n");
    let consumed_todo = write_artifact(&repo, "was-closed", "todo", "# Was closed\n");
    let shelved_disc = write_artifact(&repo, "shelved-disc", "discussion", "# Shelved\n");
    let relocated_todo = write_artifact(&repo, "moved-out", "todo", "# Moved\n");

    consume_artifact(&repo, &consumed_todo, "done");
    repo.arc(&repo.root)
        .args([
            "journal",
            "archive",
            &shelved_disc,
            "--unresolved",
            "--note",
            "parking for later",
        ])
        .assert()
        .success();
    let outside = repo.home.join("outside-relocate");
    repo.arc(&repo.root)
        .args([
            "journal",
            "relocate",
            &relocated_todo,
            "--to",
            outside.to_str().unwrap(),
            "--reason",
            "moved away",
        ])
        .assert()
        .success();

    let log_before = fs::read_to_string(journal_dir(&repo).join("events.jsonl")).unwrap();

    // Empty / whitespace reason refuses.
    for empty in ["", "   "] {
        let out = repo
            .arc(&repo.root)
            .args(["journal", "reopen", &consumed_todo, "--reason", empty])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(output_text(&out).contains("--reason must not be empty"));
    }

    // Missing selector refuses.
    let no_selector = repo
        .arc(&repo.root)
        .args(["journal", "reopen", "--reason", "why"])
        .output()
        .unwrap();
    assert!(!no_selector.status.success());

    // Mixing explicit file and session-window refuses.
    let mixed = repo
        .arc(&repo.root)
        .args([
            "journal",
            "reopen",
            &consumed_todo,
            "--session",
            "s1",
            "--since",
            "2026-01-01T00:00:00Z",
            "--reason",
            "why",
        ])
        .output()
        .unwrap();
    assert!(!mixed.status.success());
    assert!(output_text(&mixed).contains("not both"));

    // Incomplete / invalid window flags refuse.
    let no_since = repo
        .arc(&repo.root)
        .args(["journal", "reopen", "--session", "s1", "--reason", "why"])
        .output()
        .unwrap();
    assert!(!no_since.status.success());
    assert!(output_text(&no_since).contains("--since"));

    let no_session = repo
        .arc(&repo.root)
        .args([
            "journal",
            "reopen",
            "--since",
            "2026-01-01T00:00:00Z",
            "--reason",
            "why",
        ])
        .output()
        .unwrap();
    assert!(!no_session.status.success());
    assert!(output_text(&no_session).contains("--session"));

    let inverted = repo
        .arc(&repo.root)
        .args([
            "journal",
            "reopen",
            "--session",
            "s1",
            "--since",
            "2026-10-29T12:00:00Z",
            "--until",
            "2026-10-29T11:00:00Z",
            "--reason",
            "why",
        ])
        .output()
        .unwrap();
    assert!(!inverted.status.success());
    assert!(output_text(&inverted).contains("earlier than --since"));

    // Non-consumed artifacts refuse naming their current state.
    let err_open = repo
        .arc(&repo.root)
        .args(["journal", "reopen", &open_todo, "--reason", "why"])
        .output()
        .unwrap();
    assert!(!err_open.status.success());
    let msg_open = output_text(&err_open);
    assert!(
        msg_open.contains("not consumed") && msg_open.contains("open"),
        "{msg_open}"
    );

    let err_live = repo
        .arc(&repo.root)
        .args(["journal", "reopen", &live_decision, "--reason", "why"])
        .output()
        .unwrap();
    assert!(!err_live.status.success());
    let msg_live = output_text(&err_live);
    assert!(
        msg_live.contains("not consumed") && msg_live.contains("live"),
        "{msg_live}"
    );

    let err_shelved = repo
        .arc(&repo.root)
        .args(["journal", "reopen", &shelved_disc, "--reason", "why"])
        .output()
        .unwrap();
    assert!(!err_shelved.status.success());
    let msg_shelved = output_text(&err_shelved);
    assert!(
        msg_shelved.contains("not consumed") && msg_shelved.contains("archived, unresolved"),
        "{msg_shelved}"
    );

    let err_relocated = repo
        .arc(&repo.root)
        .args(["journal", "reopen", &relocated_todo, "--reason", "why"])
        .output()
        .unwrap();
    assert!(!err_relocated.status.success());
    assert!(output_text(&err_relocated).contains("relocated"));

    // Multi-file refusal is atomic: consumed_todo is NOT reopened when open_todo fails.
    let err_batch = repo
        .arc(&repo.root)
        .args([
            "journal",
            "reopen",
            &consumed_todo,
            &open_todo,
            "--reason",
            "atomic check",
        ])
        .output()
        .unwrap();
    assert!(!err_batch.status.success());
    assert_eq!(
        fs::read_to_string(journal_dir(&repo).join("events.jsonl")).unwrap(),
        log_before,
        "failed multi-file reopen must write nothing"
    );

    // Reopening an already-reopened artifact refuses naming that state.
    repo.arc(&repo.root)
        .args(["journal", "reopen", &consumed_todo, "--reason", "undo"])
        .assert()
        .success();
    let err_twice = repo
        .arc(&repo.root)
        .args(["journal", "reopen", &consumed_todo, "--reason", "again"])
        .output()
        .unwrap();
    assert!(!err_twice.status.success());
    let msg_twice = output_text(&err_twice);
    assert!(
        msg_twice.contains("not consumed") && msg_twice.contains("already reopened"),
        "{msg_twice}"
    );
}

/// Claim: `--dry-run` reports matched artifacts without writing, and reopening
/// a consumed archived artifact clears its consumption so `unarchive` restores
/// it to the live open queue.
#[test]
fn reopen_dry_run_and_archived_artifact_restoration() {
    let repo = Repo::new();
    let todo = write_artifact(&repo, "cold-item", "todo", "# Cold item\n\nBody.\n");
    consume_artifact(&repo, &todo, "done");
    repo.arc(&repo.root)
        .args(["journal", "archive", "--consumed"])
        .assert()
        .success();

    let log_path = journal_dir(&repo).join("events.jsonl");
    let before = fs::read(&log_path).unwrap();

    let dry = stdout(repo.arc(&repo.root).args([
        "journal",
        "reopen",
        &todo,
        "--reason",
        "needed back",
        "--dry-run",
    ]));
    assert!(
        dry.contains(&format!("would reopen: {todo} [was done] (cold archive)")),
        "{dry}"
    );
    assert!(dry.contains("nothing written"), "{dry}");
    assert_eq!(fs::read(&log_path).unwrap(), before);

    let out = repo
        .arc(&repo.root)
        .args(["journal", "reopen", &todo, "--reason", "needed back"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", output_text(&out));
    assert!(String::from_utf8_lossy(&out.stdout)
        .contains(&format!("reopened: {todo} [was done] (cold archive)")));
    assert!(String::from_utf8_lossy(&out.stderr).contains("journal unarchive"));

    repo.arc(&repo.root)
        .args(["journal", "unarchive", &todo])
        .assert()
        .success();
    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert!(open["open"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["file"] == todo));

    let doctor = doctor_json(&repo);
    assert!(
        doctor["problems"].as_array().unwrap().is_empty(),
        "{doctor}"
    );
}

/// Claim: session-window batch reopen matches only currently consumed artifacts
/// whose standing consumption belongs to `--session` inside `[--since, --until]`,
/// previews them under `--dry-run`, and attributes the `reopened` events to the
/// acting session (`ARC_SESSION`) rather than the filtered session.
#[test]
fn reopen_session_window_batch_and_acting_session_attribution() {
    let repo = Repo::new();
    let before_win = write_artifact(&repo, "before-win", "todo", "# Before\n");
    let match_todo = write_artifact(&repo, "match-todo", "todo", "# Match todo\n");
    let match_mem = write_artifact(&repo, "match-mem", "memory", "# Match memory\n");
    let other_sess = write_artifact(&repo, "other-sess", "todo", "# Other session\n");
    let after_win = write_artifact(&repo, "after-win", "todo", "# After\n");
    let re_consumed = write_artifact(&repo, "re-consumed", "todo", "# Re-consumed\n");
    let already_open = write_artifact(&repo, "already-open", "todo", "# Already open\n");

    // Append deterministic consumption events at known timestamps.
    let dir = journal_dir(&repo);
    let log_path = dir.join("events.jsonl");
    let mut log = fs::read_to_string(&log_path).unwrap();
    let rows = [
        serde_json::json!({"schema":"journal-events/2","ts":"2026-10-29T09:59:59Z","harness":"codex","session":"bad-session","topic":"before-win","event":"consumed","file":before_win,"outcome":"done"}),
        serde_json::json!({"schema":"journal-events/2","ts":"2026-10-29T10:00:00Z","harness":"codex","session":"bad-session","topic":"match-todo","event":"consumed","file":match_todo,"outcome":"done"}),
        serde_json::json!({"schema":"journal-events/2","ts":"2026-10-29T10:05:00Z","harness":"codex","session":"other-session","topic":"other-sess","event":"consumed","file":other_sess,"outcome":"done"}),
        serde_json::json!({"schema":"journal-events/2","ts":"2026-10-29T10:15:00Z","harness":"codex","session":"bad-session","topic":"match-mem","event":"consumed","file":match_mem,"outcome":"discarded"}),
        serde_json::json!({"schema":"journal-events/2","ts":"2026-10-29T10:20:00Z","harness":"codex","session":"bad-session","topic":"already-open","event":"consumed","file":already_open,"outcome":"done"}),
        serde_json::json!({"schema":"journal-events/2","ts":"2026-10-29T10:21:00Z","harness":"codex","session":"earlier-fix","topic":"already-open","event":"reopened","file":already_open,"note":"already fixed"}),
        serde_json::json!({"schema":"journal-events/2","ts":"2026-10-29T10:22:00Z","harness":"codex","session":"bad-session","topic":"re-consumed","event":"consumed","file":re_consumed,"outcome":"done"}),
        serde_json::json!({"schema":"journal-events/2","ts":"2026-10-29T10:23:00Z","harness":"codex","session":"earlier-fix","topic":"re-consumed","event":"reopened","file":re_consumed,"note":"fixed once"}),
        serde_json::json!({"schema":"journal-events/2","ts":"2026-10-29T10:24:00Z","harness":"codex","session":"good-session","topic":"re-consumed","event":"consumed","file":re_consumed,"outcome":"done"}),
        serde_json::json!({"schema":"journal-events/2","ts":"2026-10-29T10:30:01Z","harness":"codex","session":"bad-session","topic":"after-win","event":"consumed","file":after_win,"outcome":"done"}),
    ];
    for row in rows {
        log.push_str(&serde_json::to_string(&row).unwrap());
        log.push('\n');
    }
    fs::write(&log_path, &log).unwrap();

    // Dry run under ARC_SESSION=repair-session lists only match_todo and match_mem.
    let dry = stdout(
        repo.arc(&repo.root)
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", "repair-session")
            .args([
                "journal",
                "reopen",
                "--session",
                "bad-session",
                "--since",
                "2026-10-29T10:00:00Z",
                "--until",
                "2026-10-29T10:30:00Z",
                "--reason",
                "bulk consume used wrong list",
                "--dry-run",
            ]),
    );
    assert!(
        dry.contains(&format!("would reopen: {match_todo} [was done]")),
        "{dry}"
    );
    assert!(
        dry.contains(&format!("would reopen: {match_mem} [was discarded]")),
        "{dry}"
    );
    for excluded in [
        &before_win,
        &other_sess,
        &after_win,
        &re_consumed,
        &already_open,
    ] {
        assert!(
            !dry.contains(excluded),
            "{excluded} should not match:\n{dry}"
        );
    }
    assert!(dry.contains("would reopen 2 artifact(s)"), "{dry}");
    assert_eq!(fs::read_to_string(&log_path).unwrap(), log);

    // Execute batch reopen and verify acting session is repair-session, not bad-session.
    let run_out = stdout(
        repo.arc(&repo.root)
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", "repair-session")
            .args([
                "journal",
                "reopen",
                "--session",
                "bad-session",
                "--since",
                "2026-10-29T10:00:00Z",
                "--until",
                "2026-10-29T10:30:00Z",
                "--reason",
                "bulk consume used wrong list",
            ]),
    );
    assert!(
        run_out.contains(&format!("reopened: {match_todo}")),
        "{run_out}"
    );
    assert!(
        run_out.contains(&format!("reopened: {match_mem}")),
        "{run_out}"
    );

    let all_events = events_in(&dir);
    let reopened: Vec<&serde_json::Value> = all_events
        .iter()
        .filter(|e| e["event"] == "reopened" && e["note"] == "bulk consume used wrong list")
        .collect();
    assert_eq!(reopened.len(), 2, "{reopened:?}");
    for event in reopened {
        assert_eq!(
            event["session"], "repair-session",
            "reopened event must record acting session, not filtered session: {event}"
        );
    }

    // Re-running the same window is a clean no-op.
    let repeat = stdout(
        repo.arc(&repo.root)
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", "repair-session")
            .args([
                "journal",
                "reopen",
                "--session",
                "bad-session",
                "--since",
                "2026-10-29T10:00:00Z",
                "--until",
                "2026-10-29T10:30:00Z",
                "--reason",
                "bulk consume used wrong list",
            ]),
    );
    assert!(
        repeat.contains("no currently consumed artifacts matched"),
        "{repeat}"
    );

    // Explicit file mode works with both ARC_SESSION env and global --session before `journal`.
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", "env-actor-session")
        .args([
            "journal",
            "reopen",
            &before_win,
            "--reason",
            "explicit with env session",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "--harness",
            "codex",
            "--session",
            "flag-actor-session",
            "journal",
            "reopen",
            &after_win,
            "--reason",
            "explicit with global flag session",
        ])
        .assert()
        .success();

    let after_events = events_in(&dir);
    let env_ev = after_events
        .iter()
        .find(|e| e["file"] == before_win && e["event"] == "reopened")
        .unwrap();
    assert_eq!(env_ev["session"], "env-actor-session");
    let flag_ev = after_events
        .iter()
        .find(|e| e["file"] == after_win && e["event"] == "reopened")
        .unwrap();
    assert_eq!(flag_ev["session"], "flag-actor-session");
}

/// Claim: `--global` reopens consumed artifacts in the operator journal.
#[test]
fn reopen_works_in_global_journal() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "--global",
                "memory",
                "op-rule",
                "--body-file",
                "-",
            ])
            .write_stdin("# Operator rule\n\nKeep forever.\n"),
    );
    let mem = Path::new(out.lines().last().unwrap().trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    repo.arc(&repo.root)
        .args(["journal", "--global", "consume", &mem, "--outcome", "done"])
        .assert()
        .success();
    let mems_before = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "--global", "memories", "--json"]),
    );
    assert!(mems_before["memories"].as_array().unwrap().is_empty());

    let dry = stdout(repo.arc(&repo.root).args([
        "journal",
        "reopen",
        "--global",
        &mem,
        "--reason",
        "still active",
        "--dry-run",
    ]));
    assert!(dry.contains(&format!("would reopen: {mem}")), "{dry}");

    repo.arc(&repo.root)
        .args([
            "journal",
            "--global",
            "reopen",
            &mem,
            "--reason",
            "still active",
        ])
        .assert()
        .success();

    let mems_after = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "--global", "memories", "--json"]),
    );
    assert!(mems_after["memories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["file"] == mem));

    let global_events = events_in(&global_journal_dir(&repo));
    assert!(global_events
        .iter()
        .any(|e| e["event"] == "reopened" && e["file"] == mem && e["note"] == "still active"));

    let doctor_out = repo
        .arc(&repo.root)
        .args(["journal", "--global", "doctor", "--json"])
        .output()
        .unwrap();
    let doctor: serde_json::Value = serde_json::from_slice(&doctor_out.stdout).unwrap();
    for problem in doctor["problems"].as_array().unwrap() {
        let code = problem["code"].as_str().unwrap_or_default();
        assert!(
            !matches!(
                code,
                "unknown-jsonl-event" | "malformed-jsonl" | "dangling-artifact-reference"
            ),
            "unexpected doctor problem for reopened global event: {doctor}"
        );
    }
}

/// Claim: journal exchange exports and imports `reopened` events cleanly across
/// paired replicas so a recipient converges on the reopened state.
#[test]
fn reopen_survives_journal_exchange_export_and_import() {
    let alpha = Repo::new();
    let beta = Repo::new();
    super::journal_exchange::pair(&alpha, &beta, "alpha", "beta");

    let todo = write_artifact(&alpha, "shared-work", "todo", "# Shared work\n\nBody.\n");
    consume_artifact(&alpha, &todo, "done");

    let bundle1 = alpha.home.join("bundle1.json");
    alpha
        .arc(&alpha.root)
        .args([
            "journal",
            "export",
            &todo,
            "--output",
            bundle1.to_str().unwrap(),
        ])
        .assert()
        .success();
    beta.arc(&beta.root)
        .args(["journal", "import", bundle1.to_str().unwrap()])
        .assert()
        .success();

    let beta_open_before = json_stdout(beta.arc(&beta.root).args(["journal", "open", "--json"]));
    assert!(beta_open_before["open"].as_array().unwrap().is_empty());

    // Reopen on alpha, export again, import on beta.
    alpha
        .arc(&alpha.root)
        .args(["journal", "reopen", &todo, "--reason", "reopened on alpha"])
        .assert()
        .success();
    let bundle2 = alpha.home.join("bundle2.json");
    alpha
        .arc(&alpha.root)
        .args([
            "journal",
            "export",
            &todo,
            "--output",
            bundle2.to_str().unwrap(),
        ])
        .assert()
        .success();
    beta.arc(&beta.root)
        .args(["journal", "import", bundle2.to_str().unwrap()])
        .assert()
        .success();

    let beta_open_after = json_stdout(beta.arc(&beta.root).args(["journal", "open", "--json"]));
    assert!(beta_open_after["open"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["file"] == todo));

    let beta_list = json_stdout(beta.arc(&beta.root).args(["journal", "list", "--json"]));
    let beta_row = beta_list["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["file"] == todo)
        .unwrap();
    assert!(beta_row["consumed"].is_null(), "{beta_row}");

    let beta_doctor = doctor_json(&beta);
    assert!(
        beta_doctor["problems"].as_array().unwrap().is_empty(),
        "{beta_doctor}"
    );
}

/// Claim: checkpoints observe reopened todos as open again, and `journal doctor`
/// flags a dangling `reopened` event whose artifact body is missing.
#[test]
fn reopen_updates_checkpoint_view_and_doctor_catches_dangling_reopened() {
    let repo = Repo::new();
    let todo = write_artifact(&repo, "tracked-todo", "todo", "# Tracked todo\n");
    repo.arc(&repo.root)
        .args(["checkpoint", "record", "cp1"])
        .assert()
        .success();

    consume_artifact(&repo, &todo, "done");
    let cp_consumed =
        json_stdout(
            repo.arc(&repo.root)
                .args(["checkpoint", "show", "cp1", "--json"]),
        );
    assert_eq!(
        cp_consumed["todos"][0]["status"], "consumed",
        "{cp_consumed}"
    );

    repo.arc(&repo.root)
        .args(["journal", "reopen", &todo, "--reason", "not actually done"])
        .assert()
        .success();
    let cp_reopened =
        json_stdout(
            repo.arc(&repo.root)
                .args(["checkpoint", "show", "cp1", "--json"]),
        );
    assert_eq!(cp_reopened["todos"][0]["status"], "open", "{cp_reopened}");

    // Dangling reopened event referencing a nonexistent artifact is reported by doctor.
    let log_path = journal_dir(&repo).join("events.jsonl");
    let mut log = fs::read_to_string(&log_path).unwrap();
    let dangling = serde_json::json!({
        "schema": "journal-events/2",
        "ts": "2026-10-29T12:00:00Z",
        "topic": "ghost",
        "event": "reopened",
        "file": "20261029T120000Z-ghost-todo.md",
        "note": "dangling check"
    });
    log.push_str(&serde_json::to_string(&dangling).unwrap());
    log.push('\n');
    fs::write(&log_path, log).unwrap();

    let out = repo
        .arc(&repo.root)
        .args(["journal", "doctor", "--json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(report["problems"].as_array().unwrap().iter().any(|p| {
        p["code"] == "dangling-artifact-reference"
            && p["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("reopened references 20261029T120000Z-ghost-todo.md")
    }));
}

/// Claim: structural handoff consumptions (`journal transition`,
/// `begin --from-journal`, `journal promote --global`) are refused in explicit
/// mode and skipped in session-window mode so source and successor/change never
/// become simultaneously live, while an ordinary `done` consumption reopens
/// and can be promoted.
#[test]
fn reopen_protects_handoff_consumptions_in_both_selectors() {
    let repo = Repo::new();
    let transitioned_todo = write_artifact(&repo, "trans-src", "todo", "# Transition source\n");
    let promoted_todo = write_artifact(&repo, "promo-src", "todo", "# Promotion source\n");
    let promoted_mem = write_artifact(&repo, "promo-mem", "memory", "# Promoted memory\n");
    let ordinary_todo = write_artifact(&repo, "ordinary-work", "todo", "# Ordinary work\n");

    let window_start = "2020-01-01T00:00:00Z";
    let sess = "handoff-session";

    // Transition todo -> plan.
    let trans_out = stdout(
        repo.arc(&repo.root)
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", sess)
            .args([
                "journal",
                "transition",
                &transitioned_todo,
                "--to",
                "plan",
                "--no-planner",
            ]),
    );
    assert!(trans_out.contains("transitioned:"), "{trans_out}");

    // Promote todo -> change via begin --from-journal.
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", sess)
        .args([
            "begin",
            "from-promo-todo",
            "--no-worktree",
            "--from-journal",
            &promoted_todo,
        ])
        .assert()
        .success();

    // Promote memory -> global journal.
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", sess)
        .args(["journal", "promote", &promoted_mem, "--global"])
        .assert()
        .success();

    // Ordinary manual done consumption by the same session.
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", sess)
        .args(["journal", "consume", &ordinary_todo, "--outcome", "done"])
        .assert()
        .success();

    let log_before = fs::read_to_string(journal_dir(&repo).join("events.jsonl")).unwrap();

    // Explicit mode refuses each handoff consumption and names its successor/change.
    let err_trans = repo
        .arc(&repo.root)
        .args(["journal", "reopen", &transitioned_todo, "--reason", "probe"])
        .output()
        .unwrap();
    assert!(!err_trans.status.success());
    let msg_trans = output_text(&err_trans);
    assert!(
        msg_trans.contains("structural handoff") && msg_trans.contains("transitioned to"),
        "{msg_trans}"
    );

    let err_promo = repo
        .arc(&repo.root)
        .args(["journal", "reopen", &promoted_todo, "--reason", "probe"])
        .output()
        .unwrap();
    assert!(!err_promo.status.success());
    let msg_promo = output_text(&err_promo);
    assert!(
        msg_promo.contains("structural handoff") && msg_promo.contains("promoted to change"),
        "{msg_promo}"
    );

    let err_mem = repo
        .arc(&repo.root)
        .args(["journal", "reopen", &promoted_mem, "--reason", "probe"])
        .output()
        .unwrap();
    assert!(!err_mem.status.success());
    let msg_mem = output_text(&err_mem);
    assert!(
        msg_mem.contains("structural handoff") && msg_mem.contains("promoted to global::"),
        "{msg_mem}"
    );

    assert_eq!(
        fs::read_to_string(journal_dir(&repo).join("events.jsonl")).unwrap(),
        log_before
    );

    // Session-window dry-run skips all three handoffs and selects only ordinary_todo.
    let dry = repo
        .arc(&repo.root)
        .args([
            "journal",
            "reopen",
            "--session",
            sess,
            "--since",
            window_start,
            "--reason",
            "undo mistaken consume",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(dry.status.success(), "{}", output_text(&dry));
    let dry_stdout = String::from_utf8_lossy(&dry.stdout);
    let dry_stderr = String::from_utf8_lossy(&dry.stderr);
    assert!(
        dry_stdout.contains(&format!("would reopen: {ordinary_todo}")),
        "{dry_stdout}"
    );
    assert!(
        dry_stdout.contains("would reopen 1 artifact(s)"),
        "{dry_stdout}"
    );
    for handoff in [&transitioned_todo, &promoted_todo, &promoted_mem] {
        assert!(
            !dry_stdout.contains(handoff),
            "{handoff} leaked into dry-run:\n{dry_stdout}"
        );
        assert!(
            dry_stderr.contains(&format!("skipped handoff: {handoff}")),
            "expected skipped handoff notice for {handoff}:\n{dry_stderr}"
        );
    }

    // Real session-window run reopens only ordinary_todo and keeps all handoffs closed.
    let batch = repo
        .arc(&repo.root)
        .args([
            "journal",
            "reopen",
            "--session",
            sess,
            "--since",
            window_start,
            "--reason",
            "undo mistaken consume",
        ])
        .output()
        .unwrap();
    assert!(batch.status.success(), "{}", output_text(&batch));
    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    let open_files: Vec<&str> = open["open"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["file"].as_str().unwrap())
        .collect();
    assert!(
        open_files.contains(&ordinary_todo.as_str()),
        "{open_files:?}"
    );
    assert!(
        !open_files.contains(&transitioned_todo.as_str()),
        "{open_files:?}"
    );
    assert!(
        !open_files.contains(&promoted_todo.as_str()),
        "{open_files:?}"
    );

    // Ordinary reopened todo can now be promoted via begin --from-journal.
    repo.arc(&repo.root)
        .args([
            "begin",
            "reopened-ordinary",
            "--no-worktree",
            "--target",
            "master",
            "--from-journal",
            &ordinary_todo,
        ])
        .assert()
        .success();
}

/// Claim: each `reopened` event binds to the exact `consumed` event selected
/// under the transition lock, so importing a concurrent reopen of an earlier
/// consumption from another replica never clears a distinct, later consumption.
#[test]
fn reopen_imported_from_peer_does_not_clear_later_distinct_consumption() {
    let alpha = Repo::new();
    let beta = Repo::new();
    super::journal_exchange::pair(&alpha, &beta, "alpha", "beta");

    let file = write_artifact(&alpha, "divergent-item", "todo", "# Divergent\n\nBody.\n");
    consume_artifact(&alpha, &file, "discarded");

    // Alpha exports the initial discarded consumption to beta.
    let bundle1 = alpha.home.join("b1.json");
    alpha
        .arc(&alpha.root)
        .args([
            "journal",
            "export",
            &file,
            "--output",
            bundle1.to_str().unwrap(),
        ])
        .assert()
        .success();
    beta.arc(&beta.root)
        .args(["journal", "import", bundle1.to_str().unwrap()])
        .assert()
        .success();

    // Both replicas independently reopen that same discarded consumption.
    alpha
        .arc(&alpha.root)
        .args(["journal", "reopen", &file, "--reason", "alpha undo"])
        .assert()
        .success();
    beta.arc(&beta.root)
        .args(["journal", "reopen", &file, "--reason", "beta undo"])
        .assert()
        .success();

    // Beta then records a new, distinct `done` consumption.
    consume_artifact(&beta, &file, "done");

    // Export alpha (carrying alpha's reopen of the earlier `discarded` event)
    // and import into beta.
    let bundle2 = alpha.home.join("b2.json");
    alpha
        .arc(&alpha.root)
        .args([
            "journal",
            "export",
            &file,
            "--output",
            bundle2.to_str().unwrap(),
        ])
        .assert()
        .success();
    beta.arc(&beta.root)
        .args(["journal", "import", bundle2.to_str().unwrap()])
        .assert()
        .success();

    // Beta's later `done` consumption MUST remain standing across every view.
    let beta_open = json_stdout(beta.arc(&beta.root).args(["journal", "open", "--json"]));
    assert!(
        beta_open["open"].as_array().unwrap().is_empty(),
        "imported reopen of earlier consumption must not reopen later done consumption: {beta_open}"
    );
    let beta_list = json_stdout(beta.arc(&beta.root).args(["journal", "list", "--json"]));
    let beta_row = beta_list["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["file"] == file)
        .unwrap();
    assert_eq!(beta_row["consumed"], "done", "{beta_row}");

    let beta_filed = json_stdout(beta.arc(&beta.root).args(["journal", "filed", "--json"]));
    let beta_filed_row = beta_filed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["items"].as_array().unwrap())
        .find(|item| item["file"] == file)
        .unwrap();
    assert_eq!(beta_filed_row["fate"], "done", "{beta_filed_row}");

    let beta_inv =
        json_stdout(
            beta.arc(&beta.root)
                .args(["journal", "inventory", &file, "--json"]),
        );
    assert_eq!(beta_inv["items"][0]["resolution"], "done", "{beta_inv}");

    let beta_doctor = doctor_json(&beta);
    assert!(
        beta_doctor["problems"].as_array().unwrap().is_empty(),
        "{beta_doctor}"
    );

    // Explicitly reopening beta's standing `done` consumption binds to the new
    // consumption ID and reopens the artifact cleanly.
    beta.arc(&beta.root)
        .args(["journal", "reopen", &file, "--reason", "reopen done too"])
        .assert()
        .success();
    let beta_open_after = json_stdout(beta.arc(&beta.root).args(["journal", "open", "--json"]));
    assert!(beta_open_after["open"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["file"] == file));

    let beta_events = events_in(&journal_dir(&beta));
    let reopen_targets: Vec<&str> = beta_events
        .iter()
        .filter(|e| e["event"] == "reopened" && e["file"] == file)
        .map(|e| e["reopens"].as_str().unwrap())
        .collect();
    assert_eq!(reopen_targets.len(), 3, "{reopen_targets:?}");
    // First two reopens (beta's first + alpha's imported) targeted C1; third targets C2.
    assert_eq!(reopen_targets[0], reopen_targets[1]);
    assert_ne!(reopen_targets[0], reopen_targets[2]);
}

/// Claim: explicit acting `--session` before `reopen` (attached or separated,
/// before `journal` or between `journal` and `reopen`) is preserved even when
/// `ARC_SESSION` is set and `--session` also appears after `reopen` as a batch
/// filter; and an option value equal to a subcommand name (`--actor reopen` or
/// `--actor journal`) is never mistaken for the subcommand.
#[test]
fn reopen_separates_explicit_acting_session_from_filter_and_option_values() {
    let repo = Repo::new();
    let item_attached = write_artifact(&repo, "item-attached", "todo", "# Attached\n");
    let item_separated = write_artifact(&repo, "item-separated", "todo", "# Separated\n");
    let item_mid = write_artifact(&repo, "item-mid", "todo", "# Mid\n");
    let item_actor_reopen = write_artifact(&repo, "item-actor-reopen", "todo", "# Actor reopen\n");
    let item_actor_journal =
        write_artifact(&repo, "item-actor-journal", "todo", "# Actor journal\n");

    let dir = journal_dir(&repo);
    let log_path = dir.join("events.jsonl");
    let mut log = fs::read_to_string(&log_path).unwrap();
    for (file, topic, target_sess) in [
        (&item_attached, "item-attached", "target-attached"),
        (&item_separated, "item-separated", "target-separated"),
        (&item_mid, "item-mid", "target-mid"),
        (&item_actor_reopen, "item-actor-reopen", "target-other"),
        (&item_actor_journal, "item-actor-journal", "target-other"),
    ] {
        let row = serde_json::json!({
            "schema": "journal-events/2",
            "ts": "2026-10-29T10:10:00Z",
            "harness": "codex",
            "session": target_sess,
            "topic": topic,
            "event": "consumed",
            "file": file,
            "outcome": "done"
        });
        log.push_str(&serde_json::to_string(&row).unwrap());
        log.push('\n');
    }
    fs::write(&log_path, &log).unwrap();

    // 1. Attached global --session before `journal` + attached filter --session after `reopen`
    //    with ARC_SESSION=ambient: event session MUST be `explicit-actor`.
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", "ambient")
        .args([
            "--session=explicit-actor",
            "journal",
            "reopen",
            "--session=target-attached",
            "--since",
            "2026-10-29T10:00:00Z",
            "--reason",
            "attached test",
        ])
        .assert()
        .success();

    // 2. Space-separated global --session before `journal` + separated filter --session.
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", "ambient")
        .args([
            "--session",
            "explicit-sep",
            "journal",
            "reopen",
            "--session",
            "target-separated",
            "--since",
            "2026-10-29T10:00:00Z",
            "--reason",
            "separated test",
        ])
        .assert()
        .success();

    // 3. Global --session positioned between `journal` and `reopen`.
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", "ambient")
        .args([
            "journal",
            "--session=explicit-mid",
            "reopen",
            "--session=target-mid",
            "--since",
            "2026-10-29T10:00:00Z",
            "--reason",
            "mid test",
        ])
        .assert()
        .success();

    // 4. Option value equal to subcommand name (`--actor reopen`) + `--session=explicit-actor`
    //    in filename mode (both dry-run and real run).
    let dry = stdout(
        repo.arc(&repo.root)
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", "ambient")
            .args([
                "--actor",
                "reopen",
                "--session=explicit-actor",
                "journal",
                "reopen",
                &item_actor_reopen,
                "--reason",
                "actor named reopen",
                "--dry-run",
            ]),
    );
    assert!(
        dry.contains(&format!("would reopen: {item_actor_reopen}")),
        "{dry}"
    );
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", "ambient")
        .args([
            "--actor",
            "reopen",
            "--session=explicit-actor",
            "journal",
            "reopen",
            &item_actor_reopen,
            "--reason",
            "actor named reopen",
        ])
        .assert()
        .success();

    // 5. Option value equal to parent subcommand name (`--actor journal`).
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", "ambient")
        .args([
            "--actor",
            "journal",
            "--session=explicit-actor-j",
            "journal",
            "reopen",
            &item_actor_journal,
            "--reason",
            "actor named journal",
        ])
        .assert()
        .success();

    let all = events_in(&dir);
    let find_reopen = |file: &str| {
        all.iter()
            .find(|e| e["event"] == "reopened" && e["file"] == file)
            .cloned()
            .unwrap()
    };
    assert_eq!(find_reopen(&item_attached)["session"], "explicit-actor");
    assert_eq!(find_reopen(&item_separated)["session"], "explicit-sep");
    assert_eq!(find_reopen(&item_mid)["session"], "explicit-mid");
    let ev_actor_reopen = find_reopen(&item_actor_reopen);
    assert_eq!(ev_actor_reopen["actor"], "reopen");
    assert_eq!(ev_actor_reopen["session"], "explicit-actor");
    let ev_actor_journal = find_reopen(&item_actor_journal);
    assert_eq!(ev_actor_journal["actor"], "journal");
    assert_eq!(ev_actor_journal["session"], "explicit-actor-j");
}
