use crate::common::*;
use serde_json::{json, Value};

fn declare(repo: &Repo, budget: &str) {
    repo.declare_gates_locally(&format!(
        "[gates.unit]\ncommand='true'\nbudget='{budget}'\nbypass='documentation only'\n"
    ));
}
fn status(repo: &Repo, wt: &Path, id: &str) -> Value {
    json_stdout(repo.arc(wt).args(["status", id]))
}
fn evidence(repo: &Repo, wt: &Path, id: &str) -> Value {
    let st = json_stdout(repo.arc(wt).args(["show", id, "--json"]));
    st["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|v| v["gate"] == "unit")
        .unwrap()
        .clone()
}
fn verify(repo: &Repo, wt: &Path, run_ms: u64, code: i32) {
    repo.arc(wt)
        .env("ARC_TEST_COMMAND_RUN_MS", run_ms.to_string())
        .args(["verify", "--all", "--rerun"])
        .assert()
        .code(code);
}
fn approve(repo: &Repo, wt: &Path) {
    repo.arc(wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
}

/// Claim: an exit-zero command over budget remains a raw pass but refuses ordinary
/// authorization and reuse, until explicit rerun. Rejects a green raw pass, fallback
/// to faster evidence, and the queue executing an observed refusal again.
#[test]
fn ordinary_budget_refusal_preserves_the_pass_and_requires_explicit_rerun() {
    let repo = Repo::new();
    declare(&repo, "10s");
    let (id, wt, _) = change_with_patchset(&repo, "budget");
    approve(&repo, &wt);
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "11300")
        .args(["verify", "--all"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains(
            "gate unit: over budget, ran 11.3s of 10s",
        ));
    let raw = evidence(&repo, &wt, &id);
    assert_eq!(raw["result"], "pass");
    let s = status(&repo, &wt, &id);
    assert_eq!(s["gates"][0]["budget"]["verdict"], "over-budget");
    assert_eq!(s["gates"][0]["green_at_head"], false);
    let shown = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
    assert_eq!(shown["gate_status"][0]["budget"]["excess_ms"], 1300);
    assert!(stdout(repo.arc(&wt).args(["show", &id])).contains("over budget"));
    assert!(stdout(repo.arc(&wt).args(["explain", &id])).contains("current budget applicability"));
    repo.arc(&wt).args(["check", &id]).assert().code(5);
    let before = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .code(5);
    assert_eq!(repo.head(&repo.root), before);
    for args in [vec!["verify", "--all"], vec!["verify", "--gate", "unit"]] {
        repo.arc(&wt)
            .env("ARC_TEST_COMMAND_RUN_MS", "1")
            .args(args)
            .assert()
            .code(1);
        assert_eq!(evidence(&repo, &wt, &id)["run_ms"], 11300);
    }
    repo.arc(&wt)
        .args(["verify", "--gate", "unit", "--bypass", "documentation only"])
        .assert()
        .failure();
    let (other, _, _) = change_with_patchset(&repo, "queued-after-budget");
    repo.arc(&repo.root)
        .args(["integrate", &id, &other])
        .assert()
        .failure();
    assert_eq!(evidence(&repo, &wt, &id)["event_id"], raw["event_id"]);
    verify(&repo, &wt, 10000, 0);
    assert_eq!(status(&repo, &wt, &id)["gates"][0]["green_at_head"], true);
    verify(&repo, &wt, 10001, 1);
}

/// Claim: reducing only a budget retains identity and the newest selected receipt;
/// replay keeps delta reuse readable. Rejects identity drift, an older faster receipt,
/// an implicit rerun, and reduction consulting the current budget during replay.
#[test]
fn a_budget_reduction_rejudges_the_same_receipt_and_preserves_delta_replay() {
    let repo = Repo::new();
    let selector = "python3 -c 'import json,sys; d=json.load(sys.stdin); print(json.dumps(dict(schema=\"arc-gate-selection/1\",complete=True,request=d,gates=[])))'";
    let decl = |budget| {
        format!("[gates.unit]\ncommand='true'\nbudget='{budget}'\nselector={selector:?}\n")
    };
    repo.declare_gates_locally(&decl("20s"));
    let (id, wt, _) = change_with_patchset(&repo, "reuse-budget");
    verify(&repo, &wt, 1000, 0);
    verify(&repo, &wt, 11000, 0);
    let source = evidence(&repo, &wt, &id)["event_id"].clone();
    repo.commit(&wt, "docs.md", "text\n", "test: change selected scope");
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let selected = evidence(&repo, &wt, &id)["event_id"].clone();
    repo.declare_gates_locally(&decl("10s"));
    let replay = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
    assert_eq!(replay["gate_status"][0]["budget"]["verdict"], "over-budget");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "1")
        .args(["verify", "--all"])
        .assert()
        .code(1);
    assert_eq!(evidence(&repo, &wt, &id)["run_ms"], 11000);
    let commands =
        stdout(
            repo.arc(&wt)
                .args(["events", "--change", &id, "--type", "verification-recorded"]),
        );
    assert_eq!(commands.lines().count(), 2);
    let events =
        stdout(
            repo.arc(&wt)
                .args(["events", "--change", &id, "--type", "verification-reused"]),
        );
    let reuses = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(reuses[0]["evidence_event_id"], source);
    assert_eq!(reuses.last().unwrap()["evidence_event_id"], selected);
}

/// Claim: command timing is mandatory under a budget, including legacy and attested
/// evidence; timed legacy evidence needs no stored judgement. Rejects duration_ms as a
/// substitute and an attestation overwriting a measured budget refusal with green.
#[test]
fn untimed_evidence_is_unmeasured_and_timed_prebudget_evidence_stands() {
    let repo = Repo::new();
    declare(&repo, "10s");
    let (id, wt, head) = change_with_patchset(&repo, "unmeasured");
    verify(&repo, &wt, 9000, 0);
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        v.as_object_mut().unwrap().remove("budget_at_run");
    });
    assert_eq!(status(&repo, &wt, &id)["gates"][0]["green_at_head"], true);
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        v.as_object_mut().unwrap().remove("run_ms");
        v["duration_ms"] = json!(1);
    });
    assert_eq!(
        status(&repo, &wt, &id)["gates"][0]["budget"]["verdict"],
        "unmeasured"
    );
    repo.arc(&wt)
        .args([
            "verify",
            "--gate",
            "unit",
            "--attest",
            "--result",
            "pass",
            "--tested-revision",
            &head,
            "--execution-host",
            "fixture-runner",
            "--runner",
            "fixture",
        ])
        .assert()
        .code(1);
    assert_eq!(status(&repo, &wt, &id)["gates"][0]["green_at_head"], false);
}

