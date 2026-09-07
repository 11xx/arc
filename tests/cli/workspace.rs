use crate::common::*;

fn assert_backlog_summary_matches_rows(value: &serde_json::Value) {
    let projects = value["projects"].as_array().unwrap();
    let count = |field: &str| {
        projects
            .iter()
            .map(|project| project[field].as_array().map_or(0, Vec::len))
            .sum::<usize>()
    };
    let sum = |field: &str| {
        projects
            .iter()
            .map(|project| project[field].as_u64().unwrap())
            .sum::<u64>()
    };
    assert_eq!(value["summary"]["projects"], projects.len());
    assert_eq!(value["summary"]["needs_review"], count("needs_review"));
    assert_eq!(value["summary"]["no_patchset"], count("no_patchset"));
    assert_eq!(value["summary"]["debt_owed"], count("debt_owed"));
    assert_eq!(value["summary"]["open_items"], sum("open_items"));
    assert_eq!(value["summary"]["later_items"], sum("later_items"));
    assert_eq!(
        value["summary"]["feature_requests"],
        sum("feature_requests")
    );
    assert_eq!(
        value["summary"]["unreachable"],
        value["unreachable"].as_array().unwrap().len()
    );
}

#[test]
fn workspace_list_aggregates_repos_and_tags_rows_with_slugs() {
    // Two independent repos whose ledgers share one data_root.
    let data_root = TempDir::new().unwrap();
    let alpha = Repo::new();
    let beta = Repo::new();
    for (repo, slug) in [(&alpha, "feat-alpha"), (&beta, "feat-beta")] {
        repo.arc(&repo.root)
            .env("ARC_DATA_ROOT", data_root.path())
            .args(["begin", slug, "--no-worktree"])
            .assert()
            .success();
    }

    let mut report = alpha.arc(&alpha.root);
    report
        .env("ARC_DATA_ROOT", data_root.path())
        .args(["workspace", "list", "--json"]);
    let value = json_stdout(&mut report);
    assert_eq!(value["schema"], "arc-workspace/1");
    let slugs: Vec<String> = value["repos"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|repo| repo["changes"].as_array().unwrap())
        .map(|row| row["slug"].as_str().unwrap().to_string())
        .collect();
    assert!(slugs.contains(&"feat-alpha".to_string()), "{slugs:?}");
    assert!(slugs.contains(&"feat-beta".to_string()), "{slugs:?}");
    // Every repo bucket is keyed by its own slug directory.
    assert_eq!(value["repos"].as_array().unwrap().len(), 2);
}

/// Without a data_root the ledgers sit inside each repository's Git common
/// dir, where nothing can enumerate them. The journal registry is what knows
/// they exist, so discovery falls back to it rather than refusing.
#[test]
fn workspace_list_falls_back_to_the_project_registry() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-registry", "--no-worktree"])
        .assert()
        .success();
    // A journal write is what registers the project.
    repo.arc(&repo.root)
        .args(["journal", "log", "registered", "the project exists"])
        .assert()
        .success();

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "list", "--json"]);
    let value = json_stdout(&mut report);
    let slugs: Vec<String> = value["repos"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|repo| repo["changes"].as_array().unwrap())
        .map(|row| row["slug"].as_str().unwrap().to_string())
        .collect();
    assert!(slugs.contains(&"feat-registry".to_string()), "{slugs:?}");
}

/// Opening a change is the moment a directory provably becomes an arc project,
/// so it registers itself. Otherwise a repository with open changes but no
/// journal writes would be invisible to every cross-project view — structure,
/// not habit, has to guarantee it.
#[test]
fn begin_registers_the_project_for_cross_project_views() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-unwritten", "--no-worktree"])
        .assert()
        .success();

    // No journal artifact was ever written, only a change opened.
    let journal = journal_dir_of(&repo);
    assert!(journal.join("bindings.jsonl").is_file(), "{journal:?}");
    assert!(
        !journal.join("events.jsonl").exists(),
        "registering must not fabricate journal history"
    );

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "list", "--json"]);
    let value = json_stdout(&mut report);
    let slugs: Vec<String> = value["repos"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|repo| repo["changes"].as_array().unwrap())
        .map(|row| row["slug"].as_str().unwrap().to_string())
        .collect();
    assert!(slugs.contains(&"feat-unwritten".to_string()), "{slugs:?}");
}

fn journal_dir_of(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim())
}

/// The backlog joins both halves — what the ledger says is waiting on a
/// verdict, and what the journal says is waiting on a session.
#[test]
fn workspace_backlog_reports_ledger_and_journal_together() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-pending", "--no-worktree"])
        .assert()
        .success();
    let src = repo.home.join("body.md");
    fs::write(&src, "work waiting for a session\n").unwrap();
    repo.arc(&repo.root)
        .args([
            "journal",
            "note",
            "waiting",
            "--kind",
            "todo",
            "--body-file",
            src.to_str().unwrap(),
        ])
        .assert()
        .success();

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut report);
    assert_eq!(value["schema"], "arc-workspace-backlog/12");
    assert_eq!(value["scope"]["mode"], "global");
    assert_backlog_summary_matches_rows(&value);
    let project = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .unwrap_or_else(|| panic!("project missing: {value}"));
    assert_eq!(project["open_items"], 1);
    // A change carrying no patchset is waiting on work, not on a reviewer,
    // so it is reported apart from the review queue.
    assert!(
        project["no_patchset"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id.as_str().unwrap().starts_with("feat-pending")),
        "{project}"
    );
    assert_eq!(
        project["needs_review"].as_array().unwrap().len(),
        0,
        "{project}"
    );
}

