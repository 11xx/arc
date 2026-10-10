use super::common::*;

#[test]
fn enrolled_import_requires_exact_received_provenance_without_writes() {
    let source = Repo::new();
    stdout(
        source
            .arc(&source.root)
            .args(["begin", "admission", "--no-worktree"]),
    );
    let input = source.home.join("bundle.json");
    stdout(source.arc(&source.root).args([
        "export",
        "admission",
        "--output",
        input.to_str().unwrap(),
    ]));
    let bundle: serde_json::Value = serde_json::from_slice(&fs::read(&input).unwrap()).unwrap();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    receiver.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{}'\n[[replica.publishers.origins]]\nreplica_id='{}'\nroles=['facts']\nactors=[]\n", bundle["repository_id"].as_str().unwrap(), bundle["repository_id"].as_str().unwrap()));
    for preview in [false, true] {
        let mut cmd = receiver.arc(&receiver.root);
        cmd.args(["import", input.to_str().unwrap()]);
        if preview {
            cmd.arg("--dry-run");
        }
        cmd.assert()
            .code(18)
            .stderr(predicates::str::contains("--publisher"));
        let mut cmd = receiver.arc(&receiver.root);
        cmd.args([
            "import",
            input.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &"0".repeat(64),
        ]);
        if preview {
            cmd.arg("--dry-run");
        }
        cmd.assert()
            .code(18)
            .stderr(predicates::str::contains("digest"));
    }
    assert!(!receiver.root.join(".git/arc/config.json").exists());
    assert!(!receiver.root.join(".git/arc/changes").exists());
}

#[test]
fn project_policy_cannot_enroll_publishers() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[replica]\ntrusted_review_replicas=[]\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["policy", "show"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(".arc/policy.toml"));
}

fn exported_review() -> (Repo, PathBuf, serde_json::Value) {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "review-origin"]));
    let tree = repo.home.join(".worktrees/repo-review-origin");
    repo.commit(&tree, "feature.txt", "feature\n", "feat: feature");
    stdout(
        repo.arc(&tree)
            .args(["snapshot", "review-origin", "--solo"]),
    );
    stdout(repo.arc(&tree).env("ARC_ACTOR", "operator").args([
        "review",
        "review-origin",
        "--verdict",
        "approved",
    ]));
    let path = repo.home.join("bundle.json");
    stdout(repo.arc(&repo.root).args([
        "export",
        "review-origin",
        "--output",
        path.to_str().unwrap(),
    ]));
    let value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    (repo, path, value)
}

fn enroll(repo: &Repo, bundle: &serde_json::Value, roles: &[&str], actors: &[&str], trust: bool) {
    let source = bundle["source_replica_id"]
        .as_str()
        .unwrap_or_else(|| bundle["repository_id"].as_str().unwrap());
    let origin = bundle["repository_id"].as_str().unwrap();
    repo.declare_gates_locally(&format!("[replica]\ntrusted_review_replicas={}\n[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{source}'\n[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles={}\nactors={}\n", serde_json::to_string(&if trust { vec![origin] } else { vec![] }).unwrap(), serde_json::to_string(roles).unwrap(), serde_json::to_string(actors).unwrap()));
}

fn received(repo: &Repo, path: &Path, dry_run: bool) -> AssertCommand {
    let digest = hex::encode(Sha256::digest(fs::read(path).unwrap()));
    let mut cmd = repo.arc(&repo.root);
    cmd.args([
        "import",
        path.to_str().unwrap(),
        "--publisher",
        "channel",
        "--received-sha256",
        &digest,
    ]);
    if dry_run {
        cmd.arg("--dry-run");
    }
    cmd
}

fn bind_branch(receiver: &Repo, source: &Repo, bundle: &serde_json::Value) {
    let branch = bundle["events"][0]["branch"].as_str().unwrap();
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
}

fn write_bundle(path: &Path, bundle: &mut serde_json::Value) {
    let mut hasher = Sha256::new();
    for event in bundle["events"].as_array().unwrap() {
        hasher.update(serde_json::to_vec(event).unwrap());
        hasher.update(b"\n");
    }
    bundle["events_sha256"] = serde_json::json!(hex::encode(hasher.finalize()));
    bundle["event_count"] = serde_json::json!(bundle["events"].as_array().unwrap().len());
    fs::write(path, serde_json::to_vec(bundle).unwrap()).unwrap();
}

fn status(receiver: &Repo, bundle: &serde_json::Value) -> serde_json::Value {
    json_stdout(receiver.arc(&receiver.root).args([
        "status",
        bundle["change_id"].as_str().unwrap(),
        "--json",
    ]))
}

#[test]
fn input_verdict_preserves_operator_claim_without_review_credit() {
    let (source, path, bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(&receiver, &bundle, &["facts", "review-input"], &[], false);
    received(&receiver, &path, false).assert().success();
    let state = status(&receiver, &bundle);
    assert_eq!(state["has_valid_approval"], false, "{state}");
    let assessments = state["verdict_assessments"].as_array().unwrap();
    assert_eq!(assessments[0]["origin_replica_id"], bundle["repository_id"]);
    assert_eq!(assessments[0]["actor"], "operator");
    assert_eq!(assessments[0]["admission_provenance"], "transport-asserted");
    assert_eq!(assessments[0]["covers_current_head"], true);
    assert_eq!(assessments[0]["gating_eligible"], false);
    assert!(state["review_map"]
        .as_array()
        .is_none_or(|rows| rows.iter().all(|row| row["reviewer"] != "operator")));
    receiver
        .arc(&receiver.root)
        .args(["check", bundle["change_id"].as_str().unwrap()])
        .assert()
        .code(3)
        .stdout(predicates::str::contains(
            bundle["repository_id"].as_str().unwrap(),
        ));
}

#[test]
fn privileged_actor_and_subject_claims_require_allowlisting() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority"],
        &["worker"],
        true,
    );
    for preview in [false, true] {
        received(&receiver, &path, preview)
            .assert()
            .code(18)
            .stderr(predicates::str::contains("operator"));
    }
    let verdict = bundle["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "verdict-recorded")
        .unwrap();
    verdict["actor"] = serde_json::json!("worker");
    verdict["on_behalf_of"] = serde_json::json!("operator");
    write_bundle(&path, &mut bundle);
    received(&receiver, &path, false)
        .assert()
        .code(18)
        .stderr(predicates::str::contains("subject"));
    assert!(!receiver.root.join(".git/arc/config.json").exists());
}

