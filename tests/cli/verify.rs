use super::common::*;

fn write_two_gates(repo: &Repo, first: &str, second: &str) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!("[gates.alpha]\ncommand = {first:?}\n[gates.beta]\ncommand = {second:?}\n"),
    )
    .unwrap();
}

#[test]
fn verify_all_records_every_passing_gate_and_summary() {
    let repo = Repo::new();
    write_two_gates(&repo, "true", "true");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "all-pass", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["verify", "all-pass", "--all", "--note", "full suite"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gates: 2/2 pass"));

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "all-pass",
        "--type",
        "verification-recorded",
    ]));
    let values = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["gate"], "alpha");
    assert_eq!(values[1]["gate"], "beta");
    assert_eq!(values[0]["note"], "full suite");
    assert_eq!(values[1]["note"], "full suite");
    assert!(values[0]["exit_code"].is_i64());
    assert!(values[0]["duration_ms"].is_u64());
}

#[test]
fn verify_all_continues_after_a_failure() {
    let repo = Repo::new();
    write_two_gates(&repo, "false", "true");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "all-fail", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["verify", "all-fail", "--all"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("gates: 1/2 pass"));

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "all-fail",
        "--type",
        "verification-recorded",
    ]));
    assert_eq!(events.lines().count(), 2);
}

#[test]
fn verify_all_parallel_completes_sleep_gates_and_appends_evidence_in_name_order() {
    let repo = Repo::new();
    write_two_gates(&repo, "sleep 1", "sleep 1");
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add parallel gates"]);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "parallel-gates", "--no-worktree"]),
    );

    let started = Instant::now();
    repo.arc(&repo.root)
        .args(["verify", "parallel-gates", "--all", "--parallel"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gates: 2/2 pass"));
    assert!(
        started.elapsed() < Duration::from_millis(1800),
        "two one-second gates should overlap"
    );

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "parallel-gates",
        "--type",
        "verification-recorded",
    ]));
    let values = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let gates = values
        .iter()
        .map(|event| event["gate"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(gates, ["alpha", "beta"]);
    for event in &values {
        assert!(
            event["tested_tree"]
                .as_str()
                .is_some_and(|tree| tree.len() == 40),
            "{event}"
        );
        assert!(event["worktree_dirty"].is_null(), "{event}");
        assert!(!event["tree_moved"].as_bool().unwrap_or(false), "{event}");
    }
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "parallel-gates", "--json"]),
    );
    assert!(
        status["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|gate| gate["green_at_head"] == false),
        "{status}"
    );
}

#[test]
fn verify_all_parallel_marks_a_changed_worktree_and_keeps_it_from_green() {
    let repo = Repo::new();
    write_two_gates(&repo, "touch parallel-side-effect", "true");
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: add mutating parallel gates"],
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "parallel-mutates", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["verify", "parallel-mutates", "--all", "--parallel"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gates: 2/2 pass"));

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "parallel-mutates",
        "--type",
        "verification-recorded",
    ]));
    let values = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2);
    for event in &values {
        assert!(
            event["tested_tree"]
                .as_str()
                .is_some_and(|tree| tree.len() == 40),
            "{event}"
        );
        assert!(event["worktree_dirty"].is_null(), "{event}");
        assert_eq!(event["tree_moved"], true, "{event}");
    }
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "parallel-mutates", "--json"]),
    );
    assert!(
        status["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|gate| gate["green_at_head"] == false),
        "{status}"
    );

    // A passing result from a moved tree is not reusable: --skip-green must
    // rerun the gates that produced the non-reproducible evidence.
    let rerun = stdout(repo.arc(&repo.root).args([
        "verify",
        "parallel-mutates",
        "--all",
        "--parallel",
        "--skip-green",
    ]));
    assert!(!rerun.contains("skipped"), "{rerun}");
    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "parallel-mutates",
        "--type",
        "verification-reused",
    ]));
    assert!(events.trim().is_empty(), "{events}");
}

#[test]
fn verify_all_parallel_stays_not_green_when_a_transient_change_is_restored() {
    let repo = Repo::new();
    write_two_gates(
        &repo,
        "touch parallel-transient; sleep 0.1; rm parallel-transient",
        "sleep 0.2",
    );
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: add transient parallel gates"],
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "parallel-transient", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["verify", "parallel-transient", "--all", "--parallel"])
        .assert()
        .success();

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "parallel-transient",
        "--type",
        "verification-recorded",
    ]));
    for line in events.lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(event["worktree_dirty"].is_null(), "{event}");
        assert!(!event["tree_moved"].as_bool().unwrap_or(false), "{event}");
    }
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "parallel-transient", "--json"]),
    );
    assert!(
        status["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|gate| gate["green_at_head"] == false),
        "{status}"
    );
}

