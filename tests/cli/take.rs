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

#[test]
fn tagged_take_selects_the_chain_views_next_ready_member() {
    let repo = Repo::new();
    let low = stdout(repo.arc(&repo.root).args([
        "begin",
        "take-tag-low",
        "--no-worktree",
        "--tag",
        "program",
    ]));
    let low_id = opened_change_id(&low);
    let high = stdout(repo.arc(&repo.root).args([
        "begin",
        "take-tag-high",
        "--no-worktree",
        "--tag",
        "program",
    ]));
    let high_id = opened_change_id(&high);
    repo.arc(&repo.root)
        .args(["metadata", &low_id, "--priority", "10"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["metadata", &high_id, "--priority", "20"])
        .assert()
        .success();

    let chain: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["chain", "program", "--json"]),
    ))
    .unwrap();
    assert_eq!(chain["next_ready"], high_id);
    repo.arc(&repo.root)
        .args(["take", "--tag", "program"])
        .assert()
        .success()
        .stdout(format!("{high_id}\n"));
}
