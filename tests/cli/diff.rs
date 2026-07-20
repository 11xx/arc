use super::common::*;

#[test]
fn diff_renders_recorded_hunks_stats_pathspecs_and_snapshot_guidance() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "diff-view"]));
    let wt = repo.home.join(".worktrees/repo-diff-view");
    repo.commit(&wt, "changed.txt", "after\n", "feat: change file");
    stdout(repo.arc(&wt).args(["snapshot", "diff-view"]));

    repo.arc(&wt)
        .args(["diff", "diff-view", "--", "changed.txt"])
        .assert()
        .success()
        .stdout(predicates::str::contains("+after"));
    repo.arc(&wt)
        .args(["diff", "diff-view", "--stat"])
        .assert()
        .success()
        .stdout(predicates::str::contains("changed.txt"));

    stdout(
        repo.arc(&repo.root)
            .args(["begin", "no-snapshot", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["diff", "no-snapshot"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no snapshot recorded"))
        .stderr(predicates::str::contains("arc snapshot"));
}

#[test]
fn diff_findings_marks_changed_blobs_drifted_and_unchanged_blobs_anchored() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "anchor-view"]));
    let wt = repo.home.join(".worktrees/repo-anchor-view");
    repo.commit(&wt, "drift.txt", "one\n", "feat: add drifted file");
    repo.commit(&wt, "steady.txt", "steady\n", "feat: add steady file");
    stdout(repo.arc(&wt).args(["snapshot", "anchor-view"]));
    repo.arc(&wt)
        .args([
            "finding",
            "anchor-view",
            "--summary",
            "drifts",
            "--path",
            "drift.txt",
            "--line",
            "1",
        ])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "finding",
            "anchor-view",
            "--summary",
            "stays put",
            "--path",
            "steady.txt",
            "--line",
            "1",
        ])
        .assert()
        .success();
    repo.commit(&wt, "drift.txt", "two\n", "fix: change anchored file");
    stdout(repo.arc(&wt).args(["snapshot", "anchor-view"]));

    repo.arc(&wt)
        .args(["diff", "anchor-view", "--findings"])
        .assert()
        .success()
        .stdout(predicates::str::contains("[drifted] drift.txt:1-1"))
        .stdout(predicates::str::contains("[anchored] steady.txt:1-1"));
}
