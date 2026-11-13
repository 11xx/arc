use crate::common::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn declared(command: &str) -> Repo {
    let repo = Repo::new();
    repo.redeclare_gates(&format!("[gates.unit]\ncommand = {command:?}\nprofiles = ['local']\ntimeout = '2s'\n[gates.release]\ncommand = 'true'\nevents = ['publication']\n"));
    repo
}

fn row(report: &Value) -> &Value {
    report["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "unit")
        .unwrap()
}

fn copy_event(repo: &Repo, id: &str, source: &str, mut edit: impl FnMut(&mut Value)) {
    let directory = event_dir(repo, id);
    let mut events: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    events.sort();
    let mut value: Value = events
        .iter()
        .rev()
        .map(|p| serde_json::from_slice::<Value>(&fs::read(p).unwrap()).unwrap())
        .find(|v| v["event_type"] == source)
        .unwrap();
    let last: Value = serde_json::from_slice(&fs::read(events.last().unwrap()).unwrap()).unwrap();
    let next = last["event_id"]
        .as_str()
        .unwrap()
        .parse::<ulid::Ulid>()
        .unwrap();
    let next = ulid::Ulid::from(u128::from(next) + 1).to_string();
    value["event_id"] = json!(next);
    edit(&mut value);
    fs::write(
        directory.join(format!("{next}.json")),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
}

/// Claim: declarations without budgets expose command costs across changes and separate
/// reuse and selector skips. Rejects either skip counted as a run, legacy total time
/// preferred over command time, or a later failure mistaken for the first failure.
#[test]
fn declarations_and_costs_exclude_reuse_and_selection() {
    let repo = declared("true");
    let (id, wt, _) = change_with_patchset(&repo, "costs");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    // A separate change contributes another observed run.
    let (other_id, other, _) = change_with_patchset(&repo, "other-costs");
    repo.arc(&other)
        .args(["verify", "--all"])
        .assert()
        .success();
    rewrite_event(&repo, &other_id, "verification-recorded", |v| {
        v["run_ms"] = json!(40);
    });
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        v["run_ms"] = json!(20);
        v["duration_ms"] = json!(9000);
    });
    repo.arc(&wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success();
    copy_event(&repo, &id, "verification-reused", |v| {
        v["delta"] = json!({"source_tree":"a", "target_tree":"b", "declaration":"d", "selector":"s", "changed_paths":[], "selected_gates":[], "output_sha256":"d", "output":"{}"});
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v.as_object_mut().unwrap().remove("run_ms");
        v["duration_ms"] = json!(100);
        v["result"] = json!("fail");
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v["gate"] = json!("release");
        v["duration_ms"] = json!(1000);
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v["gate"] = json!("unit");
        v["result"] = json!("not-admitted");
        v["run_ms"] = json!(50000);
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v["attested"] = json!(true);
        v["result"] = json!("pass");
    });
    let before = tree_bytes(&repo.root.join(".git/arc"));
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(report["schema"], "arc-gates/1");
    assert_eq!(report["budget_declared"], false);
    let unit = row(&report);
    assert!(unit["budget_ms"].is_null());
    assert_eq!(unit["declaration"]["command"], "true");
    assert_eq!(unit["declaration"]["profiles"], json!(["local"]));
    assert_eq!(unit["declaration"]["timeout_seconds"], 2);
    assert_eq!(
        unit["declaration"]["events"],
        json!(["integration", "publication"])
    );
    let h = &unit["history"];
    assert_eq!(h["runs"], 3);
    assert_eq!(h["reused"], 1);
    assert_eq!(h["selected_out"], 1);
    assert_eq!(h["attested"], 1);
    assert_eq!(h["not_admitted"], 1);
    assert_eq!(h["timing"]["samples"], 3);
    assert_eq!(h["timing"]["median_ms"], 40);
    assert_eq!(h["timing"]["p90_ms"], 100);
    assert_eq!(h["timing"]["worst_ms"], 100);
    assert_eq!(h["timing"]["legacy_duration_samples"], 1);
    assert_eq!(h["ever_failed"], true);
    assert_eq!(h["last_failed_first"]["change_id"], id);
    let release = &report["gates"][0];
    assert_eq!(release["name"], "release");
    assert_eq!(release["history"]["ever_failed"], true);
    assert!(release["history"]["last_failed_first"].is_null());
    let text = stdout(repo.arc(&wt).arg("gates"));
    assert!(text.contains("budget: none declared"));
    assert!(text.contains("1 legacy duration_ms samples"));
    assert_eq!(before, tree_bytes(&repo.root.join(".git/arc")));
}

pub(super) fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    if !root.exists() {
        return files;
    }
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(tree_bytes(&path));
        } else {
            files.insert(path.clone(), fs::read(&path).unwrap());
        }
    }
    files
}

