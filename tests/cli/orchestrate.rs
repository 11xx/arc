use super::common::*;

fn replace_closure_successor(repo: &Repo, change_id: &str, successor: &str) {
    let events = repo
        .root
        .join(".git/arc/changes")
        .join(change_id)
        .join("events");
    for entry in fs::read_dir(events).unwrap() {
        let path = entry.unwrap().path();
        let mut event: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] == "change-closed" {
            event["outcome"] = serde_json::json!("superseded");
            event["superseded_by"] = serde_json::json!(successor);
            event.as_object_mut().unwrap().remove("integrated_commit");
            fs::write(path, json_file_bytes(&event)).unwrap();
            return;
        }
    }
    panic!("expected a change-closed event for {change_id}");
}

/// A dependency chain blocks until each prerequisite integrates. Status
/// suggests only other open changes whose own prerequisites are satisfied.
#[test]
fn blocker_chain_transitions_and_suggests_ready_alternatives() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "chain-a"]));
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "chain-b", "--blocked-by", "chain-a"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "chain-c", "--blocked-by", "chain-b"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "chain-d", "--blocked-by", "chain-c"]),
    );
    stdout(repo.arc(&repo.root).args(["begin", "chain-held"]));
    repo.arc(&repo.root)
        .args(["hold", "chain-held", "--reason", "do not start"])
        .assert()
        .success();

    let b_status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "chain-b", "--json"]),
    ))
    .unwrap();
    assert_eq!(b_status["blocker_status"]["blocked"], true);
    assert_eq!(
        b_status["suggested_alternatives"].as_array().unwrap().len(),
        1
    );
    assert_eq!(b_status["suggested_alternatives"][0]["slug"], "chain-a");
    let blocker_status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["blocker-status", "chain-b"]),
    ))
    .unwrap();
    assert_eq!(blocker_status["schema"], "arc-blocker-status/1");
    assert_eq!(blocker_status["blocked"], true);
    assert_eq!(blocker_status["blockers_ready"][0]["slug"], "chain-a");
    repo.arc(&repo.root)
        .args(["is-blocked", "chain-b"])
        .assert()
        .code(1);
    repo.arc(&repo.root)
        .args(["is-blocked", "does-not-exist"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("no change matches"));
    repo.arc(&repo.root)
        .args(["check", "chain-b"])
        .assert()
        .code(7);

    complete_change(&repo, "chain-a");

    let b_status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "chain-b"]))).unwrap();
    assert_eq!(b_status["blocker_status"]["blocked"], false);
    assert_eq!(b_status["suggested_alternatives"], serde_json::json!([]));
    repo.arc(&repo.root)
        .args(["is-blocked", "chain-b"])
        .assert()
        .success();

    let c_status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "chain-c"]))).unwrap();
    assert_eq!(c_status["blocker_status"]["blocked"], true);
    assert_eq!(
        c_status["suggested_alternatives"].as_array().unwrap().len(),
        1
    );
    assert_eq!(c_status["suggested_alternatives"][0]["slug"], "chain-b");

    complete_change(&repo, "chain-b");
    let c_status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "chain-c"]))).unwrap();
    assert_eq!(c_status["blocker_status"]["blocked"], false);
}

