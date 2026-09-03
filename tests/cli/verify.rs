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
    // The tree was recorded; what a shared-worktree parallel run cannot
    // establish is whether it was clean. Saying the tree went unrecorded
    // would point at the wrong recovery.
    let resume = stdout(repo.arc(&repo.root).args(["resume", "parallel-gates"]));
    assert!(
        resume.contains("not green at head: the worktree's cleanliness was not recorded"),
        "{resume}"
    );
    assert!(
        !resume.contains("the tested tree was not recorded"),
        "{resume}"
    );
    // Cleaning cannot fix evidence whose cleanliness was never observed; only
    // a sequential rerun can.
    stdout(repo.arc(&repo.root).args(["snapshot", "parallel-gates"]));
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "parallel-gates", "--json"]),
    );
    assert!(
        status["next_action"]
            .as_str()
            .unwrap()
            .starts_with("run_gate:"),
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

    // The subject is a recording checkout whose own HEAD is not the revision
    // being attested. It gets a branch of its own so the target stays where
    // the change is based; whether a change behind its target is green is a
    // different question, asked elsewhere.
    let recorder = repo.home.join("recorder");
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "recorder",
            recorder.to_str().unwrap(),
        ],
    );
    repo.commit(
        &recorder,
        "recorder.txt",
        "recorder revision\n",
        "test: advance recorder",
    );
    let recorder_revision = repo.head(&recorder);
    assert_ne!(tested_revision, recorder_revision);
    let recorded = repo
        .arc(&recorder)
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
    assert_eq!(check["schema"], "arc-check/3");
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
    assert_eq!(status["schema"], "arc-status/17");
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
        .args(["snapshot", "prov-eml", "--solo"])
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

/// Dirt stays fatal by default, so the exception is declared rather than
/// assumed. The waiver binds the way the evidence it excuses binds — to one
/// revision — so the next commit ends it instead of leaving a standing
/// exemption with no principal and no expiry.
#[test]
fn a_dirty_tree_waiver_lets_evidence_count_and_dies_at_the_next_commit() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gate"]);
    stdout(repo.arc(&repo.root).args(["begin", "waived"]));
    let wt = repo.home.join(".worktrees/repo-waived");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");

    // An untracked file nobody added: dirty, recorded, and not green.
    fs::write(wt.join("scratch.rs"), "not committed\n").unwrap();
    repo.arc(&wt)
        .args(["verify", "waived", "--gate", "unit"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "waived", "--json"]));
    assert_eq!(status["gates"][0]["green_at_head"], false, "{status}");

    // A waiver must say why: excusing the gate with no stated reason records
    // the exemption and loses the only thing a reviewer could disagree with.
    repo.arc(&wt)
        .args(["verify", "waived", "--gate", "unit", "--waive-dirty", "   "])
        .assert()
        .failure()
        .stderr(predicates::str::contains("must say why"));

    // Declared: the same dirty evidence now counts, and the waiver is visible
    // to whoever reviews the change.
    repo.arc(&wt)
        .args([
            "verify",
            "waived",
            "--gate",
            "unit",
            "--waive-dirty",
            "untracked fixture the build does not read",
        ])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "waived", "--json"]));
    assert_eq!(status["gates"][0]["worktree_dirty"], true, "{status}");
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
    assert_eq!(
        status["dirty_tree_waiver"]["reason"], "untracked fixture the build does not read",
        "{status}"
    );

    // The next commit ends it. Evidence recorded dirty at the new head is not
    // excused by a waiver declared against the old one.
    repo.commit(&wt, "more.rs", "more\n", "feat: more");
    fs::write(wt.join("scratch2.rs"), "still not committed\n").unwrap();
    repo.arc(&wt)
        .args(["verify", "waived", "--gate", "unit"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "waived", "--json"]));
    assert_eq!(status["gates"][0]["green_at_head"], false, "{status}");
    assert!(status["dirty_tree_waiver"].is_null(), "{status}");
}

/// A waiver excuses dirt somebody observed. A parallel batch records
/// cleanliness as unknown on purpose — a shared batch cannot tell whether one
/// gate changed and restored a file while another was still running — and a
/// waiver that covered that would vouch for a tree nobody saw.
#[test]
fn a_waiver_cannot_green_evidence_whose_cleanliness_is_unknown() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gate"]);
    stdout(repo.arc(&repo.root).args(["begin", "unknown-prov"]));
    let wt = repo.home.join(".worktrees/repo-unknown-prov");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    fs::write(wt.join("scratch.rs"), "not committed\n").unwrap();

    // Asking for both is refused rather than silently recording a waiver that
    // could never make anything green.
    repo.arc(&wt)
        .args([
            "verify",
            "unknown-prov",
            "--all",
            "--parallel",
            "--waive-dirty",
            "untracked fixture",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--waive-dirty cannot be combined with --parallel",
        ));

    // And a waiver already standing at this revision does not rescue evidence
    // the parallel path recorded with unknown provenance.
    repo.arc(&wt)
        .args([
            "verify",
            "unknown-prov",
            "--gate",
            "unit",
            "--waive-dirty",
            "untracked fixture",
        ])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["verify", "unknown-prov", "--all", "--parallel"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "unknown-prov", "--json"]));
    let gate = &status["gates"][0];
    assert!(gate["worktree_dirty"].is_null(), "{status}");
    assert_eq!(gate["green_at_head"], false, "{status}");
}

