use crate::common::*;
use serde_json::{json, Value};

fn declare(repo: &Repo, budget: u64) {
    repo.declare_gates_locally(&format!(
        "[gates.unit]\ncommand='true'\nbudget='{budget}ms'\n"
    ));
}
pub(super) fn readings(instructions: Option<u64>, other: u64) -> String {
    json!({"instructions": instructions, "host_busy_ms": other + 100,
        "host_capacity_ms": 10000, "cpu_user_ms": 90, "cpu_system_ms": 10,
        "prior": {"busy_ms":2600,"capacity_ms":10000,"arc_cpu_ms":100,"elapsed_ms":1000},
        "host": {"logical_cpus":10,"cpu_model":"fixture cpu","unrestricted":true},
        "rule": {"version":2,"instruction_ceiling_basis_points":10300,"minimum_instruction_rate_basis_points":1000,"minimum_other_basis_points":2500,"wall_slack_basis_points":12500,"minimum_prior_window_ms":100,"clock_coverage_slack_basis_points":500}})
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
/// its newest eligible reference. Rejects clock-only refusals and historical maximum selection.
#[test]
fn busy_host_acceptance_uses_the_newest_fitting_reference_and_is_visible() {
    let (repo, id, wt) = fixture("busy-visible");
    run(&repo, &wt, 800, Some(900), 1000, 0);
    let reference = latest(&repo, &wt, &id);
    let text = run(&repo, &wt, 1100, Some(927), 2500, 0);
    assert!(
        text.contains("accepted over budget: work 927 instructions"),
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
    let (source, source_id, first) = fixture("source-work-reference");
    source
        .arc(&first)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    source
        .arc(&source.root)
        .args(["integrate", &source_id])
        .assert()
        .success();
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
/// after a concurrent reduction has retired it; parallel probes receive no excuse.
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
            if parallel { "over-budget" } else { "excused" }
        );
    }
}

/// I5: load first produced during a gate cannot excuse that gate's overrun.
/// Rejects calling self-created load unrelated contention without a prior window.
#[test]
fn self_load_without_prior_host_load_is_refused() {
    let (repo, _, wt) = fixture("self-load");
    let mut facts: Value = serde_json::from_str(&readings(Some(1000), 5000)).unwrap();
    facts["prior"]["busy_ms"] = json!(100);
    controlled_run(&repo, &wt, 1100, facts, 1);
}

/// I5: a disabled or dropped-out counter is absent evidence, never less work.
/// Rejects accepting zero or implausibly few instructions against measured CPU.
#[test]
fn implausibly_low_instruction_counts_cannot_excuse() {
    let (repo, _, wt) = fixture("counter-dropped");
    for instructions in [0, 1, 99] {
        run(&repo, &wt, 1100, Some(instructions), 5000, 1);
    }
}

fn controlled_run(repo: &Repo, wt: &Path, ms: u64, facts: Value, code: i32) {
    repo.arc(wt)
        .env("ARC_TEST_COMMAND_RUN_MS", ms.to_string())
        .env("ARC_TEST_GATE_WORK", facts.to_string())
        .args(["verify", "--all", "--rerun"])
        .assert()
        .code(code);
}

/// I5: prior load excludes arc's CPU and needs 100 ms of observation.
/// Rejects attributing earlier gates or an unmeasurable startup window to outsiders.
#[test]
fn prior_load_excludes_arc_and_requires_a_measurable_window() {
    let (repo, _, wt) = fixture("prior-load");
    for (elapsed, arc_cpu) in [(99, 100), (1000, 101)] {
        let mut facts: Value = serde_json::from_str(&readings(Some(1000), 5000)).unwrap();
        facts["prior"]["elapsed_ms"] = json!(elapsed);
        facts["prior"]["arc_cpu_ms"] = json!(arc_cpu);
        controlled_run(&repo, &wt, 1100, facts, 1);
    }
}

