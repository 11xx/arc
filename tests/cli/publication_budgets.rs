use crate::common::*;
use serde_json::{json, Value};

fn declare(repo: &Repo, budget: &str, publication_only: bool) {
    repo.declare_gates_locally(&format!(
        "[gates.unit]\ncommand='true'\nbudget='{budget}'\n{}",
        if publication_only {
            "events=['publication']\n"
        } else {
            ""
        }
    ));
}

fn run(repo: &Repo, run_ms: u64, cached: bool, code: i32) -> String {
    let mut command = repo.arc(&repo.root);
    command
        .env("ARC_TEST_COMMAND_RUN_MS", run_ms.to_string())
        .args(["publication", "--target", "master"]);
    if cached {
        command.arg("--skip-green");
    }
    let output = command.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(code), "{text}");
    text
}

fn receipt(repo: &Repo) -> Value {
    let mut files = fs::read_dir(repo.root.join(".git/arc/repository/events"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect::<Vec<_>>();
    files.sort();
    files
        .into_iter()
        .rev()
        .map(|p| serde_json::from_slice::<Value>(&fs::read(p).unwrap()).unwrap())
        .find(|event| event["event_type"] == "publication-verified")
        .unwrap()
}

/// Claim: a fresh over-budget publication run keeps its raw pass but refuses
/// publication without moving the target and fences readers through format 8.
/// Rejects accepting exit zero as cost compliance or understating the format barrier.
#[test]
fn publication_budget_fresh_run_refuses_without_moving_target() {
    let repo = Repo::new();
    declare(&repo, "10s", true);
    let before = repo.head(&repo.root);
    let text = run(&repo, 11300, false, 1);
    assert!(text.contains("over budget, ran 11.3s of 10s"), "{text}");
    let raw = receipt(&repo);
    assert_eq!(raw["gates"]["unit"]["result"], "pass");
    assert_eq!(raw["gates"]["unit"]["run_ms"], 11300);
    assert_eq!(raw["gates"]["unit"]["execution_limit_ms"], 100000);
    assert!(raw["gates"]["unit"]["timeout_seconds"].is_null());
    assert_eq!(raw["budget_accepted"], false);
    assert_eq!(repo.head(&repo.root), before);
    let config: Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 9);
}

/// Claim: cached publication rejudges the newest matching receipt after a reduction,
/// preserving its timing without fallback or execution. Rejects frozen cost acceptance,
/// an older faster pass, and an implicit retry of an over-budget cached pass.
#[test]
fn publication_budget_cached_acceptance_rejudges_without_retry() {
    let repo = Repo::new();
    declare(&repo, "20s", true);
    run(&repo, 5000, false, 0);
    run(&repo, 11300, false, 0);
    let original = receipt(&repo)["gates"]["unit"].clone();
    declare(&repo, "10s", true);
    let before = repo.head(&repo.root);
    for _ in 0..2 {
        let text = run(&repo, 1, true, 1);
        assert!(
            text.contains("skipped (publication pass at target tree)"),
            "{text}"
        );
        assert!(text.contains("over budget, ran 11.3s of 10s"), "{text}");
        assert_eq!(receipt(&repo)["gates"]["unit"], original);
        assert_eq!(receipt(&repo)["budget_accepted"], false);
    }
    assert_eq!(repo.head(&repo.root), before);
}

/// Claim: publication-only budgets are outside the integration total. Rejects
/// charging their declarations to that total while still requiring their own cost limit,
/// and losing the format barrier when a transported publication has only a total.
#[test]
fn publication_budget_does_not_consume_the_integration_total() {
    for budget in ["budget='20s'\n", ""] {
        let repo = Repo::new();
        repo.declare_gates_locally(&format!(
            "[budget]\ntotal='1s'\n[gates.unit]\ncommand='true'\nevents=['publication']\n{budget}"
        ));
        run(&repo, 11300, false, 0);
        let id = begin_without_checkout(&repo, "publication-total-transport", &[]);
        let file = repo.home.join("publication-total.json");
        repo.arc(&repo.root)
            .args(["export", &id, "--output", file.to_str().unwrap()])
            .assert()
            .success();
        let receiver = Repo::new();
        deliver_git_objects(&repo, &receiver);
        receiver
            .arc(&receiver.root)
            .args(["import", file.to_str().unwrap()])
            .assert()
            .success();
        let config: Value =
            serde_json::from_slice(&fs::read(receiver.root.join(".git/arc/config.json")).unwrap())
                .unwrap();
        assert_eq!(
            config["schema_version"], 9,
            "publication total import did not adopt budgets"
        );
    }
}

/// Claim: reductions preserve a closed change's recorded acceptance while current
/// publication refuses the old receipt. Rejects rewriting closure history or inheriting
/// its historical cost acceptance for current publication.
#[test]
fn publication_budget_reduction_preserves_integrated_history() {
    let repo = Repo::new();
    declare(&repo, "20s", false);
    let (id, wt, _) = change_with_patchset(&repo, "publication-history");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "11300")
        .args(["verify", "--all"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["integrate", &id]).assert().success();
    let before = json_stdout(repo.arc(&repo.root).args(["show", &id, "--json"]))["closure"].clone();
    assert_eq!(before["authorization"]["gates"]["unit"]["budget_ms"], 20000);
    run(&repo, 11300, false, 0);
    declare(&repo, "10s", false);
    let text = run(&repo, 1, true, 1);
    assert!(text.contains("ran 11.3s of 10s"), "{text}");
    let after = json_stdout(repo.arc(&repo.root).args(["show", &id, "--json"]))["closure"].clone();
    assert_eq!(after, before);
    let explained = json_stdout(repo.arc(&repo.root).args(["explain", &id, "--json"]));
    assert_eq!(
        explained["historical_budget_authorization"]["gates"]["unit"]["budget_ms"],
        20000
    );
}

/// Claim: publication confirmation includes effective gate budgets and total rules
/// even though execution identity excludes them. Rejects a pass after policy changes
/// during the command without moving the checked revision or declaration identity.
#[test]
fn publication_budget_configuration_changes_refuse_acceptance() {
    for total in [false, true] {
        let repo = Repo::new();
        let policy = repo.root.join(".git/arc/operator-policy.toml");
        let marker = repo.home.join("change-budget.py");
        let command = format!("python3 '{}'", marker.display());
        let new = if total {
            "[budget]\ntotal='2s'\n".to_string()
        } else {
            format!("[gates.unit]\ncommand={command:?}\nevents=['publication']\nbudget='1ms'\n")
        };
        fs::write(
            &marker,
            format!(
                "from pathlib import Path\nPath({:?}).write_text({new:?})\n",
                policy.to_str().unwrap()
            ),
        )
        .unwrap();
        repo.redeclare_gates(&format!(
            "[gates.unit]\ncommand={command:?}\nevents=['publication']\nbudget='20s'\n"
        ));
        begin_without_checkout(&repo, "configuration-baseline", &[]);
        if total {
            repo.declare_gates_locally("[budget]\ntotal='1s'\n");
        }
        let before = repo.head(&repo.root);
        let text = run(&repo, 11300, false, 1);
        assert!(
            text.contains("publication gate declarations changed while checks ran"),
            "{text}"
        );
        assert_eq!(repo.head(&repo.root), before);
    }
}

/// Claim: a branch cannot authorize its higher budget; publication reads the
/// accepted integration baseline or the associated change's destination. Rejects
/// self-authorized higher declarations even for runs inside the old budget,
/// and an unrelated explicit change context.
#[test]
fn publication_budget_authority_comes_from_baseline_or_associated_change() {
    let repo = Repo::new();
    repo.redeclare_gates("[gates.unit]\ncommand='true'\nevents=['publication']\nbudget='10s'\n");
    repo.arc(&repo.root)
        .args(["publication", "--target", "master"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "publication budget authority is unavailable",
        ));
    let (id, wt, _) = change_with_patchset(&repo, "publication-authority");
    repo.commit(
        &wt,
        ".arc/gates.toml",
        "[gates.unit]\ncommand='true'\nevents=['publication']\nbudget='20s'\n",
        "test: declare higher branch budget",
    );
    let branch = git_out(&wt, &["branch", "--show-current"]);
    let before = repo.head(&wt);
    for explicit in [false, true] {
        let mut command = repo.arc(&repo.root);
        command
            .env("ARC_TEST_COMMAND_RUN_MS", "9000")
            .args(["publication", "--target", &branch]);
        if explicit {
            command.args(["--change", &id]);
        }
        command.assert().code(1).stderr(predicates::str::contains(
            "requires an applicable accepted raise record",
        ));
    }
    git(&repo.root, &["branch", "unassociated", &before]);
    git(&repo.root, &["checkout", "unassociated"]);
    repo.arc(&repo.root)
        .env("ARC_TEST_COMMAND_RUN_MS", "9000")
        .args(["publication", "--target", "unassociated"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(
            "requires an applicable accepted raise record",
        ));
    repo.arc(&repo.root)
        .args(["publication", "--target", "master", "--change", &id])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "open change owning the target branch",
        ));
    assert_eq!(repo.head(&wt), before);
}

