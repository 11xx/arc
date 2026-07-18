use super::common::*;

fn thread_slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

fn thread_dir(repo: &Repo) -> PathBuf {
    let out = stdout(repo.arc(&repo.root).args(["thread", "dir"]));
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
fn thread_journal_writes_typed_jsonl_events() {
    let repo = Repo::new();
    let body = repo.home.join("body.md");
    fs::write(&body, "work\n").unwrap();
    let output = stdout(repo.arc(&repo.root).args([
        "thread",
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
        .args(["thread", "journal", "typed", "progress"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "thread",
            "consume",
            &file,
            "--outcome",
            "done",
            "--note",
            "finished",
        ])
        .assert()
        .success();
    let dir = thread_dir(&repo);
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
fn thread_legacy_journal_md_still_read() {
    let repo = Repo::new();
    let dir = thread_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    let file = "20260101T000000Z-legacy-todo.md";
    fs::write(dir.join(file), "# Legacy\n").unwrap();
    let legacy = format!("- 2026-01-01T00:00:00Z old legacy legacy: Legacy ({file})\n- 2026-01-01T00:01:00Z old legacy legacy: consumed {file} [done]\n- 2026-01-01T00:02:00Z old legacy lane-a: lane opened [2h] scope=legacy: working\n");
    fs::write(dir.join("journal.md"), &legacy).unwrap();
    let open = stdout(repo.arc(&repo.root).args(["thread", "open"]));
    assert!(!open.contains(file), "{open}");
    let lanes = stdout(repo.arc(&repo.root).args(["thread", "lane", "list"]));
    assert!(lanes.contains("lane-a"), "{lanes}");
    let second = "20260101T000100Z-second-todo.md";
    fs::write(dir.join(second), "# Second\n").unwrap();
    repo.arc(&repo.root)
        .args(["thread", "consume", second])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(dir.join("journal.md")).unwrap(), legacy);
    assert_eq!(journal_events(&dir).last().unwrap()["event"], "consumed");
}

#[test]
fn thread_events_emits_merged_ndjson() {
    let repo = Repo::new();
    let dir = thread_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("journal.md"),
        "- 2026-01-01T00:00:00Z old legacy topic-a: old message\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["thread", "journal", "topic-a", "new message"])
        .assert()
        .success();
    let output = stdout(repo.arc(&repo.root).args(["thread", "events"]));
    let events: Vec<serde_json::Value> = output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["message"], "old message");
    assert_eq!(events[1]["message"], "new message");
}

#[test]
fn thread_catchup_renders_events_as_human_lines() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["thread", "journal", "topic-a", "human message"])
        .assert()
        .success();
    let output = stdout(repo.arc(&repo.root).args(["thread", "catchup"]));
    assert!(
        output.contains("- 20") && output.contains(" test session-a topic-a: human message"),
        "{output}"
    );
}

#[test]
fn thread_dir_precedence_env_over_config_over_default() {
    let repo = Repo::new();
    let canon = fs::canonicalize(&repo.root).unwrap();

    // Default: <ai_home>/threads/<repo-root-slug>.
    let expected_default = repo
        .home
        .join(".local/ai/threads")
        .join(thread_slug(&canon));
    let got_default = thread_dir(&repo);
    assert_eq!(got_default, expected_default);

    // Config override keyed by the repository-root path.
    let cfg_dir = repo.home.join(".local/ai/arc");
    fs::create_dir_all(&cfg_dir).unwrap();
    let override_dir = repo.home.join("custom-thread-archive");
    fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "[threads]\ndirs = {{ \"{}\" = \"{}\" }}\n",
            canon.display(),
            override_dir.display()
        ),
    )
    .unwrap();
    let got_config = thread_dir(&repo);
    assert_eq!(got_config, override_dir);

    // Env wins over both config and default.
    let env_dir = repo.home.join("env-thread-archive");
    let out = stdout(
        repo.arc(&repo.root)
            .env("ARC_THREAD_DIR", &env_dir)
            .args(["thread", "dir"]),
    );
    assert_eq!(PathBuf::from(out.trim()), env_dir);

    // dir prints but never creates the directory.
    assert!(!got_default.exists());
    assert!(!override_dir.exists());
}

