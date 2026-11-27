use super::common::*;

fn journal_slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

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

#[test]
fn feedback_discovers_populated_configured_scope_for_independent_nested_git_repo() {
    let repo = Repo::new();
    let outer = repo.home.join("projects").join("outer");
    let scope = outer.join("scope");
    let inner = scope.join("inner");
    fs::create_dir_all(&inner).unwrap();

    // Initialize `outer` as a Git repo and `outer/scope/inner` as an independent Git repo.
    git(&outer, &["init", "-b", "master"]);
    git(&outer, &["config", "user.name", "Fixture"]);
    git(&outer, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(outer.join("README.md"), "# outer\n").unwrap();
    git(&outer, &["add", "README.md"]);
    git(&outer, &["commit", "-m", "init outer"]);

    git(&inner, &["init", "-b", "master"]);
    git(&inner, &["config", "user.name", "Fixture"]);
    git(&inner, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(inner.join("README.md"), "# inner\n").unwrap();
    git(&inner, &["add", "README.md"]);
    git(&inner, &["commit", "-m", "init inner"]);

    let scope_canon = scope.canonicalize().unwrap();
    let abs_journal = repo.home.join("journals").join("nested-scope-journal");
    let config_path = repo.home.join(".local/ai/arc/config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap();
    cfg.push_str(&format!(
        "\n[journals.dirs]\n{} = {}\n",
        serde_json::to_string(&scope_canon.display().to_string()).unwrap(),
        serde_json::to_string(&abs_journal.display().to_string()).unwrap(),
    ));
    fs::write(&config_path, cfg).unwrap();

    // From `inner`, `arc journal dir` resolves to `abs_journal` and `journal log` populates it.
    let inner_dir = PathBuf::from(stdout(repo.arc(&inner).args(["journal", "dir"])).trim());
    assert_eq!(inner_dir, abs_journal);
    repo.arc(&inner)
        .args([
            "journal",
            "log",
            "bootstrap",
            "populate nested scope journal",
        ])
        .assert()
        .success();

    // From outside Git, `arc feedback scope --list` discovers the populated journal
    // and `arc feedback scope` files into it so `inner`'s `arc journal open` sees it.
    let scratch = repo.home.join("outside-nested-repo");
    fs::create_dir_all(&scratch).unwrap();

    let empty_list = json_stdout(
        repo.arc(&scratch)
            .args(["feedback", "scope", "--list", "--json"]),
    );
    assert_eq!(empty_list["schema"], "arc-feedback-list/1");
    assert_eq!(
        PathBuf::from(empty_list["journal_dir"].as_str().unwrap()),
        abs_journal
    );
    assert!(empty_list["items"].as_array().unwrap().is_empty());

    let created = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "scope",
                "--key",
                "nested-repo-bug",
                "--title",
                "Nested repo friction",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Observed in inner repo.\n"),
    );
    let filed_path = PathBuf::from(created["path"].as_str().unwrap());
    assert_eq!(filed_path.parent().unwrap(), abs_journal.as_path());

    let listed = json_stdout(
        repo.arc(&scratch)
            .args(["feedback", "scope", "--list", "--json"]),
    );
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    assert_eq!(listed["items"][0]["key"], "nested-repo-bug");

    let open = json_stdout(repo.arc(&inner).args(["journal", "open", "--json"]));
    assert_eq!(open["feedback"].as_array().unwrap().len(), 1);
    assert_eq!(open["feedback"][0]["feedback_key"], "nested-repo-bug");
}

#[test]
fn feedback_refuses_ambiguous_duplicated_rootless_binding_like_journal_dir() {
    let repo = Repo::new();
    let rootless = repo.home.join("rootless-project");
    fs::create_dir_all(&rootless).unwrap();

    // Bind a rootless directory via `arc journal bind`.
    let bound_journal = PathBuf::from(stdout(repo.arc(&rootless).args(["journal", "bind"])).trim());
    assert!(bound_journal.join("bindings.jsonl").is_file());

    // Copy the bound journal to a second directory under the default journal root.
    let second_slug = format!(
        "{}-dup",
        bound_journal.file_name().unwrap().to_string_lossy()
    );
    let second_journal = bound_journal.parent().unwrap().join(&second_slug);
    fs::create_dir_all(&second_journal).unwrap();
    fs::copy(
        bound_journal.join("bindings.jsonl"),
        second_journal.join("bindings.jsonl"),
    )
    .unwrap();

    // From the rootless project, `arc journal dir` refuses due to multiple journals recording this anchor.
    repo.arc(&rootless)
        .args(["journal", "dir"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "multiple journals record this anchor",
        ));

    // From outside the project, `arc feedback <second-slug>` (both write and `--list`) must refuse identically.
    let scratch = repo.home.join("outside-ambiguous");
    fs::create_dir_all(&scratch).unwrap();

    repo.arc(&scratch)
        .args([
            "feedback",
            &second_slug,
            "--key",
            "ambiguous-target",
            "--title",
            "Should refuse",
            "--body-file",
            "-",
        ])
        .write_stdin("Must not write to duplicated binding.\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "multiple journals record this anchor",
        ));

    repo.arc(&scratch)
        .args(["feedback", &second_slug, "--list"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "multiple journals record this anchor",
        ));

    assert!(
        !second_journal.join("events.jsonl").exists(),
        "refused write must not touch the duplicated journal"
    );
}

#[test]
fn feedback_relative_prefix_discovers_nested_git_repo_under_enclosing_repo() {
    let repo = Repo::new();
    let outer = repo.home.join("outer-rel");
    fs::create_dir_all(&outer).unwrap();
    git(&outer, &["init", "-b", "master"]);
    git(&outer, &["config", "user.name", "Fixture"]);
    git(&outer, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(outer.join("README.md"), "# outer-rel\n").unwrap();
    git(&outer, &["add", "."]);
    git(&outer, &["commit", "-m", "initial"]);

    let scope = outer.join("scope");
    let inner = scope.join("inner");
    fs::create_dir_all(&inner).unwrap();
    git(&inner, &["init", "-b", "master"]);
    git(&inner, &["config", "user.name", "Fixture"]);
    git(&inner, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(inner.join("README.md"), "# inner\n").unwrap();
    git(&inner, &["add", "."]);
    git(&inner, &["commit", "-m", "initial"]);

    let outer_canon = fs::canonicalize(&outer).unwrap();
    let scope_canon = fs::canonicalize(&scope).unwrap();
    let inner_canon = fs::canonicalize(&inner).unwrap();
    let expected_inner_journal = inner_canon.join("relative-journal");

    let config_path = repo.home.join(".local/ai/arc/config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap();
    cfg.push_str(&format!(
        "\n[journals.dirs]\n{} = \"relative-journal\"\n",
        serde_json::to_string(&scope_canon.display().to_string()).unwrap(),
    ));
    fs::write(&config_path, cfg).unwrap();

    assert_eq!(
        PathBuf::from(stdout(repo.arc(&inner).args(["journal", "dir"])).trim()),
        expected_inner_journal
    );
    repo.arc(&inner)
        .args([
            "journal",
            "log",
            "nested-rel-bootstrap",
            "populated from inner",
        ])
        .assert()
        .success();
    assert!(expected_inner_journal.join("events.jsonl").is_file());

    let scratch = repo.home.join("outside-nested-rel");
    fs::create_dir_all(&scratch).unwrap();

    // Selecting the nested checkout itself finds and writes to `inner/relative-journal`.
    let empty_list = json_stdout(
        repo.arc(&scratch)
            .args(["feedback", "inner", "--list", "--json"]),
    );
    assert_eq!(empty_list["schema"], "arc-feedback-list/1");
    assert_eq!(
        PathBuf::from(empty_list["journal_dir"].as_str().unwrap()),
        expected_inner_journal
    );
    assert!(empty_list["items"].as_array().unwrap().is_empty());

    let created = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "inner",
                "--key",
                "nested-rel-bug",
                "--title",
                "Nested relative friction",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Observed in inner with relative prefix.\n"),
    );
    let filed_path = PathBuf::from(created["path"].as_str().unwrap());
    assert_eq!(
        filed_path.parent().unwrap(),
        expected_inner_journal.as_path()
    );

    let open = json_stdout(repo.arc(&inner).args(["journal", "open", "--json"]));
    assert_eq!(open["feedback"].as_array().unwrap().len(), 1);
    assert_eq!(open["feedback"][0]["feedback_key"], "nested-rel-bug");

    // Prefix name (`scope`) and relative folder name (`relative-journal`) must refuse
    // and never fabricate `outer/relative-journal` or `outer/scope/relative-journal`.
    for target in ["scope", "relative-journal"] {
        repo.arc(&scratch)
            .args(["feedback", target, "--list"])
            .assert()
            .failure();
        repo.arc(&scratch)
            .args([
                "feedback",
                target,
                "--key",
                "should-not-write",
                "--title",
                "Should refuse",
                "--body-file",
                "-",
            ])
            .write_stdin("Must refuse prefix target.\n")
            .assert()
            .failure();
    }
    assert!(!outer_canon.join("relative-journal").exists());
    assert!(!scope_canon.join("relative-journal").exists());
}

#[test]
fn feedback_non_git_parent_prefix_distinguishes_relative_and_absolute_nested_repo_journals() {
    let repo = Repo::new();

    // 1. Non-Git parent prefix with a relative journal destination.
    let parent_rel = repo.home.join("parent-rel");
    let inner_rel = parent_rel.join("inner-rel");
    fs::create_dir_all(&inner_rel).unwrap();
    git(&inner_rel, &["init", "-b", "master"]);
    git(&inner_rel, &["config", "user.name", "Fixture"]);
    git(
        &inner_rel,
        &["config", "user.email", "fixture@example.invalid"],
    );
    fs::write(inner_rel.join("README.md"), "# inner-rel\n").unwrap();
    git(&inner_rel, &["add", "."]);
    git(&inner_rel, &["commit", "-m", "initial"]);

    // 2. Non-Git parent prefix with an absolute shared journal destination.
    let parent_abs = repo.home.join("parent-abs");
    let inner_abs = parent_abs.join("inner-abs");
    fs::create_dir_all(&inner_abs).unwrap();
    git(&inner_abs, &["init", "-b", "master"]);
    git(&inner_abs, &["config", "user.name", "Fixture"]);
    git(
        &inner_abs,
        &["config", "user.email", "fixture@example.invalid"],
    );
    fs::write(inner_abs.join("README.md"), "# inner-abs\n").unwrap();
    git(&inner_abs, &["add", "."]);
    git(&inner_abs, &["commit", "-m", "initial"]);

    let parent_rel_canon = fs::canonicalize(&parent_rel).unwrap();
    let inner_rel_canon = fs::canonicalize(&inner_rel).unwrap();
    let expected_rel_journal = inner_rel_canon.join("relative-journal");

    let parent_abs_canon = fs::canonicalize(&parent_abs).unwrap();
    let shared_abs_journal = repo.home.join("shared-parent-abs-journal");

    let config_path = repo.home.join(".local/ai/arc/config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap();
    cfg.push_str(&format!(
        "\n[journals.dirs]\n{} = \"relative-journal\"\n{} = {}\n",
        serde_json::to_string(&parent_rel_canon.display().to_string()).unwrap(),
        serde_json::to_string(&parent_abs_canon.display().to_string()).unwrap(),
        serde_json::to_string(&shared_abs_journal.display().to_string()).unwrap(),
    ));
    fs::write(&config_path, cfg).unwrap();

    let scratch = repo.home.join("outside-parent-prefixes");
    fs::create_dir_all(&scratch).unwrap();

    // Even before `inner-rel` logs, the non-Git parent prefix must not accept writes
    // into `parent-rel/relative-journal` under either `parent-rel` or `relative-journal`.
    for target in ["parent-rel", "relative-journal"] {
        repo.arc(&scratch)
            .args(["feedback", target, "--list"])
            .assert()
            .failure();
        repo.arc(&scratch)
            .args([
                "feedback",
                target,
                "--key",
                "wrong-parent-write",
                "--title",
                "Should refuse",
                "--body-file",
                "-",
            ])
            .write_stdin("Must not write to parent-rel/relative-journal.\n")
            .assert()
            .failure();
    }
    assert!(!parent_rel_canon.join("relative-journal").exists());

    // Populate `inner-rel`'s repo-relative journal and `inner-abs`'s shared absolute journal.
    assert_eq!(
        PathBuf::from(stdout(repo.arc(&inner_rel).args(["journal", "dir"])).trim()),
        expected_rel_journal
    );
    repo.arc(&inner_rel)
        .args(["journal", "log", "inner-rel-init", "init relative"])
        .assert()
        .success();

    assert_eq!(
        PathBuf::from(stdout(repo.arc(&inner_abs).args(["journal", "dir"])).trim()),
        shared_abs_journal
    );
    repo.arc(&inner_abs)
        .args(["journal", "log", "inner-abs-init", "init absolute"])
        .assert()
        .success();

    // Relative prefix: `inner-rel` is discovered and receives feedback visible in `inner-rel`;
    // `parent-rel` and `relative-journal` still refuse without creating `parent-rel/relative-journal`.
    let rel_created = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "inner-rel",
                "--key",
                "parent-rel-bug",
                "--title",
                "Relative child report",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Lands in inner-rel/relative-journal.\n"),
    );
    assert_eq!(
        PathBuf::from(rel_created["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        expected_rel_journal.as_path()
    );
    let rel_open = json_stdout(repo.arc(&inner_rel).args(["journal", "open", "--json"]));
    assert_eq!(rel_open["feedback"].as_array().unwrap().len(), 1);
    assert_eq!(rel_open["feedback"][0]["feedback_key"], "parent-rel-bug");

    for target in ["parent-rel", "relative-journal"] {
        repo.arc(&scratch)
            .args(["feedback", target, "--list"])
            .assert()
            .failure();
    }
    assert!(!parent_rel_canon.join("relative-journal").exists());

    // Absolute prefix: `parent-abs` maps the prefix to one shared journal visible in `inner-abs`.
    let abs_created = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "parent-abs",
                "--key",
                "parent-abs-bug",
                "--title",
                "Absolute shared report",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Lands in shared-parent-abs-journal.\n"),
    );
    assert_eq!(
        PathBuf::from(abs_created["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        shared_abs_journal.as_path()
    );
    let abs_open = json_stdout(repo.arc(&inner_abs).args(["journal", "open", "--json"]));
    assert_eq!(abs_open["feedback"].as_array().unwrap().len(), 1);
    assert_eq!(abs_open["feedback"][0]["feedback_key"], "parent-abs-bug");
}

#[test]
fn feedback_accepts_pre_upgrade_relative_journal_with_prefix_binding() {
    let repo = Repo::new();
    let outer = repo.home.join("outer-legacy");
    fs::create_dir_all(&outer).unwrap();
    git(&outer, &["init", "-b", "master"]);
    git(&outer, &["config", "user.name", "Fixture"]);
    git(&outer, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(outer.join("README.md"), "# outer-legacy\n").unwrap();
    git(&outer, &["add", "."]);
    git(&outer, &["commit", "-m", "initial"]);

    let scope = outer.join("scope");
    let inner = scope.join("inner");
    fs::create_dir_all(&inner).unwrap();
    git(&inner, &["init", "-b", "master"]);
    git(&inner, &["config", "user.name", "Fixture"]);
    git(&inner, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(inner.join("README.md"), "# inner\n").unwrap();
    git(&inner, &["add", "."]);
    git(&inner, &["commit", "-m", "initial"]);

    // Also create a second nested repo whose journal carries a binding pointing
    // to a completely unrelated directory, verifying binding safeguards stay intact.
    let foreign_other = repo.home.join("elsewhere-foreign");
    fs::create_dir_all(&foreign_other).unwrap();
    let foreign_other_canon = fs::canonicalize(&foreign_other).unwrap();
    let foreign_inner = scope.join("foreign-inner");
    fs::create_dir_all(&foreign_inner).unwrap();
    git(&foreign_inner, &["init", "-b", "master"]);
    git(&foreign_inner, &["config", "user.name", "Fixture"]);
    git(
        &foreign_inner,
        &["config", "user.email", "fixture@example.invalid"],
    );
    fs::write(foreign_inner.join("README.md"), "# foreign-inner\n").unwrap();
    git(&foreign_inner, &["add", "."]);
    git(&foreign_inner, &["commit", "-m", "initial"]);

    let scope_canon = fs::canonicalize(&scope).unwrap();
    let inner_canon = fs::canonicalize(&inner).unwrap();
    let inner_journal = inner_canon.join("relative-journal");
    let foreign_inner_canon = fs::canonicalize(&foreign_inner).unwrap();
    let foreign_inner_journal = foreign_inner_canon.join("relative-journal");

    let config_path = repo.home.join(".local/ai/arc/config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap();
    cfg.push_str(&format!(
        "\n[journals.dirs]\n{} = \"relative-journal\"\n",
        serde_json::to_string(&scope_canon.display().to_string()).unwrap(),
    ));
    fs::write(&config_path, cfg).unwrap();

    // Seed pre-upgrade journal data at `inner/relative-journal`: events.jsonl plus
    // a bindings.jsonl recording the configured prefix (`outer/scope`) rather than `inner`,
    // and no `relative-checkouts.jsonl` index entry.
    fs::create_dir_all(&inner_journal).unwrap();
    fs::write(
        inner_journal.join("bindings.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "schema": "journal-binding/1",
                "ts": "2026-01-01T00:00:00Z",
                "event": "bound",
                "anchor": scope_canon.display().to_string(),
            })
        ),
    )
    .unwrap();
    fs::write(
        inner_journal.join("events.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "schema": "journal-event/1",
                "ts": "2026-01-01T00:00:01Z",
                "event": "logged",
                "topic": "legacy-entry",
                "summary": "pre-upgrade log",
            })
        ),
    )
    .unwrap();

    // Seed a foreign-bound journal at `foreign-inner/relative-journal` whose
    // binding names an unrelated directory (`elsewhere-foreign`).
    fs::create_dir_all(&foreign_inner_journal).unwrap();
    fs::write(
        foreign_inner_journal.join("bindings.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "schema": "journal-binding/1",
                "ts": "2026-01-01T00:00:00Z",
                "event": "bound",
                "anchor": foreign_other_canon.display().to_string(),
            })
        ),
    )
    .unwrap();

    assert_eq!(
        PathBuf::from(stdout(repo.arc(&inner).args(["journal", "dir"])).trim()),
        inner_journal
    );

    let scratch = repo.home.join("outside-legacy");
    fs::create_dir_all(&scratch).unwrap();

    // Pre-upgrade `inner/relative-journal` with prefix binding `outer/scope` is
    // selectable by `inner` for both listing and writing.
    let listed = json_stdout(
        repo.arc(&scratch)
            .args(["feedback", "inner", "--list", "--json"]),
    );
    assert_eq!(listed["schema"], "arc-feedback-list/1");
    assert_eq!(
        PathBuf::from(listed["journal_dir"].as_str().unwrap()),
        inner_journal
    );

    let created = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "inner",
                "--key",
                "pre-upgrade-relative",
                "--title",
                "Pre-upgrade relative report",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Filed against pre-upgrade prefix-bound relative journal.\n"),
    );
    assert_eq!(
        PathBuf::from(created["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        inner_journal.as_path()
    );

    let open = json_stdout(repo.arc(&inner).args(["journal", "open", "--json"]));
    assert_eq!(open["feedback"].as_array().unwrap().len(), 1);
    assert_eq!(open["feedback"][0]["feedback_key"], "pre-upgrade-relative");

    // Prefix name `scope` must still refuse, and the foreign-bound journal must
    // keep its recorded foreign binding rather than silently re-attributing to `foreign-inner`.
    repo.arc(&scratch)
        .args(["feedback", "scope", "--list"])
        .assert()
        .failure();
    repo.arc(&scratch)
        .args(["feedback", "foreign-inner", "--list"])
        .assert()
        .failure();
}

#[test]
fn feedback_preserves_registered_relative_checkout_past_scan_limit_and_surfaces_truncation() {
    let repo = Repo::new();
    let outer = repo.home.join("outer-broad");
    fs::create_dir_all(&outer).unwrap();
    git(&outer, &["init", "-b", "master"]);
    git(&outer, &["config", "user.name", "Fixture"]);
    git(&outer, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(outer.join("README.md"), "# outer-broad\n").unwrap();
    git(&outer, &["add", "."]);
    git(&outer, &["commit", "-m", "initial"]);

    let scope = outer.join("scope");
    let inner = scope.join("zz-parent").join("inner");
    fs::create_dir_all(&inner).unwrap();
    git(&inner, &["init", "-b", "master"]);
    git(&inner, &["config", "user.name", "Fixture"]);
    git(&inner, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(inner.join("README.md"), "# inner\n").unwrap();
    git(&inner, &["add", "."]);
    git(&inner, &["commit", "-m", "initial"]);

    let scope_canon = fs::canonicalize(&scope).unwrap();
    let inner_canon = fs::canonicalize(&inner).unwrap();
    let expected_journal = inner_canon.join("relative-journal");

    let config_path = repo.home.join(".local/ai/arc/config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap();
    cfg.push_str(&format!(
        "\n[journals.dirs]\n{} = \"relative-journal\"\n",
        serde_json::to_string(&scope_canon.display().to_string()).unwrap(),
    ));
    fs::write(&config_path, cfg).unwrap();

    // Populate `inner`'s journal and confirm `feedback inner --list` works.
    repo.arc(&inner)
        .args([
            "journal",
            "log",
            "broad-bootstrap",
            "populated before siblings",
        ])
        .assert()
        .success();

    let scratch = repo.home.join("outside-broad");
    fs::create_dir_all(&scratch).unwrap();

    let before = json_stdout(
        repo.arc(&scratch)
            .args(["feedback", "inner", "--list", "--json"]),
    );
    assert_eq!(
        PathBuf::from(before["journal_dir"].as_str().unwrap()),
        expected_journal
    );

    let inner_slug = before["slug"].as_str().unwrap().to_string();

    // Add 4,096 empty sibling directories sorting ahead of `zz-parent`.
    for i in 0..4096 {
        fs::create_dir(scope.join(format!("aaa-{i:04}"))).unwrap();
    }

    assert_eq!(
        PathBuf::from(stdout(repo.arc(&inner).args(["journal", "dir"])).trim()),
        expected_journal
    );

    // Under an incomplete scan, the short label `inner` cannot be proven unique
    // and must refuse while pointing to the unambiguous slug.
    let short_refusal = repo
        .arc(&scratch)
        .args(["feedback", "inner", "--list"])
        .assert()
        .failure();
    let short_stderr = String::from_utf8_lossy(&short_refusal.get_output().stderr);
    assert!(
        short_stderr.contains("cannot be proven unique")
            && short_stderr.contains(&inner_slug)
            && short_stderr.contains("4096 directory scan limit"),
        "expected refusal requiring unambiguous slug under truncated scan, got: {short_stderr}"
    );

    // Addressing the registered checkout by its unambiguous slug succeeds for
    // both listing and writing, while incomplete discovery is surfaced on stderr.
    let list_out = repo
        .arc(&scratch)
        .args(["feedback", &inner_slug, "--list", "--json"])
        .output()
        .unwrap();
    assert!(
        list_out.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&list_out.stderr)
    );
    let list_json: serde_json::Value = serde_json::from_slice(&list_out.stdout).unwrap();
    assert_eq!(
        PathBuf::from(list_json["journal_dir"].as_str().unwrap()),
        expected_journal
    );
    let list_stderr = String::from_utf8_lossy(&list_out.stderr);
    assert!(
        list_stderr.contains("4096 directory scan limit")
            && list_stderr.contains(&scope_canon.display().to_string()),
        "expected incomplete discovery warning on stderr, got: {list_stderr}"
    );

    let write_out = repo
        .arc(&scratch)
        .args([
            "feedback",
            &inner_slug,
            "--key",
            "broad-prefix-bug",
            "--title",
            "Filed past scan limit",
            "--body-file",
            "-",
            "--json",
        ])
        .write_stdin("Still lands in zz-parent/inner/relative-journal.\n")
        .output()
        .unwrap();
    assert!(
        write_out.status.success(),
        "write failed: {}",
        String::from_utf8_lossy(&write_out.stderr)
    );
    let write_json: serde_json::Value = serde_json::from_slice(&write_out.stdout).unwrap();
    assert_eq!(
        PathBuf::from(write_json["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        expected_journal.as_path()
    );

    let open = json_stdout(repo.arc(&inner).args(["journal", "open", "--json"]));
    assert_eq!(open["feedback"].as_array().unwrap().len(), 1);
    assert_eq!(open["feedback"][0]["feedback_key"], "broad-prefix-bug");

    // When the checkout index is absent (simulating an unindexed pre-upgrade
    // checkout behind 4,096 siblings), short-name lookup refuses with explicit
    // truncation details, while addressing the checkout by its full slug still works.
    let index_path = repo.home.join(".local/ai/arc/relative-checkouts.jsonl");
    assert!(index_path.is_file());
    fs::remove_file(&index_path).unwrap();

    let missing_out = repo
        .arc(&scratch)
        .args(["feedback", "inner", "--list"])
        .assert()
        .failure();
    let missing_stderr = String::from_utf8_lossy(&missing_out.get_output().stderr);
    assert!(
        missing_stderr.contains("incomplete relative [journals.dirs] discovery")
            && missing_stderr.contains("4096 directory scan limit")
            && missing_stderr.contains(&scope_canon.display().to_string()),
        "expected explicit truncation notice on refusal, got: {missing_stderr}"
    );

    let unindexed_slug_list =
        json_stdout(
            repo.arc(&scratch)
                .args(["feedback", &inner_slug, "--list", "--json"]),
        );
    assert_eq!(
        PathBuf::from(unindexed_slug_list["journal_dir"].as_str().unwrap()),
        expected_journal
    );
}

#[test]
fn feedback_refuses_short_name_under_truncated_scan_and_allows_unambiguous_slugs() {
    let repo = Repo::new();
    let scope = repo.home.join("broad-ambiguous-scope");
    let aaa_inner = scope.join("aaa").join("inner");
    let zzz_inner = scope.join("zzz").join("inner");
    for checkout in [&aaa_inner, &zzz_inner] {
        fs::create_dir_all(checkout).unwrap();
        git(checkout, &["init", "-b", "master"]);
        git(checkout, &["config", "user.name", "Fixture"]);
        git(
            checkout,
            &["config", "user.email", "fixture@example.invalid"],
        );
        fs::write(checkout.join("README.md"), "# inner\n").unwrap();
        git(checkout, &["add", "."]);
        git(checkout, &["commit", "-m", "initial"]);
    }

    let scope_canon = fs::canonicalize(&scope).unwrap();
    let aaa_canon = fs::canonicalize(&aaa_inner).unwrap();
    let zzz_canon = fs::canonicalize(&zzz_inner).unwrap();
    let aaa_journal = aaa_canon.join("relative-journal");
    let zzz_journal = zzz_canon.join("relative-journal");
    let aaa_slug = journal_slug(&aaa_canon);
    let zzz_slug = journal_slug(&zzz_canon);

    let config_path = repo.home.join(".local/ai/arc/config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap();
    cfg.push_str(&format!(
        "\n[journals.dirs]\n{} = \"relative-journal\"\n",
        serde_json::to_string(&scope_canon.display().to_string()).unwrap(),
    ));
    fs::write(&config_path, cfg).unwrap();

    // Seed both checkouts with legacy prefix-bound journals (no checkout index).
    for journal in [&aaa_journal, &zzz_journal] {
        fs::create_dir_all(journal).unwrap();
        fs::write(
            journal.join("bindings.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({
                    "schema": "journal-binding/1",
                    "ts": "2026-01-01T00:00:00Z",
                    "event": "bound",
                    "anchor": scope_canon.display().to_string(),
                })
            ),
        )
        .unwrap();
        fs::write(
            journal.join("events.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({
                    "schema": "journal-event/1",
                    "ts": "2026-01-01T00:00:01Z",
                    "event": "logged",
                    "topic": "legacy-seed",
                    "summary": "pre-upgrade",
                })
            ),
        )
        .unwrap();
    }

    let scratch = repo.home.join("outside-ambiguous-truncated");
    fs::create_dir_all(&scratch).unwrap();

    // Before adding 4,096 siblings, `feedback inner` refuses as ambiguous.
    repo.arc(&scratch)
        .args(["feedback", "inner", "--list"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "matched multiple registered projects",
        ));

    // Add 4,096 shallow sibling directories and index only `aaa/inner`.
    for i in 0..4096 {
        fs::create_dir(scope.join(format!("aaa-{i:04}"))).unwrap();
    }
    repo.arc(&aaa_inner)
        .args(["journal", "log", "index-aaa", "index only aaa/inner"])
        .assert()
        .success();

    // Short name `inner` MUST still refuse rather than silently writing to `aaa/inner`.
    repo.arc(&scratch)
        .args([
            "feedback",
            "inner",
            "--key",
            "must-not-write",
            "--title",
            "Ambiguous short name",
            "--body-file",
            "-",
        ])
        .write_stdin("Must not land under incomplete discovery.\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be proven unique"))
        .stderr(predicates::str::contains(&aaa_slug));
    let aaa_open_before = json_stdout(repo.arc(&aaa_inner).args(["journal", "open", "--json"]));
    assert!(aaa_open_before["feedback"].as_array().unwrap().is_empty());

    // Exact slugs unambiguously target both the indexed `aaa/inner` and the
    // unindexed legacy `zzz/inner`.
    let aaa_created = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                &aaa_slug,
                "--key",
                "aaa-report",
                "--title",
                "AAA slug write",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Targeted aaa/inner via slug.\n"),
    );
    assert_eq!(
        PathBuf::from(aaa_created["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        aaa_journal.as_path()
    );

    let zzz_created = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                &zzz_slug,
                "--key",
                "zzz-report",
                "--title",
                "ZZZ legacy slug write",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Targeted unindexed legacy zzz/inner via slug.\n"),
    );
    assert_eq!(
        PathBuf::from(zzz_created["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        zzz_journal.as_path()
    );
    let zzz_open = json_stdout(repo.arc(&zzz_inner).args(["journal", "open", "--json"]));
    assert_eq!(zzz_open["feedback"].as_array().unwrap().len(), 1);
    assert_eq!(zzz_open["feedback"][0]["feedback_key"], "zzz-report");

    // Indexing `zzz/inner` restores the explicit multi-match ambiguity refusal on `inner`.
    repo.arc(&zzz_inner)
        .args(["journal", "log", "index-zzz", "index zzz/inner"])
        .assert()
        .success();
    repo.arc(&scratch)
        .args(["feedback", "inner", "--list"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "matched multiple registered projects",
        ));
}

#[test]
fn feedback_overlapping_relative_prefixes_match_legacy_binding_of_winning_scope() {
    let repo = Repo::new();
    let outer = repo.home.join("outer-overlap");
    fs::create_dir_all(&outer).unwrap();
    git(&outer, &["init", "-b", "master"]);
    git(&outer, &["config", "user.name", "Fixture"]);
    git(&outer, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(outer.join("README.md"), "# outer-overlap\n").unwrap();
    git(&outer, &["add", "."]);
    git(&outer, &["commit", "-m", "initial"]);

    let scope = outer.join("scope");
    let deeper = scope.join("zzz");
    let inner = deeper.join("inner");
    fs::create_dir_all(&inner).unwrap();
    git(&inner, &["init", "-b", "master"]);
    git(&inner, &["config", "user.name", "Fixture"]);
    git(&inner, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(inner.join("README.md"), "# inner\n").unwrap();
    git(&inner, &["add", "."]);
    git(&inner, &["commit", "-m", "initial"]);

    let scope_canon = fs::canonicalize(&scope).unwrap();
    let deeper_canon = fs::canonicalize(&deeper).unwrap();
    let inner_canon = fs::canonicalize(&inner).unwrap();
    let inner_journal = inner_canon.join("relative-journal");

    let config_path = repo.home.join(".local/ai/arc/config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap();
    cfg.push_str(&format!(
        "\n[journals.dirs]\n{} = \"relative-journal\"\n{} = \"relative-journal\"\n",
        serde_json::to_string(&scope_canon.display().to_string()).unwrap(),
        serde_json::to_string(&deeper_canon.display().to_string()).unwrap(),
    ));
    fs::write(&config_path, cfg).unwrap();

    // Seed a legacy journal bound to the most specific prefix (`outer/scope/zzz`).
    fs::create_dir_all(&inner_journal).unwrap();
    fs::write(
        inner_journal.join("bindings.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "schema": "journal-binding/1",
                "ts": "2026-01-01T00:00:00Z",
                "event": "bound",
                "anchor": deeper_canon.display().to_string(),
            })
        ),
    )
    .unwrap();
    fs::write(
        inner_journal.join("events.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "schema": "journal-event/1",
                "ts": "2026-01-01T00:00:01Z",
                "event": "logged",
                "topic": "overlap-legacy",
                "summary": "bound to deeper winning prefix",
            })
        ),
    )
    .unwrap();

    assert_eq!(
        PathBuf::from(stdout(repo.arc(&inner).args(["journal", "dir"])).trim()),
        inner_journal
    );

    let scratch = repo.home.join("outside-overlap");
    fs::create_dir_all(&scratch).unwrap();

    let listed = json_stdout(
        repo.arc(&scratch)
            .args(["feedback", "inner", "--list", "--json"]),
    );
    assert_eq!(
        PathBuf::from(listed["journal_dir"].as_str().unwrap()),
        inner_journal
    );

    let created = json_stdout(
        repo.arc(&scratch)
            .args([
                "feedback",
                "inner",
                "--key",
                "overlap-prefix-bug",
                "--title",
                "Overlapping relative prefixes",
                "--body-file",
                "-",
                "--json",
            ])
            .write_stdin("Matched winning deeper prefix legacy binding.\n"),
    );
    assert_eq!(
        PathBuf::from(created["path"].as_str().unwrap())
            .parent()
            .unwrap(),
        inner_journal.as_path()
    );

    let open = json_stdout(repo.arc(&inner).args(["journal", "open", "--json"]));
    assert_eq!(open["feedback"].as_array().unwrap().len(), 1);
    assert_eq!(open["feedback"][0]["feedback_key"], "overlap-prefix-bug");
}

#[test]
fn feedback_repairs_unterminated_checkout_index_and_surfaces_index_write_failures() {
    let repo = Repo::new();
    let outer = repo.home.join("outer-torn-index");
    fs::create_dir_all(&outer).unwrap();
    git(&outer, &["init", "-b", "master"]);
    git(&outer, &["config", "user.name", "Fixture"]);
    git(&outer, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(outer.join("README.md"), "# outer-torn-index\n").unwrap();
    git(&outer, &["add", "."]);
    git(&outer, &["commit", "-m", "initial"]);

    let scope = outer.join("scope");
    let inner = scope.join("zz-parent").join("inner");
    fs::create_dir_all(&inner).unwrap();
    git(&inner, &["init", "-b", "master"]);
    git(&inner, &["config", "user.name", "Fixture"]);
    git(&inner, &["config", "user.email", "fixture@example.invalid"]);
    fs::write(inner.join("README.md"), "# inner\n").unwrap();
    git(&inner, &["add", "."]);
    git(&inner, &["commit", "-m", "initial"]);

    let scope_canon = fs::canonicalize(&scope).unwrap();
    let inner_canon = fs::canonicalize(&inner).unwrap();
    let expected_journal = inner_canon.join("relative-journal");
    let inner_slug = journal_slug(&inner_canon);

    let config_path = repo.home.join(".local/ai/arc/config.toml");
    let mut cfg = fs::read_to_string(&config_path).unwrap();
    cfg.push_str(&format!(
        "\n[journals.dirs]\n{} = \"relative-journal\"\n",
        serde_json::to_string(&scope_canon.display().to_string()).unwrap(),
    ));
    fs::write(&config_path, cfg).unwrap();

    // Add 4,096 shallow sibling directories so discovery depends on the index.
    for i in 0..4096 {
        fs::create_dir(scope.join(format!("aaa-{i:04}"))).unwrap();
    }

    // Seed `relative-checkouts.jsonl` with an unterminated malformed tail line.
    let index_path = repo.home.join(".local/ai/arc/relative-checkouts.jsonl");
    fs::write(&index_path, "{\"broken\": true").unwrap();

    // `journal log` in `inner` must repair/publish cleanly so `inner` is indexed.
    repo.arc(&inner)
        .args([
            "journal",
            "log",
            "torn-index-seed",
            "register after torn tail",
        ])
        .assert()
        .success();

    let repaired = fs::read_to_string(&index_path).unwrap();
    assert!(
        repaired.ends_with('\n') && repaired.contains(&inner_canon.display().to_string()),
        "expected cleanly repaired index containing inner, got: {repaired:?}"
    );

    let scratch = repo.home.join("outside-torn-index");
    fs::create_dir_all(&scratch).unwrap();

    let listed =
        json_stdout(
            repo.arc(&scratch)
                .args(["feedback", &inner_slug, "--list", "--json"]),
        );
    assert_eq!(
        PathBuf::from(listed["journal_dir"].as_str().unwrap()),
        expected_journal
    );

    // Replace the index file with a directory to force an index publication failure
    // and verify `journal log` surfaces the failure explicitly on stderr.
    fs::remove_file(&index_path).unwrap();
    fs::create_dir(&index_path).unwrap();

    let log_out = repo
        .arc(&inner)
        .args(["journal", "log", "index-fail", "surface index error"])
        .output()
        .unwrap();
    assert!(log_out.status.success());
    let log_stderr = String::from_utf8_lossy(&log_out.stderr);
    assert!(
        log_stderr.contains("could not record relative checkout index"),
        "expected index write failure warning on stderr, got: {log_stderr}"
    );
}
