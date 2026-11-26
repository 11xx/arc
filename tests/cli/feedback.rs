use super::common::*;

fn journal_events(dir: &Path) -> Vec<serde_json::Value> {
    let text = fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn init_registered_repo(repo: &Repo, name: &str) -> PathBuf {
    let dir = repo.home.join("projects").join(name);
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-b", "master"]);
    git(&dir, &["config", "user.name", "Fixture"]);
    git(&dir, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(dir.join("README.md"), format!("# {name}\n")).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-m", "initial"]);
    // A journal write registers the project binding in the workspace registry.
    repo.arc(&dir)
        .args(["journal", "log", "bootstrap", "register project"])
        .assert()
        .success();
    dir
}

#[test]
fn feedback_files_from_outside_any_repo_into_registered_project_with_default_scaffold() {
    let repo = Repo::new();
    let tapes_repo = init_registered_repo(&repo, "tapes");
    let scratch = repo.home.join("outside-git-scratch");
    fs::create_dir_all(&scratch).unwrap();

    // File from a directory outside any Git repository into `tapes` by label.
    let out = stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "tapes",
                "--key",
                "flag-typo",
                "--title",
                "Confusing flag name",
                "--body-file",
                "-",
            ])
            .write_stdin("Tried `tapes --bad-flag`; expected help, got silent exit.\n"),
    );
    let filed_path = PathBuf::from(out.trim());
    assert!(
        filed_path.is_file(),
        "expected file at {}",
        filed_path.display()
    );
    let file_name = filed_path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        file_name.ends_with("-flag-typo-feedback.md"),
        "unexpected filename: {file_name}"
    );

    let tapes_journal =
        PathBuf::from(stdout(repo.arc(&tapes_repo).args(["journal", "dir"])).trim());
    assert_eq!(filed_path.parent().unwrap(), tapes_journal.as_path());

    let content = fs::read_to_string(&filed_path).unwrap();
    assert!(content.starts_with("# Confusing flag name\n"), "{content}");
    assert!(content.contains("## Report"), "{content}");
    assert!(content.contains("## Shape"), "{content}");
    assert!(
        content.contains("Tool and build (`--version`)"),
        "{content}"
    );
    assert!(content.contains("Expected vs actual"), "{content}");
    assert!(
        content.contains("Tried `tapes --bad-flag`; expected help, got silent exit."),
        "{content}"
    );

    // `--no-scaffold` omits the template boilerplate and keeps only the body.
    let raw_out = stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "tapes",
                "--title",
                "Raw report without key",
                "--no-scaffold",
                "--body-file",
                "-",
            ])
            .write_stdin("Just the raw observation.\n"),
    );
    let raw_path = PathBuf::from(raw_out.trim());
    let raw_name = raw_path.file_name().unwrap().to_string_lossy().to_string();
    assert!(
        raw_name.ends_with("-raw-report-without-key-feedback.md"),
        "unexpected derived topic filename: {raw_name}"
    );
    let raw_content = fs::read_to_string(&raw_path).unwrap();
    assert!(!raw_content.contains("## Shape"), "{raw_content}");
    assert!(
        raw_content.contains("Just the raw observation."),
        "{raw_content}"
    );
}

#[test]
fn feedback_unknown_project_refuses_and_lists_registered_names() {
    let repo = Repo::new();
    let _alpha = init_registered_repo(&repo, "alpha-tool");
    let _beta = init_registered_repo(&repo, "beta-tool");
    let scratch = repo.home.join("outside-scratch");
    fs::create_dir_all(&scratch).unwrap();

    let output = repo
        .arc(&scratch)
        .args([
            "feedback",
            "nonexistent-tool",
            "--title",
            "Missing",
            "--body-file",
            "-",
        ])
        .write_stdin("body\n")
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8_lossy(&output);
    assert!(
        stderr.contains("unknown project \"nonexistent-tool\""),
        "{stderr}"
    );
    assert!(stderr.contains("alpha-tool"), "{stderr}");
    assert!(stderr.contains("beta-tool"), "{stderr}");
}