/// I5: an excuse cannot exceed the delay explained by contention plus a quarter,
/// counted from the capacity the reference itself had left. Rejects arbitrary
/// waiting under a qualifying load, including one ms past the cap, and an
/// allowance computed as if a reference from a busy host had run on a quiet one.
#[test]
fn the_clock_excuse_is_bounded_by_measured_load() {
    let (repo, _, wt) = fixture("bounded-clock");
    // The fixture's reference ran 900 ms with a tenth of the host taken.
    run(&repo, &wt, 1350, Some(1000), 2500, 0);
    run(&repo, &wt, 1351, Some(1000), 2500, 1);
    // A newer reference from a quiet host explains more delay for the same clock.
    run(&repo, &wt, 900, Some(1000), 0, 0);
    run(&repo, &wt, 1500, Some(1000), 2500, 0);
    run(&repo, &wt, 1501, Some(1000), 2500, 1);
}

/// I5: CPU topology and model belong to host identity; restricted capacity refuses.
/// Rejects shared-hostname calibration after resizing, changing CPU, or CPU pinning.
#[test]
fn host_identity_and_unrestricted_capacity_are_required() {
    let (repo, _, wt) = fixture("host-capacity");
    for (key, value) in [
        ("logical_cpus", json!(4)),
        ("cpu_model", json!("different cpu")),
        ("unrestricted", json!(false)),
    ] {
        let mut facts: Value = serde_json::from_str(&readings(Some(1000), 5000)).unwrap();
        facts["host"][key] = value;
        controlled_run(&repo, &wt, 1100, facts, 1);
    }
}

/// I5: only clean, unmoved, non-probe source trees on the destination's lineage calibrate work.
/// Rejects a throwaway, dirty or parallel pass inflating an unrelated change's reference.
#[test]
fn ineligible_trees_cannot_supply_work_references() {
    for fault in ["dirty", "moved", "probe", "unrelated"] {
        let (repo, id, wt) = fixture("reference-tree");
        if fault == "unrelated" {
            let (_, other, _) = change_with_patchset(&repo, "unrelated-work");
            run(&repo, &other, 1100, Some(1000), 5000, 1);
        } else {
            rewrite_event(&repo, &id, "verification-recorded", |v| {
                if fault == "dirty" {
                    v["worktree_dirty"] = json!(true);
                    v["work_tree"]["clean"] = json!(false);
                }
                if fault == "moved" {
                    v["tree_moved"] = json!(true);
                    v["work_tree"]["unmoved"] = json!(false);
                }
                if fault == "probe" {
                    v["work_tree"]["parallel_probe"] = json!(true);
                }
            });
            run(&repo, &wt, 1100, Some(1000), 5000, 1);
        }
    }
}

/// I2: valid archived facts survive changes in reader defaults.
/// Rejects replay replacing the recorded rule and making historical acceptance unreadable.
#[test]
fn replay_uses_the_receipts_rule_thresholds() {
    let (repo, id, wt) = fixture("recorded-rule");
    run(&repo, &wt, 1100, Some(1031), 5000, 1);
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        if v["run_ms"] == 1100 {
            v["instructions"] = json!(1030);
            v["budget_at_run"]["verdict"] = json!("excused");
            v["budget_at_run"]["excuse"] = json!({
                "reference_event_id":v["work_reference"]["execution"]["event_id"],
                "reference_instructions":1000,"instructions":1030,"other_cpu_basis_points":5000});
        }
    });
    assert_eq!(verdict(&repo, &wt, &id), "excused");
}

/// I5: a parallel probe's sibling work cannot authorize a clock overrun.
/// Rejects accepting an unattributed batch even when both measured windows are busy.
#[test]
fn parallel_sibling_load_is_not_other_process_load() {
    let (repo, id, wt) = fixture("sibling-work");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "1100")
        .env("ARC_TEST_GATE_WORK", readings(Some(1000), 5000))
        .args(["verify", "--all", "--parallel", "--rerun"])
        .assert()
        .code(1);
    assert_eq!(verdict(&repo, &wt, &id), "over-budget");
}

