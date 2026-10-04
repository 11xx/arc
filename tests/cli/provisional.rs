use super::common::*;

fn authority_review_uses_the_shared_trust_policy(trust: bool) {
    let source = Repo::new();
    let (change, _, _) = change_with_patchset(&source, "delivered");
    stdout(source.arc(&source.root).env("ARC_ACTOR", "reviewer").args([
        "review",
        &change,
        "--verdict",
        "approved",
    ]));
    let input = source.home.join("delivery.json");
    stdout(
        source
            .arc(&source.root)
            .args(["export", &change, "--output", input.to_str().unwrap()]),
    );
    let mut bundle: serde_json::Value = serde_json::from_slice(&fs::read(&input).unwrap()).unwrap();
    let origin = bundle["repository_id"].as_str().unwrap().to_string();
    let receiver = Repo::new();
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "init", "authority"]),
    );
    stdout(receiver.arc(&receiver.root).args([
        "replica",
        "pair",
        "worker",
        "--repository-id",
        &origin,
    ]));
    let replica = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "status", "--json"]),
    );
    assert_eq!(
        replica["authority"]["holder"]["repository_id"],
        replica["repository_id"]
    );
    bundle["project_id"] = replica["project_id"].clone();
    let branch = bundle["events"][0]["branch"].as_str().unwrap().to_string();
    git(
        &receiver.root,
        &[
            "fetch",
            source.root.to_str().unwrap(),
            &format!("{branch}:{branch}"),
            "master:refs/remotes/source/master",
        ],
    );
    let base = git_out(&receiver.root, &["rev-parse", "refs/remotes/source/master"]);
    git(&receiver.root, &["update-ref", "refs/heads/master", &base]);
    let trusted = if trust {
        format!("[\"{origin}\"]")
    } else {
        "[]".into()
    };
    receiver.declare_gates_locally(&format!(
        "[replica]\ntrusted_review_replicas={trusted}\n[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{origin}'\n[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles=['facts','review-authority']\nactors=['reviewer']\n"
    ));
    fs::write(&input, serde_json::to_vec(&bundle).unwrap()).unwrap();
    let digest = hex::encode(Sha256::digest(fs::read(&input).unwrap()));
    stdout(receiver.arc(&receiver.root).args([
        "import",
        input.to_str().unwrap(),
        "--publisher",
        "channel",
        "--received-sha256",
        &digest,
    ]));
    let status = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["status", &change, "--json"]),
    );
    assert_eq!(status["has_valid_approval"], trust, "{status}");
    let check = json_stdout_any_status(
        receiver
            .arc(&receiver.root)
            .args(["check", &change, "--json"]),
    );
    assert_eq!(check["ready"], trust, "{check}");
}

#[test]
fn authority_with_empty_trust_requires_local_review() {
    authority_review_uses_the_shared_trust_policy(false);
}

#[test]
fn authority_accepts_a_trusted_independent_foreign_review() {
    authority_review_uses_the_shared_trust_policy(true);
}

fn worker() -> (Repo, Repo) {
    let authority = Repo::new();
    let worker = Repo::new();
    git(
        &worker.root,
        &[
            "fetch",
            authority.root.to_str().unwrap(),
            "master:refs/remotes/authority/master",
        ],
    );
    git(
        &worker.root,
        &[
            "update-ref",
            "refs/heads/master",
            "refs/remotes/authority/master",
        ],
    );
    let id = json_stdout(worker.arc(&worker.root).args(["replica", "id", "--json"]));
    stdout(
        authority
            .arc(&authority.root)
            .args(["replica", "init", "authority"]),
    );
    stdout(authority.arc(&authority.root).args([
        "replica",
        "pair",
        "worker",
        "--repository-id",
        id["repository_id"].as_str().unwrap(),
    ]));
    let path = authority.home.join("pairing.json");
    stdout(authority.arc(&authority.root).args([
        "replica",
        "export",
        "--output",
        path.to_str().unwrap(),
    ]));
    let bytes = fs::read(&path).unwrap();
    let pairing: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let origin = pairing["source_replica_id"].as_str().unwrap();
    worker.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{origin}'\n[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles=['authority']\nactors=['tester']\n"));
    stdout(worker.arc(&worker.root).args([
        "replica",
        "import",
        path.to_str().unwrap(),
        "--publisher",
        "channel",
        "--received-sha256",
        &hex::encode(Sha256::digest(&bytes)),
    ]));
    (authority, worker)
}

fn prepared(worker: &Repo) -> String {
    let (change, _, _) = change_with_patchset(worker, "prepared");
    stdout(worker.arc(&worker.root).env("ARC_ACTOR", "reviewer").args([
        "review",
        &change,
        "--verdict",
        "approved",
    ]));
    change
}

fn prepare(worker: &Repo, change: &str) {
    stdout(
        worker
            .arc(&worker.root)
            .args(["integrate", change, "--provisional"]),
    );
}

fn status(worker: &Repo, change: &str) -> serde_json::Value {
    json_stdout(worker.arc(&worker.root).args(["status", change, "--json"]))
}

#[test]
fn explicit_provisional_integration_keeps_the_change_open() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    worker
        .arc(&worker.root)
        .args(["integrate", &change])
        .assert()
        .code(17);
    prepare(&worker, &change);
    let state = json_stdout(worker.arc(&worker.root).args(["show", &change, "--json"]));
    assert!(state["closure"].is_null(), "{state}");
    assert_eq!(
        status(&worker, &change)["provisional_integrations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn provisional_merge_is_unsigned_and_moves_only_staging_and_retention() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    let before = git_out(&worker.root, &["show-ref", "--heads"]);
    fs::write(
        worker.home.join(".gitconfig"),
        "[commit]\n gpgsign = true\n[gpg]\n program = /bin/false\n",
    )
    .unwrap();
    prepare(&worker, &change);
    let merge = git_out(
        &worker.root,
        &["rev-parse", "arc/provisional/worker/master"],
    );
    assert_eq!(
        git_out(&worker.root, &["show-ref", "--heads"])
            .lines()
            .filter(|line| !line.ends_with("refs/heads/arc/provisional/worker/master"))
            .collect::<Vec<_>>()
            .join("\n"),
        before
    );
    assert!(!git_out(&worker.root, &["cat-file", "commit", &merge]).contains("gpgsig"));
    assert_eq!(
        git_out(&worker.root, &["rev-list", "--parents", "-n", "1", &merge])
            .split_whitespace()
            .count(),
        3
    );
}

#[test]
fn provisional_destinations_are_protected() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    for destination in ["master", "arc/prepared", "arc/provisional/foreign/master"] {
        worker
            .arc(&worker.root)
            .args(["integrate", &change, "--provisional", "--into", destination])
            .assert()
            .code(17);
    }
    git(&worker.root, &["branch", "arc/provisional/worker/checked"]);
    let checkout = worker.home.join("checked");
    git(
        &worker.root,
        &[
            "worktree",
            "add",
            checkout.to_str().unwrap(),
            "arc/provisional/worker/checked",
        ],
    );
    worker
        .arc(&worker.root)
        .args([
            "integrate",
            &change,
            "--provisional",
            "--into",
            "arc/provisional/worker/checked",
        ])
        .assert()
        .code(1);
}

#[test]
fn provisional_flags_and_roles_refuse_conflicts() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    for flags in [
        vec!["--shape", "ff-only"],
        vec!["--cleanup"],
        vec!["--tag", "selected"],
        vec!["--for-replica", "authority", "--for-forge", "opaque"],
    ] {
        worker
            .arc(&worker.root)
            .args(["integrate", &change, "--provisional"])
            .args(flags)
            .assert()
            .code(2);
    }
    worker
        .arc(&worker.root)
        .env("ARC_ROLE", "implementer")
        .args(["integrate", &change, "--provisional"])
        .assert()
        .code(9);
}

#[test]
fn identical_provisional_retry_reuses_the_event_and_commit() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    prepare(&worker, &change);
    let before = status(&worker, &change)["provisional_integrations"].clone();
    prepare(&worker, &change);
    let after = status(&worker, &change)["provisional_integrations"].clone();
    assert_eq!(after[0]["event_id"], before[0]["event_id"]);
    assert_eq!(
        after[0]["provisional_commit"],
        before[0]["provisional_commit"]
    );
    assert_eq!(
        after.as_array().unwrap().len(),
        before.as_array().unwrap().len()
    );
}