#[test]
fn feedback_key_deduplicates_across_sessions_and_appends_evidence() {
    let repo = Repo::new();
    let target = init_registered_repo(&repo, "target-tool");
    let scratch = repo.home.join("scratch");
    fs::create_dir_all(&scratch).unwrap();

    // Session 1 files the initial report under `--key slow-flag` with `--route upstream`.
    let created = json_stdout(
        repo.arc(&scratch)
            .args([
                "--actor",
                "agent-one",
                "--harness",
                "claude",
                "--session",
                "sess-1",
                "feedback",
                "target-tool",
                "--key",
                "slow-flag",
                "--title",
                "Slow flag hangs",
                "--route",
                "upstream",
                "--source",
                "harness=claude session=sess-1 ts=2026-11-26T10:00:00Z",
                "--item-key",
                "obs-1",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("First encounter: hung for 30s.\n"),
    );
    assert_eq!(created["schema"], "arc-feedback/1");
    assert_eq!(created["action"], "created");
    assert_eq!(created["key"], "slow-flag");
    assert_eq!(created["route"], "upstream");
    let first_file = created["file"].as_str().unwrap().to_string();
    let first_path = PathBuf::from(created["path"].as_str().unwrap());

    // Session 2 hits the same key from another directory and appends evidence instead of duplicating.
    let appended_text = stdout(
        repo.arc(&scratch)
            .args([
                "--actor",
                "agent-two",
                "--harness",
                "codex",
                "--session",
                "sess-2",
                "feedback",
                "target-tool",
                "--key",
                "slow-flag",
                "--title",
                "Reproduced on CI",
                "--source",
                "harness=codex session=sess-2 ts=2026-11-26T11:00:00Z",
                "--item-key",
                "obs-2",
                "--body-file",
                "-",
            ])
            .write_stdin("Second encounter: reproduced with --verbose.\n"),
    );
    assert!(
        appended_text.contains("appended evidence evd-")
            && appended_text.contains(&first_file)
            && appended_text.contains("(key: slow-flag)"),
        "{appended_text}"
    );

    // Repeating session 2's exact `(source, item_key)` is idempotent and appends nothing.
    let repeat = stdout(
        repo.arc(&scratch)
            .args([
                "--actor",
                "agent-two",
                "--harness",
                "codex",
                "--session",
                "sess-2",
                "feedback",
                "target-tool",
                "--key",
                "slow-flag",
                "--source",
                "harness=codex session=sess-2 ts=2026-11-26T11:00:00Z",
                "--item-key",
                "obs-2",
                "--body-file",
                "-",
            ])
            .write_stdin("Should not be written.\n"),
    );
    assert!(
        repeat.contains("existing:") && repeat.contains("[open]"),
        "{repeat}"
    );

    // Verify only one feedback file exists, with the evidence block appended.
    let target_journal = PathBuf::from(stdout(repo.arc(&target).args(["journal", "dir"])).trim());
    let feedback_files: Vec<String> = fs::read_dir(&target_journal)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with("-feedback.md"))
        .collect();
    assert_eq!(feedback_files, vec![first_file.clone()]);

    let body = fs::read_to_string(&first_path).unwrap();
    assert!(body.contains("First encounter: hung for 30s."), "{body}");
    assert!(body.contains("### Evidence evd-"), "{body}");
    assert!(body.contains("Reproduced on CI"), "{body}");
    assert!(
        body.contains("Second encounter: reproduced with --verbose."),
        "{body}"
    );
    assert!(!body.contains("Should not be written."), "{body}");

    // Check `--list --json` preserved `upstream` route and counted 1 evidence block.
    let listed =
        json_stdout(
            repo.arc(&scratch)
                .args(["feedback", "target-tool", "--list", "--json"]),
        );
    assert_eq!(listed["schema"], "arc-feedback-list/1");
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    assert_eq!(listed["items"][0]["key"], "slow-flag");
    assert_eq!(listed["items"][0]["file"], first_file);
    assert_eq!(listed["items"][0]["route"], "upstream");
    assert_eq!(listed["items"][0]["evidence_count"], 1);

    // Check recorded events carry identity and source provenance.
    let events = journal_events(&target_journal);
    let evd = events
        .iter()
        .find(|e| e["event"] == "evidence" && e["file"] == first_file)
        .expect("evidence event");
    assert_eq!(evd["harness"], "codex");
    assert_eq!(evd["session"], "sess-2");
    assert_eq!(evd["actor"], "agent-two");
    assert_eq!(evd["item_key"], "obs-2");

    // Once consumed, a later report with the same `--key` opens a fresh feedback artifact.
    repo.arc(&target)
        .args([
            "journal",
            "consume",
            &first_file,
            "--outcome",
            "discarded",
            "--note",
            "fixed upstream",
        ])
        .assert()
        .success();

    let reopened = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "target-tool",
                "--key",
                "slow-flag",
                "--title",
                "Regression after upgrade",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Reappeared in v2.\n"),
    );
    assert_eq!(reopened["action"], "created");
    assert_ne!(reopened["file"].as_str().unwrap(), first_file);
}