#[test]
fn failing_gate_exposes_only_the_final_4096_output_bytes_in_status() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.failure]\ncommand = \"printf discard; head -c 4096 /dev/zero | tr '\\\\000' x; printf err >&2; exit 1\"\n",
    )
    .unwrap();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "output-tail", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["verify", "output-tail", "--gate", "failure"])
        .assert()
        .code(1);

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "output-tail"]),
    ))
    .unwrap();
    let tail = status["gates"][0]["output_tail"].as_str().unwrap();
    assert_eq!(tail.len(), 4096);
    assert_eq!(tail, format!("{}err", "x".repeat(4093)));
}

#[test]
fn gate_timeout_records_failure_and_kills_the_process_group() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.slow]\ncommand = \"sleep 5 &\"\ntimeout = \"1s\"\n",
    )
    .unwrap();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "gate-timeout", "--no-worktree"]),
    );

    let started = Instant::now();
    repo.arc(&repo.root)
        .args(["verify", "gate-timeout", "--gate", "slow"])
        .assert()
        .code(1);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "timed-out process group was not terminated promptly"
    );

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "gate-timeout"]),
    ))
    .unwrap();
    assert_eq!(status["gates"][0]["result"], "fail");
    assert_eq!(status["gates"][0]["timed_out"], true);
}

#[test]
fn passing_gate_output_is_not_rendered_in_show() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.success]\ncommand = \"printf '\\\\164\\\\157\\\\153\\\\145\\\\156\\\\064\\\\062'\"\n",
    )
    .unwrap();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "pass-tail", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["verify", "pass-tail", "--gate", "success"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args(["show", "pass-tail"])
        .assert()
        .success()
        .stdout(predicates::str::contains("token42").not());
}

#[test]
fn verify_all_rejects_conflicting_flags_without_appending() {
    let repo = Repo::new();
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "all-flags",
        "--no-worktree",
    ])));
    let before = event_count(&repo, &change_id);

    for args in [
        vec!["verify", "all-flags", "--all", "--gate", "x"],
        vec![
            "verify",
            "all-flags",
            "--all",
            "--attest",
            "--result",
            "pass",
        ],
        vec!["verify", "all-flags", "--all", "--command", "true"],
    ] {
        repo.arc(&repo.root).args(args).assert().failure();
    }
    assert_eq!(event_count(&repo, &change_id), before);
}

#[test]
fn verify_all_rejects_missing_or_empty_gates_without_appending() {
    for empty_file in [false, true] {
        let repo = Repo::new();
        if empty_file {
            fs::create_dir_all(repo.root.join(".arc")).unwrap();
            fs::write(repo.root.join(".arc/gates.toml"), "").unwrap();
        }
        let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args([
            "begin",
            "all-empty",
            "--no-worktree",
        ])));
        let before = event_count(&repo, &change_id);
        repo.arc(&repo.root)
            .args(["verify", "all-empty", "--all"])
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "no gates declared for profile local",
            ));
        assert_eq!(event_count(&repo, &change_id), before);
    }
}

