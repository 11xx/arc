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

#[test]
fn workspace_requires_configured_data_root() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["workspace", "list"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("requires a configured data_root"));
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
    assert!(brief.contains("arc verify --command"), "{brief}");
    assert!(
        brief.contains("never edit a probe to make it pass"),
        "{brief}"
    );
}

#[test]
fn restack_advise_prints_rebase_for_dependent_and_writes_nothing() {
    let repo = Repo::new();
    let base = begin_change(&repo, "base-change", None);
    let dependent = begin_change(&repo, "dependent", Some("base-change"));

    // Integrate the blocker (recorded as a close at a revision).
    repo.arc(&repo.root)
        .args(["close", "base-change", "--integrated", "HEAD"])
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
