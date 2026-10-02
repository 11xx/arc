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
        second.contains("build: skipped (green at head; declared by .arc/gates.toml)"),
        "{second}"
    );
    assert!(
        second.contains("test: skipped (green at head; declared by .arc/gates.toml)"),
        "{second}"
    );
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

/// The `verification-reused` events of one change, as `(revision, tree,
/// evidence_event_id)`, beside the revision each reused evidence names.
fn reuses(repo: &Repo, wt: &Path, change: &str) -> Vec<(String, String, String)> {
    let events: Vec<serde_json::Value> = stdout(repo.arc(wt).args(["events", "--change", change]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    events
        .iter()
        .filter(|event| event["event_type"] == "verification-reused")
        .map(|reuse| {
            let evidence = events
                .iter()
                .find(|event| event["event_id"] == reuse["evidence_event_id"])
                .unwrap();
            (
                reuse["revision"].as_str().unwrap().to_string(),
                reuse["tree"].as_str().unwrap().to_string(),
                evidence["revision"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn skip_green_reuses_evidence_from_another_commit_holding_the_head_tree() {
    let repo = repo_with_trivial_gates();
    let (id, wt, verified) = change_with_patchset(&repo, "reworded");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();

    // Rewording the commit, as re-signing it does, makes a new commit around
    // the tree the gates already answered for.
    git(&wt, &["commit", "--amend", "-m", "test: reworded"]);
    let head = repo.head(&wt);
    assert_ne!(head, verified);
    repo.arc(&wt).args(["snapshot"]).assert().success();
    let status = json_stdout(repo.arc(&wt).args(["status"]));
    assert!(
        status["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|gate| gate["green_at_head"] == true && gate["inherited_from"] == verified),
        "{status}"
    );

    let rerun = stdout(repo.arc(&wt).args(["verify", "--all", "--skip-green"]));
    assert!(
        rerun.contains("build: skipped (green at head; declared by .arc/gates.toml)"),
        "{rerun}"
    );
    assert!(
        rerun.contains("test: skipped (green at head; declared by .arc/gates.toml)"),
        "{rerun}"
    );
    assert!(rerun.contains("gates: 2/2 pass"), "{rerun}");
    let tree = git_out(&wt, &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(
        reuses(&repo, &wt, &id),
        vec![(head.clone(), tree.clone(), verified.clone()); 2]
    );
    repo.arc(&wt).args(["show", "--json"]).assert().success();
    let check = json_stdout_any_status(repo.arc(&wt).args(["check", "--json"]));
    assert!(
        !check["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["blocker"] == "gates-not-green"),
        "{check}"
    );
}

#[test]
fn skip_green_after_a_rebase_reuses_the_evidence_for_the_merged_tree() {
    let repo = repo_with_trivial_gates();
    let (id, wt, _) = change_with_patchset(&repo, "restacked");
    repo.commit(&repo.root, "sibling.txt", "sibling\n", "test: sibling");
    repo.arc(&wt)
        .args(["verify", "--against", "master"])
        .assert()
        .success();
    let merged_tree = json_stdout(repo.arc(&wt).args(["status"]))["merged_tree"]
        .as_str()
        .unwrap()
        .to_string();

    // Moving the base and nothing else lands the head on the merged tree the
    // gates ran against.
    git(&wt, &["rebase", "master"]);
    let head = repo.head(&wt);
    assert_eq!(git_out(&wt, &["rev-parse", "HEAD^{tree}"]), merged_tree);
    repo.arc(&wt).args(["snapshot"]).assert().success();

    let rerun = stdout(repo.arc(&wt).args(["verify", "--all", "--skip-green"]));
    assert!(
        rerun.contains("build: skipped (green at head; declared by .arc/gates.toml)"),
        "{rerun}"
    );
    assert!(
        rerun.contains("test: skipped (green at head; declared by .arc/gates.toml)"),
        "{rerun}"
    );
    let reused = reuses(&repo, &wt, &id);
    assert_eq!(reused.len(), 2, "{reused:?}");
    assert!(
        reused
            .iter()
            .all(|(revision, tree, evidence)| *revision == head
                && *tree == merged_tree
                && *evidence != head),
        "{reused:?}"
    );
    repo.arc(&wt).args(["show", "--json"]).assert().success();
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
        .stdout(predicates::str::contains(
            "build: skipped (green at head; declared by .arc/gates.toml)",
        ))
        .stdout(predicates::str::contains(
            "test: skipped (green at head; declared by .arc/gates.toml)",
        ));

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
fn skip_green_requires_known_clean_local_provenance_but_allows_attested() {
    let repo = repo_with_trivial_gates();
    let (legacy_id, legacy_worktree, _) = change_with_patchset(&repo, "legacy-provenance");
    repo.arc(&legacy_worktree)
        .args(["verify", "legacy-provenance", "--gate", "build"])
        .assert()
        .success();
    rewrite_event(&repo, &legacy_id, "verification-recorded", |event| {
        let object = event.as_object_mut().unwrap();
        object.remove("tested_tree");
        object.remove("worktree_dirty");
        object.remove("tree_moved");
    });
    let rerun = stdout(repo.arc(&legacy_worktree).args([
        "verify",
        "legacy-provenance",
        "--all",
        "--skip-green",
    ]));
    assert!(!rerun.contains("build: skipped"), "{rerun}");

    let (attested_id, attested_worktree, head) = change_with_patchset(&repo, "attested-provenance");
    repo.arc(&attested_worktree)
        .args([
            "verify",
            &attested_id,
            "--gate",
            "build",
            "--attest",
            "--result",
            "pass",
            "--tested-revision",
            &head,
            "--execution-host",
            "sandbox",
            "--runner",
            "test-runner",
        ])
        .assert()
        .success();
    let attested_rerun = stdout(repo.arc(&attested_worktree).args([
        "verify",
        &attested_id,
        "--all",
        "--skip-green",
    ]));
    assert!(
        attested_rerun.contains("build: skipped (green at head; declared by .arc/gates.toml)"),
        "{attested_rerun}"
    );
}

#[test]
fn skip_green_reruns_when_the_tested_tree_cannot_be_retained() {
    let repo = repo_with_trivial_gates();
    let (change_id, worktree, _) = change_with_patchset(&repo, "retention-conflict");

    // A ref at the parent path makes every per-event tree pin fail without
    // affecting the gate command itself.
    git(
        &repo.root,
        &[
            "update-ref",
            &format!("refs/arc/tree/{change_id}"),
            &git_out(&worktree, &["rev-parse", "HEAD"]),
        ],
    );
    repo.arc(&worktree)
        .args(["verify", "retention-conflict", "--gate", "build"])
        .assert()
        .success()
        .stderr(predicates::str::contains("without local provenance"));

    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "retention-conflict", "--json"]),
    );
    let gate = &status["gates"][0];
    assert_eq!(gate["result"], "pass", "{status}");
    assert_eq!(gate["green_at_head"], false, "{status}");
    assert_eq!(gate["tested_tree"], serde_json::Value::Null, "{status}");
    assert_eq!(gate["worktree_dirty"], serde_json::Value::Null, "{status}");

    let rerun =
        stdout(
            repo.arc(&worktree)
                .args(["verify", "retention-conflict", "--all", "--skip-green"]),
        );
    assert!(!rerun.contains("build: skipped"), "{rerun}");
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

#[test]
fn reuse_at_target_tip_replays_at_the_synthesized_revision() {
    let repo = repo_with_trivial_gates();
    let begun = stdout(repo.arc(&repo.root).args(["begin", "target-tip"]));
    let id = opened_change_id(&begun);
    let wt = repo.home.join(".worktrees/repo-target-tip");
    let head = repo.head(&wt);
    assert_eq!(head, repo.head(&repo.root));
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["verify", "--against", "master", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gates: 2/2 pass"))
        .stdout(predicates::str::contains("green at the merged tree"));
    let events = stdout(repo.arc(&wt).args(["events", "--change", &id]));
    let events: Vec<serde_json::Value> = events
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let tree = git_out(&wt, &["rev-parse", "HEAD^{tree}"]);
    let reused: Vec<_> = events
        .iter()
        .filter(|event| event["event_type"] == "verification-reused")
        .collect();
    assert_eq!(reused.len(), 2);
    for reuse in reused {
        assert_ne!(reuse["revision"], head);
        assert_eq!(reuse["tree"], tree);
        let evidence = events
            .iter()
            .find(|event| event["event_id"] == reuse["evidence_event_id"])
            .unwrap();
        assert_eq!(evidence["revision"], head);
    }
    for args in [
        vec!["check"],
        vec!["query"],
        vec!["status"],
        vec!["catchup", "--json"],
    ] {
        repo.arc(&wt).args(args).assert().success();
    }
}

#[test]
fn old_shape_reuse_keeps_revision_validation() {
    let repo = repo_with_trivial_gates();
    let (id, wt, _) = change_with_patchset(&repo, "old-reuse");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    repo.arc(&wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success();
    rewrite_event(&repo, &id, "verification-reused", |event| {
        event.as_object_mut().unwrap().remove("tree");
    });
    repo.arc(&wt).args(["show", "--json"]).assert().success();
    rewrite_event(&repo, &id, "verification-reused", |event| {
        event["revision"] = "another-revision".into();
    });
    repo.arc(&wt)
        .args(["show", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("does not match passing"));
}

#[test]
fn skip_green_reruns_evidence_without_a_replayable_tree_key() {
    let repo = repo_with_trivial_gates();
    let (id, wt, head) = change_with_patchset(&repo, "missing-tree");
    repo.arc(&wt)
        .args([
            "verify",
            "--gate",
            "build",
            "--attest",
            "--result",
            "pass",
            "--tested-revision",
            &head,
            "--execution-host",
            "fixture",
            "--runner",
            "fixture",
        ])
        .assert()
        .success();
    rewrite_event(&repo, &id, "verification-recorded", |event| {
        event.as_object_mut().unwrap().remove("tree");
        event.as_object_mut().unwrap().remove("tested_tree");
    });
    repo.arc(&wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("build: skipped").not());
    repo.arc(&wt).args(["show", "--json"]).assert().success();
}

/// Attest a failing `build` at `revision`, then strip the tree fields from
/// it, the shape evidence had before arc recorded the tree it describes.
fn record_revision_keyed_build_failure(repo: &Repo, wt: &Path, change: &str, revision: &str) {
    repo.arc(wt)
        .args([
            "verify",
            change,
            "--gate",
            "build",
            "--attest",
            "--result",
            "fail",
            "--tested-revision",
            revision,
            "--execution-host",
            "fixture",
            "--runner",
            "fixture",
        ])
        .assert()
        .code(1);
    rewrite_event(repo, change, "verification-recorded", |event| {
        let object = event.as_object_mut().unwrap();
        object.remove("tree");
        object.remove("tested_tree");
    });
}

#[test]
fn skip_green_reruns_when_newer_revision_keyed_evidence_failed_at_the_tree() {
    let repo = repo_with_trivial_gates();
    let (id, wt, passed) = change_with_patchset(&repo, "newer-failure");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    git(&wt, &["commit", "--amend", "-m", "test: reworded"]);
    let head = repo.head(&wt);
    assert_ne!(head, passed);
    repo.arc(&wt).args(["snapshot"]).assert().success();
    record_revision_keyed_build_failure(&repo, &wt, &id, &head);

    let status = json_stdout_any_status(repo.arc(&wt).args(["status", "--json"]));
    let build = status["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|gate| gate["name"] == "build")
        .unwrap();
    assert_eq!(build["result"], "fail", "{status}");

    let rerun = stdout(repo.arc(&wt).args(["verify", "--all", "--skip-green"]));
    assert!(!rerun.contains("build: skipped"), "{rerun}");
    assert!(
        rerun.contains("test: skipped (green at head; declared by .arc/gates.toml)"),
        "{rerun}"
    );
    assert!(rerun.contains("gates: 2/2 pass"), "{rerun}");
    let tree = git_out(&wt, &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(reuses(&repo, &wt, &id), vec![(head, tree, passed)]);
}

#[test]
fn skip_green_against_reruns_when_newer_revision_keyed_evidence_failed_at_the_tree() {
    let repo = repo_with_trivial_gates();
    let begun = stdout(repo.arc(&repo.root).args(["begin", "tip-failure"]));
    let id = opened_change_id(&begun);
    let wt = repo.home.join(".worktrees/repo-tip-failure");
    let head = repo.head(&wt);
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    record_revision_keyed_build_failure(&repo, &wt, &id, &head);

    let rerun = stdout(
        repo.arc(&wt)
            .args(["verify", "--against", "master", "--skip-green"]),
    );
    assert!(!rerun.contains("build: skipped"), "{rerun}");
    assert!(
        rerun.contains("test: skipped (green at the merged tree"),
        "{rerun}"
    );
    assert!(rerun.contains("gates: 2/2 pass"), "{rerun}");
}
