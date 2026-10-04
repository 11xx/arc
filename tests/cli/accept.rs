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
    let binary = env!("CARGO_BIN_EXE_arc");
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
    if applied {
        assert_eq!(effects["refs"].as_array().unwrap().len(), refs.len());
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
    assert_pending_signing_effects(&d, &result, true);
    let prepared = result["prepared_head"].as_str().unwrap();
    assert_ne!(prepared, d.head);
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
    assert_eq!(retry["prepared_head"], prepared);
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
