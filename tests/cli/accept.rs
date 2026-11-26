use super::common::*;

struct Delivery {
    source: Repo,
    receiver: Repo,
    input: PathBuf,
    branch: String,
    change: String,
    head: String,
    base: String,
}

impl Delivery {
    fn new() -> Self {
        let source = Repo::new();
        let receiver = Repo::new();
        let source_id = json_stdout(source.arc(&source.root).args(["replica", "id", "--json"]))
            ["repository_id"]
            .as_str()
            .unwrap()
            .to_string();
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
            &source_id,
        ]));
        let pairing = receiver.home.join("pairing.json");
        stdout(receiver.arc(&receiver.root).args([
            "replica",
            "export",
            "--output",
            pairing.to_str().unwrap(),
        ]));
        let receiver_id = json_stdout(
            receiver
                .arc(&receiver.root)
                .args(["replica", "id", "--json"]),
        )["repository_id"]
            .as_str()
            .unwrap()
            .to_string();
        source.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='authority-channel'\nsource_replica_id='{receiver_id}'\n[[replica.publishers.origins]]\nreplica_id='{receiver_id}'\nroles=['authority']\nactors=['tester']\n"));
        stdout(source.arc(&source.root).args([
            "replica",
            "import",
            pairing.to_str().unwrap(),
            "--publisher",
            "authority-channel",
            "--received-sha256",
            &hex::encode(Sha256::digest(fs::read(&pairing).unwrap())),
        ]));
        let opened = stdout(source.arc(&source.root).env("ARC_ACTOR", "worker").args([
            "begin",
            "delivery",
            "--no-worktree",
        ]));
        let change = opened_change_id(&opened);
        let branch = git_out(&source.root, &["branch", "--show-current"]);
        let base = source.head(&source.root);
        source.commit(
            &source.root,
            "feature.txt",
            "feature\n",
            "feat: deliver feature",
        );
        stdout(source.arc(&source.root).env("ARC_ACTOR", "worker").args([
            "snapshot",
            &change,
            "--contributors",
            "worker",
        ]));
        let head = source.head(&source.root);
        let input = source.home.join("delivery ; $.json");
        stdout(source.arc(&source.root).args([
            "export",
            &change,
            "--output",
            input.to_str().unwrap(),
        ]));
        deliver_git_objects(&source, &receiver);
        git(&receiver.root, &["update-ref", "refs/heads/master", &base]);
        git(&receiver.root, &["reset", "--hard", &base]);
        git(&receiver.root, &["branch", &branch, &head]);
        receiver.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{source_id}'\n[[replica.publishers.origins]]\nreplica_id='{source_id}'\nroles=['facts','rewrite','review-input']\nactors=['worker','tester','operator']\n"));
        let key = receiver.home.join("signing-key");
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f", key.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(status.success());
        git(&receiver.root, &["config", "gpg.format", "ssh"]);
        git(
            &receiver.root,
            &["config", "user.signingkey", key.to_str().unwrap()],
        );
        Self {
            source,
            receiver,
            input,
            branch,
            change,
            head,
            base,
        }
    }

    fn command(&self) -> AssertCommand {
        let mut command = self.receiver.arc(&self.receiver.root);
        command.env("ARC_ROLE", "lead").args([
            "accept",
            self.input.to_str().unwrap(),
            "--branch",
            &self.branch,
            "--publisher",
            "channel",
            "--received-sha256",
            &hex::encode(Sha256::digest(fs::read(&self.input).unwrap())),
            "--json",
        ]);
        command
    }

    fn result(&self, code: i32) -> serde_json::Value {
        let output = self.command().assert().code(code).get_output().clone();
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stdout)))
    }
}

/// Claim: delivery acceptance refuses an empty-session contributor by value.
/// Rejects importing a harness-only contributor into the local patchset.
#[test]
fn empty_session_actors_refuse_accept_contributors() {
    let delivery = Delivery::new();
    delivery
        .command()
        .args(["--contributors", "codex:"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("codex:"))
        .stdout(predicates::str::contains("nonempty"));
    assert!(!delivery.receiver.root.join(".git/arc/changes").exists());
}

#[test]
fn accept_delivery_prepares_signed_local_review_without_transport() {
    let d = Delivery::new();
    d.probe("test -f feature.txt");
    stdout(
        d.source
            .arc(&d.source.root)
            .env("ARC_ACTOR", "operator")
            .args(["review", &d.change, "--verdict", "approved"]),
    );
    d.export();
    d.gates("true");
    let refs = git_out(
        &d.receiver.root,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/delivered",
        ],
    );
    let result = d.result(3);
    assert_eq!(result["schema"], "arc-accept/1");
    assert_eq!(result["preparation_complete"], true);
    assert_eq!(result["local_verdict_owed"], true);
    assert_ne!(result["prepared_head"], d.head);
    assert_eq!(
        refs,
        git_out(
            &d.receiver.root,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/delivered"
            ]
        )
    );
    let state = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    assert!(state.to_string().contains("worker"));
}

#[test]
fn accept_delivery_digest_refusal_is_side_effect_free() {
    let d = Delivery::new();
    fs::write(&d.input, b"invalid").unwrap();
    let mut command = d.receiver.arc(&d.receiver.root);
    let output = command
        .args([
            "accept",
            d.input.to_str().unwrap(),
            "--branch",
            &d.branch,
            "--publisher",
            "channel",
            "--received-sha256",
            &"0".repeat(64),
            "--json",
        ])
        .assert()
        .code(18)
        .get_output()
        .clone();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["stopped_at"], "import-preview");
    assert!(!d.receiver.root.join(".git/arc/changes").exists());
}

#[test]
fn accept_delivery_publisher_refusal_is_side_effect_free() {
    let d = Delivery::new();
    d.receiver.declare_gates_locally("");
    let result = d.result(18);
    assert_eq!(result["stopped_at"], "import-preview");
    assert!(!d.receiver.root.join(".git/arc/changes").exists());
}

#[test]
fn accept_delivery_branch_head_mismatch_precedes_import() {
    let d = Delivery::new();
    git(
        &d.receiver.root,
        &["update-ref", &format!("refs/heads/{}", d.branch), &d.base],
    );
    let result = d.result(1);
    assert_eq!(result["stopped_at"], "preflight");
    assert!(!d.receiver.root.join(".git/arc/changes").exists());
}

#[test]
fn accept_delivery_rebased_after_patchset_names_publishing_remedy() {
    rebased_delivery_names_publishing_remedy(false);
}

#[test]
fn accept_delivery_recorded_rebase_names_publishing_remedy() {
    rebased_delivery_names_publishing_remedy(true);
}

fn rebased_delivery_names_publishing_remedy(record_rewrite: bool) {
    let d = Delivery::new();
    git(&d.source.root, &["checkout", "master"]);
    d.source.commit(
        &d.source.root,
        "target.txt",
        "target\n",
        "feat: advance target",
    );
    git(&d.source.root, &["checkout", &d.branch]);
    git(&d.source.root, &["rebase", "master"]);
    let rebased = d.source.head(&d.source.root);
    assert_ne!(rebased, d.head);
    if record_rewrite {
        d.source
            .arc(&d.source.root)
            .args([
                "history",
                "rewrite",
                "--map",
                "-",
                "--reason",
                "rebase onto target",
            ])
            .write_stdin(format!("{} {rebased}\n", d.head))
            .assert()
            .success();
    }
    stdout(d.source.arc(&d.source.root).args([
        "export",
        &d.change,
        "--output",
        d.input.to_str().unwrap(),
    ]));
    deliver_git_objects(&d.source, &d.receiver);
    git(
        &d.receiver.root,
        &["update-ref", &format!("refs/heads/{}", d.branch), &rebased],
    );

    let result = d.result(if record_rewrite { 11 } else { 1 });
    assert_eq!(result["stopped_at"], "preflight");
    let detail = result["next_steps"].to_string();
    assert!(detail.contains("publishing replica"), "{detail}");
    assert!(
        detail.contains(&format!(
            "arc snapshot '{}' --contributors 'worker'",
            d.change
        )),
        "{detail}"
    );
    assert!(detail.contains("publisher only rebased"), "{detail}");
    assert!(
        detail.contains("export again") && detail.contains("accept the new bundle"),
        "{detail}"
    );
    assert!(!d.receiver.root.join(".git/arc/changes").exists());
}

#[test]
fn accept_delivery_missing_branch_names_transport() {
    let d = Delivery::new();
    git(&d.receiver.root, &["branch", "-D", &d.branch]);
    let result = d.result(6);
    assert!(result["next_steps"].to_string().contains(&d.branch));
}