/// `verify --attest --result` records externally observed evidence without
/// running anything, counts it toward gate green-ness, and flags it attested
/// in both the machine status and the human render.
#[test]
fn verify_attest_records_gate_evidence_without_running() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    let poison = repo.root.join("EXECUTED");
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!(
            "[gates.smoke]\ncommand = \"touch '{}' && exit 1\"\n",
            poison.display()
        ),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "gates"]);

    stdout(
        repo.arc(&repo.root)
            .args(["begin", "feat-att", "--title", "Attested"]),
    );
    let wt = repo.home.join(".worktrees").join("repo-feat-att");
    repo.commit(&wt, "att.txt", "att\n", "feat: add att");
    stdout(repo.arc(&wt).args(["snapshot", "feat-att"]));
    let tested_revision = repo.head(&wt);

    repo.arc(&wt)
        .args([
            "verify",
            "feat-att",
            "--gate",
            "smoke",
            "--attest",
            "--result",
            "pass",
            "--tested-revision",
            &tested_revision,
            "--execution-host",
            "sandbox",
            "--runner",
            "test-runner",
            "--note",
            "ran in sandbox",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("Pass (attested)"));
    assert!(!poison.exists(), "attested verify must not run the command");

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "feat-att"]))).unwrap();
    let gate = &status["gates"][0];
    assert_eq!(gate["name"], "smoke");
    assert_eq!(gate["result"], "pass");
    assert_eq!(gate["green_at_head"], true);
    assert_eq!(gate["attested"], true);

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "feat-att",
        "--type",
        "verification-recorded",
    ]));
    let recorded: serde_json::Value = serde_json::from_str(events.lines().next().unwrap()).unwrap();
    let payload = recorded.as_object().unwrap();
    assert!(!payload.contains_key("exit_code"));
    assert!(!payload.contains_key("duration_ms"));

    let bundle = repo.home.join("attested-bundle.json");
    repo.arc(&repo.root)
        .args(["export", "feat-att", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let imported_events = stdout(destination.arc(&destination.root).args([
        "events",
        "--change",
        "feat-att",
        "--type",
        "verification-recorded",
    ]));
    let imported: serde_json::Value =
        serde_json::from_str(imported_events.lines().next().unwrap()).unwrap();
    let imported_payload = imported.as_object().unwrap();
    assert!(!imported_payload.contains_key("exit_code"));
    assert!(!imported_payload.contains_key("duration_ms"));

    repo.arc(&repo.root)
        .args(["show", "feat-att"])
        .assert()
        .success()
        .stdout(predicates::str::contains("green at head (attested)"))
        .stdout(predicates::str::contains("Pass (attested)"));
}

#[test]
fn attested_verification_uses_declared_execution_context() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.smoke]\ncommand = \"false\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "test: add smoke gate"]);
    stdout(repo.arc(&repo.root).args(["begin", "external-evidence"]));
    let worktree = repo.home.join(".worktrees/repo-external-evidence");
    repo.commit(
        &worktree,
        "implementation.txt",
        "implementation\n",
        "feat: implementation",
    );
    let tested_revision = repo.head(&worktree);
    repo.arc(&worktree)
        .args(["snapshot", "external-evidence"])
        .assert()
        .success();

    repo.commit(
        &repo.root,
        "recorder.txt",
        "recorder revision\n",
        "test: advance recorder",
    );
    let recorder_revision = repo.head(&repo.root);
    assert_ne!(tested_revision, recorder_revision);
    let recorded = repo
        .arc(&repo.root)
        .args([
            "verify",
            "external-evidence",
            "--gate",
            "smoke",
            "--attest",
            "--result",
            "pass",
            "--tested-revision",
            &tested_revision,
            "--execution-host",
            "sandbox-host",
            "--runner",
            "ci/job-42",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let recorded = String::from_utf8(recorded).unwrap();
    let evidence_event = recorded
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "external-evidence",
        "--type",
        "verification-recorded",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.lines().next().unwrap()).unwrap();
    assert_eq!(event["revision"], tested_revision);
    assert_ne!(event["revision"], recorder_revision);
    assert_eq!(event["hostname"], "sandbox-host");
    assert_eq!(event["runner"], "ci/job-42");

    let status = json_stdout(repo.arc(&repo.root).args(["status", "external-evidence"]));
    let gate = &status["gates"][0];
    assert_eq!(gate["green_at_head"], true);
    assert_eq!(gate["revision"], tested_revision);
    assert_eq!(gate["hostname"], "sandbox-host");
    assert_eq!(gate["runner"], "ci/job-42");
    assert_eq!(gate["evidence_event_id"], evidence_event);
    let human = stdout(repo.arc(&repo.root).args(["show", "external-evidence"]));
    assert!(
        human.contains("attested by ci/job-42 on sandbox-host"),
        "{human}"
    );

    let context_change = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "context-without-attest",
        "--no-worktree",
    ])));
    let before = event_count(&repo, &context_change);
    repo.arc(&repo.root)
        .args([
            "verify",
            "context-without-attest",
            "--command",
            "true",
            "--tested-revision",
            "HEAD",
            "--execution-host",
            "host",
            "--runner",
            "runner",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--tested-revision, --execution-host, and --runner are only valid with --attest",
        ));
    assert_eq!(event_count(&repo, &context_change), before);
}

