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
    fs::read_to_string(dir.join("journal.jsonl"))
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
        assert_eq!(event["schema"], "thread-journal/1");
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
    assert!(!dir.join("journal.md").exists());
}

#[test]
fn journal_legacy_journal_md_still_read() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    let file = "20260101T000000Z-legacy-todo.md";
    fs::write(dir.join(file), "# Legacy\n").unwrap();
    let legacy = format!("- 2026-01-01T00:00:00Z old legacy legacy: Legacy ({file})\n- 2026-01-01T00:01:00Z old legacy legacy: consumed {file} [done]\n- 2026-01-01T00:02:00Z old legacy lane-a: lane opened [2h] scope=legacy: working\n");
    fs::write(dir.join("journal.md"), &legacy).unwrap();
    let open = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(!open.contains(file), "{open}");
    let lanes = stdout(repo.arc(&repo.root).args(["journal", "lane", "list"]));
    assert!(lanes.contains("lane-a"), "{lanes}");
    let second = "20260101T000100Z-second-todo.md";
    fs::write(dir.join(second), "# Second\n").unwrap();
    repo.arc(&repo.root)
        .args(["journal", "consume", second])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(dir.join("journal.md")).unwrap(), legacy);
    assert_eq!(journal_events(&dir).last().unwrap()["event"], "consumed");
}

#[test]
fn journal_events_emits_merged_ndjson() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("journal.md"),
        "- 2026-01-01T00:00:00Z old legacy topic-a: old message\n",
    )
    .unwrap();
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

    // Default: <ai_home>/threads/<repo-root-slug>.
    let expected_default = repo
        .home
        .join(".local/ai/threads")
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
            "[threads]\ndirs = {{ \"{}\" = \"{}\" }}\n",
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
            .env("ARC_THREAD_DIR", &env_dir)
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
    let env_cold = stdout(repo.arc(&repo.root).env("ARC_THREAD_DIR", &env_hot).args([
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
    assert_eq!(entries, vec!["journal.jsonl".to_string()]);
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
    fs::write(
        dir.join("journal.md"),
        "- 2026-01-01T00:00:00Z test old topic-a: prior journal line\n",
    )
    .unwrap();

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
    let after_first = fs::read(dir.join("journal.jsonl")).unwrap();

    repo.arc(&repo.root)
        .args(["journal", "log", "topic-b", "second message"])
        .assert()
        .success();
    let after_second = fs::read(dir.join("journal.jsonl")).unwrap();

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
fn journal_archive_moves_records_and_catchup_reads_cold_with_hot_journal() {
    let repo = Repo::new();
    let hot = journal_dir(&repo);
    fs::create_dir_all(&hot).unwrap();
    let name = "20260101T000000Z-history-note.md";
    fs::write(hot.join(name), "# History\n").unwrap();
    fs::write(
        hot.join("journal.md"),
        "- 2026-01-01T00:00:00Z test old history: prior hot journal line\n",
    )
    .unwrap();

    repo.arc(&repo.root)
        .args(["journal", "archive", name, "--note", "cold storage"])
        .assert()
        .success();
    let cold = PathBuf::from(format!("{}-archive", hot.display()));
    assert!(!hot.join(name).exists());
    assert!(cold.join(name).is_file());
    assert!(hot.join("journal.md").is_file());
    assert!(!cold.join("journal.md").exists());

    let hot_catchup = stdout(repo.arc(&repo.root).args(["journal", "catchup"]));
    assert!(!hot_catchup.contains("history  note"), "{hot_catchup}");
    let cold_catchup = stdout(
        repo.arc(&repo.root)
            .args(["journal", "catchup", "--archived"]),
    );
    assert!(cold_catchup.contains("history  note"), "{cold_catchup}");
    assert!(
        cold_catchup.contains("prior hot journal line"),
        "{cold_catchup}"
    );
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
    assert!(!hot.join("journal.md").exists());

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
    let journal = dir.join("journal.jsonl");
    let mut contents = fs::read_to_string(&journal).unwrap();
    contents.push_str("not json\n");
    contents.push_str(
        r#"{"schema":"thread-journal/1","ts":"2026-07-18T00:00:00Z","harness":"test","session":"session-a","topic":"unknown","event":"bogus"}"#,
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
        dir.join("journal.jsonl"),
        "{\"schema\":\"thread-journal/1\",\"ts\":\"2026-01-01T00:00:00Z\",\"harness\":\"h\",\"session\":\"s\",\"topic\":\"tidy\",\"event\":\"archived\",\"file\":\"prose with spaces\"}\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("problems:\n  (none)"));
}

/// The legacy spellings stay wired as aliases: `arc thread` for the
/// subcommand group and `journal` for the log-only append.
#[test]
fn journal_thread_and_nested_journal_aliases_still_work() {
    let repo = Repo::new();
    let via_alias = stdout(repo.arc(&repo.root).args(["thread", "dir"]));
    let via_primary = stdout(repo.arc(&repo.root).args(["journal", "dir"]));
    assert_eq!(via_alias, via_primary);

    repo.arc(&repo.root)
        .args(["thread", "journal", "compat", "old spelling still lands"])
        .assert()
        .success();
    let events = journal_events(&journal_dir(&repo));
    assert_eq!(events.last().unwrap()["event"], "log");
    assert_eq!(events.last().unwrap()["topic"], "compat");
}
