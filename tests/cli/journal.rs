use super::common::*;

fn journal_slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

fn journal_dir(repo: &Repo) -> PathBuf {
    let out = stdout(repo.arc(&repo.root).args(["journal", "dir"]));
    PathBuf::from(out.trim())
}

fn journal_events(dir: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn journal_log_writes_typed_jsonl_events() {
    let repo = Repo::new();
    let body = repo.home.join("body.md");
    fs::write(&body, "work\n").unwrap();
    let output = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "typed",
        "--kind",
        "todo",
        "--body-file",
        body.to_str().unwrap(),
    ]));
    let file = PathBuf::from(output.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    repo.arc(&repo.root)
        .args(["journal", "log", "typed", "progress"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "journal",
            "consume",
            &file,
            "--outcome",
            "done",
            "--note",
            "finished",
        ])
        .assert()
        .success();
    let dir = journal_dir(&repo);
    let events = journal_events(&dir);
    assert_eq!(events.len(), 3);
    for event in &events {
        assert_eq!(event["schema"], "journal-events/1");
        assert_eq!(event["harness"], "test");
        assert_eq!(event["session"], "session-a");
        assert_eq!(event["topic"], "typed");
        assert!(event["ts"].as_str().unwrap().ends_with('Z'));
    }
    assert_eq!(events[0]["event"], "note");
    assert_eq!(events[0]["file"], file);
    assert_eq!(events[1]["event"], "log");
    assert_eq!(events[1]["message"], "progress");
    assert_eq!(events[2]["event"], "consumed");
    assert_eq!(events[2]["outcome"], "done");
    assert_eq!(events[2]["note"], "finished");
}

#[test]
fn journal_events_emits_ndjson() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "topic-a", "old message"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["journal", "log", "topic-a", "new message"])
        .assert()
        .success();
    let output = stdout(repo.arc(&repo.root).args(["journal", "events"]));
    let events: Vec<serde_json::Value> = output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["message"], "old message");
    assert_eq!(events[1]["message"], "new message");
}

#[test]
fn journal_catchup_renders_events_as_human_lines() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "topic-a", "human message"])
        .assert()
        .success();
    let output = stdout(repo.arc(&repo.root).args(["journal", "catchup"]));
    assert!(
        output.contains("- 20") && output.contains(" test session-a topic-a: human message"),
        "{output}"
    );
}

#[test]
fn journal_dir_precedence_env_over_config_over_default() {
    let repo = Repo::new();
    let canon = fs::canonicalize(&repo.root).unwrap();

    // Default: <ai_home>/journals/<repo-root-slug>.
    let expected_default = repo
        .home
        .join(".local/ai/journals")
        .join(journal_slug(&canon));
    let got_default = journal_dir(&repo);
    assert_eq!(got_default, expected_default);

    // Config override keyed by the repository-root path.
    let cfg_dir = repo.home.join(".local/ai/arc");
    fs::create_dir_all(&cfg_dir).unwrap();
    let override_dir = repo.home.join("custom-journal-dir");
    fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "[journals]\ndirs = {{ \"{}\" = \"{}\" }}\n",
            canon.display(),
            override_dir.display()
        ),
    )
    .unwrap();
    let got_config = journal_dir(&repo);
    assert_eq!(got_config, override_dir);

    // Env wins over both config and default.
    let env_dir = repo.home.join("env-journal-dir");
    let out = stdout(
        repo.arc(&repo.root)
            .env("ARC_JOURNAL_DIR", &env_dir)
            .args(["journal", "dir"]),
    );
    assert_eq!(PathBuf::from(out.trim()), env_dir);

    // dir prints but never creates the directory.
    assert!(!got_default.exists());
    assert!(!override_dir.exists());
}

#[test]
fn journal_dir_archive_prints_cold_sibling_and_respects_env() {
    let repo = Repo::new();
    let hot = journal_dir(&repo);
    let cold = stdout(repo.arc(&repo.root).args(["journal", "dir", "--archive"]));
    assert_eq!(
        PathBuf::from(cold.trim()),
        PathBuf::from(format!("{}-archive", hot.display()))
    );

    let env_hot = repo.home.join("custom-hot");
    let env_cold = stdout(repo.arc(&repo.root).env("ARC_JOURNAL_DIR", &env_hot).args([
        "journal",
        "dir",
        "--archive",
    ]));
    assert_eq!(
        PathBuf::from(env_cold.trim()),
        repo.home.join("custom-hot-archive")
    );
    assert!(!PathBuf::from(env_cold.trim()).exists());
}

#[test]
fn journal_note_writes_file_and_journal_line() {
    let repo = Repo::new();
    let body_path = repo.home.join("body.md");
    let body = "# My Heading\n\nverbatim body\n";
    fs::write(&body_path, body).unwrap();

    let out = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "delegation-blocker-ux",
        "--kind",
        "handoff",
        "--body-file",
        body_path.to_str().unwrap(),
    ]));
    let file = PathBuf::from(out.trim());
    let name = file.file_name().unwrap().to_string_lossy().to_string();

    // Filename shape: <UTC yyyymmddTHHMMSSZ>-<topic>-<kind>.md
    assert!(
        name.ends_with("-delegation-blocker-ux-handoff.md"),
        "{name}"
    );
    let ts = name.split('-').next().unwrap();
    assert_eq!(ts.len(), 16, "timestamp {ts}");
    assert!(ts.ends_with('Z'));
    assert!(ts.contains('T'));

    // Body is written verbatim.
    assert_eq!(fs::read_to_string(&file).unwrap(), body);

    let event = &journal_events(file.parent().unwrap())[0];
    assert_eq!(event["event"], "note");
    assert_eq!(event["harness"], "test");
    assert_eq!(event["session"], "session-a");
    assert_eq!(event["topic"], "delegation-blocker-ux");
    assert_eq!(event["file"], name);
}

#[test]
fn journal_note_title_prepends_heading() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "topic-a",
                "--kind",
                "plan",
                "--body-file",
                "-",
                "--title",
                "The Plan",
            ])
            .write_stdin("plan contents\n"),
    );
    let file = PathBuf::from(out.trim());
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "# The Plan\n\nplan contents\n"
    );
    assert_eq!(
        journal_events(file.parent().unwrap())[0]["title"],
        "The Plan"
    );
}

#[test]
fn journal_note_rejects_invalid_kind_and_topic_without_writing() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    let body_path = repo.home.join("body.md");
    fs::write(&body_path, "body\n").unwrap();

    // Invalid kind is a clap usage error (exit 2).
    repo.arc(&repo.root)
        .args([
            "journal",
            "note",
            "topic-a",
            "--kind",
            "bogus",
            "--body-file",
            body_path.to_str().unwrap(),
        ])
        .assert()
        .code(2);

    // Non-kebab topic is a usage error (exit 1).
    repo.arc(&repo.root)
        .args([
            "journal",
            "note",
            "Bad_Topic",
            "--kind",
            "note",
            "--body-file",
            body_path.to_str().unwrap(),
        ])
        .assert()
        .failure();

    // Nothing was written.
    assert!(!dir.exists());
}

#[test]
fn journal_note_refuses_retired_kinds() {
    let repo = Repo::new();
    let body_path = repo.home.join("body.md");
    fs::write(&body_path, "body\n").unwrap();

    for kind in ["done", "inbox", "spec"] {
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "retired-kind",
                "--kind",
                kind,
                "--body-file",
                body_path.to_str().unwrap(),
            ])
            .assert()
            .code(2);
    }

    assert!(!journal_dir(&repo).exists());
}

#[test]
fn journal_note_records_each_active_kind() {
    let repo = Repo::new();
    let body_path = repo.home.join("body.md");
    fs::write(&body_path, "body\n").unwrap();
    let kinds = [
        "note",
        "memory",
        "plan",
        "handoff",
        "review",
        "conclusion",
        "todo",
        "later",
        "discussion",
        "feature-request",
    ];

    for kind in kinds {
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                &format!("active-{kind}"),
                "--kind",
                kind,
                "--body-file",
                body_path.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    assert_eq!(journal_events(&journal_dir(&repo)).len(), kinds.len());
}

#[test]
fn journal_list_parses_historical_spec_artifact() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    let filename = "20260101T000000Z-historical-api-spec.md";
    fs::write(dir.join(filename), "# Historical API\n").unwrap();

    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["journal", "list", "--json"]),
    ))
    .unwrap();
    let artifact = &value["artifacts"][0];
    assert_eq!(artifact["file"], filename);
    assert_eq!(artifact["topic"], "historical-api");
    assert_eq!(artifact["kind"], "spec");
}

#[test]
fn journal_doctor_reports_retired_kind_as_advice() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("20260101T000000Z-historical-api-spec.md"),
        "# Historical API\n",
    )
    .unwrap();
    fs::write(
        dir.join("20260102T000000Z-historical-cli-spec.md"),
        "# Historical CLI\n",
    )
    .unwrap();

    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["journal", "doctor", "--json"]),
    ))
    .unwrap();
    assert!(value["problems"].as_array().unwrap().is_empty());
    assert_eq!(
        value["advice"],
        serde_json::json!([{
            "code": "retired-artifact-kind",
            "detail": "spec: 2 hot artifacts",
        }])
    );
}

#[test]
fn journal_doctor_keeps_unknown_kind_as_problem() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("20260101T000000Z-mystery-future-kind.md"),
        "# Mystery\n",
    )
    .unwrap();

    let assert = repo
        .arc(&repo.root)
        .args(["journal", "doctor", "--json"])
        .assert()
        .code(1);
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["problems"][0]["code"], "unknown-artifact-kind");
    assert!(value["advice"].as_array().unwrap().is_empty());
}

#[test]
fn journal_log_appends_without_creating_artifact_file() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "topic-a", "consumed inbox X"])
        .assert()
        .success();
    let dir = journal_dir(&repo);
    let entries: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries, vec!["events.jsonl".to_string()]);
    let event = &journal_events(&dir)[0];
    assert_eq!(event["event"], "log");
    assert_eq!(event["message"], "consumed inbox X");
}