#[test]
fn superseded_prerequisites_resolve_after_successor_integration() {
    let repo = Repo::new();
    let prerequisite = begin_change(&repo, "superseded-a", None);
    let successor = begin_change(&repo, "superseded-a2", None);
    let dependent = begin_change(&repo, "superseded-dependent", Some(&prerequisite));

    repo.arc(&repo.root)
        .args(["close", &prerequisite, "--superseded", &successor])
        .assert()
        .success();
    let wedged: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["blocker-status", &dependent]),
    ))
    .unwrap();
    assert_eq!(wedged["blocked"], true);
    assert_eq!(wedged["blockers_ready"][0]["status"], "wedged");

    stdout(repo.arc(&repo.root).args(["snapshot", &successor]));
    repo.arc(&repo.root)
        .args(["close", &successor, "--assert-integrated", "HEAD"])
        .assert()
        .success();
    let ready: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", &dependent]))).unwrap();
    assert_eq!(ready["blocker_status"]["blocked"], false);
    assert_eq!(
        ready["blocker_status"]["blockers_ready"][0]["status"],
        "superseded-integrated"
    );
    assert_eq!(
        ready["blocker_status"]["blockers_ready"][0]["integrated"],
        true
    );
    repo.arc(&repo.root)
        .args(["is-blocked", &dependent])
        .assert()
        .success();

    let first = begin_change(&repo, "transitive-a", None);
    let second = begin_change(&repo, "transitive-a2", None);
    let third = begin_change(&repo, "transitive-a3", None);
    let transitive_dependent = begin_change(&repo, "transitive-dependent", Some(&first));
    repo.arc(&repo.root)
        .args(["close", &first, "--superseded", &second])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["close", &second, "--superseded", &third])
        .assert()
        .success();
    stdout(repo.arc(&repo.root).args(["snapshot", &third]));
    repo.arc(&repo.root)
        .args(["close", &third, "--assert-integrated", "HEAD"])
        .assert()
        .success();
    let transitive: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["blocker-status", &transitive_dependent]),
    ))
    .unwrap();
    assert_eq!(transitive["blocked"], false);
    assert_eq!(
        transitive["blockers_ready"][0]["status"],
        "superseded-integrated"
    );
    assert_eq!(transitive["blockers_ready"][0]["integrated"], true);
}

#[test]
fn wedged_prerequisites_report_recovery_and_stay_blocked() {
    let repo = Repo::new();
    let abandoned = begin_change(&repo, "abandoned-a", None);
    let dependent = begin_change(&repo, "abandoned-dependent", Some(&abandoned));
    repo.arc(&repo.root)
        .args(["close", &abandoned, "--abandoned"])
        .assert()
        .success();

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", &dependent]))).unwrap();
    let dependency = &status["blocker_status"]["blockers_ready"][0];
    assert_eq!(status["schema"], "arc-status/9");
    assert_eq!(status["blocker_status"]["blocked"], true);
    assert_eq!(status["next_action"], "repair_blockers:metadata");
    assert_eq!(dependency["status"], "wedged");
    assert_eq!(
        dependency["recovery"],
        "prerequisite closed without integration: clear or retarget with arc metadata"
    );
    repo.arc(&repo.root)
        .args(["is-blocked", &dependent])
        .assert()
        .code(1)
        .stdout(predicates::str::contains(format!(
            "blocked by {abandoned} (wedged)"
        )));
    repo.arc(&repo.root)
        .args(["check", &dependent])
        .assert()
        .code(7)
        .stdout(predicates::str::contains(
            "prerequisite closed without integration: clear or retarget with arc metadata",
        ));

    let raw_missing = begin_change(&repo, "raw-missing-a", None);
    let raw_dependent = begin_change(&repo, "raw-missing-dependent", Some(&raw_missing));
    repo.arc(&repo.root)
        .args(["close", &raw_missing, "--abandoned"])
        .assert()
        .success();
    replace_closure_successor(&repo, &raw_missing, "missing-successor");
    let missing: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["blocker-status", &raw_dependent]),
    ))
    .unwrap();
    assert_eq!(missing["blocked"], true);
    assert_eq!(missing["blockers_ready"][0]["status"], "wedged");

    let first = begin_change(&repo, "cycle-a", None);
    let second = begin_change(&repo, "cycle-a2", None);
    let cycle_dependent = begin_change(&repo, "cycle-dependent", Some(&first));
    repo.arc(&repo.root)
        .args(["close", &first, "--superseded", &second])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["close", &second, "--superseded", &first])
        .assert()
        .success();
    let cycle: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["blocker-status", &cycle_dependent]),
    ))
    .unwrap();
    assert_eq!(cycle["blocked"], true);
    assert_eq!(cycle["blockers_ready"][0]["status"], "wedged");
}