#[test]
fn workspace_backlog_and_show_project_recorded_event_identity() {
    let repo = Repo::new();
    let opened = stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "opening-lead")
            .env("ARC_HARNESS", "claude")
            .env("ARC_SESSION", "opening-session")
            .env("ARC_MODEL", "opening-model#high")
            .args(["begin", "identity"]),
    );
    let change_id = opened
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap();
    let worktree = repo.home.join(".worktrees/repo-identity");
    repo.commit(&worktree, "identity.txt", "identity\n", "feat: identity");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "patch-author")
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", "patch-session")
        .env("ARC_MODEL", "patch-model#medium")
        .env("ARC_ON_BEHALF_OF", "executor-subject")
        .args(["snapshot", change_id])
        .assert()
        .success();

    let show = json_stdout(repo.arc(&worktree).args(["show", change_id, "--json"]));
    assert_eq!(show["opened_by"], "opening-lead");
    assert_eq!(show["opened_harness"], "claude");
    assert_eq!(show["opened_session"], "opening-session");
    assert_eq!(show["opened_model"], "opening-model#high");
    assert_eq!(show["patchsets"][0]["actor"], "patch-author");
    assert_eq!(show["patchsets"][0]["on_behalf_of"], "executor-subject");
    assert_eq!(show["patchsets"][0]["harness"], "codex");
    assert_eq!(show["patchsets"][0]["session"], "patch-session");
    assert_eq!(show["patchsets"][0]["model"], "patch-model#medium");

    let mut pending = repo.arc(&worktree);
    pending.args(["workspace", "backlog", "--json"]);
    let pending = json_stdout(&mut pending);
    let review = pending["projects"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|project| project["needs_review"].as_array().unwrap())
        .find(|entry| entry["change_id"] == change_id)
        .unwrap_or_else(|| panic!("review row missing: {pending}"));
    assert_eq!(review["recorded_by"], "patch-author");
    assert_eq!(review["on_behalf_of"], "executor-subject");
    assert_eq!(review["recorded_harness"], "codex");
    assert_eq!(review["recorded_session"], "patch-session");
    assert_eq!(review["recorded_model"], "patch-model#medium");

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "debt-lead")
        .env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", "debt-session")
        .env("ARC_MODEL", "debt-model#high")
        .args(["integrate", change_id, "--debt", "review later"])
        .assert()
        .success();

    let show = json_stdout(repo.arc(&repo.root).args(["show", change_id, "--json"]));
    assert_eq!(show["debt"]["actor"], "debt-lead");
    assert_eq!(show["debt"]["harness"], "claude");
    assert_eq!(show["debt"]["session"], "debt-session");
    assert_eq!(show["debt"]["model"], "debt-model#high");

    let mut backlog = repo.arc(&repo.root);
    backlog.args(["workspace", "backlog", "--json"]);
    let backlog = json_stdout(&mut backlog);
    let debt = backlog["projects"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|project| project["debt_owed"].as_array().unwrap())
        .find(|entry| entry["change_id"] == change_id)
        .unwrap_or_else(|| panic!("debt row missing: {backlog}"));
    assert_eq!(debt["declared_by"], "debt-lead");
    assert_eq!(debt["declared_harness"], "claude");
    assert_eq!(debt["declared_session"], "debt-session");
    assert_eq!(debt["declared_model"], "debt-model#high");
}

#[test]
fn show_keeps_undeclared_session_and_model_absent() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .env_remove("ARC_SESSION")
        .env_remove("ARC_MODEL")
        .args(["begin", "absent-identity", "--no-worktree"])
        .assert()
        .success();

    let show = json_stdout(
        repo.arc(&repo.root)
            .args(["show", "absent-identity", "--json"]),
    );
    assert_eq!(show["opened_harness"], "test");
    assert!(show["opened_on_behalf_of"].is_null(), "{show}");
    assert!(show["opened_session"].is_null(), "{show}");
    assert!(show["opened_model"].is_null(), "{show}");
}

/// A change carrying a revision is the only kind a verdict can answer, and the
/// entry says how long it has been waiting without opening the change.
#[test]
fn workspace_backlog_separates_a_reviewable_change_from_an_empty_one() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-ready", "--no-worktree"])
        .assert()
        .success();
    fs::write(repo.root.join("shipped.txt"), "work\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "feat: work"]);
    repo.arc(&repo.root).args(["snapshot"]).assert().success();
    repo.arc(&repo.root)
        .args(["begin", "feat-empty", "--no-worktree", "--target", "master"])
        .assert()
        .success();

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut report);
    let project = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .unwrap_or_else(|| panic!("project missing: {value}"));

    let reviewable = project["needs_review"].as_array().unwrap();
    let entry = reviewable
        .iter()
        .find(|entry| {
            entry["change_id"]
                .as_str()
                .unwrap()
                .starts_with("feat-ready")
        })
        .unwrap_or_else(|| panic!("reviewable change missing: {project}"));
    assert_eq!(entry["patchsets"], 1, "{entry}");
    assert!(entry["waiting_days"].is_number(), "{entry}");
    // Never reviewed is not the same as reviewed and superseded.
    assert!(entry.get("superseded_verdict").is_none(), "{entry}");
    assert!(
        !reviewable.iter().any(|entry| entry["change_id"]
            .as_str()
            .unwrap()
            .starts_with("feat-empty")),
        "an empty change must not sit in the review queue: {project}"
    );
    assert!(
        project["no_patchset"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id.as_str().unwrap().starts_with("feat-empty")),
        "{project}"
    );
}

/// A journal whose project has vanished holds work no per-project command can
/// reach: standing in the project is how every other view starts. The sweep is
/// the only thing that can see it, so it names it instead of skipping it.
#[test]
fn workspace_backlog_names_an_unreachable_project() {
    let repo = Repo::new();
    let src = repo.home.join("body.md");
    fs::write(&src, "stranded\n").unwrap();
    repo.arc(&repo.root)
        .args([
            "journal",
            "note",
            "stranded",
            "--kind",
            "todo",
            "--body-file",
            src.to_str().unwrap(),
        ])
        .assert()
        .success();

    // A second journal for a project that is not there any more.
    let journals = repo.home.join(".local/ai/journals");
    let orphan = journals.join("-gone-away-project");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(orphan.join("20260101T000000Z-left-todo.md"), "# Left\n").unwrap();
    fs::write(
        orphan.join("bindings.jsonl"),
        "{\"schema\":\"journal-binding/1\",\"ts\":\"2026-01-01T00:00:00Z\",\
         \"event\":\"bound\",\"anchor\":\"/gone/away/project\"}\n",
    )
    .unwrap();

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut report);
    assert_backlog_summary_matches_rows(&value);
    let stranded = value["unreachable"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["slug"] == "-gone-away-project")
        .unwrap_or_else(|| panic!("orphan not reported: {value}"));
    assert_eq!(stranded["anchor"], "/gone/away/project");
    assert_eq!(stranded["reason"], "anchor does not exist");
}

#[test]
fn workspace_backlog_compacts_temporary_unreachable_journals() {
    let repo = Repo::new();
    let journals = repo.home.join(".local/ai/journals");
    for index in 0..5 {
        // A temporary anchor is one under the environment's temp directory,
        // which is what the classification reads; the fixture names one
        // there instead of assuming /tmp so the case holds under any TMPDIR.
        let anchor = std::env::temp_dir()
            .join(format!("arc-scratch-{index}"))
            .display()
            .to_string();
        let journal = journals.join(format!("-tmp-noise-{index}"));
        fs::create_dir_all(&journal).unwrap();
        fs::write(
            journal.join("20260101T000000Z-waiting-todo.md"),
            "# Waiting\n",
        )
        .unwrap();
        fs::write(
            journal.join("bindings.jsonl"),
            format!(
                "{{\"schema\":\"journal-binding/1\",\"ts\":\"2026-01-01T00:00:00Z\",\"event\":\"bound\",\"anchor\":\"{anchor}\"}}\n"
            ),
        )
        .unwrap();
    }
    let durable = journals.join("-durable-project");
    fs::create_dir_all(&durable).unwrap();
    fs::write(
        durable.join("20260101T000000Z-waiting-todo.md"),
        "# Waiting\n",
    )
    .unwrap();
    fs::write(
        durable.join("bindings.jsonl"),
        "{\"schema\":\"journal-binding/1\",\"ts\":\"2026-01-01T00:00:00Z\",\"event\":\"bound\",\"anchor\":\"/srv/durable-project\"}\n",
    )
    .unwrap();

    let text = stdout(repo.arc(&repo.root).args(["workspace", "backlog"]));
    assert!(
        text.contains("maintenance: 6 unreachable journals (5 temporary/scratch, 1 other)"),
        "{text}"
    );
    assert!(text.contains("-durable-project"), "{text}");
    assert!(!text.contains("-tmp-noise-0"), "{text}");
    assert!(
        text.contains("5 temporary/scratch journals hidden; rerun with --unreachable to expand"),
        "{text}"
    );

    let expanded = stdout(
        repo.arc(&repo.root)
            .args(["workspace", "backlog", "--unreachable"]),
    );
    assert!(expanded.contains("-tmp-noise-0"), "{expanded}");
    assert!(expanded.contains("-durable-project"), "{expanded}");

    let mut json = repo.arc(&repo.root);
    json.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut json);
    assert_backlog_summary_matches_rows(&value);
    assert_eq!(value["summary"]["unreachable"], 6);
}

