use super::common::*;

#[test]
fn stage_claim_acquires_default_claim_and_stamps_its_generation() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "stage-claim", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["stage", "stage-claim", "implementing", "--claim"])
        .assert()
        .success();

    let events = stdout(
        repo.arc(&repo.root)
            .args(["events", "--change", "stage-claim"]),
    );
    let events = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let claim = events
        .iter()
        .find(|event| event["event_type"] == "claim-set")
        .unwrap();
    let stage = events
        .iter()
        .find(|event| event["event_type"] == "stage-set")
        .unwrap();
    assert_eq!(stage["claim_id"], claim["claim_id"]);
    assert_eq!(stage["stage"], "implementing");
    assert_eq!(claim["ttl_seconds"], 7200);
}

#[test]
fn blocked_on_requires_a_typed_blocker_and_stats_preserve_its_kind() {
    let repo = Repo::new();
    let change_id = begin_change(&repo, "typed-blocker", None);
    repo.arc(&repo.root)
        .args(["brief", "typed-blocker", "--body-file", "-"])
        .write_stdin("first contract\n")
        .assert()
        .success();
    let second = stdout(
        repo.arc(&repo.root)
            .args([
                "brief",
                "typed-blocker",
                "--body-file",
                "-",
                "--cause-note",
                "fixture revision",
            ])
            .write_stdin("second contract\n"),
    );
    let brief_event = second
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();
    repo.arc(&repo.root)
        .args(["claim", "typed-blocker"])
        .assert()
        .success();
    let before = event_count(&repo, &change_id);

    repo.arc(&repo.root)
        .args([
            "stage",
            "typed-blocker",
            "blocked-on",
            "--note",
            "missing named symbol",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("requires --blocker"));
    repo.arc(&repo.root)
        .args([
            "stage",
            "typed-blocker",
            "blocked-on",
            "--note",
            "wrong object kind",
            "--blocker",
            &format!("finding:{brief_event}"),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no finding matches"));
    repo.arc(&repo.root)
        .args([
            "stage",
            "typed-blocker",
            "implementing",
            "--blocker",
            "external",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--blocker is only valid with blocked-on",
        ));
    assert_eq!(event_count(&repo, &change_id), before);

    repo.arc(&repo.root)
        .args([
            "stage",
            "typed-blocker",
            "blocked-on",
            "--note",
            "missing named symbol",
            "--blocker",
            "brief:v2",
        ])
        .assert()
        .success();

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "typed-blocker"]),
    ))
    .unwrap();
    assert_eq!(status["claim"]["note"], "missing named symbol");
    assert_eq!(status["claim"]["blocker"]["kind"], "brief");
    assert_eq!(status["claim"]["blocker"]["brief_event_id"], brief_event);

    let stats =
        json_stdout(
            repo.arc(&repo.root)
                .args(["stats", "--change", "typed-blocker", "--json"]),
        );
    assert_eq!(
        stats["changes"][0]["blocks_by_kind"],
        serde_json::json!({"brief": 1})
    );
    assert_eq!(
        stats["aggregate"]["blocks_by_kind"],
        serde_json::json!({"brief": 1})
    );

    rewrite_event(&repo, &change_id, "stage-set", |event| {
        event.as_object_mut().unwrap().remove("blocker");
    });
    let legacy =
        json_stdout(
            repo.arc(&repo.root)
                .args(["stats", "--change", "typed-blocker", "--json"]),
        );
    assert_eq!(
        legacy["changes"][0]["blocks_by_kind"],
        serde_json::json!({"unclassified": 1})
    );
}

