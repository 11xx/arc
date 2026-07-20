use crate::common::*;

fn repo_with_self_approval_policy() -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);
    repo
}

#[test]
fn on_behalf_of_round_trips_through_status_json() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "feat-x"]));
    let wt = repo.home.join(".worktrees").join("repo-feat-x");
    repo.commit(&wt, "feat-x.txt", "x\n", "feat: x");
    // A lead snapshots on behalf of an executor who authored the work.
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .args(["snapshot", "feat-x", "--on-behalf-of", "Executor"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "feat-x"]));
    assert_eq!(status["latest_patchset"]["actor"], "Lead");
    assert_eq!(status["latest_patchset"]["on_behalf_of"], "Executor");
}

#[test]
fn lead_snapshot_then_lead_approval_is_not_self_approval() {
    let repo = repo_with_self_approval_policy();
    stdout(repo.arc(&repo.root).args(["begin", "feat-x"]));
    let wt = repo.home.join(".worktrees").join("repo-feat-x");
    repo.commit(&wt, "feat-x.txt", "x\n", "feat: x");
    // Lead snapshots for the executor, then approves as itself: distinct
    // effective authors (Executor vs Lead), so policy permits it.
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .args(["snapshot", "feat-x", "--on-behalf-of", "Executor"])
        .assert()
        .success();
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt).args(["check", "feat-x"]).assert().success();
}

#[test]
fn approval_on_behalf_of_the_snapshot_subject_is_self_approval() {
    let repo = repo_with_self_approval_policy();
    stdout(repo.arc(&repo.root).args(["begin", "feat-x"]));
    let wt = repo.home.join(".worktrees").join("repo-feat-x");
    repo.commit(&wt, "feat-x.txt", "x\n", "feat: x");
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .args(["snapshot", "feat-x", "--on-behalf-of", "Executor"])
        .assert()
        .success();
    // Approving on behalf of the same executor makes both effective authors
    // Executor: that is self-approval and the policy rejects it.
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .args([
            "review",
            "feat-x",
            "--verdict",
            "approved",
            "--on-behalf-of",
            "Executor",
        ])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["check", "feat-x"])
        .assert()
        .code(3)
        .stdout(predicates::str::contains(
            "approval rejected by policy: self-approval",
        ));
}

#[test]
fn claims_match_ownership_by_invoker_not_subject() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "feat-x"]));
    let wt = repo.home.join(".worktrees").join("repo-feat-x");
    // A lead claims on behalf of an executor; ownership is the invoker tuple.
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", "lead-session")
        .args(["claim", "feat-x", "--on-behalf-of", "Executor"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "feat-x"]));
    assert_eq!(status["claim"]["owner"]["actor"], "Lead");
    assert_eq!(status["claim"]["owner"]["session"], "lead-session");

    // The same invoker tuple may release its own claim.
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", "lead-session")
        .args(["release-claim", "feat-x"])
        .assert()
        .success();
}