#[test]
fn workspace_backlog_scopes_reachable_and_missing_anchors_by_path() {
    let repo = Repo::new();
    let workspace = repo.home.join("projects");
    let alpha = workspace.join("one/repo");
    let beta = workspace.join("two/repo");
    let elsewhere = repo.home.join("elsewhere/repo");
    let missing_inside = workspace.join("gone/repo");
    let missing_outside = repo.home.join("gone-elsewhere/repo");

    for root in [&alpha, &beta, &elsewhere, &missing_inside, &missing_outside] {
        fs::create_dir_all(root).unwrap();
        git(root, &["init", "-b", "master"]);
        git(root, &["config", "user.name", "Tester"]);
        git(root, &["config", "user.email", "tester@example.invalid"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        fs::write(root.join("README.md"), "registered\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "init"]);
        let body = repo.home.join(format!(
            "{}.md",
            root.parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
        ));
        fs::write(&body, "work\n").unwrap();
        repo.arc(root)
            .args([
                "journal",
                "note",
                "waiting",
                "--kind",
                "todo",
                "--body-file",
                body.to_str().unwrap(),
            ])
            .assert()
            .success();
    }

    fs::remove_dir_all(&missing_inside).unwrap();
    fs::remove_dir_all(&missing_outside).unwrap();

    let mut scoped = repo.arc(&workspace);
    scoped.args(["workspace", "backlog", "--here", "--json"]);
    let value = json_stdout(&mut scoped);
    assert_eq!(value["schema"], "arc-workspace-backlog/12");
    assert_eq!(value["scope"]["mode"], "under");
    assert_eq!(
        value["scope"]["under"],
        workspace.canonicalize().unwrap().display().to_string()
    );
    let anchors: Vec<_> = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["anchor"].as_str().unwrap())
        .collect();
    assert_eq!(anchors.len(), 2, "{value}");
    assert!(anchors.iter().any(|anchor| anchor.ends_with("one/repo")));
    assert!(anchors.iter().any(|anchor| anchor.ends_with("two/repo")));
    assert!(
        !anchors
            .iter()
            .any(|anchor| anchor.ends_with("elsewhere/repo")),
        "{value}"
    );
    let unreachable = value["unreachable"].as_array().unwrap();
    assert_eq!(unreachable.len(), 1, "{value}");
    assert_eq!(
        unreachable[0]["anchor"],
        missing_inside.display().to_string()
    );

    let mut global = repo.arc(&workspace);
    global.args(["workspace", "backlog", "--global", "--json"]);
    let global = json_stdout(&mut global);
    assert_eq!(global["scope"]["mode"], "global");
    assert_eq!(global["projects"].as_array().unwrap().len(), 3, "{global}");
    assert_eq!(global["unreachable"].as_array().unwrap().len(), 2);

    let empty = repo.home.join("empty-workspace");
    fs::create_dir(&empty).unwrap();
    repo.arc(&empty)
        .args(["workspace", "backlog", "--here"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "scope: under {}",
            empty.display()
        )))
        .stdout(predicates::str::contains(
            "nothing outstanding in this workspace scope",
        ));
}

/// Printing nothing is the same shape as a command that died with its output
/// swallowed, and this rollup used to refuse loudly when it could not run. An
/// empty answer has to read as an answer, and `--json` keeps its shape.
#[test]
fn workspace_rollups_answer_when_nothing_is_registered() {
    let repo = Repo::new();
    for view in ["list", "inbox"] {
        repo.arc(&repo.root)
            .args(["workspace", view])
            .assert()
            .success()
            .stdout(predicates::str::contains("no projects found"))
            .stdout(predicates::str::contains("journals"));
    }
    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "list", "--json"]);
    let value = json_stdout(&mut report);
    assert_eq!(value["schema"], "arc-workspace/1");
    assert!(value["repos"].as_array().unwrap().is_empty(), "{value}");
}

/// An empty rollup is not proof of an empty registry: a project with no ledger
/// is registered and still contributes no store. Saying "nothing is registered"
/// there replaces silence with something worse — a confident false statement.
#[test]
fn an_empty_rollup_does_not_claim_an_empty_registry() {
    let repo = Repo::new();
    let orphan = repo.home.join(".local/ai/journals").join("-some-project");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(orphan.join("20260101T000000Z-a-todo.md"), "# Item\n").unwrap();
    fs::write(
        orphan.join("bindings.jsonl"),
        "{\"schema\":\"journal-binding/1\",\"ts\":\"2026-01-01T00:00:00Z\",\
         \"event\":\"bound\",\"anchor\":\"/gone/away\"}\n",
    )
    .unwrap();

    repo.arc(&repo.root)
        .args(["workspace", "list"])
        .assert()
        .success()
        .stdout(predicates::str::contains("1 project(s) registered"))
        .stdout(predicates::str::contains("nothing is registered").not());
}

/// `list` and `inbox` report changes, so an unreachable project has nothing to
/// contribute to them — but disappearing from a rollup is how work goes unseen,
/// which is the failure this whole feature exists to prevent. So they say what
/// they skipped and where to look, even though only `backlog` can report it.
#[test]
fn workspace_list_says_what_it_skipped() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-visible", "--no-worktree"])
        .assert()
        .success();

    let journals = repo.home.join(".local/ai/journals");
    let orphan = journals.join("-gone-elsewhere");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(orphan.join("20260101T000000Z-left-todo.md"), "# Left\n").unwrap();
    fs::write(
        orphan.join("bindings.jsonl"),
        "{\"schema\":\"journal-binding/1\",\"ts\":\"2026-01-01T00:00:00Z\",\
         \"event\":\"bound\",\"anchor\":\"/gone/elsewhere\"}\n",
    )
    .unwrap();

    repo.arc(&repo.root)
        .args(["workspace", "list"])
        .assert()
        .success()
        .stderr(predicates::str::contains("-gone-elsewhere"))
        .stderr(predicates::str::contains("/gone/elsewhere"))
        .stderr(predicates::str::contains("journal rebind"));
}