#[test]
fn claim_lifecycle_reports_defaults_renewal_conflict_release_and_expiry() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "claim-life"]));
    let change_id = opened_change_id(&opened);

    repo.arc(&repo.root)
        .args([
            "claim",
            "claim-life",
            "--ttl",
            "5m",
            "--stage-budget",
            "implementing=1m",
        ])
        .assert()
        .success();
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "claim-life"]))).unwrap();
    assert_eq!(status["schema"], "arc-status/8");
    assert_eq!(status["claim"]["owner"]["actor"], "tester");
    assert_eq!(status["claim"]["owner"]["harness"], "test");
    assert_eq!(status["claim"]["owner"]["session"], "session-a");
    assert_eq!(status["claim"]["ttl_seconds"], 300);
    assert_eq!(status["claim"]["stage"], "launch");
    assert_eq!(status["claim"]["stage_budgets"]["launch"], 60);
    assert_eq!(status["claim"]["stage_budgets"]["started"], 300);
    assert_eq!(status["claim"]["stage_budgets"]["spec-read"], 120);
    assert_eq!(status["claim"]["stage_budgets"]["implementing"], 60);
    assert_eq!(status["claim"]["stage_budgets"]["verifying"], 900);
    assert_eq!(status["claim"]["stage_budgets"]["blocked-on"], 900);
    assert_eq!(status["claim"]["stage_budgets"]["snapshotted"], 3600);
    assert_eq!(status["claim"]["active"], true);

    let original_claim_id = status["claim"]["claim_id"].clone();
    let original_claimed_at = status["claim"]["claimed_at"].clone();
    thread::sleep(Duration::from_millis(5));
    repo.arc(&repo.root)
        .args(["claim", "claim-life"])
        .assert()
        .success();
    let renewed: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "claim-life"]))).unwrap();
    assert_eq!(renewed["claim"]["claimed_at"], original_claimed_at);
    assert_eq!(renewed["claim"]["claim_id"], original_claim_id);
    assert_ne!(
        renewed["claim"]["last_activity_at"],
        status["claim"]["last_activity_at"]
    );
    let claim_events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "claim-life",
        "--type",
        "claim-set",
    ]));
    assert_eq!(claim_events.lines().count(), 2, "same owner renews");

    repo.arc(&repo.root)
        .env("ARC_SESSION", "session-b")
        .args(["claim", "claim-life"])
        .assert()
        .code(8)
        .stderr(predicates::str::contains("owner=tester"))
        .stderr(predicates::str::contains("session=session-a"))
        .stderr(predicates::str::contains("stage=launch"));
    repo.arc(&repo.root)
        .env("ARC_SESSION", "lead-session")
        .args(["release-claim", "claim-life"])
        .assert()
        .success();
    let released: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "claim-life"]))).unwrap();
    assert!(released["claim"].is_null());
    repo.arc(&repo.root)
        .args(["release-claim", "claim-life"])
        .assert()
        .code(8);

    repo.arc(&repo.root)
        .args(["claim", "claim-life", "--ttl", "1s"])
        .assert()
        .success();
    age_event(&repo, &change_id, "claim-set", 5);
    let expired: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "claim-life"]))).unwrap();
    assert_eq!(expired["claim"]["active"], false);
    assert_eq!(expired["claim"]["expired"], true);
    assert_eq!(expired["claim"]["stale"], false);
    repo.arc(&repo.root)
        .args(["stage", "claim-life", "started"])
        .assert()
        .code(8);
    repo.arc(&repo.root)
        .args(["release-claim", "claim-life"])
        .assert()
        .code(8);
    repo.arc(&repo.root)
        .env("ARC_SESSION", "session-b")
        .args(["claim", "claim-life"])
        .assert()
        .success();
    let reclaimed: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "claim-life"]))).unwrap();
    assert_eq!(reclaimed["claim"]["owner"]["session"], "session-b");
    assert_ne!(reclaimed["claim"]["claim_id"], original_claim_id);
}

#[test]
fn claim_duration_budget_and_identity_validation_is_strict() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "claim-input"]));
    for invalid in ["0s", "5d", "s", "-1m", "18446744073709551615h"] {
        repo.arc(&repo.root)
            .args(["claim", "claim-input", "--ttl", invalid])
            .assert()
            .failure();
    }
    for invalid in ["unknown=1m", "implementing", "implementing=0s"] {
        repo.arc(&repo.root)
            .args(["claim", "claim-input", "--stage-budget", invalid])
            .assert()
            .failure();
    }
    repo.arc(&repo.root)
        .env_remove("ARC_SESSION")
        .args(["claim", "claim-input"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("nonempty ARC_SESSION"));
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "")
        .args(["claim", "claim-input"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("nonempty ARC_HARNESS"));
}