#[test]
fn feedback_routes_list_journal_open_tier_and_triage_workflow() {
    let repo = Repo::new();
    let tool = init_registered_repo(&repo, "cli-tool");
    let scratch = repo.home.join("outside");
    fs::create_dir_all(&scratch).unwrap();

    // Seed non-feedback actionable items in the target project.
    let body_file = repo.home.join("seed.md");
    fs::write(&body_file, "# Seed item\n\nBody.\n").unwrap();
    let seed = body_file.to_str().unwrap();
    for (topic, kind) in [
        ("active-task", "todo"),
        ("parked-idea", "later"),
        ("proposed-api", "feature-request"),
    ] {
        repo.arc(&tool)
            .args([
                "journal",
                "note",
                topic,
                "--kind",
                kind,
                "--body-file",
                seed,
            ])
            .assert()
            .success();
    }

    // File four feedback items covering every route value (including default `local`).
    let f_local = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "cli-tool",
                "--key",
                "local-friction",
                "--title",
                "Local friction",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Local defect.\n"),
    );
    assert_eq!(f_local["route"], "local");

    let f_upstream = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "cli-tool",
                "--key",
                "upstream-bug",
                "--title",
                "Upstream bug",
                "--route",
                "upstream",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("External dependency bug.\n"),
    );
    assert_eq!(f_upstream["route"], "upstream");

    let f_operator = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "cli-tool",
                "--key",
                "policy-question",
                "--title",
                "Policy decision needed",
                "--route",
                "operator",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Needs operator policy call.\n"),
    );
    assert_eq!(f_operator["route"], "operator");

    let f_delegate = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "cli-tool",
                "--key",
                "followup-cleanup",
                "--title",
                "Delegatable cleanup",
                "--route",
                "delegate",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Clean follow-up task.\n"),
    );
    assert_eq!(f_delegate["route"], "delegate");

    // `--list` works from outside any repo and prints keys, routes, and evidence counts.
    let list_text = stdout(repo.arc(&scratch).args(["feedback", "cli-tool", "--list"]));
    assert!(
        list_text.contains("open feedback (newest first):"),
        "{list_text}"
    );
    assert!(
        list_text.contains("local-friction") && list_text.contains("[route: local]"),
        "{list_text}"
    );
    assert!(
        list_text.contains("upstream-bug") && list_text.contains("[route: upstream]"),
        "{list_text}"
    );
    assert!(
        list_text.contains("policy-question") && list_text.contains("[route: operator]"),
        "{list_text}"
    );
    assert!(
        list_text.contains("followup-cleanup") && list_text.contains("[route: delegate]"),
        "{list_text}"
    );

    // `arc journal open` places feedback in its own section apart from open work and feature requests.
    let open_text = stdout(repo.arc(&tool).args(["journal", "open"]));
    let open_idx = open_text.find("open items (newest first):").unwrap();
    let later_idx = open_text.find("later items (newest first):").unwrap();
    let fr_idx = open_text.find("feature requests (newest first):").unwrap();
    let fb_idx = open_text.find("feedback reports (newest first):").unwrap();
    assert!(
        open_idx < later_idx && later_idx < fr_idx && fr_idx < fb_idx,
        "{open_text}"
    );
    assert!(open_text[open_idx..later_idx].contains("active-task"));
    assert!(!open_text[open_idx..later_idx].contains("local-friction"));
    assert!(open_text[fr_idx..fb_idx].contains("proposed-api"));
    assert!(!open_text[fr_idx..fb_idx].contains("local-friction"));
    assert!(
        open_text[fb_idx..].contains("local-friction")
            && open_text[fb_idx..].contains("[route: local]")
    );
    assert!(
        open_text[fb_idx..].contains("upstream-bug")
            && open_text[fb_idx..].contains("[route: upstream]")
    );
    assert!(
        open_text[fb_idx..].contains("policy-question")
            && open_text[fb_idx..].contains("[route: operator]")
    );
    assert!(
        open_text[fb_idx..].contains("followup-cleanup")
            && open_text[fb_idx..].contains("[route: delegate]")
    );

    // `arc journal open --json` exposes `feedback` as a separate tier with `route` and `feedback_key`.
    let open_json = json_stdout(repo.arc(&tool).args(["journal", "open", "--json"]));
    assert_eq!(open_json["schema"], "arc-journal-open/1");
    assert_eq!(open_json["open"].as_array().unwrap().len(), 1);
    assert_eq!(open_json["later"].as_array().unwrap().len(), 1);
    assert_eq!(open_json["feature_requests"].as_array().unwrap().len(), 1);
    let fb_items = open_json["feedback"].as_array().unwrap();
    assert_eq!(fb_items.len(), 4);
    assert!(fb_items
        .iter()
        .any(|row| row["route"] == "upstream" && row["feedback_key"] == "upstream-bug"));
    assert!(fb_items
        .iter()
        .any(|row| row["route"] == "operator" && row["feedback_key"] == "policy-question"));

    // `arc journal open --kind feedback --json` filters cleanly.
    let filtered = json_stdout(
        repo.arc(&tool)
            .args(["journal", "open", "--kind", "feedback", "--json"]),
    );
    assert!(filtered["open"].as_array().unwrap().is_empty());
    assert!(filtered["feature_requests"].as_array().unwrap().is_empty());
    assert_eq!(filtered["feedback"].as_array().unwrap().len(), 4);

    // `arc journal list --kind feedback` surfaces routes in text and JSON.
    let listed = json_stdout(
        repo.arc(&tool)
            .args(["journal", "list", "--kind", "feedback", "--json"]),
    );
    assert_eq!(listed["artifacts"].as_array().unwrap().len(), 4);
    assert!(listed["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["route"] == "delegate"));

    // Triage: transition feedback -> feature-request, feedback -> todo, and consume --outcome discarded.
    let local_file = f_local["file"].as_str().unwrap();
    let delegate_file = f_delegate["file"].as_str().unwrap();
    let operator_file = f_operator["file"].as_str().unwrap();

    repo.arc(&tool)
        .args([
            "journal",
            "transition",
            local_file,
            "--to",
            "feature-request",
        ])
        .assert()
        .success();
    repo.arc(&tool)
        .args(["journal", "transition", delegate_file, "--to", "todo"])
        .assert()
        .success();
    repo.arc(&tool)
        .args([
            "journal",
            "consume",
            operator_file,
            "--outcome",
            "discarded",
            "--note",
            "handled in policy",
        ])
        .assert()
        .success();

    let after = json_stdout(repo.arc(&tool).args(["journal", "open", "--json"]));
    assert_eq!(after["open"].as_array().unwrap().len(), 2); // active-task + transitioned followup-cleanup
    assert_eq!(after["feature_requests"].as_array().unwrap().len(), 2); // proposed-api + transitioned local-friction
    assert_eq!(after["feedback"].as_array().unwrap().len(), 1); // only upstream-bug remains open
    assert_eq!(after["feedback"][0]["feedback_key"], "upstream-bug");

    // `arc journal doctor` reports healthy journal with zero problems.
    repo.arc(&tool)
        .args(["journal", "doctor"])
        .assert()
        .success();
}

#[test]
fn feedback_anchors_relative_configured_journal_dirs_and_refuses_unanchorable_ones() {
    let repo = Repo::new();
    let rel_tool = repo.home.join("projects").join("rel-tool");
    fs::create_dir_all(&rel_tool).unwrap();
    git(&rel_tool, &["init", "-b", "master"]);
    git(&rel_tool, &["config", "user.name", "Fixture"]);
    git(
        &rel_tool,
        &["config", "user.email", "fixture@example.invalid"],
    );
    fs::write(rel_tool.join("README.md"), "# rel-tool\n").unwrap();
    git(&rel_tool, &["add", "."]);
    git(&rel_tool, &["commit", "-m", "initial"]);

    let missing_tool = repo.home.join("projects").join("missing-tool");
    let config_path = repo.home.join(".local/ai/arc/config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap();
    cfg.push_str(&format!(
        "\n[journals.dirs]\n{} = \"relative-journal\"\n{} = \"relative-journal\"\n",
        serde_json::to_string(&rel_tool.display().to_string()).unwrap(),
        serde_json::to_string(&missing_tool.display().to_string()).unwrap(),
    ));
    fs::write(&config_path, cfg).unwrap();

    // A normal journal write inside `rel-tool` anchors the relative destination to `<rel-tool>/relative-journal`.
    repo.arc(&rel_tool)
        .args(["journal", "log", "bootstrap", "init relative journal"])
        .assert()
        .success();
    let expected_journal = rel_tool.canonicalize().unwrap().join("relative-journal");
    assert!(expected_journal.is_dir());

    let scratch = repo.home.join("caller-outside-git");
    fs::create_dir_all(&scratch).unwrap();

    let created = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "rel-tool",
                "--key",
                "rel-path-bug",
                "--title",
                "Relative path check",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Must land inside rel-tool/relative-journal.\n"),
    );
    let filed_path = PathBuf::from(created["path"].as_str().unwrap());
    assert_eq!(filed_path.parent().unwrap(), expected_journal.as_path());
    assert!(filed_path.is_file());
    assert!(
        !scratch.join("relative-journal").exists(),
        "feedback must never write into caller's cwd"
    );

    let open = json_stdout(repo.arc(&rel_tool).args(["journal", "open", "--json"]));
    assert_eq!(open["feedback"].as_array().unwrap().len(), 1);
    assert_eq!(open["feedback"][0]["feedback_key"], "rel-path-bug");

    // A relative destination whose configured project root does not exist must be refused cleanly.
    for args in [
        vec!["feedback", "missing-tool", "--list"],
        vec![
            "feedback",
            "missing-tool",
            "--key",
            "nope",
            "--title",
            "Unanchored",
            "--body-file",
            "-",
        ],
    ] {
        let mut cmd = repo.arc(&scratch);
        cmd.args(&args);
        if args.contains(&"--body-file") {
            cmd.write_stdin("should fail\n");
        }
        let stderr =
            String::from_utf8_lossy(&cmd.assert().failure().get_output().stderr).to_string();
        assert!(
            stderr.contains("relative journal path"),
            "expected refusal naming relative journal path: {stderr}"
        );
    }
    assert!(!scratch.join("relative-journal").exists());
}

#[test]
fn feedback_atomic_commit_avoids_unrecorded_evidence_and_duplicates_on_retry() {
    use std::os::unix::fs::PermissionsExt;

    let repo = Repo::new();
    let target = init_registered_repo(&repo, "atomic-tool");
    let scratch = repo.home.join("outside-atomic");
    fs::create_dir_all(&scratch).unwrap();

    let created = json_stdout(
        repo.arc(&scratch)
            .args([
                "--actor",
                "agent-one",
                "--harness",
                "claude",
                "--session",
                "sess-1",
                "feedback",
                "atomic-tool",
                "--key",
                "flaky-lock",
                "--title",
                "Flaky lock",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Initial report.\n"),
    );
    let file = created["file"].as_str().unwrap().to_string();
    let file_path = PathBuf::from(created["path"].as_str().unwrap());
    let target_journal = file_path.parent().unwrap().to_path_buf();
    let events_path = target_journal.join("events.jsonl");

    // 1. Hold events.lock while spawning an append: the body must remain untouched while waiting
    // on events.lock, then commit atomically once the lock is released.
    let before_lock_body = fs::read_to_string(&file_path).unwrap();
    let lock_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(target_journal.join(".locks/events.lock"))
        .unwrap();
    lock_file.lock().unwrap();

    let mut child = repo.spawn_arc_piped(
        &scratch,
        &[
            "--actor",
            "agent-two",
            "--harness",
            "codex",
            "--session",
            "sess-2",
            "feedback",
            "atomic-tool",
            "--key",
            "flaky-lock",
            "--source",
            "harness=codex session=sess-2 ts=2026-11-26T12:00:00Z",
            "--item-key",
            "lock-obs-1",
            "--body-file",
            "-",
        ],
    );
    {
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"Observed while events lock was held.\n")
            .unwrap();
    }
    thread::sleep(Duration::from_millis(400));
    assert!(
        child.try_wait().unwrap().is_none(),
        "append must wait for events.lock before committing"
    );
    assert_eq!(
        fs::read_to_string(&file_path).unwrap(),
        before_lock_body,
        "body must not be written before events.lock is acquired"
    );
    lock_file.unlock().unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "append should succeed after unlock");
    assert_eq!(
        fs::read_to_string(&file_path)
            .unwrap()
            .matches("Observed while events lock was held.")
            .count(),
        1
    );

    // 2. Make events.jsonl read-only so both append and create fail cleanly without touching bodies.
    let before_ro_body = fs::read_to_string(&file_path).unwrap();
    fs::set_permissions(&events_path, fs::Permissions::from_mode(0o444)).unwrap();

    repo.arc(&scratch)
        .args([
            "--actor",
            "agent-three",
            "--harness",
            "codex",
            "--session",
            "sess-3",
            "feedback",
            "atomic-tool",
            "--key",
            "flaky-lock",
            "--source",
            "harness=codex session=sess-3 ts=2026-11-26T12:05:00Z",
            "--item-key",
            "ro-obs-1",
            "--body-file",
            "-",
        ])
        .write_stdin("Must not be left unrecorded on failure.\n")
        .assert()
        .failure();
    assert_eq!(
        fs::read_to_string(&file_path).unwrap(),
        before_ro_body,
        "failed append must leave the artifact body unchanged"
    );

    repo.arc(&scratch)
        .args([
            "--actor",
            "agent-three",
            "--harness",
            "codex",
            "--session",
            "sess-3",
            "feedback",
            "atomic-tool",
            "--key",
            "never-created",
            "--title",
            "Should not exist",
            "--body-file",
            "-",
        ])
        .write_stdin("Must not leave an unrecorded file.\n")
        .assert()
        .failure();
    let stray: Vec<_> = fs::read_dir(&target_journal)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains("never-created"))
        .collect();
    assert!(
        stray.is_empty(),
        "failed create left unrecorded file: {stray:?}"
    );

    // Restore permissions and retry the same source item: must land exactly once with clean doctor.
    fs::set_permissions(&events_path, fs::Permissions::from_mode(0o644)).unwrap();
    repo.arc(&scratch)
        .args([
            "--actor",
            "agent-three",
            "--harness",
            "codex",
            "--session",
            "sess-3",
            "feedback",
            "atomic-tool",
            "--key",
            "flaky-lock",
            "--source",
            "harness=codex session=sess-3 ts=2026-11-26T12:05:00Z",
            "--item-key",
            "ro-obs-1",
            "--body-file",
            "-",
        ])
        .write_stdin("Must not be left unrecorded on failure.\n")
        .assert()
        .success();

    let final_body = fs::read_to_string(&file_path).unwrap();
    assert_eq!(
        final_body
            .matches("Must not be left unrecorded on failure.")
            .count(),
        1,
        "retry must not duplicate evidence:\n{final_body}"
    );
    let listed =
        json_stdout(
            repo.arc(&scratch)
                .args(["feedback", "atomic-tool", "--list", "--json"]),
        );
    assert_eq!(listed["items"][0]["file"], file);
    assert_eq!(listed["items"][0]["evidence_count"], 2);

    repo.arc(&target)
        .args(["journal", "doctor"])
        .assert()
        .success();
}