#[test]
fn imported_change_can_remove_missing_blocker() {
    let source = Repo::new();
    let blocker_out = stdout(source.arc(&source.root).args(["begin", "remote-blocker"]));
    let blocker_id = blocker_out
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string();
    stdout(source.arc(&source.root).args([
        "begin",
        "dependent-change",
        "--blocked-by",
        &blocker_id,
    ]));
    let bundle = source.home.join("dependent.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "dependent-change",
            "--output",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .env("ARC_ROLE", "implementer")
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let blocked: serde_json::Value = serde_json::from_str(&stdout(
        destination
            .arc(&destination.root)
            .args(["status", "dependent-change"]),
    ))
    .unwrap();
    assert_eq!(blocked["blocker_status"]["blocked"], true);
    assert_eq!(
        blocked["blocker_status"]["blockers_ready"][0]["status"],
        "missing"
    );

    destination
        .arc(&destination.root)
        .args([
            "metadata",
            "dependent-change",
            "--remove-blocked-by",
            &blocker_id,
        ])
        .assert()
        .success();
    let cleared: serde_json::Value = serde_json::from_str(&stdout(
        destination
            .arc(&destination.root)
            .args(["status", "dependent-change"]),
    ))
    .unwrap();
    assert_eq!(cleared["blocked_by"], serde_json::json!([]));
    assert_eq!(cleared["blocker_status"]["blocked"], false);
}

#[test]
fn batch_check_treats_all_closed_outcomes_as_terminal() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "batch-live", "--tag", "#terminal-suite"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "batch-abandoned", "--tag", "#terminal-suite"]),
    );
    repo.arc(&repo.root)
        .args(["close", "batch-abandoned", "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["metadata", "batch-abandoned", "--tag", "#too-late"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "is abandoned; event is open-only",
        ));
    complete_change(&repo, "batch-live");

    repo.arc(&repo.root)
        .args(["check", "--tag", "#terminal-suite"])
        .assert()
        .success()
        .stdout(
            predicates::str::contains("batch-live-").and(predicates::str::contains(": integrated")),
        )
        .stdout(
            predicates::str::contains("batch-abandoned-")
                .and(predicates::str::contains(": abandoned")),
        );
}

#[test]
fn query_tags_batch_views_and_actionable_errors() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "tagged-a", "--tag", "#suite", "--tag", "#fast"]),
    );
    stdout(repo.arc(&repo.root).args([
        "begin",
        "tagged-b",
        "--blocked-by",
        "tagged-a",
        "--tag",
        "#suite",
    ]));

    let query = stdout(repo.arc(&repo.root).args([
        "query",
        "--status",
        "open",
        "--target",
        "master",
        "--tag",
        "#suite",
        "--actor",
        "tester",
        "--harness",
        "test",
    ]));
    assert_eq!(query.lines().count(), 2);
    assert!(query.contains("tagged-a-"));
    assert!(query.contains("tagged-b-"));

    let wide = stdout(repo.arc(&repo.root).args(["list", "--format", "wide"]));
    assert!(wide.contains("Verdict"));
    assert!(wide.contains("blocked-by:tagged-a"));

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "tagged-b"]))).unwrap();
    assert_eq!(status["next_action"], "wait_for:blockers");
    assert_eq!(status["ready_to_integrate"], false);
    assert_eq!(status["blocker_summary"]["hold"]["active"], false);

    repo.arc(&repo.root)
        .args(["check", "tagged-b"])
        .assert()
        .code(7)
        .stdout(predicates::str::contains("Cannot integrate"))
        .stdout(predicates::str::contains("Next step: wait_for:blockers"));
    repo.arc(&repo.root)
        .args(["integrate", "tagged-b"])
        .assert()
        .code(7)
        .stderr(predicates::str::contains("prerequisite changes unresolved"));

    repo.arc(&repo.root)
        .args(["metadata", "tagged-a", "--tag", "#extra"])
        .assert()
        .success();
    let extra = stdout(
        repo.arc(&repo.root)
            .args(["query", "--tag", "#extra", "--json"]),
    );
    let rows: serde_json::Value = serde_json::from_str(&extra).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["slug"], "tagged-a");

    // Metadata events remain first-class through deterministic transfer.
    let bundle = repo.home.join("tagged-a.json");
    repo.arc(&repo.root)
        .args(["export", "tagged-a", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("unknown event type").not());
    let transferred = stdout(
        destination
            .arc(&destination.root)
            .args(["query", "--tag", "#extra", "--json"]),
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&transferred)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // B already depends on A, so making A depend on B would form a cycle.
    repo.arc(&repo.root)
        .args(["metadata", "tagged-a", "--blocked-by", "tagged-b"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("dependency cycle"));

    let batch = stdout(
        repo.arc(&repo.root)
            .args(["show", "--tag", "#suite", "--json"]),
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&batch)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    repo.arc(&repo.root)
        .args(["check", "--tag", "#suite"])
        .assert()
        .code(3)
        .stdout(predicates::str::contains("tagged-a-"))
        .stdout(predicates::str::contains("tagged-b-"));
}

fn ready_tagged_change(repo: &Repo, slug: &str, blocked_by: Option<&str>) -> String {
    let mut begin = repo.arc(&repo.root);
    begin.args(["begin", slug, "--tag", "#series"]);
    if let Some(blocker) = blocked_by {
        begin.args(["--blocked-by", blocker]);
    }
    let change_id = opened_change_id(&stdout(&mut begin));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: add {slug}"),
    );
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    change_id
}

