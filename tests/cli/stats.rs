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
    // The same cause twice, plus a second cause on the later verdict: the
    // repeated one must collapse to its single round while the other still
    // registers, so a per-verdict tally cannot pass this.
    repo.arc(&worktree)
        .args([
            "review",
            "piled-up",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "review",
            "piled-up",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
            "--cause",
            "brief",
        ])
        .assert()
        .success();
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
    // Causes are attributed to the round, so the repeated one counts once and
    // no cause tally can exceed the round count it explains.
    assert_eq!(change["review_rounds_by_cause"]["executor"], 1);
    assert_eq!(change["review_rounds_by_cause"]["brief"], 1);
    assert_eq!(report["aggregate"]["completed_rework_rounds"], 1);
    assert_eq!(report["aggregate"]["review_rounds_by_cause"]["executor"], 1);
}

/// `arc stats` knows a change took six rework rounds and not who caused them.
/// The identity is already on the ledger — a lead runs the ceremony on an
/// executor's behalf — so the rows are keyed on the subject, never the actor.
#[test]
fn stats_by_model_attributes_patchsets_and_rework_to_the_subject() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "delegated"]));
    let wt = repo.home.join(".worktrees/repo-delegated");

    // Round one: the executor's work is sent back.
    repo.commit(&wt, "one.rs", "first\n", "feat: first");
    repo.arc(&wt)
        .args(["snapshot", "delegated", "--on-behalf-of", "sol#high"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "review",
            "delegated",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
            "--on-behalf-of",
            "reviewer-model",
        ])
        .assert()
        .success();

    // Round two: a different identity writes the revision that answers it.
    repo.commit(&wt, "two.rs", "second\n", "feat: second");
    repo.arc(&wt)
        .args(["snapshot", "delegated", "--on-behalf-of", "terra#high"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "review",
            "delegated",
            "--verdict",
            "approved",
            "--on-behalf-of",
            "reviewer-model",
        ])
        .assert()
        .success();

    // A patchset nobody delegated for: counted apart, not credited to the lead.
    repo.commit(&wt, "three.rs", "third\n", "feat: third");
    repo.arc(&wt)
        .args(["snapshot", "delegated"])
        .assert()
        .success();

    let report = json_stdout(repo.arc(&wt).args(["stats", "--by-model", "--json"]));
    assert_eq!(report["schema"], "arc-stats-by-model/1");
    let rows = report["models"].as_array().unwrap();
    let row = |identity: &str| {
        rows.iter()
            .find(|row| row["identity"] == identity)
            .unwrap_or_else(|| panic!("no row for {identity}: {report}"))
            .clone()
    };

    // The round is charged to the work that was sent back, not to the
    // revision that answered it.
    let executor = row("sol#high");
    assert_eq!(executor["patchsets"], 1, "{report}");
    assert_eq!(executor["rework_rounds_caused"], 1, "{report}");
    assert_eq!(executor["verdicts"], 0, "{report}");
    assert_eq!(executor["changes"], 1, "{report}");

    let fixer = row("terra#high");
    assert_eq!(fixer["patchsets"], 1, "{report}");
    assert_eq!(fixer["rework_rounds_caused"], 0, "{report}");

    let reviewer = row("reviewer-model");
    assert_eq!(reviewer["verdicts"], 2, "{report}");
    assert_eq!(reviewer["patchsets"], 0, "{report}");
    assert_eq!(reviewer["rework_rounds_caused"], 0, "{report}");

    let unknown = row("(unattributed)");
    assert_eq!(unknown["patchsets"], 1, "{report}");

    // An identity that only filed a finding still has a row.
    repo.arc(&wt)
        .args([
            "finding",
            "delegated",
            "--summary",
            "spotted",
            "--on-behalf-of",
            "finder-model",
        ])
        .assert()
        .success();
    let report = json_stdout(repo.arc(&wt).args(["stats", "--by-model", "--json"]));
    let rows = report["models"].as_array().unwrap();
    let finder = rows
        .iter()
        .find(|row| row["identity"] == "finder-model")
        .unwrap_or_else(|| panic!("no row for finder-model: {report}"));
    assert_eq!(finder["changes"], 1, "{report}");
    assert_eq!(finder["patchsets"], 0, "{report}");

    let text = stdout(repo.arc(&wt).args(["stats", "--by-model"]));
    assert!(text.contains("sol#high"), "{text}");
    assert!(text.contains("(unattributed)"), "{text}");

    // A selection and --all cannot both be asked for, however --change is
    // spelled.
    repo.arc(&wt)
        .args(["--change", "delegated", "stats", "--by-model", "--all"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be combined"));
}