#[test]
fn journal_catchup_lists_newest_first_and_json_parses() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    // Two crafted artifacts with distinct, deterministic timestamps.
    fs::write(
        dir.join("20260101T000000Z-alpha-note.md"),
        "# Alpha heading\nold\n",
    )
    .unwrap();
    fs::write(
        dir.join("20260202T000000Z-beta-plan.md"),
        "# Beta heading\nnew\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["journal", "log", "topic-a", "prior journal line"])
        .assert()
        .success();

    // Text form is newest-first.
    let text = stdout(repo.arc(&repo.root).args(["journal", "catchup"]));
    let beta = text.find("beta").unwrap();
    let alpha = text.find("alpha").unwrap();
    assert!(beta < alpha, "beta (newer) must list before alpha:\n{text}");

    // JSON form parses and preserves order + parsed fields.
    let json = stdout(repo.arc(&repo.root).args(["journal", "catchup", "--json"]));
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["topic"], "beta");
    assert_eq!(files[0]["kind"], "plan");
    assert_eq!(files[0]["timestamp"], "20260202T000000Z");
    assert_eq!(files[0]["heading"], "# Beta heading");
    assert_eq!(files[1]["topic"], "alpha");
    assert_eq!(v["journal_tail"].as_array().unwrap().len(), 1);

    // --limit caps the artifact list.
    let limited = stdout(
        repo.arc(&repo.root)
            .args(["journal", "catchup", "--limit", "1", "--json"]),
    );
    let lv: serde_json::Value = serde_json::from_str(limited.trim()).unwrap();
    assert_eq!(lv["files"].as_array().unwrap().len(), 1);
    assert_eq!(lv["files"][0]["topic"], "beta");
}

#[test]
fn journal_memory_note_lists_and_catchup_leads() {
    let repo = Repo::new();
    let body = repo.home.join("memory.md");
    fs::write(&body, "A durable fact.\n").unwrap();
    let body = body.to_str().unwrap();
    stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "older-fact",
        "--kind",
        "memory",
        "--body-file",
        body,
        "--title",
        "Older fact",
    ]));
    std::thread::sleep(std::time::Duration::from_secs(1));
    stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "newer-fact",
        "--kind",
        "memory",
        "--body-file",
        body,
        "--title",
        "Newer fact",
    ]));

    let text = stdout(repo.arc(&repo.root).args(["journal", "memories"]));
    assert!(text.find("# Newer fact").unwrap() < text.find("# Older fact").unwrap());
    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["journal", "memories", "--json"]),
    ))
    .unwrap();
    assert_eq!(value["memories"].as_array().unwrap().len(), 2);
    assert_eq!(value["memories"][0]["topic"], "newer-fact");
    assert_eq!(value["memories"][0]["heading"], "# Newer fact");
    assert!(value["memories"][0].get("kind").is_none());
    assert!(value["memories"][0].get("lane").is_none());

    let catchup = stdout(
        repo.arc(&repo.root)
            .args(["journal", "catchup", "--limit", "1"]),
    );
    assert!(catchup.contains("# Newer fact"));
    assert!(catchup.contains("# Older fact"));
    assert!(catchup.find("lanes:").unwrap() < catchup.find("memory:").unwrap());
    assert!(catchup.find("memory:").unwrap() < catchup.find("artifacts (newest first):").unwrap());
    let catchup: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["journal", "catchup", "--limit", "1", "--json"]),
    ))
    .unwrap();
    assert_eq!(catchup["memories"].as_array().unwrap().len(), 2);
}

#[test]
fn journal_memory_retire_via_consume() {
    let repo = Repo::new();
    let body = repo.home.join("memory.md");
    fs::write(&body, "A durable fact.\n").unwrap();
    let body = body.to_str().unwrap();
    let retired = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "retired",
        "--kind",
        "memory",
        "--body-file",
        body,
        "--title",
        "Retired fact",
    ]));
    let retired = PathBuf::from(retired.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let live = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "live",
        "--kind",
        "memory",
        "--body-file",
        body,
        "--title",
        "Live fact",
    ]));
    let live = PathBuf::from(live.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    repo.arc(&repo.root)
        .args(["journal", "consume", &retired, "--outcome", "superseded"])
        .assert()
        .success();
    let memories = stdout(repo.arc(&repo.root).args(["journal", "memories"]));
    assert!(!memories.contains("Retired fact"), "{memories}");
    assert!(memories.contains("Live fact"), "{memories}");
    let catchup = stdout(repo.arc(&repo.root).args(["journal", "catchup"]));
    let memory_block = catchup
        .split_once("memory:\n")
        .unwrap()
        .1
        .split_once("dir:")
        .unwrap()
        .0;
    assert!(!memory_block.contains("Retired fact"), "{memory_block}");
    assert!(memory_block.contains("Live fact"), "{memory_block}");

    let swept = stdout(
        repo.arc(&repo.root)
            .args(["journal", "archive", "--consumed"]),
    );
    assert!(swept.contains(&retired), "{swept}");
    assert!(!swept.contains(&live), "{swept}");
    repo.arc(&repo.root)
        .args(["journal", "archive", &live])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "must be consumed before it can be archived",
        ));
}

#[test]
fn journal_memory_never_actionable() {
    let repo = Repo::new();
    let body = repo.home.join("memory.md");
    fs::write(&body, "A durable fact.\n").unwrap();
    stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "project-fact",
        "--kind",
        "memory",
        "--body-file",
        body.to_str().unwrap(),
        "--title",
        "Project fact",
    ]));

    let text = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(!text.contains("Project fact"), "{text}");
    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["journal", "open", "--json"]),
    ))
    .unwrap();
    assert!(value["open"].as_array().unwrap().is_empty());
    assert!(value["later"].as_array().unwrap().is_empty());
    repo.arc(&repo.root)
        .args(["journal", "open", "--kind", "memory"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--kind memory is not actionable"));
}

#[test]
fn journal_log_is_append_only() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "topic-a", "first message"])
        .assert()
        .success();
    let dir = journal_dir(&repo);
    let after_first = fs::read(dir.join("events.jsonl")).unwrap();

    repo.arc(&repo.root)
        .args(["journal", "log", "topic-b", "second message"])
        .assert()
        .success();
    let after_second = fs::read(dir.join("events.jsonl")).unwrap();

    // The earlier bytes are preserved verbatim; only new bytes are appended.
    assert!(after_second.starts_with(&after_first[..]));
    assert!(after_second.len() > after_first.len());
    let text = String::from_utf8(after_second).unwrap();
    assert!(
        text.contains("\"topic\":\"topic-a\"") && text.contains("\"message\":\"first message\"")
    );
    assert!(
        text.contains("\"topic\":\"topic-b\"") && text.contains("\"message\":\"second message\"")
    );
}

/// `journal open` lists unconsumed primary actionable kinds (todo/handoff/
/// inbox/plan) before lower-priority later items; `journal consume` retires
/// either through a machine-readable journal line and refuses double consumption.
#[test]
fn journal_open_and_consume_track_actionable_items() {
    let repo = Repo::new();
    let body_path = repo.home.join("body.md");
    fs::write(&body_path, "# Item\n\nbody\n").unwrap();
    let body = body_path.to_str().unwrap();

    let mut names = std::collections::HashMap::new();
    for (topic, kind) in [
        ("next-work", "todo"),
        ("pickup", "handoff"),
        ("deferred", "later"),
        ("memo", "note"),
    ] {
        let out = stdout(repo.arc(&repo.root).args([
            "journal",
            "note",
            topic,
            "--kind",
            kind,
            "--body-file",
            body,
        ]));
        let file = PathBuf::from(out.trim());
        names.insert(
            kind,
            file.file_name().unwrap().to_string_lossy().to_string(),
        );
    }

    // The todo kind produces the expected filename shape.
    assert!(
        names["todo"].ends_with("-next-work-todo.md"),
        "{:?}",
        names["todo"]
    );

    // Open renders the primary queue before its separate later tier; records
    // (note) do not appear in either section.
    let open = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(open.contains("next-work"), "{open}");
    assert!(open.contains("pickup"), "{open}");
    assert!(open.contains("deferred"), "{open}");
    assert!(!open.contains("memo"), "{open}");
    assert!(
        open.find("open items (newest first):").unwrap()
            < open.find("later items (newest first):").unwrap(),
        "{open}"
    );

    // Consume the handoff with an outcome and note; it leaves the queue.
    repo.arc(&repo.root)
        .args([
            "journal",
            "consume",
            &names["handoff"],
            "--outcome",
            "superseded",
            "--note",
            "folded",
        ])
        .assert()
        .success();
    let open: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["journal", "open", "--json"]),
    ))
    .unwrap();
    assert!(open["dir"].as_str().is_some());
    let open_files: Vec<&str> = open["open"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["file"].as_str().unwrap())
        .collect();
    assert_eq!(open_files, vec![names["todo"].as_str()]);
    let later_files: Vec<&str> = open["later"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["file"].as_str().unwrap())
        .collect();
    assert_eq!(later_files, vec![names["later"].as_str()]);

    // --kind filters a primary kind into open and later into its own array.
    let filtered: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["journal", "open", "--kind", "todo", "--json"]),
    ))
    .unwrap();
    assert_eq!(filtered["open"][0]["file"], names["todo"]);
    assert!(filtered["later"].as_array().unwrap().is_empty());
    let filtered: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["journal", "open", "--kind", "later", "--json"]),
    ))
    .unwrap();
    assert!(filtered["open"].as_array().unwrap().is_empty());
    assert_eq!(filtered["later"][0]["file"], names["later"]);
    let filtered_text = stdout(
        repo.arc(&repo.root)
            .args(["journal", "open", "--kind", "later"]),
    );
    assert!(
        filtered_text.contains("open items (newest first):\n  (none)\nlater items (newest first):"),
        "{filtered_text}"
    );
    repo.arc(&repo.root)
        .args(["journal", "open", "--kind", "note"])
        .assert()
        .failure();

    // A later item consumes just like an item in the primary queue.
    repo.arc(&repo.root)
        .args(["journal", "consume", &names["later"]])
        .assert()
        .success();
    let after_later = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(after_later.contains("later items (newest first):\n  (none)"));

    // Prose mentioning a filename near "consumed" is not the machine shape
    // and must not retire the item.
    repo.arc(&repo.root)
        .args([
            "journal",
            "log",
            "next-work",
            &format!("discussed consumed {} in passing", names["todo"]),
        ])
        .assert()
        .success();
    let still_open = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(still_open.contains("next-work"), "{still_open}");

    // Even the full machine shape quoted mid-sentence must not consume:
    // the marker has to open the journal message field.
    repo.arc(&repo.root)
        .args([
            "journal",
            "log",
            "next-work",
            &format!("reviewed consumed {} [done] but rejected it", names["todo"]),
        ])
        .assert()
        .success();
    let still_open = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(still_open.contains("next-work"), "{still_open}");

    // Exclusive creation: recreating the same timestamped path fails loudly
    // instead of overwriting a queued artifact.
    let clash = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "clash",
        "--kind",
        "todo",
        "--body-file",
        body,
    ]));
    let clash_name = PathBuf::from(clash.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let manual = journal_dir(&repo).join(&clash_name);
    assert!(manual.is_file());
    // A direct second create of the identical path (what a same-second
    // duplicate note would attempt) must be refused by exclusive creation.
    assert!(fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manual)
        .is_err());

    let events = journal_events(&journal_dir(&repo));
    let consumed = events
        .iter()
        .find(|event| event["event"] == "consumed")
        .unwrap();
    assert_eq!(consumed["file"], names["handoff"]);
    assert_eq!(consumed["outcome"], "superseded");
    assert_eq!(consumed["note"], "folded");

    // Guards: double consume, unknown artifact, and paths are refused.
    repo.arc(&repo.root)
        .args(["journal", "consume", &names["handoff"]])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["journal", "consume", "20990101T000000Z-ghost-todo.md"])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["journal", "consume", "sub/dir-file-todo.md"])
        .assert()
        .failure();
}

