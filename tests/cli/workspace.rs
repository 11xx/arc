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
    let deferred = projects
        .iter()
        .map(|project| {
            project["changes"]["deferred"]
                .as_array()
                .map_or(0, Vec::len)
        })
        .sum::<usize>();
    assert_eq!(value["summary"]["deferred"], deferred);
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
    assert_eq!(value["schema"], "arc-workspace-backlog/18");
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

    // The unreachable journal is a failed observation in the census, never an
    // observed-empty project, and the command says the collection is partial.
    assert_eq!(value["collection"]["failed"], 1, "{value}");
    assert_eq!(value["collection"]["empty"], 0, "{value}");
    let failure = value["collection"]["failures"]
        .as_array()
        .unwrap()
        .first()
        .unwrap();
    assert_eq!(failure["component"], "anchor", "{value}");
    assert!(
        failure["reason"]
            .as_str()
            .unwrap()
            .contains("anchor does not exist"),
        "{value}"
    );
}

/// The collection manifest counts discovered, selected, skipped, empty,
/// non-empty, and failed projects, and a failed observation never reads as
/// healthy emptiness.
#[test]
fn workspace_backlog_collection_manifest_counts_failures_and_empties() {
    use std::os::unix::fs::PermissionsExt;

    let outer = TempDir::new().unwrap();
    let shared_home = outer.path().join("home");
    fs::create_dir_all(&shared_home).unwrap();
    let shared = |repo: &Repo| {
        let mut cmd = repo.arc(&repo.root);
        cmd.env("HOME", &shared_home)
            .env("ARC_SANDBOX", &shared_home);
        cmd
    };

    let healthy = Repo::new();
    shared(&healthy)
        .args(["begin", "collection-healthy", "--no-worktree"])
        .assert()
        .success();

    // Registered, no ledger and no artifacts: observed and empty.
    let empty = Repo::new();
    shared(&empty)
        .args(["journal", "log", "registered", "exists"])
        .assert()
        .success();

    // Registered journal whose event log cannot be read.
    let broken = Repo::new();
    shared(&broken)
        .args(["journal", "log", "registered", "exists"])
        .assert()
        .success();
    let broken_journal = PathBuf::from(stdout(shared(&broken).args(["journal", "dir"])).trim());
    let events = broken_journal.join("events.jsonl");
    assert!(events.is_file(), "{broken_journal:?}");
    fs::set_permissions(&events, fs::Permissions::from_mode(0o000)).unwrap();

    let run = |args: &[&str]| -> (Option<i32>, serde_json::Value) {
        let mut cmd = shared(&healthy);
        cmd.args(["workspace", "backlog", "--json"]).args(args);
        let output = cmd.output().unwrap();
        let value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stdout)));
        (output.status.code(), value)
    };

    let (code, value) = run(&[]);
    assert_eq!(code, Some(16), "{value}");
    let collection = &value["collection"];
    assert_eq!(collection["discovered"], 3, "{value}");
    assert_eq!(collection["selected"], 3, "{value}");
    assert_eq!(collection["skipped"], 0, "{value}");
    assert_eq!(collection["empty"], 1, "{value}");
    assert_eq!(collection["non_empty"], 1, "{value}");
    assert_eq!(collection["failed"], 1, "{value}");
    assert_eq!(
        collection["discovered"].as_u64().unwrap(),
        collection["selected"].as_u64().unwrap() + collection["skipped"].as_u64().unwrap()
    );
    assert_eq!(
        collection["selected"].as_u64().unwrap(),
        collection["empty"].as_u64().unwrap()
            + collection["non_empty"].as_u64().unwrap()
            + collection["failed"].as_u64().unwrap()
    );
    let failure = collection["failures"].as_array().unwrap().first().unwrap();
    assert_eq!(failure["component"], "journal", "{value}");
    assert!(
        failure["reason"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("permission"),
        "{value}"
    );

    // The partial row survives beside the healthy one; the empty project is
    // censused rather than listed.
    let anchor_of = |repo: &Repo| fs::canonicalize(&repo.root).unwrap().display().to_string();
    let anchors: Vec<&str> = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|project| project["anchor"].as_str().unwrap())
        .collect();
    assert!(anchors.contains(&anchor_of(&healthy).as_str()), "{value}");
    assert!(anchors.contains(&anchor_of(&broken).as_str()), "{value}");
    assert!(!anchors.contains(&anchor_of(&empty).as_str()), "{value}");
    let broken_row = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["anchor"] == anchor_of(&broken).as_str())
        .unwrap();
    assert_eq!(broken_row["failures"][0]["component"], "journal", "{value}");

    fs::set_permissions(&events, fs::Permissions::from_mode(0o644)).unwrap();

    // A project outside the scope is skipped, not empty, and a partial report
    // becomes a clean one when the failing project is out of scope.
    let under = fs::canonicalize(&healthy.root)
        .unwrap()
        .parent()
        .unwrap()
        .display()
        .to_string();
    let (code, value) = run(&["--under", &under]);
    assert_eq!(code, Some(0), "{value}");
    let collection = &value["collection"];
    assert_eq!(collection["discovered"], 3, "{value}");
    assert_eq!(collection["selected"], 1, "{value}");
    assert_eq!(collection["skipped"], 2, "{value}");
    assert_eq!(collection["empty"], 0, "{value}");
    assert_eq!(collection["non_empty"], 1, "{value}");
    assert_eq!(collection["failed"], 0, "{value}");
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
    assert_eq!(value["schema"], "arc-workspace-backlog/18");
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
    assert_eq!(value["schema"], "arc-workspace-backlog/18");
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
    // The moved comparison reads the anchor's ledger for rewrite records.
    // A ledger comes from opening a change, the way every real project has
    // one; a queue rendering must not create it as a side effect.
    repo.arc(&repo.root)
        .args(["begin", "ledger-holder", "--no-worktree"])
        .assert()
        .success();
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
    let under_hint = command_line(
        &["workspace", "backlog", "--under", scope.to_str().unwrap()],
        &repo_root,
    );

    // --here resolves to the caller's directory; --since survives.
    let here_hint = command_line(
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

    // Global scope stays global, and --unreachable travels with it.
    let global_hint = command_line(
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

    // Chrono accepts the RFC 3339 form with a space between date and time.
    // Following the emitted shell command must preserve that one argument.
    let spaced_hint = command_line(
        &[
            "workspace",
            "backlog",
            "--global",
            "--since",
            "2026-01-01 00:00:00Z",
        ],
        &repo_root,
    );
    let mut spaced_shell = Command::new("sh");
    spaced_shell
        .arg("-c")
        .arg(&spaced_hint)
        .current_dir(&repo_root)
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
    let spaced_output = spaced_shell.output().unwrap();
    assert!(
        matches!(spaced_output.status.code(), Some(0) | Some(16)),
        "{spaced_hint}: {spaced_output:?}"
    );
    serde_json::from_slice::<serde_json::Value>(&spaced_output.stdout)
        .unwrap_or_else(|error| panic!("{spaced_hint}: {error}"));
    assert_eq!(
        under_hint,
        format!("arc workspace backlog --under {quoted_scope} --items --json",)
    );
    assert_eq!(
        here_hint,
        format!(
            "arc workspace backlog --under {quoted_cwd} --since '2099-01-01T00:00:00Z' --items --json",
        )
    );
    assert_eq!(
        global_hint,
        "arc workspace backlog --global --since '2026-01-01T00:00:00Z' --unreachable --items --json"
    );
    assert_eq!(
        spaced_hint,
        "arc workspace backlog --global --since '2026-01-01T00:00:00Z' --items --json"
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

    // JSON stays one parseable value: no footer may ride along. The project
    // anchor moved with the fixture, so the collection is partial and exits
    // 16; the value must still parse.
    repo.arc(&repo_root)
        .args(["workspace", "backlog", "--items", "--json"])
        .assert()
        .code(16);
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
    assert_eq!(value["schema"], "arc-workspace-backlog/18");
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

    // Fractional RFC 3339 precision is part of the filtering boundary. The
    // emitted cutoff must replay the same file set, not round down to the
    // second and re-admit both exact-second rows.
    let mut report = repo2.arc(&repo2.root);
    report.args([
        "workspace",
        "backlog",
        "--items",
        "--json",
        "--since",
        "2026-08-01T00:00:00.500Z",
    ]);
    let fractional = json_stdout(&mut report);
    let emitted_since = fractional["selection"]["since"]
        .as_str()
        .unwrap()
        .to_string();
    let files = |value: &serde_json::Value| {
        let mut files = Vec::new();
        if let Some(projects) = value["projects"].as_array() {
            for project in projects {
                for tier in ["open", "later", "feature_requests"] {
                    if let Some(items) = project["items"][tier].as_array() {
                        files.extend(
                            items
                                .iter()
                                .map(|item| item["file"].as_str().unwrap().to_string()),
                        );
                    }
                }
            }
        }
        files.sort();
        files
    };
    let fractional_files = files(&fractional);
    let mut replay = repo2.arc(&repo2.root);
    replay.args([
        "workspace",
        "backlog",
        "--items",
        "--json",
        "--since",
        &emitted_since,
    ]);
    let replayed = json_stdout(&mut replay);
    assert_eq!(
        files(&replayed),
        fractional_files,
        "emitted cutoff {emitted_since:?} changed the selected files"
    );
    assert_eq!(emitted_since, "2026-08-01T00:00:00.500Z");

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

/// Questions and project journal paths: every reachable project names its
/// journal directory and carries its unanswered questions; a project with
/// zero positions but an open question ranks above an artifact-only project;
/// the rollup moves exactly as the local question view moves; and questions
/// survive a --since filter, which applies only to artifact arrivals.
#[test]
fn workspace_backlog_carries_questions_and_journal_paths() {
    // Three projects: two with an unanswered question and nothing else, one
    // with an ordinary todo artifact only. They share one registry home so a
    // global report discovers all of them, which is what one AI home is on a
    // real machine.
    let outer = TempDir::new().unwrap();
    let shared_home = outer.path().join("home");
    fs::create_dir_all(&shared_home).unwrap();
    let shared = |repo: &Repo| {
        let mut cmd = repo.arc(&repo.root);
        cmd.env("HOME", &shared_home)
            .env("ARC_SANDBOX", &shared_home);
        cmd
    };
    let mut question_repos = Vec::new();
    for slug in ["question-alpha", "question-beta"] {
        let repo = Repo::new();
        shared(&repo)
            .args(["journal", "log", "registered", "the project exists"])
            .assert()
            .success();
        let path = stdout(
            shared(&repo)
                .args([
                    "journal",
                    "note",
                    slug,
                    "--kind",
                    "discussion",
                    "--body-file",
                    "-",
                ])
                .write_stdin(format!("# {slug}\n\nAn open question needs an answer.\n")),
        )
        .trim()
        .to_string();
        let file = PathBuf::from(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        shared(&repo)
            .args([
                "journal",
                "question",
                &file,
                "--placement",
                "opening",
                "--option",
                "yes",
                "--option",
                "no",
                "--body-file",
                "-",
            ])
            .write_stdin(format!("Which way for {slug}?\n"))
            .assert()
            .success();
        // Sanity: the local view sees it.
        let local = json_stdout(shared(&repo).args(["journal", "questions", "--json"]));
        assert_eq!(local["questions"].as_array().unwrap().len(), 1, "{local}");
        question_repos.push((repo, file));
    }
    let plain = Repo::new();
    shared(&plain)
        .args(["journal", "log", "registered", "the project exists"])
        .assert()
        .success();
    let plain_journal = PathBuf::from(stdout(shared(&plain).args(["journal", "dir"])).trim());
    fs::write(
        plain_journal.join("20260101T000000Z-plain-open-todo.md"),
        "# Plain\n",
    )
    .unwrap();

    // All three must be discoverable from one scope: everything sits under
    // the same sandbox home, so a global report from one of them finds the
    // rest through the registry.
    let repo = &question_repos[0].0;
    let mut report = shared(repo);
    report.args(["workspace", "backlog", "--items", "--json"]);
    let value = json_stdout(&mut report);
    let projects = value["projects"].as_array().unwrap();
    // Every fixture names its repo directory `repo`, so the rows are
    // distinguished by the question each carries rather than by label.
    let by_question = |part: &str| {
        projects
            .iter()
            .find(|project| {
                project["open_questions"].as_array().is_some_and(|rows| {
                    rows.iter()
                        .any(|row| row["heading"].as_str().unwrap_or("").contains(part))
                })
            })
            .unwrap_or_else(|| panic!("{part} missing from {value}"))
    };
    // Ranking: question projects above the artifact-only project, which has
    // no questions at all. Each row names its journal dir.
    let alpha = by_question("question-alpha");
    let beta = by_question("question-beta");
    let plain_project = projects
        .iter()
        .find(|project| {
            project["open_questions"]
                .as_array()
                .is_some_and(Vec::is_empty)
        })
        .unwrap();
    assert_eq!(alpha["decision_questions"], 1, "{alpha}");
    assert_eq!(beta["decision_questions"], 1);
    assert_eq!(plain_project["decision_questions"], 0);
    assert_eq!(value["summary"]["decision_questions"], 2);
    let rank = |project: &serde_json::Value| {
        projects
            .iter()
            .position(|candidate| candidate["anchor"] == project["anchor"])
            .unwrap()
    };
    assert!(rank(alpha) < rank(plain_project), "{}", value);
    assert!(rank(beta) < rank(plain_project), "{}", value);

    // Question shape: placement, file, and the artifact disposition.
    let question = &alpha["open_questions"][0];
    assert_eq!(question["placement"], "opening", "{}", question);
    assert_eq!(question["disposition"], "open");
    // settle_by is absent on the classic default, which reads as a person.
    assert!(question.get("settle_by").is_none(), "{}", question);
    assert_eq!(alpha["opening_question_count"], 1);
    assert_eq!(alpha["closing_question_count"], 0);
    // The journal dir points at a real directory in the registry home.
    assert!(
        PathBuf::from(alpha["journal_dir"].as_str().unwrap()).is_dir(),
        "{}",
        alpha
    );

    // The rollup moves exactly as the local view: answering removes it,
    // retracting the answer puts it back.
    let (beta_repo, beta_file) = &question_repos[1];
    let local = json_stdout(shared(beta_repo).args(["journal", "questions", "--json"]));
    let question_id = local["questions"][0]["question"]
        .as_str()
        .unwrap()
        .to_string();
    shared(beta_repo)
        .args([
            "journal",
            "answer",
            beta_file,
            "--question",
            &question_id,
            "--option",
            "yes",
            "--body-file",
            "-",
        ])
        .write_stdin("Going with yes.\n")
        .assert()
        .success();
    let mut report = shared(repo);
    report.args(["workspace", "backlog", "--json"]);
    let value = json_stdout(&mut report);
    assert_eq!(
        value["summary"]["decision_questions"], 1,
        "answered question left the rollup: {}",
        value
    );
    // A question on a consumed artifact is visible but not ranked.
    let (_, alpha_file) = &question_repos[0];
    // Consume with the question deliberately dropped: the decision moves to
    // its own artifact, and the old question becomes an unresolved record.
    shared(repo)
        .args([
            "journal",
            "consume",
            alpha_file,
            "--outcome",
            "done",
            "--drop-questions",
            "--note",
            "the decision moved to its own artifact",
        ])
        .assert()
        .success();
    let mut report = shared(repo);
    report.args(["workspace", "backlog", "--items", "--json"]);
    let value = json_stdout(&mut report);
    // Alpha's question was answered and its artifact consumed, so its
    // decision no longer counts as waiting. A question on a consumed
    // artifact is still visible, carried as an unresolved record.
    let decisions = value["summary"]["decision_questions"].as_u64().unwrap();
    assert_eq!(
        decisions, 0,
        "consumed artifact's question still counted: {}",
        value
    );
    assert_eq!(
        value["summary"]["unresolved_question_records"], 1,
        "{}",
        value
    );

    // --since retains unanswered decisions: only artifact arrivals filter.
    // Alpha's unresolved record survives under a cutoff every artifact
    // predates; the artifact tiers empty out.
    let mut report = shared(repo);
    report.args([
        "workspace",
        "backlog",
        "--json",
        "--since",
        "20990101T000000Z",
    ]);
    let value = json_stdout(&mut report);
    let alpha_only = value["projects"].as_array().unwrap().first().unwrap();
    assert_eq!(alpha_only["decision_questions"], 0, "{}", value);
    assert_eq!(alpha_only["open_questions"].as_array().unwrap().len(), 1);
    assert_eq!(value["summary"]["unresolved_question_records"], 1);
    assert_eq!(value["summary"]["open_items"], 0);
}

/// Ranking reads the declared basis, and the three facts it can rank on stay
/// separate fields: a completed project holding routine debt is not waiting on
/// a decision, and choosing another basis reorders the rows.
#[test]
fn workspace_backlog_ranks_by_a_declared_fact_and_keeps_them_separate() {
    let outer = TempDir::new().unwrap();
    let shared_home = outer.path().join("home");
    fs::create_dir_all(&shared_home).unwrap();
    let shared = |repo: &Repo| {
        let mut cmd = repo.arc(&repo.root);
        cmd.env("HOME", &shared_home)
            .env("ARC_SANDBOX", &shared_home);
        cmd
    };
    let anchor_of = |repo: &Repo| fs::canonicalize(&repo.root).unwrap().display().to_string();

    // Completed work carrying routine coverage debt: no primary work, no
    // question, and nothing waiting on a person.
    let debt_project = Repo::new();
    shared(&debt_project)
        .args(["begin", "feat-complete"])
        .assert()
        .success();
    let worktree = shared_home.join(".worktrees/repo-feat-complete");
    debt_project.commit(&worktree, "done.txt", "done\n", "feat: done");
    shared(&debt_project)
        .args(["snapshot", "feat-complete"])
        .assert()
        .success();
    shared(&debt_project)
        .args(["review", "feat-complete", "--verdict", "approved"])
        .assert()
        .success();
    shared(&debt_project)
        .args(["integrate", "feat-complete", "--debt", "review later"])
        .assert()
        .success();

    // A project waiting on a decision.
    let decision_project = Repo::new();
    let discussion = stdout(
        shared(&decision_project)
            .args([
                "journal",
                "note",
                "shape",
                "--kind",
                "discussion",
                "--body-file",
                "-",
            ])
            .write_stdin("# Shape\n"),
    );
    let discussion = PathBuf::from(discussion.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    shared(&decision_project)
        .args([
            "journal",
            "question",
            &discussion,
            "--placement",
            "opening",
            "--option",
            "yes",
            "--option",
            "no",
            "--body-file",
            "-",
        ])
        .write_stdin("Which shape?\n")
        .assert()
        .success();

    // A project with primary work and neither obligation.
    let work_project = Repo::new();
    shared(&work_project)
        .args([
            "journal",
            "note",
            "todo-work",
            "--kind",
            "todo",
            "--body-file",
            "-",
        ])
        .write_stdin("# Work\n")
        .assert()
        .success();

    // A registered project with nothing outstanding is not a row.
    let empty_project = Repo::new();
    shared(&empty_project)
        .args(["journal", "log", "registered", "exists"])
        .assert()
        .success();

    let report = |args: &[&str]| {
        let mut cmd = shared(&debt_project);
        cmd.args(["workspace", "backlog", "--json"]).args(args);
        json_stdout(&mut cmd)
    };
    let find = |value: &serde_json::Value, repo: &Repo| {
        let anchor = anchor_of(repo);
        value["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|project| project["anchor"] == anchor.as_str())
            .cloned()
            .unwrap_or_else(|| panic!("{anchor} missing from {value}"))
    };
    let rank = |value: &serde_json::Value, repo: &Repo| {
        let anchor = anchor_of(repo);
        value["projects"]
            .as_array()
            .unwrap()
            .iter()
            .position(|project| project["anchor"] == anchor.as_str())
            .unwrap_or_else(|| panic!("{anchor} missing from {value}"))
    };

    let value = report(&[]);
    assert_eq!(value["schema"], "arc-workspace-backlog/18");
    assert_eq!(value["ordering"]["basis"], "blocking", "{value}");
    assert_eq!(value["ordering"]["direction"], "descending");

    let debt = find(&value, &debt_project);
    assert_eq!(debt["coverage"], 1, "{debt}");
    assert_eq!(debt["blocking"], 0, "{debt}");
    assert_eq!(debt["availability"], 0, "{debt}");
    let decision = find(&value, &decision_project);
    assert_eq!(decision["blocking"], 1, "{decision}");
    assert_eq!(decision["coverage"], 0, "{decision}");
    let work = find(&value, &work_project);
    assert_eq!(work["availability"], 1, "{work}");
    assert_eq!(work["blocking"], 0, "{work}");
    assert_eq!(work["coverage"], 0, "{work}");
    assert!(
        !value["projects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|project| project["anchor"] == anchor_of(&empty_project).as_str()),
        "{value}"
    );

    // Ranking by blocking puts the decision first; coverage reverses the two.
    assert_eq!(rank(&value, &decision_project), 0, "{value}");
    assert!(rank(&value, &work_project) < rank(&value, &debt_project));
    let by_coverage = report(&["--rank-by", "coverage"]);
    assert_eq!(by_coverage["ordering"]["basis"], "coverage");
    assert_eq!(rank(&by_coverage, &debt_project), 0, "{by_coverage}");

    // The text report states its basis, and the replay keeps a non-default.
    let text =
        stdout(shared(&debt_project).args(["workspace", "backlog", "--rank-by", "coverage"]));
    assert!(text.contains("ordering: coverage (descending)"), "{text}");
    assert!(text.contains("--rank-by coverage"), "{text}");
}

/// The backlog keeps a project visible when its only fact is an approved but
/// held change or an uncollected round deferral: the two states the compact
/// ledger queues never enumerate.
#[test]
fn workspace_backlog_keeps_held_only_and_deferred_only_projects() {
    // An approved change under a hold has no review owed and no debt, so the
    // project's only fact is the hold.
    let held = Repo::new();
    let (held_id, ..) = change_with_patchset(&held, "only-held");
    held.arc(&held.root)
        .args(["review", "only-held", "--verdict", "approved"])
        .assert()
        .success();
    held.arc(&held.root)
        .args(["hold", "only-held", "--reason", "waiting on upstream"])
        .assert()
        .success();

    let value = json_stdout(
        held.arc(&held.root)
            .args(["workspace", "backlog", "--json"]),
    );
    assert_eq!(value["schema"], "arc-workspace-backlog/18");
    let projects = value["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1, "{value}");
    let held_rows = projects[0]["changes"]["held"].as_array().unwrap();
    assert!(
        held_rows
            .iter()
            .any(|row| row["change_id"] == held_id.as_str()),
        "{value}"
    );
    assert_eq!(projects[0]["blocking"], 0, "{value}");

    // A registered project with only a deferred round has no change bucket at
    // all; the deferral alone keeps it on the report.
    let deferred = Repo::new();
    deferred
        .arc(&deferred.root)
        .args(["journal", "log", "registered", "exists"])
        .assert()
        .success();
    let dispatch = stdout(deferred.arc(&deferred.root).args([
        "run",
        "dispatch",
        "--route",
        "r",
        "--worktree",
        "w",
        "--fork",
        "spike",
    ]));
    let dispatch = dispatch
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();
    let body = deferred.root.join("deferred.json");
    fs::write(
        &body,
        r#"[{"summary": "the listing is O(n^2)", "why": "n is under ten in every real ledger"}]"#,
    )
    .unwrap();
    deferred
        .arc(&deferred.root)
        .args([
            "run",
            "end",
            &dispatch,
            "--outcome",
            "completed",
            "--deferred-json",
            body.to_str().unwrap(),
        ])
        .assert()
        .success();

    let value = json_stdout(
        deferred
            .arc(&deferred.root)
            .args(["workspace", "backlog", "--json"]),
    );
    let projects = value["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1, "{value}");
    let deferrals = projects[0]["changes"]["deferred"].as_array().unwrap();
    assert_eq!(deferrals.len(), 1, "{value}");
    assert!(
        deferrals[0]["why"]
            .as_str()
            .unwrap()
            .contains("n is under ten"),
        "{value}"
    );
    assert_eq!(value["summary"]["deferred"], 1, "{value}");
}

/// Every predicate the inbox can assign appears in the backlog's per-project
/// change block, and a state no bucket claims lands in `unclassified` with its
/// reason rather than nowhere.
#[test]
fn workspace_backlog_exposes_every_change_predicate() {
    let repo = Repo::new();
    let mut expected: Vec<(String, &str)> = Vec::new();

    let (ready, ..) = change_with_patchset(&repo, "pred-ready");
    repo.arc(&repo.root)
        .args(["review", "pred-ready", "--verdict", "approved"])
        .assert()
        .success();
    expected.push((ready, "ready-to-integrate"));

    let (held, ..) = change_with_patchset(&repo, "pred-held");
    repo.arc(&repo.root)
        .args(["review", "pred-held", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["hold", "pred-held", "--reason", "pause"])
        .assert()
        .success();
    expected.push((held, "held"));

    let (requested, ..) = change_with_patchset(&repo, "pred-requested");
    repo.arc(&repo.root)
        .args([
            "review",
            "pred-requested",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();
    expected.push((requested, "changes-requested"));

    let (unclassified, ..) = change_with_patchset(&repo, "pred-unclassified");
    repo.arc(&repo.root)
        .args(["review", "pred-unclassified", "--verdict", "comment-only"])
        .assert()
        .success();
    expected.push((unclassified.clone(), "unclassified"));

    let blocker = begin_change(&repo, "pred-blocker", None);
    let blocked = begin_change(&repo, "pred-blocked", Some(&blocker));
    expected.push((blocked, "blocked"));

    let iterating = begin_no_worktree(&repo, "pred-iterating", &["--iterating"]);
    expected.push((iterating, "iterating"));

    let stalled = begin_change(&repo, "pred-stalled", None);
    repo.arc(&repo.root)
        .args(["claim", "pred-stalled", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    age_event(&repo, &stalled, "claim-set", 120);
    expected.push((stalled, "stalled"));

    let value = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "backlog", "--json"]),
    );
    let projects = value["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1, "{value}");
    let changes = &projects[0]["changes"];
    for (change_id, bucket) in &expected {
        let rows = changes[*bucket].as_array().unwrap();
        assert!(
            rows.iter()
                .any(|row| row["change_id"] == change_id.as_str()),
            "{change_id} is not in {bucket}: {value}"
        );
    }
    let row = changes["unclassified"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["change_id"] == unclassified.as_str())
        .unwrap();
    assert_eq!(row["reason"], "no-valid-approval", "{value}");
}

/// `--under` and `--here` pick the same projects for the workspace inbox and
/// the backlog.
#[test]
fn workspace_inbox_and_backlog_select_the_same_projects() {
    let outer = TempDir::new().unwrap();
    let shared_home = outer.path().join("home");
    fs::create_dir_all(&shared_home).unwrap();
    let shared = |repo: &Repo| {
        let mut cmd = repo.arc(&repo.root);
        cmd.env("HOME", &shared_home)
            .env("ARC_SANDBOX", &shared_home);
        cmd
    };

    let first = Repo::new();
    shared(&first)
        .args(["begin", "scope-first", "--no-worktree"])
        .assert()
        .success();
    let second = Repo::new();
    shared(&second)
        .args(["begin", "scope-second", "--no-worktree"])
        .assert()
        .success();

    let mut global = shared(&first);
    global.args(["workspace", "inbox", "--global", "--json"]);
    let inbox = json_stdout(&mut global);
    let mut global = shared(&first);
    global.args(["workspace", "backlog", "--global", "--json"]);
    let backlog = json_stdout(&mut global);
    assert_eq!(
        backlog["projects"].as_array().unwrap().len(),
        2,
        "{backlog}"
    );
    assert_eq!(inbox["repos"].as_array().unwrap().len(), 2, "{inbox}");

    let under = fs::canonicalize(&first.root)
        .unwrap()
        .parent()
        .unwrap()
        .display()
        .to_string();
    let mut scoped = shared(&first);
    scoped.args(["workspace", "inbox", "--under", &under, "--json"]);
    let inbox = json_stdout(&mut scoped);
    let mut scoped = shared(&first);
    scoped.args(["workspace", "backlog", "--under", &under, "--json"]);
    let backlog = json_stdout(&mut scoped);
    assert_eq!(
        backlog["projects"].as_array().unwrap().len(),
        1,
        "{backlog}"
    );
    assert_eq!(inbox["repos"].as_array().unwrap().len(), 1, "{inbox}");
    assert_eq!(
        inbox["repos"][0]["needs-review"].as_array().unwrap().len(),
        1,
        "{inbox}"
    );
    assert!(
        inbox["repos"][0]["needs-review"][0]["change_id"]
            .as_str()
            .unwrap()
            .starts_with("scope-first"),
        "the selected project is the first one: {inbox}"
    );

    let mut here = shared(&first);
    here.args(["workspace", "inbox", "--here", "--json"]);
    let inbox = json_stdout(&mut here);
    let mut here = shared(&first);
    here.args(["workspace", "backlog", "--here", "--json"]);
    let backlog = json_stdout(&mut here);
    assert_eq!(
        backlog["projects"].as_array().unwrap().len(),
        1,
        "{backlog}"
    );
    assert_eq!(inbox["repos"].as_array().unwrap().len(), 1, "{inbox}");
    assert!(
        backlog["projects"][0]["anchor"]
            .as_str()
            .unwrap()
            .starts_with(&under),
        "{backlog}"
    );
}

/// Fork inventory and observation boundaries: an active fork appears with its
/// metadata and adds no decision weight, retiring it removes it, and the JSON
/// report carries ordered observation timestamps with the sequential
/// consistency it actually has. The report writes no repository or journal
/// files.
#[test]
fn workspace_backlog_inventories_forks_without_obligation() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "registered", "the project exists"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["fork", "begin", "demo"])
        .assert()
        .success();

    // Fork-only project: the fork is visible even with nothing outstanding.
    let value = {
        let mut report = repo.arc(&repo.root);
        report.args(["workspace", "backlog", "--items", "--json"]);
        json_stdout(&mut report)
    };
    let project = value["projects"].as_array().unwrap().first().unwrap();
    let forks = project["forks"].as_array().unwrap();
    assert_eq!(forks.len(), 1, "{}", project);
    assert_eq!(forks[0]["slug"], "demo");
    assert_eq!(forks[0]["branch"], "fork/demo");
    assert_eq!(project["fork_count"], 1);
    assert_eq!(value["summary"]["fork_count"], 1);
    // Forks add nothing to the blocked/decision score.
    assert_eq!(project["decision_questions"], 0);
    assert_eq!(project["needs_review"].as_array().unwrap().len(), 0);
    assert_eq!(project["debt_owed"].as_array().unwrap().len(), 0);

    // The journal marker is filtered out by a future cutoff, but the active
    // branch inventory remains an independently observed orientation fact.
    let mut report = repo.arc(&repo.root);
    report.args([
        "workspace",
        "backlog",
        "--items",
        "--json",
        "--since",
        "2099-01-01T00:00:00Z",
    ]);
    let value = json_stdout(&mut report);
    let project = value["projects"].as_array().unwrap().first().unwrap();
    assert_eq!(project["open_items"], 0, "{}", project);
    assert_eq!(project["fork_count"], 1, "{}", project);
    assert_eq!(value["summary"]["fork_count"], 1, "{}", value);

    // Observation bounds are present, ordered, and sequential.
    let started = value["observation"]["started_at"].as_str().unwrap();
    let finished = value["observation"]["finished_at"].as_str().unwrap();
    assert!(started <= finished, "{started} > {finished}");
    assert_eq!(value["observation"]["consistency"], "sequential");

    // Retiring the fork removes it from the inventory.
    repo.arc(&repo.root)
        .args(["fork", "retire", "demo", "dropped: not wanted"])
        .assert()
        .success();
    let value = {
        let mut report = repo.arc(&repo.root);
        report.args(["workspace", "backlog", "--json"]);
        json_stdout(&mut report)
    };
    assert_eq!(value["summary"]["fork_count"], 0, "{value}");
    assert!(value["projects"].as_array().unwrap().is_empty(), "{value}");
}

/// A fork branch made without a marker is still part of the read-only fork
/// inventory, and its project must remain visible when it has no other work.
#[test]
fn workspace_backlog_keeps_unjournaled_forks_visible() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "registered", "the project exists"])
        .assert()
        .success();
    git(&repo.root, &["branch", "fork/manual"]);

    let value =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "backlog", "--items", "--json"]),
        );
    let project = value["projects"].as_array().unwrap().first().unwrap();
    assert_eq!(project["fork_count"], 1, "{}", project);
    assert_eq!(project["forks"][0]["slug"], "manual", "{}", project);
    assert_eq!(project["forks"][0]["branch"], "fork/manual");
    assert_eq!(value["summary"]["fork_count"], 1, "{}", value);
}

/// A failed branch inventory is an unreadable observation, not an empty fork
/// list. The workspace command carries the project context to its caller.
#[test]
fn workspace_backlog_retains_a_fork_inventory_failure() {
    use std::os::unix::fs::PermissionsExt;

    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "registered", "the project exists"])
        .assert()
        .success();
    git(&repo.root, &["branch", "fork/manual"]);

    let real_git = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git must be on PATH for this test");
    let shim_dir = repo.home.join("git-shim");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("git");
    fs::write(
        &shim,
        "#!/bin/sh\n\
if [ \"$ARC_FAIL_GIT_DISCOVERY\" = 1 ] && [ \"$1\" = rev-parse ] && [ \"$2\" = --git-common-dir ]; then\n\
  echo simulated-git-discovery-failure >&2\n\
  exit 42\n\
fi\n\
if [ \"$1\" = branch ] && [ \"$2\" = --list ] && [ \"$3\" = \"--format=%(refname:short)\" ] && [ \"$4\" = \"fork/*\" ]; then\n\
  echo simulated-fork-inventory-failure >&2\n\
  exit 42\n\
fi\n\
exec \"$ARC_REAL_GIT\" \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", shim_dir.display(), std::env::var("PATH").unwrap());

    repo.arc(&repo.root)
        .env("PATH", &path)
        .env("ARC_REAL_GIT", &real_git)
        .args(["fork", "list", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "simulated-fork-inventory-failure",
        ));
    // A failed fork inventory no longer aborts the collection: the project
    // keeps the facts that were read, the failure is named, and the exit code
    // says the report is partial.
    let assert = repo
        .arc(&repo.root)
        .env("PATH", &path)
        .env("ARC_REAL_GIT", &real_git)
        .args(["workspace", "backlog", "--json"])
        .assert()
        .code(16);
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(value["collection"]["failed"], 1, "{value}");
    let failure = value["collection"]["failures"]
        .as_array()
        .unwrap()
        .first()
        .unwrap();
    assert_eq!(failure["component"], "forks", "{value}");
    assert!(
        failure["reason"]
            .as_str()
            .unwrap()
            .contains("cannot inventory forks for"),
        "{value}"
    );
    assert!(
        failure["reason"]
            .as_str()
            .unwrap()
            .contains("simulated-fork-inventory-failure"),
        "{value}"
    );
    let assert = repo
        .arc(&repo.root)
        .env("PATH", &path)
        .env("ARC_REAL_GIT", &real_git)
        .env("ARC_FAIL_GIT_DISCOVERY", "1")
        .args(["workspace", "backlog", "--json"])
        .assert()
        .code(16);
    let value: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let failure = value["collection"]["failures"]
        .as_array()
        .unwrap()
        .first()
        .unwrap();
    assert_eq!(failure["component"], "forks", "{value}");
    assert!(
        failure["reason"]
            .as_str()
            .unwrap()
            .contains("simulated-git-discovery-failure"),
        "{value}"
    );
}

/// Opening and closing question subtotals describe the active decision set;
/// a consumed question remains visible as history but is not a subtotal.
#[test]
fn workspace_backlog_counts_active_question_subtotals() {
    let repo = Repo::new();
    let (_, active_file) = journal_artifact(
        &repo,
        "active-question",
        "discussion",
        "# Active question\n\nBody.\n",
    );
    repo.arc(&repo.root)
        .args([
            "journal",
            "question",
            &active_file,
            "--placement",
            "opening",
            "--option",
            "yes",
            "--option",
            "no",
            "--body-file",
            "-",
        ])
        .write_stdin("Which opening should remain active?\n")
        .assert()
        .success();

    let (_, consumed_file) = journal_artifact(
        &repo,
        "consumed-question",
        "discussion",
        "# Consumed question\n\nBody.\n",
    );
    repo.arc(&repo.root)
        .args([
            "journal",
            "question",
            &consumed_file,
            "--placement",
            "closing",
            "--option",
            "yes",
            "--option",
            "no",
            "--body-file",
            "-",
        ])
        .write_stdin("Which closing answer should be recorded?\n")
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "journal",
            "consume",
            &consumed_file,
            "--outcome",
            "done",
            "--drop-questions",
            "--note",
            "the closing record is retained as history",
        ])
        .assert()
        .success();

    let value =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "backlog", "--items", "--json"]),
        );
    let project = value["projects"].as_array().unwrap().first().unwrap();
    assert_eq!(project["decision_questions"], 1, "{}", project);
    assert_eq!(project["opening_question_count"], 1, "{}", project);
    assert_eq!(project["closing_question_count"], 0, "{}", project);
    assert_eq!(
        project["opening_question_count"].as_u64().unwrap()
            + project["closing_question_count"].as_u64().unwrap(),
        project["decision_questions"].as_u64().unwrap(),
        "active question subtotals do not add up: {project}"
    );
}