/// One bool could not say which kind of dirt wedged the gate, so the premise
/// that this fires overwhelmingly on untracked-only dirt was unmeasurable and
/// a waiver reason had to be written from memory.
#[test]
fn recorded_evidence_separates_tracked_dirt_from_untracked() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gate"]);
    stdout(repo.arc(&repo.root).args(["begin", "split"]));
    let wt = repo.home.join(".worktrees/repo-split");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");

    // Untracked only.
    fs::write(wt.join("scratch.rs"), "not committed\n").unwrap();
    repo.arc(&wt)
        .args(["verify", "split", "--gate", "unit"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "split", "--json"]));
    let gate = &status["gates"][0];
    assert_eq!(gate["worktree_dirty"], true, "{status}");
    assert_eq!(gate["worktree_dirty_tracked"], false, "{status}");
    assert_eq!(gate["worktree_dirty_untracked"], true, "{status}");

    // Tracked as well.
    fs::write(wt.join("work.rs"), "edited\n").unwrap();
    repo.arc(&wt)
        .args(["verify", "split", "--gate", "unit"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "split", "--json"]));
    let gate = &status["gates"][0];
    assert_eq!(gate["worktree_dirty_tracked"], true, "{status}");
    assert_eq!(gate["worktree_dirty_untracked"], true, "{status}");
}

/// A gate whose evidence cannot be reused reads `pass` in every raw result
/// field, so a caller who is only shown the result concludes the opposite of
/// what readiness concluded. Every human-facing surface names the reason, and
/// the next step names the tree rather than another run against it — re-running
/// a gate on a still-dirty tree records the same unusable evidence forever.
#[test]
fn dirty_gate_evidence_is_named_by_resume_check_and_the_next_action() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gate"]);
    stdout(repo.arc(&repo.root).args(["begin", "named"]));
    let wt = repo.home.join(".worktrees/repo-named");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    repo.arc(&wt).args(["snapshot", "named"]).assert().success();
    fs::write(wt.join("scratch.rs"), "untracked\n").unwrap();
    repo.arc(&wt)
        .args(["verify", "named", "--gate", "unit"])
        .assert()
        .success();

    let resume = stdout(repo.arc(&wt).args(["resume", "named"]));
    assert!(
        resume.contains("unit: pass (not green at head: evidence recorded on a dirty worktree"),
        "{resume}"
    );

    let check = String::from_utf8(
        repo.arc(&wt)
            .args(["check", "named"])
            .assert()
            // Not the gate exit code: an unapproved head outranks a gate that
            // is not green, and this change has no verdict. Pinning 3 keeps
            // that precedence asserted rather than accepting either code.
            .code(3)
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        check.contains("not green at head: evidence recorded on a dirty worktree"),
        "{check}"
    );

    let status = json_stdout(repo.arc(&wt).args(["status", "named", "--json"]));
    assert_eq!(status["next_action"], "clean_worktree:unit", "{status}");

    // `show` renders the same evidence as history; it must not read `Pass`
    // beside a summary saying the gate is not green.
    let shown = stdout(repo.arc(&wt).args(["show", "named"]));
    assert!(
        shown.contains("not reusable as evidence: the worktree was dirty"),
        "{shown}"
    );

    // Cleaning the tree at the same head is what the advice asked for, and it
    // must change the advice: the stale evidence can only be replaced by a
    // rerun, which cleaning cannot do.
    fs::remove_file(wt.join("scratch.rs")).unwrap();
    let status = json_stdout(repo.arc(&wt).args(["status", "named", "--json"]));
    assert_eq!(status["next_action"], "run_gate:unit", "{status}");
    assert_eq!(status["gates"][0]["green_at_head"], false, "{status}");

    // Only the rerun clears the gate.
    repo.arc(&wt)
        .args(["verify", "named", "--gate", "unit"])
        .assert()
        .success();
    let resume = stdout(repo.arc(&wt).args(["resume", "named"]));
    assert!(
        resume.contains("unit: pass (undiscriminated)\n"),
        "{resume}"
    );

    // A failing gate on a dirty tree is still told to clean first: while the
    // tree is dirty no run produces usable evidence, whatever the result. The
    // advice terminates because it reads the live tree — once clean, it is the
    // rerun.
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"test ! -e scratch.rs\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: a gate the untracked file fails"],
    );
    git(&wt, &["merge", "--no-edit", "master"]);
    stdout(repo.arc(&wt).args(["snapshot", "named"]));
    fs::write(wt.join("scratch.rs"), "untracked again\n").unwrap();
    repo.arc(&wt)
        .args(["verify", "named", "--gate", "unit"])
        .assert()
        .failure();
    let status = json_stdout(repo.arc(&wt).args(["status", "named", "--json"]));
    let gate = &status["gates"][0];
    assert_eq!(gate["result"], "fail", "{status}");
    assert_eq!(gate["worktree_dirty"], true, "{status}");
    assert_eq!(gate["revision"], status["current_head"], "{status}");
    assert_eq!(status["head_matches_latest_patchset"], true, "{status}");
    assert_eq!(status["next_action"], "clean_worktree:unit", "{status}");

    // This gate fails *only* because of the untracked file, which is the case
    // a rerun could never clear. Cleaning, then rerunning, reaches green —
    // and the advice walks exactly that path.
    fs::remove_file(wt.join("scratch.rs")).unwrap();
    let status = json_stdout(repo.arc(&wt).args(["status", "named", "--json"]));
    assert_eq!(status["next_action"], "run_gate:unit", "{status}");
    repo.arc(&wt)
        .args(["verify", "named", "--gate", "unit"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "named", "--json"]));
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
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