#[test]
fn feedback_explicit_key_takes_precedence_over_newer_unkeyed_matching_topic() {
    let repo = Repo::new();
    let _target = init_registered_repo(&repo, "key-precedence");
    let scratch = repo.home.join("outside-precedence");
    fs::create_dir_all(&scratch).unwrap();

    // 1. Explicitly keyed report with `--route upstream`.
    let keyed = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "key-precedence",
                "--key",
                "finding",
                "--route",
                "upstream",
                "--title",
                "Original keyed finding",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Explicitly keyed observation.\n"),
    );
    let keyed_file = keyed["file"].as_str().unwrap().to_string();

    // Ensure the unkeyed report has a strictly newer timestamp filename.
    thread::sleep(Duration::from_millis(1_100));

    // 2. Unrelated unkeyed report whose derived topic is also `finding`, with `--route delegate`.
    let unkeyed = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "key-precedence",
                "--title",
                "Finding",
                "--route",
                "delegate",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Unrelated unkeyed report.\n"),
    );
    let unkeyed_file = unkeyed["file"].as_str().unwrap().to_string();
    assert_ne!(keyed_file, unkeyed_file);
    assert!(
        unkeyed_file > keyed_file,
        "expected newer filename: {unkeyed_file} > {keyed_file}"
    );

    // 3. Subsequent `--key finding` must append to the explicitly keyed report and preserve `upstream`.
    let appended = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "key-precedence",
                "--key",
                "finding",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Second observation for keyed finding.\n"),
    );
    assert_eq!(appended["action"], "appended");
    assert_eq!(appended["file"], keyed_file);
    assert_eq!(appended["route"], "upstream");

    let listed =
        json_stdout(
            repo.arc(&scratch)
                .args(["feedback", "key-precedence", "--list", "--json"]),
        );
    let items = listed["items"].as_array().unwrap();
    let keyed_row = items.iter().find(|r| r["file"] == keyed_file).unwrap();
    let unkeyed_row = items.iter().find(|r| r["file"] == unkeyed_file).unwrap();
    assert_eq!(keyed_row["evidence_count"], 1);
    assert_eq!(keyed_row["route"], "upstream");
    assert_eq!(unkeyed_row["evidence_count"], 0);
    assert_eq!(unkeyed_row["route"], "delegate");
}