#[test]
fn journal_feature_requests_have_their_own_open_queue_tier() {
    let repo = Repo::new();
    let body_path = repo.home.join("body.md");
    fs::write(&body_path, "# Requested capability\n\nbody\n").unwrap();
    let body = body_path.to_str().unwrap();

    let mut names = std::collections::HashMap::new();
    for (topic, kind) in [
        ("primary-work", "todo"),
        ("deferred-work", "later"),
        ("requested-capability", "feature-request"),
    ] {
        let out = stdout(repo.arc(&repo.root).args([
            "journal",
            "note",
            topic,
            "--kind",
            kind,
            "--body-file",
            body,
        ]));
        names.insert(
            kind,
            PathBuf::from(out.trim())
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string(),
        );
    }

    assert!(
        names["feature-request"].ends_with("-requested-capability-feature-request.md"),
        "{:?}",
        names["feature-request"]
    );
    let event = journal_events(&journal_dir(&repo))
        .into_iter()
        .find(|event| event["file"] == names["feature-request"])
        .unwrap();
    assert_eq!(event["event"], "note");

    let crafted_name = "20260101T000000Z-capability-with-hyphens-feature-request.md";
    fs::write(journal_dir(&repo).join(crafted_name), "# Crafted request\n").unwrap();

    let text = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    let open_section = text.find("open items (newest first):").unwrap();
    let later_section = text.find("later items (newest first):").unwrap();
    let feature_section = text.find("feature requests (newest first):").unwrap();
    assert!(
        open_section < later_section && later_section < feature_section,
        "{text}"
    );
    assert!(text[open_section..later_section].contains("primary-work"));
    assert!(text[later_section..feature_section].contains("deferred-work"));
    assert!(text[feature_section..].contains("requested-capability"));
    assert!(text[feature_section..].contains("capability-with-hyphens"));

    let json: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["journal", "open", "--json"]),
    ))
    .unwrap();
    assert_eq!(json["open"][0]["file"], names["todo"]);
    assert_eq!(json["later"][0]["file"], names["later"]);
    let feature_requests = json["feature_requests"].as_array().unwrap();
    assert_eq!(feature_requests.len(), 2);
    assert_eq!(feature_requests[0]["file"], names["feature-request"]);
    assert_eq!(feature_requests[1]["file"], crafted_name);
    assert_eq!(feature_requests[1]["topic"], "capability-with-hyphens");

    let filtered: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "journal",
        "open",
        "--kind",
        "feature-request",
        "--json",
    ])))
    .unwrap();
    assert!(filtered["open"].as_array().unwrap().is_empty());
    assert!(filtered["later"].as_array().unwrap().is_empty());
    assert_eq!(filtered["feature_requests"].as_array().unwrap().len(), 2);
    assert_eq!(
        filtered["feature_requests"][0]["file"],
        names["feature-request"]
    );

    repo.arc(&repo.root)
        .args(["journal", "consume", &names["feature-request"]])
        .assert()
        .success();
    let after = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(!after.contains("requested-capability"), "{after}");
    assert!(after.contains("capability-with-hyphens"), "{after}");
    let doctor = repo
        .arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doctor = String::from_utf8(doctor).unwrap();
    assert!(!doctor.contains("unknown-artifact-kind"), "{doctor}");
}

#[test]
fn journal_archive_moves_records_and_catchup_reads_cold_with_hot_journal() {
    let repo = Repo::new();
    let hot = journal_dir(&repo);
    fs::create_dir_all(&hot).unwrap();
    let name = "20260101T000000Z-history-note.md";
    fs::write(hot.join(name), "# History\n").unwrap();

    repo.arc(&repo.root)
        .args(["journal", "archive", name, "--note", "cold storage"])
        .assert()
        .success();
    let cold = PathBuf::from(format!("{}-archive", hot.display()));
    assert!(!hot.join(name).exists());
    assert!(cold.join(name).is_file());
    assert!(hot.join("events.jsonl").is_file());
    assert!(!cold.join("events.jsonl").exists());

    let hot_catchup = stdout(repo.arc(&repo.root).args(["journal", "catchup"]));
    assert!(!hot_catchup.contains("history  note"), "{hot_catchup}");
    let cold_catchup = stdout(
        repo.arc(&repo.root)
            .args(["journal", "catchup", "--archived"]),
    );
    assert!(cold_catchup.contains("history  note"), "{cold_catchup}");
    assert!(cold_catchup.contains("cold storage"), "{cold_catchup}");
    let archived = journal_events(&hot).pop().unwrap();
    assert_eq!(archived["event"], "archived");
    assert_eq!(archived["file"], name);
    assert_eq!(archived["note"], "cold storage");
}

#[test]
fn journal_archive_refuses_unconsumed_later_then_accepts_consumed() {
    let repo = Repo::new();
    let hot = journal_dir(&repo);
    fs::create_dir_all(&hot).unwrap();
    let name = "20260101T000000Z-next-later.md";
    fs::write(hot.join(name), "later\n").unwrap();

    repo.arc(&repo.root)
        .args(["journal", "archive", name])
        .assert()
        .failure();
    assert!(hot.join(name).is_file());
    assert!(!hot.join("events.jsonl").exists());

    repo.arc(&repo.root)
        .args(["journal", "consume", name])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["journal", "archive", name])
        .assert()
        .success();
    assert!(!hot.join(name).exists());
    assert!(PathBuf::from(format!("{}-archive", hot.display()))
        .join(name)
        .is_file());
}

#[test]
fn journal_archive_consumed_bulk_filters_age_and_rejects_flag_misuse() {
    let repo = Repo::new();
    let hot = journal_dir(&repo);
    fs::create_dir_all(&hot).unwrap();
    let old = "20200101T000000Z-old-todo.md";
    let old_later = "20200101T000000Z-old-later.md";
    let new = "29990101T000000Z-new-plan.md";
    let open = "20200101T000000Z-open-inbox.md";
    let record = "20200101T000000Z-record-note.md";
    for name in [old, old_later, new, open, record] {
        fs::write(hot.join(name), name).unwrap();
    }
    for name in [old, old_later, new] {
        repo.arc(&repo.root)
            .args(["journal", "consume", name])
            .assert()
            .success();
    }

    let output = stdout(repo.arc(&repo.root).args([
        "journal",
        "archive",
        "--consumed",
        "--older-than-days",
        "30",
    ]));
    assert_eq!(output.lines().collect::<Vec<_>>(), vec![old_later, old]);
    let cold = PathBuf::from(format!("{}-archive", hot.display()));
    assert!(cold.join(old).is_file());
    assert!(cold.join(old_later).is_file());
    assert!(hot.join(new).is_file());
    assert!(hot.join(open).is_file());
    assert!(hot.join(record).is_file());

    repo.arc(&repo.root)
        .args(["journal", "archive", "--older-than-days", "30"])
        .assert()
        .code(2);
    repo.arc(&repo.root)
        .args(["journal", "archive", new, "--consumed"])
        .assert()
        .code(2);
}

#[test]
fn journal_archive_refuses_cold_name_collision_without_moving_source() {
    let repo = Repo::new();
    let hot = journal_dir(&repo);
    let cold = PathBuf::from(format!("{}-archive", hot.display()));
    fs::create_dir_all(&hot).unwrap();
    fs::create_dir_all(&cold).unwrap();
    let name = "20200101T000000Z-history-note.md";
    fs::write(hot.join(name), "hot\n").unwrap();
    fs::write(cold.join(name), "cold\n").unwrap();

    repo.arc(&repo.root)
        .args(["journal", "archive", name])
        .assert()
        .failure();
    assert_eq!(fs::read_to_string(hot.join(name)).unwrap(), "hot\n");
    assert_eq!(fs::read_to_string(cold.join(name)).unwrap(), "cold\n");
}