/// I2: an excuse stripped of its recorded rule is not a judgement a reader can
/// repeat. Rejects accepting a receipt whose rule and thresholds were removed.
#[test]
fn an_excuse_without_its_recorded_rule_is_refused() {
    let (repo, id, wt) = fixture("ruleless-excuse");
    run(&repo, &wt, 1100, Some(1000), 5000, 0);
    assert_eq!(verdict(&repo, &wt, &id), "excused");
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        if v["run_ms"] == 1100 {
            v.as_object_mut().unwrap().remove("work_conditions");
        }
    });
    let out = repo
        .arc(&wt)
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("no rule version"), "{text}");
}

/// I5: a cached publication pass retains its execution facts without becoming
/// a fresh work reference. Rejects selecting a copied receipt as the newest direct pass.
#[test]
fn cached_publication_passes_cannot_supply_fresh_work_references() {
    let repo = Repo::new();
    repo.declare_gates_locally(
        "[gates.unit]\ncommand='true'\nbudget='1s'\nevents=['publication']\n",
    );
    let publication = |ms: u64| {
        repo.arc(&repo.root)
            .env("ARC_TEST_COMMAND_RUN_MS", ms.to_string())
            .env("ARC_TEST_GATE_WORK", readings(Some(1000), 5000))
            .args(["publication", "--target", "master"])
            .assert()
            .success();
    };
    let last = || {
        let mut events: Vec<Value> = fs::read_dir(repo.root.join(".git/arc/repository/events"))
            .unwrap()
            .map(|e| serde_json::from_slice(&fs::read(e.unwrap().path()).unwrap()).unwrap())
            .filter(|e: &Value| e["event_type"] == "publication-verified")
            .collect();
        events.sort_by_key(|e| e["event_id"].as_str().unwrap().to_owned());
        events.pop().unwrap()
    };
    publication(900);
    let execution = last()["event_id"].clone();
    repo.arc(&repo.root)
        .args(["publication", "--target", "master", "--skip-green"])
        .assert()
        .success();
    let copy = last()["event_id"].clone();
    assert_ne!(execution, copy);
    publication(1100);
    assert_eq!(
        last()["gates"]["unit"]["work_reference"]["execution"]["event_id"],
        execution
    );
}

/// I12: a completed run waits for final tree CPU and counter accounting.
/// Rejects a stopped acknowledgement letting the runner return with an incomplete receipt.
#[cfg(target_os = "linux")]
#[test]
fn a_run_waits_for_final_tree_accounting() {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixListener;
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.unit]\ncommand='sleep 30 >/dev/null 2>&1 &'\n");
    let (id, wt, _) = change_with_patchset(&repo, "final-accounting");
    let path = repo.home.join("final.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let child = repo.spawn_arc_piped_with_env(
        &wt,
        &["verify", "--all", "--rerun"],
        &[("ARC_TEST_HOLDER_FINAL_SOCKET", &path)],
    );
    // SAFETY: the child PID is reserved until wait; pidfd_open returns an owned descriptor.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0) } as i32;
    assert!(
        fd >= 0,
        "pidfd_open failed: {}",
        std::io::Error::last_os_error()
    );
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut readiness = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: poll borrows the two live descriptors.
    let ready = unsafe { libc::poll(readiness.as_mut_ptr(), readiness.len() as _, 30_000) };
    if ready <= 0 || readiness[0].revents & libc::POLLIN == 0 {
        drop(listener);
        // SAFETY: the unreaped fixture child owns this PID.
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        let output = child.wait_with_output().unwrap();
        panic!("runner exited before final tree accounting: {output:?}");
    }
    let (mut socket, _) = listener.accept().unwrap();
    socket.read_exact(&mut [0_u8]).unwrap();
    let mut event = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let exited = unsafe { libc::poll(&mut event, 1, 100) };
    socket.write_all(b"+").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(exited, 0, "runner returned before final tree accounting");
    assert!(output.status.success(), "{output:?}");
    let receipt = latest(&repo, &wt, &id);
    assert!(
        receipt["cpu_user_ms"].is_u64() && receipt["cpu_system_ms"].is_u64(),
        "final CPU facts missing: {receipt}"
    );
}