/// Claim: publication timing and its absence survive export/import, are judged at
/// the receiver, and remain readable. Rejects losing measurements in transport,
/// accepting untimed legacy receipts, or rerunning cached budget refusals.
#[test]
fn publication_budget_timing_and_absence_survive_export_import() {
    for timed in [true, false] {
        let source = Repo::new();
        declare(&source, "20s", true);
        run(&source, 11300, false, 0);
        if !timed {
            for entry in fs::read_dir(source.root.join(".git/arc/repository/events")).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                if event["event_type"] == "publication-verified" {
                    for key in ["run_ms", "budget_at_run", "hostname"] {
                        event["gates"]["unit"].as_object_mut().unwrap().remove(key);
                    }
                    event.as_object_mut().unwrap().remove("budget_accepted");
                    fs::write(path, serde_json::to_vec(&event).unwrap()).unwrap();
                }
            }
        }
        let id = begin_without_checkout(&source, "publication-transport", &[]);
        let file = source.home.join("publication-timing.json");
        source
            .arc(&source.root)
            .args(["export", &id, "--output", file.to_str().unwrap()])
            .assert()
            .success();
        let bundle: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        let exported = bundle["repository_events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["event_type"] == "publication-verified")
            .unwrap();
        assert_eq!(
            exported["gates"]["unit"]["run_ms"],
            if timed { json!(11300) } else { Value::Null }
        );
        let receiver = Repo::new();
        deliver_git_objects(&source, &receiver);
        receiver
            .arc(&receiver.root)
            .args(["import", file.to_str().unwrap()])
            .assert()
            .success();
        declare(&receiver, "10s", true);
        let text = run(&receiver, 1, true, 1);
        assert!(
            text.contains("skipped (publication pass at target tree)"),
            "{text}"
        );
        assert!(
            text.contains(if timed {
                "ran 11.3s of 10s"
            } else {
                "unmeasured: run_ms is missing"
            }),
            "{text}"
        );
        assert_eq!(
            receipt(&receiver)["gates"]["unit"]["run_ms"],
            if timed { json!(11300) } else { Value::Null }
        );
    }
}