/// Opening a change registers a project with a binding and nothing else, so a
/// repository moved before anything is written to its journal leaves a
/// directory with no artifacts that still names a project holding open work.
/// A dead anchor is what makes an orphan; artifacts only add to it.
#[test]
fn workspace_backlog_names_a_binding_only_orphan() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-registered", "--no-worktree"])
        .assert()
        .success();

    // A second project, registered the same way and then gone.
    let journals = repo.home.join(".local/ai/journals");
    let orphan = journals.join("-vanished-project");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(
        orphan.join("bindings.jsonl"),
        "{\"schema\":\"journal-binding/1\",\"ts\":\"2026-01-01T00:00:00Z\",\
         \"event\":\"bound\",\"anchor\":\"/vanished/project\"}\n",
    )
    .unwrap();
    assert!(
        fs::read_dir(&orphan)
            .unwrap()
            .all(|entry| entry.unwrap().file_name() == "bindings.jsonl"),
        "fixture must hold no artifacts"
    );

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut report);
    let named = value["unreachable"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["slug"] == "-vanished-project")
        .unwrap_or_else(|| panic!("binding-only orphan not reported: {value}"));
    assert_eq!(named["anchor"], "/vanished/project");
}

/// Under --since the journal counts mean arrivals, not outstanding work, or a
/// delta would read as a full report and be believed as one.
#[test]
fn workspace_backlog_since_counts_arrivals_only() {
    let repo = Repo::new();
    let src = repo.home.join("body.md");
    fs::write(&src, "older\n").unwrap();
    repo.arc(&repo.root)
        .args([
            "journal",
            "note",
            "older",
            "--kind",
            "todo",
            "--body-file",
            src.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Everything filed so far predates a cutoff in the far future.
    let mut report = repo.arc(&repo.root);
    report.args([
        "workspace",
        "backlog",
        "--json",
        "--since",
        "20990101T000000Z",
    ]);
    let value = json_stdout(&mut report);
    let mine = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"));
    assert!(mine.is_none_or(|entry| entry["open_items"] == 0), "{value}");

    // And a cutoff in the past counts it.
    let mut report = repo.arc(&repo.root);
    report.args([
        "workspace",
        "backlog",
        "--json",
        "--since",
        "20000101T000000Z",
    ]);
    let value = json_stdout(&mut report);
    let mine = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .unwrap_or_else(|| panic!("project missing: {value}"));
    assert_eq!(mine["open_items"], 1);
}

#[test]
fn workspace_backlog_items() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "registered", "the project exists"])
        .assert()
        .success();
    let journal = journal_dir_of(&repo);
    for (file, body) in [
        ("20260101T000000Z-old-open-todo.md", "# Old open\n\nbody\n"),
        (
            "20260101T120000Z-old-later-later.md",
            "# Old later\n\nbody\n",
        ),
        (
            "20260103T000000Z-new-open-handoff.md",
            "# New open\n\nbody\n",
        ),
        (
            "20260103T120000Z-new-feature-feature-request.md",
            "# New feature\n\nbody\n",
        ),
    ] {
        fs::write(journal.join(file), body).unwrap();
    }

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "backlog", "--items", "--json"]);
    let value = json_stdout(&mut report);
    assert_eq!(value["schema"], "arc-workspace-backlog/12");
    let project = value["projects"].as_array().unwrap().first().unwrap();
    let items = &project["items"];
    let assert_tier = |actual: &serde_json::Value, expected: &[(&str, &str)]| {
        let entries = actual.as_array().unwrap();
        assert_eq!(entries.len(), expected.len());
        for (entry, (file, kind)) in entries.iter().zip(expected) {
            assert_eq!(entry["file"], *file);
            assert_eq!(entry["kind"], *kind);
        }
    };
    assert_tier(
        &items["open"],
        &[
            ("20260103T000000Z-new-open-handoff.md", "handoff"),
            ("20260101T000000Z-old-open-todo.md", "todo"),
        ],
    );
    assert_tier(
        &items["later"],
        &[("20260101T120000Z-old-later-later.md", "later")],
    );
    assert_tier(
        &items["feature_requests"],
        &[(
            "20260103T120000Z-new-feature-feature-request.md",
            "feature-request",
        )],
    );
    for (count, tier) in [
        ("open_items", "open"),
        ("later_items", "later"),
        ("feature_requests", "feature_requests"),
    ] {
        assert_eq!(
            project[count].as_u64().unwrap() as usize,
            items[tier].as_array().unwrap().len(),
        );
    }

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut report);
    let project = value["projects"].as_array().unwrap().first().unwrap();
    assert!(!project.as_object().unwrap().contains_key("items"));

    let mut report = repo.arc(&repo.root);
    report.args([
        "workspace",
        "backlog",
        "--items",
        "--json",
        "--since",
        "20260103T000000Z",
    ]);
    let value = json_stdout(&mut report);
    let project = value["projects"].as_array().unwrap().first().unwrap();
    let items = &project["items"];
    assert_tier(
        &items["open"],
        &[("20260103T000000Z-new-open-handoff.md", "handoff")],
    );
    assert_tier(&items["later"], &[]);
    assert_tier(
        &items["feature_requests"],
        &[(
            "20260103T120000Z-new-feature-feature-request.md",
            "feature-request",
        )],
    );
    for (count, tier) in [
        ("open_items", "open"),
        ("later_items", "later"),
        ("feature_requests", "feature_requests"),
    ] {
        assert_eq!(
            project[count].as_u64().unwrap() as usize,
            items[tier].as_array().unwrap().len(),
        );
    }
}

#[test]
fn workspace_backlog_items_surface_the_same_verification_annotation() {
    let repo = Repo::new();
    let seed = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "workspace-check",
                "--kind",
                "todo",
                "--body-file",
                "-",
            ])
            .write_stdin("# Workspace check\n"),
    );
    let file = PathBuf::from(seed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let revision = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args(["journal", "verified", &file])
        .assert()
        .success();

    let journal_text = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    let expected = format!("[verified at {}", &revision[..8]);
    assert!(journal_text.contains(&expected), "{journal_text}");

    // The two queues share one renderer, so a row reads the same whichever
    // command printed it.
    let workspace_text = stdout(
        repo.arc(&repo.root)
            .args(["workspace", "backlog", "--items"]),
    );
    assert!(workspace_text.contains(&expected), "{workspace_text}");

    let value =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "backlog", "--items", "--json"]),
        );
    let item = &value["projects"][0]["items"]["open"][0];
    assert_eq!(item["file"], file);
    assert_eq!(item["verification"]["revision"], revision);
    assert_eq!(item["verification"]["moved"], false);
}

#[test]
fn brief_scaffold_sol_low_records_the_fences() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-x", "--no-worktree"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["brief", "feat-x", "--scaffold", "sol-low"])
        .assert()
        .success();

    let brief = stdout(repo.arc(&repo.root).args(["brief", "feat-x"]));
    assert!(brief.contains("Scope ceiling"), "{brief}");
    assert!(brief.contains("danger-full-access"), "{brief}");
    assert!(brief.contains("staged, no SHA"), "{brief}");
    assert!(brief.contains("heartbeat"), "{brief}");
    assert!(brief.contains("Acceptance probes"), "{brief}");
    // The rule the scaffold exists to carry: naming a command is not a probe
    // contract, and a probe that passes at both ends proves nothing.
    assert!(brief.contains("--probes-json"), "{brief}");
    assert!(brief.contains("--probe-phase baseline"), "{brief}");
    assert!(brief.contains("**fails at that brief"), "{brief}");
    assert!(brief.contains("the expected reason"), "{brief}");
    // Attested evidence cannot support the confirmation the line above asks
    // for, and the scaffold must not ask for both at once.
    assert!(brief.contains("An attested baseline"), "{brief}");
    assert!(brief.contains("no probe contract was recorded"), "{brief}");
    assert!(
        brief.contains("exit` inside one exits only that subshell"),
        "{brief}"
    );
    // The remedy has to be sound shell, and the attestation a command an
    // executor can actually run.
    assert!(brief.contains("out=$(cmd) || exit 1"), "{brief}");
    assert!(brief.contains("--result fail --tested-revision"), "{brief}");
    assert!(brief.contains("--execution-host"), "{brief}");
    assert!(brief.contains("HEAD *is* the brief"), "{brief}");
    // A baseline measured at a base the work is no longer built on can pass
    // for something the target brought rather than for the change.
    assert!(brief.contains("ask for a new"), "{brief}");
    // The claim that probes never gate was true before probes were declarable
    // and is false now; a scaffold that still said it would teach the wrong
    // contract to every delegated executor.
    assert!(!brief.contains("never gates integration"), "{brief}");
    assert!(brief.contains("never edit a probe to make it"), "{brief}");
}