#[test]
fn digest_principal_exporter_and_origin_refuse_preview_and_execution() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    enroll(&receiver, &bundle, &["facts", "review-input"], &[], false);
    for preview in [false, true] {
        let digest = hex::encode(Sha256::digest(fs::read(&path).unwrap()));
        let mut cmd = receiver.arc(&receiver.root);
        cmd.args([
            "import",
            path.to_str().unwrap(),
            "--publisher",
            "stranger",
            "--received-sha256",
            &digest,
        ]);
        if preview {
            cmd.arg("--dry-run");
        }
        cmd.assert()
            .code(18)
            .stderr(predicates::str::contains("stranger"));
    }
    let original = bundle.clone();
    bundle["source_replica_id"] = serde_json::json!("wrong-exporter");
    write_bundle(&path, &mut bundle);
    for preview in [false, true] {
        received(&receiver, &path, preview)
            .assert()
            .code(18)
            .stderr(predicates::str::contains("exporter"));
    }
    bundle = original;
    let verdict = bundle["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "verdict-recorded")
        .unwrap();
    verdict["repository_id"] = serde_json::json!("forged-authority-origin");
    write_bundle(&path, &mut bundle);
    for preview in [false, true] {
        received(&receiver, &path, preview)
            .assert()
            .code(18)
            .stderr(predicates::str::contains("forged-authority-origin"));
    }
    assert!(!receiver.root.join(".git/arc/config.json").exists());
    assert!(!receiver.root.join(".git/arc/imports").exists());
}

#[test]
fn foreign_review_requires_capability_and_current_replica_trust() {
    let (source, path, bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority"],
        &["operator"],
        false,
    );
    received(&receiver, &path, false).assert().success();
    assert_eq!(status(&receiver, &bundle)["has_valid_approval"], false);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority"],
        &["operator"],
        true,
    );
    let state = status(&receiver, &bundle);
    assert_eq!(state["has_valid_approval"], true, "{state}");
    let check = json_stdout(receiver.arc(&receiver.root).args([
        "check",
        bundle["change_id"].as_str().unwrap(),
        "--json",
    ]));
    assert_eq!(check["schema"], "arc-check/4");
    assert_eq!(check["verdict_assessments"], state["verdict_assessments"]);
    assert_eq!(
        check["selected_authorizing_event"],
        state["verdict_assessments"][0]["event_id"]
    );
}

#[test]
fn trusted_foreign_approval_cannot_replace_local_refusal() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority"],
        &["operator"],
        true,
    );
    received(&receiver, &path, false).assert().success();
    let change = bundle["change_id"].as_str().unwrap().to_string();
    stdout(
        receiver
            .arc(&receiver.root)
            .env("ARC_ACTOR", "local-reviewer")
            .args([
                "review",
                &change,
                "--verdict",
                "changes-requested",
                "--cause",
                "executor",
            ]),
    );
    let refusal = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["show", &change, "--json"]),
    );
    let local_id = refusal["verdicts"].as_array().unwrap().last().unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    let verdict = bundle["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "verdict-recorded")
        .unwrap();
    verdict["event_id"] = serde_json::json!("ZZZZ-foreign-approval");
    verdict["relation"] = serde_json::json!({"kind":"supersedes", "observed":[local_id]});
    write_bundle(&path, &mut bundle);
    received(&receiver, &path, false).assert().success();
    let state = status(&receiver, &bundle);
    assert_eq!(state["has_valid_approval"], false, "{state}");
    assert_eq!(state["verdict"]["actor"], "local-reviewer", "{state}");
}

#[test]
fn relay_export_preserves_origin_and_identifies_the_exporter() {
    let (source, path, bundle) = exported_review();
    let relay = Repo::new();
    deliver_git_objects(&source, &relay);
    stdout(
        relay
            .arc(&relay.root)
            .args(["import", path.to_str().unwrap()]),
    );
    let output = relay.home.join("relay.json");
    stdout(relay.arc(&relay.root).args([
        "export",
        bundle["change_id"].as_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]));
    let relayed: serde_json::Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    let identity = json_stdout(relay.arc(&relay.root).args(["replica", "id", "--json"]));
    assert_eq!(relayed["source_replica_id"], identity["repository_id"]);
    assert_eq!(relayed["repository_id"], bundle["repository_id"]);
    assert_eq!(relayed["events"], bundle["events"]);
    assert!(!relayed.to_string().contains("transport-asserted"));
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    enroll(&receiver, &relayed, &["facts", "review-input"], &[], false);
    received(&receiver, &output, false).assert().success();
    let show = json_stdout(receiver.arc(&receiver.root).args([
        "show",
        bundle["change_id"].as_str().unwrap(),
        "--json",
    ]));
    assert_eq!(
        show["verdicts"][0]["origin_replica_id"],
        bundle["repository_id"]
    );
}

#[test]
fn unknown_final_payload_refuses_the_whole_batch_and_temporary_batches_are_invisible() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    enroll(&receiver, &bundle, &["facts", "review-input"], &[], false);
    let mut unknown = bundle["events"][0].clone();
    unknown["event_id"] = serde_json::json!("ZZZZ-unknown");
    unknown["event_type"] = serde_json::json!("future-privileged-event");
    bundle["events"].as_array_mut().unwrap().push(unknown);
    write_bundle(&path, &mut bundle);
    for preview in [false, true] {
        received(&receiver, &path, preview)
            .assert()
            .code(18)
            .stderr(predicates::str::contains("unclassified"));
    }
    assert!(!receiver.root.join(".git/arc/config.json").exists());
    bundle["events"].as_array_mut().unwrap().pop();
    write_bundle(&path, &mut bundle);
    stdout(receiver.arc(&receiver.root).args(["replica", "id"]));
    let imports = receiver.root.join(".git/arc/imports");
    fs::create_dir_all(&imports).unwrap();
    fs::write(imports.join(".unpublished.tmp"), b"{partial batch").unwrap();
    assert!(
        json_stdout(receiver.arc(&receiver.root).args(["list", "--json"]))
            .as_array()
            .unwrap()
            .is_empty()
    );
    received(&receiver, &path, false).assert().success();
    let records: Vec<_> = fs::read_dir(&imports)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path
                .extension()
                .is_some_and(|extension| extension == "json"))
            .then(|| serde_json::from_slice::<serde_json::Value>(&fs::read(path).unwrap()).unwrap())
        })
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["schema"], "arc-import-batch/1");
    assert_eq!(records[0]["events"], bundle["events"]);
    assert_eq!(
        records[0]["authorization"]["events"]
            .as_array()
            .unwrap()
            .len(),
        bundle["events"].as_array().unwrap().len()
    );
    received(&receiver, &path, false).assert().success();
    assert_eq!(fs::read_dir(imports).unwrap().count(), 2);
}