#[test]
fn journal_lane_open_writes_marker_and_list_shows_live() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args([
            "journal",
            "lane",
            "open",
            "work-a",
            "--scope",
            "topic-a,topic-b",
            "--ttl",
            "30m",
            "--status",
            "implementing",
        ])
        .assert()
        .success();
    let event = journal_events(&journal_dir(&repo)).pop().unwrap();
    assert_eq!(event["event"], "lane-opened");
    assert_eq!(event["ttl_seconds"], 1800);
    assert_eq!(event["scope"], serde_json::json!(["topic-a", "topic-b"]));
    assert_eq!(event["status"], "implementing");

    let text = stdout(repo.arc(&repo.root).args(["journal", "lane", "list"]));
    assert!(text.contains("work-a  test session-a  live"), "{text}");
    assert!(text.contains("+scope: topic-a, topic-b"), "{text}");
    assert!(text.contains("implementing"), "{text}");

    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["journal", "lane", "list", "--json"]),
    ))
    .unwrap();
    assert_eq!(value["lanes"][0]["topic"], "work-a");
    assert_eq!(value["lanes"][0]["owner_harness"], "test");
    assert_eq!(value["lanes"][0]["owner_session"], "session-a");
    assert_eq!(value["lanes"][0]["state"], "live");
    assert_eq!(value["lanes"][0]["ttl_seconds"], 1800);
    assert_eq!(
        value["lanes"][0]["scope"],
        serde_json::json!(["topic-a", "topic-b"])
    );
    assert_eq!(value["lanes"][0]["status"], "implementing");
}

#[test]
fn journal_lane_requires_session_identity() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    repo.arc(&repo.root)
        .env_remove("ARC_SESSION")
        .args(["journal", "lane", "open", "work-a"])
        .assert()
        .failure();
    assert!(!dir.exists());
}

#[test]
fn journal_lane_rule_of_one_implicit_close() {
    let repo = Repo::new();
    for topic in ["lane-a", "lane-b"] {
        repo.arc(&repo.root)
            .args(["journal", "lane", "open", topic])
            .assert()
            .success();
    }
    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["journal", "lane", "list", "--json"]),
    ))
    .unwrap();
    assert_eq!(value["lanes"].as_array().unwrap().len(), 1);
    assert_eq!(value["lanes"][0]["topic"], "lane-b");
}

#[test]
fn journal_lane_renew_owner_only_and_updates_ttl() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "lane", "open", "work-a"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["journal", "lane", "renew", "work-a", "--ttl", "45m"])
        .assert()
        .success();
    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["journal", "lane", "list", "--json"]),
    ))
    .unwrap();
    assert_eq!(value["lanes"][0]["ttl_seconds"], 2700);
    repo.arc(&repo.root)
        .env("ARC_SESSION", "session-b")
        .args(["journal", "lane", "renew", "work-a"])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["journal", "lane", "renew", "unknown"])
        .assert()
        .failure();
}

#[test]
fn journal_lane_close_owner_and_takeover_semantics() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "lane", "open", "done-lane"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["journal", "lane", "close", "done-lane"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["journal", "lane", "close", "done-lane"])
        .assert()
        .failure();

    repo.arc(&repo.root)
        .args(["journal", "lane", "open", "takeover", "--ttl", "1s"])
        .assert()
        .success();
    let live_conflict = repo
        .arc(&repo.root)
        .env("ARC_SESSION", "session-b")
        .args([
            "journal",
            "lane",
            "close",
            "takeover",
            "--outcome",
            "expired",
        ])
        .output()
        .unwrap();
    assert!(!live_conflict.status.success());
    let stderr = String::from_utf8_lossy(&live_conflict.stderr);
    assert!(stderr.contains("owner test session-a"), "{stderr}");
    assert!(stderr.contains("idle"), "{stderr}");
    assert!(stderr.contains("ttl 1s"), "{stderr}");
    thread::sleep(Duration::from_secs(2));
    repo.arc(&repo.root)
        .env("ARC_SESSION", "session-b")
        .args([
            "journal",
            "lane",
            "close",
            "takeover",
            "--outcome",
            "expired",
        ])
        .assert()
        .success();
}

#[test]
fn journal_lane_liveness_refreshes_from_any_owner_journal_line() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "lane", "open", "work-a", "--ttl", "1s"])
        .assert()
        .success();
    thread::sleep(Duration::from_secs(2));
    repo.arc(&repo.root)
        .args(["journal", "log", "other-topic", "still active"])
        .assert()
        .success();
    let text = stdout(repo.arc(&repo.root).args(["journal", "lane", "list"]));
    assert!(text.contains("work-a  test session-a  live"), "{text}");
}

#[test]
fn journal_open_annotates_items_covered_by_live_lanes() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("20260101T000000Z-covered-todo.md"), "# Covered\n").unwrap();
    fs::write(dir.join("20260101T000001Z-free-todo.md"), "# Free\n").unwrap();
    // A generous TTL keeps the lane comfortably live across the several CLI
    // round-trips these assertions make; the staleness path is exercised
    // separately so this test never races the liveness clock under load.
    repo.arc(&repo.root)
        .env("ARC_SESSION", "external-session")
        .args([
            "journal",
            "lane",
            "open",
            "external-lane",
            "--scope",
            "covered",
            "--ttl",
            "1h",
        ])
        .assert()
        .success();

    let text = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(
        text.contains("covered  todo  # Covered [lane: external-lane — test external, external]"),
        "{text}"
    );
    assert!(!text.contains("# Free [lane:"), "{text}");
    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["journal", "open", "--json"]),
    ))
    .unwrap();
    let covered = value["open"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["topic"] == "covered")
        .unwrap();
    assert_eq!(covered["lane"]["topic"], "external-lane");
    assert_eq!(covered["lane"]["owner_session"], "external-session");
    assert_eq!(covered["lane"]["this_session"], false);
}

#[test]
fn journal_open_drops_annotation_once_a_lane_goes_stale() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("20260101T000000Z-covered-todo.md"), "# Covered\n").unwrap();
    repo.arc(&repo.root)
        .env("ARC_SESSION", "external-session")
        .args([
            "journal",
            "lane",
            "open",
            "external-lane",
            "--scope",
            "covered",
            "--ttl",
            "1s",
        ])
        .assert()
        .success();

    thread::sleep(Duration::from_secs(2));
    let stale = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(!stale.contains("[lane:"), "{stale}");
}

#[test]
fn journal_catchup_shows_lanes_block() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "lane", "open", "work-a"])
        .assert()
        .success();
    let text = stdout(repo.arc(&repo.root).args(["journal", "catchup"]));
    assert!(text.starts_with("lanes:\n"), "{text}");
    assert!(text.find("lanes:").unwrap() < text.find("artifacts (newest first):").unwrap());
    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["journal", "catchup", "--json"]),
    ))
    .unwrap();
    assert_eq!(value["lanes"][0]["topic"], "work-a");
}

#[test]
fn journal_doctor_clean_archive_exits_zero() {
    let repo = Repo::new();
    let body = repo.home.join("body.md");
    fs::write(&body, "content\n").unwrap();
    for (topic, kind) in [("clean-note", "note"), ("shared-fact", "memory")] {
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                topic,
                "--kind",
                kind,
                "--body-file",
                body.to_str().unwrap(),
            ])
            .assert()
            .success();
    }
    repo.arc(&repo.root)
        .args(["journal", "lane", "open", "active-work"])
        .assert()
        .success();

    let text = stdout(repo.arc(&repo.root).args(["journal", "doctor"]));
    assert_eq!(text, "problems:\n  (none)\nadvice:\n  (none)\n");
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();

    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["journal", "doctor", "--json"]),
    ))
    .unwrap();
    assert!(value["problems"].as_array().unwrap().is_empty());
    assert!(value["advice"].as_array().unwrap().is_empty());
}

#[test]
fn journal_doctor_reports_problems_and_exits_one() {
    let repo = Repo::new();
    let body = repo.home.join("body.md");
    fs::write(&body, "work\n").unwrap();
    let artifact = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "missing-work",
        "--kind",
        "todo",
        "--body-file",
        body.to_str().unwrap(),
    ]));
    let artifact = PathBuf::from(artifact.trim());
    let filename = artifact.file_name().unwrap().to_str().unwrap().to_string();
    repo.arc(&repo.root)
        .args(["journal", "consume", &filename])
        .assert()
        .success();
    fs::remove_file(artifact).unwrap();

    let dir = journal_dir(&repo);
    let journal = dir.join("events.jsonl");
    let mut contents = fs::read_to_string(&journal).unwrap();
    contents.push_str("not json\n");
    contents.push_str(
        r#"{"schema":"journal-events/1","ts":"2026-07-18T00:00:00Z","harness":"test","session":"session-a","topic":"unknown","event":"bogus"}"#,
    );
    contents.push('\n');
    fs::write(journal, contents).unwrap();
    fs::write(dir.join("bad.md"), "bad\n").unwrap();
    fs::write(dir.join("foo-topic-bogus.md"), "bad kind\n").unwrap();

    let assert = repo
        .arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .code(1);
    let text = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    for code in [
        "malformed-jsonl",
        "unknown-jsonl-event",
        "malformed-artifact-name",
        "unknown-artifact-kind",
        "dangling-artifact-reference",
    ] {
        assert!(text.contains(code), "missing {code}: {text}");
    }
}

#[test]
fn journal_doctor_advice_only_exits_zero() {
    let repo = Repo::new();
    let body = repo.home.join("body.md");
    fs::write(&body, "work\n").unwrap();
    let artifact = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "finished-work",
        "--kind",
        "todo",
        "--body-file",
        body.to_str().unwrap(),
    ]));
    let filename = PathBuf::from(artifact.trim())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    repo.arc(&repo.root)
        .args(["journal", "consume", &filename])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["journal", "lane", "open", "idle-work", "--ttl", "1s"])
        .assert()
        .success();
    thread::sleep(Duration::from_secs(2));

    let assert = repo
        .arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();
    let text = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(text.contains("archivable-artifacts"), "{text}");
    assert!(text.contains("stale-lane"), "{text}");
    assert!(text.contains("problems:\n  (none)"), "{text}");
}

