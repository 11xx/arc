use crate::common::*;

#[test]
fn stats_json_carries_schema_and_reports_selected_change() {
    let repo = Repo::new();
    let (_id, wt, _head) = change_with_patchset(&repo, "feat-x");
    repo.arc(&wt)
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "feat-x"])
        .assert()
        .success();

    let report = json_stdout(repo.arc(&repo.root).args(["stats", "--all", "--json"]));
    assert_eq!(report["schema"], "arc-stats/1");

    let changes = report["changes"].as_array().unwrap();
    let feat = changes
        .iter()
        .find(|change| change["slug"] == "feat-x")
        .expect("completed change should appear in stats");
    assert_eq!(feat["state"], "closed");
    // An integrated change has a measured open→integrated wall time.
    assert!(feat["wall_time_seconds"].is_number());
    assert_eq!(feat["patchset_count"], 1);
    assert!(report["aggregate"]["changes"].as_u64().unwrap() >= 1);
}

#[test]
fn rework_requires_changes_requested_then_new_patchset_then_approval() {
    let repo = Repo::new();

    let (_, first_pass, _) = change_with_patchset(&repo, "first-pass");
    repo.arc(&first_pass)
        .args(["review", "first-pass", "--verdict", "approved"])
        .assert()
        .success();

    let (_, reversal, _) = change_with_patchset(&repo, "same-patchset-reversal");
    repo.arc(&reversal)
        .args([
            "review",
            "same-patchset-reversal",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();
    repo.arc(&reversal)
        .args(["review", "same-patchset-reversal", "--verdict", "approved"])
        .assert()
        .success();

    let (_, reworked, _) = change_with_patchset(&repo, "two-rounds");
    repo.arc(&reworked)
        .args([
            "review",
            "two-rounds",
            "--verdict",
            "changes-requested",
            "--cause",
            "brief",
        ])
        .assert()
        .success();
    repo.commit(&reworked, "round-2.txt", "two\n", "fix: address round one");
    repo.arc(&reworked)
        .args(["snapshot", "two-rounds"])
        .assert()
        .success();
    repo.arc(&reworked)
        .args([
            "review",
            "two-rounds",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();
    repo.commit(
        &reworked,
        "round-3.txt",
        "three\n",
        "fix: address round two",
    );
    repo.arc(&reworked)
        .args(["snapshot", "two-rounds"])
        .assert()
        .success();
    repo.arc(&reworked)
        .args(["review", "two-rounds", "--verdict", "approved"])
        .assert()
        .success();

    let report = json_stdout(repo.arc(&repo.root).args(["stats", "--all", "--json"]));
    let changes = report["changes"].as_array().unwrap();
    let by_slug = |slug| {
        changes
            .iter()
            .find(|change| change["slug"] == slug)
            .unwrap()
    };

    assert_eq!(by_slug("first-pass")["changes_requested_rounds"], 0);
    assert_eq!(by_slug("first-pass")["completed_rework_rounds"], 0);
    assert_eq!(by_slug("first-pass")["reworked"], false);
    assert_eq!(by_slug("first-pass")["first_pass_approval"], true);

    assert_eq!(
        by_slug("same-patchset-reversal")["changes_requested_rounds"],
        1
    );
    assert_eq!(
        by_slug("same-patchset-reversal")["completed_rework_rounds"],
        0
    );
    assert_eq!(by_slug("same-patchset-reversal")["reworked"], false);
    assert_eq!(
        by_slug("same-patchset-reversal")["first_pass_approval"],
        false
    );

    assert_eq!(by_slug("two-rounds")["changes_requested_rounds"], 2);
    assert_eq!(by_slug("two-rounds")["completed_rework_rounds"], 2);
    assert_eq!(by_slug("two-rounds")["reworked"], true);
    assert_eq!(by_slug("two-rounds")["first_pass_approval"], false);

    assert_eq!(report["aggregate"]["changes_reworked"], 1);
    assert_eq!(report["aggregate"]["first_pass_approvals"], 1);
    assert_eq!(report["aggregate"]["completed_rework_rounds"], 2);
}

/// Reviewers add feedback in more than one sitting, so a patchset can collect
/// several changes-requested verdicts before the author answers. One revision
/// answers them all, so they are one round — counting verdict events instead
/// would inflate every rework figure a lead reads to judge delegation.
#[test]
fn several_changes_requested_on_one_patchset_are_one_round() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "piled-up");
    for cause in ["brief", "executor"] {
        repo.arc(&worktree)
            .args([
                "review",
                "piled-up",
                "--verdict",
                "changes-requested",
                "--cause",
                cause,
            ])
            .assert()
            .success();
    }
    repo.commit(&worktree, "answer.txt", "one\n", "fix: answer both rounds");
    repo.arc(&worktree)
        .args(["snapshot", "piled-up"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "piled-up", "--verdict", "approved"])
        .assert()
        .success();

    let report = json_stdout(repo.arc(&repo.root).args(["stats", "--all", "--json"]));
    let change = report["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["slug"] == "piled-up")
        .unwrap();
    assert_eq!(change["changes_requested_rounds"], 1);
    assert_eq!(change["completed_rework_rounds"], 1);
    assert_eq!(change["reworked"], true);
    assert_eq!(change["first_pass_approval"], false);
    assert_eq!(report["aggregate"]["completed_rework_rounds"], 1);
}