/// Claim: measurement executes exactly the requested rounds on the current tree,
/// prints wall, CPU, physical/logical cores and spread, and leaves ledger and output artifacts untouched.
/// Rejects recording verification evidence or measuring a different gate or tree.
#[test]
fn measurement_is_transient_and_honors_selection_and_failures() {
    let repo = declared("test -f marker && printf x >> measured");
    let (_, wt, _) = change_with_patchset(&repo, "measure");
    fs::write(wt.join("marker"), "present").unwrap();
    let before = tree_bytes(&repo.root.join(".git/arc"));
    let text = stdout(
        repo.arc(&wt)
            .args(["gates", "measure", "--runs", "3", "--gate", "unit"]),
    );
    assert_eq!(fs::read(wt.join("measured")).unwrap(), b"xxx");
    assert!(text.contains("unit 3/3: wall"), "{text}");
    assert!(text.contains("CPU") && text.contains("spread"), "{text}");
    assert!(
        text.contains("host cores: physical ") && text.contains("; logical "),
        "{text}"
    );
    let topology = fs::read_to_string("/proc/cpuinfo").unwrap();
    let physical = topology
        .split("\n\n")
        .filter_map(|block| {
            let field = |name: &str| {
                block.lines().find_map(|line| {
                    line.split_once(':')
                        .filter(|(k, _)| k.trim() == name)
                        .map(|(_, v)| v.trim())
                })
            };
            Some((field("physical id")?, field("core id")?))
        })
        .collect::<std::collections::BTreeSet<_>>();
    if !physical.is_empty() {
        assert!(
            text.contains(&format!("physical {};", physical.len())),
            "{text}"
        );
    }
    assert!(!text.contains("CPU -"), "{text}");
    assert!(!text.contains("release 1/"));
    assert_eq!(before, tree_bytes(&repo.root.join(".git/arc")));
    fs::remove_file(wt.join("marker")).unwrap();
    repo.arc(&wt)
        .args(["gates", "measure", "--runs", "1", "--gate", "unit"])
        .assert()
        .code(1);
    repo.arc(&wt)
        .args(["gates", "measure", "--runs", "0"])
        .assert()
        .code(2);
    repo.arc(&wt)
        .args(["gates", "measure", "--gate", "missing"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("undeclared gate"));
    repo.arc(&wt)
        .args(["gates", "measure", "--concurrent"])
        .assert()
        .code(2);
}

/// Claim: declarations can be reported before a ledger exists and unreadable changes
/// produce an explicit partial report. Rejects creating a ledger or hiding read errors.
#[test]
fn reports_without_a_ledger_and_preserves_partial_history() {
    let repo = declared("true");
    let report = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    assert_eq!(row(&report)["history"]["runs"], 0);
    assert!(row(&report)["history"]["timing"]["median_ms"].is_null());
    assert!(!repo.root.join(".git/arc/config.json").exists());
    let (id, wt, _) = change_with_patchset(&repo, "damaged");
    fs::write(event_dir(&repo, &id).join("broken.json"), "{").unwrap();
    let out = repo
        .arc(&wt)
        .args(["gates", "--json"])
        .assert()
        .code(16)
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["failures"][0]["change_id"], id);
}