#[test]
fn input_debt_and_untrusted_audit_do_not_waive_or_discharge_review() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    let patchset = bundle["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "patchset-added")
        .unwrap()
        .clone();
    let mut debt = bundle["events"][0].clone();
    debt["event_id"] = serde_json::json!("ZZZZ-001-debt");
    debt["event_type"] = serde_json::json!("debt-declared");
    debt["reason"] = serde_json::json!("independent review owed");
    debt["missing"] = serde_json::json!("independent-review");
    debt["patchset_id"] = patchset["patchset_id"].clone();
    // An input-only obligation cannot authorize the open change.
    bundle["events"].as_array_mut().unwrap().push(debt);
    write_bundle(&path, &mut bundle);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-input", "debt-input"],
        &[],
        false,
    );
    received(&receiver, &path, false).assert().success();
    assert_eq!(status(&receiver, &bundle)["has_valid_approval"], false);
    let mut closure = bundle["events"][0].clone();
    closure["event_id"] = serde_json::json!("ZZZZ-002-integration");
    closure["event_type"] = serde_json::json!("change-integrated");
    closure["integrated_commit"] = patchset["head"].clone();
    closure["source_patchset_id"] = patchset["patchset_id"].clone();
    closure["source_head"] = patchset["head"].clone();
    closure["target_before"] = patchset["base"].clone();
    let mut audit = bundle["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "verdict-recorded")
        .unwrap()
        .clone();
    audit["event_id"] = serde_json::json!("ZZZZ-003-audit");
    audit["event_type"] = serde_json::json!("audit-verdict-recorded");
    audit["revision"] = patchset["head"].clone();
    // The audit's time is later than the obligation.
    audit["created_at"] = serde_json::json!("2099-01-01T00:00:00Z");
    bundle["events"]
        .as_array_mut()
        .unwrap()
        .extend([closure, audit]);
    write_bundle(&path, &mut bundle);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-input", "debt-input", "integration"],
        &["tester", "operator"],
        false,
    );
    received(&receiver, &path, false).assert().success();
    let show = json_stdout(receiver.arc(&receiver.root).args([
        "show",
        bundle["change_id"].as_str().unwrap(),
        "--json",
    ]));
    assert!(show["debt"]["discharged_by"].is_null(), "{show}");
    assert_eq!(
        show["closure"]["origin_replica_id"],
        bundle["repository_id"]
    );
}

#[test]
fn untrusted_corroboration_cannot_clear_local_provisional_approval() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(&receiver, &bundle, &["facts", "review-input"], &[], false);
    received(&receiver, &path, false).assert().success();
    let change = bundle["change_id"].as_str().unwrap().to_string();
    stdout(
        receiver
            .arc(&receiver.root)
            .env("ARC_ACTOR", "local-reviewer")
            .args([
                "review",
                &change,
                "--verdict",
                "approved",
                "--provisional",
                "corroboration owed",
            ]),
    );
    let verdict = bundle["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "verdict-recorded")
        .unwrap();
    verdict["event_id"] = serde_json::json!("ZZZZ-later-foreign-verdict");
    verdict["created_at"] = serde_json::json!("2099-01-01T00:00:00Z");
    write_bundle(&path, &mut bundle);
    received(&receiver, &path, false).assert().success();
    let state = status(&receiver, &bundle);
    assert_eq!(state["provisional_approval_outstanding"], true, "{state}");
}

#[test]
fn legacy_event_files_and_imports_have_unknown_admission_and_no_foreign_approval() {
    let (source, path, bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    stdout(receiver.arc(&receiver.root).args(["replica", "id"]));
    let events = receiver
        .root
        .join(".git/arc/changes")
        .join(bundle["change_id"].as_str().unwrap())
        .join("events");
    fs::create_dir_all(&events).unwrap();
    for event in bundle["events"].as_array().unwrap() {
        fs::write(
            events.join(format!("{}.json", event["event_id"].as_str().unwrap())),
            serde_json::to_vec(event).unwrap(),
        )
        .unwrap();
    }
    let state = status(&receiver, &bundle);
    assert_eq!(
        state["verdict_assessments"][0]["admission_provenance"],
        "legacy-unknown"
    );
    assert_eq!(state["has_valid_approval"], false);
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["import", path.to_str().unwrap()]),
    );
    assert_eq!(
        status(&receiver, &bundle)["verdict_assessments"][0]["admission_provenance"],
        "legacy-unknown"
    );
    assert!(!receiver.root.join(".git/arc/imports").exists());
}

#[test]
fn paired_imports_require_enrollment_and_project_identity() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "init", "authority"]),
    );
    let replica = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "status", "--json"]),
    );
    stdout(receiver.arc(&receiver.root).args([
        "replica",
        "pair",
        "worker",
        "--repository-id",
        bundle["repository_id"].as_str().unwrap(),
    ]));
    for preview in [false, true] {
        let mut cmd = receiver.arc(&receiver.root);
        cmd.args(["import", path.to_str().unwrap()]);
        if preview {
            cmd.arg("--dry-run");
        }
        cmd.assert()
            .code(18)
            .stderr(predicates::str::contains("--publisher"));
    }
    enroll(&receiver, &bundle, &["facts", "review-input"], &[], false);
    received(&receiver, &path, false)
        .assert()
        .code(18)
        .stderr(predicates::str::contains("project"));
    bundle["project_id"] = replica["project_id"].clone();
    write_bundle(&path, &mut bundle);
    received(&receiver, &path, false).assert().success();
}

#[test]
fn replica_receipts_read_previous_schemas_without_authentication() {
    let origin = Repo::new();
    let receiver = Repo::new();
    let identity = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "id", "--json"]),
    );
    stdout(origin.arc(&origin.root).args(["replica", "init", "origin"]));
    stdout(origin.arc(&origin.root).args([
        "replica",
        "pair",
        "receiver",
        "--repository-id",
        identity["repository_id"].as_str().unwrap(),
    ]));
    let path = origin.home.join("replica.json");
    stdout(origin.arc(&origin.root).args([
        "replica",
        "export",
        "--output",
        path.to_str().unwrap(),
    ]));
    // An unenrolled, unpaired receiver can structurally admit a legacy transfer.
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "import", path.to_str().unwrap()]),
    );
    let receipt = fs::read_dir(receiver.root.join(".git/arc/replicas/imports"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(record["schema"], "arc-replica-import/3");
    assert!(record["authorization"].is_null());
    for schema in ["arc-replica-import/1", "arc-replica-import/2"] {
        record["schema"] = serde_json::json!(schema);
        record.as_object_mut().unwrap().remove("authorization");
        fs::write(&receipt, serde_json::to_vec(&record).unwrap()).unwrap();
        let state = json_stdout(
            receiver
                .arc(&receiver.root)
                .args(["replica", "status", "--json"]),
        );
        assert_eq!(state["local"]["name"], "receiver");
        assert!(!state.to_string().contains("transport-asserted"));
    }
}

#[test]
fn trusted_review_still_requires_independent_declared_identity() {
    let (source, path, original) = exported_review();
    for (actor, actor_source, reason) in [
        ("tester", "env", "contributor"),
        ("reviewer", "git-fallback", "assumed"),
    ] {
        let receiver = Repo::new();
        deliver_git_objects(&source, &receiver);
        let mut bundle = original.clone();
        let verdict = bundle["events"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|event| event["event_type"] == "verdict-recorded")
            .unwrap();
        verdict["actor"] = serde_json::json!(actor);
        verdict["actor_source"] = serde_json::json!(actor_source);
        write_bundle(&path, &mut bundle);
        bind_branch(&receiver, &source, &bundle);
        enroll(
            &receiver,
            &bundle,
            &["facts", "review-authority"],
            &[actor],
            true,
        );
        let policy = receiver.root.join(".git/arc/operator-policy.toml");
        let mut text = fs::read_to_string(&policy).unwrap();
        text.push_str("\n[policy]\nforbid_self_approval=true\n[danger]\npaths=['feature.txt']\n");
        fs::write(policy, text).unwrap();
        received(&receiver, &path, false).assert().success();
        let state = status(&receiver, &bundle);
        assert_eq!(state["has_valid_approval"], false, "{state}");
        assert!(
            state["approval_rejection_reason"]
                .as_str()
                .unwrap()
                .contains(reason),
            "{state}"
        );
        assert_eq!(state["verdict_assessments"][0]["gating_eligible"], false);
    }
}

#[test]
fn foreign_edges_cannot_settle_a_contested_local_verdict_graph() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority"],
        &["operator"],
        true,
    );
    received(&receiver, &path, false).assert().success();
    let change = bundle["change_id"].as_str().unwrap().to_string();
    stdout(
        receiver
            .arc(&receiver.root)
            .env("ARC_ACTOR", "local-reviewer")
            .args(["review", &change, "--verdict", "approved"]),
    );
    let files = receiver
        .root
        .join(".git/arc/changes")
        .join(&change)
        .join("events");
    let local_path = fs::read_dir(&files)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let local: serde_json::Value = serde_json::from_slice(&fs::read(&local_path).unwrap()).unwrap();
    let local_id = local["event_id"].as_str().unwrap();
    let mut fork = local.clone();
    fork["event_id"] = serde_json::json!("ZZZZ-001-local-fork");
    fork["actor"] = serde_json::json!("other-local-reviewer");
    fs::write(
        files.join("ZZZZ-001-local-fork.json"),
        serde_json::to_vec(&fork).unwrap(),
    )
    .unwrap();
    assert_eq!(status(&receiver, &bundle)["verdict_contested"], true);
    let foreign = bundle["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "verdict-recorded")
        .unwrap();
    foreign["event_id"] = serde_json::json!("ZZZZ-002-foreign-resolution");
    foreign["relation"] =
        serde_json::json!({"kind":"supersedes", "observed":[local_id, "ZZZZ-001-local-fork"]});
    write_bundle(&path, &mut bundle);
    received(&receiver, &path, false).assert().success();
    let state = status(&receiver, &bundle);
    assert_eq!(state["has_valid_approval"], false);
    assert_eq!(state["verdict_contested"], true);
    assert_eq!(state["recorded_verdict_tips"].as_array().unwrap().len(), 1);
    assert_eq!(state["eligible_verdict_tips"].as_array().unwrap().len(), 2);
}