#[test]
fn accept_delivery_wrong_and_occupied_worktrees_refuse() {
    let d = Delivery::new();
    for path in [&d.source.root, &d.receiver.root, &d.receiver.home] {
        let output = d
            .command()
            .args(["--worktree", path.to_str().unwrap()])
            .assert()
            .code(1)
            .get_output()
            .clone();
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["stopped_at"], "preflight");
    }
}

#[test]
fn accept_delivery_binding_is_local_and_retry_is_idempotent() {
    let d = Delivery::new();
    let first = d.result(3);
    let second = d.result(3);
    assert_eq!(first["prepared_head"], second["prepared_head"]);
    assert_eq!(first["worktree"], second["worktree"]);
    let events = stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["events", "--change", &d.change]),
    );
    assert_eq!(events.matches("change-checkout-bound").count(), 1);
    assert!(!events.contains("verdict-recorded"));
}

#[test]
fn accept_delivery_shared_change_history_refuses_signing() {
    let d = Delivery::new();
    git(&d.receiver.root, &["branch", "other", &d.head]);
    let result = d.result(1);
    assert_eq!(result["stopped_at"], "sign");
    assert!(result["steps"].to_string().contains("import"));
}

#[test]
fn accept_delivery_dry_run_leaves_objects_refs_and_store_unchanged() {
    let d = Delivery::new();
    let refs = git_out(&d.receiver.root, &["show-ref"]);
    let objects = git_out(&d.receiver.root, &["count-objects", "-v"]);
    d.command().arg("--dry-run").assert().success();
    assert_eq!(refs, git_out(&d.receiver.root, &["show-ref"]));
    assert_eq!(objects, git_out(&d.receiver.root, &["count-objects", "-v"]));
    assert!(!d.receiver.root.join(".git/arc/changes").exists());
    assert!(!d.receiver.home.join(".worktrees").exists());
}

#[test]
fn accept_delivery_moved_target_requires_explicit_replay() {
    let mut d = Delivery::new();
    let branch = "arc/delivery;$(false)".to_string();
    git(&d.receiver.root, &["branch", "-m", &d.branch, &branch]);
    d.branch = branch;
    d.mutate_bundle(|bundle| bundle["events"][0]["branch"] = d.branch.clone().into());
    d.receiver.commit(
        &d.receiver.root,
        "advance.txt",
        "advance\n",
        "feat: advance target",
    );
    let result = d.result(11);
    assert_eq!(result["stopped_at"], "preflight");
    let steps = result["next_steps"].to_string();
    assert!(steps.contains("rebase") && steps.contains("snapshot") && steps.contains("review"));
    let detail = result["next_steps"][0].as_str().unwrap();
    let start = detail.find("# Replay in").unwrap();
    let remedy = &detail[start..];
    assert!(remedy.contains("'arc/delivery;$(false)'"));
    assert!(Command::new("sh")
        .args(["-n", "-c", remedy])
        .status()
        .unwrap()
        .success());
}

#[test]
fn accept_delivery_requires_lead_and_paired_authority() {
    let d = Delivery::new();
    d.command().env("ARC_ROLE", "implementer").assert().code(9);
    stdout(d.receiver.arc(&d.receiver.root).args([
        "replica",
        "authority",
        "offer",
        "--to",
        "worker",
    ]));
    let result = d.result(17);
    assert_eq!(result["stopped_at"], "preflight");
    assert!(result["next_steps"].to_string().contains("offer"));
}

impl Delivery {
    fn export(&self) {
        stdout(self.source.arc(&self.source.root).args([
            "export",
            &self.change,
            "--output",
            self.input.to_str().unwrap(),
        ]));
    }
    fn probe(&self, command: &str) {
        self.source
            .arc(&self.source.root)
            .args([
                "brief",
                &self.change,
                "--body-file",
                "-",
                "--base",
                &self.base,
                "--probe",
                &format!("discrimination={command}"),
            ])
            .write_stdin("Delivered contract\n")
            .assert()
            .success();
        stdout(
            self.source
                .arc(&self.source.root)
                .env("ARC_ACTOR", "worker")
                .args(["snapshot", &self.change, "--contributors", "worker"]),
        );
        self.export();
    }
    fn gates(&self, command: &str) {
        let path = self.receiver.root.join(".git/arc/operator-policy.toml");
        let policy = fs::read_to_string(&path).unwrap();
        let policy = policy.split("\n[gates.acceptance]").next().unwrap();
        fs::write(
            path,
            format!(
                "{policy}\n[gates.acceptance]\ncommand={}\n",
                serde_json::to_string(command).unwrap()
            ),
        )
        .unwrap();
    }
}

#[test]
fn accept_delivery_runs_local_gates_and_reports_retained_failure() {
    let d = Delivery::new();
    d.gates("false");
    let result = d.result(5);
    assert_eq!(result["stopped_at"], "gate:acceptance");
    assert_ne!(result["prepared_head"], d.head);
    let events = fixture_events(&d.receiver, &d.change);
    let verification = events
        .iter()
        .find(|event| {
            event["event_type"] == "verification-recorded" && event["gate"] == "acceptance"
        })
        .unwrap();
    assert_eq!(verification["result"], "fail");
    assert_eq!(verification["revision"], result["prepared_head"]);
    assert_eq!(
        transcript_step(&result, "gate:acceptance")["event_ids"],
        serde_json::json!([verification["event_id"]])
    );
    let pin = format!(
        "refs/arc/tree/{}/{}",
        d.change,
        verification["event_id"].as_str().unwrap()
    );
    assert_eq!(
        git_out(&d.receiver.root, &["rev-parse", &pin]),
        verification["tested_tree"]
    );
    assert!(!result["next_steps"].as_array().unwrap().is_empty());
    d.gates("true");
    assert_eq!(d.result(3)["preparation_complete"], true);
}

#[test]
fn accept_delivery_bound_brief_baseline_and_final_use_scratch() {
    let d = Delivery::new();
    d.probe("test -f feature.txt");
    d.source
        .arc(&d.source.root)
        .args([
            "brief",
            &d.change,
            "--body-file",
            "-",
            "--cause-note",
            "additional contract",
            "--probes-json",
            "[{\"name\":\"wrong\",\"command\":\"true\"}]",
        ])
        .write_stdin("Later contract\n")
        .assert()
        .success();
    d.export();
    let unrelated = d.receiver.home.join("unrelated checkout");
    git(
        &d.receiver.root,
        &[
            "worktree",
            "add",
            "--detach",
            unrelated.to_str().unwrap(),
            &d.base,
        ],
    );
    let result = d.result(3);
    let steps = result["steps"].to_string();
    assert!(steps.contains("probe:discrimination:baseline"));
    assert!(steps.contains("probe:discrimination:final"));
    assert!(!steps.contains("probe:wrong"));
    assert!(unrelated.exists());
    assert_eq!(git_out(&unrelated, &["rev-parse", "HEAD"]), d.base);
}

#[test]
fn accept_delivery_probe_discrimination_and_infrastructure_have_distinct_exits() {
    for (command, code, phase) in [
        ("true", 12, "baseline"),
        ("false", 12, "final"),
        ("exec /absent-acceptance-command", 1, "baseline"),
    ] {
        let d = Delivery::new();
        d.probe(command);
        let result = d.result(code);
        assert_eq!(
            result["stopped_at"],
            format!("probe:discrimination:{phase}")
        );
        assert!(result["next_steps"].to_string().contains("brief"));
    }
}

#[test]
fn accept_delivery_gate_moving_branch_or_policy_stops_next_effect() {
    for command in ["git update-ref refs/heads/master HEAD", "printf '\\n[review]\\nchecklist=[\"changed\"]\\n' >> $(git rev-parse --git-common-dir)/arc/operator-policy.toml"] {
        let d = Delivery::new();
        d.gates(command);
        let result = d.result(1);
        assert!(!result["preparation_complete"].as_bool().unwrap());
        assert!(result["next_steps"].to_string().contains("moved"));
    }
}

#[test]
fn accept_delivery_branch_moving_during_gate_names_publishing_remedy() {
    let d = Delivery::new();
    d.gates(&format!(
        "git update-ref refs/heads/{} {}",
        d.branch, d.base
    ));
    let result = d.result(1);
    let detail = result["next_steps"].to_string();
    assert!(detail.contains("change branch moved"), "{detail}");
    assert!(detail.contains("publishing replica"), "{detail}");
    assert!(
        detail.contains(&format!(
            "arc snapshot '{}' --contributors 'worker'",
            d.change
        )),
        "{detail}"
    );
    assert!(
        detail.contains("export again") && detail.contains("accept the new bundle"),
        "{detail}"
    );
}