#[test]
fn probe_records_expected_baseline_fail_and_final_pass_against_one_brief() {
    let repo = Repo::new();
    let change_id = opened_change_id(&stdout(
        repo.arc(&repo.root).args(["begin", "declared-probe"]),
    ));
    let worktree = repo.home.join(".worktrees/repo-declared-probe");
    let brief_base = repo.head(&worktree);
    let before_invalid = event_count(&repo, &change_id);
    repo.arc(&worktree)
        .args([
            "brief",
            "declared-probe",
            "--body-file",
            "-",
            "--probes-json",
            "-",
        ])
        .write_stdin("ambiguous stdin\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--body-file - and --probes-json - cannot both read stdin",
        ));
    assert_eq!(event_count(&repo, &change_id), before_invalid);
    let probes = worktree.join("probes.json");
    fs::write(
        &probes,
        r#"[
  {"name":"marker-exists","command":"test -f probe-marker.txt"},
  {"name":"unexpected-pass","command":"true"}
]"#,
    )
    .unwrap();
    let brief_output = repo
        .arc(&worktree)
        .args([
            "brief",
            "declared-probe",
            "--body-file",
            "-",
            "--probes-json",
            probes.to_str().unwrap(),
        ])
        .write_stdin("probe contract\n")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let brief_output = String::from_utf8(brief_output).unwrap();
    let brief_event = brief_output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();

    repo.arc(&worktree)
        .args([
            "verify",
            "declared-probe",
            "--probe",
            "unexpected-pass",
            "--probe-phase",
            "baseline",
        ])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("Pass"));
    repo.arc(&worktree)
        .args([
            "verify",
            "declared-probe",
            "--probe",
            "marker-exists",
            "--probe-phase",
            "baseline",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("Fail"));

    repo.commit(
        &worktree,
        "probe-marker.txt",
        "present\n",
        "feat: satisfy probe",
    );
    let final_revision = repo.head(&worktree);
    repo.arc(&worktree)
        .args(["snapshot", "declared-probe"])
        .assert()
        .success();
    let before_wrong_revision = event_count(&repo, &change_id);
    repo.arc(&worktree)
        .args([
            "verify",
            "declared-probe",
            "--probe",
            "marker-exists",
            "--probe-phase",
            "baseline",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(format!(
            "baseline probe requires HEAD {brief_base}"
        )));
    assert_eq!(event_count(&repo, &change_id), before_wrong_revision);
    repo.arc(&worktree)
        .args(["verify", "declared-probe", "--probe", "marker-exists"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Pass"));

    let events = stdout(repo.arc(&worktree).args([
        "events",
        "--change",
        "declared-probe",
        "--type",
        "verification-recorded",
    ]));
    let evidence = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let marker = evidence
        .iter()
        .filter(|event| event["probe"]["name"] == "marker-exists")
        .collect::<Vec<_>>();
    assert_eq!(marker.len(), 2);
    assert_eq!(marker[0]["probe"]["brief_event_id"], brief_event);
    assert_eq!(marker[0]["probe"]["phase"], "baseline");
    assert_eq!(marker[0]["revision"], brief_base);
    assert_eq!(marker[0]["result"], "fail");
    assert_eq!(marker[1]["probe"]["brief_event_id"], brief_event);
    assert_eq!(marker[1]["probe"]["phase"], "final");
    assert_eq!(marker[1]["revision"], final_revision);
    assert_eq!(marker[1]["result"], "pass");
    assert!(marker.iter().all(|event| event.get("gate").is_none()));
    let unexpected = evidence
        .iter()
        .find(|event| event["probe"]["name"] == "unexpected-pass")
        .unwrap();
    assert_eq!(unexpected["result"], "pass");
    assert_eq!(unexpected["probe"]["phase"], "baseline");

    let show = json_stdout(
        repo.arc(&worktree)
            .args(["show", "declared-probe", "--json"]),
    );
    assert_eq!(
        show["briefs"][0]["acceptance_probes"][0]["name"],
        "marker-exists"
    );
    assert_eq!(
        show["briefs"][0]["acceptance_probes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn declared_probe_blocks_until_discriminating_evidence_matches_patchset() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.test]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gate"]);
    stdout(repo.arc(&repo.root).args(["begin", "probe-readiness"]));
    let worktree = repo.home.join(".worktrees/repo-probe-readiness");
    let base_revision = repo.head(&worktree);
    let probes = worktree.join("readiness-probes.json");
    fs::write(
        &probes,
        r#"[{"name":"marker-exists","command":"test -f readiness-marker.txt"}]"#,
    )
    .unwrap();
    repo.arc(&worktree)
        .args([
            "brief",
            "probe-readiness",
            "--body-file",
            "-",
            "--probes-json",
            probes.to_str().unwrap(),
        ])
        .write_stdin("readiness contract v1\n")
        .assert()
        .success();
    repo.commit(
        &worktree,
        "readiness-marker.txt",
        "present\n",
        "feat: satisfy readiness probe",
    );
    repo.arc(&worktree)
        .args(["snapshot", "probe-readiness"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["verify", "probe-readiness", "--all"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "probe-readiness", "--verdict", "approved"])
        .assert()
        .success();

    let output = repo
        .arc(&worktree)
        .args(["check", "probe-readiness", "--json"])
        .assert()
        .code(12)
        .get_output()
        .stdout
        .clone();
    let check: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(check["schema"], "arc-check/1");
    assert!(check["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(
            |blocker| blocker["blocker"] == "acceptance-probes-not-green"
                && blocker["exit_code"] == 12
        ));

    repo.arc(&worktree)
        .args(["verify", "probe-readiness", "--probe", "marker-exists"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["check", "probe-readiness"])
        .assert()
        .code(12);

    git(&worktree, &["switch", "--detach", &base_revision]);
    repo.arc(&worktree)
        .args([
            "verify",
            "probe-readiness",
            "--probe",
            "marker-exists",
            "--probe-phase",
            "baseline",
        ])
        .assert()
        .success();
    git(&worktree, &["switch", "arc/probe-readiness"]);
    repo.arc(&worktree)
        .args(["check", "probe-readiness"])
        .assert()
        .success();

    repo.commit(
        &worktree,
        "second-head.txt",
        "new head\n",
        "feat: move patchset head",
    );
    repo.arc(&worktree)
        .args(["snapshot", "probe-readiness"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["verify", "probe-readiness", "--all"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "probe-readiness", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["check", "probe-readiness"])
        .assert()
        .code(12);
    repo.arc(&worktree)
        .args(["verify", "probe-readiness", "--probe", "marker-exists"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["check", "probe-readiness"])
        .assert()
        .success();

    repo.arc(&worktree)
        .args([
            "brief",
            "probe-readiness",
            "--body-file",
            "-",
            "--cause-note",
            "fixture revision",
            "--probes-json",
            probes.to_str().unwrap(),
        ])
        .write_stdin("readiness contract v2\n")
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["snapshot", "probe-readiness"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "probe-readiness", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["check", "probe-readiness"])
        .assert()
        .code(12);

    let status = json_stdout(repo.arc(&worktree).args(["status", "probe-readiness"]));
    assert_eq!(status["schema"], "arc-status/6");
    assert_eq!(status["probes"][0]["name"], "marker-exists");
    assert_eq!(status["probes"][0]["brief_version"], 2);
    assert_eq!(status["probes"][0]["discriminating_at_head"], false);
    let human = stdout(repo.arc(&worktree).args(["show", "probe-readiness"]));
    assert!(human.contains("proves behavioral discrimination, not semantic relevance"));
    assert!(human.contains("reviewer must inspect the baseline output"));
}

/// --attest requires --result; --result without --attest is a usage error; and
/// a failing attestation reports exit 1 while still recording evidence.
#[test]
fn verify_attest_result_flag_pairing_is_enforced() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "feat-pair", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["verify", "feat-pair", "--command", "true", "--attest"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--attest requires --result"));

    repo.arc(&repo.root)
        .args([
            "verify",
            "feat-pair",
            "--command",
            "true",
            "--result",
            "pass",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--result is only valid with --attest",
        ));

    // A failing attestation is recorded and reports failure to the caller.
    let tested_revision = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args([
            "verify",
            "feat-pair",
            "--command",
            "false",
            "--attest",
            "--result",
            "fail",
            "--tested-revision",
            &tested_revision,
            "--execution-host",
            "sandbox",
            "--runner",
            "test-runner",
        ])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("Fail (attested)"));
}

/// A gate that advances the branch head while running triggers an advisory
/// warning; the recorded evidence stays pinned to the pre-gate revision.
#[test]
fn verify_warns_when_head_moves_during_the_gate() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "feat-move"]));
    let wt = repo.home.join(".worktrees").join("repo-feat-move");
    repo.commit(&wt, "move.txt", "move\n", "feat: add move");
    let pre = repo.head(&wt);

    repo.arc(&wt)
        .args([
            "verify",
            "feat-move",
            "--command",
            "git commit --allow-empty -m moved-during-gate",
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains(format!(
            "head moved during verification ({pre}"
        )));

    let post = repo.head(&wt);
    assert_ne!(pre, post, "the gate command must have moved the head");

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "feat-move",
        "--type",
        "verification-recorded",
    ]));
    let recorded: serde_json::Value = serde_json::from_str(events.lines().next().unwrap()).unwrap();
    assert_eq!(recorded["revision"], pre);
}

/// Provenance comparison matches the claim actor against the git identity by
/// name or email. An actor that differs from the author name but equals its
/// email is the same identity, so no mismatch is raised.
#[test]
fn provenance_email_match_suppresses_name_only_false_positive() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "prov-eml"]));
    let wt = repo.home.join(".worktrees").join("repo-prov-eml");
    repo.arc(&wt)
        .env("ARC_ACTOR", "tester@example.invalid")
        .args(["claim", "prov-eml"])
        .assert()
        .success();
    repo.commit(&wt, "prov.txt", "prov\n", "feat: prov");
    repo.arc(&wt)
        .args(["snapshot", "prov-eml"])
        .assert()
        .success();

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "prov-eml"]))).unwrap();
    assert_eq!(status["latest_patchset"]["author"]["name"], "Tester");
    assert_eq!(
        status["latest_patchset"]["author"]["email"],
        "tester@example.invalid"
    );
    assert_eq!(
        status["latest_patchset"]["claim_actor"],
        "tester@example.invalid"
    );
    assert_eq!(status["latest_patchset"]["provenance_mismatch"], false);
    assert_eq!(status["claim"]["provenance_mismatch"], false);

    repo.arc(&repo.root)
        .args(["show", "prov-eml"])
        .assert()
        .success()
        .stdout(predicates::str::contains("PROVENANCE MISMATCH").not());
}