/// Claim: workspace gate reports use each project's ledger and the registry's scope,
/// retirement and unreachable accounting. Rejects substituting the caller's ledger,
/// dropping projects without gates, observing retirees, or hiding orphaned anchors.
#[test]
fn workspace_uses_own_ledgers_scope_and_registration_accounting() {
    let repo = declared("true");
    let (_, wt, _) = change_with_patchset(&repo, "workspace-costs");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let second = repo.home.join("second");
    git(
        &repo.home,
        &[
            "clone",
            "-q",
            repo.root.to_str().unwrap(),
            second.to_str().unwrap(),
        ],
    );
    repo.arc(&second)
        .args(["begin", "second-costs", "--no-worktree"])
        .assert()
        .success();
    for _ in 0..2 {
        repo.arc(&second)
            .args(["verify", "--all", "--rerun"])
            .assert()
            .success();
    }
    let empty = repo.home.join("empty");
    let gone = repo.home.join("gone");
    let retired = repo.home.join("retired");
    let mut retired_journal = PathBuf::new();
    for anchor in [&empty, &gone, &retired] {
        fs::create_dir_all(anchor).unwrap();
        git(anchor, &["init", "-q", "-b", "master"]);
        repo.arc(anchor)
            .args(["journal", "note", "registration", "--body-file", "-"])
            .write_stdin("registered\n")
            .assert()
            .success();
        if anchor == &retired {
            retired_journal =
                PathBuf::from(stdout(repo.arc(anchor).args(["journal", "dir"])).trim());
        }
    }
    fs::remove_dir_all(empty.join(".git")).unwrap();
    fs::remove_dir_all(&gone).unwrap();
    fs::remove_dir_all(&retired).unwrap();
    repo.arc(&repo.root)
        .args(["workspace", "retire"])
        .arg(&retired_journal)
        .args(["--reason", "fixture removed"])
        .assert()
        .success();
    let before = tree_bytes(&repo.home.join(".local/ai"));
    let out = repo
        .arc(&repo.root)
        .args(["workspace", "gates", "--global", "--retired", "--json"])
        .assert()
        .code(16)
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], "arc-workspace-gates/1");
    let c = &report["collection"];
    assert_eq!(c["discovered"], 4);
    assert_eq!(c["selected"], 4);
    assert_eq!(c["non_empty"], 2);
    assert_eq!(c["empty"], 1);
    assert_eq!(c["failed"], 1);
    assert_eq!(c["retired"], 1);
    assert_eq!(report["unreachable"].as_array().unwrap().len(), 1);
    assert_eq!(report["retired"].as_array().unwrap().len(), 1);
    let projects = report["projects"].as_array().unwrap();
    let first = projects
        .iter()
        .find(|p| p["anchor"] == repo.root.to_str().unwrap())
        .unwrap();
    let second = projects
        .iter()
        .find(|p| p["anchor"] == second.to_str().unwrap())
        .unwrap();
    assert_eq!(row(&first["report"])["history"]["runs"], 1);
    assert_eq!(row(&second["report"])["history"]["runs"], 2);
    assert!(projects.iter().any(|p| p["declares_gates"] == false));
    assert_eq!(before, tree_bytes(&repo.home.join(".local/ai")));
    let scoped = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "gates", "--here", "--json"]),
    );
    assert_eq!(scoped["collection"]["selected"], 1);
    assert_eq!(scoped["collection"]["skipped"], 3);
    assert_eq!(scoped["collection"]["retired"], 0);
    let under = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "gates", "--under"])
            .arg(&repo.root)
            .arg("--json"),
    );
    assert_eq!(scoped, under);
}

/// Claim: gate cost reports read imported and local verification history with one
/// ledger observation, one repository read for completed landings, and no state
/// reduction. Rejects ignoring landing outcomes, fetching import batches per
/// change, or reducing states to read gate events.
#[test]
fn reports_hold_one_typed_ledger_read_without_reducing_states() {
    let source = declared("true");
    let (imported, _, _) = change_with_patchset(&source, "imported-costs");
    let bundle = source.home.join("gate-costs.json");
    source
        .arc(&source.root)
        .args(["export", &imported, "--output"])
        .arg(&bundle)
        .assert()
        .success();
    let repo = declared("true");
    deliver_git_objects(&source, &repo);
    repo.arc(&repo.root)
        .arg("import")
        .arg(&bundle)
        .assert()
        .success();
    change_with_patchset(&repo, "local-costs");
    for args in [
        vec!["gates", "--json"],
        vec!["workspace", "gates", "--global", "--json"],
    ] {
        let (trace, reads, code) = crate::landing_reads::traced(&repo, &repo.root, &args);
        assert_eq!(code, Some(0), "{args:?}: {trace}");
        assert_eq!(trace["ledger"]["import"]["decoded"], 1, "{args:?}: {trace}");
        assert_eq!(
            trace["cache"]["import_batches"]["hits"]
                .as_u64()
                .unwrap_or(0),
            0,
            "{args:?} fetched held import batches per change: {trace}"
        );
        assert_eq!(
            trace["counts"]["repository_materializations"], 1,
            "{args:?}: {trace}"
        );
        for key in ["changes_reduced", "scope_derivations"] {
            assert_eq!(
                trace["counts"][key].as_u64().unwrap_or(0),
                0,
                "{args:?}: {key}: {trace}"
            );
        }
        assert_eq!(
            trace["ledger"]["change"]["opened"], trace["ledger"]["change"]["decoded"],
            "{args:?}: {trace}"
        );
        for kind in ["change", "import"] {
            assert_eq!(reads["most_reads_per_file"][kind], 1, "{args:?}: {reads}");
        }
    }
}