#[test]
fn full_and_delta_batches_and_repository_only_deltas_are_idempotent() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-input", "rewrite"],
        &[],
        false,
    );
    received(&receiver, &path, false).assert().success();
    let original = bundle.clone();
    let mut suffix = bundle["events"][0].clone();
    suffix["event_id"] = serde_json::json!("ZZZZ-001-comment");
    suffix["event_type"] = serde_json::json!("comment-added");
    suffix["body"] = serde_json::json!("relay fact");
    suffix["patchset_id"] = serde_json::Value::Null;
    bundle["events"]
        .as_array_mut()
        .unwrap()
        .push(suffix.clone());
    write_bundle(&path, &mut bundle);
    let complete_digest = bundle["events_sha256"].clone();
    bundle["since"] = serde_json::json!({"sha256":original["events_sha256"], "event_count":original["event_count"]});
    bundle["events"] = serde_json::json!([suffix]);
    bundle["event_count"] = serde_json::json!(1);
    bundle["events_sha256"] = complete_digest.clone();
    fs::write(&path, serde_json::to_vec(&bundle).unwrap()).unwrap();
    received(&receiver, &path, false).assert().success();
    received(&receiver, &path, false).assert().success();
    let mut rewrite = original["events"][0].clone();
    rewrite["event_id"] = serde_json::json!("ZZZZ-002-rewrite");
    rewrite["change_id"] = serde_json::json!("repository");
    rewrite["event_type"] = serde_json::json!("history-rewritten");
    rewrite["mapping"] = serde_json::json!({"1111111111111111111111111111111111111111":"2222222222222222222222222222222222222222"});
    rewrite["reason"] = serde_json::json!("signature maintenance");
    rewrite["signature_only"] = serde_json::json!(["1111111111111111111111111111111111111111"]);
    bundle["events"] = serde_json::json!([]);
    bundle["event_count"] = serde_json::json!(0);
    bundle["since"] = serde_json::json!({"sha256":complete_digest, "event_count":original["event_count"].as_u64().unwrap()+1});
    bundle["repository_events"] = serde_json::json!([rewrite]);
    fs::write(&path, serde_json::to_vec(&bundle).unwrap()).unwrap();
    received(&receiver, &path, false).assert().success();
    received(&receiver, &path, false).assert().success();
    let records: Vec<serde_json::Value> = fs::read_dir(receiver.root.join(".git/arc/imports"))
        .unwrap()
        .map(|entry| serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
        .collect();
    assert_eq!(records.len(), 3);
    let repository = records
        .iter()
        .find(|batch| !batch["repository_events"].as_array().unwrap().is_empty())
        .unwrap();
    assert!(repository["repository_events"][0]
        .get("signature_only")
        .is_none());
    let output = receiver.home.join("reexport.json");
    stdout(receiver.arc(&receiver.root).args([
        "export",
        original["change_id"].as_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]));
    let exported: serde_json::Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(exported["repository_events"].as_array().unwrap().len(), 1);
    assert_eq!(exported["events_sha256"], bundle["events_sha256"]);
}

#[test]
fn replica_import_provenance_refusals_preserve_the_destination() {
    let origin = Repo::new();
    let receiver = Repo::new();
    let id = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "id", "--json"]),
    );
    stdout(origin.arc(&origin.root).args(["replica", "init", "origin"]));
    stdout(origin.arc(&origin.root).args([
        "replica",
        "pair",
        "receiver",
        "--repository-id",
        id["repository_id"].as_str().unwrap(),
    ]));
    let path = origin.home.join("replicas.json");
    stdout(origin.arc(&origin.root).args([
        "replica",
        "export",
        "--output",
        path.to_str().unwrap(),
    ]));
    let mut bundle: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    receiver.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{}'\n[[replica.publishers.origins]]\nreplica_id='{}'\nroles=['authority']\nactors=['tester']\n", bundle["source_replica_id"].as_str().unwrap(),bundle["source_replica_id"].as_str().unwrap()));
    let config = fs::read(receiver.root.join(".git/arc/config.json")).unwrap();
    for preview in [false, true] {
        for principal in [None, Some("unknown"), Some("channel")] {
            let mut cmd = receiver.arc(&receiver.root);
            cmd.args(["replica", "import", path.to_str().unwrap()]);
            if let Some(principal) = principal {
                cmd.args([
                    "--publisher",
                    principal,
                    "--received-sha256",
                    &if principal == "unknown" {
                        hex::encode(Sha256::digest(fs::read(&path).unwrap()))
                    } else {
                        "0".repeat(64)
                    },
                ]);
            }
            if preview {
                cmd.arg("--dry-run");
            }
            cmd.assert().code(18);
        }
    }
    bundle["source_replica_id"] = serde_json::json!("wrong-exporter");
    fs::write(&path, serde_json::to_vec(&bundle).unwrap()).unwrap();
    for preview in [false, true] {
        let mut cmd = receiver.arc(&receiver.root);
        cmd.args([
            "replica",
            "import",
            path.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &hex::encode(Sha256::digest(fs::read(&path).unwrap())),
        ]);
        if preview {
            cmd.arg("--dry-run");
        }
        cmd.assert()
            .code(18)
            .stderr(predicates::str::contains("exporter"));
    }
    assert_eq!(
        fs::read(receiver.root.join(".git/arc/config.json")).unwrap(),
        config
    );
    assert!(!receiver.root.join(".git/arc/replicas/imports").exists());
}