/// A change opened with `--no-worktree` has nowhere to run its gates. Once the
/// primary branch moves past it, evidence recorded from the primary checkout
/// lands at the wrong revision, status ignores it, and `next_action` keeps
/// advising a gate run that can never complete.
#[test]
fn a_gate_run_away_from_the_change_head_is_refused_with_the_step_that_fixes_it() {
    let repo = Repo::new();
    write_two_gates(&repo, "true", "true");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "checkoutless", "--no-worktree"]),
    );
    repo.commit(
        &repo.root,
        "moved.txt",
        "primary moved on",
        "chore: move on",
    );

    repo.arc(&repo.root)
        .args(["verify", "checkoutless", "--gate", "alpha"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("has no checkout"))
        .stderr(predicates::str::contains("git worktree add"))
        .stderr(predicates::str::contains("--attest --tested-revision"));

    // No evidence was recorded, so nothing landed at a revision status ignores.
    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "checkoutless",
        "--type",
        "verification-recorded",
    ]));
    assert!(events.trim().is_empty(), "{events}");
}

/// A change that does have a checkout needs no refusal: its gates belong to
/// that checkout whichever one the caller stands in, and the evidence lands
/// at its head where status counts it.
#[test]
fn a_gate_run_from_another_checkout_runs_in_the_change_worktree() {
    let repo = Repo::new();
    write_two_gates(&repo, "true", "true");
    stdout(repo.arc(&repo.root).args(["begin", "elsewhere"]));
    let worktree = repo.home.join(".worktrees/repo-elsewhere");
    repo.commit(
        &repo.root,
        "moved.txt",
        "primary moved on",
        "chore: move on",
    );

    repo.arc(&repo.root)
        .args(["verify", "elsewhere", "--all"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "running in {}",
            worktree.display()
        )))
        .stdout(predicates::str::contains("gates: 2/2 pass"));

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "elsewhere",
        "--type",
        "verification-recorded",
    ]));
    let first: serde_json::Value =
        serde_json::from_str(events.lines().next().expect("alpha ran")).unwrap();
    assert_eq!(first["revision"], repo.head(&worktree), "{first}");
}

/// Attestation is the documented escape for evidence arc did not run, and it
/// carries its own revision, so the refusal must not reach it.
#[test]
fn attested_evidence_is_exempt_from_the_change_head_check() {
    let repo = Repo::new();
    write_two_gates(&repo, "true", "true");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "attested", "--no-worktree"]),
    );
    let change_head = repo.head(&repo.root);
    repo.commit(
        &repo.root,
        "moved.txt",
        "primary moved on",
        "chore: move on",
    );

    repo.arc(&repo.root)
        .args([
            "verify",
            "attested",
            "--gate",
            "alpha",
            "--attest",
            "--result",
            "pass",
            "--tested-revision",
            &change_head,
            "--execution-host",
            "runner-1",
            "--runner",
            "ci",
        ])
        .assert()
        .success();
}