/// Claim: local and workspace reports expose the lock name and effective admission
/// limit, including the lock's default, in text and JSON. Rejects omitted admission
/// fields, an invented lock on an unrestricted gate, or dropping nested declarations.
#[test]
fn reports_include_exclusive_and_effective_admission() {
    let repo = Repo::new();
    let name = format!("report-{}", ulid::Ulid::new());
    repo.redeclare_gates(&format!("[gates.default]\ncommand='true'\nexclusive={name:?}\n[gates.limited]\ncommand='true'\nexclusive={name:?}\nadmission='150ms'\n[gates.plain]\ncommand='true'\n"));
    let report = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    assert_eq!(report["schema"], "arc-gates/1");
    let declared = |name: &str| {
        &report["gates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap()["declaration"]
    };
    assert_eq!(
        declared("default")["exclusive"],
        name,
        "gate report omitted lock"
    );
    assert_eq!(
        declared("default")["admission_ms"],
        60_000,
        "gate report omitted effective admission"
    );
    assert_eq!(declared("limited")["exclusive"], name);
    assert_eq!(declared("limited")["admission_ms"], 150);
    for field in ["exclusive", "admission_ms"] {
        assert!(
            declared("plain").get(field).is_some_and(Value::is_null),
            "missing declaration field {field}: {report}"
        );
    }
    let text = stdout(repo.arc(&repo.root).arg("gates"));
    assert!(
        text.contains(&format!("exclusive: {name}; admission: 60.000s")),
        "{text}"
    );
    assert!(
        text.contains(&format!("exclusive: {name}; admission: 0.150s")),
        "{text}"
    );
    assert!(text.contains("exclusive: none; admission: none"), "{text}");
    assert!(!repo.root.join(".git/arc/config.json").exists());
    repo.arc(&repo.root)
        .args(["begin", "declarations", "--no-worktree"])
        .assert()
        .success();
    let workspace =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "gates", "--here", "--json"]),
        );
    assert_eq!(workspace["schema"], "arc-workspace-gates/1");
    assert_eq!(workspace["projects"][0]["report"]["gates"], report["gates"]);
    let workspace_text = stdout(repo.arc(&repo.root).args(["workspace", "gates", "--here"]));
    assert!(
        workspace_text.contains(&format!("exclusive: {name}; admission: 0.150s")),
        "{workspace_text}"
    );
}