#[test]
fn feedback_pinned_reports_leave_list_and_do_not_capture_keyed_appends() {
    let repo = Repo::new();
    let target = init_registered_repo(&repo, "pin-tool");
    let scratch = repo.home.join("outside-pin");
    fs::create_dir_all(&scratch).unwrap();

    let first = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "pin-tool",
                "--key",
                "standing-issue",
                "--title",
                "Standing reference report",
                "--route",
                "operator",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Pinned reference observation.\n"),
    );
    let first_file = first["file"].as_str().unwrap().to_string();
    let first_path = PathBuf::from(first["path"].as_str().unwrap());

    // Pin the feedback report: it must leave both `journal open` and `feedback --list`.
    repo.arc(&target)
        .args([
            "journal",
            "pin",
            &first_file,
            "--reason",
            "standing reference",
        ])
        .assert()
        .success();

    let open = json_stdout(repo.arc(&target).args(["journal", "open", "--json"]));
    assert!(open["feedback"].as_array().unwrap().is_empty(), "{open}");
    let listed = json_stdout(
        repo.arc(&scratch)
            .args(["feedback", "pin-tool", "--list", "--json"]),
    );
    assert!(listed["items"].as_array().unwrap().is_empty(), "{listed}");

    // Filing under the same `--key` must create a fresh report rather than appending to the pinned one.
    let before_pin_body = fs::read_to_string(&first_path).unwrap();
    let second = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "pin-tool",
                "--key",
                "standing-issue",
                "--title",
                "Fresh report after pin",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("New friction occurrence.\n"),
    );
    assert_eq!(second["action"], "created");
    assert_ne!(second["file"].as_str().unwrap(), first_file);
    assert_eq!(
        fs::read_to_string(&first_path).unwrap(),
        before_pin_body,
        "pinned feedback report must not receive new keyed appends"
    );

    let listed_after = json_stdout(
        repo.arc(&scratch)
            .args(["feedback", "pin-tool", "--list", "--json"]),
    );
    let items = listed_after["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["file"], second["file"]);
}

