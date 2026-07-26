use crate::common::*;

fn repo_with_trivial_gates() -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.build]\ncommand = \"true\"\n[gates.test]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "gates"]);
    repo
}

#[test]
fn skip_green_skips_only_at_matching_head_and_reruns_after_a_commit() {
    let repo = repo_with_trivial_gates();
    let (_id, wt, _head) = change_with_patchset(&repo, "feat-x");

    // Nothing is green yet: both gates run.
    let first = stdout(
        repo.arc(&wt)
            .args(["verify", "feat-x", "--all", "--skip-green"]),
    );
    assert!(first.contains("gates: 2/2 pass"), "{first}");
    assert!(!first.contains("skipped"), "{first}");

    // Re-run at the same head: both are green and skipped.
    let second = stdout(
        repo.arc(&wt)
            .args(["verify", "feat-x", "--all", "--skip-green"]),
    );
    assert!(
        second.contains("build: skipped (green at head)"),
        "{second}"
    );
    assert!(second.contains("test: skipped (green at head)"), "{second}");
    assert!(second.contains("gates: 2/2 pass"), "{second}");

    // A new commit moves the head, so the gates run again.
    repo.commit(&wt, "feat-x.txt", "more\n", "feat: more");
    stdout(repo.arc(&wt).args(["snapshot", "feat-x"]));
    let third = stdout(
        repo.arc(&wt)
            .args(["verify", "feat-x", "--all", "--skip-green"]),
    );
    assert!(
        !third.contains("skipped"),
        "should rerun after commit:\n{third}"
    );
    assert!(third.contains("gates: 2/2 pass"), "{third}");
}

#[test]
fn verification_run_records_manifest_results_and_reused_evidence() {
    let repo = repo_with_trivial_gates();
    let (_id, worktree, head) = change_with_patchset(&repo, "run-identity");

    repo.arc(&worktree)
        .args(["verify", "run-identity", "--all"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["verify", "run-identity", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("build: skipped (green at head)"))
        .stdout(predicates::str::contains("test: skipped (green at head)"));

    let events = stdout(
        repo.arc(&worktree)
            .args(["events", "--change", "run-identity"]),
    );
    let events = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let manifests = events
        .iter()
        .filter(|event| event["event_type"] == "verification-run-started")
        .collect::<Vec<_>>();
    assert_eq!(manifests.len(), 2, "{events:#?}");
    let first_run = manifests[0]["event_id"].as_str().unwrap();
    let second_run = manifests[1]["event_id"].as_str().unwrap();
    assert_eq!(manifests[0]["revision"], head);
    assert_eq!(manifests[0]["mode"], "sequential");
    assert_eq!(manifests[0]["skip_green"], false);
    assert_eq!(manifests[1]["skip_green"], true);
    assert_eq!(
        manifests[1]["gates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|gate| gate["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["build", "test"]
    );

    let observations = events
        .iter()
        .filter(|event| event["event_type"] == "verification-recorded")
        .collect::<Vec<_>>();
    assert_eq!(observations.len(), 2);
    assert!(observations
        .iter()
        .all(|event| event["run_id"] == first_run));
    let original_ids = observations
        .iter()
        .map(|event| event["event_id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    let reused = events
        .iter()
        .filter(|event| event["event_type"] == "verification-reused")
        .collect::<Vec<_>>();
    assert_eq!(reused.len(), 2);
    assert!(reused.iter().all(|event| event["run_id"] == second_run));
    assert!(reused.iter().all(|event| event["revision"] == head
        && original_ids.contains(event["evidence_event_id"].as_str().unwrap())));
    assert!(!events
        .iter()
        .any(|event| event["event_type"] == "verification-run-completed"));

    let show = json_stdout(repo.arc(&worktree).args(["show", "run-identity", "--json"]));
    let runs = show["verification_runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| run["complete"] == true));
    assert!(runs
        .iter()
        .all(|run| run["missing_gates"].as_array().unwrap().is_empty()));
    assert_eq!(runs[1]["terminals"].as_array().unwrap().len(), 2);
    let human = stdout(repo.arc(&worktree).args(["show", "run-identity"]));
    assert!(human.contains(&format!("Verification run `{second_run}` — complete")));
    assert!(human.contains("reused"));
}

#[test]
fn skip_green_requires_all() {
    let repo = repo_with_trivial_gates();
    change_with_patchset(&repo, "feat-x");
    repo.arc(&repo.root)
        .args(["verify", "feat-x", "--gate", "build", "--skip-green"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--skip-green requires --all"));
}