#[test]
fn journal_log_free_text_is_never_promoted_to_typed_events() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args([
            "journal",
            "log",
            "tidy",
            "archived old files by hand: details",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "journal",
            "log",
            "tidy",
            "consumed nothing [done] just prose",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "journal",
            "log",
            "tidy",
            "lane opened [30m] scope=foo: prose",
        ])
        .assert()
        .success();
    let dir = journal_dir(&repo);
    let events = journal_events(&dir);
    assert_eq!(events.len(), 3);
    for event in &events {
        assert_eq!(event["event"], "log", "{event}");
        assert!(event["message"].as_str().unwrap().len() > 10, "{event}");
    }
    let doctor = stdout(repo.arc(&repo.root).args(["journal", "doctor"]));
    assert!(!doctor.contains("dangling-artifact-reference"), "{doctor}");
}

#[test]
fn journal_doctor_ignores_non_artifact_file_references() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        "{\"schema\":\"journal-events/1\",\"ts\":\"2026-01-01T00:00:00Z\",\"harness\":\"h\",\"session\":\"s\",\"topic\":\"tidy\",\"event\":\"archived\",\"file\":\"prose with spaces\"}\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("problems:\n  (none)"));
}

/// The thread spellings are gone: no `arc thread` subcommand and no
/// nested `journal` alias for the log-only append.
#[test]
fn journal_thread_spellings_are_rejected() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["thread", "dir"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("unrecognized subcommand"));
    repo.arc(&repo.root)
        .args(["journal", "journal", "compat", "old spelling"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("unrecognized subcommand"));
}

/// A stray legacy `journal.md` is inert: events ignores it and doctor
/// reports it as a malformed artifact name instead of reading it.
#[test]
fn journal_stray_legacy_file_is_inert() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("journal.md"),
        "- 2026-01-01T00:00:00Z old legacy topic-a: old message\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["journal", "log", "topic-a", "live message"])
        .assert()
        .success();
    let output = stdout(repo.arc(&repo.root).args(["journal", "events"]));
    assert!(!output.contains("old message"), "{output}");
    let doctor = repo
        .arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .failure();
    let text = String::from_utf8_lossy(&doctor.get_output().stdout).to_string();
    assert!(
        text.contains("malformed-artifact-name: journal.md"),
        "{text}"
    );
}

#[test]
fn begin_from_journal_todo_consumes_and_seeds_brief() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "widget",
                "--kind",
                "todo",
                "--body-file",
                "-",
            ])
            .write_stdin("do the widget\n"),
    );
    let file = PathBuf::from(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    repo.arc(&repo.root)
        .args(["begin", "widget", "--no-worktree", "--from-journal", &file])
        .assert()
        .success();

    // The change records where it came from.
    let show = json_stdout(repo.arc(&repo.root).args(["show", "widget", "--json"]));
    assert_eq!(show["journal_ref"], file);
    repo.arc(&repo.root)
        .args(["brief", "widget"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Seeded from journal artifact"))
        .stdout(predicates::str::contains("do the widget"));

    // The source item is consumed as superseded and leaves the open queue.
    let events = journal_events(&journal_dir(&repo));
    assert!(
        events.iter().any(|event| event["event"] == "consumed"
            && event["outcome"] == "superseded"
            && event["file"] == file),
        "{events:?}"
    );
    let open = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(!open.contains(&file), "consumed item still open:\n{open}");
}

#[test]
fn begin_from_journal_plan_opens_multiple_changes_with_ref() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "roadmap",
                "--kind",
                "plan",
                "--body-file",
                "-",
            ])
            .write_stdin("split this plan\n"),
    );
    let file = PathBuf::from(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    for slug in ["roadmap-one", "roadmap-two"] {
        repo.arc(&repo.root)
            .args(["begin", slug, "--no-worktree", "--from-journal", &file])
            .assert()
            .success();
        let show = json_stdout(repo.arc(&repo.root).args(["show", slug, "--json"]));
        assert_eq!(show["journal_ref"], file);
    }
}

#[test]
fn begin_from_journal_plan_leaves_item_unconsumed() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "roadmap",
                "--kind",
                "plan",
                "--body-file",
                "-",
            ])
            .write_stdin("keep this plan open\n"),
    );
    let file = PathBuf::from(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    repo.arc(&repo.root)
        .args([
            "begin",
            "roadmap-member",
            "--no-worktree",
            "--from-journal",
            &file,
        ])
        .assert()
        .success();

    let events = journal_events(&journal_dir(&repo));
    assert!(
        !events
            .iter()
            .any(|event| event["event"] == "consumed" && event["file"] == file),
        "{events:?}"
    );
    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert!(
        open["open"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["file"] == file),
        "{open}"
    );
}

#[test]
fn begin_from_journal_rejects_explicitly_consumed_plan() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "roadmap",
                "--kind",
                "plan",
                "--body-file",
                "-",
            ])
            .write_stdin("finished plan\n"),
    );
    let file = PathBuf::from(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    repo.arc(&repo.root)
        .args(["journal", "consume", &file])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args([
            "begin",
            "roadmap-member",
            "--no-worktree",
            "--from-journal",
            &file,
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already consumed"));
    repo.arc(&repo.root)
        .args(["show", "roadmap-member"])
        .assert()
        .failure();
}

#[test]
fn begin_from_journal_rejects_a_missing_item() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args([
            "begin",
            "widget",
            "--no-worktree",
            "--from-journal",
            "nope.md",
        ])
        .assert()
        .failure();
    // No change was created by the rejected begin.
    repo.arc(&repo.root)
        .args(["show", "widget"])
        .assert()
        .failure();
}

#[test]
fn auto_log_narrates_integrate_when_enabled() {
    let repo = Repo::new();
    let cfg = repo.home.join(".local/ai/arc");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(cfg.join("config.toml"), "[journal]\nauto_log = true\n").unwrap();

    let (change_id, worktree, _head) = change_with_patchset(&repo, "feat-x");
    repo.arc(&worktree)
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "feat-x"])
        .assert()
        .success();

    let events = journal_events(&journal_dir(&repo));
    assert!(
        events.iter().any(|event| event["event"] == "log"
            && event["message"]
                .as_str()
                .is_some_and(|m| m.starts_with(&format!("integrated {change_id}")))),
        "{events:?}"
    );
}

#[test]
fn journal_open_annotates_a_matching_change() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "feat-x",
                "--kind",
                "todo",
                "--body-file",
                "-",
            ])
            .write_stdin("x\n"),
    );
    repo.arc(&repo.root)
        .args(["begin", "feat-x", "--no-worktree"])
        .assert()
        .success();

    let open = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(open.contains("[change feat-x-"), "{open}");
}

#[test]
fn auto_log_write_failure_is_a_warning_not_a_command_failure() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    let cfg = repo.home.join(".local/ai/arc");
    fs::create_dir_all(&cfg).unwrap();
    fs::write(cfg.join("config.toml"), "[journal]\nauto_log = true\n").unwrap();

    // Pre-create the journal dir read-only so the advisory append fails.
    let jd = journal_dir(&repo);
    fs::create_dir_all(&jd).unwrap();
    fs::set_permissions(&jd, fs::Permissions::from_mode(0o500)).unwrap();

    let assert = repo
        .arc(&repo.root)
        .args(["begin", "feat-x", "--no-worktree"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    // Restore permissions so the tempdir can be cleaned up.
    fs::set_permissions(&jd, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(stderr.contains("auto-log failed"), "stderr was: {stderr}");
}

#[test]
fn journal_list_enumerates_all_kinds_newest_first() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("20260101T000000Z-alpha-note.md"),
        "# Alpha heading\nold\n",
    )
    .unwrap();
    fs::write(
        dir.join("20260202T000000Z-beta-conclusion.md"),
        "# Beta heading\nmid\n",
    )
    .unwrap();
    fs::write(
        dir.join("20260303T000000Z-gamma-plan.md"),
        "# Gamma heading\nnew\n",
    )
    .unwrap();
    // Non-artifact files never list.
    fs::write(dir.join("README.md"), "not an artifact\n").unwrap();

    let text = stdout(repo.arc(&repo.root).args(["journal", "list"]));
    let gamma = text.find("gamma").unwrap();
    let beta = text.find("beta").unwrap();
    let alpha = text.find("alpha").unwrap();
    assert!(gamma < beta && beta < alpha, "newest first:\n{text}");
    assert!(text.contains("# Alpha heading"), "{text}");
    assert!(!text.contains("not an artifact"), "{text}");

    // Non-actionable kinds are listed too — the gap `open`/`memories` leave.
    let notes = stdout(
        repo.arc(&repo.root)
            .args(["journal", "list", "--kind", "note"]),
    );
    assert!(notes.contains("alpha"), "{notes}");
    assert!(!notes.contains("gamma"), "{notes}");
    let conclusions =
        stdout(
            repo.arc(&repo.root)
                .args(["journal", "list", "--kind", "conclusion"]),
        );
    assert!(conclusions.contains("beta"), "{conclusions}");

    // A kind with no matches lists nothing, successfully.
    let reviews = stdout(
        repo.arc(&repo.root)
            .args(["journal", "list", "--kind", "review"]),
    );
    assert!(reviews.contains("(none)"), "{reviews}");

    // JSON form parses and carries the full field set.
    let json = stdout(repo.arc(&repo.root).args(["journal", "list", "--json"]));
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    let artifacts = v["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 3);
    assert_eq!(artifacts[0]["topic"], "gamma");
    assert_eq!(artifacts[0]["kind"], "plan");
    assert_eq!(artifacts[0]["timestamp"], "20260303T000000Z");
    assert_eq!(artifacts[0]["heading"], "# Gamma heading");
    assert_eq!(artifacts[0]["file"], "20260303T000000Z-gamma-plan.md");
    assert!(artifacts[0]["consumed"].is_null());
    assert_eq!(artifacts[2]["topic"], "alpha");
}

#[test]
fn journal_list_marks_consumed_without_hiding() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    let name = "20260101T000000Z-duty-todo.md";
    fs::write(dir.join(name), "# Duty\n").unwrap();
    repo.arc(&repo.root)
        .args(["journal", "consume", name, "--outcome", "discarded"])
        .assert()
        .success();

    let text = stdout(repo.arc(&repo.root).args(["journal", "list"]));
    assert!(text.contains("duty"), "{text}");
    assert!(text.contains("[consumed: discarded]"), "{text}");

    let json = stdout(repo.arc(&repo.root).args(["journal", "list", "--json"]));
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    let artifacts = v["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0]["consumed"], "discarded");

    // An empty journal lists nothing and still succeeds.
    let repo = Repo::new();
    let text = stdout(repo.arc(&repo.root).args(["journal", "list"]));
    assert!(text.contains("(none)"), "{text}");
}

#[test]
fn journal_list_fails_open_on_malformed_and_unknown_outcomes() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    let name = "20260101T000000Z-duty-todo.md";
    fs::write(dir.join(name), "# Duty\n").unwrap();
    // A malformed line and a consumed event with an unrecognized outcome:
    // both must be skipped, leaving the item unmarked.
    fs::write(
        dir.join("events.jsonl"),
        concat!(
            "not json at all\n",
            "{\"schema\":\"journal-events/1\",\"ts\":\"2026-01-01T00:00:01Z\",",
            "\"harness\":\"test\",\"session\":\"test\",\"topic\":\"duty\",",
            "\"event\":\"consumed\",\"file\":\"20260101T000000Z-duty-todo.md\",",
            "\"outcome\":\"maybe\"}\n",
        ),
    )
    .unwrap();

    let text = stdout(repo.arc(&repo.root).args(["journal", "list"]));
    assert!(text.contains("duty"), "{text}");
    assert!(!text.contains("[consumed:"), "{text}");
}

