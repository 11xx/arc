use super::common::*;

#[test]
fn take_claims_ready_work_by_priority_and_skips_blocked_or_held_changes() {
    let repo = Repo::new();
    let low = stdout(
        repo.arc(&repo.root)
            .args(["begin", "take-low", "--no-worktree"]),
    );
    let low_id = opened_change_id(&low);
    let high = stdout(
        repo.arc(&repo.root)
            .args(["begin", "take-high", "--no-worktree"]),
    );
    let high_id = opened_change_id(&high);
    let prerequisite =
        stdout(
            repo.arc(&repo.root)
                .args(["begin", "take-prerequisite", "--no-worktree"]),
        );
    let prerequisite_id = opened_change_id(&prerequisite);
    stdout(repo.arc(&repo.root).args([
        "begin",
        "take-blocked",
        "--no-worktree",
        "--blocked-by",
        &prerequisite_id,
    ]));
    repo.arc(&repo.root)
        .args(["hold", "take-prerequisite", "--reason", "wait"])
        .assert()
        .success();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "take-held", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["hold", "take-held", "--reason", "wait"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args(["metadata", "take-low", "--priority", "10"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["metadata", "take-high", "--priority", "20"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["claim", "take-high", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    age_event(&repo, &high_id, "claim-set", 5);

    repo.arc(&repo.root)
        .args(["take"])
        .assert()
        .success()
        .stdout(format!("{high_id}\n"));
    repo.arc(&repo.root)
        .args(["take"])
        .assert()
        .success()
        .stdout(format!("{low_id}\n"));
    repo.arc(&repo.root).args(["take"]).assert().code(2);
}
