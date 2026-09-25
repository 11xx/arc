use super::common::*;

/// Replica commands must identify each local store before two independently
/// initialized repositories can exchange pairing records.
#[test]
fn independent_stores_expose_their_replica_identity() {
    let source = Repo::new();
    let recipient = Repo::new();

    let source_output = source
        .arc(&source.root)
        .args(["replica", "id", "--json"])
        .output()
        .unwrap();
    let recipient_output = recipient
        .arc(&recipient.root)
        .args(["replica", "id", "--json"])
        .output()
        .unwrap();

    assert!(
        source_output.status.success(),
        "source replica identity failed: {}",
        String::from_utf8_lossy(&source_output.stderr)
    );
    assert!(
        recipient_output.status.success(),
        "recipient replica identity failed: {}",
        String::from_utf8_lossy(&recipient_output.stderr)
    );
    let source_identity: serde_json::Value = serde_json::from_slice(&source_output.stdout).unwrap();
    let recipient_identity: serde_json::Value =
        serde_json::from_slice(&recipient_output.stdout).unwrap();
    assert_eq!(source_identity["schema"], "arc-replica-id/1");
    assert_eq!(recipient_identity["schema"], "arc-replica-id/1");
    assert_ne!(
        source_identity["repository_id"],
        recipient_identity["repository_id"]
    );
}