#[test]
fn feedback_overlong_key_refuses_cleanly_without_stranding_pending_transaction() {
    let repo = Repo::new();
    let target = init_registered_repo(&repo, "long-key-tool");
    let target_journal = PathBuf::from(stdout(repo.arc(&target).args(["journal", "dir"])).trim());
    let scratch = repo.home.join("outside-long-key");
    fs::create_dir_all(&scratch).unwrap();

    let overlong_key = "x".repeat(256);
    let output = repo
        .arc(&scratch)
        .args([
            "feedback",
            "long-key-tool",
            "--key",
            &overlong_key,
            "--title",
            "Overlong key",
            "--body-file",
            "-",
        ])
        .write_stdin("Should be rejected before touching exchange/pending.json.\n")
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8_lossy(&output);
    assert!(stderr.contains("too long"), "{stderr}");
    assert!(
        !target_journal.join("exchange/pending.json").exists(),
        "rejected overlong key must never strand exchange/pending.json"
    );

    // Subsequent short valid feedback write and ordinary journal write must both succeed cleanly.
    let created = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "long-key-tool",
                "--key",
                "short-valid",
                "--title",
                "Short valid key",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Valid feedback after rejected overlong key.\n"),
    );
    assert_eq!(created["action"], "created");
    assert_eq!(created["key"], "short-valid");

    repo.arc(&target)
        .args(["journal", "log", "health", "ordinary journal write works"])
        .assert()
        .success();

    repo.arc(&target)
        .args(["journal", "doctor"])
        .assert()
        .success();
}