/// A probe at its default Final phase records evidence status counts only at
/// the patchset head, so it belongs to the change's checkout exactly as a gate
/// does. Only a baseline probe belongs off the head.
#[test]
fn a_final_probe_run_from_another_checkout_records_at_the_change_head() {
    let repo = Repo::new();
    let change_id = opened_change_id(&stdout(
        repo.arc(&repo.root).args(["begin", "final-probe-head"]),
    ));
    let worktree = repo.home.join(".worktrees/repo-final-probe-head");
    let probes = worktree.join("probes.json");
    fs::write(
        &probes,
        r#"[{"name":"marker-exists","command":"test -f probe-marker.txt"}]"#,
    )
    .unwrap();
    repo.arc(&worktree)
        .args([
            "brief",
            "final-probe-head",
            "--body-file",
            "-",
            "--probes-json",
            probes.to_str().unwrap(),
        ])
        .write_stdin("probe contract\n")
        .assert()
        .success();
    repo.commit(
        &repo.root,
        "moved.txt",
        "primary moved on",
        "chore: move on",
    );

    repo.arc(&repo.root)
        .args(["verify", "final-probe-head", "--probe", "marker-exists"])
        .assert()
        .failure();

    // The evidence sits at the change's head, where status counts it, rather
    // than at the head of the checkout the command was typed in.
    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        &change_id,
        "--type",
        "verification-recorded",
    ]));
    let recorded: serde_json::Value =
        serde_json::from_str(events.lines().next().expect("the probe ran")).unwrap();
    assert_eq!(recorded["revision"], repo.head(&worktree), "{recorded}");
    assert_ne!(recorded["revision"], repo.head(&repo.root), "{recorded}");
}

/// A baseline probe is the one evidence kind that must sit off the change
/// head, so the refusal must not reach it. It has its own, stricter rule.
#[test]
fn a_baseline_probe_still_runs_at_the_brief_base_revision() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "baseline-probe-head"]));
    let worktree = repo.home.join(".worktrees/repo-baseline-probe-head");
    let probes = worktree.join("probes.json");
    fs::write(
        &probes,
        r#"[{"name":"marker-exists","command":"test -f probe-marker.txt"}]"#,
    )
    .unwrap();
    repo.arc(&worktree)
        .args([
            "brief",
            "baseline-probe-head",
            "--body-file",
            "-",
            "--probes-json",
            probes.to_str().unwrap(),
        ])
        .write_stdin("probe contract\n")
        .assert()
        .success();

    repo.arc(&worktree)
        .args([
            "verify",
            "baseline-probe-head",
            "--probe",
            "marker-exists",
            "--probe-phase",
            "baseline",
        ])
        .assert()
        .success();
}

/// Attestation is the caller's assertion, so arc takes the revision it is
/// given — but evidence off the change head is ignored exactly as it is for a
/// gate arc ran itself, and silence is what makes that a trap.
#[test]
fn attesting_off_the_change_head_warns_that_the_evidence_will_not_count() {
    let repo = Repo::new();
    write_two_gates(&repo, "true", "true");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "attest-off-head", "--no-worktree"]),
    );
    repo.commit(
        &repo.root,
        "moved.txt",
        "primary moved on",
        "chore: move on",
    );
    let off_head = repo.head(&repo.root);

    repo.arc(&repo.root)
        .args([
            "verify",
            "attest-off-head",
            "--gate",
            "alpha",
            "--attest",
            "--result",
            "pass",
            "--tested-revision",
            &off_head,
            "--execution-host",
            "runner-1",
            "--runner",
            "ci",
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains("will not discharge the gate"));
}

/// A checkout in the wrong state is not a missing checkout: advising
/// `git worktree add` beside a worktree that already exists is advice that
/// cannot be followed, which is the class of bug this change exists to close.
#[test]
fn a_detached_head_in_the_change_worktree_is_not_reported_as_having_no_checkout() {
    let repo = Repo::new();
    write_two_gates(&repo, "true", "true");
    stdout(repo.arc(&repo.root).args(["begin", "detached-here"]));
    let worktree = repo.home.join(".worktrees/repo-detached-here");
    repo.commit(&worktree, "work.txt", "work", "feat: work");
    git(&worktree, &["checkout", "--detach", "HEAD~1"]);

    for cwd in [&worktree, &repo.root] {
        repo.arc(cwd)
            .args(["verify", "detached-here", "--gate", "alpha"])
            .assert()
            .failure()
            .stderr(predicates::str::contains(format!(
                "{} but that worktree's HEAD is not its branch head",
                worktree.display()
            )))
            .stderr(predicates::str::contains(format!(
                "git -C {} checkout arc/detached-here",
                worktree.display()
            )))
            .stderr(predicates::str::contains("has no checkout").not());
    }
}

/// The ledger records whatever worktree path it was given, so a change opened
/// through a symlinked path stores the unresolved one while the caller's cwd
/// is always the kernel-resolved path. A lexical comparison of the two falls
/// through to the "no checkout" diagnosis, advising `git worktree add` beside
/// the checkout the caller is standing in.
#[test]
fn a_worktree_recorded_through_a_symlink_still_diagnoses_the_wrong_checkout_state() {
    let repo = Repo::new();
    write_two_gates(&repo, "true", "true");
    let real = repo.home.join("real-worktrees");
    fs::create_dir_all(&real).unwrap();
    let link = repo.home.join("linked-worktrees");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    stdout(repo.arc(&repo.root).args([
        "begin",
        "linked-here",
        "--worktree",
        link.join("wt").to_str().unwrap(),
    ]));
    let worktree = real.join("wt");
    repo.commit(&worktree, "work.txt", "work", "feat: work");
    git(&worktree, &["checkout", "--detach", "HEAD~1"]);

    repo.arc(&worktree)
        .args(["verify", "linked-here", "--gate", "alpha"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "but that worktree's HEAD is not its branch head",
        ))
        .stderr(predicates::str::contains("has no checkout").not());
}