#[test]
fn accept_delivery_retry_preserves_patchset_and_attribution() {
    let d = Delivery::new();
    let first = d.result(3);
    let before = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    let second = d.result(3);
    let after = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    assert_eq!(first["prepared_head"], second["prepared_head"]);
    assert_eq!(before["patchsets"], after["patchsets"]);
    assert!(after["patchsets"].to_string().contains("worker"));
}

impl Delivery {
    fn import(&self) {
        stdout(self.receiver.arc(&self.receiver.root).args([
            "import",
            self.input.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &hex::encode(Sha256::digest(fs::read(&self.input).unwrap())),
        ]));
    }
    fn local_approval(&self) {
        self.import();
        stdout(
            self.receiver
                .arc(&self.receiver.root)
                .env("ARC_ACTOR", "local-reviewer")
                .args(["review", &self.change, "--verdict", "approved"]),
        );
    }
    fn mutate_bundle(&self, edit: impl FnOnce(&mut serde_json::Value)) {
        let mut bundle: serde_json::Value =
            serde_json::from_slice(&fs::read(&self.input).unwrap()).unwrap();
        edit(&mut bundle);
        let mut digest = Sha256::new();
        for event in bundle["events"].as_array().unwrap() {
            digest.update(serde_json::to_vec(event).unwrap());
            digest.update(b"\n");
        }
        bundle["events_sha256"] = hex::encode(digest.finalize()).into();
        bundle["event_count"] = bundle["events"].as_array().unwrap().len().into();
        fs::write(&self.input, serde_json::to_vec(&bundle).unwrap()).unwrap();
    }
}

#[test]
fn accept_delivery_signature_only_signing_carries_local_approval_without_verdict() {
    let d = Delivery::new();
    d.local_approval();
    let before = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    let result = d.result(0);
    assert_eq!(result["local_verdict_owed"], false);
    assert_eq!(result["readiness"]["has_valid_approval"], true);
    let after = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    assert_eq!(before["verdicts"], after["verdicts"]);
    assert_eq!(before["patchsets"][0]["id"], after["patchsets"][0]["id"]);
    assert_eq!(
        before["patchsets"][0]["contributors"],
        after["patchsets"][0]["contributors"]
    );
    assert_eq!(after["patchsets"].as_array().unwrap().len(), 1);
}

#[test]
fn accept_delivery_real_local_verdict_fork_preserves_all_tips() {
    let d = Delivery::new();
    d.local_approval();
    let state = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    let id = state["verdicts"][0]["event_id"].as_str().unwrap();
    let directory = d
        .receiver
        .root
        .join(".git/arc/changes")
        .join(&d.change)
        .join("events");
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join(format!("{id}.json"))).unwrap()).unwrap();
    for suffix in ["a", "b"] {
        let mut fork = original.clone();
        let event = ulid::Ulid::from_parts(
            ulid::Ulid::new().timestamp_ms() + 1,
            if suffix == "a" { 0 } else { 1 },
        )
        .to_string();
        fork["event_id"] = event.clone().into();
        fork["relation"] = serde_json::json!({"kind":"supersedes", "observed":[id]});
        fs::write(
            directory.join(format!("{event}.json")),
            serde_json::to_vec(&fork).unwrap(),
        )
        .unwrap();
    }
    let result = d.result(3);
    assert_eq!(result["verdict_tips"].as_array().unwrap().len(), 2);
    assert!(result["verdict_tips"]
        .to_string()
        .contains("origin_replica_id"));
    assert_eq!(result["preparation_complete"], true);
    assert_eq!(result["readiness"]["verdict_contested"], true);
}