#[test]
fn concurrent_claims_have_one_winner_and_stage_release_replay_stays_readable() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "claim-race"]));
    let change_id = opened_change_id(&opened);

    let transition_lock = hold_transition_lock(&repo, &change_id);
    let mut contenders = (0..16)
        .map(|index| {
            spawn_arc_with_session(
                &repo,
                &repo.root,
                &["claim", "claim-race"],
                &format!("contender-{index}"),
            )
        })
        .collect::<Vec<_>>();
    let mut contender_refs = contenders.iter_mut().collect::<Vec<_>>();
    assert_waiting_on_transition_lock(&mut contender_refs);
    transition_lock.unlock().unwrap();
    let statuses = contenders.iter_mut().map(wait_for_exit).collect::<Vec<_>>();
    assert_eq!(
        statuses.iter().filter(|status| status.success()).count(),
        1,
        "exactly one serialized acquisition succeeds"
    );
    assert!(statuses
        .iter()
        .filter(|status| !status.success())
        .all(|status| status.code() == Some(8)));
    let claim_events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "claim-race",
        "--type",
        "claim-set",
    ]));
    assert_eq!(claim_events.lines().count(), 1);
    assert!(repo
        .root
        .join(".git/arc/locks")
        .join(format!("{change_id}.lock"))
        .is_file());

    repo.arc(&repo.root)
        .env("ARC_SESSION", "lead")
        .args(["release-claim", "claim-race"])
        .assert()
        .success();
    for iteration in 0..12 {
        repo.arc(&repo.root)
            .args(["claim", "claim-race"])
            .assert()
            .success();
        let mut stage = spawn_arc_with_session(
            &repo,
            &repo.root,
            &["stage", "claim-race", "started"],
            "session-a",
        );
        let mut release = spawn_arc_with_session(
            &repo,
            &repo.root,
            &["release-claim", "claim-race"],
            &format!("lead-{iteration}"),
        );
        let stage_status = wait_for_exit(&mut stage);
        let release_status = wait_for_exit(&mut release);
        assert!(release_status.success());
        assert!(stage_status.success() || stage_status.code() == Some(8));
        repo.arc(&repo.root)
            .args(["status", "claim-race"])
            .assert()
            .success()
            .stdout(predicates::str::contains("\"claim\": null"));
    }
}

#[test]
fn transition_lock_acquisition_is_bounded() {
    let repo = Repo::new();
    let opened = stdout(
        repo.arc(&repo.root)
            .args(["begin", "bounded-lock", "--no-worktree"]),
    );
    let change_id = opened_change_id(&opened);
    let transition_lock = hold_transition_lock(&repo, &change_id);
    let started = Instant::now();
    let mut claim = spawn_arc(&repo, &repo.root, &["claim", "bounded-lock"]);
    assert_eq!(wait_for_exit(&mut claim).code(), Some(1));
    assert!(started.elapsed() < Duration::from_secs(3));
    transition_lock.unlock().unwrap();
}

#[test]
fn claim_persists_normalized_identity_and_handles_maximum_ttl() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "claim-normalized"]));
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "  Executor  ")
        .env("ARC_HARNESS", "  codex  ")
        .env("ARC_SESSION", "  thread-1  ")
        .args(["claim", "claim-normalized", "--ttl", "9223372036854775807s"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "  Executor  ")
        .env("ARC_HARNESS", "  codex  ")
        .env("ARC_SESSION", "  thread-1  ")
        .args(["stage", "claim-normalized", "started"])
        .assert()
        .success();

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "claim-normalized"]),
    ))
    .unwrap();
    assert_eq!(status["claim"]["owner"]["actor"], "Executor");
    assert_eq!(status["claim"]["owner"]["harness"], "codex");
    assert_eq!(status["claim"]["owner"]["session"], "thread-1");
    assert_eq!(status["claim"]["active"], true);
    assert_eq!(
        status["claim"]["ttl_seconds"],
        serde_json::Value::from(i64::MAX)
    );

    let transitions = stdout(
        repo.arc(&repo.root)
            .args(["events", "--change", "claim-normalized"]),
    );
    for event in transitions.lines().filter_map(|line| {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        matches!(
            event["event_type"].as_str(),
            Some("claim-set" | "stage-set")
        )
        .then_some(event)
    }) {
        assert_eq!(event["actor"], "Executor");
        assert_eq!(event["harness"], "codex");
        assert_eq!(event["session"], "thread-1");
    }
}

