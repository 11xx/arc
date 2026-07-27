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

pub(crate) fn doctor_groups_advice_and_ignores_closed_claims() {
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

pub(crate) fn doctor_reports_closed_registered_worktrees_without_removing_them() {
    let repo = Repo::new();
    let close_with_worktree = |slug: &str| {
        let opened = stdout(repo.arc(&repo.root).args(["begin", slug]));
        let change_id = opened_change_id(&opened);
        let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
        repo.arc(&repo.root)
            .args(["close", slug, "--abandoned"])
            .assert()
            .success();
        (change_id, worktree)
    };
    let (first_id, first_path) = close_with_worktree("doctor-closed-one");
    let (second_id, second_path) = close_with_worktree("doctor-closed-two");
    let (removed_id, removed_path) = close_with_worktree("doctor-closed-removed");
    git(
        &repo.root,
        &["worktree", "remove", removed_path.to_str().unwrap()],
    );

    let registrations_before = git_out(&repo.root, &["worktree", "list", "--porcelain"]);
    let default = stdout(repo.arc(&repo.root).arg("doctor"));
    assert!(
        default.contains(
            "closed-change-worktree: 2 registered worktrees belong to closed changes; \
             run arc doctor --verbose to list change/path pairs; remove only with \
             git worktree remove <path>"
        ),
        "{default}"
    );
    assert!(!default.contains(&first_id), "{default}");
    assert!(!default.contains(&second_id), "{default}");
    assert!(!default.contains(&removed_id), "{default}");

    let verbose = stdout(repo.arc(&repo.root).args(["doctor", "--verbose"]));
    for (change_id, path) in [(&first_id, &first_path), (&second_id, &second_path)] {
        assert!(
            verbose.contains(&format!(
                "closed-change-worktree: {change_id} [abandoned]: {}",
                path.display()
            )),
            "{verbose}"
        );
    }
    assert!(!verbose.contains(&removed_id), "{verbose}");
    assert!(first_path.is_dir());
    assert!(second_path.is_dir());
    assert_eq!(
        git_out(&repo.root, &["worktree", "list", "--porcelain"]),
        registrations_before
    );
}