#[test]
fn restack_advise_prints_rebase_for_dependent_and_writes_nothing() {
    let repo = Repo::new();
    let base = begin_change(&repo, "base-change", None);
    let dependent = begin_change(&repo, "dependent", Some("base-change"));

    // Integrate the blocker (asserted: arc did not perform this merge, so the
    // assertion has to name the patchset it claims reached the target).
    stdout(repo.arc(&repo.root).args(["snapshot", "base-change"]));
    repo.arc(&repo.root)
        .args(["close", "base-change", "--assert-integrated", "HEAD"])
        .assert()
        .success();

    let before = event_count(&repo, &dependent);
    let out = stdout(
        repo.arc(&repo.root)
            .args(["restack", "base-change", "--advise"]),
    );
    assert!(out.contains("rebase --onto"), "{out}");
    assert!(out.contains(&dependent), "{out}");
    assert_eq!(
        event_count(&repo, &dependent),
        before,
        "restack must not write events"
    );
    let _ = base;
}

/// Commit distance is integration staleness. Unrelated target work does not
/// become conflict risk merely because there is more of it.
#[test]
fn workspace_backlog_names_how_far_a_change_is_behind_its_target() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-stale", "--no-worktree"])
        .assert()
        .success();
    fs::write(repo.root.join("work.txt"), "work\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "feat: work"]);
    repo.arc(&repo.root).args(["snapshot"]).assert().success();

    // The target takes a commit the change was never based on.
    let branch = git_out(&repo.root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    git(&repo.root, &["checkout", "master"]);
    fs::write(repo.root.join("sibling.txt"), "sibling\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "feat: sibling"]);
    git(&repo.root, &["checkout", branch.trim()]);

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut report);
    let project = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .unwrap_or_else(|| panic!("project missing: {value}"));
    let entry = project["needs_review"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["change_id"]
                .as_str()
                .unwrap()
                .starts_with("feat-stale")
        })
        .unwrap_or_else(|| panic!("reviewable change missing: {project}"));
    assert_eq!(entry["behind_target"], 1, "{entry}");
    assert_eq!(
        entry["target_path_overlap"],
        serde_json::json!([]),
        "{entry}"
    );
}

/// Target movement through a change's own paths is direct file overlap. A
/// semantic conflict can cross paths and is established only by evaluation.
#[test]
fn workspace_backlog_names_target_paths_that_overlap_the_change() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-overlap", "--no-worktree"])
        .assert()
        .success();
    fs::write(repo.root.join("shared.txt"), "change\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "feat: change shared path"]);
    repo.arc(&repo.root).args(["snapshot"]).assert().success();

    let branch = git_out(&repo.root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    git(&repo.root, &["checkout", "master"]);
    fs::write(repo.root.join("shared.txt"), "target\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "feat: target shared path"]);
    git(&repo.root, &["checkout", branch.trim()]);

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut report);
    let project = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .unwrap_or_else(|| panic!("project missing: {value}"));
    let entry = project["needs_review"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["change_id"]
                .as_str()
                .unwrap()
                .starts_with("feat-overlap")
        })
        .unwrap_or_else(|| panic!("reviewable change missing: {project}"));
    assert_eq!(entry["behind_target"], 1, "{entry}");
    assert_eq!(
        entry["target_path_overlap"],
        serde_json::json!(["shared.txt"]),
        "{entry}"
    );
}

/// A failed Git probe is a fact the caller needs. Zero distance and an empty
/// surface set are known answers and cannot stand in for it.
#[test]
fn workspace_backlog_preserves_unknown_git_probes() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-unknown", "--no-worktree"])
        .assert()
        .success();
    fs::write(repo.root.join("work.txt"), "work\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "feat: work"]);
    repo.arc(&repo.root).args(["snapshot"]).assert().success();
    git(&repo.root, &["branch", "-D", "master"]);

    let mut json = repo.arc(&repo.root);
    json.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut json);
    let project = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .unwrap_or_else(|| panic!("project missing: {value}"));
    let entry = project["needs_review"].as_array().unwrap().first().unwrap();
    assert!(entry.get("behind_target").is_some(), "{entry}");
    assert!(entry["behind_target"].is_null(), "{entry}");
    assert!(entry.get("target_path_overlap").is_some(), "{entry}");
    assert!(entry["target_path_overlap"].is_null(), "{entry}");

    let text = stdout(repo.arc(&repo.root).args(["workspace", "backlog"]));
    assert!(text.contains("target distance unknown"), "{text}");
    assert!(text.contains("target path overlap unknown"), "{text}");
}

/// Debt is recorded per change, so a file carried by several obligations is
/// invisible from any one of them. Reviewing one such change does not read
/// that file's other unread revisions, and the report says which files those
/// are.
#[test]
fn workspace_backlog_names_a_path_more_than_one_obligation_carries() {
    let repo = Repo::new();
    for (slug, other) in [("feat-first", "first.txt"), ("feat-second", "second.txt")] {
        repo.arc(&repo.root)
            .args(["begin", slug, "--no-worktree", "--target", "master"])
            .assert()
            .success();
        // One file both changes touch, and one only this change touches.
        fs::write(repo.root.join("shared.txt"), format!("{slug}\n")).unwrap();
        fs::write(repo.root.join(other), "own\n").unwrap();
        git(&repo.root, &["add", "-A"]);
        git(&repo.root, &["commit", "-m", &format!("feat: {slug}")]);
        repo.arc(&repo.root).args(["snapshot"]).assert().success();
        repo.arc(&repo.root)
            .args([
                "debt",
                slug,
                "--reason",
                "no independent reviewer reachable",
            ])
            .assert()
            .success();
        // Integration merges into the target, which has to be the checkout.
        git(&repo.root, &["checkout", "master"]);
        repo.arc(&repo.root)
            .args(["integrate", slug])
            .assert()
            .success();
    }

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut report);
    let project = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .unwrap_or_else(|| panic!("project missing: {value}"));

    let shared = &project["shared_surfaces"];
    let carriers = shared["shared.txt"]
        .as_array()
        .unwrap_or_else(|| panic!("shared.txt not reported: {project}"));
    assert_eq!(carriers.len(), 2, "{shared}");
    // A path only one obligation carries is not shared, and saying so would
    // make every touched file look like a collision.
    assert!(shared.get("first.txt").is_none(), "{shared}");
    assert!(shared.get("second.txt").is_none(), "{shared}");
}