/// Declaring one of the change's own integration gates as an acceptance probe
/// is usually the mistake that makes a probe undischargeable — a gate is
/// expected to be green on both sides. It is not provably one, since a gate
/// command can genuinely fail before a change and pass after, so it is said at
/// the moment of the declaration rather than refused.
#[test]
fn brief_warns_when_a_probe_runs_one_of_the_change_gates() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.test]\ncommand = \"true\"\n\n[gates.publish]\ncommand = \"false\"\nprofiles = [\"release\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gates"]);
    stdout(repo.arc(&repo.root).args(["begin", "gate-as-probe"]));
    let worktree = repo.home.join(".worktrees/repo-gate-as-probe");

    // Whitespace is not what distinguishes a copied gate from a retyped one.
    repo.arc(&worktree)
        .args([
            "brief",
            "gate-as-probe",
            "--body-file",
            "-",
            "--probes-json",
            r#"[{"name":"marker","command":"true "}]"#,
        ])
        .write_stdin("contract v1\n")
        .assert()
        .success()
        .stderr(predicates::str::contains("runs gate \"test\""));

    // A gate this change's profile does not require says nothing about it,
    // and the probe records inline.
    repo.arc(&worktree)
        .args([
            "brief",
            "gate-as-probe",
            "--body-file",
            "-",
            "--cause-note",
            "second declaration",
            "--probes-json",
            r#"[{"name":"marker","command":"false"}]"#,
        ])
        .write_stdin("contract v2\n")
        .assert()
        .success()
        .stderr(predicates::str::contains("runs gate").not());
    let show = json_stdout(
        repo.arc(&worktree)
            .args(["show", "gate-as-probe", "--json"]),
    );
    assert_eq!(show["briefs"][1]["acceptance_probes"][0]["name"], "marker");
}