#[test]
fn accept_delivery_untrusted_foreign_verdict_forks_do_not_displace_authority() {
    let d = Delivery::new();
    d.local_approval();
    stdout(
        d.source
            .arc(&d.source.root)
            .env("ARC_ACTOR", "operator")
            .args(["review", &d.change, "--verdict", "approved"]),
    );
    d.export();
    d.mutate_bundle(|bundle| {
        let original = bundle["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["event_type"] == "verdict-recorded")
            .unwrap()
            .clone();
        let mut fork = original.clone();
        fork["event_id"] = ulid::Ulid::new().to_string().into();
        fork["relation"] =
            serde_json::json!({"kind":"supersedes", "observed":[original["event_id"]]});
        bundle["events"].as_array_mut().unwrap().push(fork);
    });
    let result = d.result(0);
    assert_eq!(result["readiness"]["has_valid_approval"], true);
    assert_eq!(result["verdict_tips"].as_array().unwrap().len(), 1);
    assert_eq!(
        result["readiness"]["verdict_assessments"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn accept_delivery_remote_checkout_ownership_cannot_confer_cleanup_rights() {
    let d = Delivery::new();
    d.mutate_bundle(|bundle| {
        let event = &mut bundle["events"][0];
        event["worktree"] = d.receiver.root.to_str().unwrap().into();
        event["worktree_created"] = true.into();
        event["worktree_owner_token"] = "foreign-token".into();
    });
    let path = d.receiver.home.join("existing ; checkout");
    git(
        &d.receiver.root,
        &["worktree", "add", path.to_str().unwrap(), &d.branch],
    );
    let output = d
        .command()
        .args(["--worktree", path.to_str().unwrap()])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["worktree"], path.to_str().unwrap());
    let state = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    assert_eq!(state["worktree_created"], false);
    assert!(state["worktree_owner_token"].is_null());
    assert!(!path.join(".git/arc-owner").exists());
}

#[test]
fn accept_delivery_missing_objects_stop_before_import() {
    let d = Delivery::new();
    let unavailable = "f".repeat(40);
    d.mutate_bundle(|bundle| {
        bundle["objects"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"oid":unavailable, "role":"baseline"}))
    });
    let result = d.result(6);
    assert_eq!(result["stopped_at"], "import-preview");
    assert!(result["next_steps"].to_string().contains(&unavailable));
    assert!(!d.receiver.root.join(".git/arc/changes").exists());
}

#[test]
fn accept_delivery_imported_green_evidence_does_not_suppress_local_run() {
    let d = Delivery::new();
    let source_policy = d.source.root.join(".git/arc/operator-policy.toml");
    let existing = fs::read_to_string(&source_policy).unwrap();
    fs::write(
        source_policy,
        format!("{existing}\n[gates.acceptance]\ncommand='true'\n"),
    )
    .unwrap();
    stdout(
        d.source
            .arc(&d.source.root)
            .args(["verify", &d.change, "--gate", "acceptance"]),
    );
    d.export();
    d.gates("true");
    let result = d.result(3);
    let gate = result["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "gate:acceptance")
        .unwrap();
    assert!(!gate["event_ids"].as_array().unwrap().is_empty());
    assert!(!gate["outcome"].as_str().unwrap().contains("skipped"));
}

#[test]
fn accept_delivery_signing_failure_retains_import_and_shell_safe_retry() {
    let d = Delivery::new();
    git(
        &d.receiver.root,
        &["config", "user.signingkey", "/absent signing key"],
    );
    let result = d.result(1);
    assert_eq!(result["stopped_at"], "sign");
    assert!(!result["steps"][2]["event_ids"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(result["next_steps"]
        .to_string()
        .contains("delivery ; $.json'"));
    assert!(!result["preparation_complete"].as_bool().unwrap());
}

#[test]
fn accept_delivery_absent_brief_binding_is_preserved() {
    let d = Delivery::new();
    d.source
        .arc(&d.source.root)
        .args([
            "brief",
            &d.change,
            "--body-file",
            "-",
            "--probe",
            "later=true",
        ])
        .write_stdin("Later brief\n")
        .assert()
        .success();
    d.export();
    let result = d.result(3);
    assert!(!result["steps"].to_string().contains("probe:later"));
    let state = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    assert_eq!(state["patchsets"].as_array().unwrap().len(), 1);
    assert!(state["patchsets"][0]["brief_ref"].is_null());
}

#[test]
fn accept_delivery_provisional_base_requires_canonical_prerequisite_replay() {
    let mut d = Delivery::new();
    d.source.commit(
        &d.source.root,
        "extra.txt",
        "extra\n",
        "feat: stacked content",
    );
    let provisional_base = d.head.clone();
    stdout(
        d.source
            .arc(&d.source.root)
            .env("ARC_ACTOR", "worker")
            .args([
                "snapshot",
                &d.change,
                "--base",
                &provisional_base,
                "--contributors",
                "worker",
            ]),
    );
    d.head = d.source.head(&d.source.root);
    d.export();
    deliver_git_objects(&d.source, &d.receiver);
    git(
        &d.receiver.root,
        &["update-ref", &format!("refs/heads/{}", d.branch), &d.head],
    );
    let result = d.result(11);
    assert!(result["next_steps"].to_string().contains("canonical"));
    assert!(result["next_steps"].to_string().contains("review"));
    assert!(!d.receiver.root.join(".git/arc/changes").exists());
}

#[test]
fn accept_delivery_rewrite_recovery_reuses_signed_objects_and_ref_moves() {
    for phase in ["intent", "refs"] {
        let d = Delivery::new();
        let output = d
            .command()
            .env("ARC_REWRITE_INTERRUPT", phase)
            .assert()
            .code(1)
            .get_output()
            .clone();
        let interrupted: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(interrupted["stopped_at"], "sign");
        assert_pending_signing_effects(&d, &interrupted, phase == "refs");
        let objects = git_out(&d.receiver.root, &["count-objects", "-v"]);
        let result = d.result(3);
        assert_eq!(result["preparation_complete"], true);
        assert_eq!(objects, git_out(&d.receiver.root, &["count-objects", "-v"]));
        let events = stdout(
            d.receiver
                .arc(&d.receiver.root)
                .args(["events", "--repository"]),
        );
        assert_eq!(events.matches("history-rewritten").count(), 1);
    }
}

#[test]
fn accept_delivery_never_exposes_shared_or_unsigned_signing_overrides() {
    let d = Delivery::new();
    for flag in ["--include-shared", "--no-sign", "--from"] {
        d.command()
            .arg(flag)
            .assert()
            .code(2)
            .stderr(predicates::str::contains(format!(
                "unexpected argument '{flag}'"
            )));
    }
    assert!(!d.receiver.root.join(".git/arc/changes").exists());
}

#[test]
fn accept_delivery_authority_movement_by_gate_stops_with_retained_effects() {
    let d = Delivery::new();
    let binary = arc_binary();
    d.gates(&format!(
        "'{}' --role lead replica authority offer --to worker",
        binary
    ));
    let result = d.result(17);
    assert_eq!(result["stopped_at"], "gate:acceptance");
    assert!(result["next_steps"].to_string().contains("offer"));
    assert_ne!(result["prepared_head"], d.head);
}

#[test]
fn accept_delivery_foreign_review_exception_is_explicit_in_readiness() {
    let d = Delivery::new();
    stdout(
        d.source
            .arc(&d.source.root)
            .env("ARC_ACTOR", "operator")
            .args(["review", &d.change, "--verdict", "approved"]),
    );
    d.export();
    let source = json_stdout(
        d.source
            .arc(&d.source.root)
            .args(["replica", "id", "--json"]),
    )["repository_id"]
        .as_str()
        .unwrap()
        .to_string();
    let policy = d.receiver.root.join(".git/arc/operator-policy.toml");
    let original = fs::read_to_string(&policy)
        .unwrap()
        .replace("'review-input'", "'review-authority'");
    fs::write(
        policy,
        format!("[replica]\ntrusted_review_replicas=['{source}']\n{original}"),
    )
    .unwrap();
    let result = d.result(0);
    assert_eq!(result["local_verdict_owed"], false);
    assert_eq!(
        result["readiness"]["verdict_assessments"][0]["origin_replica_id"],
        source
    );
    assert_eq!(
        result["readiness"]["verdict_assessments"][0]["gating_eligible"],
        true
    );
}

#[test]
fn accept_delivery_dangling_occupied_path_refuses_before_import() {
    let d = Delivery::new();
    let path = d.receiver.home.join("dangling checkout");
    std::os::unix::fs::symlink(d.receiver.home.join("absent checkout"), &path).unwrap();
    let output = d
        .command()
        .args(["--worktree", path.to_str().unwrap()])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["stopped_at"], "preflight");
    assert!(!d.receiver.root.join(".git/arc/changes").exists());
}

#[test]
fn accept_delivery_snapshot_spool_diagnostics_stay_inside_json() {
    let d = Delivery::new();
    let checkout = d.receiver.home.join("spooled checkout");
    git(
        &d.receiver.root,
        &["worktree", "add", checkout.to_str().unwrap(), &d.branch],
    );
    fs::write(d.receiver.root.join(".git/info/exclude"), ".arc/outbox/\n").unwrap();
    let outbox = checkout.join(".arc/outbox");
    fs::create_dir_all(&outbox).unwrap();
    fs::write(outbox.join("invalid.json"), "invalid spool\n").unwrap();
    let output = d
        .command()
        .args(["--worktree", checkout.to_str().unwrap()])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result["steps"].to_string().contains("kept invalid.json"));
    assert!(outbox.join("invalid.json").exists());
}

#[test]
fn accept_delivery_gate_execution_error_is_not_a_failed_gate() {
    let d = Delivery::new();
    d.gates("exec /absent-acceptance-command");
    let result = d.result(1);
    assert_eq!(result["stopped_at"], "gate:acceptance");
    assert!(result["next_steps"].to_string().contains("execute"));
}

fn fixture_events(repo: &Repo, change: &str) -> Vec<serde_json::Value> {
    stdout(repo.arc(&repo.root).args(["events", "--change", change]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn transcript_step<'a>(result: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    result["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == name)
        .unwrap()
}

fn assert_pending_signing_effects(d: &Delivery, result: &serde_json::Value, applied: bool) {
    let pending_path = d
        .receiver
        .root
        .join(".git/arc/repository/rewrite-intent.json");
    let pending: serde_json::Value =
        serde_json::from_slice(&fs::read(&pending_path).unwrap()).unwrap();
    let effects = &transcript_step(result, "sign")["effects"];
    assert_eq!(
        effects["pending_rewrite"]["path"],
        pending_path.to_str().unwrap()
    );
    assert_eq!(effects["pending_rewrite"]["intent"], pending);
    assert!(effects["pending_rewrite"]["recorded_event_id"].is_null());
    let refs = pending["refs"].as_object().unwrap();
    for (name, moved) in refs {
        let actual = git_out(&d.receiver.root, &["rev-parse", name]);
        assert_eq!(effects["pending_rewrite"]["ref_states"][name], actual);
        assert_eq!(actual, moved[if applied { "new" } else { "old" }]);
        if applied {
            let observed = effects["refs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["name"] == *name)
                .unwrap();
            assert_eq!(observed["before"], moved["old"]);
            assert_eq!(observed["after"], moved["new"]);
        }
    }
    // The commits the rewrite replaces are held in the same transaction, so
    // the refs that hold them are effects of it too.
    let retained = pending["retained"].as_object().unwrap();
    assert!(!retained.is_empty());
    for (name, commit) in retained {
        if applied {
            let actual = git_out(&d.receiver.root, &["rev-parse", name]);
            assert_eq!(&actual, commit.as_str().unwrap());
            assert_eq!(effects["pending_rewrite"]["ref_states"][name], actual);
        } else {
            assert!(effects["pending_rewrite"]["ref_states"][name].is_null());
        }
    }
    if applied {
        assert_eq!(
            effects["refs"].as_array().unwrap().len(),
            refs.len() + retained.len()
        );
    } else {
        assert!(effects["refs"].as_array().unwrap().is_empty());
    }
    for successor in pending["mapping"].as_object().unwrap().values() {
        let successor = successor.as_str().unwrap();
        git(
            &d.receiver.root,
            &["cat-file", "-e", &format!("{successor}^{{commit}}")],
        );
        let object = effects["objects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|object| object["oid"] == successor)
            .unwrap();
        assert_eq!(object["kind"], "commit");
    }
    assert!(transcript_step(result, "sign")["event_ids"]
        .as_array()
        .unwrap()
        .is_empty());
    let continuation = &effects["pending_rewrite"]["continuation"];
    assert_eq!(continuation["cwd"], result["worktree"]);
    assert_eq!(
        continuation["argv"],
        serde_json::json!(["arc", "rewrite", "sign"])
    );
}

#[test]
fn accept_delivery_sign_publication_failure_reports_actual_refs_objects_and_intent() {
    use std::os::unix::fs::PermissionsExt;
    let d = Delivery::new();
    let events = d.receiver.root.join(".git/arc/repository/events");
    fs::create_dir_all(&events).unwrap();
    fs::set_permissions(&events, fs::Permissions::from_mode(0o500)).unwrap();
    let output = d.command().assert().code(1).get_output().clone();
    fs::set_permissions(&events, fs::Permissions::from_mode(0o700)).unwrap();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["stopped_at"], "sign");
    assert_pending_signing_effects(&d, &result, false);
    let prepared = result["prepared_head"].as_str().unwrap();
    assert_eq!(prepared, d.head);
    let successor = transcript_step(&result, "sign")["effects"]["pending_rewrite"]["intent"]
        ["mapping"][&d.head]
        .as_str()
        .unwrap();
    assert_eq!(
        git_out(&d.receiver.root, &["rev-parse", &d.branch]),
        prepared
    );
    assert!(fs::read_dir(&events).unwrap().next().is_none());
    let binding = fixture_events(&d.receiver, &d.change)
        .into_iter()
        .find(|event| event["event_type"] == "change-checkout-bound")
        .unwrap();
    assert!(transcript_step(&result, "checkout-binding")["event_ids"]
        .as_array()
        .unwrap()
        .contains(&binding["event_id"]));
    let retry = d.result(3);
    assert_eq!(retry["prepared_head"], successor);
    assert!(!d
        .receiver
        .root
        .join(".git/arc/repository/rewrite-intent.json")
        .exists());
    assert_eq!(fs::read_dir(&events).unwrap().count(), 1);
}

#[test]
fn accept_delivery_text_reports_partial_signing_effects_before_refusal() {
    use std::os::unix::fs::PermissionsExt;
    let d = Delivery::new();
    let events = d.receiver.root.join(".git/arc/repository/events");
    fs::create_dir_all(&events).unwrap();
    fs::set_permissions(&events, fs::Permissions::from_mode(0o500)).unwrap();
    let output = d
        .receiver
        .arc(&d.receiver.root)
        .env("ARC_ROLE", "lead")
        .args([
            "accept",
            d.input.to_str().unwrap(),
            "--branch",
            &d.branch,
            "--publisher",
            "channel",
            "--received-sha256",
            &hex::encode(Sha256::digest(fs::read(&d.input).unwrap())),
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    fs::set_permissions(&events, fs::Permissions::from_mode(0o700)).unwrap();
    let pending: serde_json::Value = serde_json::from_slice(
        &fs::read(
            d.receiver
                .root
                .join(".git/arc/repository/rewrite-intent.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("pending rewrite"));
    for (name, moved) in pending["refs"].as_object().unwrap() {
        assert!(text.contains(name));
        assert!(text.contains(moved["old"].as_str().unwrap()));
        assert!(text.contains(moved["new"].as_str().unwrap()));
    }
    for (old, new) in pending["mapping"].as_object().unwrap() {
        assert!(text.contains(old) && text.contains(new.as_str().unwrap()));
    }
    assert!(text.contains("commit written") && text.contains("continue:"));
}

#[test]
fn accept_delivery_clock_skew_orders_all_local_events_after_admitted_history() {
    let d = Delivery::new();
    d.probe("test -f feature.txt");
    d.gates("true");
    let future = chrono::Utc::now() + chrono::TimeDelta::hours(1);
    let mut floor = String::new();
    d.mutate_bundle(|bundle| {
        let mut replacements = std::collections::BTreeMap::new();
        for (index, event) in bundle["events"].as_array().unwrap().iter().enumerate() {
            let id =
                ulid::Ulid::from_parts(future.timestamp_millis() as u64, index as u128).to_string();
            replacements.insert(event["event_id"].as_str().unwrap().to_string(), id.clone());
            floor = id;
        }
        fn replace(
            value: &mut serde_json::Value,
            replacements: &std::collections::BTreeMap<String, String>,
        ) {
            match value {
                serde_json::Value::String(value) => {
                    if let Some(replacement) = replacements.get(value) {
                        *value = replacement.clone();
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        replace(item, replacements);
                    }
                }
                serde_json::Value::Object(items) => {
                    for item in items.values_mut() {
                        replace(item, replacements);
                    }
                }
                _ => {}
            }
        }
        replace(&mut bundle["events"], &replacements);
        for event in bundle["events"].as_array_mut().unwrap() {
            event["created_at"] = future.to_rfc3339().into();
        }
    });
    let output = d
        .command()
        .args(["--contributors", "worker,helper"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["preparation_complete"], true);
    let events = fixture_events(&d.receiver, &d.change);
    let receiver_id = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["replica", "id", "--json"]),
    )["repository_id"]
        .clone();
    assert_eq!(events[0]["event_type"], "change-opened");
    for event in events
        .iter()
        .filter(|event| event["repository_id"] == receiver_id)
    {
        assert!(event["event_id"].as_str().unwrap() > floor.as_str());
        assert!(
            chrono::DateTime::parse_from_rfc3339(event["created_at"].as_str().unwrap()).unwrap()
                < future
        );
    }
    for name in [
        "checkout-binding",
        "sign",
        "snapshot",
        "gate:acceptance",
        "probe:discrimination:baseline",
        "probe:discrimination:final",
    ] {
        let ids = transcript_step(&result, name)["event_ids"]
            .as_array()
            .unwrap();
        assert!(!ids.is_empty(), "{name} recorded no evidence");
        assert!(ids.iter().all(|id| id.as_str().unwrap() > floor.as_str()));
    }
    let shown = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    assert_eq!(shown["verifications"].as_array().unwrap().len(), 3);
    stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["verify", &d.change, "--all"]),
    );
    let run_events = fixture_events(&d.receiver, &d.change);
    let run = run_events
        .iter()
        .find(|event| event["event_type"] == "verification-run-started")
        .unwrap();
    assert!(run["event_id"].as_str().unwrap() > floor.as_str());
    let evidence = run_events
        .iter()
        .find(|event| event["run_id"] == run["event_id"])
        .unwrap();
    assert!(evidence["event_id"].as_str().unwrap() > run["event_id"].as_str().unwrap());
    json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    let output = d
        .command()
        .args(["--contributors", "worker,helper"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let retry: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(retry["prepared_head"], result["prepared_head"]);
    assert!(transcript_step(&retry, "import")["event_ids"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(transcript_step(&retry, "checkout-binding")["event_ids"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn accept_delivery_transcript_commands_include_real_arguments_and_internal_steps() {
    let d = Delivery::new();
    let checkout = d.receiver.home.join("delivered ; checkout");
    let output = d
        .command()
        .args(["--worktree", checkout.to_str().unwrap()])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let preflight = transcript_step(&result, "preflight")["argv"]
        .as_array()
        .unwrap();
    for argument in [
        "--publisher",
        "--received-sha256",
        "--worktree",
        "--dry-run",
    ] {
        assert!(preflight.contains(&serde_json::json!(argument)));
    }
    let preflight_args: Vec<_> = preflight
        .iter()
        .skip(1)
        .map(|value| value.as_str().unwrap())
        .collect();
    d.receiver
        .arc(&d.receiver.root)
        .env("ARC_ROLE", "lead")
        .args(preflight_args)
        .assert()
        .success();
    assert_eq!(
        transcript_step(&result, "checkout-allocation")["argv"],
        serde_json::json!([
            "git",
            "worktree",
            "add",
            "--no-checkout",
            checkout,
            d.branch
        ])
    );
    assert_eq!(
        transcript_step(&result, "checkout-population")["argv"],
        serde_json::json!(["git", "read-tree", "--reset", "-u", "HEAD"])
    );
    assert_eq!(
        transcript_step(&result, "checkout-population")["cwd"],
        checkout.to_str().unwrap()
    );
    assert_eq!(transcript_step(&result, "checkout-hook")["argv"][0], "git");
    let binding = transcript_step(&result, "checkout-binding");
    assert_eq!(binding["kind"], "internal");
    assert!(binding["argv"].as_array().unwrap().is_empty());
    let output = d
        .command()
        .args(["--worktree", checkout.to_str().unwrap(), "--dry-run"])
        .assert()
        .success()
        .get_output()
        .clone();
    let dry: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(dry["steps"]
        .as_array()
        .unwrap()
        .iter()
        .all(|step| step["effects"]["refs"].as_array().unwrap().is_empty()));
}

#[test]
fn accept_delivery_signer_failure_reports_commits_written_before_intent() {
    use std::os::unix::fs::PermissionsExt;
    let mut d = Delivery::new();
    d.source.commit(
        &d.source.root,
        "second.txt",
        "second\n",
        "feat: second commit",
    );
    stdout(
        d.source
            .arc(&d.source.root)
            .env("ARC_ACTOR", "worker")
            .args(["snapshot", &d.change, "--contributors", "worker"]),
    );
    d.head = d.source.head(&d.source.root);
    d.export();
    deliver_git_objects(&d.source, &d.receiver);
    git(
        &d.receiver.root,
        &["update-ref", &format!("refs/heads/{}", d.branch), &d.head],
    );
    let signer = d.receiver.home.join("fail-second-signature");
    let counter = d.receiver.home.join("signature-count");
    fs::write(&signer, format!("#!/bin/sh\nif [ \"$2\" = sign ]; then\n  count=$(cat '{}' 2>/dev/null || printf 0)\n  count=$((count + 1))\n  printf '%s\\n' \"$count\" > '{}'\n  if [ \"$count\" -eq 2 ]; then exit 1; fi\nfi\nexec ssh-keygen \"$@\"\n", counter.display(), counter.display())).unwrap();
    fs::set_permissions(&signer, fs::Permissions::from_mode(0o700)).unwrap();
    git(
        &d.receiver.root,
        &["config", "gpg.ssh.program", signer.to_str().unwrap()],
    );
    let result = d.result(1);
    assert_eq!(result["stopped_at"], "sign");
    let sign = transcript_step(&result, "sign");
    assert!(sign["effects"]["pending_rewrite"].is_null());
    assert!(sign["effects"]["refs"].as_array().unwrap().is_empty());
    assert!(sign["event_ids"].as_array().unwrap().is_empty());
    let commits: Vec<_> = sign["effects"]["objects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|object| object["kind"] == "commit")
        .collect();
    assert_eq!(commits.len(), 1);
    let oid = commits[0]["oid"].as_str().unwrap();
    assert!(git_out(&d.receiver.root, &["cat-file", "commit", oid]).contains("gpgsig "));
    assert_eq!(git_out(&d.receiver.root, &["rev-parse", &d.branch]), d.head);
    assert_eq!(result["prepared_head"], d.head);
    assert!(result["next_steps"].to_string().contains("retry"));
    git(
        &d.receiver.root,
        &["config", "gpg.ssh.program", "ssh-keygen"],
    );
    assert_eq!(d.result(3)["preparation_complete"], true);
}

impl Delivery {
    fn hour_ahead(&self) {
        let future = chrono::Utc::now() + chrono::TimeDelta::hours(1);
        self.mutate_bundle(|bundle| {
            let mut ids = std::collections::BTreeMap::new();
            for (index, event) in bundle["events"].as_array().unwrap().iter().enumerate() {
                ids.insert(
                    event["event_id"].as_str().unwrap().to_string(),
                    ulid::Ulid::from_parts(future.timestamp_millis() as u64, index as u128)
                        .to_string(),
                );
            }
            fn remap(
                value: &mut serde_json::Value,
                ids: &std::collections::BTreeMap<String, String>,
            ) {
                match value {
                    serde_json::Value::String(text) => {
                        if let Some(id) = ids.get(text) {
                            *text = id.clone();
                        }
                    }
                    serde_json::Value::Array(items) => {
                        for item in items {
                            remap(item, ids);
                        }
                    }
                    serde_json::Value::Object(items) => {
                        for item in items.values_mut() {
                            remap(item, ids);
                        }
                    }
                    _ => {}
                }
            }
            remap(&mut bundle["events"], &ids);
            for event in bundle["events"].as_array_mut().unwrap() {
                event["created_at"] = future.to_rfc3339().into();
            }
        });
    }
}

fn assert_fresh_writer_follows_future_history(writer: &str) {
    let d = Delivery::new();
    d.hour_ahead();
    assert_eq!(d.result(3)["preparation_complete"], true);
    let before = fixture_events(&d.receiver, &d.change);
    let floor = before.last().unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut command = d.receiver.arc(&d.receiver.root);
    command.env("ARC_ROLE", "lead").args([writer, &d.change]);
    match writer {
        "debt" | "hold" => {
            command.args(["--reason", "fixture follow-up"]);
        }
        "message" => {
            command.args(["--type", "note", "--summary", "fixture follow-up"]);
        }
        _ => {}
    }
    command.assert().success();
    let shown = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    assert_eq!(shown["change_id"], d.change);
    let after = fixture_events(&d.receiver, &d.change);
    let appended: Vec<_> = after
        .iter()
        .filter(|event| {
            !before
                .iter()
                .any(|held| held["event_id"] == event["event_id"])
        })
        .collect();
    assert_eq!(appended.len(), 1, "{writer}");
    assert!(
        appended[0]["event_id"].as_str().unwrap() > floor.as_str(),
        "{writer}"
    );
}

#[test]
fn accept_delivery_fresh_debt_follows_future_history() {
    assert_fresh_writer_follows_future_history("debt");
}
#[test]
fn accept_delivery_fresh_hold_follows_future_history() {
    assert_fresh_writer_follows_future_history("hold");
}
#[test]
fn accept_delivery_fresh_claim_follows_future_history() {
    assert_fresh_writer_follows_future_history("claim");
}
#[test]
fn accept_delivery_fresh_message_follows_future_history() {
    assert_fresh_writer_follows_future_history("message");
}

#[test]
fn accept_delivery_binding_failure_reports_owner_and_printed_retry_completes() {
    use std::os::unix::fs::PermissionsExt;
    let d = Delivery::new();
    d.import();
    let events = d
        .receiver
        .root
        .join(".git/arc/changes")
        .join(&d.change)
        .join("events");
    fs::create_dir_all(&events).unwrap();
    fs::set_permissions(&events, fs::Permissions::from_mode(0o500)).unwrap();
    let output = d.command().assert().code(1).get_output().clone();
    fs::set_permissions(&events, fs::Permissions::from_mode(0o700)).unwrap();
    let failed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(failed["stopped_at"], "checkout-binding");
    let checkout = PathBuf::from(failed["worktree"].as_str().unwrap());
    let admin = PathBuf::from(git_out(&checkout, &["rev-parse", "--absolute-git-dir"]));
    let owner_path = admin.join("arc-owner");
    let owner: serde_json::Value = serde_json::from_slice(&fs::read(&owner_path).unwrap()).unwrap();
    let effects = &transcript_step(&failed, "checkout-binding")["effects"];
    let written = effects["owner_records"].as_array().unwrap();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0]["path"], owner_path.to_str().unwrap());
    assert_eq!(written[0]["token"], owner["token"]);
    assert_eq!(effects["pending_binding"]["owner_token"], owner["token"]);
    assert!(effects["pending_binding"]["binding_event_id"].is_null());
    assert_eq!(
        transcript_step(&failed, "checkout-allocation")["effects"]["worktrees_added"][0]["path"],
        checkout.to_str().unwrap()
    );
    assert!(fixture_events(&d.receiver, &d.change)
        .iter()
        .all(|event| event["event_type"] != "change-checkout-bound"));
    json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    let sentinel = checkout.join("sentinel.txt");
    fs::write(&sentinel, "unrelated content\n").unwrap();
    d.command().assert().code(1);
    assert_eq!(
        fs::read_to_string(&sentinel).unwrap(),
        "unrelated content\n"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&owner_path).unwrap()).unwrap(),
        owner
    );
    fs::remove_file(&sentinel).unwrap();
    let mut displaced = owner.clone();
    displaced["token"] = "unrelated-owner".into();
    fs::write(&owner_path, serde_json::to_vec(&displaced).unwrap()).unwrap();
    d.command().assert().code(1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&owner_path).unwrap()).unwrap(),
        displaced
    );
    fs::write(&owner_path, serde_json::to_vec(&owner).unwrap()).unwrap();
    let retry = failed["next_steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value.as_str())
        .find_map(|line| line.split_once("retry: ").map(|(_, command)| command))
        .unwrap();
    let output = d
        .receiver
        .arc_shell(&d.receiver.root, retry)
        .assert()
        .code(3)
        .get_output()
        .clone();
    let prepared: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(prepared["preparation_complete"], true);
    assert_eq!(prepared["worktree"], failed["worktree"]);
    let shown = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    assert_eq!(shown["worktree_created"], true);
    assert_eq!(shown["worktree_owner_token"], owner["token"]);
    assert_eq!(
        fixture_events(&d.receiver, &d.change)
            .iter()
            .filter(|event| event["event_type"] == "change-checkout-bound")
            .count(),
        1
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(owner_path).unwrap()).unwrap(),
        owner
    );
    assert_eq!(git_out(&checkout, &["status", "--porcelain"]), "");
}

#[test]
fn accept_delivery_text_binding_failure_names_retained_owner_and_unpublished_binding() {
    use std::os::unix::fs::PermissionsExt;
    let d = Delivery::new();
    d.import();
    let events = d
        .receiver
        .root
        .join(".git/arc/changes")
        .join(&d.change)
        .join("events");
    fs::create_dir_all(&events).unwrap();
    fs::set_permissions(&events, fs::Permissions::from_mode(0o500)).unwrap();
    let output = d
        .receiver
        .arc(&d.receiver.root)
        .env("ARC_ROLE", "lead")
        .args([
            "accept",
            d.input.to_str().unwrap(),
            "--branch",
            &d.branch,
            "--publisher",
            "channel",
            "--received-sha256",
            &hex::encode(Sha256::digest(fs::read(&d.input).unwrap())),
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    fs::set_permissions(&events, fs::Permissions::from_mode(0o700)).unwrap();
    let checkout = d
        .receiver
        .home
        .join(".worktrees")
        .join(format!("accept-{}", d.change));
    let admin = PathBuf::from(git_out(&checkout, &["rev-parse", "--absolute-git-dir"]));
    let owner: serde_json::Value =
        serde_json::from_slice(&fs::read(admin.join("arc-owner")).unwrap()).unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("owner record written") && text.contains(owner["token"].as_str().unwrap())
    );
    assert!(text.contains("unpublished binding") && text.contains(checkout.to_str().unwrap()));
    let retry = text
        .lines()
        .find_map(|line| line.split_once("retry: ").map(|(_, command)| command))
        .unwrap();
    d.receiver
        .arc_shell(&d.receiver.root, retry)
        .assert()
        .code(3);
    json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
}

impl Delivery {
    /// Record an observation whose content is in the source's custody and
    /// export the delivery with its custody sidecar. Returns the sidecar and
    /// the digest of its manifest, which the bundle binds.
    fn deliver_with_custody(&self) -> (PathBuf, String) {
        let supplied = self.source.home.join("supplied");
        super::forge_custody::materialize(&supplied, &["first", "{}"]);
        let observation = self.source.home.join("observation.json");
        fs::write(
            &observation,
            super::forge_observation::document("read-1", "g1", "first", None).to_string(),
        )
        .unwrap();
        stdout(self.source.arc(&self.source.root).args([
            "forge",
            "observe",
            &self.change,
            "--file",
            observation.to_str().unwrap(),
            "--custody-dir",
            supplied.to_str().unwrap(),
        ]));
        let sidecar = self.source.home.join("delivery.custody");
        stdout(self.source.arc(&self.source.root).args([
            "export",
            &self.change,
            "--output",
            self.input.to_str().unwrap(),
            "--custody-dir",
            sidecar.to_str().unwrap(),
        ]));
        let manifest = hex::encode(Sha256::digest(
            fs::read(sidecar.join("manifest.json")).unwrap(),
        ));
        (sidecar, manifest)
    }

    fn run(&self, extra: &[&str], code: i32) -> serde_json::Value {
        let output = self
            .command()
            .args(extra)
            .assert()
            .code(code)
            .get_output()
            .clone();
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stdout)))
    }
}

fn step_outcome<'a>(result: &'a serde_json::Value, name: &str) -> &'a str {
    result["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == name)
        .unwrap_or_else(|| panic!("no {name} step: {result}"))["outcome"]
        .as_str()
        .unwrap()
}

#[test]
fn accept_delivery_names_unavailable_custody_and_the_flag_that_installs_it() {
    let d = Delivery::new();
    d.gates("true");
    let (_, manifest) = d.deliver_with_custody();
    let note = "custody: sidecar not supplied; 2 references unavailable";

    let preview = d.run(&["--dry-run"], 0);
    assert!(step_outcome(&preview, "import-preview").contains(note));
    let advice = preview["next_steps"].to_string();
    assert!(
        advice.contains("custody unavailable")
            && advice.contains(&manifest)
            && advice.contains("--custody-dir"),
        "{advice}"
    );

    let result = d.run(&[], 3);
    let imported = step_outcome(&result, "import");
    assert!(
        imported.contains(note)
            && imported.contains(&manifest)
            && imported.contains("--custody-dir"),
        "{imported}"
    );
    assert!(result["next_steps"].to_string().contains(&manifest));
    assert!(!d.receiver.root.join(".git/arc/custody").exists());
}

#[test]
fn accept_delivery_installs_a_supplied_custody_sidecar() {
    let d = Delivery::new();
    d.gates("true");
    let (sidecar, manifest) = d.deliver_with_custody();
    let dir = sidecar.to_str().unwrap();

    let preview = d.run(&["--dry-run", "--custody-dir", dir], 0);
    assert_eq!(preview["custody"]["phase"], "planned");
    assert_eq!(preview["custody"]["installed"], 2);
    assert!(
        step_outcome(&preview, "import-preview")
            .contains("custody: 2 would install, 0 held, 0 unavailable"),
        "{preview}"
    );
    assert!(!preview["next_steps"]
        .to_string()
        .contains("custody unavailable"));
    assert!(!d.receiver.root.join(".git/arc/custody").exists());

    // A sidecar that fails judgment stops the acceptance before any effect.
    let damaged = d.source.home.join("damaged.custody");
    let content = fs::read_dir(sidecar.join("sha256"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    fs::create_dir_all(damaged.join("sha256")).unwrap();
    for entry in fs::read_dir(&sidecar).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::copy(entry.path(), damaged.join(entry.file_name())).unwrap();
        }
    }
    for entry in fs::read_dir(sidecar.join("sha256")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), damaged.join("sha256").join(entry.file_name())).unwrap();
    }
    fs::write(
        damaged.join("sha256").join(content.file_name()),
        "damaged!!",
    )
    .unwrap();
    let refused = d.run(&["--custody-dir", damaged.to_str().unwrap()], 1);
    assert_eq!(refused["stopped_at"], "import-preview");
    assert!(!d.receiver.root.join(".git/arc/changes").exists());
    assert!(!d.receiver.root.join(".git/arc/custody").exists());

    let result = d.run(&["--custody-dir", dir], 3);
    assert_eq!(result["custody"]["phase"], "installed");
    assert_eq!(result["custody"]["installed"], 2);
    let imported = step_outcome(&result, "import");
    assert!(
        imported.contains("custody: 2 installed, 0 held, 0 unavailable (complete)"),
        "{imported}"
    );
    assert!(!result["next_steps"].to_string().contains(&manifest));
    for body in ["first", "{}"] {
        assert_eq!(
            fs::read(super::forge_custody::custody_file(&d.receiver, body)).unwrap(),
            body.as_bytes()
        );
    }
}

/// Admitting a pending rewrite stays a signature-only judgement: a pending
/// map whose successor only appended a role entry is no signing, and accept
/// refuses to recover it. Rejects admission reading the approval-carry rule.
#[test]
fn accept_refuses_a_pending_rewrite_that_only_added_role_entries() {
    use std::os::unix::fs::PermissionsExt;
    let d = Delivery::new();
    let events = d.receiver.root.join(".git/arc/repository/events");
    fs::create_dir_all(&events).unwrap();
    fs::set_permissions(&events, fs::Permissions::from_mode(0o500)).unwrap();
    d.command().assert().code(1);
    fs::set_permissions(&events, fs::Permissions::from_mode(0o700)).unwrap();
    let intent_path = d
        .receiver
        .root
        .join(".git/arc/repository/rewrite-intent.json");
    let mut intent: serde_json::Value =
        serde_json::from_slice(&fs::read(&intent_path).unwrap()).unwrap();
    assert_eq!(
        git_out(&d.receiver.root, &["rev-parse", &d.branch]),
        d.head,
        "the failed recording left the branch where it was"
    );
    let raw = crate::role_carry::raw_commit(&d.receiver.root, &d.head);
    let added = crate::role_carry::write_commit(
        &d.receiver.root,
        &[
            &raw[..],
            b"\nReviewed-by: Reviewer <reviewer@example.invalid>\n",
        ]
        .concat(),
    );
    intent["mapping"][&d.head] = serde_json::json!(added);
    intent["refs"][format!("refs/heads/{}", d.branch)]["new"] = serde_json::json!(added);
    fs::write(&intent_path, serde_json::to_vec_pretty(&intent).unwrap()).unwrap();

    let output = d.command().assert().code(11).get_output().clone();
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("pending rewrite changes content or provisional ancestry"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        git_out(&d.receiver.root, &["rev-parse", &d.branch]),
        d.head,
        "a refused recovery moves no ref"
    );
}

/// Claim: accept's preflight reads a delivered change closed by a landing as
/// integrated, and refuses to prepare it again. Rejects a preflight that
/// reduces the change's own events alone, where the landed change reads
/// open and the retry is prepared as if it had never shipped.
#[test]
fn accept_delivery_preflight_refuses_a_change_its_landing_integrated() {
    let d = Delivery::new();
    d.local_approval();
    d.gates("true");
    d.result(0);
    let landed = d
        .receiver
        .arc(&d.receiver.root)
        .env("ARC_ROLE", "lead")
        .args(["land", &d.change])
        .output()
        .unwrap();
    assert!(
        landed.status.success(),
        "{}",
        String::from_utf8_lossy(&landed.stderr)
    );
    let retried = d.command().arg("--dry-run").output().unwrap();
    let transcript = String::from_utf8_lossy(&retried.stdout).into_owned()
        + &String::from_utf8_lossy(&retried.stderr);
    assert!(!retried.status.success(), "{transcript}");
    assert!(transcript.contains("is integrated"), "{transcript}");
}

/// Claim: accept refuses measured over-budget passes and invalid totals even with
/// green rows. Rejects an independent acceptance path treating raw pass as sufficient.
#[test]
fn budget_acceptance_refuses_overruns_and_invalid_green_totals() {
    for total in [false, true] {
        let d = Delivery::new();
        d.gates("true");
        let path = d.receiver.root.join(".git/arc/operator-policy.toml");
        let policy = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            format!(
                "{policy}budget='10s'\n{}",
                if total { "[budget]\ntotal='9s'\n" } else { "" }
            ),
        )
        .unwrap();
        d.local_approval();
        let out = d
            .command()
            .env(
                "ARC_TEST_COMMAND_RUN_MS",
                if total { "1000" } else { "11000" },
            )
            .assert()
            .code(5)
            .get_output()
            .stdout
            .clone();
        let result: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            result["stopped_at"],
            if total {
                "readiness"
            } else {
                "gate:acceptance"
            }
        );
        if total {
            assert!(!result["readiness"]["budget_errors"]
                .as_array()
                .unwrap()
                .is_empty());
        }
        let shown = json_stdout(
            d.receiver
                .arc(&d.receiver.root)
                .args(["show", &d.change, "--json"]),
        );
        assert!(shown["closure"].is_null());
        assert_eq!(
            shown["verifications"].as_array().unwrap().last().unwrap()["result"],
            "pass"
        );
    }
}

/// Claim: acceptance imports bypass history but still executes and requires its
/// local gate. Rejects a delivered bypass excusing a failed acceptance gate.
#[test]
fn retired_bypass_does_not_authorize_accept() {
    let d = Delivery::new();
    crate::verification_outcomes::historical_bypass(
        &d.source,
        &d.source.root,
        &d.change,
        "acceptance",
    );
    d.export();
    d.gates("false");
    let policy_path = d.receiver.root.join(".git/arc/operator-policy.toml");
    let policy = fs::read_to_string(&policy_path).unwrap().replace(
        "roles=['facts','rewrite','review-input']",
        "roles=['facts','rewrite','review-input','waiver']",
    );
    fs::write(
        policy_path,
        format!("{policy}bypass='documentation only'\n"),
    )
    .unwrap();
    let before = d.receiver.head(&d.receiver.root);
    let result = d.result(5);
    assert_eq!(result["stopped_at"], "gate:acceptance", "{result}");
    let events = fixture_events(&d.receiver, &d.change);
    assert!(events
        .iter()
        .any(|event| event["event_type"] == "verification-bypassed"));
    assert!(events
        .iter()
        .any(|event| event["event_type"] == "verification-recorded"
            && event["gate"] == "acceptance"
            && event["result"] == "fail"));
    assert_eq!(d.receiver.head(&d.receiver.root), before);
}

/// Claim: delivery acceptance forces a fresh execution of every required gate
/// using its accepted batch. Rejects invoking consenting gates individually,
/// reusing their earlier passes, or omitting per-gate observed receipts.
#[test]
fn accept_delivery_runs_fresh_accepted_concurrent_batch() {
    let d = Delivery::new();
    let file = d.receiver.root.join(".git/arc/operator-policy.toml");
    let original = fs::read_to_string(&file).unwrap();
    fs::write(&file, format!("{original}\n[gates.a]\ncommand='true'\nconcurrent=true\n[gates.b]\ncommand='true'\nconcurrent=true\n")).unwrap();
    d.receiver
        .arc(&d.receiver.root)
        .args(["gates", "measure", "--concurrent"])
        .assert()
        .success();
    let result = d.result(3);
    assert_eq!(result["preparation_complete"], true);
    assert!(
        result["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["name"] == "head-gates"
                && step["event_ids"].as_array().unwrap().len() >= 3),
        "{result}"
    );
    let state = json_stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["show", &d.change, "--json"]),
    );
    let entries = state["verifications"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{state}");
    assert!(entries
        .iter()
        .all(|entry| entry["concurrent_batch"].is_object() && entry["result"] == "pass"));
}

#[test]
fn accept_succeeds_for_unrelated_change_despite_unlanded_rewrite_conflict() {
    let mut d = Delivery::new();
    // Create an unlanded local rewrite on an unrelated change A on the receiver.
    let local_opened = stdout(
        d.receiver
            .arc(&d.receiver.root)
            .args(["begin", "other-local"]),
    );
    let other_change = opened_change_id(&local_opened);
    let other_wt = d.receiver.home.join(".worktrees/repo-other-local");
    d.receiver
        .commit(&other_wt, "other.txt", "other\n", "feat: other local");
    let other_original = d.receiver.head(&other_wt);
    stdout(d.receiver.arc(&other_wt).args(["snapshot", &other_change]));
    d.receiver
        .arc(&other_wt)
        .args([
            "rewrite",
            "trailers",
            "--from",
            &other_original,
            "--no-sign",
            "--append",
            "Planned-by: Local <local@example.invalid>",
        ])
        .assert()
        .success();

    // Inject a contradicting rewrite for `other_original` into the unrelated
    // delivery bundle (as an older exporter would carry).
    let mut bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(&d.input).unwrap()).unwrap();
    let other_successor = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let source_id = bundle["source_replica_id"].as_str().unwrap().to_string();
    bundle["repository_events"] = serde_json::json!([{
        "schema_version": 7,
        "event_id": "01ZZZCONFLICTINGOTHER00000",
        "repository_id": source_id,
        "change_id": "repository",
        "actor": "worker",
        "created_at": "2026-10-28T18:00:00Z",
        "event_type": "history-rewritten",
        "mapping": { other_original.clone(): other_successor },
        "reason": "replica re-signed other change",
        "tool": "arc rewrite sign",
        "refs": {
            "refs/heads/arc/other-local": {
                "old": other_original,
                "new": other_successor
            }
        }
    }]);
    let modified = d.source.home.join("delivery-with-unrelated-conflict.json");
    fs::write(&modified, json_file_bytes(&bundle)).unwrap();
    d.input = modified;

    // `arc accept` for the unrelated delivery must succeed cleanly (exit 3: local review owed).
    let result = d.result(3);
    assert_eq!(result["preparation_complete"], true, "{result}");
    let import_step = result["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "import")
        .unwrap();
    assert_eq!(import_step["exit_code"], 0, "{result}");
    let doctor = json_stdout(d.receiver.arc(&d.receiver.root).args(["doctor", "--json"]));
    assert_eq!(doctor["problems"], serde_json::json!([]), "{doctor}");
}

#[test]
fn accept_dry_run_refuses_conflicting_rewrite_on_delivery_target_branch() {
    let mut d = Delivery::new();

    // Create revisions E, F, G postdating the delivery's base and uncited by the delivery.
    let base = d.receiver.head(&d.receiver.root);
    let tree = git_out(
        &d.receiver.root,
        &["rev-parse", &format!("{base}^{{tree}}")],
    );
    let make_commit = |msg: &str| {
        git_out(
            &d.receiver.root,
            &["commit-tree", &tree, "-p", &base, "-m", msg],
        )
    };
    let commit_e = make_commit("target E");
    let commit_f = make_commit("target F");
    let commit_g = make_commit("target G");

    // Hold E -> F locally on receiver.
    d.receiver
        .arc(&d.receiver.root)
        .args([
            "history",
            "rewrite",
            "--map",
            "-",
            "--reason",
            "local master rewrite",
        ])
        .write_stdin(format!("{commit_e} {commit_f}\n"))
        .assert()
        .success();

    // Give the delivery bundle a conflicting E -> G rewrite moving refs/heads/master.
    let mut bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(&d.input).unwrap()).unwrap();
    let source_id = bundle["source_replica_id"].as_str().unwrap().to_string();
    bundle["repository_events"] = serde_json::json!([{
        "schema_version": 7,
        "event_id": "01ZZZACCEPTTARGETCONFLICT0",
        "repository_id": source_id,
        "change_id": "repository",
        "actor": "worker",
        "created_at": "2026-10-28T18:00:00Z",
        "event_type": "history-rewritten",
        "mapping": { commit_e.clone(): commit_g.clone() },
        "reason": "incoming master rewrite",
        "tool": "arc rewrite sign",
        "refs": {
            "refs/heads/master": {
                "old": commit_e,
                "new": commit_g
            }
        }
    }]);
    let modified = d.source.home.join("delivery-with-target-conflict.json");
    fs::write(&modified, json_file_bytes(&bundle)).unwrap();
    d.input = modified;

    let out = d.command().arg("--dry-run").output().unwrap();
    assert!(
        !out.status.success(),
        "accept --dry-run must refuse conflicting target-branch rewrite; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains(&commit_e) && combined.contains("01ZZZACCEPTTARGETCONFLICT0"),
        "accept --dry-run refusal should surface the target-branch conflict:\n{combined}"
    );
}