/// A recorded integration range can become unreadable after history is
/// rewritten. The debt remains real, while its surfaces become unknown.
#[test]
fn workspace_backlog_preserves_an_unreadable_debt_range() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-unreadable", "--no-worktree"])
        .assert()
        .success();
    fs::write(repo.root.join("work.txt"), "work\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "feat: work"]);
    repo.arc(&repo.root).args(["snapshot"]).assert().success();
    repo.arc(&repo.root)
        .args(["debt", "feat-unreadable", "--reason", "review unavailable"])
        .assert()
        .success();
    git(&repo.root, &["checkout", "master"]);
    repo.arc(&repo.root)
        .args(["integrate", "feat-unreadable"])
        .assert()
        .success();

    let event_dir = repo
        .root
        .join(".git/arc/changes")
        .read_dir()
        .unwrap()
        .find_map(|entry| {
            let path = entry.unwrap().path();
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("feat-unreadable")
                .then_some(path.join("events"))
        })
        .unwrap();
    let integration_path = event_dir
        .read_dir()
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            serde_json::from_slice::<serde_json::Value>(&fs::read(path).unwrap())
                .is_ok_and(|event| event["event_type"] == "change-integrated")
        })
        .unwrap();
    let mut integration: serde_json::Value =
        serde_json::from_slice(&fs::read(&integration_path).unwrap()).unwrap();
    integration["target_before"] = serde_json::json!("missing-target");
    integration["integrated_commit"] = serde_json::json!("missing-integration");
    fs::write(
        integration_path,
        serde_json::to_vec_pretty(&integration).unwrap(),
    )
    .unwrap();

    let mut json = repo.arc(&repo.root);
    json.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut json);
    let project = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .unwrap_or_else(|| panic!("project missing: {value}"));
    let debt = project["debt_owed"].as_array().unwrap().first().unwrap();
    assert!(debt.get("surfaces").is_some(), "{debt}");
    assert!(debt["surfaces"].is_null(), "{debt}");

    let text = stdout(repo.arc(&repo.root).args(["workspace", "backlog"]));
    assert!(text.contains("surfaces unknown"), "{text}");
}

/// The backlog is read to decide which obligation to open next. A row that says
/// only that a debt exists cannot answer that; the kind, what review the work
/// did have, and who produced it are what separate one obligation from another.
#[test]
fn workspace_backlog_debt_rows_carry_the_kind_and_its_coordinates() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "feat-owed", "--no-worktree"])
        .assert()
        .success();
    let brief = repo.home.join("owed-brief.md");
    fs::write(&brief, "do the thing\n").unwrap();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Planner")
        .env("ARC_MODEL", "gpt-5.6-sol#high")
        .args([
            "brief",
            "feat-owed",
            "--title",
            "Contract",
            "--body-file",
            brief.to_str().unwrap(),
        ])
        .assert()
        .success();
    fs::write(repo.root.join("work.txt"), "work\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "feat: work"]);
    repo.arc(&repo.root)
        .env("ARC_MODEL", "gpt-5.6-luna#max")
        .args(["snapshot"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args([
            "--model",
            "gpt-5.6-terra#low",
            "review",
            "feat-owed",
            "--verdict",
            "approved",
            "--route-version",
            "2026.09",
        ])
        .assert()
        .success();
    git(&repo.root, &["checkout", "master"]);
    repo.arc(&repo.root)
        .args(["integrate", "feat-owed", "--debt", "a second pass is owed"])
        .assert()
        .success();

    let value = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "backlog", "--json"]),
    );
    let project = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .unwrap_or_else(|| panic!("project missing: {value}"));
    let debt = project["debt_owed"].as_array().unwrap().first().unwrap();
    assert_eq!(debt["missing"], "independent-review", "{debt}");
    assert_eq!(debt["coverage"][0]["reviewer"], "Reviewer", "{debt}");
    assert_eq!(debt["coverage"][0]["model"], "gpt-5.6-terra#low", "{debt}");
    assert_eq!(debt["coverage"][0]["effort"], "low", "{debt}");
    assert_eq!(debt["coverage"][0]["route_version"], "2026.09", "{debt}");
    assert_eq!(debt["production"]["planner"]["actor"], "Planner", "{debt}");
    assert_eq!(debt["production"]["brief_version"], 1, "{debt}");
    assert_eq!(debt["production"]["implementer"]["effort"], "max", "{debt}");
    assert_eq!(debt["production"]["following_brief"], true, "{debt}");
    assert_eq!(
        value["summary"]["debt_owed_by_kind"][0]["kind"], "independent-review",
        "{value}"
    );
    assert_eq!(
        value["summary"]["debt_owed_by_kind"][0]["count"], 1,
        "{value}"
    );

    let text = stdout(repo.arc(&repo.root).args(["workspace", "backlog"]));
    assert!(
        text.contains(
            "debt: independent-review; planned by Planner@high (brief v1), \
implemented by tester@max; coverage: Reviewer@low [route 2026.09]"
        ),
        "{text}"
    );
    assert!(text.contains("debt-owed (independent-review 1)"), "{text}");
}