/// A file that exists was named deliberately, whatever its name looks like.
#[test]
fn probes_json_prefers_an_existing_path_over_inline_json() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "bracket-path"]));
    let worktree = repo.home.join(".worktrees/repo-bracket-path");
    let path = worktree.join("[probes].json");
    fs::write(
        &path,
        r#"[{"name":"from-file","command":"test -f marker.txt"}]"#,
    )
    .unwrap();
    repo.arc(&worktree)
        .args([
            "brief",
            "bracket-path",
            "--body-file",
            "-",
            "--probes-json",
            path.to_str().unwrap(),
        ])
        .write_stdin("contract v1\n")
        .assert()
        .success();
    let show = json_stdout(repo.arc(&worktree).args(["show", "bracket-path", "--json"]));
    assert_eq!(
        show["briefs"][0]["acceptance_probes"][0]["name"],
        "from-file"
    );
}

/// A brief based on the head under review asks for a Fail and a Pass at one
/// revision. Nothing at declaration time can tell that apart from the
/// legitimate rebind — re-recording a brief and re-snapshotting to correct a
/// probe declaration — so the impossibility is named where it is provable: at
/// the blocker, where the base is the head that shipped.
#[test]
fn check_names_a_probe_that_cannot_discharge() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "base-is-head"]));
    let worktree = repo.home.join(".worktrees/repo-base-is-head");
    repo.commit(&worktree, "marker.txt", "present\n", "feat: work");
    let head = repo.head(&worktree);
    repo.arc(&worktree)
        .args([
            "brief",
            "base-is-head",
            "--body-file",
            "-",
            "--base",
            &head,
            "--probes-json",
            r#"[{"name":"marker","command":"test -f marker.txt"}]"#,
        ])
        .write_stdin("contract v1\n")
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["snapshot", "base-is-head"])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "base-is-head", "--json"]),
    );
    assert_eq!(status["probes"][0]["undischargeable"], true, "{status}");
    let check = json_stdout(
        repo.arc(&worktree)
            .args(["check", "base-is-head", "--json"]),
    );
    assert!(
        check["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["blocker"] == "acceptance-probes-not-green"),
        "{check}"
    );

    let text = stdout(repo.arc(&worktree).args(["check", "base-is-head"]));
    assert!(text.contains("cannot discharge"), "{text}");
    let explained = stdout(
        repo.arc(&worktree)
            .args(["check", "base-is-head", "--explain"]),
    );
    assert!(explained.contains("cannot discharge"), "{explained}");

    // Evidence claiming both a Fail and a Pass at one revision is
    // contradictory, not a discharged probe.
    repo.arc(&worktree)
        .args([
            "verify",
            "base-is-head",
            "--probe",
            "marker",
            "--probe-phase",
            "baseline",
            "--attest",
            "--result",
            "fail",
            "--tested-revision",
            &head,
            "--execution-host",
            "elsewhere",
            "--runner",
            "baseline",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "verify",
            "base-is-head",
            "--probe",
            "marker",
            "--attest",
            "--result",
            "pass",
            "--tested-revision",
            &head,
            "--execution-host",
            "elsewhere",
            "--runner",
            "final",
        ])
        .assert()
        .success();
    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "base-is-head", "--json"]),
    );
    assert_eq!(
        status["probes"][0]["discriminating_at_head"], false,
        "{status}"
    );
}