/// A change declaring a gate that fails until a marker is committed, beside a
/// gate that only ever passes. Returns the change ID and the failing
/// evidence's event ID, with the fix committed and the gate not yet rerun.
fn change_with_a_fixed_gate(repo: &Repo, slug: &str) -> (String, String) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.fixable]\ncommand = \"test -f marker\"\n[gates.plain]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", "."]);
    git(&repo.root, &["commit", "-m", "gates"]);
    let begun =
        stdout(
            repo.arc(&repo.root)
                .args(["begin", slug, "--no-worktree", "--target", "master"]),
        );
    let change_id = begun
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .expect("begin names the change")
        .to_string();

    repo.arc(&repo.root)
        .args(["verify", slug, "--gate", "fixable"])
        .assert()
        .code(1);
    let failing = last_verification_event_id(repo, slug, "fail");
    repo.commit(&repo.root, "marker", "", "fix: add marker");
    (change_id, failing)
}

fn last_verification_event_id(repo: &Repo, slug: &str, result: &str) -> String {
    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        slug,
        "--type",
        "verification-recorded",
    ]));
    events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .rfind(|event| event["result"] == result)
        .expect("a verification with that result")["event_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn last_gate_verification_event_id(repo: &Repo, slug: &str, gate: &str, result: &str) -> String {
    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        slug,
        "--type",
        "verification-recorded",
    ]));
    events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .rfind(|event| event["result"] == result && event["gate"] == gate)
        .expect("a verification of that gate with that result")["event_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn gate_row(repo: &Repo, slug: &str, name: &str) -> serde_json::Value {
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", slug]))).unwrap();
    status["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|gate| gate["name"] == name)
        .expect("gate row")
        .clone()
}

#[test]
fn a_falsified_pass_records_the_failure_it_answers() {
    let repo = Repo::new();
    let (_, failing) = change_with_a_fixed_gate(&repo, "falsified");

    repo.arc(&repo.root)
        .args([
            "verify",
            "falsified",
            "--gate",
            "fixable",
            "--falsified-by",
            &failing,
            "--predicted",
            "marker absent",
        ])
        .assert()
        .success();

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "falsified",
        "--type",
        "verification-recorded",
    ]));
    let recorded: serde_json::Value =
        serde_json::from_str(events.lines().next_back().unwrap()).unwrap();
    assert_eq!(recorded["result"], "pass");
    assert_eq!(recorded["falsification"]["event_id"], failing.as_str());
    assert_eq!(
        recorded["falsification"]["predicted_reason"],
        "marker absent"
    );
    // The revision is read from the referenced failure, never from the caller,
    // so the two halves of the reference cannot disagree.
    let failed_at = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "falsified",
        "--type",
        "verification-recorded",
    ]))
    .lines()
    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
    .find(|event| event["event_id"] == failing.as_str())
    .unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(recorded["falsification"]["revision"], failed_at.as_str());
}

#[test]
fn a_falsification_reference_to_a_passing_event_is_refused() {
    let repo = Repo::new();
    change_with_a_fixed_gate(&repo, "ref-pass");
    repo.arc(&repo.root)
        .args(["verify", "ref-pass", "--gate", "plain"])
        .assert()
        .success();
    let passing = last_verification_event_id(&repo, "ref-pass", "pass");

    repo.arc(&repo.root)
        .args([
            "verify",
            "ref-pass",
            "--gate",
            "plain",
            "--falsified-by",
            &passing,
            "--predicted",
            "anything",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("recorded a pass"));
}

#[test]
fn a_falsification_reference_to_another_gate_is_refused() {
    let repo = Repo::new();
    let (_, failing) = change_with_a_fixed_gate(&repo, "ref-other-gate");

    repo.arc(&repo.root)
        .args([
            "verify",
            "ref-other-gate",
            "--gate",
            "plain",
            "--falsified-by",
            &failing,
            "--predicted",
            "marker absent",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "recorded gate \"fixable\", not gate \"plain\"",
        ));
}

#[test]
fn a_falsification_reference_to_another_change_is_refused() {
    let repo = Repo::new();
    let (_, failing) = change_with_a_fixed_gate(&repo, "ref-source");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "ref-target", "--target", "master"]),
    );
    let worktree = repo.home.join(".worktrees").join("repo-ref-target");

    repo.arc(&worktree)
        .args([
            "verify",
            "ref-target",
            "--gate",
            "plain",
            "--falsified-by",
            &failing,
            "--predicted",
            "marker absent",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "names no earlier verification on this change",
        ));
}

