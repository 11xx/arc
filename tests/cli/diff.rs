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

#[test]
fn diff_between_and_since_approved_render_only_the_review_delta() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "interdiff-view"]));
    let wt = repo.home.join(".worktrees/repo-interdiff-view");
    repo.commit(&wt, "first.txt", "first\n", "feat: first review");
    stdout(repo.arc(&wt).args(["snapshot", "interdiff-view"]));
    repo.arc(&wt)
        .args(["review", "interdiff-view", "--verdict", "approved"])
        .assert()
        .success();
    repo.commit(&wt, "second.txt", "second\n", "feat: review delta");
    stdout(repo.arc(&wt).args(["snapshot", "interdiff-view"]));
    repo.arc(&wt)
        .args(["diff", "interdiff-view", "--between", "ps-01", "ps-02"])
        .assert()
        .success()
        .stdout(predicates::str::contains("+second"))
        .stdout(predicates::str::contains("first.txt").not());
    repo.arc(&wt)
        .args(["diff", "interdiff-view", "--since-approved"])
        .assert()
        .success()
        .stdout(predicates::str::contains("+second"));
}

#[test]
fn snapshot_after_rebase_records_the_current_merge_base() {
    let repo = Repo::new();
    let original_base = repo.head(&repo.root);
    stdout(repo.arc(&repo.root).args(["begin", "rebased-base"]));
    let wt = repo.home.join(".worktrees/repo-rebased-base");
    repo.commit(&wt, "change.txt", "change\n", "feat: add change");
    repo.commit(
        &repo.root,
        "upstream.txt",
        "upstream\n",
        "feat: advance target",
    );
    git(&wt, &["rebase", "master"]);
    stdout(repo.arc(&wt).args(["snapshot", "rebased-base"]));

    let state: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&wt).args([
        "show",
        "rebased-base",
        "--json",
    ])))
    .unwrap();
    let patchset = state["patchsets"].as_array().unwrap().last().unwrap();
    let merge_base = git_out(&wt, &["merge-base", "HEAD", "master"]);
    assert_eq!(patchset["base"], merge_base);
    assert_eq!(patchset["merge_base"], merge_base);
    assert_ne!(patchset["base"], original_base);
}

#[test]
fn diff_after_rebase_renders_only_the_change() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "rebased-diff"]));
    let wt = repo.home.join(".worktrees/repo-rebased-diff");
    repo.commit(&wt, "change.txt", "change\n", "feat: add change");
    repo.commit(
        &repo.root,
        "upstream.txt",
        "upstream\n",
        "feat: advance target",
    );
    git(&wt, &["rebase", "master"]);
    stdout(repo.arc(&wt).args(["snapshot", "rebased-diff"]));

    repo.arc(&wt)
        .args(["diff", "rebased-diff"])
        .assert()
        .success()
        .stdout(predicates::str::contains("change.txt"))
        .stdout(predicates::str::contains("upstream.txt").not());
}

#[test]
fn snapshot_without_rebase_keeps_the_original_base_behavior() {
    let repo = Repo::new();
    let original_base = repo.head(&repo.root);
    stdout(repo.arc(&repo.root).args(["begin", "steady-base"]));
    let wt = repo.home.join(".worktrees/repo-steady-base");
    repo.commit(&wt, "change.txt", "change\n", "feat: add change");
    stdout(repo.arc(&wt).args(["snapshot", "steady-base"]));

    let state: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&wt).args([
        "show",
        "steady-base",
        "--json",
    ])))
    .unwrap();
    let patchset = state["patchsets"].as_array().unwrap().last().unwrap();
    assert_eq!(patchset["base"], original_base);
    repo.arc(&wt)
        .args(["diff", "steady-base"])
        .assert()
        .success()
        .stdout(predicates::str::contains("change.txt"));
}