/// A gate and a probe are different objects reached by adjacent flags, so a
/// gate lookup that misses a name the brief declares names the right flag
/// instead of only the file it searched.
#[test]
fn verify_gate_miss_points_at_a_declared_probe() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "gate-miss"]));
    let worktree = repo.home.join(".worktrees/repo-gate-miss");
    repo.arc(&worktree)
        .args([
            "brief",
            "gate-miss",
            "--body-file",
            "-",
            "--probes-json",
            r#"[{"name":"marker","command":"test -f marker.txt"}]"#,
        ])
        .write_stdin("contract v1\n")
        .assert()
        .success();

    repo.arc(&worktree)
        .args(["verify", "gate-miss", "--gate", "marker"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("arc verify --probe marker"));

    // A name that is neither still reports only what it searched.
    repo.arc(&worktree)
        .args(["verify", "gate-miss", "--gate", "nowhere"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("arc verify --probe").not());
}

/// Commands that take the change as an optional positional accept `--change`
/// too, so a caller moving between them does not have to guess which spelling
/// this one wanted.
#[test]
fn change_flag_works_where_the_positional_does() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "flagged"]));
    let worktree = repo.home.join(".worktrees/repo-flagged");
    let positional = stdout(repo.arc(&worktree).args(["log", "flagged"]));
    let flagged = stdout(repo.arc(&worktree).args(["log", "--change", "flagged"]));
    assert_eq!(positional, flagged);
    assert!(!positional.trim().is_empty());

    // Two spellings naming different changes is a mistake, not a precedence
    // question — but a slug and a full ID for one change are one reference.
    stdout(repo.arc(&repo.root).args(["begin", "other"]));
    repo.arc(&worktree)
        .args(["log", "flagged", "--change", "other"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("they disagree"));
    let id = stdout(repo.arc(&worktree).args(["query", "--json"]));
    let id = serde_json::from_str::<serde_json::Value>(&id).unwrap();
    let full = id
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["change_id"].as_str().unwrap().to_string())
        .find(|row| row.starts_with("flagged-"))
        .unwrap();
    repo.arc(&worktree)
        .args(["log", "flagged", "--change", &full])
        .assert()
        .success();

    // Commands that resolve the change themselves take the flag too, and
    // refuse a disagreeing pair on the same terms.
    repo.arc(&worktree)
        .args(["--change", "flagged", "prompt"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["--change", "other", "changelog", "flagged"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("they disagree"));

    // A command that refuses a change beside --tag has to see the flag in
    // order to refuse it.
    repo.arc(&worktree)
        .args(["--change", "flagged", "show", "--tag", "no-such-tag"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not both"));

    // An optional positional that never called infer takes it too.
    repo.arc(&worktree)
        .args(["--change", "flagged", "integrate", "--dry-run"])
        .assert()
        .stderr(predicates::str::contains("provide a change").not());

    // A command with its own --change still binds it locally, and the pair it
    // refuses stays refused when the flag is given before the subcommand.
    repo.arc(&worktree)
        .args(["stats", "--change", "flagged", "--json"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["--change", "flagged", "stats", "--tag", "anything"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("mutually exclusive"));
}

/// Evidence recorded against a commit describes a tree no checkout of that
/// commit reproduces whenever the worktree carried uncommitted work — which is
/// the ordinary shape of agent execution. The run is still recorded, because
/// discarding it would push loops toward not recording at all; it just does
/// not count as green.
#[test]
fn evidence_from_a_dirty_worktree_is_recorded_and_not_green() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gate"]);
    stdout(repo.arc(&repo.root).args(["begin", "dirty"]));
    let wt = repo.home.join(".worktrees/repo-dirty");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");

    // Clean: the recorded tree is the commit's own, and the gate is green.
    repo.arc(&wt)
        .args(["verify", "dirty", "--gate", "unit"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "dirty", "--json"]));
    let gate = &status["gates"][0];
    assert_eq!(gate["green_at_head"], true, "{status}");
    assert_eq!(gate["worktree_dirty"], false, "{status}");
    assert!(
        gate["tested_tree"].as_str().is_some_and(|t| t.len() == 40),
        "{status}"
    );

    // Dirty: recorded, displayed, and not green.
    fs::write(wt.join("work.rs"), "staged but uncommitted\n").unwrap();
    repo.arc(&wt)
        .args(["verify", "dirty", "--gate", "unit"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "dirty", "--json"]));
    let gate = &status["gates"][0];
    assert_eq!(gate["result"], "pass", "{status}");
    assert_eq!(gate["worktree_dirty"], true, "{status}");
    assert_eq!(gate["green_at_head"], false, "{status}");

    // An untracked file is uncommitted work too.
    fs::write(wt.join("work.rs"), "done\n").unwrap();
    fs::write(wt.join("scratch.rs"), "not committed\n").unwrap();
    repo.arc(&wt)
        .args(["verify", "dirty", "--gate", "unit"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "dirty", "--json"]));
    assert_eq!(status["gates"][0]["worktree_dirty"], true, "{status}");
}

/// Re-snapshotting at the same head is how a patchset binds to a corrected
/// brief. Counting that as a revision cycle would inflate the rework signal
/// for exactly the leads careful enough to correct one.
#[test]
fn a_rebind_is_not_a_rework_round() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "rebound"]));
    let wt = repo.home.join(".worktrees/repo-rebound");
    repo.arc(&wt)
        .args(["brief", "rebound", "--body-file", "-"])
        .write_stdin("contract v1\n")
        .assert()
        .success();
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    stdout(repo.arc(&wt).args(["snapshot", "rebound"]));
    repo.arc(&wt)
        .args([
            "review",
            "rebound",
            "--verdict",
            "changes-requested",
            "--cause",
            "brief",
        ])
        .assert()
        .success();

    // Correct the brief and rebind: the code has not moved.
    repo.arc(&wt)
        .args([
            "brief",
            "rebound",
            "--body-file",
            "-",
            "--cause-note",
            "the brief was wrong",
        ])
        .write_stdin("contract v2\n")
        .assert()
        .success();
    stdout(repo.arc(&wt).args(["snapshot", "rebound"]));
    repo.arc(&wt)
        .args(["review", "rebound", "--verdict", "approved"])
        .assert()
        .success();

    let stats = json_stdout(
        repo.arc(&wt)
            .args(["stats", "--change", "rebound", "--json"]),
    );
    assert_eq!(stats["changes"][0]["patchset_count"], 2, "{stats}");
    assert_eq!(stats["changes"][0]["completed_rework_rounds"], 0, "{stats}");
    assert_eq!(stats["changes"][0]["reworked"], false, "{stats}");

    // A later round that does move the code does not retroactively change
    // what the rebind was.
    repo.commit(&wt, "work.rs", "revised\n", "fix: revise");
    stdout(repo.arc(&wt).args(["snapshot", "rebound"]));
    repo.arc(&wt)
        .args(["review", "rebound", "--verdict", "approved"])
        .assert()
        .success();
    let stats = json_stdout(
        repo.arc(&wt)
            .args(["stats", "--change", "rebound", "--json"]),
    );
    assert_eq!(stats["changes"][0]["patchset_count"], 3, "{stats}");
    assert_eq!(stats["changes"][0]["completed_rework_rounds"], 0, "{stats}");
}