/// The revision is derived at write time, so only a ledger edited by hand or
/// imported from elsewhere can carry a reference whose halves disagree. It is
/// refused on load rather than believed.
#[test]
fn a_falsification_reference_with_a_mismatched_revision_is_refused_on_load() {
    let repo = Repo::new();
    let (change_id, failing) = change_with_a_fixed_gate(&repo, "ref-revision");
    repo.arc(&repo.root)
        .args([
            "verify",
            "ref-revision",
            "--gate",
            "fixable",
            "--falsified-by",
            &failing,
            "--predicted",
            "marker absent",
        ])
        .assert()
        .success();
    rewrite_event(&repo, &change_id, "verification-recorded", |event| {
        event["falsification"]["revision"] = serde_json::Value::String("0".repeat(40));
    });

    repo.arc(&repo.root)
        .args(["status", "ref-revision"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("was recorded at"));
}

#[test]
fn each_half_of_a_falsification_reference_requires_the_other() {
    let repo = Repo::new();
    let (_, failing) = change_with_a_fixed_gate(&repo, "halves");

    repo.arc(&repo.root)
        .args([
            "verify",
            "halves",
            "--gate",
            "plain",
            "--predicted",
            "marker absent",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--predicted requires --falsified-by",
        ));

    repo.arc(&repo.root)
        .args([
            "verify",
            "halves",
            "--gate",
            "plain",
            "--falsified-by",
            &failing,
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--falsified-by requires --predicted",
        ));
}

#[test]
fn a_falsification_is_refused_before_the_gate_runs() {
    let repo = Repo::new();
    change_with_a_fixed_gate(&repo, "no-run");

    repo.arc(&repo.root)
        .args([
            "verify",
            "no-run",
            "--gate",
            "plain",
            "--falsified-by",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "--predicted",
            "marker absent",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("running:").not());
}

#[test]
fn discrimination_renders_in_both_states_in_json_and_text() {
    let repo = Repo::new();
    let (_, failing) = change_with_a_fixed_gate(&repo, "both-states");
    repo.arc(&repo.root)
        .args([
            "verify",
            "both-states",
            "--gate",
            "fixable",
            "--falsified-by",
            &failing,
            "--predicted",
            "marker absent",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["verify", "both-states", "--gate", "plain"])
        .assert()
        .success();

    let fixable = gate_row(&repo, "both-states", "fixable");
    assert_eq!(fixable["discrimination"], "discriminating");
    assert_eq!(fixable["falsification"]["event_id"], failing.as_str());
    let plain = gate_row(&repo, "both-states", "plain");
    assert_eq!(plain["discrimination"], "undiscriminated");
    assert!(plain["falsification"].is_null());

    let shown = stdout(repo.arc(&repo.root).args(["show", "both-states"]));
    assert!(shown.contains("(discriminating: failed at "));
    assert!(shown.contains(": marker absent)"));
    assert!(shown.contains("(undiscriminated)"));

    let explained = stdout(
        repo.arc(&repo.root)
            .args(["check", "both-states", "--explain"]),
    );
    assert!(explained.contains("gate `fixable`: (discriminating: failed at "));
    assert!(explained.contains("gate `plain`: (undiscriminated)"));
}

/// Evidence written before arc recorded falsification carries no such field.
/// It loads, and reads as undiscriminated rather than as a claim about what
/// the gate can detect.
#[test]
fn evidence_without_the_field_loads_as_undiscriminated() {
    let repo = Repo::new();
    let (change_id, failing) = change_with_a_fixed_gate(&repo, "legacy");
    repo.arc(&repo.root)
        .args([
            "verify",
            "legacy",
            "--gate",
            "fixable",
            "--falsified-by",
            &failing,
            "--predicted",
            "marker absent",
        ])
        .assert()
        .success();
    rewrite_event(&repo, &change_id, "verification-recorded", |event| {
        event.as_object_mut().unwrap().remove("falsification");
    });

    let fixable = gate_row(&repo, "legacy", "fixable");
    assert_eq!(fixable["result"], "pass");
    assert_eq!(fixable["discrimination"], "undiscriminated");
    assert!(fixable["falsification"].is_null());
}

/// Discrimination is advisory. Recording one changes what the gate row says
/// about the evidence and nothing about whether the change may integrate, so
/// the readiness codes are taken across the same change at the same head with
/// discrimination as the only thing that moved.
#[test]
fn readiness_codes_are_unaffected_by_discrimination() {
    let repo = Repo::new();
    let (_, failing) = change_with_a_fixed_gate(&repo, "advisory");
    for gate in ["fixable", "plain"] {
        repo.arc(&repo.root)
            .args(["verify", "advisory", "--gate", gate])
            .assert()
            .success();
    }
    let before = gate_row(&repo, "advisory", "fixable");
    assert_eq!(before["discrimination"], "undiscriminated");
    let check_before = repo
        .arc(&repo.root)
        .args(["check", "advisory"])
        .assert()
        .get_output()
        .status
        .code();
    let done_before = repo
        .arc(&repo.root)
        .args(["done", "advisory"])
        .assert()
        .get_output()
        .status
        .code();

    repo.arc(&repo.root)
        .args([
            "verify",
            "advisory",
            "--gate",
            "fixable",
            "--falsified-by",
            &failing,
            "--predicted",
            "marker absent",
        ])
        .assert()
        .success();

    // The variable actually moved, so the codes below are evidence rather than
    // two readings of the same state.
    let after = gate_row(&repo, "advisory", "fixable");
    assert_eq!(after["discrimination"], "discriminating");
    assert_eq!(
        repo.arc(&repo.root)
            .args(["check", "advisory"])
            .assert()
            .get_output()
            .status
            .code(),
        check_before
    );
    assert_eq!(
        repo.arc(&repo.root)
            .args(["done", "advisory"])
            .assert()
            .get_output()
            .status
            .code(),
        done_before
    );
}

/// Falsification is a fact about the gate at a revision, not about one run.
/// `verify --all` — and so every `arc done` — appends fresh passing evidence
/// with no reference at the same head; the gate must still read as having been
/// shown able to fail, with the two ids saying which run proved what.
#[test]
fn a_rerun_at_the_same_head_does_not_retract_discrimination() {
    let repo = Repo::new();
    let (_, failing) = change_with_a_fixed_gate(&repo, "rerun");
    repo.arc(&repo.root)
        .args([
            "verify",
            "rerun",
            "--gate",
            "fixable",
            "--falsified-by",
            &failing,
            "--predicted",
            "marker absent",
        ])
        .assert()
        .success();
    let discriminating_evidence =
        last_gate_verification_event_id(&repo, "rerun", "fixable", "pass");

    repo.arc(&repo.root)
        .args(["verify", "rerun", "--all"])
        .assert()
        .success();
    let counted = last_gate_verification_event_id(&repo, "rerun", "fixable", "pass");
    assert_ne!(
        counted, discriminating_evidence,
        "the rerun must append newer evidence, or this proves nothing"
    );

    let fixable = gate_row(&repo, "rerun", "fixable");
    assert_eq!(fixable["discrimination"], "discriminating");
    assert_eq!(fixable["evidence_event_id"], counted.as_str());
    assert_eq!(
        fixable["discrimination_event_id"],
        discriminating_evidence.as_str()
    );
    assert_eq!(fixable["falsification"]["event_id"], failing.as_str());

    // A gate that never carried a reference is untouched by the same rerun.
    let plain = gate_row(&repo, "rerun", "plain");
    assert_eq!(plain["discrimination"], "undiscriminated");
    assert!(plain["discrimination_event_id"].is_null());
}

#[test]
fn evidence_recorded_before_arc_kept_the_tree_still_counts_at_its_own_head() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "test: add unit gate"]);
    let begun = stdout(repo.arc(&repo.root).args(["begin", "legacy-evidence"]));
    let change_id = begun
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let worktree = repo.home.join(".worktrees/repo-legacy-evidence");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    let recorded = stdout(
        repo.arc(&worktree)
            .args(["verify", "legacy-evidence", "--all"]),
    );
    let event_id = recorded
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();

    // A ledger written before arc recorded the tree beside the revision. The
    // revision it names is what resolves the tree back, so the evidence keeps
    // meaning what it meant.
    let path = repo
        .root
        .join(".git/arc/changes")
        .join(&change_id)
        .join("events")
        .join(format!("{event_id}.json"));
    let mut event: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(event["tree"].is_string(), "{event}");
    event.as_object_mut().unwrap().remove("tree");
    fs::write(&path, json_file_bytes(&event)).unwrap();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "legacy-evidence"]));
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
    assert!(status["gates"][0]["evaluated_tree"].is_null(), "{status}");
    repo.arc(&worktree)
        .args(["snapshot", "legacy-evidence"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "legacy-evidence", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "legacy-evidence"])
        .assert()
        .success();
}

