use crate::common::*;
use serde_json::{json, Value};

fn declare(repo: &Repo, budget: u64) {
    repo.declare_gates_locally(&format!(
        "[gates.unit]\ncommand='true'\nbudget='{budget}ms'\n"
    ));
}
fn readings(instructions: Option<u64>, other: u64) -> String {
    json!({"instructions": instructions, "host_busy_ms": other + 100,
        "host_capacity_ms": 10000, "cpu_user_ms": 90, "cpu_system_ms": 10})
    .to_string()
}
fn run(
    repo: &Repo,
    wt: &Path,
    ms: u64,
    instructions: Option<u64>,
    other: u64,
    code: i32,
) -> String {
    let out = repo
        .arc(wt)
        .env("ARC_TEST_COMMAND_RUN_MS", ms.to_string())
        .env("ARC_TEST_GATE_WORK", readings(instructions, other))
        .args(["verify", "--all", "--rerun"])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(code), "{text}");
    text
}
fn shown(repo: &Repo, wt: &Path, id: &str) -> Value {
    json_stdout(repo.arc(wt).args(["show", id, "--json"]))
}
fn latest(repo: &Repo, wt: &Path, id: &str) -> Value {
    shown(repo, wt, id)["verifications"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone()
}
fn fixture(slug: &str) -> (Repo, String, PathBuf) {
    let repo = Repo::new();
    declare(&repo, 1000);
    let (id, wt, _) = change_with_patchset(&repo, slug);
    run(&repo, &wt, 900, Some(1000), 1000, 0);
    (repo, id, wt)
}
fn verdict(repo: &Repo, wt: &Path, id: &str) -> Value {
    shown(repo, wt, id)["gate_status"][0]["budget"]["verdict"].clone()
}

/// I5: callers accept an over-clock pass at both inclusive thresholds and see
/// its largest eligible reference. Rejects clock-only refusals and newest-only selection.
#[test]
fn busy_host_acceptance_uses_the_largest_fitting_reference_and_is_visible() {
    let (repo, id, wt) = fixture("busy-visible");
    let reference = latest(&repo, &wt, &id);
    run(&repo, &wt, 800, Some(900), 1000, 0);
    let text = run(&repo, &wt, 1100, Some(1030), 2500, 0);
    assert!(
        text.contains("accepted over budget: work 1030 instructions"),
        "{text}"
    );
    assert!(text.contains("other processes 25.00%"), "{text}");
    let raw = latest(&repo, &wt, &id);
    assert_eq!(
        raw["work_reference"]["execution"]["event_id"],
        reference["event_id"]
    );
    assert_eq!(verdict(&repo, &wt, &id), "excused");
    for command in ["show", "explain"] {
        assert!(stdout(repo.arc(&wt).args([command, &id])).contains("accepted over budget"));
    }
    let status = json_stdout(repo.arc(&wt).args(["status", &id]));
    assert_eq!(status["gates"][0]["green_at_head"], true);
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", &id]).assert().success();
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(report["gates"][0]["excused_runs"], 1);
    assert_eq!(
        report["gates"][0]["last_excused"]["event_id"],
        raw["event_id"]
    );
    let config: Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 12);
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
}

/// I5: a quiet host cannot excuse an over-clock pass. Rejects instruction-only acceptance.
#[test]
fn a_quiet_host_refuses_even_matching_work() {
    let (repo, id, wt) = fixture("quiet-work");
    run(&repo, &wt, 1100, Some(1000), 2499, 1);
    assert_eq!(verdict(&repo, &wt, &id), "over-budget");
}

/// I5: work above 103 percent refuses on a busy host. Rejects a load-only allowance.
#[test]
fn more_work_refuses_even_on_a_busy_host() {
    let (repo, id, wt) = fixture("more-work");
    run(&repo, &wt, 1100, Some(1031), 5000, 1);
    assert_eq!(verdict(&repo, &wt, &id), "over-budget");
}