#[test]
fn tagged_integration_unblocks_and_merges_a_chain_in_dependency_order() {
    let repo = Repo::new();
    let first = ready_tagged_change(&repo, "series-first", None);
    let second = ready_tagged_change(&repo, "series-second", Some(&first));

    repo.arc(&repo.root)
        .args(["integrate", "--tag", "#series"])
        .assert()
        .success();

    let subjects = git_out(&repo.root, &["log", "--format=%s", "-2"]);
    assert_eq!(
        subjects.lines().collect::<Vec<_>>(),
        [
            "merge(series-second): series second",
            "merge(series-first): series first"
        ]
    );
    for change in [first, second] {
        let status: serde_json::Value =
            serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", &change]))).unwrap();
        assert_eq!(status["closure"]["outcome"], "integrated");
    }
}

#[test]
fn tagged_integration_cleanup_from_first_selected_worktree_keeps_batch_context_valid() {
    let repo = Repo::new();
    let first = ready_tagged_change(&repo, "series-cleanup-first", None);
    let second = ready_tagged_change(&repo, "series-cleanup-second", Some(&first));
    let first_worktree = repo.home.join(".worktrees/repo-series-cleanup-first");
    let second_worktree = repo.home.join(".worktrees/repo-series-cleanup-second");

    repo.arc(&first_worktree)
        .args(["integrate", "--tag", "#series", "--cleanup"])
        .assert()
        .success();

    assert!(!first_worktree.exists());
    assert!(!second_worktree.exists());
    for branch in ["arc/series-cleanup-first", "arc/series-cleanup-second"] {
        assert!(git_out(&repo.root, &["branch", "--list", branch]).is_empty());
    }
    for change in [first, second] {
        let status: serde_json::Value =
            serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", &change]))).unwrap();
        assert_eq!(status["closure"]["outcome"], "integrated");
    }
}

#[test]
fn tagged_integration_skips_closed_members_in_deterministic_order() {
    let repo = Repo::new();
    let closed = ready_tagged_change(&repo, "series-closed", None);
    repo.arc(&repo.root)
        .args(["integrate", "series-closed"])
        .assert()
        .success();
    let live = ready_tagged_change(&repo, "series-live", None);

    let output = stdout(repo.arc(&repo.root).args(["integrate", "--tag", "#series"]));
    assert!(output.contains(&format!("{closed}: integrated")));
    assert!(
        output.find(&format!("{closed}: integrated")).unwrap()
            < output.find("integrated:").unwrap()
    );
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", &live]))).unwrap();
    assert_eq!(status["closure"]["outcome"], "integrated");
}

