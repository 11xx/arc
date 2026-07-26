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

#[test]
fn doctor_groups_advice_and_ignores_closed_claims() {
    let repo = Repo::new();
    let expired_claim = |slug: &str| {
        let opened = stdout(repo.arc(&repo.root).args(["begin", slug, "--no-worktree"]));
        let change_id = opened_change_id(&opened);
        repo.arc(&repo.root)
            .args(["claim", slug, "--ttl", "1s"])
            .assert()
            .success();
        age_event(&repo, &change_id, "claim-set", 5);
        change_id
    };
    let first = expired_claim("doctor-open-one");
    let second = expired_claim("doctor-open-two");
    let closed = expired_claim("doctor-closed");
    repo.arc(&repo.root)
        .args(["close", "doctor-closed", "--abandoned"])
        .assert()
        .success();

    let default = stdout(repo.arc(&repo.root).arg("doctor"));
    assert_eq!(
        default.matches("long-expired-claim").count(),
        1,
        "{default}"
    );
    assert!(
        default.contains(
            "long-expired-claim: 2 open changes have claims expired for more than one TTL; \
             run arc doctor --verbose to identify them"
        ),
        "{default}"
    );
    assert!(!default.contains(&first), "{default}");
    assert!(!default.contains(&second), "{default}");
    assert!(!default.contains(&closed), "{default}");

    let verbose = stdout(repo.arc(&repo.root).args(["doctor", "--verbose"]));
    assert_eq!(
        verbose.matches("long-expired-claim").count(),
        2,
        "{verbose}"
    );
    assert!(verbose.contains(&first), "{verbose}");
    assert!(verbose.contains(&second), "{verbose}");
    assert!(!verbose.contains(&closed), "{verbose}");

    let json = json_stdout(repo.arc(&repo.root).args(["doctor", "--json"]));
    let claims = json["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["code"] == "long-expired-claim")
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 2);
    assert!(claims
        .iter()
        .all(|finding| !finding["detail"].as_str().unwrap().contains(&closed)));

    repo.arc(&repo.root)
        .args(["doctor", "--verbose", "--json"])
        .assert()
        .failure()
        .code(2);
}