#[test]
fn journal_show_prints_body_and_resolves_cold_archive() {
    let repo = Repo::new();
    let body = repo.home.join("body.md");
    fs::write(&body, "# Show me\n\nexact body, verbatim\n").unwrap();
    let out = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "printable",
        "--kind",
        "note",
        "--body-file",
        body.to_str().unwrap(),
    ]));
    let file = PathBuf::from(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    let shown = stdout(repo.arc(&repo.root).args(["journal", "show", &file]));
    assert_eq!(shown, "# Show me\n\nexact body, verbatim\n");

    // After archiving, show falls back to the cold sibling.
    repo.arc(&repo.root)
        .args(["journal", "archive", &file])
        .assert()
        .success();
    let hot = journal_dir(&repo);
    assert!(!hot.join(&file).exists());
    let shown = stdout(repo.arc(&repo.root).args(["journal", "show", &file]));
    assert_eq!(shown, "# Show me\n\nexact body, verbatim\n");

    // With the same filename in both dirs, hot takes precedence.
    fs::write(hot.join(&file), "# Hotter\n").unwrap();
    let shown = stdout(repo.arc(&repo.root).args(["journal", "show", &file]));
    assert_eq!(shown, "# Hotter\n");
}

#[test]
fn journal_show_rejects_paths_and_unknown_names() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();

    for bad in ["../escape-note.md", "sub/dir-note.md"] {
        repo.arc(&repo.root)
            .args(["journal", "show", bad])
            .assert()
            .failure();
    }
    // Well-formed artifact name that does not exist: clean failure.
    repo.arc(&repo.root)
        .args(["journal", "show", "20260101T000000Z-ghost-note.md"])
        .assert()
        .failure();
    // Not an artifact name at all: clean failure, no panic.
    repo.arc(&repo.root)
        .args(["journal", "show", "events.jsonl"])
        .assert()
        .failure();
}

#[test]
fn journal_stamp_prints_house_format_matching_event_ts() {
    let repo = Repo::new();

    let before = stdout(repo.arc(&repo.root).args(["journal", "stamp"]));
    let before = before.trim();
    // House format: RFC 3339 seconds, Z suffix, no fractional part.
    assert!(before.ends_with('Z'), "{before}");
    assert!(!before.contains('.'), "{before}");
    let parsed = chrono::DateTime::parse_from_rfc3339(before).unwrap();
    // Within a small tolerance of the test's own clock.
    let skew = (chrono::Utc::now() - parsed.with_timezone(&chrono::Utc))
        .num_seconds()
        .abs();
    assert!(skew < 60, "stamp off by {skew}s: {before}");

    // An event written between two stamps carries a ts lexically between
    // them — the exact same spelling, so prose and log cross-grep.
    let body = repo.home.join("body.md");
    fs::write(&body, "x\n").unwrap();
    stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "stamped",
        "--kind",
        "note",
        "--body-file",
        body.to_str().unwrap(),
    ]));
    let after = stdout(repo.arc(&repo.root).args(["journal", "stamp"]));
    let after = after.trim();
    assert!(before <= after, "{before} !<= {after}");

    let dir = journal_dir(&repo);
    let events = journal_events(&dir);
    let ts = events[0]["ts"].as_str().unwrap();
    assert!(before <= ts && ts <= after, "{before} <= {ts} <= {after}");
    assert!(!ts.contains('.'), "{ts}");
}

#[test]
fn discussion_kind_rides_the_open_queue_and_promotes() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "naming",
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin("# What do we call it?\n"),
    );
    let file = PathBuf::from(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    // Open queue: primary section, not later.
    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    let open_files: Vec<&str> = open["open"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["file"].as_str().unwrap())
        .collect();
    assert!(open_files.contains(&file.as_str()), "{open}");
    assert_eq!(open["later"].as_array().unwrap().len(), 0, "{open}");

    // Enumerated by list --kind discussion.
    let listed = stdout(
        repo.arc(&repo.root)
            .args(["journal", "list", "--kind", "discussion"]),
    );
    assert!(listed.contains("naming"), "{listed}");

    // Promote: begin --from-journal accepts an open discussion and
    // consumes it superseded; a second begin is refused as consumed.
    repo.arc(&repo.root)
        .args(["begin", "naming", "--no-worktree", "--from-journal", &file])
        .assert()
        .success();
    let show = json_stdout(repo.arc(&repo.root).args(["show", "naming", "--json"]));
    assert_eq!(show["journal_ref"], file);
    let events = journal_events(&journal_dir(&repo));
    assert!(
        events.iter().any(|event| event["event"] == "consumed"
            && event["outcome"] == "superseded"
            && event["file"] == file),
        "{events:?}"
    );
    repo.arc(&repo.root)
        .args([
            "begin",
            "naming-again",
            "--no-worktree",
            "--from-journal",
            &file,
        ])
        .assert()
        .failure();
}

#[test]
fn journal_note_scaffold_records_template_and_prepends() {
    let repo = Repo::new();

    // Scaffold alone records the built-in template.
    let out = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "debate",
        "--kind",
        "discussion",
        "--scaffold",
        "discussion",
    ]));
    let path = PathBuf::from(out.trim());
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.contains("## Positions"), "{body}");
    assert!(body.contains("### Position (<model[#effort]"), "{body}");

    // With a body, the template is prepended ahead of it.
    let src = repo.home.join("position.md");
    fs::write(&src, "my own opening take\n").unwrap();
    let out = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "debate-two",
        "--kind",
        "discussion",
        "--scaffold",
        "discussion",
        "--body-file",
        src.to_str().unwrap(),
    ]));
    let body = fs::read_to_string(out.trim()).unwrap();
    let template_at = body.find("## Positions").unwrap();
    let take_at = body.find("my own opening take").unwrap();
    assert!(template_at < take_at, "{body}");
}

#[test]
fn journal_note_scaffold_repo_override_wins_and_unknown_bails() {
    let repo = Repo::new();
    let templates = repo.root.join(".arc/templates");
    fs::create_dir_all(&templates).unwrap();
    fs::write(templates.join("discussion.md"), "HOUSE STYLE\n").unwrap();

    let out = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "house",
        "--kind",
        "discussion",
        "--scaffold",
        "discussion",
    ]));
    let body = fs::read_to_string(out.trim()).unwrap();
    assert_eq!(body, "HOUSE STYLE\n");

    // Unknown scaffold: fails cleanly and writes nothing.
    let before = stdout(repo.arc(&repo.root).args(["journal", "list", "--json"]));
    repo.arc(&repo.root)
        .args([
            "journal",
            "note",
            "nope",
            "--kind",
            "note",
            "--scaffold",
            "no-such-scaffold",
        ])
        .assert()
        .failure();
    let after = stdout(repo.arc(&repo.root).args(["journal", "list", "--json"]));
    assert_eq!(before, after);
}

#[test]
fn journal_note_requires_a_body_source() {
    let repo = Repo::new();
    // Neither --body-file nor --scaffold: clap refuses before anything runs.
    repo.arc(&repo.root)
        .args(["journal", "note", "empty", "--kind", "note"])
        .assert()
        .failure();
    let listed = stdout(repo.arc(&repo.root).args(["journal", "list"]));
    assert!(listed.contains("(none)"), "{listed}");
}

#[test]
fn decision_records_without_joining_journal_open() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "chosen-name",
                "--kind",
                "decision",
                "--body-file",
                "-",
            ])
            .write_stdin("# Use arc\n"),
    );
    let file = PathBuf::from(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert!(
        !open["open"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["file"] == file),
        "{open}"
    );
}