#[test]
fn tagged_integration_skips_member_closed_while_waiting_for_target_lock() {
    let repo = Repo::new();
    let change = ready_tagged_change(&repo, "series-closed-under-lock", None);
    let target_lock = hold_target_lock(&repo, "master");
    let mut integrate = spawn_arc(&repo, &repo.root, &["integrate", "--tag", "#series"]);
    assert_waiting_on_transition_lock(&mut [&mut integrate]);

    repo.arc(&repo.root)
        .args(["close", &change, "--abandoned"])
        .assert()
        .success();
    target_lock.unlock().unwrap();

    assert!(wait_for_exit(&mut integrate).success());
    assert!(child_stdout(&mut integrate).contains(&format!("{change}: abandoned")));
}

#[test]
fn tagged_integration_stops_at_first_nonready_member_after_prior_merge() {
    let repo = Repo::new();
    let first = ready_tagged_change(&repo, "series-ready", None);
    let mut begin = repo.arc(&repo.root);
    begin.args([
        "begin",
        "series-not-ready",
        "--tag",
        "#series",
        "--blocked-by",
        &first,
    ]);
    let second = opened_change_id(&stdout(&mut begin));

    repo.arc(&repo.root)
        .args(["integrate", "--tag", "#series"])
        .assert()
        .code(3);

    let first_status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", &first]))).unwrap();
    let second_status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", &second]))).unwrap();
    assert_eq!(first_status["closure"]["outcome"], "integrated");
    assert_eq!(second_status["state"], "open");
    assert!(repo.root.join("series-ready.txt").exists());
}

#[test]
fn tagged_integration_reports_selector_and_option_errors() {
    let repo = Repo::new();
    ready_tagged_change(&repo, "series-options", None);

    repo.arc(&repo.root)
        .args(["integrate"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "provide a change or at least one --tag",
        ));
    repo.arc(&repo.root)
        .args(["integrate", "series-options", "--tag", "#series"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "provide a change or --tag, not both",
        ));
    repo.arc(&repo.root)
        .args(["integrate", "--tag", "#missing"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no changes match tags #missing"));
    repo.arc(&repo.root)
        .args(["integrate", "--tag", "#series", "--into", "master"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--into is only valid"));
    repo.arc(&repo.root)
        .args(["integrate", "--tag", "#series", "--message", "custom"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--message is only valid"));
}

#[test]
fn concurrent_metadata_updates_cannot_create_a_dependency_cycle() {
    let repo = Repo::new();
    let first = begin_change(&repo, "cycle-race-a", None);
    let second = begin_change(&repo, "cycle-race-b", None);

    let graph_lock = hold_graph_lock(&repo);
    let mut first_to_second = spawn_arc(
        &repo,
        &repo.root,
        &["metadata", &first, "--blocked-by", &second],
    );
    let mut second_to_first = spawn_arc(
        &repo,
        &repo.root,
        &["metadata", &second, "--blocked-by", &first],
    );
    assert_waiting_on_transition_lock(&mut [&mut first_to_second, &mut second_to_first]);
    graph_lock.unlock().unwrap();

    let first_status = wait_for_exit(&mut first_to_second);
    let second_status = wait_for_exit(&mut second_to_first);
    assert_ne!(first_status.success(), second_status.success());
    assert_eq!(
        [first_status, second_status]
            .iter()
            .filter(|status| status.success())
            .count(),
        1
    );
    let first_state: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["show", &first, "--json"]),
    ))
    .unwrap();
    let second_state: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["show", &second, "--json"]),
    ))
    .unwrap();
    let edge_count = first_state["blocked_by"].as_array().unwrap().len()
        + second_state["blocked_by"].as_array().unwrap().len();
    assert_eq!(edge_count, 1);
}
