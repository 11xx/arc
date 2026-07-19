use super::common::*;

#[test]
fn doctor_clean_ledger_exits_zero() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("problems:\n  (none)"));
    repo.arc(&repo.root)
        .args(["doctor", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"schema\":\"arc-doctor/1\""));
}

#[test]
fn doctor_reports_malformed_event_as_problem() {
    let repo = Repo::new();
    let output = stdout(
        repo.arc(&repo.root)
            .args(["begin", "doctor-bad-event", "--no-worktree"]),
    );
    let change_id = opened_change_id(&output);
    fs::write(event_dir(&repo, &change_id).join("BAD.json"), b"not json\n").unwrap();

    repo.arc(&repo.root)
        .args(["doctor"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("malformed-event"));
}

#[test]
fn doctor_reports_orphaned_tmp_as_advice_without_failing() {
    let repo = Repo::new();
    let output = stdout(
        repo.arc(&repo.root)
            .args(["begin", "doctor-tmp", "--no-worktree"]),
    );
    let change_id = opened_change_id(&output);
    let temporary = event_dir(&repo, &change_id).join(".event.TEST.tmp");
    fs::write(&temporary, b"partial").unwrap();

    repo.arc(&repo.root)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("orphaned-temporary-file"));
    assert!(temporary.is_file(), "doctor must be read-only");
}