/// I5: absent counters or a missing compatible earlier pass cannot loosen a budget.
/// Rejects absence-as-zero and comparing different commands or hosts.
#[test]
fn absent_or_incompatible_references_never_excuse() {
    let repo = Repo::new();
    declare(&repo, 1000);
    let (id, wt, _) = change_with_patchset(&repo, "no-reference");
    run(&repo, &wt, 1100, Some(1000), 5000, 1);
    run(&repo, &wt, 900, Some(1000), 1000, 0);
    run(&repo, &wt, 1100, None, 5000, 1);
    // Change every potential earlier reference's host, preserving its structure.
    let dir = event_dir(&repo, &id);
    for path in fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()) {
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if value["event_type"] == "verification-recorded" && value["run_ms"] == 900 {
            value["hostname"] = json!("another-host");
            value["work_identity"]["hostname"] = json!("another-host");
            fs::write(path, json_file_bytes(&value)).unwrap();
        }
    }
    run(&repo, &wt, 1100, Some(1000), 5000, 1);
    repo.declare_gates_locally("[gates.unit]\ncommand='true; :'\nbudget='1000ms'\n");
    run(&repo, &wt, 1100, Some(1000), 5000, 1);
}

/// I5: lowering the live budget retires an excuse whose reference no longer fits.
/// Rejects recorded acceptance authorizing check, reuse or integration after tightening.
#[test]
fn lowering_the_budget_retires_the_reference_without_rewriting_history() {
    let (repo, id, wt) = fixture("lower-work");
    run(&repo, &wt, 1100, Some(1000), 5000, 0);
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    declare(&repo, 899);
    assert_eq!(verdict(&repo, &wt, &id), "over-budget");
    assert_eq!(
        latest(&repo, &wt, &id)["budget_at_run"]["verdict"],
        "excused"
    );
    repo.arc(&wt).args(["check", &id]).assert().code(5);
    repo.arc(&wt).args(["verify", "--all"]).assert().code(1);
    let head = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .code(5);
    assert_eq!(repo.head(&repo.root), head);
}

/// I5: an excused run never raises the work reference, even after its wall time
/// fits a larger budget. Rejects cumulative three-percent growth through excuses.
#[test]
fn an_excused_run_cannot_become_a_reference_after_a_budget_increase() {
    let (repo, id, wt) = fixture("no-ratcheting");
    run(&repo, &wt, 1100, Some(1030), 5000, 0);
    declare(&repo, 1100);
    run(&repo, &wt, 1200, Some(1040), 5000, 1);
    assert_eq!(
        latest(&repo, &wt, &id)["work_reference"]["instructions"],
        1000
    );
}

/// I5: a busy-host excuse cannot justify a budget raise. Rejects accepting an
/// excused direct execution merely because it is inside the proposed budget.
#[test]
fn an_excused_run_cannot_justify_a_budget_raise() {
    let repo = Repo::new();
    repo.redeclare_gates("[budget]\ntotal='20s'\n[selection]\ninert=['.arc/']\n[gates.unit]\ncommand='true'\nbudget='10s'\npaths=['src/']\n");
    let (_, wt, _) = change_with_patchset(&repo, "no-excused-raise");
    fs::create_dir_all(wt.join("src")).unwrap();
    repo.commit(
        &wt,
        "src/code.rs",
        &(0..1000).map(|i| format!("line_{i}\n")).collect::<String>(),
        "test: add source",
    );
    repo.commit(&wt, ".arc/gates.toml", "[budget]\ntotal='20s'\n[selection]\ninert=['.arc/']\n[gates.unit]\ncommand='true'\nbudget='11s'\npaths=['src/']\n", "test: declare raise");
    repo.arc(&wt).args(["snapshot"]).assert().success();
    run(&repo, &wt, 9000, Some(1000), 1000, 0);
    run(&repo, &wt, 10500, Some(1000), 5000, 0);
    repo.arc(&wt)
        .args([
            "gates",
            "raise",
            "unit",
            "--reason",
            "Measured coverage cost",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "newest direct execution must be clean",
        ));
}