#[test]
fn stage_requires_owned_live_claim_and_tracks_heartbeats_advisory_order_and_snapshot() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "stage-life"]));
    let worktree = repo.home.join(".worktrees/repo-stage-life");

    repo.arc(&repo.root)
        .args(["stage", "stage-life", "started"])
        .assert()
        .code(8);
    repo.arc(&repo.root)
        .args(["claim", "stage-life", "--ttl", "1s"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "stage",
            "stage-life",
            "implementing",
            "--note",
            "skipped ahead",
        ])
        .assert()
        .success();
    let first: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stage-life"]))).unwrap();
    thread::sleep(Duration::from_millis(5));
    repo.arc(&repo.root)
        .args(["stage", "stage-life", "implementing", "--note", "heartbeat"])
        .assert()
        .success();
    let heartbeat: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stage-life"]))).unwrap();
    assert_ne!(
        first["claim"]["stage_started_at"], heartbeat["claim"]["stage_started_at"],
        "a heartbeat refreshes age-in-stage and its budget clock"
    );
    assert_ne!(
        first["claim"]["last_activity_at"],
        heartbeat["claim"]["last_activity_at"]
    );
    assert_eq!(heartbeat["claim"]["note"], "heartbeat");

    repo.arc(&repo.root)
        .env("ARC_SESSION", "foreign")
        .args(["stage", "stage-life", "verifying"])
        .assert()
        .code(8);
    repo.arc(&repo.root)
        .args(["stage", "stage-life", "blocked-on"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("requires a nonempty --note"));
    repo.arc(&repo.root)
        .args([
            "stage",
            "stage-life",
            "blocked-on",
            "--note",
            "waiting for input",
            "--blocker",
            "external",
        ])
        .assert()
        .success();

    repo.commit(&worktree, "stage.txt", "done\n", "feat: finish stage work");
    repo.arc(&worktree)
        .args(["snapshot", "stage-life"])
        .assert()
        .success();
    let snapshotted: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stage-life"]))).unwrap();
    assert_eq!(snapshotted["claim"]["stage"], "snapshotted");
    assert_eq!(snapshotted["claim"]["stale"], false);
    repo.arc(&repo.root)
        .args(["stage", "stage-life", "snapshotted"])
        .assert()
        .failure();

    let change_id = opened_change_id(&opened);
    age_event(&repo, &change_id, "patchset-added", 5);
    repo.arc(&repo.root)
        .args(["claim", "stage-life"])
        .assert()
        .success();
    let restarted: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stage-life"]))).unwrap();
    assert_eq!(restarted["claim"]["stage"], "launch");
    assert_eq!(restarted["claim"]["expired"], false);
}