/// The human report ends with the one command that re-runs its exact scope
/// as itemized JSON, so a reader following the bare guide never reassembles
/// per-project reports by hand. The hint names the resolved scope, the
/// normalized cutoff, and the caller's --unreachable choice, and quotes
/// paths so awkward names survive the shell.
#[test]
fn workspace_backlog_detail_hint_preserves_selection() {
    // Space and apostrophe in one scope path: the hint must quote both.
    let outer = TempDir::new().unwrap();
    let scope = outer.path().join("ws it's");
    fs::create_dir_all(&scope).unwrap();

    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "registered", "the project exists"])
        .assert()
        .success();
    fs::rename(&repo.root, scope.join("repo")).unwrap();
    let repo_root = scope.join("repo");
    // The quoting under test: a single quote becomes '\'' inside a
    // single-quoted POSIX shell argument.
    let quoted_scope = format!("'{}'", scope.display().to_string().replace('\'', "'\\''"));

    let command_line = |args: &[&str], cwd: &Path| {
        let text = stdout(repo.arc(cwd).args(args));
        text.lines()
            .rev()
            .find(|line| line.trim_start().starts_with("detail:"))
            .map(|line| {
                line.trim_start()
                    .strip_prefix("detail: ")
                    .unwrap()
                    .to_string()
            })
            .unwrap_or_else(|| panic!("no detail hint in:\n{text}"))
    };

    // Ordinary scoped run: --under names the requested path canonically.
    let hint = command_line(
        &["workspace", "backlog", "--under", scope.to_str().unwrap()],
        &repo_root,
    );
    assert_eq!(
        hint,
        format!("arc workspace backlog --under {quoted_scope} --items --json",)
    );

    // --here resolves to the caller's directory; --since survives.
    let hint = command_line(
        &[
            "workspace",
            "backlog",
            "--here",
            "--since",
            "20990101T000000Z",
        ],
        &repo_root,
    );
    let quoted_cwd = format!(
        "'{}'",
        repo_root.display().to_string().replace('\'', "'\\''")
    );
    assert_eq!(
        hint,
        format!(
            "arc workspace backlog --under {quoted_cwd} --since 20990101T000000Z --items --json",
        )
    );

    // Global scope stays global, and --unreachable travels with it.
    let hint = command_line(
        &[
            "workspace",
            "backlog",
            "--global",
            "--unreachable",
            "--since",
            "2026-01-01T00:00:00Z",
        ],
        &repo_root,
    );
    assert_eq!(
        hint,
        "arc workspace backlog --global --since 2026-01-01T00:00:00Z --unreachable --items --json"
    );

    // Following the hint in an isolated fixture reproduces this report's
    // scope, item set, and counts exactly.
    let mut direct = repo.arc(&repo_root);
    direct
        .args([
            "workspace",
            "backlog",
            "--under",
            scope.to_str().unwrap(),
            "--items",
            "--json",
        ])
        .env_remove("ARC_JOURNAL_DIR");
    let expected = json_stdout(&mut direct);

    let hint = command_line(
        &["workspace", "backlog", "--under", scope.to_str().unwrap()],
        &repo_root,
    );
    // The hint is a POSIX shell command line, so it is followed the way a
    // shell would read it — quoting included — rather than re-split by hand.
    let mut shell = Command::new("sh");
    shell
        .arg("-c")
        .arg(&hint)
        .current_dir(&repo_root)
        // The hint names bare `arc`; put the binary under test first on PATH
        // so the follow exercises this build, not an installed one.
        .env(
            "PATH",
            format!(
                "{}:{}",
                PathBuf::from(env!("CARGO_BIN_EXE_arc"))
                    .parent()
                    .and_then(|dir| dir.to_str())
                    .unwrap_or_default(),
                std::env::var("PATH").as_deref().unwrap_or_default(),
            ),
        )
        .env("HOME", &repo.home)
        .env("ARC_SANDBOX", &repo.home)
        .env("ARC_ACTOR", "tester")
        .env("ARC_HARNESS", "test")
        .env("ARC_SESSION", "session-a")
        .env_remove("ARC_JOURNAL_DIR")
        .env_remove("ARC_MODEL")
        .env_remove("ARC_DATA_ROOT");
    let followed_out = shell.output().unwrap();
    assert!(followed_out.status.success(), "{hint}: {followed_out:?}");
    let actual: serde_json::Value =
        serde_json::from_str(std::str::from_utf8(&followed_out.stdout).unwrap())
            .unwrap_or_else(|error| panic!("{hint}: {error}"));
    assert_eq!(actual["scope"], expected["scope"], "{hint}");
    assert_eq!(actual["summary"], expected["summary"], "{hint}");
    assert_eq!(actual["projects"], expected["projects"], "{hint}");
    assert_eq!(actual["unreachable"], expected["unreachable"], "{hint}");

    // An empty report still names the command that would itemize it.
    let empty = outer.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let text = stdout(repo.arc(&repo_root).args([
        "workspace",
        "backlog",
        "--under",
        empty.to_str().unwrap(),
    ]));
    assert!(
        text.contains(&format!(
            "arc workspace backlog --under '{}' --items --json",
            empty.display()
        )),
        "{text}"
    );

    // JSON stays one parseable value: no footer may ride along.
    repo.arc(&repo_root)
        .args(["workspace", "backlog", "--items", "--json"])
        .assert()
        .success();
    let text = stdout(
        repo.arc(&repo_root)
            .args(["workspace", "backlog", "--items", "--json"]),
    );
    serde_json::from_str::<serde_json::Value>(&text).unwrap();
}

/// One timestamp interpretation across the queue: a legacy stamp (no `Z`)
/// filters under a cutoff exactly as the canonical form of the same instant,
/// an unreadable stamp stays visible and is counted as unknown time instead
/// of being dropped or dated, and the JSON states the selection itself.
#[test]
fn workspace_backlog_timestamp_interpretation_is_explicit() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "registered", "the project exists"])
        .assert()
        .success();
    let journal = journal_dir_of(&repo);
    // Five July 2026 rows: three canonical, two legacy, all the same month.
    for (file, kind) in [
        ("20260701T000000Z-canon-open-todo.md", "todo"),
        ("20260702T000000-canon-open-todo.md", "todo"),
        ("20260703T000000Z-canon-open-handoff.md", "handoff"),
        ("20260704T000000Z-canon-open-later.md", "later"),
        (
            "20260705T000000-canon-open-feature-request.md",
            "feature-request",
        ),
        ("20260706T0000XX-strictly-not-a-stamp-todo.md", "todo"),
    ] {
        fs::write(journal.join(file), format!("# {}\n\nbody\n", kind)).unwrap();
    }

    // A cutoff between the two months: five July rows arrive, whatever form
    // their stamp was written in; the malformed one is unknown time.
    let mut report = repo.arc(&repo.root);
    report.args([
        "workspace",
        "backlog",
        "--items",
        "--json",
        "--since",
        "20260601T000000Z",
    ]);
    let value = json_stdout(&mut report);
    assert_eq!(value["schema"], "arc-workspace-backlog/12");
    let selection = &value["selection"];
    assert_eq!(selection["since"], "2026-06-01T00:00:00Z", "{}", selection);
    assert_eq!(selection["journal_counts"], "arrivals");
    assert_eq!(selection["includes_unknown_time"], true);

    let project = value["projects"].as_array().unwrap().first().unwrap();
    // The undated row rides inside its tier (fail-open visibility), so open
    // counts 3 dated + 1 unknown; the separate count states the subset.
    assert_eq!(project["open_items"], 4, "{}", project);
    assert_eq!(project["later_items"], 1);
    assert_eq!(project["feature_requests"], 1);
    let unknown = project["unknown_time_items"]
        .as_array()
        .unwrap_or(&Vec::new())
        .clone();
    assert_eq!(unknown.len(), 1, "{}", project);
    assert_eq!(
        unknown[0]["file"],
        "20260706T0000XX-strictly-not-a-stamp-todo.md"
    );
    assert!(unknown[0]["filed_at"].is_null(), "{}", unknown[0]);
    assert_eq!(unknown[0]["timestamp_status"], "invalid");
    assert_eq!(value["summary"]["unknown_time_items"], 1);

    // Every emitted row states how its stamp read; the malformed one is the
    // only invalid row in the whole report.
    // The undated row appears inside its tier listing and is mirrored by
    // unknown_time_items, which is a pointer to a subset, not a move.
    let mut all_rows: Vec<&serde_json::Value> = project["items"]["open"]
        .as_array()
        .unwrap()
        .iter()
        .collect();
    all_rows.extend(project["items"]["later"].as_array().unwrap().iter());
    all_rows.extend(
        project["items"]["feature_requests"]
            .as_array()
            .unwrap()
            .iter(),
    );
    assert_eq!(all_rows.len(), 6, "{}", project);
    let invalid: Vec<&&serde_json::Value> = all_rows
        .iter()
        .filter(|row| row["timestamp_status"] == "invalid")
        .collect();
    assert_eq!(invalid.len(), 1, "{}", project);
    assert_eq!(invalid[0]["file"], unknown[0]["file"]);
    assert_eq!(invalid[0]["timestamp_status"], "invalid");
    // A dated row carries both the raw stamp and its RFC 3339 reading.
    let canonical_row = all_rows
        .iter()
        .find(|row| row["file"] == "20260701T000000Z-canon-open-todo.md")
        .unwrap();
    assert_eq!(canonical_row["timestamp_status"], "canonical");
    assert_eq!(canonical_row["filed_at"], "2026-07-01T00:00:00Z");
    let legacy_row = all_rows
        .iter()
        .find(|row| row["timestamp_status"] == "legacy")
        .unwrap();
    assert_eq!(legacy_row["file"], "20260702T000000-canon-open-todo.md");
    assert_eq!(legacy_row["filed_at"], "2026-07-02T00:00:00Z");

    // Same digits, both forms, in an isolated second journal: the two forms
    // of one instant are identical to the cutoff logic.
    let repo2 = Repo::new();
    repo2
        .arc(&repo2.root)
        .args(["journal", "log", "registered", "the project exists"])
        .assert()
        .success();
    let journal2 = journal_dir_of(&repo2);
    fs::write(
        journal2.join("20260801T000000Z-twin-open-todo.md"),
        "# twin\n",
    )
    .unwrap();
    fs::write(
        journal2.join("20260801T000000-twin-2-open-todo.md"),
        "# twin\n",
    )
    .unwrap();
    // Before the instant: both forms arrive. After it: both drop, and the
    // report is empty for that project. The two forms of one instant filter
    // identically on both sides of the boundary.
    let mut report = repo2.arc(&repo2.root);
    report.args([
        "workspace",
        "backlog",
        "--items",
        "--json",
        "--since",
        "2026-07-31T23:59:59Z",
    ]);
    let value = json_stdout(&mut report);
    let open_rows = value["projects"].as_array().unwrap()[0]["items"]["open"]
        .as_array()
        .unwrap();
    assert_eq!(open_rows.len(), 2, "{value}");
    assert_eq!(open_rows[0]["timestamp_status"], "canonical");
    assert_eq!(open_rows[1]["timestamp_status"], "legacy");
    assert_eq!(open_rows[0]["filed_at"], open_rows[1]["filed_at"]);

    let mut report = repo2.arc(&repo2.root);
    report.args([
        "workspace",
        "backlog",
        "--items",
        "--json",
        "--since",
        "2026-08-01T00:00:01Z",
    ]);
    let value = json_stdout(&mut report);
    assert!(value["projects"].as_array().unwrap().is_empty(), "{value}");
}