#[test]
fn forged_signature_only_rewrite_cannot_carry_a_trusted_approval() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority", "rewrite"],
        &["operator"],
        true,
    );
    received(&receiver, &path, false).assert().success();
    assert_eq!(status(&receiver, &bundle)["has_valid_approval"], true);
    let old = bundle["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "patchset-added")
        .unwrap()["head"]
        .as_str()
        .unwrap()
        .to_string();
    let tree = source.home.join(".worktrees/repo-review-origin");
    source.commit(
        &tree,
        "feature.txt",
        "different content\n",
        "fix: revise feature",
    );
    let successor = source.head(&tree);
    bind_branch(&receiver, &source, &bundle);
    let mut rewrite = bundle["events"][0].clone();
    rewrite["event_id"] = serde_json::json!("ZZZZ-rewrite-content");
    rewrite["change_id"] = serde_json::json!("repository");
    rewrite["event_type"] = serde_json::json!("history-rewritten");
    rewrite["reason"] = serde_json::json!("claimed signature maintenance");
    rewrite["mapping"] = serde_json::json!({old.clone():successor});
    rewrite["signature_only"] = serde_json::json!([old]);
    bundle["repository_events"] = serde_json::json!([rewrite]);
    write_bundle(&path, &mut bundle);
    received(&receiver, &path, false).assert().success();
    let state = status(&receiver, &bundle);
    assert_eq!(state["has_valid_approval"], false, "{state}");
    assert_eq!(
        state["verdict_assessments"][0]["covers_current_head"],
        false
    );
    assert_eq!(state["verdict_assessments"][0]["gating_eligible"], true);
}

#[test]
fn authorization_movement_before_publication_preserves_admitted_state() {
    let (source, path, bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    enroll(&receiver, &bundle, &["facts", "review-input"], &[], false);
    let id = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "id", "--json"]),
    );
    assert!(id["repository_id"].is_string());
    let locks = receiver.root.join(".git/arc/locks");
    fs::create_dir_all(&locks).unwrap();
    let lock_path = locks.join("replica-state.lock");
    let locked = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    locked.lock().unwrap();
    let config = fs::read(receiver.root.join(".git/arc/config.json")).unwrap();
    let template = received(&receiver, &path, false);
    let mut command = Command::new(template.get_program());
    command
        .args(template.get_args())
        .current_dir(template.get_current_dir().unwrap())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in template.get_envs() {
        match value {
            Some(value) => {
                command.env(key, value);
            }
            None => {
                command.env_remove(key);
            }
        }
    }
    let child = command.spawn().unwrap();
    let descriptors = PathBuf::from(format!("/proc/{}/fd", child.id()));
    let start = Instant::now();
    loop {
        let waiting = fs::read_dir(&descriptors)
            .unwrap()
            .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
            .any(|path| path == lock_path);
        if waiting {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "import never reached its publication lock"
        );
        thread::sleep(Duration::from_millis(5));
    }
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-input", "rewrite"],
        &[],
        false,
    );
    locked.unlock().unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(18), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("authorization configuration moved"));
    assert_eq!(
        fs::read(receiver.root.join(".git/arc/config.json")).unwrap(),
        config
    );
    assert!(!receiver.root.join(".git/arc/imports").exists());
    assert!(!receiver.root.join(".git/arc/changes").exists());
}

#[test]
fn legacy_import_cannot_gain_local_review_credit_by_claiming_the_receiver_origin() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    let identity = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "id", "--json"]),
    );
    let verdict = bundle["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "verdict-recorded")
        .unwrap();
    verdict["repository_id"] = identity["repository_id"].clone();
    write_bundle(&path, &mut bundle);
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["import", path.to_str().unwrap()]),
    );
    let state = status(&receiver, &bundle);
    assert_eq!(state["has_valid_approval"], false, "{state}");
    assert_eq!(
        state["verdict_assessments"][0]["origin_replica_id"],
        identity["repository_id"]
    );
    assert_eq!(
        state["verdict_assessments"][0]["admission_provenance"],
        "legacy-unknown"
    );
}

#[test]
fn digest_objects_and_authorization_refuse_before_publication() {
    let (source, input, bundle) = exported_review();
    let receiver = Repo::new();
    enroll(&receiver, &bundle, &["facts", "review-input"], &[], false);
    for preview in [false, true] {
        received(&receiver, &input, preview)
            .assert()
            .code(1)
            .stderr(predicates::str::contains("missing bundled Git objects"));
    }
    assert!(!receiver.root.join(".git/arc/config.json").exists());
    assert!(!receiver.root.join(".git/arc/imports").exists());
    let bytes = fs::read(&input).unwrap();
    for preview in [false, true] {
        let mut command = receiver.arc(&receiver.root);
        command.args([
            "import",
            input.to_str().unwrap(),
            "--provenance-only",
            "--publisher",
            "unknown",
            "--received-sha256",
            &hex::encode(Sha256::digest(&bytes)),
        ]);
        if preview {
            command.arg("--dry-run");
        }
        command
            .assert()
            .code(18)
            .stderr(predicates::str::contains("unknown principal"));
    }
    fs::write(&input, b"not a bundle").unwrap();
    receiver
        .arc(&receiver.root)
        .args([
            "import",
            input.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &"0".repeat(64),
        ])
        .assert()
        .code(18)
        .stderr(predicates::str::contains("digest mismatch"));
    assert!(!receiver.root.join(".git/arc/config.json").exists());
    fs::write(&input, bytes).unwrap();
    deliver_git_objects(&source, &receiver);
    received(&receiver, &input, false).assert().success();
}

