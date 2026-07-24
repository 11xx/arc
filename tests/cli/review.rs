use super::common::*;

#[test]
fn read_view_prints_verdict_history_and_body() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "review-read");
    repo.arc(&worktree)
        .args([
            "review",
            "review-read",
            "--verdict",
            "approved",
            "--body",
            "The implementation is sound.",
        ])
        .assert()
        .success();

    repo.arc(&worktree)
        .args(["review", "review-read"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Approved on `ps-01`"))
        .stdout(predicates::str::contains("The implementation is sound."));
}

#[test]
fn read_view_plainly_reports_no_verdict() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "review-empty");

    repo.arc(&worktree)
        .args(["review", "review-empty"])
        .assert()
        .success()
        .stdout(predicates::str::contains("No verdicts recorded."))
        .stdout(predicates::str::contains(
            "Valid approval for current head: no",
        ));
}

#[test]
fn read_view_marks_approval_stale_after_new_snapshot() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "review-stale");
    repo.arc(&worktree)
        .args(["review", "review-stale", "--verdict", "approved"])
        .assert()
        .success();
    repo.commit(
        &worktree,
        "later.txt",
        "later\n",
        "test: add later revision",
    );
    stdout(repo.arc(&worktree).args(["snapshot", "review-stale"]));

    repo.arc(&worktree)
        .args(["review", "review-stale"])
        .assert()
        .success()
        .stdout(predicates::str::contains("STALE for current head"));
}

#[test]
fn read_view_json_has_versioned_schema() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "review-json");
    let output = repo
        .arc(&worktree)
        .args(["review", "review-json", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["schema"], "arc-review/1");
}

#[test]
fn review_write_path_still_records_a_verdict() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "review-write");

    repo.arc(&worktree)
        .args(["review", "review-write", "--verdict", "approved"])
        .assert()
        .success()
        .stdout(predicates::str::contains("verdict: Approved on ps-01"));
}