/// I5: integration may accept the excuse but cannot retain it as a base-pass seed.
/// Rejects another change inheriting an excused execution through a baseline.
#[test]
fn an_excused_run_cannot_seed_a_base_pass() {
    let repo = Repo::new();
    repo.redeclare_gates("[selection]\nbase_passes=true\n[gates.unit]\ncommand='true'\nbudget='1s'\npaths=['src/']\n");
    let (id, wt, _) = change_with_patchset(&repo, "no-excused-base");
    run(&repo, &wt, 900, Some(1000), 1000, 0);
    run(&repo, &wt, 1100, Some(1000), 5000, 0);
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    let files = fs::read_dir(repo.root.join(".git/arc/repository/events")).unwrap();
    for path in files.map(|e| e.unwrap().path()) {
        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        if value["event_type"] == "gate-baseline-recorded" {
            assert!(
                value["baseline"]["gates"]["unit"].is_null(),
                "excused run seeded a baseline: {value}"
            );
        }
    }
}

/// I5: imported excuses retain a self-contained earlier execution witness and are
/// rejudged by the receiver's budget. Rejects dependence on a missing source change.
#[test]
fn imported_excuses_preserve_the_reference_and_follow_receiver_budgets() {
    let (source, _, first) = fixture("source-work-reference");
    let (id, wt, _) = change_with_patchset(&source, "transport-work");
    run(&source, &wt, 1100, Some(1000), 5000, 0);
    assert_ne!(
        latest(&source, &wt, &id)["work_reference"]["execution"]["change_id"],
        id
    );
    let path = source.home.join("work-bundle.json");
    source
        .arc(&wt)
        .args(["export", &id, "--output", path.to_str().unwrap()])
        .assert()
        .success();
    let bundle: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    git(
        &receiver.root,
        &[
            "branch",
            bundle["events"][0]["branch"].as_str().unwrap(),
            &source.head(&wt),
        ],
    );
    receiver.declare_gates_locally(&format!("[gates.unit]\ncommand='true'\nbudget='1s'\n[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{}'\n[[replica.publishers.origins]]\nreplica_id='{}'\nroles=['facts']\nactors=[]\n", bundle["source_replica_id"].as_str().unwrap(), bundle["source_replica_id"].as_str().unwrap()));
    receiver
        .arc(&receiver.root)
        .args([
            "import",
            path.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &hex::encode(Sha256::digest(fs::read(&path).unwrap())),
        ])
        .assert()
        .success();
    assert_eq!(verdict(&receiver, &receiver.root, &id), "excused");
    let config: Value =
        serde_json::from_slice(&fs::read(receiver.root.join(".git/arc/config.json")).unwrap())
            .unwrap();
    assert_eq!(config["schema_version"], 12);
    fs::write(
        receiver.root.join(".git/arc/operator-policy.toml"),
        "[gates.unit]\ncommand='true'\nbudget='899ms'\n",
    )
    .unwrap();
    assert_eq!(verdict(&receiver, &receiver.root, &id), "over-budget");
    assert_eq!(
        latest(&source, &first, &id)["budget_at_run"]["verdict"],
        "excused"
    );
}

/// I2: replay refuses forged counts, later references and a failed source pass.
/// Rejects a portable work witness whose structure cannot support its claim.
#[test]
fn replay_refuses_forged_or_stale_work_witnesses() {
    let (repo, id, wt) = fixture("forged-work");
    run(&repo, &wt, 1100, Some(1000), 5000, 0);
    let original: Value = fs::read_dir(event_dir(&repo, &id))
        .unwrap()
        .map(|entry| {
            serde_json::from_slice::<Value>(&fs::read(entry.unwrap().path()).unwrap()).unwrap()
        })
        .find(|value| value["event_id"] == latest(&repo, &wt, &id)["event_id"])
        .unwrap();
    for fault in [
        "count",
        "later",
        "failed",
        "host",
        "excused",
        "judgement",
        "coordinates",
    ] {
        rewrite_event(&repo, &id, "verification-recorded", |v| {
            *v = original.clone();
            if fault == "count" {
                v["work_reference"]["instructions"] = json!(999999);
            } else if fault == "later" {
                v["work_reference"]["execution"]["event_id"] = v["event_id"].clone();
            } else if fault == "coordinates" {
                v["work_reference"]["execution"]["repository_id"] = json!("");
            } else if fault == "failed" {
                v["work_reference"]["execution"]["result"] = json!("fail");
            } else if fault == "host" {
                v["work_identity"]["hostname"] = json!("other-host");
            } else if fault == "judgement" {
                v["budget_at_run"]["verdict"] = json!("within-budget");
            } else {
                v["work_reference"] = Value::Null;
            }
            if fault == "later" || fault == "failed" || fault == "coordinates" {
                let bytes = serde_json::to_vec(&v["work_reference"]["execution"]).unwrap();
                v["work_reference"]["digest"] =
                    json!(format!("sha256:{}", hex::encode(Sha256::digest(bytes))));
            }
        });
        repo.arc(&wt)
            .args(["show", &id, "--json"])
            .assert()
            .failure();
    }
}