#[test]
fn head_coverage_does_not_select_a_verdict_for_another_patchset() {
    let (source, path, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority"],
        &["operator"],
        true,
    );
    received(&receiver, &path, false).assert().success();
    let mut patchset = bundle["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "patchset-added")
        .unwrap()
        .clone();
    patchset["event_id"] = serde_json::json!("ZZZZ-same-head-patchset");
    patchset["patchset_id"] = serde_json::json!("ps-02");
    bundle["events"].as_array_mut().unwrap().push(patchset);
    write_bundle(&path, &mut bundle);
    received(&receiver, &path, false).assert().success();
    let state = status(&receiver, &bundle);
    assert_eq!(state["has_valid_approval"], false);
    assert_eq!(state["verdict_assessments"][0]["covers_current_head"], true);
    assert_eq!(state["verdict_assessments"][0]["gating_eligible"], true);
    assert_eq!(
        state["verdict_assessments"][0]["selected_authorizing_event"],
        false
    );
    let check = json_stdout_any_status(receiver.arc(&receiver.root).args([
        "check",
        bundle["change_id"].as_str().unwrap(),
        "--json",
    ]));
    assert!(check["selected_authorizing_event"].is_null());
}

#[test]
fn replica_admission_preserves_the_receivers_declared_actor_guard() {
    let origin = Repo::new();
    let receiver = Repo::new();
    let identity = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "id", "--json"]),
    );
    stdout(origin.arc(&origin.root).args(["replica", "init", "origin"]));
    stdout(origin.arc(&origin.root).args([
        "replica",
        "pair",
        "receiver",
        "--repository-id",
        identity["repository_id"].as_str().unwrap(),
    ]));
    let input = origin.home.join("replicas.json");
    stdout(origin.arc(&origin.root).args([
        "replica",
        "export",
        "--output",
        input.to_str().unwrap(),
    ]));
    let bytes = fs::read(&input).unwrap();
    let bundle: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    receiver.declare_gates_locally(&format!("[policy]\nrequire_declared_actor=true\n[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{}'\n[[replica.publishers.origins]]\nreplica_id='{}'\nroles=['authority']\nactors=['tester']\n", bundle["source_replica_id"].as_str().unwrap(), bundle["source_replica_id"].as_str().unwrap()));
    let config = fs::read(receiver.root.join(".git/arc/config.json")).unwrap();
    receiver
        .arc(&receiver.root)
        .env_remove("ARC_ACTOR")
        .args([
            "replica",
            "import",
            input.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &hex::encode(Sha256::digest(bytes)),
        ])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("declared actor"));
    assert_eq!(
        fs::read(receiver.root.join(".git/arc/config.json")).unwrap(),
        config
    );
    assert!(!receiver.root.join(".git/arc/replicas/imports").exists());
}

#[test]
fn receipts_cannot_cross_change_content_or_scope() {
    let (source, input, original) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &original);
    let mut legacy = original.clone();
    legacy["change_id"] = serde_json::json!("legacy-collision");
    for event in legacy["events"].as_array_mut().unwrap() {
        event["change_id"] = serde_json::json!("legacy-collision");
    }
    write_bundle(&input, &mut legacy);
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["import", input.to_str().unwrap()]),
    );
    enroll(
        &receiver,
        &original,
        &["facts", "review-authority"],
        &["operator"],
        true,
    );
    let before = fs::read(receiver.root.join(".git/arc/config.json")).unwrap();
    fs::write(&input, serde_json::to_vec(&original).unwrap()).unwrap();
    received(&receiver, &input, false)
        .assert()
        .code(1)
        .stderr(predicates::str::contains("conflicting"));
    let state = status(&receiver, &legacy);
    assert_eq!(
        state["verdict_assessments"][0]["admission_provenance"],
        "legacy-unknown"
    );
    assert_eq!(state["has_valid_approval"], false);
    assert_eq!(
        fs::read(receiver.root.join(".git/arc/config.json")).unwrap(),
        before
    );
}

