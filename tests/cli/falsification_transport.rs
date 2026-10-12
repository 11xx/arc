use super::common::*;

struct Fixture {
    source: Repo,
    change: String,
    old: String,
    new: String,
    prefix: PathBuf,
    full: PathBuf,
    delta: PathBuf,
}

fn export(repo: &Repo, change: &str, path: &Path, since: Option<&str>) {
    let mut cmd = repo.arc(&repo.root);
    cmd.args(["export", change, "--output", path.to_str().unwrap()]);
    if let Some(since) = since {
        cmd.args(["--since", since]);
    }
    cmd.assert().success();
}

fn read(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn fixture() -> Fixture {
    let source = Repo::new();
    source.declare_gates_locally("[gates.fixable]\ncommand='test -f marker'\n");
    let change = opened_change_id(&stdout(source.arc(&source.root).args([
        "begin",
        "projected",
        "--no-worktree",
    ])));
    source.commit(&source.root, "value", "broken", "test: broken value");
    let old = source.head(&source.root);
    git(
        &source.root,
        &["update-ref", "refs/heads/failure-evidence", &old],
    );
    source
        .arc(&source.root)
        .args(["verify", &change, "--gate", "fixable"])
        .assert()
        .code(1);
    let prefix = source.home.join("prefix.json");
    export(&source, &change, &prefix, None);
    fs::write(source.root.join("marker"), "fixed").unwrap();
    git(&source.root, &["add", "marker"]);
    git(&source.root, &["commit", "--amend", "--no-edit"]);
    let new = source.head(&source.root);
    let map = source.home.join("map");
    fs::write(&map, format!("{old} {new}\n")).unwrap();
    source
        .arc(&source.root)
        .args([
            "history",
            "rewrite",
            "--map",
            map.to_str().unwrap(),
            "--reason",
            "commit amendment",
        ])
        .assert()
        .success();
    let failure = read(&prefix)["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["result"] == "fail")
        .unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    source
        .arc(&source.root)
        .args([
            "verify",
            &change,
            "--gate",
            "fixable",
            "--falsified-by",
            &failure,
            "--predicted",
            "marker absent",
        ])
        .assert()
        .success();
    rewrite_event(&source, &change, "verification-recorded", |event| {
        if event["result"] == "pass" {
            event["falsification"]["revision"] = new.clone().into();
        }
    });
    let full = source.home.join("full.json");
    let delta = source.home.join("delta.json");
    export(&source, &change, &full, None);
    export(
        &source,
        &change,
        &delta,
        read(&prefix)["events_sha256"].as_str(),
    );
    Fixture {
        source,
        change,
        old,
        new,
        prefix,
        full,
        delta,
    }
}

fn enroll(receiver: &Repo, bundle: &serde_json::Value, rewrite: bool) {
    let source = bundle["source_replica_id"].as_str().unwrap();
    let roles = if rewrite {
        "['facts','rewrite']"
    } else {
        "['facts']"
    };
    receiver.declare_gates_locally(&format!(
        "[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{source}'\n[[replica.publishers.origins]]\nreplica_id='{source}'\nroles={roles}\nactors=[]\n"
    ));
}

fn receive(receiver: &Repo, path: &Path, dry_run: bool) -> AssertCommand {
    let mut cmd = receiver.arc(&receiver.root);
    cmd.args([
        "import",
        path.to_str().unwrap(),
        "--publisher",
        "channel",
        "--received-sha256",
        &hex::encode(Sha256::digest(fs::read(path).unwrap())),
    ]);
    if dry_run {
        cmd.arg("--dry-run");
    }
    cmd
}

fn receiver(f: &Fixture, delta: bool) -> Repo {
    let receiver = Repo::new();
    deliver_git_objects(&f.source, &receiver);
    enroll(&receiver, &read(&f.full), true);
    if delta {
        receive(&receiver, &f.prefix, false).assert().success();
    }
    receiver
}

#[test]
fn projected_falsification_full_and_delta_transport_preserves_events() {
    let f = fixture();
    let recorded = read(&f.full);
    assert_eq!(recorded["schema"], "arc-bundle/6");
    for delta in [false, true] {
        let receiver = receiver(&f, delta);
        let path = if delta { &f.delta } else { &f.full };
        let before = receiver.root.join(".git/arc/imports");
        let count = fs::read_dir(&before)
            .map(|entries| entries.count())
            .unwrap_or(0);
        receive(&receiver, path, true).assert().success();
        assert_eq!(
            fs::read_dir(&before)
                .map(|entries| entries.count())
                .unwrap_or(0),
            count
        );
        receive(&receiver, path, false).assert().success();
        let imported = receiver.home.join("imported.json");
        export(&receiver, &f.change, &imported, None);
        assert_eq!(read(&imported)["events"], recorded["events"]);
        receiver
            .arc(&receiver.root)
            .args(["show", &f.change, "--json"])
            .assert()
            .success();
        let report = json_stdout(receiver.arc(&receiver.root).args(["doctor", "--json"]));
        assert_eq!(report["problems"], serde_json::json!([]), "{report}");
    }
}

#[test]
fn projected_falsification_transport_refuses_invalid_proof() {
    let f = fixture();
    for delta in [false, true] {
        for kind in [
            "absent",
            "withdrawn",
            "contradictory",
            "unrelated",
            "convergent",
        ] {
            let receiver = receiver(&f, delta);
            let mut bundle = read(if delta { &f.delta } else { &f.full });
            let rewrite = bundle["repository_events"][0].clone();
            let mut other = rewrite.clone();
            other["event_id"] = "ZZZZ-proof".into();
            let third = "3333333333333333333333333333333333333333";
            match kind {
                "absent" => bundle["repository_events"] = serde_json::json!([]),
                "withdrawn" => {
                    other["event_type"] = "history-rewrite-withdrawn".into();
                    other["rewrite_event_id"] = rewrite["event_id"].clone();
                    other["reason"] = "withdraw proof".into();
                    bundle["repository_events"]
                        .as_array_mut()
                        .unwrap()
                        .push(other);
                }
                "contradictory" => {
                    other["mapping"] = serde_json::json!({f.old.clone(): third});
                    bundle["repository_events"]
                        .as_array_mut()
                        .unwrap()
                        .push(other);
                }
                "unrelated" => {
                    bundle["repository_events"][0]["mapping"] = serde_json::json!({third: f.new})
                }
                "convergent" => {
                    bundle["repository_events"][0]["mapping"] =
                        serde_json::json!({f.old.clone(): third, f.new.clone(): third})
                }
                _ => unreachable!(),
            }
            bundle["objects"] = serde_json::json!([]);
            let path = f.source.home.join(format!("{kind}-{delta}.json"));
            fs::write(&path, json_file_bytes(&bundle)).unwrap();
            for dry_run in [true, false] {
                let reason = if kind == "contradictory" {
                    "rewrites disagree"
                } else {
                    "was recorded at"
                };
                receive(&receiver, &path, dry_run)
                    .assert()
                    .code(1)
                    .stderr(predicates::str::contains(reason));
            }
            assert_eq!(
                fs::read_dir(receiver.root.join(".git/arc/imports"))
                    .map(|entries| entries.count())
                    .unwrap_or(0),
                usize::from(delta)
            );
        }
    }
}

#[test]
fn projected_falsification_requires_publisher_rewrite_authority() {
    let f = fixture();
    for delta in [false, true] {
        let receiver = receiver(&f, delta);
        enroll(&receiver, &read(&f.full), false);
        for dry_run in [true, false] {
            receive(&receiver, if delta { &f.delta } else { &f.full }, dry_run)
                .assert()
                .code(18)
                .stderr(predicates::str::contains("Rewrite"));
        }
    }
    let receiver = Repo::new();
    deliver_git_objects(&f.source, &receiver);
    for dry_run in [true, false] {
        let mut cmd = receiver.arc(&receiver.root);
        cmd.args(["import", f.full.to_str().unwrap()]);
        if dry_run {
            cmd.arg("--dry-run");
        }
        cmd.assert()
            .code(1)
            .stderr(predicates::str::contains("was recorded at"));
    }
}

#[test]
fn projected_falsification_doctor_keeps_recorded_revision_diagnostics() {
    let f = fixture();
    let before = read(&f.full)["events"].clone();
    let report = json_stdout(f.source.arc(&f.source.root).args(["doctor", "--json"]));
    assert_eq!(report["problems"], serde_json::json!([]), "{report}");
    rewrite_event(&f.source, &f.change, "verification-recorded", |event| {
        if event["result"] == "pass" {
            event["revision"] = "4444444444444444444444444444444444444444".into();
        }
    });
    let report = json_stdout_any_status(f.source.arc(&f.source.root).args(["doctor", "--json"]));
    assert!(
        report["advice"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "dangling-revision"),
        "{report}"
    );
    assert!(
        !report["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "unreplayable-change"),
        "{report}"
    );
    assert_eq!(read(&f.full)["events"], before);
}

#[test]
fn projected_falsification_peer_claim_contest_uses_admitted_proof() {
    let f = fixture();
    let receiver = Repo::new();
    let id = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "id", "--json"]),
    );
    f.source
        .arc(&f.source.root)
        .args(["replica", "init", "origin"])
        .assert()
        .success();
    f.source
        .arc(&f.source.root)
        .args([
            "replica",
            "pair",
            "peer",
            "--repository-id",
            id["repository_id"].as_str().unwrap(),
        ])
        .assert()
        .success();
    let pairing = f.source.home.join("pairing.json");
    f.source
        .arc(&f.source.root)
        .args(["replica", "export", "--output", pairing.to_str().unwrap()])
        .assert()
        .success();
    let bundle = read(&pairing);
    let origin = bundle["source_replica_id"].as_str().unwrap();
    receiver.declare_gates_locally(&format!(
        "[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{origin}'\n[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles=['authority']\nactors=['tester']\n"
    ));
    receiver
        .arc(&receiver.root)
        .args([
            "replica",
            "import",
            pairing.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &hex::encode(Sha256::digest(fs::read(&pairing).unwrap())),
        ])
        .assert()
        .success();
    export(&f.source, &f.change, &f.full, None);
    enroll(&receiver, &read(&f.full), true);
    deliver_git_objects(&f.source, &receiver);
    receive(&receiver, &f.full, false).assert().success();
    receiver
        .arc(&receiver.root)
        .env("ARC_ACTOR", "peer-executor")
        .args(["claim", &f.change, "--ttl", "30m"])
        .assert()
        .success();
    f.source
        .arc(&f.source.root)
        .env("ARC_ACTOR", "origin-executor")
        .args(["claim", &f.change, "--ttl", "30m"])
        .assert()
        .success();
    let token = read(&f.full)["events_sha256"].as_str().unwrap().to_string();
    export(&f.source, &f.change, &f.delta, Some(&token));
    export(&f.source, &f.change, &f.full, None);
    for path in [&f.full, &f.delta] {
        for dry_run in [true, false] {
            receive(&receiver, path, dry_run)
                .assert()
                .code(1)
                .stdout(predicates::str::contains("claim contest"));
        }
    }
    let state = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["show", &f.change, "--json"]),
    );
    assert_eq!(state["claim"]["owner"]["actor"], "peer-executor");
}