#[test]
fn stale_claims_are_time_derived_and_watch_until_stalled_reaches() {
    let repo = Repo::new();

    stdout(repo.arc(&repo.root).args(["begin", "wall-clock-stall"]));
    repo.arc(&repo.root)
        .args(["claim", "wall-clock-stall", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    let started = Instant::now();
    let mut watcher = spawn_arc(
        &repo,
        &repo.root,
        &[
            "watch",
            "wall-clock-stall",
            "--until",
            "stalled",
            "--timeout",
            "4",
        ],
    );
    assert!(wait_for_exit(&mut watcher).success());
    assert_eq!(child_stdout(&mut watcher), "reached: stalled\n");
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "a fresh claim must become stalled through wall-clock passage"
    );

    let launch = stdout(repo.arc(&repo.root).args(["begin", "stale-launch"]));
    let launch_id = opened_change_id(&launch);
    repo.arc(&repo.root)
        .args(["claim", "stale-launch", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    age_event(&repo, &launch_id, "claim-set", 5);
    let launch_status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "stale-launch"]),
    ))
    .unwrap();
    assert_eq!(launch_status["claim"]["active"], true);
    assert_eq!(launch_status["claim"]["stale"], true);
    assert_eq!(launch_status["claim"]["stage"], "launch");
    repo.arc(&repo.root)
        .args([
            "watch",
            "stale-launch",
            "--until",
            "stalled",
            "--timeout",
            "1",
        ])
        .assert()
        .success()
        .stdout("reached: stalled\n");
    repo.arc(&repo.root)
        .env("ARC_SESSION", "lead-recovery")
        .args(["release-claim", "stale-launch"])
        .assert()
        .success();

    let implementing = stdout(repo.arc(&repo.root).args(["begin", "stale-impl"]));
    let implementing_id = opened_change_id(&implementing);
    repo.arc(&repo.root)
        .args(["claim", "stale-impl", "--stage-budget", "implementing=1s"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["stage", "stale-impl", "implementing"])
        .assert()
        .success();
    age_event(&repo, &implementing_id, "stage-set", 5);
    let implementing_status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stale-impl"]))).unwrap();
    assert_eq!(implementing_status["claim"]["stale"], true);
    assert!(
        implementing_status["claim"]["age_seconds"]
            .as_u64()
            .unwrap()
            >= 5
    );

    repo.arc(&repo.root)
        .args([
            "stage",
            "stale-impl",
            "blocked-on",
            "--note",
            "distress",
            "--blocker",
            "external",
        ])
        .assert()
        .success();
    age_event(&repo, &implementing_id, "stage-set", 60);
    let blocked: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stale-impl"]))).unwrap();
    assert_eq!(blocked["claim"]["stage"], "blocked-on");
    assert_eq!(blocked["claim"]["stale"], false);
    assert_eq!(blocked["claim"]["budget_seconds"], 900);
}

#[test]
fn takeover_refuses_an_active_claim_that_is_not_stale() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "takeover-fresh"]));
    repo.arc(&repo.root)
        .args(["claim", "takeover-fresh"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "taker")
        .env("ARC_HARNESS", "other")
        .env("ARC_SESSION", "session-b")
        .args(["claim", "takeover-fresh", "--takeover"])
        .assert()
        .code(8)
        .stderr(predicates::str::contains(
            "--takeover is unavailable because the claim is not yet stale",
        ));

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "takeover-fresh"]),
    ))
    .unwrap();
    assert_eq!(status["claim"]["owner"]["actor"], "tester");
    assert_eq!(status["claim"]["owner"]["session"], "session-a");
}

#[test]
fn stale_claim_conflict_names_the_explicit_takeover_path() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "takeover-offered"]));
    let change_id = opened_change_id(&opened);
    repo.arc(&repo.root)
        .args(["claim", "takeover-offered", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    age_event(&repo, &change_id, "claim-set", 5);

    repo.arc(&repo.root)
        .env("ARC_SESSION", "session-b")
        .args(["claim", "takeover-offered"])
        .assert()
        .code(8)
        .stderr(predicates::str::contains(
            "--takeover would displace this stale claim",
        ));
}

#[test]
fn takeover_records_and_reports_the_displaced_claim() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "takeover-recorded"]));
    let change_id = opened_change_id(&opened);
    repo.arc(&repo.root)
        .args([
            "claim",
            "takeover-recorded",
            "--stage-budget",
            "implementing=1s",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["stage", "takeover-recorded", "implementing"])
        .assert()
        .success();
    age_event(&repo, &change_id, "stage-set", 5);

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "taker")
        .env("ARC_HARNESS", "other")
        .env("ARC_SESSION", "session-b")
        .args(["claim", "takeover-recorded", "--takeover"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "displaced: owner=tester harness=test session=session-a stage=implementing",
        ));

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "takeover-recorded",
        "--type",
        "claim-set",
    ]));
    let replacement: serde_json::Value =
        serde_json::from_str(events.lines().last().unwrap()).unwrap();
    assert_eq!(replacement["displaced"]["actor"], "tester");
    assert_eq!(replacement["displaced"]["harness"], "test");
    assert_eq!(replacement["displaced"]["session"], "session-a");
    assert_eq!(replacement["displaced"]["stage"], "implementing");
    assert!(replacement["displaced"]["claim_id"].is_string());

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "takeover-recorded"]),
    ))
    .unwrap();
    assert_eq!(status["claim"]["owner"]["actor"], "taker");
    assert_eq!(status["claim"]["owner"]["harness"], "other");
    assert_eq!(status["claim"]["owner"]["session"], "session-b");
}

