use crate::common::*;
use serde_json::{json, Value};

const SCRIPT: &str = r#"import os, pathlib, signal, sys, time
name = sys.argv[1]
home = pathlib.Path(os.environ['ARC_SANDBOX'])
signal.alarm(8)
mode = os.environ.get('BATCH_MODE', 'alone')
if name == 'z_alone':
    (home / 'alone-finished').touch()
elif mode == 'together' and name in ('a', 'b'):
    if os.environ.get('REQUIRE_ALONE'):
        assert (home / 'alone-finished').exists(), 'standalone gate must finish first'
    if name == 'a':
        with open(home / 'a-to-b', 'w') as out: out.write('a')
        with open(home / 'b-to-a') as inp: assert inp.read() == 'b'
    else:
        with open(home / 'a-to-b') as inp: assert inp.read() == 'a'
        with open(home / 'b-to-a', 'w') as out: out.write('b')
if mode == 'fail' and name == 'a': sys.exit(1)
if mode == 'timeout' and name == 'a': time.sleep(30)
if mode == 'move' and name == 'a': pathlib.Path('README.md').write_text('moved\n')
with open(home / 'executions', 'a') as out: out.write(name + '\n')
"#;

fn declaration(extra: &str, a: &str, b: &str) -> String {
    format!("{extra}\n[gates.a]\ncommand='python3 .arc/batch.py a'\nconcurrent=true\nbudget='{a}'\ntimeout='2s'\npaths=['a/']\n[gates.b]\ncommand='python3 .arc/batch.py b'\nconcurrent=true\nbudget='{b}'\ntimeout='2s'\npaths=['b/']\n")
}
fn fixture(extra: &str, a: &str, b: &str) -> (Repo, String, PathBuf) {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    repo.commit(&repo.root, ".arc/batch.py", SCRIPT, "test: gate rendezvous");
    repo.redeclare_gates(&declaration(extra, a, b));
    for name in ["a-to-b", "b-to-a"] {
        assert!(Command::new("mkfifo")
            .arg(repo.home.join(name))
            .status()
            .unwrap()
            .success());
    }
    let (id, wt, _) = change_with_patchset(&repo, "together");
    (repo, id, wt)
}
fn measure(repo: &Repo, wt: &Path) {
    repo.arc(wt)
        .env("BATCH_MODE", "together")
        .args(["gates", "measure", "--concurrent"])
        .assert()
        .success()
        .stdout(predicates::str::contains("concurrent batch accepted"));
}
fn show(repo: &Repo, wt: &Path) -> Value {
    json_stdout(repo.arc(wt).args(["show", "--json"]))
}
fn status(repo: &Repo, wt: &Path) -> Value {
    json_stdout(repo.arc(wt).arg("status"))
}
fn entries(state: &Value) -> Vec<&Value> {
    state["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["gate"] == "a" || entry["gate"] == "b")
        .collect()
}
fn acceptance_file(repo: &Repo) -> PathBuf {
    fs::read_dir(repo.root.join(".git/arc/concurrent"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}
fn approve(repo: &Repo, wt: &Path) {
    repo.arc(wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
}

/// Claim: a measured batch overlaps after standalone gates and supplies green,
/// compact per-command receipts in every view. Rejects embedded measurement
/// rounds or declarations, sequential scheduling, a batch preceding its
/// standalone obligations, or shared command-time accounting.
#[test]
fn measured_batch_runs_together_after_standalone_and_keeps_own_receipts() {
    let (repo, id, wt) = fixture(
        "[gates.z_alone]\ncommand='python3 .arc/batch.py z_alone'\nbudget='10s'",
        "10s",
        "10s",
    );
    repo.declare_gates_locally(
        &declaration("", "10s", "10s")
            .replace("paths=['a/']", "paths=['a/']\nexclusive='lock-a'")
            .replace("paths=['b/']", "paths=['b/']\nexclusive='lock-b'"),
    );
    let before = super::gate_reports::tree_bytes(&repo.root.join(".git/arc"));
    measure(&repo, &wt);
    let after = super::gate_reports::tree_bytes(&repo.root.join(".git/arc"));
    for (path, bytes) in before {
        if !path.ends_with("config.json") {
            assert_eq!(after.get(&path), Some(&bytes));
        }
    }
    let local: Value = serde_json::from_slice(&fs::read(acceptance_file(&repo)).unwrap()).unwrap();
    assert_eq!(local["rounds"].as_array().unwrap().len(), 5);
    assert!(local["members"]
        .as_object()
        .unwrap()
        .values()
        .all(Value::is_object));
    assert!(
        entries(&show(&repo, &wt)).is_empty(),
        "measurement must not record verification evidence"
    );
    fs::remove_file(repo.home.join("executions")).unwrap();
    repo.arc(&wt)
        .env("BATCH_MODE", "together")
        .env("REQUIRE_ALONE", "1")
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(repo.home.join("executions"))
            .unwrap()
            .lines()
            .next(),
        Some("z_alone")
    );
    let state = show(&repo, &wt);
    let evidence = entries(&state);
    assert_eq!(state["verification_runs"][0]["mode"], "declared");
    assert_eq!(evidence.len(), 2);
    assert_eq!(
        evidence[0]["concurrent_batch"]["batch_id"],
        evidence[1]["concurrent_batch"]["batch_id"]
    );
    for entry in evidence {
        assert_eq!(entry["worktree_dirty"], false);
        assert_eq!(
            entry["exclusive"],
            format!("lock-{}", entry["gate"].as_str().unwrap())
        );
        assert_eq!(entry["admission_ms"], 60000);
        for key in [
            "run_ms",
            "cpu_user_ms",
            "cpu_system_ms",
            "load_average",
            "wait_ms",
        ] {
            assert!(!entry[key].is_null(), "{key}: {entry}");
        }
        let acceptance = &entry["concurrent_batch"]["acceptance"];
        assert_eq!(acceptance.as_object().unwrap().len(), 4);
        assert!(acceptance["members"]
            .as_object()
            .unwrap()
            .values()
            .all(Value::is_string));
        assert!(acceptance["rounds"].is_null());
        assert!(acceptance["measured_at"].is_null());
    }
    assert!(status(&repo, &wt)["gates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|g| g["green_at_head"] == true));
    approve(&repo, &wt);
    for args in [vec!["check"], vec!["show"], vec!["explain", &id]] {
        let text = stdout(repo.arc(&wt).args(args.clone()));
        assert!(text.contains("concurrent batch"), "{args:?}: {text}");
    }
    let gates = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(gates["gates"][0]["declaration"]["concurrent"], true);
    assert!(gates["gates"][0]["concurrent_acceptance"]["measured_at"].is_string());
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    let closed = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
    let authorization = &closed["closure"]["authorization"]["gates"]["a"]["concurrent_acceptance"];
    assert_eq!(authorization.as_object().unwrap().len(), 4);
    assert!(authorization["members"]
        .as_object()
        .unwrap()
        .values()
        .all(Value::is_string));
    assert!(
        authorization.is_object(),
        "authorization must freeze acceptance: {closed}"
    );
}

/// Claim: no current measurement means standalone execution, and identity or
/// membership changes require remeasurement. Rejects trusting consent alone,
/// or letting a stale measurement authorize a changed set.
#[test]
fn unmeasured_or_changed_batches_run_alone_and_report_why() {
    let (repo, _, wt) = fixture("", "10s", "10s");
    repo.arc(&wt)
        .args(["verify", "--all"])
        .assert()
        .success()
        .stderr(predicates::str::contains("no current local measurement"));
    assert!(entries(&show(&repo, &wt))
        .iter()
        .all(|e| e["concurrent_batch"].is_null()));
    measure(&repo, &wt);
    let original = fs::read_to_string(wt.join(".arc/gates.toml")).unwrap();
    repo.commit(
        &wt,
        ".arc/gates.toml",
        &original.replace("timeout='2s'", "timeout='1s'"),
        "test: change identity",
    );
    repo.arc(&wt)
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success()
        .stderr(predicates::str::contains("no current local measurement"));
    repo.commit(
        &wt,
        ".arc/gates.toml",
        &format!("{original}\n[gates.c]\ncommand='true'\nconcurrent=true\nbudget='10s'\n"),
        "test: add member",
    );
    repo.arc(&wt)
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success()
        .stderr(predicates::str::contains("no current local measurement"));
    assert!(stdout(repo.arc(&wt).arg("gates")).contains("not accepted on this store and host"));
}

/// Claim: failed, dirty or moving measurement cannot establish acceptance and
/// a failed repeat revokes its prior claim. Rejects partial-round acceptance,
/// accepting modified content, or keeping a failed measurement's old authority.
#[test]
fn every_measurement_round_must_pass_on_unchanged_clean_content() {
    let (repo, _, wt) = fixture("", "10s", "10s");
    measure(&repo, &wt);
    repo.arc(&wt)
        .env("BATCH_MODE", "fail")
        .args(["gates", "measure", "--concurrent"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains(
            "every gate must pass all five rounds",
        ));
    assert!(!acceptance_file_optional(&repo));
    repo.arc(&wt)
        .env("BATCH_MODE", "move")
        .args(["gates", "measure", "--concurrent"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("worktree moved"));
    assert!(!acceptance_file_optional(&repo));
    repo.arc(&wt)
        .args(["gates", "measure", "--concurrent"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("clean worktree"));
    repo.arc(&wt)
        .args(["gates", "measure", "--concurrent", "--runs", "4"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("five rounds"));
}
fn acceptance_file_optional(repo: &Repo) -> bool {
    fs::read_dir(repo.root.join(".git/arc/concurrent"))
        .is_ok_and(|mut files| files.next().is_some())
}

/// Claim: clean batch provenance is binding even under a dirty waiver, and
/// movement invalidates every member. Rejects a waiver laundering batch dirt
/// or a member retaining green after a peer changes the shared tree.
#[test]
fn moved_or_dirty_batch_is_never_green() {
    let (repo, _, wt) = fixture("", "10s", "10s");
    measure(&repo, &wt);
    repo.arc(&wt)
        .env("BATCH_MODE", "move")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .code(1);
    assert!(status(&repo, &wt)["gates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|g| g["tree_moved"] == true && g["green_at_head"] == false));
    repo.arc(&wt)
        .args([
            "verify",
            "--all",
            "--rerun",
            "--waive-dirty",
            "fixture dirt",
        ])
        .assert()
        .code(1);
    assert!(status(&repo, &wt)["gates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|g| g["green_at_head"] == false));
}

/// Claim: each member's own command time and timeout judge its receipt.
/// Rejects batch-wall budget accounting, a peer's budget refusal spreading
/// to another member, or one timeout changing a peer's raw passing result.
#[test]
fn batch_budgets_and_timeouts_are_per_member() {
    let (repo, _, wt) = fixture("", "200ms", "50ms");
    measure(&repo, &wt);
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "100")
        .args(["verify", "--all"])
        .assert()
        .code(1);
    let s = status(&repo, &wt);
    assert_eq!(s["gates"][0]["green_at_head"], true);
    assert_eq!(s["gates"][1]["budget"]["verdict"], "over-budget");
    repo.arc(&wt)
        .env("BATCH_MODE", "timeout")
        .env("ARC_TEST_COMMAND_RUN_MS", "10")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .code(1);
    let state = show(&repo, &wt);
    let evidence = entries(&state);
    assert_eq!(evidence[evidence.len() - 2]["timed_out"], true);
    assert_eq!(evidence[evidence.len() - 1]["result"], "pass");
}

/// Claim: total discounts exactly a locally accepted profile batch and
/// authorization loses that discount when acceptance disappears. Rejects
/// consent-only totals or a removed measurement keeping old passes green.
#[test]
fn total_and_authorization_require_current_local_acceptance() {
    let (repo, _, wt) = fixture("[budget]\ntotal='15s'", "10s", "10s");
    assert!(!status(&repo, &wt)["budget_errors"]
        .as_array()
        .unwrap()
        .is_empty());
    measure(&repo, &wt);
    assert!(status(&repo, &wt)["budget_errors"]
        .as_array()
        .unwrap()
        .is_empty());
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(report["integration_budget_sum_ms"], 10000);
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    fs::remove_file(acceptance_file(&repo)).unwrap();
    let s = status(&repo, &wt);
    assert!(!s["budget_errors"].as_array().unwrap().is_empty());
    assert!(s["gates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|g| g["batch_acceptance_missing"] == true && g["green_at_head"] == false));
    assert!(stdout(repo.arc(&wt).args(["show"])).contains("not current on this store and host"));
}

/// Claim: explicit single-gate runs and a selected remainder execute alone,
/// while rerun executes the full accepted batch. Rejects running unchanged
/// members again or treating a one-gate remainder as a measured batch.
#[test]
fn selection_and_single_gate_runs_preserve_standalone_boundary() {
    let (repo, _, wt) = fixture("", "10s", "10s");
    measure(&repo, &wt);
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    fs::create_dir_all(wt.join("a")).unwrap();
    repo.commit(&wt, "a/source", "changed", "test: affect a");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let state = show(&repo, &wt);
    let evidence = entries(&state);
    assert!(
        evidence.iter().rev().find(|e| e["gate"] == "a").unwrap()["concurrent_batch"].is_null()
    );
    repo.arc(&wt)
        .args(["verify", "--gate", "b", "--rerun"])
        .assert()
        .success();
    repo.arc(&wt)
        .env("BATCH_MODE", "together")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
}

/// Claim: duplicate exclusive names are declaration errors before any gate
/// runs. Rejects deadlocking, serializing, or accepting a contradictory batch.
#[test]
fn duplicate_exclusive_names_are_refused() {
    let (repo, _, wt) = fixture("", "10s", "10s");
    let gates = fs::read_to_string(wt.join(".arc/gates.toml"))
        .unwrap()
        .replace("concurrent=true", "concurrent=true\nexclusive='same-lock'");
    repo.commit(&wt, ".arc/gates.toml", &gates, "test: duplicate lock");
    repo.arc(&wt)
        .args(["gates", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("same exclusive lock"));
    repo.arc(&wt)
        .args(["verify", "--all"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("same exclusive lock"));
    assert!(!repo.home.join("executions").exists());
}

/// Claim: foreign batch facts cannot become local green evidence and frozen
/// receipt validation detects forged acceptance. Rejects a sender's measurement
/// or an altered member identity overriding the receiver's own measurement.
#[test]
fn foreign_and_corrupt_measurements_cannot_authorize_receipts() {
    let (repo, source_id, wt) = fixture("", "10s", "10s");
    measure(&repo, &wt);
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let bundle = repo.home.join("batch.json");
    repo.arc(&wt)
        .args(["export", &source_id, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let receiver = Repo::new();
    deliver_git_objects(&repo, &receiver);
    git(
        &receiver.root,
        &["reset", "--hard", "refs/delivered/heads/master"],
    );
    let source = show(&repo, &wt);
    let branch = source["branch"].as_str().unwrap();
    git(&receiver.root, &["branch", branch, &repo.head(&wt)]);
    receiver
        .arc(&receiver.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let imported = receiver.home.join("imported");
    git(
        &receiver.root,
        &["worktree", "add", imported.to_str().unwrap(), branch],
    );
    let check = || {
        status(&receiver, &imported)["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|gate| gate["green_at_head"] == false)
    };
    assert!(
        check(),
        "sender measurement must not authorize imported passes"
    );
    for name in ["a-to-b", "b-to-a"] {
        assert!(Command::new("mkfifo")
            .arg(receiver.home.join(name))
            .status()
            .unwrap()
            .success());
    }
    measure(&receiver, &imported);
    assert!(
        check(),
        "receiver measurement must not relabel sender execution as local"
    );
    receiver
        .arc(&imported)
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
    assert!(status(&receiver, &imported)["gates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|gate| gate["green_at_head"] == true));
    let st = show(&repo, &wt);
    let id = st["change_id"].as_str().unwrap();
    rewrite_event(&repo, id, "verification-recorded", |event| {
        event["concurrent_batch"]["acceptance"]["members"]["a"] =
            json!(format!("sha256:{}", "0".repeat(64)));
    });
    repo.arc(&wt)
        .args(["show", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "invalid concurrent batch provenance",
        ));
    let config: Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 11);
}

/// Claim: snapshot, done, merged-tree verification and both landing forms use
/// the accepted scheduler. Rejects a wrapper quietly falling back to serial
/// execution or failing to record batch provenance on the actual shipped tree.
#[test]
fn required_set_callers_use_the_accepted_batch() {
    for caller in ["snapshot", "done", "against", "stack", "composed"] {
        let (repo, lower, wt) = fixture("[budget]\ntotal='15s'", "10s", "10s");
        measure(&repo, &wt);
        match caller {
            "snapshot" => {
                repo.arc(&wt)
                    .env("BATCH_MODE", "together")
                    .args(["snapshot", "--verify", "--rerun"])
                    .assert()
                    .success();
            }
            "done" => {
                approve(&repo, &wt);
                repo.arc(&wt)
                    .env("BATCH_MODE", "together")
                    .args(["done"])
                    .assert()
                    .success();
            }
            "against" => {
                repo.commit(
                    &repo.root,
                    "target.txt",
                    "target",
                    "test: advance destination",
                );
                repo.arc(&wt)
                    .env("BATCH_MODE", "together")
                    .args(["verify", "--all", "--against", "master"])
                    .assert()
                    .success();
            }
            shape => {
                approve(&repo, &wt);
                let mut args = vec!["begin", "upper"];
                let base = repo.head(&wt);
                if shape == "stack" {
                    args.extend(["--base", &base]);
                }
                let upper = opened_change_id(&stdout(repo.arc(&repo.root).args(args)));
                let upper_wt = repo.home.join(".worktrees/repo-upper");
                repo.commit(&upper_wt, "upper.txt", "upper", "test: upper member");
                repo.arc(&upper_wt).args(["snapshot"]).assert().success();
                approve(&repo, &upper_wt);
                repo.arc(&repo.root)
                    .env("BATCH_MODE", "together")
                    .args([
                        "land",
                        &lower,
                        &upper,
                        "--debt",
                        "test composition",
                        "--max-attempts",
                        "1",
                    ])
                    .assert()
                    .success();
                let state = json_stdout(repo.arc(&upper_wt).args(["show", &upper, "--json"]));
                assert!(
                    entries(&state)
                        .iter()
                        .any(|e| e["concurrent_batch"].is_object()),
                    "{shape}: {state}"
                );
                continue;
            }
        }
        assert!(
            entries(&show(&repo, &wt))
                .iter()
                .any(|e| e["concurrent_batch"].is_object()),
            "{caller}"
        );
    }
}

/// Claim: clean batch executions justify raises and seed reusable baselines,
/// while publication still runs complete standalone checks. Rejects losing
/// direct-execution provenance, dropping baseline batch facts, or batching publication.
#[test]
fn batch_members_support_raises_base_passes_and_standalone_publication() {
    let (repo, warm_id, wt) = fixture(
        "[selection]\nbase_passes=true\ninert=['**/*.md']",
        "10s",
        "10s",
    );
    measure(&repo, &wt);
    repo.arc(&wt)
        .env("BATCH_MODE", "together")
        .args(["verify", "--all"])
        .assert()
        .success();
    approve(&repo, &wt);
    repo.arc(&repo.root)
        .args(["integrate", &warm_id])
        .assert()
        .success();
    let id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "docs"])));
    let docs = repo.home.join(".worktrees/repo-docs");
    repo.commit(&docs, "notes.md", "docs", "test: documentation");
    repo.arc(&docs).args(["snapshot"]).assert().success();
    repo.arc(&docs).args(["verify", "--all"]).assert().success();
    let state = show(&repo, &docs);
    assert_eq!(
        entries(&state)
            .iter()
            .filter(|entry| entry["base_pass"].is_object() && entry["concurrent_batch"].is_object())
            .count(),
        2,
        "{state}"
    );
    approve(&repo, &docs);
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    fs::remove_file(repo.home.join("executions")).unwrap();
    repo.arc(&repo.root)
        .args(["publication", "--target", "master"])
        .assert()
        .success();
    let ran = fs::read_to_string(repo.home.join("executions")).unwrap();
    assert_eq!(ran.lines().collect::<Vec<_>>(), ["a", "b"]);

    let (repo, _, wt) = fixture("", "10s", "10s");
    measure(&repo, &wt);
    fs::create_dir_all(wt.join("a")).unwrap();
    repo.commit(
        &wt,
        "a/growth",
        &(0..100).map(|i| format!("line_{i}\n")).collect::<String>(),
        "test: qualifying growth",
    );
    let gates = fs::read_to_string(wt.join(".arc/gates.toml"))
        .unwrap()
        .replacen("budget='10s'", "budget='10.4s'", 1);
    repo.commit(&wt, ".arc/gates.toml", &gates, "test: proposed budget");
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "10300")
        .args(["verify", "--all"])
        .assert()
        .code(1);
    repo.arc(&wt)
        .args([
            "gates",
            "raise",
            "a",
            "--reason",
            "Measured and reduced the added work",
        ])
        .assert()
        .success();
    assert!(show(&repo, &wt)["budget_raises"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["raise"]["increases"][0]["execution"].is_string()));
}

/// Claim: total-only raises use the accepted maximum, and profiles whose
/// membership differs receive no discount. Rejects sequential declaration sums
/// creating unnecessary raises or another profile borrowing a larger batch.
#[test]
fn total_raise_and_profile_costs_follow_actual_accepted_membership() {
    let extra = "[budget]\ntotal='10s'\n[gates.c]\ncommand='true'\nbudget='6s'\n";
    let (repo, _, wt) = fixture(extra, "6s", "6s");
    fs::create_dir_all(wt.join("src")).unwrap();
    repo.commit(
        &wt,
        "src/growth",
        &(0..500).map(|i| format!("line_{i}\n")).collect::<String>(),
        "test: total growth",
    );
    let original = fs::read_to_string(wt.join(".arc/gates.toml")).unwrap();
    repo.commit(
        &wt,
        ".arc/gates.toml",
        &original.replace("total='10s'", "total='12s'"),
        "test: total declaration",
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt)
        .args([
            "gates",
            "raise",
            "--total",
            "--reason",
            "Measured complete declaration cost",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("declaration sum"));
    measure(&repo, &wt);
    repo.arc(&wt)
        .args([
            "gates",
            "raise",
            "--total",
            "--reason",
            "Measured complete declaration cost",
        ])
        .assert()
        .success();
    let state = show(&repo, &wt);
    assert!(
        state["budget_raises"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["raise"]["increases"][0]["declaration_sum_ms"] == 12000),
        "{state}"
    );

    let (repo, _, wt) = fixture(
        "[gates.c]\ncommand='true'\nbudget='6s'\nconcurrent=true\nprofiles=['release']",
        "6s",
        "6s",
    );
    measure(&repo, &wt);
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(report["integration_budget_sum_ms"], 12000);
    assert_eq!(report["integration_budget_sums_ms"]["release"], 6000);
    repo.arc(&wt)
        .args(["verify", "--all"])
        .assert()
        .success()
        .stderr(predicates::str::contains("no current local measurement"));
}

/// Claim: a shared landing execution keeps every compatible exclusive lock
/// and the shortest admission limit, and refuses incompatible names. Rejects
/// losing a member's resource requirement while coalescing identical checks.
#[test]
fn landing_coalescing_preserves_lock_and_admission_requirements() {
    for conflicting in [false, true] {
        let (repo, lower, lower_wt) = fixture("", "10s", "10s");
        let original = fs::read_to_string(lower_wt.join(".arc/gates.toml")).unwrap();
        repo.commit(
            &lower_wt,
            ".arc/gates.toml",
            &original.replace(
                "paths=['a/']",
                "paths=['a/']\nexclusive='shared-a'\nadmission='5s'",
            ),
            "test: first resource constraint",
        );
        repo.arc(&lower_wt).args(["snapshot"]).assert().success();
        approve(&repo, &lower_wt);
        let upper = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "upper"])));
        let upper_wt = repo.home.join(".worktrees/repo-upper");
        let lock = if conflicting { "other-a" } else { "shared-a" };
        repo.commit(
            &upper_wt,
            ".arc/gates.toml",
            &original.replace(
                "paths=['a/']",
                &format!("paths=['a/']\nexclusive='{lock}'\nadmission='50ms'"),
            ),
            "test: second resource constraint",
        );
        repo.arc(&upper_wt).args(["snapshot"]).assert().success();
        approve(&repo, &upper_wt);
        let mut plan = repo.arc(&repo.root);
        plan.args([
            "land",
            &lower,
            &upper,
            "--dry-run",
            "--json",
            "--debt",
            "test shared constraints",
        ]);
        if conflicting {
            plan.assert()
                .failure()
                .stderr(predicates::str::contains("conflicting exclusive locks"));
        } else {
            let plan = json_stdout(&mut plan);
            assert_eq!(plan["gates"]["a"]["exclusive"], "shared-a", "{plan}");
            assert_eq!(plan["gates"]["a"]["admission_ms"], 50, "{plan}");
        }
    }
}

/// I5: measured concurrent batches cannot excuse unattributed sibling CPU load.
/// Rejects treating an accepted batch's own gates as independent host contention.
#[test]
fn a_measured_batch_cannot_excuse_its_own_cpu_load() {
    let (repo, _, wt) = fixture("", "1s", "1s");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "900")
        .env(
            "ARC_TEST_GATE_WORK",
            super::gate_work::readings(Some(1000), 1000),
        )
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
    measure(&repo, &wt);
    repo.arc(&wt)
        .env("BATCH_MODE", "together")
        .env("ARC_TEST_COMMAND_RUN_MS", "1100")
        .env(
            "ARC_TEST_GATE_WORK",
            super::gate_work::readings(Some(1000), 5000),
        )
        .args(["verify", "--all", "--rerun"])
        .assert()
        .code(1);
    let state = show(&repo, &wt);
    for entry in entries(&state).into_iter().rev().take(2) {
        assert_eq!(entry["budget_at_run"]["verdict"], "over-budget");
        assert_eq!(entry["work_conditions"]["concurrent_unattributed"], true);
    }
}