#[test]
fn begin_from_journal_refuses_decision() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "chosen-name",
                "--kind",
                "decision",
                "--body-file",
                "-",
            ])
            .write_stdin("# Use arc\n"),
    );
    let file = PathBuf::from(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    repo.arc(&repo.root)
        .args([
            "begin",
            "chosen-name",
            "--no-worktree",
            "--from-journal",
            &file,
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not an actionable item"));
}

#[test]
fn consume_done_links_decision_in_discussion_resolution() {
    let repo = Repo::new();
    let decision = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "colors",
                "--kind",
                "decision",
                "--body-file",
                "-",
            ])
            .write_stdin("# Blue\n"),
    );
    let decision = PathBuf::from(decision.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let discussion = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "colors",
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin("# Which color?\n"),
    );
    let discussion = PathBuf::from(discussion.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    repo.arc(&repo.root)
        .args([
            "journal",
            "consume",
            &discussion,
            "--outcome",
            "done",
            "--decision",
            &decision,
        ])
        .assert()
        .success();
    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &discussion, "--json"]),
        );
    assert_eq!(summary["resolution"]["decision"], decision);
}

#[test]
fn invalid_decision_targets_leave_discussion_open() {
    let repo = Repo::new();
    let non_decision = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "background",
                "--kind",
                "note",
                "--body-file",
                "-",
            ])
            .write_stdin("# Context\n"),
    );
    let non_decision = PathBuf::from(non_decision.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let discussion = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "colors",
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin("# Which color?\n"),
    );
    let discussion = PathBuf::from(discussion.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    for target in [
        "20260101T000000Z-missing-decision.md",
        non_decision.as_str(),
        "nested/20260101T000000Z-colors-decision.md",
    ] {
        repo.arc(&repo.root)
            .args([
                "journal",
                "consume",
                &discussion,
                "--outcome",
                "done",
                "--decision",
                target,
            ])
            .assert()
            .failure();
        let summary = json_stdout(repo.arc(&repo.root).args([
            "journal",
            "discussion",
            &discussion,
            "--json",
        ]));
        assert!(summary.get("resolution").is_none(), "{summary}");
    }
}

#[test]
fn decision_can_resolve_discussion_with_different_topic() {
    let repo = Repo::new();
    let decision = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "shared-policy",
                "--kind",
                "decision",
                "--body-file",
                "-",
            ])
            .write_stdin("# Use blue everywhere\n"),
    );
    let decision = PathBuf::from(decision.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let discussion = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "button-color",
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin("# Which button color?\n"),
    );
    let discussion = PathBuf::from(discussion.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    repo.arc(&repo.root)
        .args([
            "journal",
            "consume",
            &discussion,
            "--outcome",
            "done",
            "--decision",
            &decision,
        ])
        .assert()
        .success();
}

#[test]
fn discussion_consumed_without_decision_still_reads() {
    let repo = Repo::new();
    let discussion = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "colors",
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin("# Which color?\n"),
    );
    let discussion = PathBuf::from(discussion.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    repo.arc(&repo.root)
        .args(["journal", "consume", &discussion, "--outcome", "done"])
        .assert()
        .success();
    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &discussion, "--json"]),
        );
    assert_eq!(summary["resolution"]["outcome"], "done");
    assert!(summary["resolution"].get("decision").is_none(), "{summary}");
}

#[test]
fn discussion_consume_done_records_decision_with_note() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "naming",
                "--kind",
                "discussion",
                "--scaffold",
                "discussion",
            ])
            .write_stdin("unused"),
    );
    let file = PathBuf::from(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    // Decided, no code: consume done with a pointer to the conclusion.
    repo.arc(&repo.root)
        .args([
            "journal",
            "consume",
            &file,
            "--outcome",
            "done",
            "--note",
            "settled: change over slice",
        ])
        .assert()
        .success();
    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert_eq!(open["open"].as_array().unwrap().len(), 0, "{open}");
    let events = journal_events(&journal_dir(&repo));
    let consumed = events
        .iter()
        .find(|event| event["event"] == "consumed" && event["file"] == file)
        .unwrap();
    assert_eq!(consumed["outcome"], "done");
    assert_eq!(consumed["note"], "settled: change over slice");
    // The decision stays visible in list with its outcome marker.
    let listed = stdout(repo.arc(&repo.root).args(["journal", "list"]));
    assert!(listed.contains("[consumed: done]"), "{listed}");
}

#[test]
fn journal_events_stamp_model_only_when_set() {
    let repo = Repo::new();
    let body = repo.home.join("body.md");
    fs::write(&body, "stamped\n").unwrap();

    // Via the flag.
    stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "stamped",
        "--kind",
        "note",
        "--body-file",
        body.to_str().unwrap(),
        "--model",
        "kimi-k3#high",
    ]));
    // Via the env var.
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "log", "stamped", "env path"])
            .env("ARC_MODEL", "gpt-5.6-sol#low"),
    );
    // Unset and empty both omit the field.
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "log", "stamped", "unset path"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "log", "stamped", "empty path", "--model", ""]),
    );

    let dir = journal_dir(&repo);
    let events = journal_events(&dir);
    assert_eq!(events.len(), 4);
    assert_eq!(events[0]["model"], "kimi-k3#high");
    assert_eq!(events[1]["model"], "gpt-5.6-sol#low");
    assert!(events[2].get("model").is_none());
    assert!(events[3].get("model").is_none());
    // Harness/session keep their existing behavior alongside.
    assert_eq!(events[0]["harness"], "test");
    assert_eq!(events[0]["session"], "session-a");
}

#[test]
fn journal_append_writes_position_block_and_typed_event() {
    let repo = Repo::new();
    // Seed a discussion to argue in.
    let seed = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "debate",
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin("# Debate\n\n## Positions\n"),
    );
    let file = PathBuf::from(seed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    // First position: model known, answering another position via --ref.
    let p1 = repo.home.join("p1.md");
    fs::write(&p1, "Position: for\nBecause reasons.\n").unwrap();
    repo.arc(&repo.root)
        .args([
            "journal",
            "append",
            &file,
            "--ref",
            "2026-01-01T00:00:00Z",
            "--body-file",
            p1.to_str().unwrap(),
        ])
        .env("ARC_HARNESS", "opencode")
        .env("ARC_MODEL", "kimi-k3#high")
        .assert()
        .success();

    // Second position: no model, no ref.
    repo.arc(&repo.root)
        .args(["journal", "append", &file, "--body-file", "-"])
        .env("ARC_HARNESS", "codex")
        .write_stdin("Position: against\nCounter.\n")
        .assert()
        .success();

    let dir = journal_dir(&repo);
    let body = fs::read_to_string(dir.join(&file)).unwrap();
    // Headings are tool-computed: every position gets a stable ULID-backed
    // reply target, plus model via harness when the model is known.
    assert!(
        body.contains("### Position pos-")
            && body.contains("(kimi-k3#high via opencode, 20")
            && body.contains("Because reasons."),
        "{body}"
    );
    assert!(body.contains("(codex, 20"), "{body}");
    assert!(!body.contains("via codex"), "{body}");

    // The typed events are the machine-readable half; ref and model are
    // recorded only when present.
    let events = journal_events(&dir);
    let positions: Vec<&serde_json::Value> =
        events.iter().filter(|e| e["event"] == "position").collect();
    assert_eq!(positions.len(), 2);
    assert_eq!(positions[0]["topic"], "debate");
    assert_eq!(positions[0]["file"], file);
    assert_eq!(positions[0]["model"], "kimi-k3#high");
    assert_eq!(positions[0]["ref"], "2026-01-01T00:00:00Z");
    let first_id = positions[0]["position_id"].as_str().unwrap();
    let second_id = positions[1]["position_id"].as_str().unwrap();
    assert!(first_id.starts_with("pos-") && first_id.len() == 30);
    assert!(second_id.starts_with("pos-") && second_id.len() == 30);
    assert_ne!(first_id, second_id);
    assert!(body.contains(first_id) && body.contains(second_id));
    assert!(positions[1].get("model").is_none());
    assert!(positions[1].get("ref").is_none());

    // A position event is a known event: doctor must not flag it.
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("unknown-jsonl-event").not());
}

#[test]
fn journal_append_rejects_paths_missing_files_and_non_artifacts() {
    let repo = Repo::new();
    // A path, not a bare filename.
    repo.arc(&repo.root)
        .args(["journal", "append", "sub/x.md", "--body-file", "-"])
        .write_stdin("x\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a path"));
    // A well-formed name that does not exist.
    repo.arc(&repo.root)
        .args([
            "journal",
            "append",
            "20260101T000000Z-ghost-discussion.md",
            "--body-file",
            "-",
        ])
        .write_stdin("x\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains("no such artifact"));
    // A name that is not artifact-shaped.
    repo.arc(&repo.root)
        .args(["journal", "append", "notes.md", "--body-file", "-"])
        .write_stdin("x\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a journal artifact name"));
}

#[test]
fn journal_append_rejects_consumed_artifact() {
    let repo = Repo::new();
    let seed = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "closed-debate",
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin("# Closed debate\n"),
    );
    let file = PathBuf::from(seed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    repo.arc(&repo.root)
        .args(["journal", "consume", &file, "--outcome", "done"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["journal", "append", &file, "--body-file", "-"])
        .write_stdin("Position: for\nToo late.\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "cannot append to consumed artifact",
        ));

    let dir = journal_dir(&repo);
    assert!(!fs::read_to_string(dir.join(file))
        .unwrap()
        .contains("Too late"));
    assert_eq!(
        journal_events(&dir)
            .iter()
            .filter(|event| event["event"] == "position")
            .count(),
        0
    );
}

#[test]
fn journal_open_annotates_item_age() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args([
            "journal",
            "note",
            "waiting",
            "--kind",
            "todo",
            "--body-file",
            "-",
        ])
        .write_stdin("x\n")
        .assert()
        .success();
    // Text: a fresh item reads "(<n>s old)"; JSON: a numeric age_seconds.
    repo.arc(&repo.root)
        .args(["journal", "open"])
        .assert()
        .success()
        .stdout(predicates::str::contains("s old)"));
    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert!(open["open"][0]["age_seconds"].as_u64().is_some(), "{open}");
}