/// I5: fresh and cached publication use the same busy-host judgement as verify.
/// Rejects clock-only fresh refusal and frozen acceptance after a budget reduction.
#[test]
fn publication_excuses_are_rejudged_when_cached() {
    let repo = Repo::new();
    repo.declare_gates_locally(
        "[gates.unit]\ncommand='true'\nbudget='1s'\nevents=['publication']\n",
    );
    for ms in [900, 1100] {
        repo.arc(&repo.root)
            .env("ARC_TEST_COMMAND_RUN_MS", ms.to_string())
            .env("ARC_TEST_GATE_WORK", readings(Some(1000), 5000))
            .args(["publication", "--target", "master"])
            .assert()
            .success();
    }
    repo.declare_gates_locally(
        "[gates.unit]\ncommand='true'\nbudget='899ms'\nevents=['publication']\n",
    );
    repo.arc(&repo.root)
        .args(["publication", "--target", "master", "--skip-green"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("over budget"));
}

/// Measurements show the recorded instruction count and other-work share.
/// Rejects leaving controlled observations invisible in the measure output.
#[test]
fn measurements_print_work_and_other_process_share() {
    let repo = Repo::new();
    declare(&repo, 1000);
    repo.arc(&repo.root)
        .env("ARC_TEST_GATE_WORK", readings(Some(123456), 2500))
        .args(["gates", "measure", "--runs", "1"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "instructions 123456; other processes 25.00%",
        ));
}

/// A measured pass retains its work identity even without a declared budget.
/// Rejects losing calibration when a budget is adopted or changing legacy matching.
#[test]
fn an_unbudgeted_pass_can_supply_a_later_work_reference() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.unit]\ncommand='true'\n");
    let (id, wt, _) = change_with_patchset(&repo, "adopt-work-budget");
    run(&repo, &wt, 900, Some(1000), 1000, 0);
    let reference = latest(&repo, &wt, &id);
    assert!(reference["declaration"].is_null());
    declare(&repo, 1000);
    run(&repo, &wt, 1100, Some(1000), 5000, 0);
    assert_eq!(
        latest(&repo, &wt, &id)["work_reference"]["execution"]["event_id"],
        reference["event_id"]
    );
}

/// Verify labels its pass using the budget in force when it returns, while the
/// receipt retains its run-time budget. Rejects announcing an accepted excuse
/// after a concurrent reduction has retired it, in serial and parallel output.
#[test]
fn verify_does_not_announce_a_retired_excuse_as_accepted() {
    for parallel in [false, true] {
        let (repo, id, wt) = fixture("retired-excuse-output");
        let work = readings(Some(1000), 5000);
        let args = if parallel {
            vec!["verify", "--all", "--rerun", "--parallel"]
        } else {
            vec!["verify", "--all", "--rerun"]
        };
        let mut child = spawn_arc_at_rendezvous(
            &repo,
            &wt,
            &args,
            "gate-command-spawned",
            &[
                ("ARC_TEST_COMMAND_RUN_MS", "1100"),
                ("ARC_TEST_GATE_WORK", &work),
            ],
        );
        declare(&repo, 899);
        release_lock_wait(&mut child);
        let result = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&result.stdout);
        assert_eq!(result.status.code(), Some(1), "{text}");
        assert!(
            !text.contains("accepted over budget"),
            "retired excuse announced: {text}"
        );
        assert_eq!(
            latest(&repo, &wt, &id)["budget_at_run"]["verdict"],
            "excused"
        );
    }
}
