//! Integration edges: what a target checkout may carry, what an
//! already-contained head closes as, and where a merge may run.

use crate::common::*;

fn repo_with_gates() -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.smoke]\ncommand = \"test -f README.md\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare gates"]);
    repo
}

/// A change with one commit, a recorded patchset, green gates, and an
/// approving verdict: nothing but the merge is left.
///
/// The commit force-adds its path, so a change can carry a file that ignore
/// rules in the shared repository would otherwise keep out of the index.
fn approved_change(repo: &Repo, slug: &str, file: &str, content: &str) -> (String, PathBuf) {
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    if let Some(parent) = Path::new(file).parent() {
        fs::create_dir_all(worktree.join(parent)).unwrap();
    }
    fs::write(worktree.join(file), content).unwrap();
    git(&worktree, &["add", "-f", "--", file]);
    git(&worktree, &["commit", "-m", &format!("feat: {slug}")]);
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&worktree)
        .args(["verify", slug, "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    (change_id, worktree)
}

#[test]
fn unrelated_untracked_and_ignored_files_survive_integration() {
    let repo = repo_with_gates();
    fs::write(repo.root.join(".gitignore"), "build/\n").unwrap();
    git(&repo.root, &["add", ".gitignore"]);
    git(&repo.root, &["commit", "-m", "test: ignore build output"]);
    let (_change, _wt) = approved_change(&repo, "archive-beside", "feature.txt", "feature\n");

    // The observed case: a deliberate local archive at the checkout root,
    // plus ignored build output. Neither is part of the change.
    let archive = repo.root.join("local-packages.zip");
    fs::write(&archive, b"archive bytes nobody committed\n").unwrap();
    fs::create_dir_all(repo.root.join("build")).unwrap();
    let ignored = repo.root.join("build/output.bin");
    fs::write(&ignored, b"ignored build output\n").unwrap();
    let old_master = repo.head(&repo.root);

    let out = stdout(repo.arc(&repo.root).args(["integrate", "archive-beside"]));
    assert!(
        out.contains("local-packages.zip") && out.contains("build/"),
        "the preflight names the untracked paths it leaves:\n{out}"
    );

    assert_eq!(
        fs::read(&archive).unwrap(),
        b"archive bytes nobody committed\n",
        "the archive keeps its bytes"
    );
    assert_eq!(
        fs::read(&ignored).unwrap(),
        b"ignored build output\n",
        "the ignored file keeps its bytes"
    );
    assert_eq!(
        git_out(&repo.root, &["status", "--porcelain", "local-packages.zip"]),
        "?? local-packages.zip"
    );
    let merged = repo.head(&repo.root);
    assert_ne!(merged, old_master);
    assert_eq!(
        git_out(&repo.root, &["rev-list", "--parents", "-n", "1", &merged])
            .split_whitespace()
            .count(),
        3
    );
}

#[test]
fn merge_writing_an_untracked_target_path_is_refused_by_name() {
    let repo = repo_with_gates();
    let (_change, _wt) = approved_change(&repo, "takes-name", "collide.txt", "from the change\n");
    let collide = repo.root.join("collide.txt");
    fs::write(&collide, b"precious local bytes\n").unwrap();
    let old_master = repo.head(&repo.root);

    repo.arc(&repo.root)
        .args(["integrate", "takes-name", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "untracked or ignored: collide.txt",
        ));

    repo.arc(&repo.root)
        .args(["integrate", "takes-name"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "untracked or ignored: collide.txt",
        ));

    assert_eq!(repo.head(&repo.root), old_master, "nothing merged");
    assert_eq!(fs::read(&collide).unwrap(), b"precious local bytes\n");
}

#[test]
fn an_ignored_file_the_merge_writes_is_refused_by_name() {
    let repo = repo_with_gates();
    fs::write(repo.root.join(".gitignore"), "build/\n").unwrap();
    git(&repo.root, &["add", ".gitignore"]);
    git(&repo.root, &["commit", "-m", "test: ignore build output"]);
    let (_change, _wt) = approved_change(
        &repo,
        "takes-ignored",
        "build/output.bin",
        "from the change\n",
    );
    fs::create_dir_all(repo.root.join("build")).unwrap();
    let ignored = repo.root.join("build/output.bin");
    fs::write(&ignored, b"ignored bytes\n").unwrap();
    let old_master = repo.head(&repo.root);

    repo.arc(&repo.root)
        .args(["integrate", "takes-ignored", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "untracked or ignored: build/output.bin",
        ));

    assert_eq!(repo.head(&repo.root), old_master, "nothing merged");
    assert_eq!(fs::read(&ignored).unwrap(), b"ignored bytes\n");
}

#[test]
fn merge_through_an_untracked_parent_is_refused_by_name() {
    let repo = repo_with_gates();
    let (_change, _wt) = approved_change(&repo, "nested-write", "a/b.txt", "nested\n");
    let blocker = repo.root.join("a");
    fs::write(&blocker, b"a file where the merge needs a directory\n").unwrap();

    repo.arc(&repo.root)
        .args(["integrate", "nested-write"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("untracked or ignored: a"));

    assert_eq!(
        fs::read(&blocker).unwrap(),
        b"a file where the merge needs a directory\n"
    );
    assert!(!repo.root.join("a/b.txt").exists());
}

#[test]
fn tracked_modifications_refuse_by_their_own_reason() {
    let repo = repo_with_gates();
    let (_change, _wt) = approved_change(&repo, "dirty-target", "feature.txt", "feature\n");
    let old_master = repo.head(&repo.root);

    // An unstaged edit to a tracked file.
    fs::write(repo.root.join("README.md"), "edited\n").unwrap();
    repo.arc(&repo.root)
        .args(["integrate", "dirty-target"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("tracked modifications"));
    git(&repo.root, &["checkout", "--", "README.md"]);
    // A staged addition is tracked dirt too.
    fs::write(repo.root.join("staged.txt"), "staged\n").unwrap();
    git(&repo.root, &["add", "staged.txt"]);
    repo.arc(&repo.root)
        .args(["integrate", "dirty-target", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("tracked modifications"));

    assert_eq!(repo.head(&repo.root), old_master, "nothing merged");
}