#[test]
fn an_unpublished_failure_preserves_the_store_format() {
    let (source, input, bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    enroll(&receiver, &bundle, &["facts", "review-input"], &[], false);
    stdout(receiver.arc(&receiver.root).args(["replica", "id"]));
    let config = receiver.root.join(".git/arc/config.json");
    let before = fs::read(&config).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let imports = receiver.root.join(".git/arc/imports");
    fs::create_dir_all(&imports).unwrap();
    fs::set_permissions(&imports, fs::Permissions::from_mode(0o500)).unwrap();
    received(&receiver, &input, false).assert().code(1);
    let after = fs::read(config).unwrap();
    fs::set_permissions(&imports, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(after, before);
}

#[test]
fn input_provisional_verdict_cannot_create_a_receiver_obligation() {
    let (source, input, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(&receiver, &bundle, &["facts", "review-input"], &[], false);
    received(&receiver, &input, false).assert().success();
    stdout(
        receiver
            .arc(&receiver.root)
            .env("ARC_ACTOR", "local-reviewer")
            .args([
                "review",
                bundle["change_id"].as_str().unwrap(),
                "--verdict",
                "approved",
            ]),
    );
    let verdict = bundle["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "verdict-recorded")
        .unwrap();
    verdict["event_id"] = serde_json::json!("ZZZZ-input-provisional");
    verdict["provisional"] = serde_json::json!("unproven input");
    verdict["created_at"] = serde_json::json!("2099-01-01T00:00:00Z");
    write_bundle(&input, &mut bundle);
    received(&receiver, &input, false).assert().success();
    let state = status(&receiver, &bundle);
    assert_eq!(state["has_valid_approval"], true);
    assert_eq!(state["provisional_approval_outstanding"], false);
}

#[test]
fn untrusted_disposition_cannot_release_a_local_blocker() {
    let (source, input, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority"],
        &["operator"],
        false,
    );
    received(&receiver, &input, false).assert().success();
    let change = bundle["change_id"].as_str().unwrap().to_string();
    stdout(
        receiver
            .arc(&receiver.root)
            .env("ARC_ACTOR", "local-reviewer")
            .args(["review", &change, "--verdict", "approved"]),
    );
    stdout(receiver.arc(&receiver.root).args([
        "finding",
        &change,
        "--blocking",
        "--summary",
        "local blocker",
    ]));
    let recorded = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["show", &change, "--json"]),
    );
    let finding = recorded["findings"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let mut disposition = bundle["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "verdict-recorded")
        .unwrap()
        .clone();
    disposition["event_type"] = serde_json::json!("disposition-recorded");
    disposition["event_id"] = serde_json::json!("ZZZZ-foreign-disposition");
    disposition["finding_id"] = serde_json::json!(finding);
    disposition["status"] = serde_json::json!("resolved");
    disposition["commit"] =
        serde_json::json!(source.head(&source.home.join(".worktrees/repo-review-origin")));
    disposition["evidence"] = serde_json::json!("reported fix");
    disposition["supersedes"] = serde_json::json!([]);
    let prefix = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["export", &change, "--output", "-"]),
    );
    bundle["events"] = prefix["events"].clone();
    bundle["events"]
        .as_array_mut()
        .unwrap()
        .push(disposition.clone());
    write_bundle(&input, &mut bundle);
    let checksum = bundle["events_sha256"].clone();
    bundle["since"] =
        serde_json::json!({"sha256":prefix["events_sha256"],"event_count":prefix["event_count"]});
    bundle["events"] = serde_json::json!([disposition]);
    bundle["event_count"] = serde_json::json!(1);
    bundle["events_sha256"] = checksum;
    fs::write(&input, serde_json::to_vec(&bundle).unwrap()).unwrap();
    received(&receiver, &input, false).assert().success();
    let state = status(&receiver, &bundle);
    assert!(state["open_blocking_findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == &finding));
    receiver
        .arc(&receiver.root)
        .args(["check", &change])
        .assert()
        .code(2);
}

fn spawn_received(receiver: &Repo, input: &Path) -> Child {
    let template = received(receiver, input, false);
    let mut command = Command::new(template.get_program());
    command
        .args(template.get_args())
        .current_dir(template.get_current_dir().unwrap())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in template.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    command.spawn().unwrap()
}

#[test]
fn policy_revocation_during_format_wait_refuses_without_writes() {
    let (source, input, bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority"],
        &["operator"],
        true,
    );
    stdout(receiver.arc(&receiver.root).args(["replica", "id"]));
    let config = fs::read(receiver.root.join(".git/arc/config.json")).unwrap();
    let locks = receiver.root.join(".git/arc/locks");
    fs::create_dir_all(&locks).unwrap();
    let lock_path = locks.join("format.lock");
    let locked = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    locked.lock().unwrap();
    let child = spawn_received(&receiver, &input);
    let fds = PathBuf::from(format!("/proc/{}/fd", child.id()));
    let start = Instant::now();
    loop {
        let waiting = fs::read_dir(&fds)
            .unwrap()
            .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
            .any(|path| path == lock_path);
        if waiting {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(1));
        thread::sleep(Duration::from_millis(5));
    }
    let policy = receiver.home.join("revoked.toml");
    fs::write(&policy,format!("[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{}'\n[[replica.publishers.origins]]\nreplica_id='{}'\nroles=['facts','review-input']\nactors=[]\n",bundle["source_replica_id"].as_str().unwrap(),bundle["repository_id"].as_str().unwrap())).unwrap();
    stdout(receiver.arc(&receiver.root).args([
        "policy",
        "write",
        "--body-file",
        policy.to_str().unwrap(),
    ]));
    locked.unlock().unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(18), "{output:?}");
    assert_eq!(
        fs::read(receiver.root.join(".git/arc/config.json")).unwrap(),
        config
    );
    assert!(!receiver.root.join(".git/arc/imports").exists());
    assert!(!receiver.root.join(".git/refs/arc/keep").exists());
}

#[test]
fn explain_uses_the_exact_eligible_debt_discharge_witness() {
    let (source, input, mut bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    let patchset = bundle["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "patchset-added")
        .unwrap()
        .clone();
    let review = bundle["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "verdict-recorded")
        .unwrap()
        .clone();
    let mut debt = bundle["events"][0].clone();
    debt["event_id"] = serde_json::json!("ZZZZ-001-debt");
    debt["event_type"] = serde_json::json!("debt-declared");
    debt["reason"] = serde_json::json!("review owed");
    debt["missing"] = serde_json::json!("independent-review");
    debt["patchset_id"] = patchset["patchset_id"].clone();
    let mut closure = bundle["events"][0].clone();
    closure["event_id"] = serde_json::json!("ZZZZ-002-integrated");
    closure["event_type"] = serde_json::json!("change-integrated");
    closure["integrated_commit"] = patchset["head"].clone();
    closure["source_patchset_id"] = patchset["patchset_id"].clone();
    closure["source_head"] = patchset["head"].clone();
    closure["target_before"] = patchset["base"].clone();
    closure["authorization"] = serde_json::json!({"gate_evidence":{},"prerequisites":[],"blocking_findings":[],"holds":[],"gates":{},"policy":{"forbid_self_approval":false,"require_declared_actor":false,"provenance_git_identity":"shared","declared_by":{}},"audit_debt_event_id":"ZZZZ-001-debt"});
    let mut audit = review.clone();
    audit["event_id"] = serde_json::json!("ZZZZ-003-foreign-audit");
    audit["event_type"] = serde_json::json!("audit-verdict-recorded");
    audit["revision"] = patchset["head"].clone();
    audit["actor"] = serde_json::json!("same-reviewer");
    audit["model"] = serde_json::json!("reviewer#medium");
    audit["verdict"] = serde_json::json!("comment-only");
    audit["created_at"] = serde_json::json!("2099-01-01T00:00:00Z");
    bundle["events"]
        .as_array_mut()
        .unwrap()
        .extend([debt, closure, audit]);
    write_bundle(&input, &mut bundle);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-input", "debt-input", "integration"],
        &["tester", "operator"],
        false,
    );
    received(&receiver, &input, false).assert().success();
    let identity = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "id", "--json"]),
    );
    let mut local = review;
    local["event_id"] = serde_json::json!("ZZZZ-004-local-review");
    local["repository_id"] = identity["repository_id"].clone();
    local["actor"] = serde_json::json!("same-reviewer");
    local["model"] = serde_json::json!("reviewer#medium");
    local["created_at"] = serde_json::json!("2099-01-02T00:00:00Z");
    let change = bundle["change_id"].as_str().unwrap();
    let events = receiver
        .root
        .join(".git/arc/changes")
        .join(change)
        .join("events");
    fs::create_dir_all(&events).unwrap();
    fs::write(
        events.join("ZZZZ-004-local-review.json"),
        serde_json::to_vec(&local).unwrap(),
    )
    .unwrap();
    let explanation = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["explain", change, "--json"]),
    );
    fn witness(value: &serde_json::Value) -> Option<&serde_json::Value> {
        match value {
            serde_json::Value::Object(map) => map
                .get("discharged_later_by")
                .or_else(|| map.values().find_map(witness)),
            serde_json::Value::Array(values) => values.iter().find_map(witness),
            _ => None,
        }
    }
    assert_eq!(
        witness(&explanation),
        Some(&serde_json::json!("ZZZZ-004-local-review")),
        "{explanation}"
    );
    let state = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["show", change, "--json"]),
    );
    assert_eq!(
        state["debt"]["discharged_by_event_id"],
        "ZZZZ-004-local-review"
    );
}

