use crate::common::*;

fn declaring(text: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(repo.root.join(".arc/gates.toml"), text).unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare gates"]);
    repo
}

fn events(repo: &Repo, wt: &Path, change: &str, event_type: &str) -> Vec<serde_json::Value> {
    stdout(repo.arc(wt).args(["events", "--change", change]))
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["event_type"] == event_type)
        .collect()
}

fn gate<'a>(status: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    status["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|gate| gate["name"] == name)
        .unwrap()
}

/// Claim: a gate command exiting 75 within its timeout records `not-admitted`,
/// which blocks integration as a gate that did not run, not as one that
/// failed, and which `--skip-green` never reuses. Rejects recording the exit
/// as a failing test, as arc did before the convention was read.
#[test]
fn exit_75_records_not_admitted_and_blocks_integration_as_unrun() {
    let repo = declaring("[gates.heavy]\ncommand = \"echo queue full; exit 75\"\n");
    let (id, wt, head) = change_with_patchset(&repo, "queued");
    let verified = stdout_any_status(repo.arc(&wt).args(["verify", "--gate", "heavy"]));
    assert!(
        verified.contains("verification: NotAdmitted at"),
        "{verified}"
    );
    assert!(verified.contains("before any check ran"), "{verified}");

    let recorded = events(&repo, &wt, &id, "verification-recorded");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["result"], "not-admitted");
    assert_eq!(recorded[0]["exit_code"], 75);
    assert_eq!(recorded[0]["revision"], head.as_str());

    let status = json_stdout(repo.arc(&wt).args(["status"]));
    let heavy = gate(&status, "heavy");
    assert_eq!(heavy["result"], "not-admitted", "{status}");
    assert_eq!(heavy["green_at_head"], false, "{status}");
    assert_eq!(status["next_action"], "run_gate:heavy", "{status}");

    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    let check = stdout_any_status(repo.arc(&wt).args(["check"]));
    assert!(check.contains("was not admitted"), "{check}");
    assert!(!check.contains("the gate failed"), "{check}");
    repo.arc(&wt).args(["check"]).assert().code(5);
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .code(5);

    let rerun = stdout_any_status(repo.arc(&wt).args(["verify", "--all", "--skip-green"]));
    assert!(!rerun.contains("skipped"), "{rerun}");
    assert_eq!(events(&repo, &wt, &id, "verification-recorded").len(), 2);
}

/// Claim: a timeout outranks the exit status, so a command that exits 75 and
/// is still holding its output when the timeout elapses timed out. Rejects
/// reading the exit status before the timeout.
#[test]
fn exit_75_after_a_timeout_is_a_timeout() {
    let repo = declaring("[gates.heavy]\ncommand = \"sleep 5 & exit 75\"\ntimeout = \"1s\"\n");
    let (id, wt, _) = change_with_patchset(&repo, "overrun");
    repo.arc(&wt)
        .args(["verify", "--gate", "heavy"])
        .assert()
        .code(1);
    let recorded = events(&repo, &wt, &id, "verification-recorded");
    assert_eq!(recorded[0]["result"], "fail", "{recorded:?}");
    assert_eq!(recorded[0]["timed_out"], true, "{recorded:?}");
    let status = json_stdout(repo.arc(&wt).args(["status"]));
    assert_eq!(gate(&status, "heavy")["timed_out"], true, "{status}");
}