/// Rewrite the latest repository event of `event_type` in place.
fn rewrite_repository_event(
    repo: &Repo,
    event_type: &str,
    update: impl FnOnce(&mut serde_json::Value),
) {
    let dir = repo.root.join(".git/arc/repository/events");
    let mut paths = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    let (path, mut value) = paths
        .into_iter()
        .rev()
        .map(|path| {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            (path, value)
        })
        .find(|(_, value)| value["event_type"] == event_type)
        .expect("repository event type should exist");
    update(&mut value);
    fs::write(path, json_file_bytes(&value)).unwrap();
}

#[test]
fn doctor_reports_a_falsification_revision_that_identity_recovery_replaces() {
    let f = fixture();
    let missing = "5555555555555555555555555555555555555555";
    // The recorded map moves the failure's revision to a commit this
    // repository does not hold, and the falsification names that successor.
    // Identity recovery reads the falsification as the failure's own revision,
    // so only the recorded reference still names the missing commit.
    rewrite_repository_event(&f.source, "history-rewritten", |event| {
        event["mapping"] = serde_json::json!({ f.old.clone(): missing });
    });
    rewrite_event(&f.source, &f.change, "verification-recorded", |event| {
        if event["result"] == "pass" {
            event["falsification"]["revision"] = missing.into();
        }
    });
    let report = json_stdout_any_status(f.source.arc(&f.source.root).args(["doctor", "--json"]));
    let dangling: Vec<_> = report["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["code"] == "dangling-revision")
        .collect();
    assert!(
        dangling.iter().any(|finding| {
            let detail = finding["detail"].as_str().unwrap();
            detail.starts_with("55555555") && detail.contains("falsification")
        }),
        "{report}"
    );
    let text = String::from_utf8_lossy(
        &f.source
            .arc(&f.source.root)
            .arg("doctor")
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(text.contains("55555555"), "{text}");
}