#[test]
fn feedback_nested_configured_scope_inside_git_repo_does_not_target_unreachable_journal() {
    let repo = Repo::new();
    let tool = repo.home.join("projects").join("tool");
    let nested = tool.join("nested");
    fs::create_dir_all(&nested).unwrap();
    git(&tool, &["init", "-b", "master"]);
    git(&tool, &["config", "user.name", "Fixture"]);
    git(&tool, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(tool.join("README.md"), "# tool\n").unwrap();
    fs::write(nested.join("mod.rs"), "// nested\n").unwrap();
    git(&tool, &["add", "."]);
    git(&tool, &["commit", "-m", "initial"]);

    let tool_canon = tool.canonicalize().unwrap();
    let nested_canon = nested.canonicalize().unwrap();
    let config_path = repo.home.join(".local/ai/arc/config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap();
    cfg.push_str(&format!(
        "\n[journals.dirs]\n{} = \"main-journal\"\n{} = \"unused-journal\"\n",
        serde_json::to_string(&tool_canon.display().to_string()).unwrap(),
        serde_json::to_string(&nested_canon.display().to_string()).unwrap(),
    ));
    fs::write(&config_path, cfg).unwrap();

    // From `tool/nested`, ordinary journal resolution uses the Git root's `tool/main-journal`.
    let resolved_from_nested =
        PathBuf::from(stdout(repo.arc(&nested).args(["journal", "dir"])).trim());
    let main_journal = tool_canon.join("main-journal");
    assert_eq!(resolved_from_nested, main_journal);

    let scratch = repo.home.join("outside-nested");
    fs::create_dir_all(&scratch).unwrap();

    // `arc feedback nested` (write and --list) must refuse rather than targeting `tool/unused-journal`.
    repo.arc(&scratch)
        .args(["feedback", "nested", "--key", "scope", "--body-file", "-"])
        .write_stdin("Must not land in unused-journal.\n")
        .assert()
        .failure();
    repo.arc(&scratch)
        .args(["feedback", "nested", "--list"])
        .assert()
        .failure();
    assert!(
        !tool_canon.join("unused-journal").exists(),
        "unused-journal must never be created"
    );

    // Filing against `tool` lands in `tool/main-journal` and is visible from `tool/nested`.
    let created = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "tool",
                "--key",
                "scope",
                "--title",
                "Main scope report",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Lands in main-journal.\n"),
    );
    let filed_path = PathBuf::from(created["path"].as_str().unwrap());
    assert_eq!(filed_path.parent().unwrap(), main_journal.as_path());

    let open = json_stdout(repo.arc(&nested).args(["journal", "open", "--json"]));
    assert_eq!(open["feedback"].as_array().unwrap().len(), 1);
    assert_eq!(open["feedback"][0]["feedback_key"], "scope");

    let listed = json_stdout(
        repo.arc(&scratch)
            .args(["feedback", "tool", "--list", "--json"]),
    );
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    assert_eq!(listed["items"][0]["key"], "scope");
}

#[test]
fn feedback_never_copies_caller_repo_scaffold_into_different_target_journal() {
    let repo = Repo::new();
    let plain_target = init_registered_repo(&repo, "plain-target");

    // Set up a caller Git repo carrying custom `.arc/templates/feedback.md` and `caller-custom.md`.
    let caller_repo = repo.home.join("projects").join("caller-repo");
    fs::create_dir_all(caller_repo.join(".arc/templates")).unwrap();
    git(&caller_repo, &["init", "-b", "master"]);
    git(&caller_repo, &["config", "user.name", "Fixture"]);
    git(
        &caller_repo,
        &["config", "user.email", "fixture@example.invalid"],
    );
    fs::write(
        caller_repo.join(".arc/templates/feedback.md"),
        "CALLER-ONLY-FEEDBACK-MARKER\n\n{{body}}\n",
    )
    .unwrap();
    fs::write(
        caller_repo.join(".arc/templates/caller-custom.md"),
        "CALLER-ONLY-CUSTOM-MARKER\n\n{{body}}\n",
    )
    .unwrap();
    git(&caller_repo, &["add", "."]);
    git(&caller_repo, &["commit", "-m", "add caller templates"]);

    // Register an absent `offline-tool` scope with an absolute target journal.
    let offline_anchor = repo.home.join("projects").join("offline-tool");
    let offline_journal = repo.home.join("journals").join("offline-tool");
    fs::create_dir_all(&offline_journal).unwrap();
    let config_path = repo.home.join(".local/ai/arc/config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap();
    cfg.push_str(&format!(
        "\n[journals.dirs]\n{} = {}\n",
        serde_json::to_string(&offline_anchor.display().to_string()).unwrap(),
        serde_json::to_string(&offline_journal.display().to_string()).unwrap(),
    ));
    fs::write(&config_path, cfg).unwrap();

    // 1. Filing into `offline-tool` from `caller-repo` must use the built-in scaffold, never caller's.
    let offline_created = json_stdout(
        repo.arc(&caller_repo)
            .args([
                "feedback",
                "offline-tool",
                "--key",
                "offline-bug",
                "--title",
                "Offline bug",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Explicit offline report body.\n"),
    );
    let offline_body = fs::read_to_string(offline_created["path"].as_str().unwrap()).unwrap();
    assert!(
        !offline_body.contains("CALLER-ONLY-FEEDBACK-MARKER"),
        "caller template leaked into offline target:\n{offline_body}"
    );
    assert!(offline_body.contains("## Shape"), "{offline_body}");
    assert!(
        offline_body.contains("Explicit offline report body."),
        "{offline_body}"
    );

    // 2. Requesting a caller-only custom scaffold for `offline-tool` must refuse.
    repo.arc(&caller_repo)
        .args([
            "feedback",
            "offline-tool",
            "--key",
            "offline-custom",
            "--scaffold",
            "caller-custom",
            "--body-file",
            "-",
        ])
        .write_stdin("Should refuse caller-only scaffold.\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown scaffold"));

    // 3. Filing into a reachable target without its own template also must not inherit caller's template.
    let plain_created = json_stdout(
        repo.arc(&caller_repo)
            .args([
                "feedback",
                "plain-target",
                "--key",
                "plain-bug",
                "--title",
                "Plain bug",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Explicit plain target body.\n"),
    );
    let plain_body = fs::read_to_string(plain_created["path"].as_str().unwrap()).unwrap();
    assert!(
        !plain_body.contains("CALLER-ONLY-FEEDBACK-MARKER"),
        "caller template leaked into reachable target:\n{plain_body}"
    );
    assert!(plain_body.contains("## Shape"), "{plain_body}");
    assert!(
        plain_body.contains("Explicit plain target body."),
        "{plain_body}"
    );

    repo.arc(&plain_target)
        .args(["journal", "doctor"])
        .assert()
        .success();
}