/// A path in an imported change event describes its recording replica and
/// cannot resolve to a checkout in the importing replica.
#[test]
fn imported_change_paths_remain_machine_local() {
    let source = Repo::new();
    let recipient = Repo::new();

    let opened = stdout(source.arc(&source.root).args(["begin", "replica-path"]));
    let change_id = opened_change_id(&opened);
    let bundle = source.home.join("change.json");
    source
        .arc(&source.root)
        .args(["export", &change_id, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    recipient
        .arc(&recipient.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let events = stdout(
        recipient
            .arc(&recipient.root)
            .args(["events", "--change", &change_id]),
    );
    assert!(
        events.contains(
            source
                .home
                .join(".worktrees/repo-replica-path")
                .to_string_lossy()
                .as_ref()
        ),
        "the imported source event lost its recorded path: {events}"
    );
    let status = json_stdout(
        recipient
            .arc(&recipient.root)
            .args(["status", &change_id, "--json"]),
    );
    assert_eq!(status["worktree"], serde_json::Value::Null, "{status}");
    assert!(
        !status.to_string().contains(
            source
                .home
                .join(".worktrees/repo-replica-path")
                .to_string_lossy()
                .as_ref()
        ),
        "a remote checkout path reached the local status: {status}"
    );
}

/// Pairing gives the initial replica the sole authority. An offer relinquishes
/// it until its named recipient imports the file, and a repeated import does
/// not create another receipt or acquisition.
#[test]
fn authority_moves_only_through_an_imported_offer() {
    let source = Repo::new();
    let recipient = Repo::new();
    let offline_peer = Repo::new();
    let recipient_id = repository_id(&recipient);
    let offline_peer_id = repository_id(&offline_peer);

    source
        .arc(&source.root)
        .args(["replica", "init", "origin"])
        .assert()
        .success();
    source
        .arc(&source.root)
        .args(["replica", "authority", "offer", "--to", "origin"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("itself"));
    source
        .arc(&source.root)
        .args(["replica", "pair", "peer", "--repository-id", &recipient_id])
        .assert()
        .success();
    source
        .arc(&source.root)
        .args([
            "replica",
            "pair",
            "offline",
            "--repository-id",
            &offline_peer_id,
        ])
        .assert()
        .success();
    let pairing = source.home.join("pairing.json");
    export_replica(&source, &pairing);
    recipient
        .arc(&recipient.root)
        .args(["replica", "import", pairing.to_str().unwrap(), "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains("dry-run: would import"));
    assert!(!recipient.root.join(".git/arc/replicas/imports").exists());
    import_replica(&recipient, &pairing);
    import_replica(&offline_peer, &pairing);

    let source_status = replica_status(&source);
    assert_eq!(source_status["schema"], "arc-replica/1");
    assert_eq!(source_status["local"]["name"], "origin", "{source_status}");
    assert!(
        source_status["peers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|peer| peer["name"] == "peer"),
        "{source_status}"
    );

    let catchup = json_stdout(recipient.arc(&recipient.root).args(["catchup", "--json"]));
    assert_eq!(catchup["schema"], "arc-catchup/10", "{catchup}");
    assert_eq!(catchup["replica"]["local"]["name"], "peer", "{catchup}");
    assert!(
        catchup["replica"]["peers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|peer| peer["name"] == "origin"),
        "{catchup}"
    );
    assert_eq!(catchup["replica"]["authority"]["holder"]["name"], "origin");
    let doctor = json_stdout(recipient.arc(&recipient.root).args(["doctor", "--json"]));
    assert_eq!(doctor["schema"], "arc-doctor/4", "{doctor}");
    assert_eq!(doctor["replica"]["local"]["name"], "peer", "{doctor}");

    let _ = change_with_patchset(&recipient, "authority-work");
    recipient
        .arc(&recipient.root)
        .args(["review", "authority-work", "--verdict", "approved"])
        .assert()
        .success();
    let source_change = begin_change(&source, "origin-work", None);
    let offline_change = begin_change(&offline_peer, "offline-work", None);
    let source_head = source.head(&source.root);
    let recipient_head = recipient.head(&recipient.root);

    recipient
        .arc(&recipient.root)
        .args(["integrate", "authority-work"])
        .assert()
        .code(17)
        .stderr(predicates::str::contains("origin"));
    offline_peer
        .arc(&offline_peer.root)
        .args(["integrate", &offline_change])
        .assert()
        .code(17)
        .stderr(predicates::str::contains("origin"));
    offline_peer
        .arc(&offline_peer.root)
        .args([
            "replica",
            "authority",
            "reclaim",
            "--because",
            "not the offerer",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("offer"));
    offline_peer
        .arc(&offline_peer.root)
        .args(["replica", "init", "second-project"])
        .assert()
        .failure();

    source
        .arc(&source.root)
        .args(["replica", "authority", "offer", "--to", "peer"])
        .assert()
        .success();
    let offer = source.home.join("offer.json");
    export_replica(&source, &offer);
    let in_flight = replica_status(&source);
    assert!(in_flight["authority"]["holder"].is_null(), "{in_flight}");
    assert_eq!(
        in_flight["authority"]["offer_in_flight"]["to"]["name"],
        "peer"
    );
    let catchup = json_stdout(source.arc(&source.root).args(["catchup", "--json"]));
    assert!(
        catchup["replica"]["authority"]["holder"].is_null(),
        "{catchup}"
    );
    assert_eq!(
        catchup["replica"]["authority"]["offer_in_flight"]["to"]["name"], "peer",
        "{catchup}"
    );
    let doctor = json_stdout(source.arc(&source.root).args(["doctor", "--json"]));
    assert!(
        doctor["replica"]["authority"]["holder"].is_null(),
        "{doctor}"
    );
    assert_eq!(
        doctor["replica"]["authority"]["offer_in_flight"]["to"]["name"], "peer",
        "{doctor}"
    );
    let catchup_text = stdout(source.arc(&source.root).arg("catchup"));
    assert!(
        catchup_text.contains("integration authority: offer in flight from origin to peer"),
        "{catchup_text}"
    );
    let doctor_text = stdout(source.arc(&source.root).arg("doctor"));
    assert!(
        doctor_text.contains("integration authority: offer in flight from origin to peer"),
        "{doctor_text}"
    );
    source
        .arc(&source.root)
        .args(["integrate", &source_change])
        .assert()
        .code(17)
        .stderr(predicates::str::contains("peer"));
    recipient
        .arc(&recipient.root)
        .args(["integrate", "authority-work"])
        .assert()
        .code(17)
        .stderr(predicates::str::contains("origin"));
    assert_eq!(source.head(&source.root), source_head);
    assert_eq!(recipient.head(&recipient.root), recipient_head);

    import_replica(&recipient, &offer);
    let imports = replica_import_count(&recipient);
    let acquired = replica_status(&recipient);
    assert_eq!(
        acquired["authority"]["holder"]["name"], "peer",
        "{acquired}"
    );
    assert!(
        acquired["authority"]["offer_in_flight"].is_null(),
        "{acquired}"
    );
    recipient
        .arc(&recipient.root)
        .args(["integrate", "authority-work"])
        .assert()
        .success();
    recipient
        .arc(&recipient.root)
        .args(["replica", "import", offer.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(replica_import_count(&recipient), imports);
    assert_eq!(
        replica_status(&recipient)["authority"]["holder"]["name"],
        "peer"
    );

    let acknowledgement = recipient.home.join("acknowledgement.json");
    export_replica(&recipient, &acknowledgement);
    import_replica(&source, &acknowledgement);
    assert_eq!(
        replica_status(&source)["authority"]["holder"]["name"],
        "peer"
    );

    recipient
        .arc(&recipient.root)
        .args(["replica", "authority", "offer", "--to", "origin"])
        .assert()
        .success();
    let return_offer = recipient.home.join("return.json");
    export_replica(&recipient, &return_offer);
    import_replica(&source, &return_offer);
    assert_eq!(
        replica_status(&source)["authority"]["holder"]["name"],
        "origin"
    );
    let source_offer = source.home.join("source-offer.json");
    source
        .arc(&source.root)
        .args(["replica", "authority", "offer", "--to", "peer"])
        .assert()
        .success();
    export_replica(&source, &source_offer);
    import_replica(&recipient, &source_offer);
    assert_eq!(
        replica_status(&recipient)["authority"]["holder"]["name"],
        "peer"
    );

    source
        .arc(&source.root)
        .args([
            "replica",
            "authority",
            "reclaim",
            "--because",
            "The handoff file was reported missing.",
        ])
        .assert()
        .success();
    let reclaimed = source.home.join("reclaimed.json");
    export_replica(&source, &reclaimed);
    import_replica(&recipient, &reclaimed);
    let after_reclaim = replica_status(&recipient);
    assert_eq!(after_reclaim["authority"]["holder"]["name"], "origin");
    assert_eq!(
        after_reclaim["authority"]["last_reclaim"]["reason"],
        "The handoff file was reported missing."
    );
    // The peer had already acquired that offer, so both replicas held
    // authority for a while. The reclaim decides the holder; the overlap is
    // reported on both sides rather than disappearing.
    let contest = &after_reclaim["authority"]["contested"][0];
    assert_eq!(contest["acquired_by"]["name"], "peer");
    assert_eq!(contest["reclaimed_by"]["name"], "origin");
    let acknowledgement = recipient.home.join("contest-ack.json");
    export_replica(&recipient, &acknowledgement);
    import_replica(&source, &acknowledgement);
    let source_view = replica_status(&source);
    assert_eq!(source_view["authority"]["holder"]["name"], "origin");
    assert_eq!(
        source_view["authority"]["contested"][0]["acquired_by"]["name"],
        "peer"
    );
    recipient
        .arc(&recipient.root)
        .args(["replica", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("contested: peer acquired offer"));
}

/// An imported live claim cannot displace a different live local claim. The
/// complete change bundle is refused and neither claim is rewritten.
#[test]
fn importing_a_contested_live_claim_writes_nothing() {
    let source = Repo::new();
    let recipient = Repo::new();
    let recipient_id = repository_id(&recipient);
    source
        .arc(&source.root)
        .args(["replica", "init", "origin"])
        .assert()
        .success();
    source
        .arc(&source.root)
        .args(["replica", "pair", "peer", "--repository-id", &recipient_id])
        .assert()
        .success();
    let pairing = source.home.join("pairing.json");
    export_replica(&source, &pairing);
    import_replica(&recipient, &pairing);

    let change_id = begin_change(&source, "contested-claim", None);
    let bundle = source.home.join("change.json");
    source
        .arc(&source.root)
        .args(["export", &change_id, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    recipient
        .arc(&recipient.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();

    recipient
        .arc(&recipient.root)
        .env("ARC_ACTOR", "peer-executor")
        .args(["claim", &change_id, "--ttl", "30m"])
        .assert()
        .success();
    source
        .arc(&source.root)
        .env("ARC_ACTOR", "origin-executor")
        .args(["claim", &change_id, "--ttl", "30m"])
        .assert()
        .success();
    source
        .arc(&source.root)
        .args(["export", &change_id, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let before = event_count(&recipient, &change_id);
    recipient
        .arc(&recipient.root)
        .args(["import", bundle.to_str().unwrap(), "--dry-run"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("claim contest"));
    assert_eq!(event_count(&recipient, &change_id), before);
    recipient
        .arc(&recipient.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("claim contest"))
        .stdout(predicates::str::contains("origin"))
        .stdout(predicates::str::contains("peer"));
    assert_eq!(event_count(&recipient, &change_id), before);
    let status = json_stdout(
        recipient
            .arc(&recipient.root)
            .args(["status", &change_id, "--json"]),
    );
    assert_eq!(
        status["claim"]["owner"]["actor"], "peer-executor",
        "{status}"
    );
}

fn repository_id(repo: &Repo) -> String {
    let output = repo
        .arc(&repo.root)
        .args(["replica", "id", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "replica id failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let identity: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    identity["repository_id"].as_str().unwrap().to_string()
}

fn export_replica(repo: &Repo, path: &Path) {
    repo.arc(&repo.root)
        .args(["replica", "export", "--output", path.to_str().unwrap()])
        .assert()
        .success();
}

fn import_replica(repo: &Repo, path: &Path) {
    repo.arc(&repo.root)
        .args(["replica", "import", path.to_str().unwrap()])
        .assert()
        .success();
}

fn replica_status(repo: &Repo) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["replica", "status", "--json"]))
}

fn replica_import_count(repo: &Repo) -> usize {
    fs::read_dir(repo.root.join(".git/arc/replicas/imports"))
        .unwrap()
        .count()
}