#[test]
fn provisional_obligations_have_separate_aggregate_sections() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    prepare(&worker, &change);
    let catchup = json_stdout(worker.arc(&worker.root).args(["catchup", "--json"]));
    assert_eq!(
        catchup["provisional_integrations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(stdout(worker.arc(&worker.root).args(["catchup"])).contains("Provisional integrations"));
}

fn deliver(source: &Repo, receiver: &Repo, change: &str, trust: bool) -> serde_json::Value {
    let input = source.home.join(format!("{change}.json"));
    stdout(
        source
            .arc(&source.root)
            .args(["export", change, "--output", input.to_str().unwrap()]),
    );
    let bytes = fs::read(&input).unwrap();
    let bundle: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    deliver_git_objects(source, receiver);
    let branch = bundle["events"][0]["branch"].as_str().unwrap();
    if git_out(&receiver.root, &["branch", "--list", branch]).is_empty() {
        git(
            &receiver.root,
            &["branch", branch, &format!("refs/delivered/heads/{branch}")],
        );
    } else {
        git(
            &receiver.root,
            &[
                "update-ref",
                &format!("refs/heads/{branch}"),
                &format!("refs/delivered/heads/{branch}"),
            ],
        );
    }
    let exporter = bundle["source_replica_id"].as_str().unwrap();
    let mut origins = std::collections::BTreeSet::new();
    for event in bundle["events"]
        .as_array()
        .unwrap()
        .iter()
        .chain(bundle["repository_events"].as_array().into_iter().flatten())
    {
        origins.insert(event["repository_id"].as_str().unwrap().to_string());
    }
    let trusted = if trust {
        serde_json::to_string(&origins).unwrap()
    } else {
        "[]".into()
    };
    let mut policy = format!("[replica]\ntrusted_review_replicas={trusted}\n[[replica.publishers]]\nprincipal='delivery'\nsource_replica_id='{exporter}'\n");
    for origin in origins {
        policy.push_str(&format!("[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles=['facts','review-authority','provisional-integration','integration','rewrite']\nactors=['tester','reviewer','authority-reviewer']\n"));
    }
    receiver.declare_gates_locally(&policy);
    stdout(receiver.arc(&receiver.root).args([
        "import",
        input.to_str().unwrap(),
        "--publisher",
        "delivery",
        "--received-sha256",
        &hex::encode(Sha256::digest(&bytes)),
    ]));
    bundle
}

fn obligation(worker: &Repo, change: &str) -> serde_json::Value {
    status(worker, change)["provisional_integrations"][0].clone()
}

fn approve(repo: &Repo, change: &str) {
    stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "authority-reviewer")
            .args(["review", change, "--verdict", "approved"]),
    );
}

fn integrate_authority(repo: &Repo, change: &str) -> String {
    stdout(repo.arc(&repo.root).args([
        "integrate",
        change,
        "--shape",
        "no-ff",
        "--message",
        "merge: authoritative acceptance",
    ]));
    json_stdout(repo.arc(&repo.root).args(["show", change, "--json"]))["closure"]["event_id"]
        .as_str()
        .unwrap()
        .into()
}

fn dependent(worker: &Repo, prerequisite: &str) -> (String, PathBuf) {
    let change = opened_change_id(&stdout(worker.arc(&worker.root).args([
        "begin",
        "dependent",
        "--target",
        "master",
        "--base",
        "arc/provisional/worker/master",
        "--blocked-by",
        prerequisite,
    ])));
    let worktree = worker.home.join(".worktrees/repo-dependent");
    worker.commit(&worktree, "dependent.txt", "dependent\n", "feat: dependent");
    stdout(worker.arc(&worktree).args(["snapshot", &change]));
    approve(worker, &change);
    (change, worktree)
}