/// A change behind its target, with the unit gate evaluated on the merge.
///
/// Returns the worktree, the tree the gate answered for, and the revision the
/// evidence was recorded at.
fn evaluated_merge(repo: &Repo, slug: &str) -> (PathBuf, String, String) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "test: add unit gate"]);
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let worktree = repo.home.join(format!(".worktrees/repo-{slug}"));
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");

    // The target moves without touching what the change touches, so the merge
    // ships content neither branch committed and the gate must answer for it.
    repo.commit(
        &repo.root,
        "sibling.txt",
        "sibling\n",
        "test: unrelated sibling",
    );
    repo.arc(&worktree)
        .args(["verify", slug, "--against", "master"])
        .assert()
        .success();

    let evaluated = json_stdout(repo.arc(&repo.root).args(["status", slug]));
    let merged_tree = evaluated["merged_tree"].as_str().unwrap().to_string();
    let revision = evaluated["gates"][0]["revision"]
        .as_str()
        .unwrap()
        .to_string();
    (worktree, merged_tree, revision)
}

#[test]
fn a_rebase_onto_the_evaluated_tree_inherits_the_gate_evidence() {
    let repo = Repo::new();
    let (worktree, merged_tree, evidence_revision) = evaluated_merge(&repo, "rebased");

    // The rebase moves the base and nothing else, so the new head carries the
    // very content the gate ran against.
    git(&worktree, &["rebase", "master"]);
    let head = repo.head(&worktree);
    assert_eq!(
        git_out(&worktree, &["rev-parse", "HEAD^{tree}"]),
        merged_tree
    );
    assert_ne!(head, evidence_revision);
    repo.arc(&worktree)
        .args(["snapshot", "rebased"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "rebased"]));
    let gate = &status["gates"][0];
    assert_eq!(gate["result"], "pass", "{status}");
    assert_eq!(gate["green_at_head"], true, "{status}");
    // The head is on the target tip, so what ships is the head's own content
    // and there is no separate merged tree to name.
    assert!(gate["evaluated_tree"].is_null(), "{status}");
    assert_eq!(
        gate["inherited_from"],
        evidence_revision.as_str(),
        "{status}"
    );
    assert!(
        !status["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["blocker"] == "gates-not-green"),
        "{status}"
    );

    repo.arc(&repo.root)
        .args(["check", "rebased", "--explain"])
        .assert()
        .stdout(predicates::str::contains(format!(
            "inherited from {}",
            &evidence_revision[..8]
        )));
}