#[test]
fn takeover_displaces_a_stale_snapshotted_claim() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "takeover-snapshotted"]));
    let change_id = opened_change_id(&opened);
    let worktree = repo.home.join(".worktrees/repo-takeover-snapshotted");
    repo.commit(
        &worktree,
        "change.txt",
        "change\n",
        "feat: add snapshot change",
    );
    repo.arc(&worktree)
        .args([
            "claim",
            "takeover-snapshotted",
            "--stage-budget",
            "snapshotted=1s",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["snapshot", "takeover-snapshotted"])
        .assert()
        .success();
    age_event(&repo, &change_id, "patchset-added", 5);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "taker")
        .env("ARC_HARNESS", "other")
        .env("ARC_SESSION", "session-b")
        .args(["claim", "takeover-snapshotted", "--takeover"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "displaced: owner=tester harness=test session=session-a stage=snapshotted",
        ));
}

#[test]
fn takeover_displaces_a_stale_blocked_on_claim() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "takeover-blocked"]));
    let change_id = opened_change_id(&opened);
    repo.arc(&repo.root)
        .args([
            "claim",
            "takeover-blocked",
            "--stage-budget",
            "blocked-on=1s",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "stage",
            "takeover-blocked",
            "blocked-on",
            "--note",
            "waiting",
            "--blocker",
            "external",
        ])
        .assert()
        .success();
    age_event(&repo, &change_id, "stage-set", 5);

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "taker")
        .env("ARC_HARNESS", "other")
        .env("ARC_SESSION", "session-b")
        .args(["claim", "takeover-blocked", "--takeover"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "displaced: owner=tester harness=test session=session-a stage=blocked-on",
        ));
}

#[test]
fn takeover_replay_has_exactly_one_live_claim_owned_by_the_taker() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "takeover-replay"]));
    let change_id = opened_change_id(&opened);
    repo.arc(&repo.root)
        .args(["claim", "takeover-replay", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    age_event(&repo, &change_id, "claim-set", 5);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "taker")
        .env("ARC_SESSION", "session-b")
        .args(["claim", "takeover-replay", "--takeover"])
        .assert()
        .success();

    let replayed: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "takeover-replay"]),
    ))
    .unwrap();
    assert_eq!(replayed["claim"]["owner"]["actor"], "taker");
    assert_eq!(replayed["claim"]["owner"]["session"], "session-b");
    assert!(replayed["claim"].is_object());
}

#[test]
fn claim_event_without_displaced_still_deserializes() {
    let repo = Repo::new();
    let opened = stdout(
        repo.arc(&repo.root)
            .args(["begin", "claim-backward-compatible"]),
    );
    let change_id = opened_change_id(&opened);
    repo.arc(&repo.root)
        .args(["claim", "claim-backward-compatible"])
        .assert()
        .success();
    rewrite_event(&repo, &change_id, "claim-set", |event| {
        assert!(event.as_object_mut().unwrap().remove("displaced").is_none());
    });

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["status", "claim-backward-compatible"]),
    ))
    .unwrap();
    assert_eq!(status["claim"]["owner"]["actor"], "tester");
}

#[test]
fn alternatives_exclude_active_claims_but_include_stale_and_expired_claims() {
    let repo = Repo::new();
    let prerequisite = stdout(repo.arc(&repo.root).args(["begin", "alt-prereq"]));
    let prerequisite_id = opened_change_id(&prerequisite);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "alt-blocked", "--blocked-by", &prerequisite_id]),
    );
    stdout(repo.arc(&repo.root).args(["begin", "alt-active"]));
    let stale = stdout(repo.arc(&repo.root).args(["begin", "alt-stale"]));
    let stale_id = opened_change_id(&stale);
    let expired = stdout(repo.arc(&repo.root).args(["begin", "alt-expired"]));
    let expired_id = opened_change_id(&expired);

    repo.arc(&repo.root)
        .args(["claim", "alt-active"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["claim", "alt-stale", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    age_event(&repo, &stale_id, "claim-set", 5);
    repo.arc(&repo.root)
        .args(["claim", "alt-expired", "--ttl", "1s"])
        .assert()
        .success();
    age_event(&repo, &expired_id, "claim-set", 5);

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "alt-blocked"]),
    ))
    .unwrap();
    let slugs = status["suggested_alternatives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|alternative| alternative["slug"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(slugs.contains(&"alt-prereq"));
    assert!(slugs.contains(&"alt-stale"));
    assert!(slugs.contains(&"alt-expired"));
    assert!(!slugs.contains(&"alt-active"));
}