#[test]
fn local_provisional_prerequisite_permits_stacking_without_a_closure() {
    let (_authority, worker) = worker();
    let prerequisite = prepared(&worker);
    prepare(&worker, &prerequisite);
    let (change, _) = dependent(&worker, &prerequisite);
    worker
        .arc(&worker.root)
        .args(["check", &change])
        .assert()
        .code(17);
    worker
        .arc(&worker.root)
        .args(["check", &change, "--provisional"])
        .assert()
        .success();
    let plan = json_stdout(worker.arc(&worker.root).args([
        "integrate",
        &change,
        "--provisional",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(plan["mode"], "provisional");
    assert_eq!(plan["publication_target"], "master");
    assert!(plan["authorization"]["prerequisites"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        plan["authorization"]["provisional_prerequisites"][0]["change_id"],
        prerequisite
    );
    prepare(&worker, &change);
    assert!(json_stdout(
        worker
            .arc(&worker.root)
            .args(["show", &prerequisite, "--json"])
    )["closure"]
        .is_null());
}

#[test]
fn stale_missing_or_foreign_preparations_do_not_satisfy_dependencies() {
    for damage in ["stale", "missing", "foreign"] {
        let (authority, worker) = worker();
        let prerequisite = prepared(&worker);
        prepare(&worker, &prerequisite);
        let (change, worktree) = dependent(&worker, &prerequisite);
        match damage {
            "stale" => {
                let tree = worker.home.join(".worktrees/repo-prepared");
                worker.commit(&tree, "stale.txt", "later\n", "feat: later");
                stdout(worker.arc(&tree).args(["snapshot", &prerequisite]));
            }
            "missing" => {
                let merge = obligation(&worker, &prerequisite)["provisional_commit"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let object = worker
                    .root
                    .join(".git/objects")
                    .join(&merge[..2])
                    .join(&merge[2..]);
                fs::remove_file(object).unwrap();
            }
            "foreign" => {
                deliver(&worker, &authority, &prerequisite, true);
                let head = worker.head(&worktree);
                git(
                    &authority.root,
                    &[
                        "fetch",
                        worker.root.to_str().unwrap(),
                        "arc/dependent:arc/dependent",
                    ],
                );
                git(
                    &authority.root,
                    &["branch", "arc/provisional/authority/master", &head],
                );
                // Canonical integration never consumes foreign provisional dependency facts.
                let bundle = deliver(&worker, &authority, &change, true);
                authority
                    .arc(&authority.root)
                    .args([
                        "check",
                        bundle["change_id"].as_str().unwrap(),
                        "--shape",
                        "no-ff",
                    ])
                    .assert()
                    .code(7);
                continue;
            }
            _ => unreachable!(),
        }
        worker
            .arc(&worker.root)
            .args(["check", &change, "--provisional"])
            .assert()
            .code(7);
    }
}

#[test]
fn authority_follows_canonical_order_and_requires_replay_of_provisional_ancestry() {
    let (authority, worker) = worker();
    let prerequisite = prepared(&worker);
    prepare(&worker, &prerequisite);
    let (change, worktree) = dependent(&worker, &prerequisite);
    deliver(&worker, &authority, &prerequisite, true);
    deliver(&worker, &authority, &change, true);
    authority
        .arc(&authority.root)
        .args(["integrate", &change, "--shape", "no-ff"])
        .assert()
        .code(7);
    integrate_authority(&authority, &prerequisite);
    authority
        .arc(&authority.root)
        .args(["integrate", &change, "--shape", "no-ff"])
        .assert()
        .code(11)
        .stderr(predicates::str::contains("new patchset"));
    git(
        &worker.root,
        &[
            "fetch",
            authority.root.to_str().unwrap(),
            "master:refs/remotes/accepted/master",
        ],
    );
    git(
        &worktree,
        &[
            "rebase",
            "--onto",
            "refs/remotes/accepted/master",
            "arc/provisional/worker/master",
        ],
    );
    stdout(worker.arc(&worktree).args(["snapshot", &change]));
    worker
        .arc(&worker.root)
        .args(["check", &change, "--provisional"])
        .assert()
        .code(3);
}

#[test]
fn canonical_acceptance_atomically_resolves_same_and_revised_heads_with_shared_review() {
    for (trust, revised) in [(false, false), (true, false), (true, true)] {
        let (authority, worker) = worker();
        let change = prepared(&worker);
        prepare(&worker, &change);
        let delivered = obligation(&worker, &change);
        if revised {
            let tree = worker.home.join(".worktrees/repo-prepared");
            worker.commit(&tree, "revision.txt", "revision\n", "feat: revision");
            stdout(worker.arc(&tree).args(["snapshot", &change]));
            approve(&worker, &change);
        }
        deliver(&worker, &authority, &change, trust);
        if !trust {
            authority
                .arc(&authority.root)
                .args(["integrate", &change, "--shape", "no-ff"])
                .assert()
                .code(3);
            approve(&authority, &change);
        }
        let integration = integrate_authority(&authority, &change);
        let row = obligation(&authority, &change);
        assert_eq!(row["disposition"], "resolved");
        assert_eq!(row["integration_event_id"], integration);
        assert_eq!(
            row["resolution_kind"],
            if revised { "revised-head" } else { "same-head" }
        );
        let events: Vec<serde_json::Value> = stdout(
            authority
                .arc(&authority.root)
                .args(["events", "--change", &change]),
        )
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
        let event = events
            .iter()
            .find(|event| event["event_id"] == integration)
            .unwrap();
        assert_eq!(
            event["provisional_resolutions"][0]["provisional_event_id"],
            delivered["event_id"]
        );
        assert!(event["authority_grant_event_id"].is_string());
    }
}

#[test]
fn authority_refusal_and_nonintegrating_closures_keep_delivery_visible() {
    for outcome in ["refused", "abandoned", "superseded"] {
        let (authority, worker) = worker();
        let change = prepared(&worker);
        prepare(&worker, &change);
        deliver(&worker, &authority, &change, true);
        match outcome {
            "refused" => {
                stdout(authority.arc(&authority.root).args([
                    "review",
                    &change,
                    "--verdict",
                    "changes-requested",
                    "--cause",
                    "executor",
                ]));
                deliver(&authority, &worker, &change, true);
                let row = obligation(&worker, &change);
                assert_eq!(row["disposition"], "authority-refused");
                assert!(row["refusal_event_id"].is_string());
            }
            "abandoned" => {
                stdout(
                    worker
                        .arc(&worker.root)
                        .args(["close", &change, "--abandoned"]),
                );
            }
            "superseded" => {
                let successor = begin_change(&worker, "successor", None);
                stdout(worker.arc(&worker.root).args([
                    "close",
                    &change,
                    "--superseded",
                    &successor,
                ]));
            }
            _ => unreachable!(),
        }
        let catchup = json_stdout(worker.arc(&worker.root).args(["catchup", "--json"]));
        assert_eq!(
            catchup["provisional_integrations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

fn export_without_preparation(worker: &Repo, authority: &Repo, change: &str) {
    let events = worker
        .root
        .join(".git/arc/changes")
        .join(change)
        .join("events");
    let event = obligation(worker, change)["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = events.join(format!("{event}.json"));
    let saved = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    deliver(worker, authority, change, true);
    fs::write(&path, saved).unwrap();
}

#[test]
fn late_delivery_requires_explicit_valid_authority_reconciliation() {
    let (authority, worker) = worker();
    let change = prepared(&worker);
    prepare(&worker, &change);
    let id = obligation(&worker, &change)["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    export_without_preparation(&worker, &authority, &change);
    let integration = integrate_authority(&authority, &change);
    deliver(&worker, &authority, &change, true);
    assert_eq!(
        obligation(&authority, &change)["disposition"],
        "requires-reconciliation"
    );
    authority
        .arc(&authority.root)
        .args([
            "replica",
            "provisional",
            "resolve",
            &change,
            "--integration",
            "missing",
            "--because",
            "late delivery",
        ])
        .assert()
        .code(1);
    authority
        .arc(&authority.root)
        .args([
            "replica",
            "provisional",
            "resolve",
            &change,
            "--integration",
            &integration,
            "--provisional",
            "missing",
            "--because",
            "late delivery",
        ])
        .assert()
        .code(1);
    worker
        .arc(&worker.root)
        .args([
            "replica",
            "provisional",
            "resolve",
            &change,
            "--integration",
            &integration,
            "--because",
            "worker lacks authority",
        ])
        .assert()
        .code(17);
    authority
        .arc(&authority.root)
        .env("ARC_ROLE", "reviewer")
        .args([
            "replica",
            "provisional",
            "resolve",
            &change,
            "--integration",
            &integration,
            "--because",
            "lead only",
        ])
        .assert()
        .code(9);
    stdout(authority.arc(&authority.root).args([
        "replica",
        "provisional",
        "resolve",
        &change,
        "--integration",
        &integration,
        "--provisional",
        &id,
        "--because",
        "late delivery",
    ]));
    let before = obligation(&authority, &change);
    stdout(authority.arc(&authority.root).args([
        "replica",
        "provisional",
        "resolve",
        &change,
        "--integration",
        &integration,
        "--provisional",
        &id,
        "--because",
        "idempotent retry",
    ]));
    assert_eq!(
        obligation(&authority, &change)["integration_event_id"],
        before["integration_event_id"]
    );
}

#[test]
fn forge_settlement_uses_assertion_evidence_without_transport() {
    let (authority, worker) = worker();
    let change = prepared(&worker);
    stdout(worker.arc(&worker.root).args([
        "integrate",
        &change,
        "--provisional",
        "--for-forge",
        "opaque-reference",
    ]));
    deliver(&worker, &authority, &change, true);
    let source = status(&authority, &change)["current_head"]
        .as_str()
        .unwrap()
        .to_string();
    stdout(authority.arc(&authority.root).args([
        "external",
        "verdict",
        &change,
        "--verdict",
        "approved",
        "--decided-by",
        "Maintainer",
        "--reference",
        "opaque-reference",
        "--revision",
        &source,
    ]));
    git(
        &authority.root,
        &[
            "merge",
            "--no-ff",
            "--no-gpg-sign",
            "-m",
            "merge: asserted acceptance",
            "arc/prepared",
        ],
    );
    let integrated = authority.head(&authority.root);
    stdout(authority.arc(&authority.root).args([
        "close",
        &change,
        "--assert-integrated",
        &integrated,
        "--external-reference",
        "opaque-reference",
    ]));
    let integration = json_stdout(
        authority
            .arc(&authority.root)
            .args(["show", &change, "--json"]),
    )["closure"]["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    let output = stdout(authority.arc(&authority.root).args([
        "replica",
        "provisional",
        "resolve",
        &change,
        "--integration",
        &integration,
        "--because",
        "forge acceptance",
    ]));
    assert!(output.contains("resolved by assertion"), "{output}");
    assert_eq!(
        obligation(&authority, &change)["disposition"],
        "resolved-by-assertion"
    );
}

#[test]
fn provisional_policy_modes_and_inflight_recipient_are_explicit() {
    let unpaired = Repo::new();
    let change = prepared(&unpaired);
    unpaired
        .arc(&unpaired.root)
        .args(["integrate", &change, "--provisional"])
        .assert()
        .code(17);
    let (authority, worker) = worker();
    let change = prepared(&authority);
    authority
        .arc(&authority.root)
        .args(["integrate", &change, "--provisional"])
        .assert()
        .code(17);
    let change = prepared(&worker);
    worker.declare_gates_locally("[replica]\nprovisional_enabled=false\n");
    worker
        .arc(&worker.root)
        .args(["integrate", &change, "--provisional"])
        .assert()
        .code(17);
    worker.declare_gates_locally("[replica]\nprovisional_enabled=true\nprotected_targets=['arc/provisional/worker/protected']\n");
    worker
        .arc(&worker.root)
        .args([
            "integrate",
            &change,
            "--provisional",
            "--into",
            "arc/provisional/worker/protected",
        ])
        .assert()
        .code(17);
}

#[test]
fn provisional_guard_exit_vocabulary_is_preserved() {
    for (guard, code) in [
        ("approval", 3),
        ("finding", 2),
        ("hold", 4),
        ("gate", 5),
        ("iteration", 13),
    ] {
        let (_authority, worker) = worker();
        let change = if guard == "approval" {
            change_with_patchset(&worker, "prepared").0
        } else {
            prepared(&worker)
        };
        match guard {
            "finding" => {
                stdout(worker.arc(&worker.root).args([
                    "finding",
                    &change,
                    "--blocking",
                    "--summary",
                    "blocked",
                    "--body",
                    "repair needed",
                ]));
            }
            "hold" => {
                stdout(
                    worker
                        .arc(&worker.root)
                        .args(["hold", &change, "--reason", "wait"]),
                );
            }
            "gate" => worker.declare_gates_locally("[gates.required]\ncommand='false'\n"),
            "iteration" => {
                stdout(worker.arc(&worker.root).args(["iterating", &change]));
            }
            _ => (),
        }
        worker
            .arc(&worker.root)
            .args(["check", &change, "--provisional"])
            .assert()
            .code(code);
        worker
            .arc(&worker.root)
            .args(["integrate", &change, "--provisional"])
            .assert()
            .code(code);
    }
}

#[test]
fn interruption_before_or_after_ref_publication_recovers_the_exact_intent() {
    use std::os::unix::fs::PermissionsExt;
    for after in [false, true] {
        let (_authority, worker) = worker();
        let change = prepared(&worker);
        let hook = worker.root.join(".git/hooks/reference-transaction");
        let intent_path = worker
            .root
            .join(".git/arc/provisional")
            .join(format!("{change}.json"));
        let script = if after {
            format!("#!/bin/sh\nif [ \"$1\" = committed ]; then\npython3 - '{}' '{}' <<'PY'\nimport json, pathlib, sys\ne=json.load(open(sys.argv[1]))['event']\np=pathlib.Path(sys.argv[2])/'changes'/e['change_id']/'events'/(e['event_id']+'.json')\np.mkdir(parents=True)\nPY\nfi\n", intent_path.display(), worker.root.join(".git/arc").display())
        } else {
            "#!/bin/sh\nif [ \"$1\" = prepared ]; then exit 1; fi\n".into()
        };
        fs::write(&hook, script).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        worker
            .arc(&worker.root)
            .args(["integrate", &change, "--provisional"])
            .assert()
            .code(1);
        let intent: serde_json::Value =
            serde_json::from_slice(&fs::read(&intent_path).unwrap()).unwrap();
        let event = &intent["event"];
        if after {
            let blocked = worker
                .root
                .join(".git/arc/changes")
                .join(&change)
                .join("events")
                .join(format!("{}.json", event["event_id"].as_str().unwrap()));
            fs::remove_dir(blocked).unwrap();
        } else {
            assert!(git_out(
                &worker.root,
                &["branch", "--list", "arc/provisional/worker/master"]
            )
            .is_empty());
        }
        fs::remove_file(&hook).unwrap();
        prepare(&worker, &change);
        assert_eq!(obligation(&worker, &change)["event_id"], event["event_id"]);
        assert_eq!(
            obligation(&worker, &change)["provisional_commit"],
            event["provisional_commit"]
        );
        assert!(!intent_path.exists());
        assert_eq!(
            status(&worker, &change)["provisional_integrations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn workspace_reports_unresolved_preparations_and_preserves_partial_exit() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    prepare(&worker, &change);
    let report = json_stdout(
        worker
            .arc(&worker.root)
            .args(["workspace", "report", "--json"]),
    );
    assert_eq!(
        report["sections"]["provisional_integrations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        stdout(worker.arc(&worker.root).args(["workspace", "report"]))
            .contains("Provisional integrations")
    );
    let path = worker
        .root
        .join(".git/arc/changes")
        .join(&change)
        .join("events");
    fs::write(path.join("malformed.json"), "{broken").unwrap();
    worker
        .arc(&worker.root)
        .args(["workspace", "report", "--json"])
        .assert()
        .code(16);
}

#[test]
fn signing_preserves_exact_head_review_and_same_head_acceptance() {
    let (authority, worker) = worker();
    let change = prepared(&worker);
    prepare(&worker, &change);
    let original = obligation(&worker, &change)["source_head"]
        .as_str()
        .unwrap()
        .to_string();
    let key = worker.home.join("signing-key");
    assert!(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status()
        .unwrap()
        .success());
    let signers = worker.home.join("allowed-signers");
    fs::write(
        &signers,
        format!(
            "tester@example.invalid {}",
            fs::read_to_string(key.with_extension("pub")).unwrap()
        ),
    )
    .unwrap();
    git(&worker.root, &["config", "gpg.format", "ssh"]);
    git(
        &worker.root,
        &[
            "config",
            "gpg.ssh.allowedSignersFile",
            signers.to_str().unwrap(),
        ],
    );
    let worktree = worker.home.join(".worktrees/repo-prepared");
    stdout(worker.arc(&worktree).args([
        "rewrite",
        "sign",
        "--key",
        key.to_str().unwrap(),
        "--from",
        &original,
        "--include-shared",
    ]));
    assert_ne!(worker.head(&worktree), original);
    assert_eq!(status(&worker, &change)["has_valid_approval"], true);
    deliver(&worker, &authority, &change, true);
    integrate_authority(&authority, &change);
    assert_eq!(
        obligation(&authority, &change)["resolution_kind"],
        "same-head"
    );
}

#[test]
fn provisional_probe_and_fork_guards_retain_their_codes() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    stdout(
        worker
            .arc(&worker.root)
            .args([
                "brief",
                &change,
                "--body-file",
                "-",
                "--probe",
                "regression=false",
            ])
            .write_stdin("A discriminating regression probe."),
    );
    let worktree = worker.home.join(".worktrees/repo-prepared");
    stdout(worker.arc(&worktree).args(["snapshot", &change]));
    approve(&worker, &change);
    worker
        .arc(&worker.root)
        .args(["check", &change, "--provisional"])
        .assert()
        .code(12);
    worker
        .arc(&worker.root)
        .args(["integrate", &change, "--provisional"])
        .assert()
        .code(12);
    git(&worker.root, &["branch", "fork/prepared", "arc/prepared"]);
    rewrite_event(&worker, &change, "change-opened", |event| {
        event["branch"] = serde_json::json!("fork/prepared");
    });
    worker
        .arc(&worker.root)
        .args(["check", &change, "--provisional"])
        .assert()
        .code(15);
    worker
        .arc(&worker.root)
        .args(["integrate", &change, "--provisional"])
        .assert()
        .code(15);
}

#[test]
fn canonical_policy_cannot_be_weakened_by_staging_declarations() {
    let (_authority, worker) = worker();
    fs::create_dir_all(worker.root.join(".arc")).unwrap();
    let canonical = "[policy]\nforbid_self_approval=true\n[replica]\nprovisional_enabled=true\n";
    worker.commit(
        &worker.root,
        ".arc/policy.toml",
        canonical,
        "policy: require independent review",
    );
    worker.commit(
        &worker.root,
        ".arc/gates.toml",
        "[gates.required]\ncommand='false'\n",
        "gates: require canonical check",
    );
    let staging = "arc/provisional/worker/master";
    let staging_checkout = worker.home.join("staging-policy");
    git(
        &worker.root,
        &[
            "worktree",
            "add",
            "-b",
            staging,
            staging_checkout.to_str().unwrap(),
            "master",
        ],
    );
    let weakened = "[policy]\nforbid_self_approval=false\n[replica]\nprovisional_enabled=true\n";
    worker.commit(
        &staging_checkout,
        ".arc/policy.toml",
        weakened,
        "policy: permit staging self approval",
    );
    worker.commit(
        &staging_checkout,
        ".arc/gates.toml",
        "[gates.required]\ncommand='true'\n",
        "gates: weaken staging check",
    );
    git(
        &worker.root,
        &["worktree", "remove", staging_checkout.to_str().unwrap()],
    );
    assert_eq!(
        git_out(&worker.root, &["show", "master:.arc/policy.toml"]),
        canonical.trim()
    );
    assert_eq!(
        git_out(
            &worker.root,
            &["show", &format!("{staging}:.arc/policy.toml")]
        ),
        weakened.trim()
    );
    let (change, _, _) = change_with_patchset(&worker, "prepared");
    stdout(
        worker
            .arc(&worker.root)
            .args(["review", &change, "--verdict", "approved"]),
    );
    let stage_evidence =
        stdout(
            worker
                .arc(&worker.root)
                .args(["verify", &change, "--against", staging]),
        );
    assert!(
        stage_evidence.contains("verification: Pass"),
        "{stage_evidence}"
    );
    let evidence = json_stdout(worker.arc(&worker.root).args(["show", &change, "--json"]));
    assert_eq!(
        evidence["verifications"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["command"],
        "true"
    );
    let before = git_out(&worker.root, &["show-ref", "--heads"]);
    worker
        .arc(&worker.root)
        .args(["check", &change, "--provisional"])
        .assert()
        .code(3);
    worker
        .arc(&worker.root)
        .args(["integrate", &change, "--provisional"])
        .assert()
        .code(3);
    worker
        .arc(&worker.root)
        .args([
            "integrate",
            &change,
            "--provisional",
            "--debt",
            "local preparation owed review",
        ])
        .assert()
        .code(5);
    assert_eq!(git_out(&worker.root, &["show-ref", "--heads"]), before);
    assert!(status(&worker, &change)["provisional_integrations"]
        .as_array()
        .unwrap()
        .is_empty());
    worker.commit(
        &worker.root,
        ".arc/policy.toml",
        "[replica]\nprovisional_enabled=false\n",
        "policy: disable staging preparation",
    );
    worker
        .arc(&worker.root)
        .args(["check", &change, "--provisional"])
        .assert()
        .code(17);
}

#[test]
fn provisional_import_requires_its_publishing_capability_and_pins_delivery_objects() {
    let (authority, worker) = worker();
    let change = prepared(&worker);
    prepare(&worker, &change);
    let input = worker.home.join("capability.json");
    stdout(
        worker
            .arc(&worker.root)
            .args(["export", &change, "--output", input.to_str().unwrap()]),
    );
    let bytes = fs::read(&input).unwrap();
    let bundle: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(bundle["objects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|object| object["role"] == "provisional-merge"));
    deliver_git_objects(&worker, &authority);
    let origin = bundle["source_replica_id"].as_str().unwrap();
    authority.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{origin}'\n[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles=['facts','review-authority']\nactors=['tester','reviewer']\n"));
    authority
        .arc(&authority.root)
        .args([
            "import",
            input.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &hex::encode(Sha256::digest(&bytes)),
        ])
        .assert()
        .code(18)
        .stderr(predicates::str::contains("ProvisionalIntegration"));
    deliver(&worker, &authority, &change, true);
    let row = obligation(&authority, &change);
    assert_eq!(
        git_out(
            &authority.root,
            &[
                "rev-parse",
                &format!(
                    "refs/arc/provisional/{change}/{}",
                    row["event_id"].as_str().unwrap()
                )
            ]
        ),
        row["provisional_commit"].as_str().unwrap()
    );
}

#[test]
fn replacing_provisional_ancestry_is_not_signature_only_and_forces_a_patchset() {
    use std::io::Write;
    let (authority, worker) = worker();
    let prerequisite = prepared(&worker);
    prepare(&worker, &prerequisite);
    let staged = obligation(&worker, &prerequisite)["provisional_commit"]
        .as_str()
        .unwrap()
        .to_string();
    let (change, worktree) = dependent(&worker, &prerequisite);
    let old = worker.head(&worktree);
    deliver(&worker, &authority, &prerequisite, true);
    stdout(authority.arc(&authority.root).args([
        "integrate",
        &prerequisite,
        "--shape",
        "no-ff",
        "--message",
        "merge: canonical acceptance",
    ]));
    let accepted = authority.head(&authority.root);
    deliver(&authority, &worker, &prerequisite, true);
    let raw = Command::new("git")
        .current_dir(&worktree)
        .args(["cat-file", "commit", &old])
        .output()
        .unwrap()
        .stdout;
    let replaced = String::from_utf8(raw).unwrap().replace(
        &format!("parent {staged}\n"),
        &format!("parent {accepted}\n"),
    );
    let mut writer = Command::new("git")
        .current_dir(&worktree)
        .args(["hash-object", "-t", "commit", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(replaced.as_bytes())
        .unwrap();
    let output = writer.wait_with_output().unwrap();
    assert!(output.status.success());
    let rewritten = String::from_utf8(output.stdout).unwrap().trim().to_string();
    git(
        &worker.root,
        &["update-ref", "refs/heads/arc/dependent", &rewritten, &old],
    );
    stdout(
        worker
            .arc(&worker.root)
            .args([
                "history",
                "rewrite",
                "--map",
                "-",
                "--reason",
                "replay onto accepted prerequisite",
            ])
            .write_stdin(format!("{staged} {accepted}\n{old} {rewritten}\n")),
    );
    let status = status(&worker, &change);
    assert_eq!(status["has_valid_approval"], false, "{status}");
    let before = status["latest_patchset"]["id"].clone();
    worker
        .arc(&worker.root)
        .args([
            "integrate",
            &change,
            "--provisional",
            "--debt",
            "explicit debt still requires a replay snapshot",
        ])
        .assert()
        .code(3);
    stdout(worker.arc(&worktree).args(["snapshot", &change]));
    let after = self::status(&worker, &change);
    assert_ne!(
        after["latest_patchset"]["id"], before,
        "ancestry replay requires a new patchset"
    );
    assert_eq!(after["has_valid_approval"], false);
    approve(&worker, &change);
    assert_eq!(self::status(&worker, &change)["has_valid_approval"], true);
}

#[test]
fn an_authority_refusal_blocks_stacking_until_its_verdict_is_superseded() {
    let (authority, worker) = worker();
    let prerequisite = prepared(&worker);
    prepare(&worker, &prerequisite);
    let (change, _) = dependent(&worker, &prerequisite);
    deliver(&worker, &authority, &prerequisite, true);
    stdout(authority.arc(&authority.root).args([
        "review",
        &prerequisite,
        "--verdict",
        "changes-requested",
        "--cause",
        "executor",
    ]));
    deliver(&authority, &worker, &prerequisite, false);
    assert_eq!(
        obligation(&worker, &prerequisite)["disposition"],
        "authority-refused"
    );
    worker
        .arc(&worker.root)
        .args(["check", &change, "--provisional"])
        .assert()
        .code(7);
    approve(&authority, &prerequisite);
    deliver(&authority, &worker, &prerequisite, false);
    worker
        .arc(&worker.root)
        .args(["check", &change, "--provisional"])
        .assert()
        .success();
}

#[test]
fn a_delivery_recorded_after_canonical_closure_stays_visible_and_can_reconcile() {
    let (authority, worker) = worker();
    let change = prepared(&worker);
    deliver(&worker, &authority, &change, true);
    let integration = integrate_authority(&authority, &change);
    rewrite_event(&authority, &change, "change-integrated", |event| {
        event
            .as_object_mut()
            .unwrap()
            .remove("authority_grant_event_id");
    });
    prepare(&worker, &change);
    let id = obligation(&worker, &change)["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        id > integration,
        "late recording must follow closure in event-ID order"
    );
    deliver(&worker, &authority, &change, true);
    assert_eq!(
        obligation(&authority, &change)["disposition"],
        "requires-reconciliation"
    );
    stdout(authority.arc(&authority.root).args([
        "replica",
        "provisional",
        "resolve",
        &change,
        "--integration",
        &integration,
        "--because",
        "late delivery of accepted head",
    ]));
    assert_eq!(obligation(&authority, &change)["disposition"], "resolved");
}

#[test]
fn late_resolution_rejects_conflicting_patchset_kind_and_grant_evidence() {
    for damaged in ["patchset", "kind", "grant"] {
        let (authority, worker) = worker();
        let change = prepared(&worker);
        prepare(&worker, &change);
        export_without_preparation(&worker, &authority, &change);
        let integration = integrate_authority(&authority, &change);
        deliver(&worker, &authority, &change, true);
        if damaged == "grant" {
            rewrite_event(&authority, &change, "change-integrated", |event| {
                event["authority_grant_event_id"] = serde_json::json!("missing-grant");
            });
        } else {
            stdout(authority.arc(&authority.root).args([
                "replica",
                "provisional",
                "resolve",
                &change,
                "--integration",
                &integration,
                "--because",
                "late acceptance",
            ]));
            rewrite_event(
                &authority,
                &change,
                "provisional-integration-resolved",
                |event| {
                    if damaged == "patchset" {
                        event["accepted_patchset_id"] = serde_json::json!("ps-missing");
                    } else {
                        event["kind"] = serde_json::json!("revised-head");
                    }
                },
            );
        }
        let id = obligation(&authority, &change)["event_id"]
            .as_str()
            .unwrap()
            .to_string();
        authority
            .arc(&authority.root)
            .args([
                "replica",
                "provisional",
                "resolve",
                &change,
                "--integration",
                &integration,
                "--provisional",
                &id,
                "--because",
                "validate evidence",
            ])
            .assert()
            .code(1);
    }
}

#[test]
fn protected_primary_source_and_layered_policy_destinations_refuse() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    git(
        &worker.root,
        &["checkout", "-b", "arc/provisional/worker/primary"],
    );
    worker
        .arc(&worker.root)
        .args([
            "integrate",
            &change,
            "--provisional",
            "--into",
            "arc/provisional/worker/primary",
        ])
        .assert()
        .code(17)
        .stderr(predicates::str::contains("primary checkout"));
    git(&worker.root, &["checkout", "master"]);
    git(
        &worker.root,
        &[
            "branch",
            "-m",
            "arc/prepared",
            "arc/provisional/worker/source",
        ],
    );
    rewrite_event(&worker, &change, "change-opened", |event| {
        event["branch"] = serde_json::json!("arc/provisional/worker/source");
    });
    worker
        .arc(&worker.root)
        .args([
            "integrate",
            &change,
            "--provisional",
            "--into",
            "arc/provisional/worker/source",
        ])
        .assert()
        .code(17)
        .stderr(predicates::str::contains("source branch"));
    fs::create_dir_all(worker.root.join(".arc")).unwrap();
    worker.commit(&worker.root, ".arc/policy.toml", "[replica]\nprovisional_enabled=false\nprotected_targets=['arc/provisional/worker/project']\n", "policy: restrict preparation");
    worker.declare_gates_locally("[replica]\nprovisional_enabled=true\nprotected_targets=['arc/provisional/worker/operator']\n");
    worker
        .arc(&worker.root)
        .args(["integrate", &change, "--provisional"])
        .assert()
        .code(17)
        .stderr(predicates::str::contains("disabled"));
    worker.commit(&worker.root, ".arc/policy.toml", "[replica]\nprovisional_enabled=true\nprotected_targets=['arc/provisional/worker/project']\n", "policy: permit protected staging");
    for target in [
        "arc/provisional/worker/project",
        "arc/provisional/worker/operator",
    ] {
        worker
            .arc(&worker.root)
            .args(["integrate", &change, "--provisional", "--into", target])
            .assert()
            .code(17)
            .stderr(predicates::str::contains("protected_targets"));
    }
    worker.declare_gates_locally("[replica]\nprovisional_enabled=false\n");
    worker
        .arc(&worker.root)
        .args(["check", &change, "--provisional"])
        .assert()
        .code(17);
}

#[test]
fn an_inflight_offer_requires_an_explicit_routing_recipient() {
    let (authority, worker) = worker();
    let offline = Repo::new();
    let id = json_stdout(offline.arc(&offline.root).args(["replica", "id", "--json"]));
    stdout(authority.arc(&authority.root).args([
        "replica",
        "pair",
        "offline",
        "--repository-id",
        id["repository_id"].as_str().unwrap(),
    ]));
    stdout(authority.arc(&authority.root).args([
        "replica",
        "authority",
        "offer",
        "--to",
        "offline",
    ]));
    let pairing = authority.home.join("flight.json");
    stdout(authority.arc(&authority.root).args([
        "replica",
        "export",
        "--output",
        pairing.to_str().unwrap(),
    ]));
    let bytes = fs::read(&pairing).unwrap();
    stdout(worker.arc(&worker.root).args([
        "replica",
        "import",
        pairing.to_str().unwrap(),
        "--publisher",
        "channel",
        "--received-sha256",
        &hex::encode(Sha256::digest(bytes)),
    ]));
    let change = prepared(&worker);
    let checked = worker
        .arc(&worker.root)
        .args(["check", &change, "--provisional"])
        .assert()
        .code(17);
    let diagnostic = String::from_utf8_lossy(&checked.get_output().stderr);
    let remedy = diagnostic
        .lines()
        .find_map(|line| line.strip_prefix("Try: "))
        .unwrap_or_else(|| panic!("no executable remedy: {diagnostic}"));
    let heads = git_out(&worker.root, &["show-ref", "--heads"]);
    stdout(&mut worker.arc_shell(&worker.root, remedy));
    assert_eq!(git_out(&worker.root, &["show-ref", "--heads"]), heads);
    worker
        .arc(&worker.root)
        .args(["integrate", &change, "--provisional"])
        .assert()
        .code(17)
        .stderr(predicates::str::contains("offer"));
    worker
        .arc(&worker.root)
        .args([
            "integrate",
            &change,
            "--provisional",
            "--for-replica",
            "missing",
        ])
        .assert()
        .code(17);
    stdout(worker.arc(&worker.root).args([
        "integrate",
        &change,
        "--provisional",
        "--for-forge",
        "opaque",
    ]));
}

#[test]
fn staging_conflict_merged_tree_and_closed_guards_preserve_their_codes() {
    for guard in ["conflict", "merged-tree", "closed"] {
        let (_authority, worker) = worker();
        let change = prepared(&worker);
        let tree = worker.home.join(".worktrees/repo-prepared");
        match guard {
            "conflict" => {
                worker.commit(&tree, "README.md", "source\n", "feat: source edit");
                stdout(worker.arc(&tree).args(["snapshot", &change]));
                approve(&worker, &change);
                worker.commit(&worker.root, "README.md", "target\n", "feat: target edit");
            }
            "merged-tree" => {
                worker.declare_gates_locally("[gates.required]\ncommand='true'\n");
                stdout(worker.arc(&tree).args(["verify", &change, "--all"]));
                worker.commit(
                    &worker.root,
                    "target.txt",
                    "target\n",
                    "feat: target advances",
                );
            }
            "closed" => {
                stdout(
                    worker
                        .arc(&worker.root)
                        .args(["close", &change, "--abandoned"]),
                );
            }
            _ => unreachable!(),
        }
        let code = match guard {
            "conflict" => 11,
            "merged-tree" => 14,
            _ => 6,
        };
        worker
            .arc(&worker.root)
            .args(["check", &change, "--provisional"])
            .assert()
            .code(code);
        worker
            .arc(&worker.root)
            .args(["integrate", &change, "--provisional"])
            .assert()
            .code(code);
    }
}

#[test]
fn completed_intent_recovery_preserves_a_competing_staging_ref() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    prepare(&worker, &change);
    let row = obligation(&worker, &change);
    let event_path = worker
        .root
        .join(".git/arc/changes")
        .join(&change)
        .join("events")
        .join(format!("{}.json", row["event_id"].as_str().unwrap()));
    let event: serde_json::Value = serde_json::from_slice(&fs::read(event_path).unwrap()).unwrap();
    let intent = worker
        .root
        .join(".git/arc/provisional")
        .join(format!("{change}.json"));
    fs::write(
        &intent,
        serde_json::to_vec(&serde_json::json!({ "event": event, "old_staging": null })).unwrap(),
    )
    .unwrap();
    let competing = worker.head(&worker.root);
    git(
        &worker.root,
        &[
            "update-ref",
            "refs/heads/arc/provisional/worker/master",
            &competing,
        ],
    );
    prepare(&worker, &change);
    assert_eq!(
        git_out(
            &worker.root,
            &["rev-parse", "arc/provisional/worker/master"]
        ),
        competing
    );
    assert!(!intent.exists());
    assert_eq!(
        status(&worker, &change)["provisional_integrations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn recipient_routing_does_not_grant_or_restrict_publication_authority() {
    let (authority, worker) = worker();
    let change = prepared(&worker);
    stdout(worker.arc(&worker.root).args([
        "integrate",
        &change,
        "--provisional",
        "--for-replica",
        "worker",
    ]));
    worker
        .arc(&worker.root)
        .args(["integrate", &change])
        .assert()
        .code(17);
    let recipient = obligation(&worker, &change)["recipient"].clone();
    deliver(&worker, &authority, &change, true);
    integrate_authority(&authority, &change);
    assert_eq!(obligation(&authority, &change)["recipient"], recipient);
    assert_eq!(obligation(&authority, &change)["disposition"], "resolved");
}

#[test]
fn retry_plan_and_execution_agree_on_reusing_the_completed_preparation() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    prepare(&worker, &change);
    let existing = obligation(&worker, &change);
    let plan = json_stdout(worker.arc(&worker.root).args([
        "integrate",
        &change,
        "--provisional",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(plan["merge_commit"], false, "{plan}");
    assert_eq!(plan["reuse_event_id"], existing["event_id"]);
    assert_eq!(plan["reuse_commit"], existing["provisional_commit"]);
    prepare(&worker, &change);
    assert_eq!(
        status(&worker, &change)["provisional_integrations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn provisional_expected_basis_names_moved_guards_beside_the_refusal() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    let expected = worker.home.join("provisional-plan.json");
    fs::write(
        &expected,
        stdout(worker.arc(&worker.root).args([
            "integrate",
            &change,
            "--provisional",
            "--dry-run",
            "--json",
        ])),
    )
    .unwrap();
    worker.declare_gates_locally("[gates.required]\ncommand='false'\n");
    let refused = worker
        .arc(&worker.root)
        .args(["integrate", &change, "--provisional", "--expect-basis"])
        .arg(&expected)
        .assert()
        .code(5);
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr);
    assert!(stderr.contains("you checked"), "{stderr}");
    assert!(stderr.contains("required"), "{stderr}");
    assert!(!stderr.contains("warning:"), "{stderr}");
    assert!(status(&worker, &change)["provisional_integrations"]
        .as_array()
        .unwrap()
        .is_empty());
    worker.declare_gates_locally("");
    git(
        &worker.root,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "chore: advance canonical base",
        ],
    );
    let permitted = worker
        .arc(&worker.root)
        .args(["integrate", &change, "--provisional", "--expect-basis"])
        .arg(&expected)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&permitted.get_output().stderr);
    assert!(
        stderr.contains("warning: the target you checked moved"),
        "{stderr}"
    );
}

#[test]
fn a_signature_only_staging_successor_retains_local_prerequisite_coverage() {
    let (_authority, worker) = worker();
    let prerequisite = prepared(&worker);
    prepare(&worker, &prerequisite);
    let original = obligation(&worker, &prerequisite)["provisional_commit"]
        .as_str()
        .unwrap()
        .to_string();
    let key = worker.home.join("staging-signing-key");
    assert!(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status()
        .unwrap()
        .success());
    git(&worker.root, &["config", "gpg.format", "ssh"]);
    git(&worker.root, &["switch", "arc/provisional/worker/master"]);
    stdout(worker.arc(&worker.root).args([
        "rewrite",
        "sign",
        "--key",
        key.to_str().unwrap(),
        "--from",
        &original,
        "--include-shared",
    ]));
    let signed = worker.head(&worker.root);
    assert_ne!(signed, original);
    git(&worker.root, &["switch", "master"]);
    let (dependent, _) = dependent(&worker, &prerequisite);
    let checked = json_stdout(worker.arc(&worker.root).args([
        "check",
        &dependent,
        "--provisional",
        "--json",
    ]));
    assert_eq!(
        checked["provisional_prerequisites"][0]["provisional_commit"],
        signed
    );
    assert_eq!(
        obligation(&worker, &prerequisite)["provisional_commit"],
        original
    );
    assert_eq!(obligation(&worker, &prerequisite)["local_merge"], signed);
    prepare(&worker, &dependent);
    assert_eq!(status(&worker, &prerequisite)["state"], "open");
}

fn block_provisional_event_observation(worker: &Repo, change: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let hook = worker.root.join(".git/hooks/reference-transaction");
    let intent = worker
        .root
        .join(".git/arc/provisional")
        .join(format!("{change}.json"));
    let events = worker
        .root
        .join(".git/arc/changes")
        .join(change)
        .join("events");
    fs::write(&hook, format!("#!/bin/sh\nif [ \"$1\" = committed ]; then\npython3 - '{}' '{}' <<'PY'\nimport json, pathlib, sys\ne=json.load(open(sys.argv[1]))['event']\n(pathlib.Path(sys.argv[2])/(e['event_id']+'.json')).mkdir()\nPY\nfi\n", intent.display(), events.display())).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    intent
}

fn block_provisional_publication(worker: &Repo, change: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let hook = worker.root.join(".git/hooks/reference-transaction");
    let intent = worker
        .root
        .join(".git/arc/provisional")
        .join(format!("{change}.json"));
    let events = worker
        .root
        .join(".git/arc/changes")
        .join(change)
        .join("events");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nif [ \"$1\" = committed ]; then chmod 555 '{}'; fi\n",
            events.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    intent
}

fn repair_provisional_publication(worker: &Repo, intent: &Path) -> serde_json::Value {
    use std::os::unix::fs::PermissionsExt;
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(intent).unwrap()).unwrap();
    let event = &saved["event"];
    let blocked = worker
        .root
        .join(".git/arc/changes")
        .join(event["change_id"].as_str().unwrap())
        .join("events")
        .join(format!("{}.json", event["event_id"].as_str().unwrap()));
    if blocked.is_dir() {
        fs::remove_dir(blocked).unwrap();
    } else {
        fs::set_permissions(blocked.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::remove_file(worker.root.join(".git/hooks/reference-transaction")).unwrap();
    saved
}

fn import_replica_facts(source: &Repo, receiver: &Repo) {
    let path = source.home.join("authority-transition.json");
    stdout(source.arc(&source.root).args([
        "replica",
        "export",
        "--output",
        path.to_str().unwrap(),
    ]));
    let bytes = fs::read(&path).unwrap();
    let bundle: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let exporter = bundle["source_replica_id"].as_str().unwrap();
    let origins: std::collections::BTreeSet<_> = bundle["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["repository_id"].as_str().unwrap())
        .collect();
    let mut policy = format!(
        "[[replica.publishers]]\nprincipal='authority-transfer'\nsource_replica_id='{exporter}'\n"
    );
    for origin in origins {
        policy.push_str(&format!("[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles=['authority']\nactors=['tester']\n"));
    }
    receiver.declare_gates_locally(&policy);
    stdout(receiver.arc(&receiver.root).args([
        "replica",
        "import",
        path.to_str().unwrap(),
        "--publisher",
        "authority-transfer",
        "--received-sha256",
        &hex::encode(Sha256::digest(bytes)),
    ]));
}

#[test]
fn failed_publication_reports_effects_and_discoverable_intents() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    let intent = block_provisional_publication(&worker, &change);
    let failed = worker
        .arc(&worker.root)
        .args(["integrate", &change, "--provisional"])
        .assert()
        .code(1);
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&intent).unwrap()).unwrap();
    let event = &saved["event"];
    let merge = event["provisional_commit"].as_str().unwrap();
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&failed.get_output().stdout),
        String::from_utf8_lossy(&failed.get_output().stderr)
    );
    for expected in [
        merge,
        "refs/heads/arc/provisional/worker/master",
        &format!(
            "refs/arc/provisional/{change}/{}",
            event["event_id"].as_str().unwrap()
        ),
        intent.to_str().unwrap(),
        &format!("arc integrate {change} --provisional"),
    ] {
        assert!(
            diagnostic.contains(expected),
            "missing {expected}: {diagnostic}"
        );
    }
    assert!(diagnostic.contains("event state=absent"), "{diagnostic}");
    let status = status(&worker, &change);
    assert_eq!(
        status["unfinished_provisional_integrations"][0]["event_id"],
        event["event_id"]
    );
    assert!(
        stdout(worker.arc(&worker.root).args(["show", &change])).contains("unfinished provisional")
    );
    let doctor = json_stdout_any_status(worker.arc(&worker.root).args(["doctor", "--json"]));
    assert_eq!(
        doctor["unfinished_provisional_integrations"][0]["event_id"],
        event["event_id"]
    );
    assert!(doctor["problems"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["code"] == "unfinished-provisional-integration"));
    let workspace = json_stdout(
        worker
            .arc(&worker.root)
            .args(["workspace", "report", "--json"]),
    );
    assert_eq!(
        workspace["sections"]["unfinished_provisional_integrations"][0]["event_id"],
        event["event_id"]
    );
    assert!(
        stdout(worker.arc(&worker.root).args(["workspace", "report"]))
            .contains("Unfinished provisional integrations")
    );
    repair_provisional_publication(&worker, &intent);
    prepare(&worker, &change);
}

#[test]
fn failed_publication_has_structured_effects_in_execution_json() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    let intent = block_provisional_event_observation(&worker, &change);
    let failed = worker
        .arc(&worker.root)
        .args(["integrate", &change, "--provisional", "--json"])
        .assert()
        .code(1);
    let output: serde_json::Value = serde_json::from_slice(&failed.get_output().stdout).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&intent).unwrap()).unwrap();
    assert_eq!(output["schema"], "arc-provisional-transition/1");
    assert_eq!(output["exit_code"], 1);
    assert_eq!(output["transition"]["applied"], true);
    assert_eq!(output["transition"]["event_state"], "unknown");
    assert_eq!(output["transition"]["event_id"], saved["event"]["event_id"]);
    assert_eq!(
        output["transition"]["staging_head"],
        saved["event"]["provisional_commit"]
    );
    assert_eq!(
        output["transition"]["retention_head"],
        saved["event"]["provisional_commit"]
    );
    assert_eq!(
        output["transition"]["intent_path"],
        intent.to_str().unwrap()
    );
    repair_provisional_publication(&worker, &intent);
    let recovered = json_stdout(worker.arc(&worker.root).args([
        "integrate",
        &change,
        "--provisional",
        "--json",
    ]));
    assert_eq!(recovered["exit_code"], 0);
    assert_eq!(recovered["transition"]["event_state"], "published");
    assert!(!intent.exists());
}

#[test]
fn applied_intent_recovers_before_changed_authority_or_mode_eligibility() {
    for mode in ["authority", "disabled"] {
        let (authority, worker) = worker();
        let change = prepared(&worker);
        let intent = block_provisional_publication(&worker, &change);
        worker
            .arc(&worker.root)
            .args(["integrate", &change, "--provisional"])
            .assert()
            .code(1);
        let saved = repair_provisional_publication(&worker, &intent);
        if mode == "authority" {
            stdout(authority.arc(&authority.root).args([
                "replica",
                "authority",
                "offer",
                "--to",
                "worker",
            ]));
            import_replica_facts(&authority, &worker);
        } else {
            worker.declare_gates_locally("[replica]\nprovisional_enabled=false\n");
        }
        prepare(&worker, &change);
        assert!(!intent.exists());
        let row = obligation(&worker, &change);
        assert_eq!(row["event_id"], saved["event"]["event_id"]);
        let event_path = worker
            .root
            .join(".git/arc/changes")
            .join(&change)
            .join("events")
            .join(format!("{}.json", row["event_id"].as_str().unwrap()));
        let published: serde_json::Value =
            serde_json::from_slice(&fs::read(event_path).unwrap()).unwrap();
        assert_eq!(published, saved["event"]);
        assert_eq!(
            git_out(
                &worker.root,
                &["rev-parse", "arc/provisional/worker/master"]
            ),
            saved["event"]["provisional_commit"].as_str().unwrap()
        );
    }
}

#[test]
fn applied_intent_recovers_without_resetting_a_later_preparation() {
    let (_authority, worker) = worker();
    let change = prepared(&worker);
    let intent = block_provisional_publication(&worker, &change);
    worker
        .arc(&worker.root)
        .args(["integrate", &change, "--provisional"])
        .assert()
        .code(1);
    let saved = repair_provisional_publication(&worker, &intent);
    let (later, _, _) = change_with_patchset(&worker, "later");
    approve(&worker, &later);
    prepare(&worker, &later);
    let advanced = git_out(
        &worker.root,
        &["rev-parse", "arc/provisional/worker/master"],
    );
    assert_ne!(
        advanced,
        saved["event"]["provisional_commit"].as_str().unwrap()
    );
    prepare(&worker, &change);
    assert!(!intent.exists());
    assert_eq!(
        obligation(&worker, &change)["event_id"],
        saved["event"]["event_id"]
    );
    assert_eq!(
        git_out(
            &worker.root,
            &["rev-parse", "arc/provisional/worker/master"]
        ),
        advanced
    );
    assert_eq!(obligation(&worker, &later)["provisional_commit"], advanced);
}

#[test]
fn an_authority_refusal_survives_transfer_and_still_blocks_stacking() {
    let (authority, worker) = worker();
    let successor = Repo::new();
    let successor_id = json_stdout(
        successor
            .arc(&successor.root)
            .args(["replica", "id", "--json"]),
    );
    stdout(authority.arc(&authority.root).args([
        "replica",
        "pair",
        "successor",
        "--repository-id",
        successor_id["repository_id"].as_str().unwrap(),
    ]));
    import_replica_facts(&authority, &worker);
    import_replica_facts(&authority, &successor);
    let prerequisite = prepared(&worker);
    prepare(&worker, &prerequisite);
    let (dependent, _) = dependent(&worker, &prerequisite);
    deliver(&worker, &authority, &prerequisite, true);
    stdout(authority.arc(&authority.root).args([
        "review",
        &prerequisite,
        "--verdict",
        "changes-requested",
        "--cause",
        "executor",
    ]));
    deliver(&authority, &worker, &prerequisite, false);
    let refused = obligation(&worker, &prerequisite);
    assert_eq!(refused["disposition"], "authority-refused");
    stdout(authority.arc(&authority.root).args([
        "replica",
        "authority",
        "offer",
        "--to",
        "successor",
    ]));
    import_replica_facts(&authority, &successor);
    import_replica_facts(&successor, &worker);
    let after = obligation(&worker, &prerequisite);
    assert_eq!(after["disposition"], "authority-refused");
    assert_eq!(after["refusal_event_id"], refused["refusal_event_id"]);
    worker
        .arc(&worker.root)
        .args(["check", &dependent, "--provisional"])
        .assert()
        .code(7);
    worker
        .arc(&worker.root)
        .args(["integrate", &dependent, "--provisional"])
        .assert()
        .code(7);
    assert!(stdout(worker.arc(&worker.root).args(["catchup"]))
        .contains(refused["refusal_event_id"].as_str().unwrap()));
}

#[test]
fn trusted_foreign_approval_cannot_overcome_an_authority_local_refusal() {
    let (authority, worker) = worker();
    let change = prepared(&worker);
    deliver(&worker, &authority, &change, true);
    stdout(
        authority
            .arc(&authority.root)
            .env("ARC_ACTOR", "authority-reviewer")
            .args([
                "review",
                &change,
                "--verdict",
                "changes-requested",
                "--cause",
                "executor",
            ]),
    );
    let refused = status(&authority, &change);
    let local = refused["verdict"]["event_id"].as_str().unwrap().to_string();
    deliver(&authority, &worker, &change, false);
    stdout(worker.arc(&worker.root).env("ARC_ACTOR", "reviewer").args([
        "review",
        &change,
        "--verdict",
        "approved",
    ]));
    let path = worker.home.join("foreign-after-local-refusal.json");
    stdout(
        worker
            .arc(&worker.root)
            .args(["export", &change, "--output", path.to_str().unwrap()]),
    );
    let mut bundle: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let origin = bundle["source_replica_id"].as_str().unwrap().to_string();
    let approval = bundle["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .rev()
        .find(|event| event["event_type"] == "verdict-recorded" && event["repository_id"] == origin)
        .unwrap();
    approval["relation"] = serde_json::json!({ "kind": "supersedes", "observed": [local] });
    refresh_bundle_checksum(&mut bundle);
    let bytes = serde_json::to_vec(&bundle).unwrap();
    fs::write(&path, &bytes).unwrap();
    stdout(authority.arc(&authority.root).args([
        "import",
        path.to_str().unwrap(),
        "--publisher",
        "delivery",
        "--received-sha256",
        &hex::encode(Sha256::digest(bytes)),
    ]));
    let received = status(&authority, &change);
    assert_eq!(received["has_valid_approval"], false);
    assert_eq!(received["verdict"]["event_id"], local);
    authority
        .arc(&authority.root)
        .args(["check", &change])
        .assert()
        .code(3);
    authority
        .arc(&authority.root)
        .args(["integrate", &change])
        .assert()
        .code(3);
}

fn hour_ahead_clock(repo: &Repo) -> PathBuf {
    let source = repo.home.join("hour-ahead.c");
    let library = repo.home.join("hour-ahead.so");
    fs::write(
        &source,
        r#"#define _GNU_SOURCE
#include <dlfcn.h>
#include <time.h>
int clock_gettime(clockid_t clock, struct timespec *value) {
    static int (*original)(clockid_t, struct timespec *);
    if (!original) original = dlsym(RTLD_NEXT, "clock_gettime");
    int result = original(clock, value);
    if (!result && clock == CLOCK_REALTIME) value->tv_sec += 3600;
    return result;
}
"#,
    )
    .unwrap();
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC"])
        .arg(&source)
        .arg("-o")
        .arg(&library)
        .arg("-ldl")
        .status()
        .unwrap()
        .success());
    library
}

#[test]
fn a_clock_ahead_previous_grant_approval_cannot_clear_an_unobserved_rejection() {
    let (authority, worker) = worker();
    let successor = Repo::new();
    let successor_id = json_stdout(
        successor
            .arc(&successor.root)
            .args(["replica", "id", "--json"]),
    );
    stdout(authority.arc(&authority.root).args([
        "replica",
        "pair",
        "successor",
        "--repository-id",
        successor_id["repository_id"].as_str().unwrap(),
    ]));
    import_replica_facts(&authority, &successor);
    import_replica_facts(&authority, &worker);
    let prerequisite = prepared(&worker);
    prepare(&worker, &prerequisite);
    let (dependent, _) = dependent(&worker, &prerequisite);
    deliver(&worker, &authority, &prerequisite, false);
    stdout(
        authority
            .arc(&authority.root)
            .env("LD_PRELOAD", hour_ahead_clock(&authority))
            .env("ARC_ACTOR", "authority-reviewer")
            .args(["review", &prerequisite, "--verdict", "approved"]),
    );
    let older = json_stdout(
        authority
            .arc(&authority.root)
            .args(["show", &prerequisite, "--json"]),
    );
    let approval = older["verdicts"].as_array().unwrap().last().unwrap();
    assert!(approval["relation"].is_null());
    let approval_id = approval["event_id"].as_str().unwrap().to_string();
    stdout(authority.arc(&authority.root).args([
        "replica",
        "authority",
        "offer",
        "--to",
        "successor",
    ]));
    import_replica_facts(&authority, &successor);
    import_replica_facts(&successor, &worker);
    deliver(&worker, &successor, &prerequisite, false);
    stdout(successor.arc(&successor.root).args([
        "review",
        &prerequisite,
        "--verdict",
        "changes-requested",
        "--cause",
        "executor",
    ]));
    deliver(&successor, &worker, &prerequisite, false);
    let refused = obligation(&worker, &prerequisite);
    let rejection = refused["refusal_event_id"].as_str().unwrap();
    assert!(
        approval_id.as_str() > rejection,
        "the earlier approval has a higher clock-derived ID"
    );
    worker
        .arc(&worker.root)
        .args(["check", &dependent, "--provisional"])
        .assert()
        .code(7);
    deliver(&authority, &worker, &prerequisite, false);
    let after = obligation(&worker, &prerequisite);
    assert_eq!(after["disposition"], "authority-refused");
    assert_eq!(after["refusal_event_id"], rejection);
    worker
        .arc(&worker.root)
        .args(["check", &dependent, "--provisional"])
        .assert()
        .code(7);
    worker
        .arc(&worker.root)
        .args(["integrate", &dependent, "--provisional"])
        .assert()
        .code(7);
    approve(&successor, &prerequisite);
    deliver(&successor, &worker, &prerequisite, false);
    worker
        .arc(&worker.root)
        .args(["check", &dependent, "--provisional"])
        .assert()
        .success();
}