/// Recorded debt versus effective obligation: two typed nothing-read
/// obligations and one legacy untyped obligation produce effective counts of
/// nothing-read 2 and independent-review 1, with the legacy subset counted.
/// The legacy row still carries no recorded missing; grouping rows by
/// effective_missing reproduces the summary split. Event bytes and discharge
/// behavior stay untouched.
#[test]
fn workspace_backlog_distinguishes_recorded_debt_from_legacy_default() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);

    let ship_with_debt = |slug: &str| {
        let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])));
        let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
        repo.commit(
            &worktree,
            &format!("{slug}.txt"),
            &format!("{slug}\n"),
            &format!("feat: {slug}"),
        );
        stdout(repo.arc(&worktree).args(["snapshot", slug]));
        repo.arc(&repo.root)
            .args(["integrate", slug, "--debt", "unreviewed on purpose"])
            .assert()
            .success();
        change_id
    };
    let independent = ship_with_debt("typed-independent");
    let nothing = ship_with_debt("typed-nothing");
    let legacy = ship_with_debt("legacy-untyped");

    // The legacy one is rewritten to the pre-kind event shape: the untyped
    // audit-debt-declared that earlier builds wrote.
    rewrite_event(&repo, &legacy, "debt-declared", |event| {
        event["event_type"] = serde_json::json!("audit-debt-declared");
        event.as_object_mut().unwrap().remove("missing");
        event.as_object_mut().unwrap().remove("coverage");
        event.as_object_mut().unwrap().remove("production");
    });

    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut report);
    let project = value["projects"].as_array().unwrap().first().unwrap();
    let debts = project["debt_owed"].as_array().unwrap();
    assert_eq!(debts.len(), 3, "{}", project);

    // A shipped-with-verdict debt records contributor-only; two obligations
    // carrying a verdict behind its debt would make the kind
    // independent-review, but a bare ship derives nothing-read. The typed
    // pair below is produced by giving the first change one verdict, so its
    // effective kind is contributor-only by the ledger, not by declaration.
    let by_change: std::collections::BTreeMap<&str, &serde_json::Value> = debts
        .iter()
        .map(|row| (row["change_id"].as_str().unwrap(), row))
        .collect();
    let independent_row = by_change[independent.as_str()];
    let nothing_row = by_change[nothing.as_str()];
    let legacy_row = by_change[legacy.as_str()];
    let _ = (independent_row, nothing_row, legacy_row);

    // The legacy row still has no recorded kind; its effective value is the
    // meaning the readers already give it, and its basis says so.
    assert!(legacy_row.get("missing").is_none(), "{}", legacy_row);
    assert_eq!(legacy_row["effective_missing"], "independent-review");
    assert_eq!(legacy_row["missing_basis"], "legacy-default");
    assert_eq!(legacy_row["typed"], false);

    // The two typed rows carry their recorded kinds and a recorded basis.
    assert_eq!(independent_row["missing"], "nothing-read");
    assert_eq!(independent_row["effective_missing"], "nothing-read");
    assert_eq!(independent_row["missing_basis"], "recorded");
    assert_eq!(independent_row["typed"], true);
    assert_eq!(nothing_row["missing"], "nothing-read");
    assert_eq!(nothing_row["missing_basis"], "recorded");

    // The summary counts the effective population and names its legacy subset:
    // two nothing-read and one legacy row that reads as independent-review.
    let kinds: Vec<&serde_json::Value> = value["summary"]["debt_owed_by_kind"]
        .as_array()
        .unwrap()
        .iter()
        .collect();
    let kind_of = |name: &str| {
        kinds
            .iter()
            .find(|entry| entry["kind"] == name)
            .map(|entry| entry["count"].as_u64().unwrap())
            .unwrap_or(0)
    };
    assert_eq!(kind_of("nothing-read"), 2);
    assert_eq!(kind_of("independent-review"), 1);
    assert_eq!(value["summary"]["legacy_debt_owed"], 1);
    assert_eq!(value["summary"]["debt_owed"], 3);

    // The text view names the legacy subset beside its row.
    let text = stdout(repo.arc(&repo.root).args(["workspace", "backlog"]));
    assert!(
        text.contains("3 debt-owed, 1 legacy-untyped (nothing-read 2, independent-review 1)"),
        "{}",
        text
    );
    assert!(text.contains("legacy event, no kind recorded"), "{}", text);
    // Discharge still works on the legacy row: the audit path is unchanged.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args(["audit", "legacy-untyped", "--verdict", "approved"])
        .assert()
        .success();
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "legacy-untyped", "--json"]),
    );
    assert_eq!(status["debt_outstanding"], false, "{status}");
}
