use crate::common::*;

fn repo_with_trivial_gates() -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.build]\ncommand = \"true\"\n[gates.test]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "gates"]);
    repo
}

#[test]
fn skip_green_skips_only_at_matching_head_and_reruns_after_a_commit() {
    let repo = repo_with_trivial_gates();
    let (_id, wt, _head) = change_with_patchset(&repo, "feat-x");

    // Nothing is green yet: both gates run.
    let first = stdout(
        repo.arc(&wt)
            .args(["verify", "feat-x", "--all", "--skip-green"]),
    );
    assert!(first.contains("gates: 2/2 pass"), "{first}");
    assert!(!first.contains("skipped"), "{first}");

    // Re-run at the same head: both are green and skipped.
    let second = stdout(
        repo.arc(&wt)
            .args(["verify", "feat-x", "--all", "--skip-green"]),
    );
    assert!(
        second.contains("build: skipped (green at head)"),
        "{second}"
    );
    assert!(second.contains("test: skipped (green at head)"), "{second}");
    assert!(second.contains("gates: 2/2 pass"), "{second}");

    // A new commit moves the head, so the gates run again.
    repo.commit(&wt, "feat-x.txt", "more\n", "feat: more");
    stdout(repo.arc(&wt).args(["snapshot", "feat-x"]));
    let third = stdout(
        repo.arc(&wt)
            .args(["verify", "feat-x", "--all", "--skip-green"]),
    );
    assert!(
        !third.contains("skipped"),
        "should rerun after commit:\n{third}"
    );
    assert!(third.contains("gates: 2/2 pass"), "{third}");
}

#[test]
fn skip_green_requires_all() {
    let repo = repo_with_trivial_gates();
    change_with_patchset(&repo, "feat-x");
    repo.arc(&repo.root)
        .args(["verify", "feat-x", "--gate", "build", "--skip-green"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--skip-green requires --all"));
}
