use crate::common::*;

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
    assert_eq!(value["schema"], "arc-workspace-backlog/1");
    let project = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .unwrap_or_else(|| panic!("project missing: {value}"));
    assert_eq!(project["open_items"], 1);
    // A change with no verdict yet is waiting on a reviewer.
    assert!(
        project["needs_review"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id.as_str().unwrap().starts_with("feat-pending")),
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
    let stranded = value["unreachable"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["slug"] == "-gone-away-project")
        .unwrap_or_else(|| panic!("orphan not reported: {value}"));
    assert_eq!(stranded["anchor"], "/gone/away/project");
    assert_eq!(stranded["reason"], "anchor does not exist");
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