#[test]
fn integration_warns_for_foreign_claim_but_still_succeeds_when_green() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "claimed-green"]));
    let worktree = repo.home.join(".worktrees/repo-claimed-green");
    repo.commit(&worktree, "green.txt", "green\n", "feat: add green change");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "executor")
        .env("ARC_SESSION", "executor-session")
        .args(["claim", "claimed-green"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["snapshot", "claimed-green"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "claimed-green", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "claimed-green"])
        .assert()
        .success()
        .stderr(predicates::str::contains(
            "warning: active foreign claim by executor",
        ));
}

#[test]
fn transition_lock_serializes_hold_against_integrate() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "hold-integrate-race");
    repo.arc(&worktree)
        .args(["review", "hold-integrate-race", "--verdict", "approved"])
        .assert()
        .success();

    let transition_lock = hold_transition_lock(&repo, &change_id);
    let mut integrate = spawn_arc_with_session(
        &repo,
        &repo.root,
        &["integrate", "hold-integrate-race"],
        "lead-integrate",
    );
    let mut hold = spawn_arc_with_session(
        &repo,
        &repo.root,
        &[
            "hold",
            "hold-integrate-race",
            "--reason",
            "concurrent user hold",
        ],
        "lead-hold",
    );
    assert_waiting_on_transition_lock(&mut [&mut integrate, &mut hold]);
    transition_lock.unlock().unwrap();

    let integrate_status = wait_for_exit(&mut integrate);
    let hold_status = wait_for_exit(&mut hold);
    assert_ne!(
        integrate_status.success(),
        hold_status.success(),
        "serialized hold/integrate permits exactly one transition"
    );
    if integrate_status.success() {
        assert_eq!(hold_status.code(), Some(1));
    } else {
        assert_eq!(integrate_status.code(), Some(4));
        assert!(hold_status.success());
    }
    repo.arc(&repo.root)
        .args(["status", "hold-integrate-race"])
        .assert()
        .success();
}

#[test]
fn concurrent_integrations_serialize_on_the_target_branch_lock() {
    let repo = Repo::new();
    let (_, first_worktree, _) = change_with_patchset(&repo, "target-race-a");
    let (_, second_worktree, _) = change_with_patchset(&repo, "target-race-b");
    repo.arc(&first_worktree)
        .args(["review", "target-race-a", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&second_worktree)
        .args(["review", "target-race-b", "--verdict", "approved"])
        .assert()
        .success();

    let target_lock = hold_target_lock(&repo, "master");
    let mut first = spawn_arc_with_session(
        &repo,
        &repo.root,
        &["integrate", "target-race-a"],
        "integrator-a",
    );
    let mut second = spawn_arc_with_session(
        &repo,
        &repo.root,
        &["integrate", "target-race-b"],
        "integrator-b",
    );
    assert_waiting_on_transition_lock(&mut [&mut first, &mut second]);
    target_lock.unlock().unwrap();

    assert!(wait_for_exit(&mut first).success());
    assert!(wait_for_exit(&mut second).success());
    assert!(repo.root.join("target-race-a.txt").is_file());
    assert!(repo.root.join("target-race-b.txt").is_file());
    repo.arc(&repo.root)
        .args(["status", "target-race-a"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"outcome\": \"integrated\""));
    repo.arc(&repo.root)
        .args(["status", "target-race-b"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"outcome\": \"integrated\""));
}

#[test]
fn verification_gate_can_mutate_arc_state_without_lock_reentry_deadlock() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "verify-reentry", "--no-worktree"]),
    );
    let binary = std::env::var("CARGO_BIN_EXE_arc").unwrap();
    let gate = format!("\"{binary}\" hold verify-reentry --reason gate-self-mutation");
    let mut verify = spawn_arc(
        &repo,
        &repo.root,
        &["verify", "verify-reentry", "--command", &gate],
    );
    assert!(wait_for_exit(&mut verify).success());

    let state: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "show",
        "verify-reentry",
        "--json",
    ])))
    .unwrap();
    let holds = state["holds"].as_object().expect("holds map");
    assert_eq!(holds.len(), 1);
    assert_eq!(
        holds.values().next().unwrap()["reason"],
        "gate-self-mutation"
    );
    assert_eq!(state["verifications"].as_array().unwrap().len(), 1);
}

