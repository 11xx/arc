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

fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
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
/// prints wall, CPU and spread, and leaves ledger and output artifacts untouched.
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