/// Claim: the effective total is a separate authorization rule for every profile,
/// including an active profile with zero rows. Rejects acceptance of invalid totals
/// merely because every gate row is green or no gate row exists.
#[test]
fn invalid_effective_totals_block_with_green_or_absent_gate_rows() {
    for (empty, missing) in [(false, false), (true, false), (false, true)] {
        let repo = Repo::new();
        let profiles = if empty { "profiles=['other']\n" } else { "" };
        let gate_budget = if missing { "" } else { "budget='10s'\n" };
        repo.declare_gates_locally(&format!(
            "[budget]\ntotal='9s'\n[gates.unit]\ncommand='true'\n{gate_budget}{profiles}"
        ));
        let (id, wt, _) =
            change_with_patchset(&repo, if empty { "empty-total" } else { "green-total" });
        approve(&repo, &wt);
        if !empty {
            verify(&repo, &wt, 1000, 0);
        }
        let s = status(&repo, &wt, &id);
        assert!(s["budget_errors"].as_array().unwrap().iter().any(|e| e
            .as_str()
            .unwrap()
            .contains(if missing {
                "needs a budget"
            } else {
                "exceeds total"
            })));
        assert!(s["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["green_at_head"] == true));
        repo.arc(&wt).args(["check", &id]).assert().code(5);
        let before = repo.head(&repo.root);
        repo.arc(&repo.root)
            .args(["integrate", &id])
            .assert()
            .code(5);
        assert_eq!(repo.head(&repo.root), before);
        let doctor = stdout_any_status(repo.arc(&wt).args(["doctor", "--json"]));
        assert!(doctor.contains("gate-budget-invalid"));
    }
}

/// Claim: explicit timeouts remain declared seconds while fractional budgets derive
/// a separate millisecond command limit. Rejects rounding, a derived declared timeout,
/// or preparation and admission being part of the budget clock.
#[test]
fn timeout_and_fractional_execution_limit_round_trip_separately() {
    let repo = Repo::new();
    declare(&repo, "2.5s");
    let (id, wt, _) = change_with_patchset(&repo, "fractional");
    verify(&repo, &wt, 2500, 0);
    let raw = evidence(&repo, &wt, &id);
    assert!(raw["timeout_seconds"].is_null());
    assert_eq!(raw["execution_limit_ms"], 25000);
    assert_eq!(raw["execution_limit_origin"], "budget-times-ten");
    repo.declare_gates_locally("[gates.unit]\ncommand='true'\nbudget='500ms'\ntimeout='2s'\n");
    verify(&repo, &wt, 500, 0);
    let raw = evidence(&repo, &wt, &id);
    assert_eq!(raw["timeout_seconds"], 2);
    assert_eq!(raw["execution_limit_ms"], 2000);
    assert_eq!(raw["execution_limit_origin"], "timeout");
}

/// Claim: parallel preflight exits nonzero for an over-budget raw pass while its
/// provenance stays unknown. Rejects raw pass counting masking the budget refusal.
#[test]
fn parallel_preflight_refuses_over_budget_without_conferring_green() {
    let repo = Repo::new();
    declare(&repo, "10s");
    let (id, wt, _) = change_with_patchset(&repo, "parallel-budget");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "11000")
        .args(["verify", "--all", "--parallel"])
        .assert()
        .code(1);
    assert_eq!(evidence(&repo, &wt, &id)["result"], "pass");
    assert!(evidence(&repo, &wt, &id)["worktree_dirty"].is_null());
    assert_eq!(status(&repo, &wt, &id)["gates"][0]["green_at_head"], false);
}

/// Claim: integrated authorization stays recorded after a reduction while current
/// applicability changes. Rejects rewriting closure, replay failure or presenting
/// historical acceptance as current budget compliance.
#[test]
fn closed_history_and_current_applicability_are_labelled_separately() {
    let repo = Repo::new();
    declare(&repo, "20s");
    let (id, wt, _) = change_with_patchset(&repo, "closed-budget");
    approve(&repo, &wt);
    verify(&repo, &wt, 11000, 0);
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    declare(&repo, "10s");
    let s = json_stdout(repo.arc(&repo.root).args(["show", &id, "--json"]));
    assert_eq!(
        s["closure"]["authorization"]["gates"]["unit"]["budget_ms"],
        20000
    );
    let e = json_stdout(repo.arc(&repo.root).args(["explain", &id, "--json"]));
    assert_eq!(
        e["historical_budget_authorization"]["gates"]["unit"]["budget_ms"],
        20000
    );
    assert_eq!(
        e["current_budget_applicability"][0]["budget"]["verdict"],
        "over-budget"
    );
}

/// Claim: gate reports use command measurements for budget percentages, list the
/// newest overrun, and init writes a measured declaration with headroom once. Rejects exact-worst budgets, legacy
/// duration percentages and replacing an existing declared budget.
#[test]
fn reports_and_init_expose_budget_cost_without_legacy_percentages() {
    let repo = Repo::new();
    declare(&repo, "10s");
    let (id, wt, _) = change_with_patchset(&repo, "report-budget");
    verify(&repo, &wt, 11000, 1);
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(report["gates"][0]["budget_share_percent"], 110.0);
    assert_eq!(report["gates"][0]["last_over_budget"]["run_ms"], 11000);
    assert!(stdout(repo.arc(&wt).args(["gates", "over", &id])).contains("excess 1s"));
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        v.as_object_mut().unwrap().remove("run_ms");
        v["duration_ms"] = json!(99999);
    });
    assert!(
        json_stdout(repo.arc(&wt).args(["gates", "--json"]))["gates"][0]["budget_share_percent"]
            .is_null()
    );
    let fresh = Repo::new();
    fresh.redeclare_gates("[gates.unit]\ncommand='true'\n");
    fresh
        .arc(&fresh.root)
        .env("ARC_TEST_COMMAND_RUN_MS", "2500")
        .args(["gates", "init"])
        .assert()
        .success()
        .stdout(predicates::str::contains("budget total: 2.7s"));
    fresh
        .arc(&fresh.root)
        .args(["gates", "init"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("existing budget"));
}