/// A report writes nothing: the repository's tracked tree, its ledger, and
/// the journal are byte-identical across a backlog run.
#[test]
fn workspace_backlog_writes_nothing() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journal", "log", "registered", "the project exists"])
        .assert()
        .success();
    // The journal write above creates the journal; the ledger root comes
    // with `journal log` registering the project.
    let journal = journal_dir_of(&repo);
    assert!(journal.is_dir(), "journal dir should exist after log");
    let snapshot_dir = |dir: &Path| -> Vec<(String, [u8; 32])> {
        let mut entries: Vec<(String, [u8; 32])> = match fs::read_dir(dir) {
            Ok(entries) => entries,
            // A root that does not exist yet is part of the before-state.
            Err(_) if !dir.exists() => return Vec::new(),
            Err(error) => panic!("cannot read {}: {error}", dir.display()),
        }
        .map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                (format!("{:?}", path), [0u8; 32])
            } else {
                let bytes = fs::read(&path).unwrap();
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&Sha256::digest(&bytes));
                (path.display().to_string(), hash)
            }
        })
        .collect();
        entries.sort();
        entries
    };
    let ledger_root = repo.root.join(".git/arc");
    let before = snapshot_dir(&journal);
    let before_git = snapshot_dir(&ledger_root);
    let before_status = git_out(&repo.root, &["status", "--porcelain"]);

    repo.arc(&repo.root)
        .args(["workspace", "backlog", "--items", "--json"])
        .assert()
        .success();

    assert_eq!(snapshot_dir(&journal), before, "journal changed");
    let after_git = snapshot_dir(&ledger_root);
    assert_eq!(after_git.len(), before_git.len(), "ledger changed");
    for ((before_path, before_hash), (after_path, after_hash)) in
        before_git.iter().zip(after_git.iter())
    {
        assert_eq!(before_path, after_path, "ledger changed");
        assert_eq!(before_hash, after_hash, "ledger changed: {before_path}");
    }
    assert_eq!(
        git_out(&repo.root, &["status", "--porcelain"]),
        before_status,
        "tracked tree changed"
    );
}