#[test]
fn a_rebase_that_changes_what_ships_inherits_nothing() {
    let repo = Repo::new();
    let (worktree, merged_tree, _) = evaluated_merge(&repo, "restacked");

    // The target moves again before the rebase, so the head the rebase
    // produces holds content no run has answered for.
    repo.commit(
        &repo.root,
        "sibling.txt",
        "revised\n",
        "test: sibling moves",
    );
    git(&worktree, &["rebase", "master"]);
    assert_ne!(
        git_out(&worktree, &["rev-parse", "HEAD^{tree}"]),
        merged_tree
    );
    repo.arc(&worktree)
        .args(["snapshot", "restacked"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "restacked"]));
    let gate = &status["gates"][0];
    assert_eq!(gate["result"], "pending", "{status}");
    assert_eq!(gate["green_at_head"], false, "{status}");
    assert!(gate["inherited_from"].is_null(), "{status}");
}

/// A gate command that fails unless it can see the change's own commit, so
/// the evidence proves which tree was read rather than only which revision
/// was recorded.
fn commit_gate_reading(repo: &Repo, file: &str) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!("[gates.alpha]\ncommand = \"test -f {file}\"\n"),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gates"]);
}

#[test]
fn snapshot_verify_from_another_checkout_gates_the_changes_worktree() {
    let repo = Repo::new();
    commit_gate_reading(&repo, "anchor-run.txt");
    let (_, worktree, head) = change_with_patchset(&repo, "anchor-run");

    let out = repo
        .arc(&repo.root)
        .args(["snapshot", "anchor-run", "--verify"])
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{printed}");
    assert!(
        printed.contains(&format!("running in {}", worktree.display())),
        "the run names the checkout it happened in: {printed}"
    );
    assert!(printed.contains("gates: 1/1 pass"), "{printed}");

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "anchor-run"]))).unwrap();
    assert_eq!(status["gates"][0]["name"], "alpha", "{status}");
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "anchor-run",
        "--type",
        "verification-recorded",
    ]));
    let recorded: serde_json::Value =
        serde_json::from_str(events.lines().next().expect("one gate ran")).unwrap();
    assert_eq!(recorded["revision"], head, "{recorded}");
}

#[test]
fn verifying_from_another_checkout_refuses_when_the_worktree_is_gone() {
    let repo = Repo::new();
    commit_gate_reading(&repo, "gone.txt");
    let (change_id, worktree, _) = change_with_patchset(&repo, "gone");
    git(
        &repo.root,
        &[
            "worktree",
            "remove",
            "--force",
            &worktree.display().to_string(),
        ],
    );

    repo.arc(&repo.root)
        .args(["verify", "gone", "--gate", "alpha"])
        .assert()
        .failure()
        .stderr(
            predicates::str::contains(worktree.display().to_string()).and(
                predicates::str::contains(format!(
                    "git worktree add {} arc/gone",
                    worktree.display()
                )),
            ),
        );
    assert!(change_id.starts_with("gone"), "{change_id}");
}