#[test]
fn receipt_readers_reject_changed_content_and_scope() {
    let (source, input, bundle) = exported_review();
    for scope_corruption in [false, true] {
        let receiver = Repo::new();
        deliver_git_objects(&source, &receiver);
        enroll(
            &receiver,
            &bundle,
            &["facts", "review-authority"],
            &["operator"],
            true,
        );
        received(&receiver, &input, false).assert().success();
        let path = fs::read_dir(receiver.root.join(".git/arc/imports"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut batch: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if scope_corruption {
            batch["authorization"]["events"][0]["scope"] = serde_json::json!("repository");
        } else {
            batch["events"][0]["title"] = serde_json::json!("changed after admission");
        }
        fs::write(path, serde_json::to_vec(&batch).unwrap()).unwrap();
        for verb in ["show", "status", "check", "explain", "export"] {
            let mut command = receiver.arc(&receiver.root);
            command.args([verb, bundle["change_id"].as_str().unwrap()]);
            if verb == "export" {
                command.args(["--output", "-"]);
            } else {
                command.arg("--json");
            }
            command
                .assert()
                .code(1)
                .stderr(predicates::str::contains("receipt"));
        }
    }
}

#[test]
fn trusted_foreign_provisional_cannot_shadow_local_approval() {
    let (source, input, bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority"],
        &["operator"],
        true,
    );
    received(&receiver, &input, false).assert().success();
    let change = bundle["change_id"].as_str().unwrap();
    stdout(
        receiver
            .arc(&receiver.root)
            .env("ARC_ACTOR", "local-reviewer")
            .args(["review", change, "--verdict", "approved"]),
    );
    let before = status(&receiver, &bundle);
    assert_eq!(before["provisional_approval_outstanding"], false);
    stdout(source.arc(&source.root).env("ARC_ACTOR", "operator").args([
        "review",
        change,
        "--verdict",
        "approved",
        "--provisional",
        "needs corroboration",
    ]));
    stdout(
        source
            .arc(&source.root)
            .args(["export", change, "--output", input.to_str().unwrap()]),
    );
    received(&receiver, &input, false).assert().success();
    let after = status(&receiver, &bundle);
    assert_eq!(after["verdict"]["event_id"], before["verdict"]["event_id"]);
    assert_eq!(after["has_valid_approval"], true);
    let foreign = after["verdict_assessments"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(foreign["gating_eligible"], true);
    assert_eq!(foreign["selected_authorizing_event"], false);
    assert_eq!(after["provisional_approval_outstanding"], false, "{after}");
    assert!(!stdout(
        receiver
            .arc(&receiver.root)
            .args(["query", "--provisional"])
    )
    .contains(change));
    assert!(
        !stdout(receiver.arc(&receiver.root).args(["check", change]))
            .contains("provisional-approval")
    );
    let history = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["review", change, "--json"]),
    );
    assert!(history["verdicts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|verdict| verdict["provisional_outstanding"] != true));
}

#[test]
fn imported_integrated_findings_replay_after_local_dispositions() {
    let (source, input, bundle) = exported_review();
    let change = bundle["change_id"].as_str().unwrap();
    let finding = stdout(source.arc(&source.root).args([
        "finding",
        change,
        "--blocking",
        "--summary",
        "source blocker",
    ]))
    .lines()
    .find_map(|line| line.strip_prefix("finding: "))
    .unwrap()
    .to_string();
    stdout(
        source
            .arc(&source.root)
            .env("ARC_ACTOR", "operator")
            .args(["resolve", change, &finding, "--status", "resolved"]),
    );
    stdout(
        source
            .arc(&source.root)
            .args(["integrate", change, "--no-cleanup"]),
    );
    stdout(
        source
            .arc(&source.root)
            .args(["export", change, "--output", input.to_str().unwrap()]),
    );
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority", "integration"],
        &["operator", "tester"],
        false,
    );
    received(&receiver, &input, false).assert().success();
    let imported = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["show", change, "--json"]),
    );
    assert_eq!(
        imported["findings"][&finding]["dispositions"][0]["gating_eligible"],
        false
    );
    for disposition in ["resolved", "still-open", "accepted-risk", "obsolete"] {
        stdout(receiver.arc(&receiver.root).args([
            "resolve",
            change,
            &finding,
            "--status",
            disposition,
        ]));
        let replayed = json_stdout(
            receiver
                .arc(&receiver.root)
                .args(["show", change, "--json"]),
        );
        assert_eq!(
            replayed["findings"][&finding]["after_integration"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["status"],
            disposition
        );
        stdout(
            receiver
                .arc(&receiver.root)
                .args(["status", change, "--json"]),
        );
        stdout(
            receiver
                .arc(&receiver.root)
                .args(["findings", change, "--format", "json"]),
        );
        stdout(
            receiver
                .arc(&receiver.root)
                .args(["review", change, "--json"]),
        );
        stdout(
            receiver
                .arc(&receiver.root)
                .args(["explain", change, "--json"]),
        );
    }
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority", "integration"],
        &["operator", "tester"],
        true,
    );
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["show", change, "--json"]),
    );
    let relay = receiver.home.join("relay.json");
    stdout(receiver.arc(&receiver.root).args([
        "export",
        change,
        "--output",
        relay.to_str().unwrap(),
    ]));
    let downstream = Repo::new();
    deliver_git_objects(&source, &downstream);
    stdout(
        downstream
            .arc(&downstream.root)
            .args(["import", relay.to_str().unwrap()]),
    );
    stdout(
        downstream
            .arc(&downstream.root)
            .args(["show", change, "--json"]),
    );
}

#[test]
fn imported_findings_replay_after_receiver_integration_and_trust_revocation() {
    let (source, input, bundle) = exported_review();
    let change = bundle["change_id"].as_str().unwrap();
    let finding = stdout(source.arc(&source.root).args([
        "finding",
        change,
        "--blocking",
        "--summary",
        "imported blocker",
    ]))
    .lines()
    .find_map(|line| line.strip_prefix("finding: "))
    .unwrap()
    .to_string();
    stdout(
        source
            .arc(&source.root)
            .env("ARC_ACTOR", "operator")
            .args(["resolve", change, &finding, "--status", "resolved"]),
    );
    stdout(
        source
            .arc(&source.root)
            .args(["export", change, "--output", input.to_str().unwrap()]),
    );
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority"],
        &["operator"],
        true,
    );
    received(&receiver, &input, false).assert().success();
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["integrate", change, "--no-cleanup"]),
    );
    enroll(
        &receiver,
        &bundle,
        &["facts", "review-authority"],
        &["operator"],
        false,
    );
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["resolve", change, &finding, "--status", "resolved"]),
    );
    stdout(receiver.arc(&receiver.root).args([
        "reply",
        change,
        &finding,
        "--body",
        "receiver resolution evidence",
    ]));
    let replayed = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["show", change, "--json"]),
    );
    assert_eq!(
        replayed["findings"][&finding]["after_integration"][0]["status"],
        "resolved"
    );
    assert_eq!(
        replayed["findings"][&finding]["replies"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["status", change, "--json"]),
    );
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["export", change, "--output", "-"]),
    );
}

#[test]
fn aggregate_reads_preserve_imported_authorization() {
    let (source, path, bundle) = exported_review();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    bind_branch(&receiver, &source, &bundle);
    enroll(&receiver, &bundle, &["facts", "review-input"], &[], false);
    let local = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
    for slug in local {
        stdout(receiver.arc(&receiver.root).args(["begin", slug]));
    }
    received(&receiver, &path, false).assert().success();
    let imported = bundle["change_id"].as_str().unwrap();

    let listing = stdout(receiver.arc(&receiver.root).args(["list"]));
    for slug in local {
        assert!(listing.contains(slug), "{slug} missing from:\n{listing}");
    }
    assert!(listing.contains(imported), "{listing}");

    stdout(receiver.arc(&receiver.root).args(["list", "--json"]));
    for args in [vec!["stats"], vec!["doctor"]] {
        stdout(receiver.arc(&receiver.root).args(&args));
    }

    // The admission still holds for the imported change.
    let state = status(&receiver, &bundle);
    assert_eq!(
        state["verdict_assessments"][0]["admission_provenance"], "transport-asserted",
        "{state}"
    );
    assert_eq!(state["has_valid_approval"], false, "{state}");
}

#[test]
fn production_scope_reads_never_write_instrumentation_paths() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "read-boundary"]));
    let ledger = repo.root.join(".git/arc/config.json");
    let original = fs::read(&ledger).unwrap();
    for args in [
        vec!["list"],
        vec!["list", "--json"],
        vec!["stats"],
        vec!["doctor"],
        vec!["show", "read-boundary", "--json"],
        vec!["status", "read-boundary", "--json"],
    ] {
        stdout(
            repo.arc(&repo.root)
                .env_remove("ARC_ACTOR")
                .env("ARC_SCOPE_COMPUTED", &ledger)
                .args(&args),
        );
        assert_eq!(
            fs::read(&ledger).unwrap(),
            original,
            "{args:?} modified the ledger"
        );
    }
    let absent = repo.home.join("diagnostic-marker");
    stdout(
        repo.arc(&repo.root)
            .env("ARC_SCOPE_COMPUTED", &absent)
            .args(["list"]),
    );
    assert!(!absent.exists());
}