#[test]
fn thread_dir_archive_prints_cold_sibling_and_respects_env() {
    let repo = Repo::new();
    let hot = thread_dir(&repo);
    let cold = stdout(repo.arc(&repo.root).args(["thread", "dir", "--archive"]));
    assert_eq!(
        PathBuf::from(cold.trim()),
        PathBuf::from(format!("{}-archive", hot.display()))
    );

    let env_hot = repo.home.join("custom-hot");
    let env_cold = stdout(repo.arc(&repo.root).env("ARC_THREAD_DIR", &env_hot).args([
        "thread",
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
fn thread_note_writes_file_and_journal_line() {
    let repo = Repo::new();
    let body_path = repo.home.join("body.md");
    let body = "# My Heading\n\nverbatim body\n";
    fs::write(&body_path, body).unwrap();

    let out = stdout(repo.arc(&repo.root).args([
        "thread",
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
fn thread_note_title_prepends_heading() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "thread",
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
fn thread_note_rejects_invalid_kind_and_topic_without_writing() {
    let repo = Repo::new();
    let dir = thread_dir(&repo);
    let body_path = repo.home.join("body.md");
    fs::write(&body_path, "body\n").unwrap();

    // Invalid kind is a clap usage error (exit 2).
    repo.arc(&repo.root)
        .args([
            "thread",
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
            "thread",
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
fn thread_journal_appends_without_creating_artifact_file() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["thread", "journal", "topic-a", "consumed inbox X"])
        .assert()
        .success();
    let dir = thread_dir(&repo);
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
fn thread_catchup_lists_newest_first_and_json_parses() {
    let repo = Repo::new();
    let dir = thread_dir(&repo);
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
    let text = stdout(repo.arc(&repo.root).args(["thread", "catchup"]));
    let beta = text.find("beta").unwrap();
    let alpha = text.find("alpha").unwrap();
    assert!(beta < alpha, "beta (newer) must list before alpha:\n{text}");

    // JSON form parses and preserves order + parsed fields.
    let json = stdout(repo.arc(&repo.root).args(["thread", "catchup", "--json"]));
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
            .args(["thread", "catchup", "--limit", "1", "--json"]),
    );
    let lv: serde_json::Value = serde_json::from_str(limited.trim()).unwrap();
    assert_eq!(lv["files"].as_array().unwrap().len(), 1);
    assert_eq!(lv["files"][0]["topic"], "beta");
}

#[test]
fn thread_journal_is_append_only() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["thread", "journal", "topic-a", "first message"])
        .assert()
        .success();
    let dir = thread_dir(&repo);
    let after_first = fs::read(dir.join("journal.jsonl")).unwrap();

    repo.arc(&repo.root)
        .args(["thread", "journal", "topic-b", "second message"])
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

/// `thread open` lists unconsumed primary actionable kinds (todo/handoff/
/// inbox/plan) before lower-priority later items; `thread consume` retires
/// either through a machine-readable journal line and refuses double consumption.
#[test]
fn thread_open_and_consume_track_actionable_items() {
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
            "thread",
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
    let open = stdout(repo.arc(&repo.root).args(["thread", "open"]));
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
            "thread",
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
        repo.arc(&repo.root).args(["thread", "open", "--json"]),
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
            .args(["thread", "open", "--kind", "todo", "--json"]),
    ))
    .unwrap();
    assert_eq!(filtered["open"][0]["file"], names["todo"]);
    assert!(filtered["later"].as_array().unwrap().is_empty());
    let filtered: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["thread", "open", "--kind", "later", "--json"]),
    ))
    .unwrap();
    assert!(filtered["open"].as_array().unwrap().is_empty());
    assert_eq!(filtered["later"][0]["file"], names["later"]);
    let filtered_text = stdout(
        repo.arc(&repo.root)
            .args(["thread", "open", "--kind", "later"]),
    );
    assert!(
        filtered_text.contains("open items (newest first):\n  (none)\nlater items (newest first):"),
        "{filtered_text}"
    );
    repo.arc(&repo.root)
        .args(["thread", "open", "--kind", "note"])
        .assert()
        .failure();

    // A later item consumes just like an item in the primary queue.
    repo.arc(&repo.root)
        .args(["thread", "consume", &names["later"]])
        .assert()
        .success();
    let after_later = stdout(repo.arc(&repo.root).args(["thread", "open"]));
    assert!(after_later.contains("later items (newest first):\n  (none)"));

    // Prose mentioning a filename near "consumed" is not the machine shape
    // and must not retire the item.
    repo.arc(&repo.root)
        .args([
            "thread",
            "journal",
            "next-work",
            &format!("discussed consumed {} in passing", names["todo"]),
        ])
        .assert()
        .success();
    let still_open = stdout(repo.arc(&repo.root).args(["thread", "open"]));
    assert!(still_open.contains("next-work"), "{still_open}");

    // Even the full machine shape quoted mid-sentence must not consume:
    // the marker has to open the journal message field.
    repo.arc(&repo.root)
        .args([
            "thread",
            "journal",
            "next-work",
            &format!("reviewed consumed {} [done] but rejected it", names["todo"]),
        ])
        .assert()
        .success();
    let still_open = stdout(repo.arc(&repo.root).args(["thread", "open"]));
    assert!(still_open.contains("next-work"), "{still_open}");

    // Exclusive creation: recreating the same timestamped path fails loudly
    // instead of overwriting a queued artifact.
    let clash = stdout(repo.arc(&repo.root).args([
        "thread",
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
    let manual = thread_dir(&repo).join(&clash_name);
    assert!(manual.is_file());
    // A direct second create of the identical path (what a same-second
    // duplicate note would attempt) must be refused by exclusive creation.
    assert!(fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manual)
        .is_err());

    let events = journal_events(&thread_dir(&repo));
    let consumed = events
        .iter()
        .find(|event| event["event"] == "consumed")
        .unwrap();
    assert_eq!(consumed["file"], names["handoff"]);
    assert_eq!(consumed["outcome"], "superseded");
    assert_eq!(consumed["note"], "folded");

    // Guards: double consume, unknown artifact, and paths are refused.
    repo.arc(&repo.root)
        .args(["thread", "consume", &names["handoff"]])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["thread", "consume", "20990101T000000Z-ghost-todo.md"])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["thread", "consume", "sub/dir-file-todo.md"])
        .assert()
        .failure();
}

#[test]
fn thread_archive_moves_records_and_catchup_reads_cold_with_hot_journal() {
    let repo = Repo::new();
    let hot = thread_dir(&repo);
    fs::create_dir_all(&hot).unwrap();
    let name = "20260101T000000Z-history-note.md";
    fs::write(hot.join(name), "# History\n").unwrap();
    fs::write(
        hot.join("journal.md"),
        "- 2026-01-01T00:00:00Z test old history: prior hot journal line\n",
    )
    .unwrap();

    repo.arc(&repo.root)
        .args(["thread", "archive", name, "--note", "cold storage"])
        .assert()
        .success();
    let cold = PathBuf::from(format!("{}-archive", hot.display()));
    assert!(!hot.join(name).exists());
    assert!(cold.join(name).is_file());
    assert!(hot.join("journal.md").is_file());
    assert!(!cold.join("journal.md").exists());

    let hot_catchup = stdout(repo.arc(&repo.root).args(["thread", "catchup"]));
    assert!(!hot_catchup.contains("history  note"), "{hot_catchup}");
    let cold_catchup = stdout(
        repo.arc(&repo.root)
            .args(["thread", "catchup", "--archived"]),
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
fn thread_archive_refuses_unconsumed_later_then_accepts_consumed() {
    let repo = Repo::new();
    let hot = thread_dir(&repo);
    fs::create_dir_all(&hot).unwrap();
    let name = "20260101T000000Z-next-later.md";
    fs::write(hot.join(name), "later\n").unwrap();

    repo.arc(&repo.root)
        .args(["thread", "archive", name])
        .assert()
        .failure();
    assert!(hot.join(name).is_file());
    assert!(!hot.join("journal.md").exists());

    repo.arc(&repo.root)
        .args(["thread", "consume", name])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["thread", "archive", name])
        .assert()
        .success();
    assert!(!hot.join(name).exists());
    assert!(PathBuf::from(format!("{}-archive", hot.display()))
        .join(name)
        .is_file());
}

#[test]
fn thread_archive_consumed_bulk_filters_age_and_rejects_flag_misuse() {
    let repo = Repo::new();
    let hot = thread_dir(&repo);
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
            .args(["thread", "consume", name])
            .assert()
            .success();
    }

    let output = stdout(repo.arc(&repo.root).args([
        "thread",
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
        .args(["thread", "archive", "--older-than-days", "30"])
        .assert()
        .code(2);
    repo.arc(&repo.root)
        .args(["thread", "archive", new, "--consumed"])
        .assert()
        .code(2);
}

#[test]
fn thread_archive_refuses_cold_name_collision_without_moving_source() {
    let repo = Repo::new();
    let hot = thread_dir(&repo);
    let cold = PathBuf::from(format!("{}-archive", hot.display()));
    fs::create_dir_all(&hot).unwrap();
    fs::create_dir_all(&cold).unwrap();
    let name = "20200101T000000Z-history-note.md";
    fs::write(hot.join(name), "hot\n").unwrap();
    fs::write(cold.join(name), "cold\n").unwrap();

    repo.arc(&repo.root)
        .args(["thread", "archive", name])
        .assert()
        .failure();
    assert_eq!(fs::read_to_string(hot.join(name)).unwrap(), "hot\n");
    assert_eq!(fs::read_to_string(cold.join(name)).unwrap(), "cold\n");
}

#[test]
fn thread_lane_open_writes_marker_and_list_shows_live() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args([
            "thread",
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
    let event = journal_events(&thread_dir(&repo)).pop().unwrap();
    assert_eq!(event["event"], "lane-opened");
    assert_eq!(event["ttl_seconds"], 1800);
    assert_eq!(event["scope"], serde_json::json!(["topic-a", "topic-b"]));
    assert_eq!(event["status"], "implementing");

    let text = stdout(repo.arc(&repo.root).args(["thread", "lane", "list"]));
    assert!(text.contains("work-a  test session-a  live"), "{text}");
    assert!(text.contains("+scope: topic-a, topic-b"), "{text}");
    assert!(text.contains("implementing"), "{text}");

    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["thread", "lane", "list", "--json"]),
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
fn thread_lane_requires_session_identity() {
    let repo = Repo::new();
    let dir = thread_dir(&repo);
    repo.arc(&repo.root)
        .env_remove("ARC_SESSION")
        .args(["thread", "lane", "open", "work-a"])
        .assert()
        .failure();
    assert!(!dir.exists());
}

#[test]
fn thread_lane_rule_of_one_implicit_close() {
    let repo = Repo::new();
    for topic in ["lane-a", "lane-b"] {
        repo.arc(&repo.root)
            .args(["thread", "lane", "open", topic])
            .assert()
            .success();
    }
    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["thread", "lane", "list", "--json"]),
    ))
    .unwrap();
    assert_eq!(value["lanes"].as_array().unwrap().len(), 1);
    assert_eq!(value["lanes"][0]["topic"], "lane-b");
}

#[test]
fn thread_lane_renew_owner_only_and_updates_ttl() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["thread", "lane", "open", "work-a"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["thread", "lane", "renew", "work-a", "--ttl", "45m"])
        .assert()
        .success();
    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["thread", "lane", "list", "--json"]),
    ))
    .unwrap();
    assert_eq!(value["lanes"][0]["ttl_seconds"], 2700);
    repo.arc(&repo.root)
        .env("ARC_SESSION", "session-b")
        .args(["thread", "lane", "renew", "work-a"])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["thread", "lane", "renew", "unknown"])
        .assert()
        .failure();
}