/// Claim: init budgets every gate from the worst of all requested serial rounds,
/// defaults to six, and reports spread without evidence. Rejects single-round,
/// last-round or cheapest-round budgets and partial writes after a failure.
#[test]
fn initialization_uses_complete_rounds_and_is_atomic() {
    let repo = Repo::new();
    repo.redeclare_gates("[gates.unit]\ncommand='printf x >> rounds; case $(wc -c < rounds) in 1) sleep 0.01;; 2) sleep 0.08;; *) sleep 0.02;; esac'\n");
    let text = stdout(repo.arc(&repo.root).args(["gates", "init", "--runs", "3"]));
    assert_eq!(fs::read(repo.root.join("rounds")).unwrap(), b"xxx");
    let measured: Vec<u64> = text
        .lines()
        .filter_map(|line| line.split_once("run_ms=").map(|(_, n)| n.parse().unwrap()))
        .collect();
    assert_eq!(measured.len(), 3, "{text}");
    let report = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    assert_eq!(
        report["gates"][0]["budget_ms"],
        *measured.iter().max().unwrap()
    );
    assert!(
        text.contains("from 3 rounds") && text.contains("spread"),
        "{text}"
    );
    assert!(!repo.root.join(".git/arc/config.json").exists());

    let default = Repo::new();
    default.redeclare_gates("[gates.unit]\ncommand='printf x >> rounds'\n");
    let text = stdout(default.arc(&default.root).args(["gates", "init"]));
    assert_eq!(fs::read(default.root.join("rounds")).unwrap(), b"xxxxxx");
    assert!(text.contains("from 6 rounds"), "{text}");
    let failed = Repo::new();
    failed.redeclare_gates(
        "[gates.unit]\ncommand='printf x >> rounds; test $(wc -c < rounds) -ne 2'\n",
    );
    let before = fs::read(failed.root.join(".arc/gates.toml")).unwrap();
    failed
        .arc(&failed.root)
        .args(["gates", "init", "--runs", "3"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("round 2/3"));
    assert_eq!(
        fs::read(failed.root.join(".arc/gates.toml")).unwrap(),
        before
    );
    assert_eq!(fs::read(failed.root.join("rounds")).unwrap(), b"xx");
    failed
        .arc(&failed.root)
        .args(["gates", "init", "--runs", "0"])
        .assert()
        .code(2);
}

/// Claim: receipt diagnosis separates waiting, queueing, preparation and duplicate
/// executions with numbers, and leaves absent fields unmeasured. Rejects treating
/// reused evidence or legacy duration as observed command/CPU time, or excluding
/// integrated changes that needed no execution from the duplication denominator.
#[test]
fn receipt_cost_kinds_and_missing_fields_are_explicit() {
    let repo = declared("true");
    let (id, wt, _) = change_with_patchset(&repo, "diagnose");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    rewrite_event(&repo, &id, "verification-recorded", |v| {
        v["run_ms"] = json!(1000);
        v["cpu_user_ms"] = json!(80);
        v["cpu_system_ms"] = json!(20);
        v["wait_ms"] = json!(200);
        v["prepare_ms"] = json!(150);
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v.as_object_mut().unwrap().remove("run_id");
    });
    copy_event(&repo, &id, "verification-recorded", |v| {
        v["event_type"] = json!("change-closed");
        v["outcome"] = json!("integrated");
    });
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let diagnosis = &row(&report)["diagnosis"];
    assert_eq!(
        diagnosis["kinds"],
        json!(["waiting", "queueing", "preparation", "duplication"])
    );
    assert_eq!(diagnosis["wall"]["median_ms"], 1000);
    assert_eq!(diagnosis["cpu"]["median_ms"], 100);
    assert_eq!(diagnosis["wait"]["median_ms"], 200);
    assert_eq!(diagnosis["preparation"]["median_ms"], 150);
    assert_eq!(diagnosis["runs_per_integrated_change"], 2.0);
    let text = stdout(repo.arc(&wt).args(["gates", "over", &id, "--gate", "unit"]));
    assert!(
        text.contains("waiting, queueing, preparation, duplication") && text.contains("1000ms"),
        "{text}"
    );
    assert!(text.contains("get the number first; measure serially; remove waste; rank checks by what they guard and cut from the bottom, never by cost alone; make what matters fit; lower concurrency until nothing fails; stop at a named floor; raise the budget last"), "{text}");
    let (other_id, _, _) = change_with_patchset(&repo, "no-executions");
    copy_event(&repo, &other_id, "patchset-added", |v| {
        v["event_type"] = json!("change-closed");
        v["outcome"] = json!("integrated");
    });
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(row(&report)["diagnosis"]["integrated_changes"], 2);
    assert_eq!(row(&report)["diagnosis"]["runs_per_integrated_change"], 1.0);
    for event in fs::read_dir(event_dir(&repo, &id)).unwrap() {
        let path = event.unwrap().path();
        let mut v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if v["event_type"] != "verification-recorded" {
            continue;
        }
        v["cpu_user_ms"] = json!(700);
        v["cpu_system_ms"] = json!(300);
        v["wait_ms"] = json!(99);
        v["prepare_ms"] = json!(99);
        fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    }
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(row(&report)["diagnosis"]["kinds"], json!(["real-work"]));
    for event in fs::read_dir(event_dir(&repo, &id)).unwrap() {
        let path = event.unwrap().path();
        let mut v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if v["event_type"] != "verification-recorded" {
            continue;
        }
        for key in [
            "run_ms",
            "cpu_user_ms",
            "cpu_system_ms",
            "wait_ms",
            "prepare_ms",
        ] {
            v.as_object_mut().unwrap().remove(key);
        }
        v["duration_ms"] = json!(99999);
        fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    }
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let diagnosis = &row(&report)["diagnosis"];
    assert!(diagnosis["wall"]["median_ms"].is_null(), "{diagnosis}");
    assert_eq!(
        diagnosis["unmeasured"],
        json!(["waiting/real-work", "queueing", "preparation"])
    );
}

/// Claim: timings receipts preserve bounded per-check cost and failed-change history,
/// and reuse contributes no extra observations. Rejects losing failures, counting
/// reuses as check executions, or making an advisory timings path change identity.
#[test]
fn check_costs_and_catches_are_retained_without_changing_identity() {
    let repo = Repo::new();
    let command =
        "printf 'arc-check-timings/1\\nfast\\t5\\tpass\\nslow\\t1200\\tfail\\n' > checks.tsv";
    repo.redeclare_gates(&format!(
        "[gates.unit]\ncommand={command:?}\ntimings='checks.tsv'\npaths=['src/']\n"
    ));
    repo.commit(
        &repo.root,
        ".gitignore",
        "checks.tsv\n",
        "test: ignore telemetry",
    );
    let (id, wt, _) = change_with_patchset(&repo, "checks");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let shown = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
    let checks = &shown["verifications"][0]["check_timings"];
    assert_eq!(checks["checks"].as_array().unwrap().len(), 2, "{shown}");
    assert_eq!(checks["checks"][0]["name"], "slow");
    repo.declare_gates_locally(&format!(
        "[gates.unit]\ncommand={command:?}\npaths=['src/']\ntimings='other.tsv'\n"
    ));
    repo.arc(&wt)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped"));
    repo.declare_gates_locally(&format!(
        "[gates.unit]\ncommand={command:?}\npaths=['src/']\ntimings='checks.tsv'\n"
    ));
    let (other_id, other_wt, _) = change_with_patchset(&repo, "other-checks");
    repo.arc(&other_wt)
        .args(["verify", "--all"])
        .assert()
        .success();
    rewrite_event(&repo, &other_id, "verification-recorded", |v| {
        v["check_timings"]["checks"][0]["ms"] = json!(1400);
        v["check_timings"]["checks"][0]["failed"] = json!(false);
    });
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    let costs = &report["gates"][0]["checks"];
    assert_eq!(costs["measured_runs"], 2, "{report}");
    assert_eq!(costs["slowest"][0]["median_ms"], 1200);
    assert_eq!(costs["slowest"][0]["runs"], 2);
    assert_eq!(costs["slowest"][0]["failed_on"], json!([id]));
    let over = stdout(repo.arc(&wt).args(["gates", "over"]));
    assert!(
        over.contains("never failed in 2 runs, costs 0.005 seconds"),
        "{over}"
    );
    assert!(
        over.contains("failed on") && over.contains("1.200 seconds"),
        "{over}"
    );
}

/// Claim: malformed, missing, stale, nonregular, oversized and over-entry-limit
/// files remain advisory, while compact receipts retain at most 32 checks and name
/// drops. Rejects failing a passing gate, stale reuse or unbounded receipt storage.
#[test]
fn check_timings_are_bounded_fresh_and_advisory() {
    let repo = Repo::new();
    let command = "case $(cat mode) in missing) :;; malformed) printf 'arc-check-timings/1\\nbad' > checks.tsv;; oversized) head -c 65537 /dev/zero > checks.tsv;; entries) { printf 'arc-check-timings/1\\n'; seq 1 513 | while read n; do printf 'c%s\\t1\\tpass\\n' \"$n\"; done; } > checks.tsv;; capped) { printf 'arc-check-timings/1\\n'; seq 1 40 | while read n; do printf 'c%s\\t%s\\tpass\\n' \"$n\" \"$n\"; done; printf 'caught\\t1\\tfail\\n'; } > checks.tsv;; stale) :;; fifo) rm -f checks.tsv; mkfifo checks.tsv;; esac";
    repo.redeclare_gates(&format!(
        "[gates.unit]\ncommand={command:?}\ntimings='checks.tsv'\n"
    ));
    let (id, wt, _) = change_with_patchset(&repo, "bounded");
    for mode in [
        "missing",
        "malformed",
        "oversized",
        "entries",
        "capped",
        "stale",
        "fifo",
    ] {
        fs::write(wt.join("mode"), mode).unwrap();
        repo.arc(&wt)
            .args(["verify", "--all", "--rerun"])
            .assert()
            .success();
        let shown = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
        let checks = &shown["verifications"].as_array().unwrap().last().unwrap()["check_timings"];
        if mode == "capped" {
            assert_eq!(checks["checks"].as_array().unwrap().len(), 32, "{checks}");
            assert_eq!(checks["dropped"], 9);
            assert_eq!(checks["checks"][0]["name"], "caught");
        } else {
            assert!(checks["error"].is_string(), "{mode}: {checks}");
            assert!(
                checks["checks"].as_array().unwrap().is_empty(),
                "{mode}: {checks}"
            );
        }
    }
}