#[test]
fn snapshot_captures_git_identities_and_renders_claim_actor_mismatch() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "snapshot-who"]));
    let worktree = repo.home.join(".worktrees/repo-snapshot-who");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Executor Person")
        .args(["claim", "snapshot-who"])
        .assert()
        .success();
    repo.commit(&worktree, "who.txt", "who\n", "feat: record identity");
    repo.arc(&worktree)
        .args(["snapshot", "snapshot-who"])
        .assert()
        .success();

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "snapshot-who"]),
    ))
    .unwrap();
    assert_eq!(status["latest_patchset"]["author"]["name"], "Tester");
    assert_eq!(
        status["latest_patchset"]["author"]["email"],
        "tester@example.invalid"
    );
    assert_eq!(status["latest_patchset"]["committer"]["name"], "Tester");
    assert_eq!(status["latest_patchset"]["claim_actor"], "Executor Person");
    assert_eq!(status["latest_patchset"]["provenance_mismatch"], true);
    assert_eq!(status["claim"]["snapshot_author"]["name"], "Tester");
    assert_eq!(status["claim"]["provenance_mismatch"], true);
    repo.arc(&repo.root)
        .args(["show", "snapshot-who"])
        .assert()
        .success()
        .stdout(predicates::str::contains("PROVENANCE MISMATCH"))
        .stdout(predicates::str::contains("--on-behalf-of"));
}

#[test]
fn shared_git_identity_omits_inapplicable_provenance_mismatch() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[provenance]\ngit_identity = \"shared\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy: share git identity"]);
    stdout(repo.arc(&repo.root).args(["begin", "snapshot-shared"]));
    let worktree = repo.home.join(".worktrees/repo-snapshot-shared");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Executor Person")
        .args(["claim", "snapshot-shared"])
        .assert()
        .success();
    repo.commit(&worktree, "who.txt", "who\n", "feat: record identity");
    repo.arc(&worktree)
        .args(["snapshot", "snapshot-shared"])
        .assert()
        .success();

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "snapshot-shared"]),
    ))
    .unwrap();
    assert!(status["latest_patchset"]
        .as_object()
        .unwrap()
        .get("provenance_mismatch")
        .is_none());
    assert!(status["claim"]
        .as_object()
        .unwrap()
        .get("provenance_mismatch")
        .is_none());
    repo.arc(&repo.root)
        .args(["show", "snapshot-shared"])
        .assert()
        .success()
        .stdout(predicates::str::contains("PROVENANCE MISMATCH").not());
}

#[test]
fn matching_git_identity_is_unaffected_by_shared_mode() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[provenance]\ngit_identity = \"shared\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy: share git identity"]);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "snapshot-shared-match"]),
    );
    let worktree = repo.home.join(".worktrees/repo-snapshot-shared-match");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Tester")
        .args(["claim", "snapshot-shared-match"])
        .assert()
        .success();
    repo.commit(&worktree, "who.txt", "who\n", "feat: record identity");
    repo.arc(&worktree)
        .args(["snapshot", "snapshot-shared-match"])
        .assert()
        .success();

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["status", "snapshot-shared-match"]),
    ))
    .unwrap();
    assert!(status["latest_patchset"]
        .as_object()
        .unwrap()
        .get("provenance_mismatch")
        .is_none());
    assert!(status["claim"]
        .as_object()
        .unwrap()
        .get("provenance_mismatch")
        .is_none());
}