#[test]
fn thread_lane_close_owner_and_takeover_semantics() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["thread", "lane", "open", "done-lane"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["thread", "lane", "close", "done-lane"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["thread", "lane", "close", "done-lane"])
        .assert()
        .failure();

    repo.arc(&repo.root)
        .args(["thread", "lane", "open", "takeover", "--ttl", "1s"])
        .assert()
        .success();
    let live_conflict = repo
        .arc(&repo.root)
        .env("ARC_SESSION", "session-b")
        .args([
            "thread",
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
            "thread",
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
fn thread_lane_liveness_refreshes_from_any_owner_journal_line() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["thread", "lane", "open", "work-a", "--ttl", "1s"])
        .assert()
        .success();
    thread::sleep(Duration::from_secs(2));
    repo.arc(&repo.root)
        .args(["thread", "journal", "other-topic", "still active"])
        .assert()
        .success();
    let text = stdout(repo.arc(&repo.root).args(["thread", "lane", "list"]));
    assert!(text.contains("work-a  test session-a  live"), "{text}");
}

#[test]
fn thread_open_annotates_items_covered_by_live_lanes() {
    let repo = Repo::new();
    let dir = thread_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("20260101T000000Z-covered-todo.md"), "# Covered\n").unwrap();
    fs::write(dir.join("20260101T000001Z-free-todo.md"), "# Free\n").unwrap();
    repo.arc(&repo.root)
        .env("ARC_SESSION", "external-session")
        .args([
            "thread",
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

    let text = stdout(repo.arc(&repo.root).args(["thread", "open"]));
    assert!(
        text.contains("covered  todo  # Covered [lane: external-lane — test external, external]"),
        "{text}"
    );
    assert!(!text.contains("# Free [lane:"), "{text}");
    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["thread", "open", "--json"]),
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
    let stale = stdout(repo.arc(&repo.root).args(["thread", "open"]));
    assert!(!stale.contains("[lane:"), "{stale}");
}

#[test]
fn thread_catchup_shows_lanes_block() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["thread", "lane", "open", "work-a"])
        .assert()
        .success();
    let text = stdout(repo.arc(&repo.root).args(["thread", "catchup"]));
    assert!(text.starts_with("lanes:\n"), "{text}");
    assert!(text.find("lanes:").unwrap() < text.find("artifacts (newest first):").unwrap());
    let value: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["thread", "catchup", "--json"]),
    ))
    .unwrap();
    assert_eq!(value["lanes"][0]["topic"], "work-a");
}