/// I2: a rule version fixes its thresholds, including on imported historical facts.
/// Rejects relabelling a four-percent allowance as the three-percent rule.
#[test]
fn a_rule_version_refuses_relabelled_thresholds() {
    let (repo, id, wt) = fixture("relabelled-rule");
    run(&repo, &wt, 1100, Some(1030), 5000, 0);
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        if v["run_ms"] == 1100 {
            v["work_conditions"]["rule"]["instruction_ceiling_basis_points"] = json!(10400);
        }
    });
    repo.arc(&wt)
        .args(["show", &id, "--json"])
        .assert()
        .failure();
}

/// I5: other-process share excludes arc's own process CPU as well as its trees.
/// Rejects passing the 25-percent threshold by charging arc's capture work to outsiders.
#[test]
fn other_process_share_excludes_the_invocation_cpu() {
    let (repo, _, wt) = fixture("own-process-cpu");
    let mut facts: Value = serde_json::from_str(&readings(Some(1000), 2500)).unwrap();
    facts["invocation_cpu_ms"] = json!(200);
    controlled_run(&repo, &wt, 1100, facts, 1);
}

/// I5: a gate leader pinned after exec cannot excuse its run using whole-host capacity.
/// Rejects measuring only arc's inherited affinity while the command uses fewer CPUs.
#[cfg(target_os = "linux")]
#[test]
fn a_gate_leaders_restricted_affinity_refuses_an_excuse() {
    let mut mask = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
    assert_eq!(
        unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&mask), &mut mask) },
        0
    );
    let cpus: Vec<_> = (0..libc::CPU_SETSIZE as usize)
        .filter(|cpu| unsafe { libc::CPU_ISSET(*cpu, &mask) })
        .collect();
    if cpus.len() <= 1
        || cpus.len() != unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) } as usize
    {
        return;
    }
    let repo = Repo::new();
    let command = r#"exec python3 -c 'import os; os.sched_setaffinity(0,{int(c) for c in os.environ["W1_CPUS"].split(",")})'"#;
    repo.declare_gates_locally(&format!(
        "[gates.unit]\ncommand={}\nbudget='1s'\n",
        serde_json::to_string(command).unwrap()
    ));
    let (id, wt, _) = change_with_patchset(&repo, "pinned-command");
    let mut facts: Value = serde_json::from_str(&readings(Some(1000), 5000)).unwrap();
    facts.as_object_mut().unwrap().remove("host");
    for (allowed, ms, code) in [
        (
            cpus.iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(","),
            900,
            0,
        ),
        (cpus[0].to_string(), 1100, 1),
    ] {
        repo.arc(&wt)
            .env("W1_CPUS", allowed)
            .env("ARC_TEST_COMMAND_RUN_MS", ms.to_string())
            .env("ARC_TEST_GATE_WORK", facts.to_string())
            .args(["verify", "--all", "--rerun"])
            .assert()
            .code(code);
    }
    assert_eq!(
        latest(&repo, &wt, &id)["work_identity"]["host"]["unrestricted"],
        false
    );
}

/// I12: escaped descendants retain their lifetime and leave incomplete work absent.
/// Rejects killing escaped work or reporting a complete count before its tree exits.
#[cfg(target_os = "linux")]
#[test]
fn escaped_descendants_leave_work_absent_until_their_tree_exits() {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixListener;
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    repo.commit(
        &repo.root,
        ".arc/escape.py",
        r#"import os,socket,time
r,w=os.pipe()
if os.fork()==0:
 os.setsid()
 s=socket.socket(socket.AF_UNIX);s.connect(os.environ['W1_ESCAPE_SOCKET'])
 s.sendall((str(os.getpid())+'\n').encode());s.recv(1);s.close()
 os.close(1);os.close(2);os.write(w,b'+');os.close(w)
 time.sleep(30)
 os._exit(0)
os.close(w);os.read(r,1);os._exit(0)
"#,
        "test: add escaped child fixture",
    );
    repo.declare_gates_locally("[gates.unit]\ncommand='exec python3 .arc/escape.py'\n");
    let (id, wt, _) = change_with_patchset(&repo, "escaped-accounting");
    let path = repo.home.join("escaped.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let child = repo.spawn_arc_piped_with_env(
        &wt,
        &["verify", "--all", "--rerun"],
        &[("W1_ESCAPE_SOCKET", &path)],
    );
    let (mut socket, _) = listener.accept().unwrap();
    let mut text = String::new();
    BufReader::new(&mut socket).read_line(&mut text).unwrap();
    let pid: i32 = text.trim().parse().unwrap();
    // SAFETY: this descendant is blocked on the fixture socket while its PID is captured.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
    assert!(fd >= 0);
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    socket.write_all(b"+").unwrap();
    let mut event = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let output = child.wait_with_output().unwrap();
    let exited = unsafe { libc::poll(&mut event, 1, 0) };
    // SAFETY: the pidfd identifies only the fixture descendant, even after PID reuse.
    unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            libc::SIGKILL,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        );
        libc::poll(&mut event, 1, 5000);
    }
    assert_eq!(
        exited, 0,
        "escaped descendant killed by cleanup: {output:?}"
    );
    assert!(output.status.success(), "{output:?}");
    let raw = latest(&repo, &wt, &id);
    for field in [
        "instructions",
        "cpu_user_ms",
        "cpu_system_ms",
        "tree_cpu_ms",
    ] {
        assert!(raw[field].is_null(), "incomplete tree work recorded: {raw}");
    }
}

