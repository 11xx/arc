use super::common::*;

/// Skipping an event type is safe for a comment and fatal for a closure: a
/// build that did not know an integration event would read the change as open
/// and close it a second way. So the bundle carries the format it was written
/// with, and an older importer refuses rather than half-reading it.
/// A bundle carrying an integration event makes the destination hold events
/// an older build would skip, exactly as recording one locally does. A stamp
/// applied on only one of those paths protects only half the ledgers.
#[test]
fn importing_an_integration_stamps_the_destination_store_format() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "shipped"]));
    let worktree = repo.home.join(".worktrees/repo-shipped");
    repo.commit(&worktree, "shipped.rs", "done\n", "feat: shipped");
    stdout(repo.arc(&worktree).args(["snapshot", "shipped"]));
    stdout(
        repo.arc(&repo.root)
            .args(["review", "shipped", "--verdict", "approved"]),
    );
    repo.arc(&repo.root)
        .args(["integrate", "shipped"])
        .assert()
        .success();
    let bundle = repo.home.join("shipped.json");
    repo.arc(&repo.root)
        .args(["export", "shipped", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    // A destination whose store predates the format.
    let other = Repo::new();
    stdout(
        other
            .arc(&other.root)
            .args(["begin", "seed", "--no-worktree"]),
    );
    let config_path = other.root.join(".git/arc/config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["schema_version"] = serde_json::json!(1);
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    other
        .arc(&other.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 2, "{config}");
}

#[test]
fn importing_a_waiver_only_integration_stamps_store_format_three() {
    let source = Repo::new();
    stdout(source.arc(&source.root).args(["begin", "waiver-only"]));
    let worktree = source.home.join(".worktrees/repo-waiver-only");
    source.commit(
        &worktree,
        "waiver.txt",
        "review later\n",
        "feat: waiver only",
    );
    stdout(source.arc(&worktree).args(["snapshot", "waiver-only"]));
    source
        .arc(&source.root)
        .args(["integrate", "waiver-only", "--audit-debt", "review later"])
        .assert()
        .success();
    let event_dir = source.root.join(".git/arc/changes");
    let integration_path = fs::read_dir(&event_dir)
        .unwrap()
        .flat_map(|change| fs::read_dir(change.unwrap().path().join("events")).unwrap())
        .map(|event| event.unwrap().path())
        .find(|path| {
            serde_json::from_slice::<serde_json::Value>(&fs::read(path).unwrap())
                .is_ok_and(|event| event["event_type"] == "change-integrated")
        })
        .unwrap();
    let mut integration: serde_json::Value =
        serde_json::from_slice(&fs::read(&integration_path).unwrap()).unwrap();
    integration["authorization"]["verdict_event_id"] = serde_json::Value::Null;
    fs::write(
        &integration_path,
        serde_json::to_vec_pretty(&integration).unwrap(),
    )
    .unwrap();
    let source_config_path = source.root.join(".git/arc/config.json");
    let mut source_config: serde_json::Value =
        serde_json::from_slice(&fs::read(&source_config_path).unwrap()).unwrap();
    source_config["schema_version"] = serde_json::json!(2);
    fs::write(
        &source_config_path,
        serde_json::to_vec_pretty(&source_config).unwrap(),
    )
    .unwrap();
    source
        .arc(&source.root)
        .args(["status", "waiver-only", "--json"])
        .assert()
        .success();
    let repaired: serde_json::Value =
        serde_json::from_slice(&fs::read(&source_config_path).unwrap()).unwrap();
    assert_eq!(repaired["schema_version"], 3, "{repaired}");

    let bundle = source.home.join("waiver-only.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "waiver-only",
            "--output",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(worktree.is_dir());

    let destination = Repo::new();
    stdout(
        destination
            .arc(&destination.root)
            .args(["begin", "seed", "--no-worktree"]),
    );
    let config_path = destination.root.join(".git/arc/config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["schema_version"] = serde_json::json!(2);
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 3, "{config}");
}

#[test]
fn a_bundle_from_a_newer_arc_is_refused_rather_than_partially_imported() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "future", "--no-worktree"]),
    );
    let bundle = repo.home.join("future.json");
    repo.arc(&repo.root)
        .args(["export", "future", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    assert!(value["store_format"].as_u64().is_some(), "{value}");
    value["store_format"] = serde_json::json!(9999);
    fs::write(&bundle, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

    let other = Repo::new();
    other
        .arc(&other.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .failure();
}

#[test]
fn export_is_deterministic() {
    let repo = Repo::new();
    change_with_patchset(&repo, "move-d");
    let first = repo.home.join("first.json");
    let second = repo.home.join("second.json");

    repo.arc(&repo.root)
        .args(["export", "move-d", "--output", first.to_str().unwrap()])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["export", "move-d", "--output", second.to_str().unwrap()])
        .assert()
        .success();

    assert_eq!(fs::read(first).unwrap(), fs::read(second).unwrap());
}

#[test]
fn bundle_roundtrip_preserves_claim_stage_and_snapshot_provenance_events() {
    let source = Repo::new();
    let opened = stdout(source.arc(&source.root).args(["begin", "move-claim"]));
    let change_id = opened_change_id(&opened);
    let worktree = source.home.join(".worktrees/repo-move-claim");
    source
        .arc(&worktree)
        .env("ARC_ACTOR", "Executor")
        .args([
            "claim",
            "move-claim",
            "--ttl",
            "5m",
            "--stage-budget",
            "implementing=2m",
        ])
        .assert()
        .success();
    source
        .arc(&worktree)
        .env("ARC_ACTOR", "Executor")
        .args(["stage", "move-claim", "implementing"])
        .assert()
        .success();
    source.commit(&worktree, "move.txt", "move\n", "feat: move claimed work");
    source
        .arc(&worktree)
        .args(["snapshot", "move-claim", "--solo"])
        .assert()
        .success();

    let bundle = source.home.join("claim-bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-claim", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let exported: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let event_types = exported["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["event_type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(event_types.contains(&"claim-set"));
    assert!(event_types.contains(&"stage-set"));
    let claim = exported["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "claim-set")
        .unwrap();
    let stage = exported["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "stage-set")
        .unwrap();
    let patchset = exported["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "patchset-added")
        .unwrap();
    assert_eq!(patchset["author_name"], "Tester");
    assert_eq!(patchset["committer_email"], "tester@example.invalid");
    assert_eq!(stage["claim_id"], claim["claim_id"]);
    assert_eq!(patchset["claim_id"], claim["claim_id"]);
    assert_eq!(patchset["claim_actor"], "Executor");

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("unknown event type").not());
    let roundtrip = destination.home.join("claim-roundtrip.json");
    destination
        .arc(&destination.root)
        .args([
            "export",
            &change_id,
            "--output",
            roundtrip.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert_eq!(fs::read(bundle).unwrap(), fs::read(roundtrip).unwrap());
}

#[test]
fn old_patchset_events_without_identity_fields_remain_readable() {
    let repo = Repo::new();
    let (change_id, _, _) = change_with_patchset(&repo, "old-patchset");
    rewrite_event(&repo, &change_id, "patchset-added", |event| {
        event.as_object_mut().unwrap().remove("author_name");
        event.as_object_mut().unwrap().remove("author_email");
        event.as_object_mut().unwrap().remove("committer_name");
        event.as_object_mut().unwrap().remove("committer_email");
    });

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "old-patchset"]),
    ))
    .unwrap();
    assert!(status["latest_patchset"]["author"].is_null());
    assert!(status["latest_patchset"]["committer"].is_null());
    repo.arc(&repo.root)
        .args(["show", "old-patchset"])
        .assert()
        .success();
}

#[test]
fn verdict_event_without_body_remains_readable() {
    let source = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&source, "old-verdict");
    source
        .arc(&worktree)
        .args([
            "review",
            "old-verdict",
            "--verdict",
            "approved",
            "--body",
            "temporary body",
        ])
        .assert()
        .success();
    rewrite_event(&source, &change_id, "verdict-recorded", |event| {
        assert!(event.as_object_mut().unwrap().remove("body").is_some());
    });

    let bundle = source.home.join("old-verdict.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "old-verdict",
            "--output",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let status = json_stdout(
        destination
            .arc(&destination.root)
            .args(["status", &change_id]),
    );
    assert_eq!(status["verdict"]["verdict"], "approved");
    assert!(!status["verdict"].as_object().unwrap().contains_key("body"));
}

#[test]
fn claim_events_without_generation_fields_are_rejected() {
    let repo = Repo::new();
    let opened =
        stdout(
            repo.arc(&repo.root)
                .args(["begin", "old-claim-generation", "--no-worktree"]),
        );
    let change_id = opened_change_id(&opened);
    repo.arc(&repo.root)
        .args(["claim", "old-claim-generation"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["stage", "old-claim-generation", "started"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["release-claim", "old-claim-generation"])
        .assert()
        .success();
    // The claim protocol shipped with generations from the start: the binary
    // always writes claim_id, so a claim event without one is corruption or a
    // forgery and must fail loud rather than replay through inference.
    rewrite_event(&repo, &change_id, "claim-set", |event| {
        event.as_object_mut().unwrap().remove("claim_id");
    });

    repo.arc(&repo.root)
        .args(["status", "old-claim-generation"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("malformed event file"));
}

#[test]
fn export_import_roundtrip_is_byte_identical() {
    let source = Repo::new();
    change_with_patchset(&source, "move-r");
    let bundle = source.home.join("bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-r", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let roundtrip = destination.home.join("roundtrip.json");
    destination
        .arc(&destination.root)
        .args(["export", "move-r", "--output", roundtrip.to_str().unwrap()])
        .assert()
        .success();

    assert_eq!(fs::read(bundle).unwrap(), fs::read(roundtrip).unwrap());

    // Continuing the change on the destination creates events with that
    // store's repository ID. Mixed provenance remains exportable/importable.
    destination
        .arc(&destination.root)
        .args(["hold", "move-r", "--reason", "continue elsewhere"])
        .assert()
        .success();
    let continued = destination.home.join("continued.json");
    destination
        .arc(&destination.root)
        .args(["export", "move-r", "--output", continued.to_str().unwrap()])
        .assert()
        .success();
    let third = Repo::new();
    third
        .arc(&third.root)
        .args(["import", continued.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn import_is_idempotent() {
    let source = Repo::new();
    change_with_patchset(&source, "move-i");
    let bundle = source.home.join("bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-i", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let event_count = serde_json::from_slice::<serde_json::Value>(&fs::read(&bundle).unwrap())
        .unwrap()["event_count"]
        .as_u64()
        .unwrap();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "summary: new=0 skipped={event_count} conflicts=0"
        )));
}

#[test]
fn import_conflict_writes_nothing() {
    let source = Repo::new();
    let (change_id, _, _) = change_with_patchset(&source, "move-c");
    let bundle = source.home.join("bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-c", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let bundle_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let event_id = bundle_json["events"][0]["event_id"].as_str().unwrap();
    let event_path = destination
        .root
        .join(".git/arc/changes")
        .join(&change_id)
        .join("events")
        .join(format!("{event_id}.json"));
    let mut tampered: serde_json::Value =
        serde_json::from_slice(&fs::read(&event_path).unwrap()).unwrap();
    tampered["actor"] = serde_json::Value::String("tampered".into());
    let tampered_bytes = json_file_bytes(&tampered);
    fs::write(&event_path, &tampered_bytes).unwrap();

    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(predicates::str::contains(format!("conflict: {event_id}")))
        .stdout(predicates::str::contains(
            "aborted: no events or refs written",
        ));
    assert_eq!(fs::read(event_path).unwrap(), tampered_bytes);
}

#[test]
fn import_rejects_malformed_known_events_before_writing() {
    let source = Repo::new();
    stdout(
        source
            .arc(&source.root)
            .args(["begin", "move-malformed", "--no-worktree"]),
    );
    source
        .arc(&source.root)
        .args(["claim", "move-malformed"])
        .assert()
        .success();
    source
        .arc(&source.root)
        .args(["stage", "move-malformed", "started"])
        .assert()
        .success();
    let bundle_path = source.home.join("malformed.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "move-malformed",
            "--output",
            bundle_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let mut bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle_path).unwrap()).unwrap();
    let original_bundle = bundle.clone();
    // A recognized tag must fail typed decoding when its payload is malformed;
    // it must never degrade to the opaque future-event path.
    let claim = bundle["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "claim-set")
        .unwrap();
    claim["ttl_seconds"] = serde_json::Value::String("not-seconds".into());
    refresh_bundle_checksum(&mut bundle);
    fs::write(&bundle_path, json_file_bytes(&bundle)).unwrap();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains(" is malformed"));
    assert!(!destination.root.join(".git/arc").exists());

    let mut malformed_envelope = original_bundle.clone();
    malformed_envelope["events"][0]["created_at"] = serde_json::Value::String("not-a-date".into());
    refresh_bundle_checksum(&mut malformed_envelope);
    let malformed_envelope_path = source.home.join("malformed-envelope.json");
    fs::write(
        &malformed_envelope_path,
        json_file_bytes(&malformed_envelope),
    )
    .unwrap();
    let malformed_envelope_destination = Repo::new();
    malformed_envelope_destination
        .arc(&malformed_envelope_destination.root)
        .args(["import", malformed_envelope_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains(" is malformed"));
    assert!(!malformed_envelope_destination
        .root
        .join(".git/arc")
        .exists());

    let mut ownerless = original_bundle;
    let stage = ownerless["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "stage-set")
        .unwrap();
    stage.as_object_mut().unwrap().remove("session");
    refresh_bundle_checksum(&mut ownerless);
    let ownerless_path = source.home.join("ownerless-stage.json");
    fs::write(&ownerless_path, json_file_bytes(&ownerless)).unwrap();
    let ownerless_destination = Repo::new();
    ownerless_destination
        .arc(&ownerless_destination.root)
        .args(["import", ownerless_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("has no session"));
    assert!(!ownerless_destination.root.join(".git/arc").exists());
}

#[test]
fn import_rejects_malformed_changelog_before_writing() {
    let source = Repo::new();
    let (_, worktree, _) = change_with_patchset(&source, "move-changelog");
    let changelog_body = source.home.join("changelog-body.txt");
    fs::write(&changelog_body, "Reject malformed changelog events.\n").unwrap();
    source
        .arc(&worktree)
        .args([
            "changelog",
            "move-changelog",
            "--category",
            "fixed",
            "--body-file",
            changelog_body.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bundle_path = source.home.join("malformed-changelog.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "move-changelog",
            "--output",
            bundle_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let mut bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle_path).unwrap()).unwrap();
    let changelog = bundle["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "changelog-recorded")
        .unwrap();
    assert!(changelog.as_object_mut().unwrap().remove("body").is_some());
    refresh_bundle_checksum(&mut bundle);
    fs::write(&bundle_path, json_file_bytes(&bundle)).unwrap();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains(" is malformed"));
    assert!(!destination.root.join(".git/arc").exists());
}

#[test]
fn import_replays_combined_history_before_writing() {
    let source = Repo::new();
    let opened = stdout(
        source
            .arc(&source.root)
            .args(["begin", "move-combined", "--no-worktree"]),
    );
    let change_id = opened_change_id(&opened);
    let bundle_path = source.home.join("combined.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "move-combined",
            "--output",
            bundle_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle_path).unwrap()).unwrap();
    let bundled_open = bundle["events"][0].clone();
    let bundled_event_id = bundled_open["event_id"].as_str().unwrap().to_string();

    let destination = Repo::new();
    stdout(
        destination
            .arc(&destination.root)
            .args(["begin", "seed-store", "--no-worktree"]),
    );
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(destination.root.join(".git/arc/config.json")).unwrap())
            .unwrap();
    let mut local_open = bundled_open;
    local_open["event_id"] = serde_json::Value::String("00000000000000000000000000".into());
    local_open["repository_id"] = config["repository_id"].clone();
    let local_dir = event_dir(&destination, &change_id);
    fs::create_dir_all(&local_dir).unwrap();
    fs::write(
        local_dir.join("00000000000000000000000000.json"),
        json_file_bytes(&local_open),
    )
    .unwrap();

    destination
        .arc(&destination.root)
        .args(["import", bundle_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "combined local and bundled known events are not replayable",
        ));
    assert!(!local_dir.join(format!("{bundled_event_id}.json")).exists());
}

#[test]
fn stale_imported_release_stage_and_snapshot_cannot_mutate_a_replacement_claim() {
    let source = Repo::new();
    let opened = stdout(source.arc(&source.root).args(["begin", "move-stale-claim"]));
    let change_id = opened_change_id(&opened);
    let worktree = source.home.join(".worktrees/repo-move-stale-claim");
    source
        .arc(&worktree)
        .env("ARC_ACTOR", "Source Executor")
        .args(["claim", "move-stale-claim"])
        .assert()
        .success();
    let initial = source.home.join("initial.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "move-stale-claim",
            "--output",
            initial.to_str().unwrap(),
        ])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", initial.to_str().unwrap()])
        .assert()
        .success();
    destination
        .arc(&destination.root)
        .env("ARC_SESSION", "lead")
        .args(["release-claim", "move-stale-claim"])
        .assert()
        .success();
    destination
        .arc(&destination.root)
        .env("ARC_ACTOR", "Replacement Executor")
        .env("ARC_SESSION", "replacement-session")
        .args(["claim", "move-stale-claim"])
        .assert()
        .success();
    let replacement: serde_json::Value = serde_json::from_str(&stdout(
        destination
            .arc(&destination.root)
            .args(["status", "move-stale-claim"]),
    ))
    .unwrap();
    let replacement_claim_id = replacement["claim"]["claim_id"].clone();

    thread::sleep(Duration::from_millis(5));
    source
        .arc(&worktree)
        .env("ARC_ACTOR", "Source Executor")
        .args(["stage", "move-stale-claim", "started"])
        .assert()
        .success();
    source.commit(
        &worktree,
        "stale.txt",
        "source snapshot\n",
        "test: snapshot source claim",
    );
    source
        .arc(&worktree)
        .args(["snapshot", "move-stale-claim", "--solo"])
        .assert()
        .success();
    source
        .arc(&source.root)
        .env("ARC_SESSION", "source-lead")
        .args(["release-claim", "move-stale-claim"])
        .assert()
        .success();
    let updated = source.home.join("updated.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "move-stale-claim",
            "--output",
            updated.to_str().unwrap(),
        ])
        .assert()
        .success();

    destination
        .arc(&destination.root)
        .args(["import", updated.to_str().unwrap()])
        .assert()
        .success();
    let status: serde_json::Value = serde_json::from_str(&stdout(
        destination
            .arc(&destination.root)
            .args(["status", "move-stale-claim"]),
    ))
    .unwrap();
    assert_eq!(status["claim"]["claim_id"], replacement_claim_id);
    assert_eq!(status["claim"]["owner"]["actor"], "Replacement Executor");
    assert_eq!(status["claim"]["stage"], "launch");
    assert!(status["claim"]["snapshot_author"].is_null());

    let state: serde_json::Value = serde_json::from_str(&stdout(
        destination
            .arc(&destination.root)
            .args(["show", &change_id, "--json"]),
    ))
    .unwrap();
    let patchset = state["patchsets"].as_array().unwrap().last().unwrap();
    assert_eq!(patchset["claim_actor"], "Source Executor");
    assert_ne!(patchset["claim_id"], replacement_claim_id);
}

#[test]
fn import_dry_run_into_fresh_repo_writes_nothing() {
    let source = Repo::new();
    change_with_patchset(&source, "move-p");
    let bundle = source.home.join("bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-p", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let event_count = serde_json::from_slice::<serde_json::Value>(&fs::read(&bundle).unwrap())
        .unwrap()["event_count"]
        .as_u64()
        .unwrap();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap(), "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "summary: new={event_count} skipped=0 conflicts=0"
        )))
        .stdout(predicates::str::contains(
            "dry-run: no events or refs written",
        ));
    assert!(!destination.root.join(".git/arc").exists());
}

#[test]
fn import_restores_patchset_retention_refs() {
    let repo = Repo::new();
    let (change_id, _, head) = change_with_patchset(&repo, "move-k");
    let bundle = repo.home.join("bundle.json");
    repo.arc(&repo.root)
        .args(["export", "move-k", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let change_dir = repo.root.join(".git/arc/changes").join(&change_id);
    fs::remove_dir_all(change_dir).unwrap();
    let retention_ref = format!("refs/arc/keep/{change_id}/ps-01");
    git(&repo.root, &["update-ref", "-d", &retention_ref]);

    repo.arc(&repo.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(git_out(&repo.root, &["rev-parse", &retention_ref]), head);
}

#[test]
fn import_preserves_unknown_event_bytes() {
    let source = Repo::new();
    let out = stdout(
        source
            .arc(&source.root)
            .args(["begin", "move-u", "--no-worktree"]),
    );
    let change_id = out
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(source.root.join(".git/arc/config.json")).unwrap())
            .unwrap();
    let event_id = "ZZZZZZZZZZZZZZZZZZZZZZZZZZ";
    // A complete envelope with an unrecognized tag pins the opaque side of the
    // classifier partition while remaining readable by typed consumers.
    let unknown = serde_json::json!({
        "schema_version": 1,
        "event_id": event_id,
        "repository_id": config["repository_id"],
        "change_id": change_id,
        "actor": "future-agent",
        "created_at": "2026-07-16T00:00:00Z",
        "event_type": "future-thing",
        "future_payload": {"kept": [1, 2, 3], "nested": true}
    });
    let source_event = source
        .root
        .join(".git/arc/changes")
        .join(change_id)
        .join("events")
        .join(format!("{event_id}.json"));
    let unknown_bytes = json_file_bytes(&unknown);
    fs::write(&source_event, &unknown_bytes).unwrap();
    let bundle = source.home.join("bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-u", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "unknown event type: {event_id} future-thing"
        )));
    let destination_event = destination
        .root
        .join(".git/arc/changes")
        .join(change_id)
        .join("events")
        .join(format!("{event_id}.json"));
    assert_eq!(fs::read(destination_event).unwrap(), unknown_bytes);

    let streamed = stdout(destination.arc(&destination.root).args([
        "events",
        "--change",
        "move-u",
        "--type",
        "future-thing",
    ]));
    let streamed: serde_json::Value = serde_json::from_str(streamed.trim()).unwrap();
    assert_eq!(streamed, unknown);
}

#[test]
fn export_import_roundtrips_message_events() {
    let source = Repo::new();
    change_with_patchset(&source, "msg-move");
    source
        .arc(&source.root)
        .args([
            "message",
            "msg-move",
            "--type",
            "status",
            "--summary",
            "portable announcement",
            "--json",
            "{\"k\":\"v\"}",
        ])
        .assert()
        .success();
    let bundle = source.home.join("bundle.json");
    source
        .arc(&source.root)
        .args(["export", "msg-move", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let roundtrip = destination.home.join("roundtrip.json");
    destination
        .arc(&destination.root)
        .args([
            "export",
            "msg-move",
            "--output",
            roundtrip.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert_eq!(fs::read(&bundle).unwrap(), fs::read(&roundtrip).unwrap());

    let messages = json_stdout(
        destination
            .arc(&destination.root)
            .args(["messages", "--json"]),
    );
    let messages = messages.as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["summary"], "portable announcement");
    assert_eq!(messages[0]["metadata"]["k"], "v");
}

#[test]
fn export_import_preserves_plan_links_on_every_brief_version() {
    let source = Repo::new();
    let base_revision = source.head(&source.root);
    stdout(
        source
            .arc(&source.root)
            .args(["begin", "brief-bundle", "--no-worktree"]),
    );
    let mut plans = Vec::new();
    for topic in ["portable-first", "portable-second"] {
        let path = stdout(
            source
                .arc(&source.root)
                .args([
                    "journal",
                    "note",
                    topic,
                    "--kind",
                    "plan",
                    "--body-file",
                    "-",
                ])
                .write_stdin("# Plan\n"),
        );
        plans.push(
            Path::new(path.trim())
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
    }
    for (index, (plan_ref, plan_slice, body)) in [
        (&plans[0], "first-slice", "first contract\n"),
        (&plans[1], "second-slice", "second contract\n"),
    ]
    .into_iter()
    .enumerate()
    {
        let first = index == 0;
        source
            .arc(&source.root)
            .args([
                "brief",
                "brief-bundle",
                "--body-file",
                "-",
                "--plan-ref",
                plan_ref,
                "--plan-slice",
                plan_slice,
            ])
            .args(if first {
                vec![]
            } else {
                vec!["--cause-note", "fixture revision"]
            })
            .write_stdin(body)
            .assert()
            .success();
    }

    let bundle = source.home.join("brief-bundle.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "brief-bundle",
            "--output",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    for (version, plan_ref, plan_slice, body) in [
        (1, &plans[0], "first-slice", "first contract\n"),
        (2, &plans[1], "second-slice", "second contract\n"),
    ] {
        destination
            .arc(&destination.root)
            .args(["brief", "brief-bundle", "--version", &version.to_string()])
            .assert()
            .success()
            .stdout(format!(
                "base-revision: {base_revision}\nplan-ref: {plan_ref}\nplan-slice: {plan_slice}\n\n{body}"
            ));
    }
}
