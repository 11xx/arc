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