/// I5: an exec that drops inherited counters cannot establish a work reference.
/// Rejects treating startup-only instructions as the full work of a CPU-bound command.
#[cfg(target_os = "linux")]
#[test]
fn an_execute_only_command_cannot_seed_a_dropped_counter_reference() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    let opaque = repo.home.join("opaque-python");
    fs::copy("/usr/bin/python3", &opaque).unwrap();
    fs::set_permissions(&opaque, fs::Permissions::from_mode(0o111)).unwrap();
    let command = r#"exec "$W1_PYTHON" -c 'sum(range(4000000))'"#;
    repo.declare_gates_locally(&format!(
        "[gates.unit]\ncommand={}\nbudget='10s'\n",
        serde_json::to_string(command).unwrap()
    ));
    let (id, wt, _) = change_with_patchset(&repo, "exec-counter-drop");
    repo.arc(&wt)
        .env("W1_PYTHON", "/usr/bin/python3")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
    let ordinary = latest(&repo, &wt, &id);
    if ordinary["instructions"].is_null() || ordinary["tree_cpu_ms"].is_null() {
        return;
    }
    repo.arc(&wt)
        .env("W1_PYTHON", &opaque)
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
    let dropped = latest(&repo, &wt, &id);
    assert!(
        dropped["instructions"].is_null(),
        "detached counter treated as work: {dropped}"
    );
}

/// I5: a run whose tree clock covers less of its waited CPU than its
/// reference's did holds work the counters could not follow. Rejects excusing
/// it on an unchanged instruction count, and refusing a run inside the slack.
#[test]
fn a_clock_that_covers_less_than_the_reference_cannot_excuse() {
    // The fixture's reference clock covers all 100 ms of its waited CPU.
    let (repo, _, wt) = fixture("clock-coverage");
    for (clock, code) in [(95, 0), (94, 1)] {
        let mut facts: Value = serde_json::from_str(&readings(Some(1000), 5000)).unwrap();
        facts["tree_cpu_ms"] = json!(clock);
        controlled_run(&repo, &wt, 1100, facts, code);
    }
}

/// I2: a receipt judged under rule 1 names thresholds without a coverage
/// condition and stays readable and judged by them. Rejects a reader that
/// requires the newer threshold of every recorded rule, and one that applies
/// the newer condition to an older receipt.
#[test]
fn a_rule_one_receipt_keeps_its_thresholds() {
    let (repo, id, wt) = fixture("rule-one");
    let mut facts: Value = serde_json::from_str(&readings(Some(1000), 5000)).unwrap();
    facts["rule"] = json!({"version":1,"instruction_ceiling_basis_points":10300,
        "minimum_instruction_rate_basis_points":1000,"minimum_other_basis_points":2500,
        "wall_slack_basis_points":12500,"minimum_prior_window_ms":100});
    facts["tree_cpu_ms"] = json!(94);
    controlled_run(&repo, &wt, 1100, facts, 0);
    assert_eq!(verdict(&repo, &wt, &id), "excused");
}