/// Claim: publication's accepted baseline follows the destination recorded by an
/// integrated change, including --into. Rejects using its original, looser target
/// to authorize an unrecorded increase on an otherwise unassociated branch,
/// including an execution inside the old budget.
#[test]
fn publication_budget_baseline_uses_the_recorded_integration_destination() {
    let repo = Repo::new();
    repo.redeclare_gates("[gates.unit]\ncommand='true'\nevents=['publication']\nbudget='20s'\n");
    git(&repo.root, &["checkout", "-b", "release"]);
    repo.commit(
        &repo.root,
        ".arc/gates.toml",
        "[gates.unit]\ncommand='true'\nevents=['publication']\nbudget='10s'\n",
        "test: tighten release budget",
    );
    git(&repo.root, &["checkout", "master"]);
    let (id, wt, _) = change_with_patchset(&repo, "publication-into");
    git(&wt, &["rebase", "release"]);
    repo.arc(&wt).args(["snapshot", &id]).assert().success();
    let source = repo.head(&wt);
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &id, "--into", "release"])
        .assert()
        .success();
    git(
        &repo.root,
        &["checkout", "-b", "publication-alias", &source],
    );
    repo.commit(
        &repo.root,
        ".arc/gates.toml",
        "[gates.unit]\ncommand='true'\nevents=['publication']\nbudget='20s'\n",
        "test: higher unaccepted publication budget",
    );
    git(&repo.root, &["checkout", "master"]);
    repo.arc(&repo.root)
        .env("ARC_TEST_COMMAND_RUN_MS", "9000")
        .args(["publication", "--target", "publication-alias"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(
            "requires an applicable accepted raise record",
        ));
}