/// A request is answered by the patchset that follows it, so a later round
/// approving a patchset that happens to share the requested head does not
/// erase the revision cycle in between.
#[test]
fn a_rework_round_is_paired_with_the_patchset_that_answers_it() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "paired"]));
    let wt = repo.home.join(".worktrees/repo-paired");
    repo.commit(&wt, "work.rs", "first\n", "feat: first");
    stdout(repo.arc(&wt).args(["snapshot", "paired"]));
    repo.arc(&wt)
        .args([
            "review",
            "paired",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();

    repo.commit(&wt, "work.rs", "second\n", "fix: revise");
    stdout(repo.arc(&wt).args(["snapshot", "paired"]));
    repo.arc(&wt)
        .args(["review", "paired", "--verdict", "approved"])
        .assert()
        .success();

    let stats = json_stdout(
        repo.arc(&wt)
            .args(["stats", "--change", "paired", "--json"]),
    );
    assert_eq!(stats["changes"][0]["completed_rework_rounds"], 1, "{stats}");
    assert_eq!(stats["changes"][0]["reworked"], true, "{stats}");
}

/// The recorded tree is kept reachable by a ref. A tree named only in the
/// ledger is a string, and Git does not read JSON.
#[test]
fn a_recorded_tree_survives_garbage_collection() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gate"]);
    stdout(repo.arc(&repo.root).args(["begin", "durable"]));
    let wt = repo.home.join(".worktrees/repo-durable");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    fs::write(wt.join("scratch.rs"), "uncommitted\n").unwrap();
    repo.arc(&wt)
        .args(["verify", "durable", "--gate", "unit"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "durable", "--json"]));
    let tree = status["gates"][0]["tested_tree"]
        .as_str()
        .unwrap()
        .to_string();
    git(&repo.root, &["reflog", "expire", "--expire=now", "--all"]);
    git(&repo.root, &["gc", "--prune=now", "--quiet"]);
    assert!(
        std::process::Command::new("git")
            .args(["cat-file", "-e", &tree])
            .current_dir(&repo.root)
            .status()
            .unwrap()
            .success(),
        "{tree} was collected"
    );

    // A pin that outlives the change forever would grow a ref per run. What
    // survives integration is what is not already reachable from what shipped.
    let refs_before = stdout_lines(&repo.root, "refs/arc/tree/");
    assert!(!refs_before.is_empty(), "{refs_before:?}");
    fs::remove_file(wt.join("scratch.rs")).unwrap();
    stdout(repo.arc(&wt).args(["snapshot", "durable"]));
    repo.arc(&wt)
        .args(["verify", "durable", "--gate", "unit"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["review", "durable", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["integrate", "durable"])
        .assert()
        .success();
    let kept = stdout_lines(&repo.root, "refs/arc/tree/");
    // The clean tree is now reachable from the integration commit and is
    // released; the dirty one never was, so it stays pinned.
    assert!(kept.len() < refs_before.len() + 1, "{kept:?}");
    assert!(
        std::process::Command::new("git")
            .args(["cat-file", "-e", &tree])
            .current_dir(&repo.root)
            .status()
            .unwrap()
            .success(),
        "the dirty tree {tree} must stay pinned"
    );
}

fn stdout_lines(cwd: &std::path::Path, prefix: &str) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", prefix])
        .current_dir(cwd)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}