/// Claim: a fractional budget stops an unbounded command with a distinct derived
/// millisecond limit. Rejects truncating the bound to zero or failing to stop it;
/// no assertion depends on elapsed wall time under host load.
#[test]
fn a_fractional_derived_limit_stops_the_command() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.unit]\ncommand='while :; do :; done'\nbudget='0.002s'\n");
    let (id, wt, _) = change_with_patchset(&repo, "stop-budget");
    repo.arc(&wt).args(["verify", "--all"]).assert().code(1);
    let e = evidence(&repo, &wt, &id);
    assert_eq!(e["timed_out"], true);
    assert_eq!(e["execution_limit_ms"], 20);
    assert!(e["timeout_seconds"].is_null());
}

/// Claim: a budget edited while a command is in preparation is freshly judged
/// before verify returns, even though execution identity is unchanged. Rejects
/// trusting only the budget captured before preparation.
#[test]
fn a_budget_change_during_execution_is_freshly_judged() {
    use std::io::Write;
    let repo = Repo::new();
    declare(&repo, "20s");
    let (id, wt, _) = change_with_patchset(&repo, "moving-budget");
    let template = repo.arc(&wt);
    let mut command = Command::new(template.get_program());
    command
        .args(["verify", "--all"])
        .current_dir(&wt)
        .env("ARC_TEST_IMPORT_RENDEZVOUS", "gate-command-spawned")
        .env("ARC_TEST_COMMAND_RUN_MS", "11000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in template.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    let mut child = command.spawn().unwrap();
    let mut reader = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "arc test: import gate-command-spawned\n" {
            break;
        }
    }
    declare(&repo, "10s");
    child.stdin.take().unwrap().write_all(b"+").unwrap();
    let result = child.wait_with_output().unwrap();
    assert_eq!(
        result.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert_eq!(
        evidence(&repo, &wt, &id)["budget_at_run"]["budget_ms"],
        20000
    );
    assert_eq!(
        status(&repo, &wt, &id)["gates"][0]["budget"]["budget_ms"],
        10000
    );
}

/// Claim: imported untimed evidence remains unmeasured, and selector reuse carries
/// raw timing through transport and reduction. Rejects transport granting a budget
/// exemption or a receiver losing the source timing on a reuse receipt.
#[test]
fn imported_timing_and_absence_are_judged_at_the_receiver() {
    for timed in [true, false] {
        let source = Repo::new();
        let selector = "python3 -c 'import json,sys; d=json.load(sys.stdin); print(json.dumps(dict(schema=\"arc-gate-selection/1\",complete=True,request=d,gates=[])))'";
        source.declare_gates_locally(&format!(
            "[gates.unit]\ncommand='true'\nselector={selector:?}\n"
        ));
        let (id, wt, _) = change_with_patchset(
            &source,
            if timed {
                "timed-import"
            } else {
                "untimed-import"
            },
        );
        verify(&source, &wt, 9000, 0);
        source.commit(
            &wt,
            "scope.md",
            "selected scope\n",
            "test: advance selected tree",
        );
        source.arc(&wt).args(["snapshot"]).assert().success();
        source.arc(&wt).args(["verify", "--all"]).assert().success();
        if !timed {
            rewrite_event(&source, &id, "verification-recorded", |v| {
                v.as_object_mut().unwrap().remove("run_ms");
                v["duration_ms"] = json!(1);
            });
        }
        let path = source.home.join("budget-bundle.json");
        source
            .arc(&wt)
            .args(["export", &id, "--output", path.to_str().unwrap()])
            .assert()
            .success();
        let bundle: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(
            bundle["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["event_type"] == "verification-reused"
                    && !event["delta"].is_null())
        );
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
        receiver.declare_gates_locally(&format!("[gates.unit]\ncommand='true'\nbudget='10s'\nselector={selector:?}\n[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{}'\n[[replica.publishers.origins]]\nreplica_id='{}'\nroles=['facts']\nactors=[]\n", bundle["source_replica_id"].as_str().unwrap(), bundle["source_replica_id"].as_str().unwrap()));
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
        let projected = json_stdout(receiver.arc(&receiver.root).args(["show", &id, "--json"]));
        assert_eq!(
            projected["gate_status"][0]["budget"]["verdict"],
            if timed { "within-budget" } else { "unmeasured" }
        );
        if timed {
            let policy = receiver.root.join(".git/arc/operator-policy.toml");
            fs::write(
                &policy,
                fs::read_to_string(&policy)
                    .unwrap()
                    .replace("budget='10s'", "budget='8s'"),
            )
            .unwrap();
            let lowered = json_stdout(receiver.arc(&receiver.root).args(["show", &id, "--json"]));
            assert_eq!(
                lowered["gate_status"][0]["budget"]["verdict"],
                "over-budget"
            );
            assert_eq!(lowered["gate_status"][0]["run_ms"], 9000);
        }
    }
}

/// Claim: an effective gate budget or total edited after the initial authorization
/// is detected before integration moves a ref. Rejects treating unchanged execution
/// identity and individually green gate rows as a stable authorization basis.
#[test]
fn budget_changes_between_authorization_reads_refuse_the_effect() {
    use std::io::Write;
    for total in [false, true] {
        let repo = Repo::new();
        let declaration = |amount| {
            format!(
                "[gates.unit]\ncommand='true'\nbudget='{}'\n{}",
                if total { "10s" } else { amount },
                if total {
                    format!("[budget]\ntotal='{amount}'\n")
                } else {
                    String::new()
                }
            )
        };
        repo.declare_gates_locally(&declaration("20s"));
        let (id, wt, _) = change_with_patchset(
            &repo,
            if total {
                "moving-total"
            } else {
                "moving-authority-budget"
            },
        );
        approve(&repo, &wt);
        verify(&repo, &wt, if total { 9000 } else { 11000 }, 0);
        let before = repo.head(&repo.root);
        let template = repo.arc(&repo.root);
        let mut command = Command::new(template.get_program());
        command
            .args(["integrate", &id])
            .current_dir(&repo.root)
            .env(
                "ARC_TEST_IMPORT_RENDEZVOUS",
                "integration-budget-confirmation",
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in template.get_envs() {
            if let Some(value) = value {
                command.env(key, value);
            } else {
                command.env_remove(key);
            }
        }
        let mut child = command.spawn().unwrap();
        let mut reader = BufReader::new(child.stderr.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert!(
                reader.read_line(&mut line).unwrap() > 0,
                "integration never reached confirmation"
            );
            if line == "arc test: import integration-budget-confirmation\n" {
                break;
            }
        }
        repo.declare_gates_locally(&declaration(if total { "15s" } else { "10s" }));
        child.stdin.take().unwrap().write_all(b"+").unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(!result.status.success());
        assert_eq!(repo.head(&repo.root), before);
        assert!(
            json_stdout(repo.arc(&repo.root).args(["show", &id, "--json"]))["closure"].is_null()
        );
    }
}

/// Claim: a queue projection stops on recorded budget refusal or invalid totals.
/// Rejects presenting either as work to execute again or a conditional integration.
#[test]
fn budget_queue_projection_stops_before_scheduling_recorded_refusal() {
    for total in [false, true] {
        let repo = Repo::new();
        repo.declare_gates_locally(&format!(
            "[gates.unit]\ncommand='true'\nbudget='10s'\n{}",
            if total { "[budget]\ntotal='9s'\n" } else { "" }
        ));
        let (id, wt, _) = change_with_patchset(&repo, "queue-budget");
        approve(&repo, &wt);
        verify(
            &repo,
            &wt,
            if total { 1000 } else { 11000 },
            if total { 0 } else { 1 },
        );
        let (other, _, _) = change_with_patchset(&repo, "queue-after-budget");
        let output = repo
            .arc(&repo.root)
            .args(["integrate", &id, &other, "--dry-run"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            !text.contains(&format!("would integrate {id}")),
            "budget refusal was planned as work: {text}"
        );
        assert_eq!(output.status.code(), Some(5));
    }
}

/// Claim: init measures every declaration, including operator-only gates, and writes
/// budgets with headroom into the project while preserving operator policy and
/// check identity. Rejects exact-worst budgets, requiring an existing project
/// declaration or editing operator policy.
#[test]
fn budget_init_measures_operator_only_declarations_without_editing_policy() {
    let repo = Repo::new();
    repo.declare_gates_locally(
        "[gates.unit]\ncommand='true'\nprofiles=['local']\nenvironment='printf fixed'\n",
    );
    let policy = fs::read(repo.root.join(".git/arc/operator-policy.toml")).unwrap();
    repo.arc(&repo.root)
        .env("ARC_TEST_COMMAND_RUN_MS", "1500")
        .args(["gates", "init"])
        .assert()
        .success();
    assert_eq!(
        fs::read(repo.root.join(".git/arc/operator-policy.toml")).unwrap(),
        policy
    );
    let gates = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    assert_eq!(gates["gates"][0]["budget_ms"], 1600);
    assert_eq!(
        gates["gates"][0]["declaration"]["environment"],
        "printf fixed"
    );
    assert_eq!(
        gates["gates"][0]["declaration"]["profiles"],
        json!(["local"])
    );
}

/// Claim: lowering a same-tree budget retains the newest applicable receipt even
/// when an older pass is faster. Rejects filtering budget failures before selection
/// and falling back to the earlier pass or executing an implicit retry.
#[test]
fn budget_reduction_never_selects_an_older_faster_same_tree_pass() {
    let repo = Repo::new();
    declare(&repo, "20s");
    let (id, wt, _) = change_with_patchset(&repo, "same-tree-budget");
    verify(&repo, &wt, 1000, 0);
    verify(&repo, &wt, 11000, 0);
    let selected = evidence(&repo, &wt, &id)["event_id"].clone();
    declare(&repo, "10s");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "1")
        .args(["verify", "--all"])
        .assert()
        .code(1);
    assert_eq!(evidence(&repo, &wt, &id)["event_id"], selected);
    assert_eq!(
        status(&repo, &wt, &id)["gates"][0]["evidence_event_id"],
        selected
    );
}

/// Claim: verification adopts the format barrier for a total declaration even
/// when an individual budget is missing; reads alone keep the format. Rejects
/// an older executable retaining access after a budget-aware verification write.
#[test]
fn budget_total_declarations_adopt_even_when_a_gate_budget_is_missing() {
    let repo = Repo::new();
    repo.declare_gates_locally("[budget]\ntotal='10s'\n[gates.unit]\ncommand='true'\n");
    let (id, wt, _) = change_with_patchset(&repo, "adopt-total");
    let config = repo.root.join(".git/arc/config.json");
    let before = fs::read(&config).unwrap();
    let _ = status(&repo, &wt, &id);
    assert_eq!(fs::read(&config).unwrap(), before);
    verify(&repo, &wt, 1000, 0);
    let after: Value = serde_json::from_slice(&fs::read(config).unwrap()).unwrap();
    assert_eq!(after["schema_version"], 9);
}

/// Claim: named path-based delta reuse judges its selected receipt without running
/// the command again. Rejects a zero exit for an over-budget selected pass and
/// any retry that was not explicitly requested.
#[test]
fn budget_named_path_reuse_refuses_until_explicit_rerun() {
    let repo = Repo::new();
    let counter = repo.home.join("unit-runs");
    let command = format!("echo run >> '{}'", counter.display());
    let declared = |budget| {
        format!("[selection]\ninert=['docs/']\n[gates.unit]\ncommand={command:?}\npaths=['src/']\nbudget='{budget}'\n")
    };
    repo.declare_gates_locally(&declared("20s"));
    let (id, wt, _) = change_with_patchset(&repo, "named-path-budget");
    verify(&repo, &wt, 11000, 0);
    fs::create_dir_all(wt.join("docs")).unwrap();
    repo.commit(
        &wt,
        "docs/note.md",
        "inert text\n",
        "test: advance inert scope",
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.declare_gates_locally(&declared("10s"));
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "1")
        .args(["verify", "--gate", "unit"])
        .assert()
        .code(1);
    assert_eq!(fs::read_to_string(&counter).unwrap().lines().count(), 1);
    assert_eq!(
        status(&repo, &wt, &id)["gates"][0]["budget"]["verdict"],
        "over-budget"
    );
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "1")
        .args(["verify", "--gate", "unit", "--rerun"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(counter).unwrap().lines().count(), 2);
}

/// Claim: verification's recent timing median counts observed runs once, even
/// after repeated selector reuse. Rejects copied source clocks multiplying a slow
/// sample and warning above 80% when the observed-run median is below it.
#[test]
fn budget_warning_median_does_not_count_selector_receipts_as_runs() {
    let repo = Repo::new();
    repo.declare_gates_locally("[selection]\ninert=['docs/']\n[gates.unit]\ncommand='true'\npaths=['src/']\nbudget='10s'\n");
    let (_, wt, _) = change_with_patchset(&repo, "median-budget");
    verify(&repo, &wt, 9000, 0);
    fs::create_dir_all(wt.join("docs")).unwrap();
    for i in 0..5 {
        repo.commit(
            &wt,
            "docs/note.md",
            &format!("{i}\n"),
            "test: advance inert scope",
        );
        repo.arc(&wt).args(["verify", "--all"]).assert().success();
    }
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "6000")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success()
        .stderr(predicates::str::contains("recent command median").not());
}