#[test]
fn journal_open_uses_latest_position_activity_for_discussion_age() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    let file = "20000101T000000Z-active-debate-discussion.md";
    fs::write(dir.join(file), "# Active debate\n\n## Positions\n").unwrap();

    repo.arc(&repo.root)
        .args(["journal", "append", file, "--body-file", "-"])
        .write_stdin("Position: for\nFresh answer.\n")
        .assert()
        .success();

    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    let item = open["open"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["file"] == file)
        .unwrap();
    assert!(item["age_seconds"].as_u64().unwrap() < 60, "{item}");
    let summary = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "discussion", file, "--json"]),
    );
    assert!(summary["age_seconds"].as_u64().unwrap() < 60, "{summary}");
}

#[test]
fn journal_discussion_summarizes_stances_participants_and_resolution() {
    let repo = Repo::new();
    let seed = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "colors",
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin("# Colors\n\n## Positions\n"),
    );
    let file = PathBuf::from(seed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    // Two "for" (one with a model, session s1), one "against" (session s2).
    repo.arc(&repo.root)
        .args(["journal", "append", &file, "--body-file", "-"])
        .env("ARC_HARNESS", "opencode")
        .env("ARC_SESSION", "s1")
        .env("ARC_MODEL", "kimi-k3#high")
        .write_stdin("Position: for\nBlue.\n")
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "journal",
            "append",
            &file,
            "--ref",
            "2026-01-01T00:00:00Z",
            "--body-file",
            "-",
        ])
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", "s2")
        .write_stdin("Position: against\nRed.\n")
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["journal", "append", &file, "--body-file", "-"])
        .env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", "s1")
        .write_stdin("Position: for\nBlue again.\n")
        .assert()
        .success();

    // Resolve from session s1 — which authored a position — so the flag trips.
    repo.arc(&repo.root)
        .args(["journal", "consume", &file, "--outcome", "done"])
        .env("ARC_HARNESS", "opencode")
        .env("ARC_SESSION", "s1")
        .assert()
        .success();

    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    assert_eq!(summary["positions"], 3);
    assert_eq!(summary["stances"]["for"], 2);
    assert_eq!(summary["stances"]["against"], 1);
    assert_eq!(summary["stances"]["amend"], 0);
    // Three distinct sessions authored via journal append; one named a --ref.
    assert_eq!(summary["participants"].as_array().unwrap().len(), 3);
    assert_eq!(summary["reply_refs"], 1);
    assert_eq!(summary["resolution"]["outcome"], "done");
    assert_eq!(summary["resolution"]["resolver_participated"], true);

    // Text form names the resolver-participation.
    repo.arc(&repo.root)
        .args(["journal", "discussion", &file])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "resolver also authored a position",
        ));
}

fn discussion_fixture(repo: &Repo, topic: &str) -> String {
    let seed = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                topic,
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin("# Discussion\n\n## Positions\n"),
    );
    PathBuf::from(seed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string()
}

fn append_discussion_position(
    repo: &Repo,
    file: &str,
    reference: Option<&str>,
    harness: &str,
) -> String {
    let mut args = vec!["journal", "append", file];
    if let Some(reference) = reference {
        args.extend(["--ref", reference]);
    }
    args.extend(["--body-file", "-"]);
    repo.arc(&repo.root)
        .args(args)
        .env("ARC_HARNESS", harness)
        .write_stdin("Position: for\n")
        .assert()
        .success();
    journal_events(&journal_dir(repo))
        .into_iter()
        .rev()
        .find(|event| event["event"] == "position")
        .unwrap()["position_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn journal_discussion_groups_three_deep_chain_into_rounds() {
    let repo = Repo::new();
    let file = discussion_fixture(&repo, "deep-rounds");
    let first = append_discussion_position(&repo, &file, None, "claude");
    let second = append_discussion_position(&repo, &file, Some(&first), "codex");
    let third = append_discussion_position(&repo, &file, Some(&second), "opencode");

    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    assert_eq!(summary["rounds"][0]["depth"], 1);
    assert_eq!(
        summary["rounds"][0]["positions"],
        serde_json::json!([first])
    );
    assert_eq!(summary["rounds"][1]["depth"], 2);
    assert_eq!(
        summary["rounds"][1]["positions"],
        serde_json::json!([second])
    );
    assert_eq!(summary["rounds"][2]["depth"], 3);
    assert_eq!(
        summary["rounds"][2]["positions"],
        serde_json::json!([third])
    );
}

#[test]
fn journal_discussion_groups_sibling_replies_into_one_round() {
    let repo = Repo::new();
    let file = discussion_fixture(&repo, "sibling-round");
    let parent = append_discussion_position(&repo, &file, None, "claude");
    let first = append_discussion_position(&repo, &file, Some(&parent), "codex");
    let second = append_discussion_position(&repo, &file, Some(&parent), "opencode");

    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    assert_eq!(
        summary["rounds"][1]["positions"],
        serde_json::json!([first, second])
    );
}

#[test]
fn journal_discussion_reports_participants_per_round() {
    let repo = Repo::new();
    let file = discussion_fixture(&repo, "round-participants");
    let parent = append_discussion_position(&repo, &file, None, "claude");
    append_discussion_position(&repo, &file, Some(&parent), "codex");
    append_discussion_position(&repo, &file, Some(&parent), "opencode");

    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    assert_eq!(
        summary["rounds"][0]["participants"],
        serde_json::json!(["claude"])
    );
    assert_eq!(
        summary["rounds"][1]["participants"],
        serde_json::json!(["codex", "opencode"])
    );
}

#[test]
fn journal_discussion_lists_exactly_unanswered_leaf_positions() {
    let repo = Repo::new();
    let file = discussion_fixture(&repo, "unanswered-leaves");
    let parent = append_discussion_position(&repo, &file, None, "claude");
    let first_leaf = append_discussion_position(&repo, &file, Some(&parent), "codex");
    let second_leaf = append_discussion_position(&repo, &file, Some(&parent), "opencode");

    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    assert_eq!(
        summary["unanswered"],
        serde_json::json!([first_leaf, second_leaf])
    );
}

#[test]
fn journal_discussion_places_positions_without_refs_in_round_one() {
    let repo = Repo::new();
    let file = discussion_fixture(&repo, "root-positions");
    let first = append_discussion_position(&repo, &file, None, "claude");
    let second = append_discussion_position(&repo, &file, None, "codex");

    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    assert_eq!(summary["rounds"].as_array().unwrap().len(), 1);
    assert_eq!(summary["rounds"][0]["depth"], 1);
    assert_eq!(
        summary["rounds"][0]["positions"],
        serde_json::json!([first, second])
    );
}

#[test]
fn journal_discussion_bounds_ref_cycles() {
    let repo = Repo::new();
    let file = discussion_fixture(&repo, "cyclic-replies");
    let first = append_discussion_position(&repo, &file, None, "claude");
    let second = append_discussion_position(&repo, &file, Some(&first), "codex");
    let dir = journal_dir(&repo);
    let events_path = dir.join("events.jsonl");
    let mut events = journal_events(&dir);
    events
        .iter_mut()
        .find(|event| event["position_id"] == first)
        .unwrap()["ref"] = serde_json::json!(second);
    let contents = events
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(events_path, format!("{contents}\n")).unwrap();

    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    assert_eq!(summary["rounds"].as_array().unwrap().len(), 1);
    assert_eq!(summary["rounds"][0]["depth"], 1);
}

#[test]
fn journal_discussion_scopes_stances_to_position_blocks() {
    let repo = Repo::new();
    let seed = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "stance-scope",
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin(
                "# Stance scope\n\nAn example outside a position says Position: for.\n\nPosition: for\n",
            ),
    );
    let file = PathBuf::from(seed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    repo.arc(&repo.root)
        .args(["journal", "append", &file, "--body-file", "-"])
        .write_stdin("Position: against\nPosition: for is quoted below, not a second vote.\n")
        .assert()
        .success();

    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    assert_eq!(summary["positions"], 1);
    assert_eq!(summary["stances"]["for"], 0);
    assert_eq!(summary["stances"]["against"], 1);
    assert_eq!(summary["stances"]["other"], 0);
}

#[test]
fn resolver_participation_requires_matching_harness_and_session() {
    let repo = Repo::new();
    let seed = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "native-identity",
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin("# Native identity\n"),
    );
    let file = PathBuf::from(seed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    repo.arc(&repo.root)
        .args(["journal", "append", &file, "--body-file", "-"])
        .env("ARC_HARNESS", "opencode")
        .env("ARC_SESSION", "same-native-id")
        .write_stdin("Position: for\nOpenCode position.\n")
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["journal", "consume", &file, "--outcome", "done"])
        .env("ARC_HARNESS", "pi")
        .env("ARC_SESSION", "same-native-id")
        .assert()
        .success();

    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    assert_eq!(summary["resolution"]["resolver_participated"], false);
}

#[test]
fn journal_discussion_rejects_non_discussion_kinds() {
    let repo = Repo::new();
    let seed = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "plain",
                "--kind",
                "todo",
                "--body-file",
                "-",
            ])
            .write_stdin("x\n"),
    );
    let file = PathBuf::from(seed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    repo.arc(&repo.root)
        .args(["journal", "discussion", &file])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a discussion"));
}

#[test]
fn begin_from_journal_plan_seeds_no_brief() {
    let repo = Repo::new();
    let seed = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "roadmap",
                "--kind",
                "plan",
                "--body-file",
                "-",
            ])
            .write_stdin("# Roadmap\n\nBuild two changes.\n"),
    );
    let file = PathBuf::from(seed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    repo.arc(&repo.root)
        .args([
            "begin",
            "roadmap-member",
            "--no-worktree",
            "--from-journal",
            &file,
        ])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args(["brief", "roadmap-member"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no brief recorded"));
}
