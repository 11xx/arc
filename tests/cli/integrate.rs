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
    approved_change_blocked_by(repo, slug, file, content, &[])
}

fn approved_change_blocked_by(
    repo: &Repo,
    slug: &str,
    file: &str,
    content: &str,
    blocked_by: &[&str],
) -> (String, PathBuf) {
    let mut args = vec!["begin", slug, "--tag", "series"];
    for blocker in blocked_by {
        args.push("--blocked-by");
        args.push(blocker);
    }
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(&args)));
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

/// A verification-only change: its branch is `base` itself, it commits
/// nothing, and it records a patchset, gate evidence at that head, and an
/// approving verdict.
fn verification_only(
    repo: &Repo,
    slug: &str,
    base: &str,
    blocked_by: &[&str],
) -> (String, PathBuf) {
    let mut args = vec!["begin", slug, "--tag", "series", "--base", base];
    for blocker in blocked_by {
        args.push("--blocked-by");
        args.push(blocker);
    }
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(&args)));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
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
fn a_queue_closes_an_already_contained_slice_and_its_successor_proceeds() {
    let repo = repo_with_gates();
    // A verification-only slice shares its predecessor's head and is ordered
    // directly behind it, so the predecessor's merge is what contains it.
    let (first_id, first_wt) = approved_change(&repo, "contained-first", "first.txt", "first\n");
    let first_head = repo.head(&first_wt);
    let (verify_id, _) = verification_only(&repo, "contained-verify", &first_head, &[&first_id]);
    let (second_id, _) = approved_change_blocked_by(
        &repo,
        "contained-second",
        "second.txt",
        "second\n",
        &[&first_id],
    );
    let (tail_id, _) =
        approved_change_blocked_by(&repo, "contained-tail", "tail.txt", "tail\n", &[&verify_id]);

    let out = stdout(repo.arc(&repo.root).args(["integrate", "--tag", "series"]));
    assert!(
        out.contains("queue: 4 changes in dependency order"),
        "{out}"
    );
    for id in [&first_id, &verify_id, &second_id, &tail_id] {
        assert!(out.contains(&format!("landed: {id}")), "{out}");
    }

    let status: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "status",
        "contained-verify",
        "--json",
    ])))
    .unwrap();
    assert_eq!(status["state"], "closed");
    assert_eq!(status["closure"]["outcome"], "integrated");
    assert_eq!(status["closure"]["integration"], "guarded");

    // The containment is recorded, not invented: the closure names the
    // revision that already held the head, and no merge commit was made for
    // it.
    let event = events(&repo, &verify_id)
        .into_iter()
        .find(|event| event["event_type"] == "change-integrated")
        .expect("the verification-only slice records an integration");
    assert_eq!(event["already_contained"], true, "{event}");
    assert_eq!(event["source_head"], first_head.as_str());
    assert_eq!(event["target_before"], event["integrated_commit"]);
    let merges = git_out(
        &repo.root,
        &["log", "--first-parent", "--merges", "--format=%s"],
    );
    assert_eq!(merges.lines().count(), 3, "{merges}");
    assert!(
        !merges
            .lines()
            .any(|subject| subject.contains("contained-verify")),
        "the contained slice created no merge: {merges}"
    );
}

#[test]
fn a_head_equal_to_the_target_closes_at_that_revision() {
    let repo = repo_with_gates();
    let (first_id, _) = approved_change(&repo, "equal-first", "first.txt", "first\n");
    repo.arc(&repo.root)
        .args(["integrate", "equal-first"])
        .assert()
        .success();
    let target = repo.head(&repo.root);

    let (verify_id, _) = verification_only(&repo, "equal-verify", &target, &[&first_id]);
    let (tail_id, _) =
        approved_change_blocked_by(&repo, "equal-tail", "tail.txt", "tail\n", &[&verify_id]);

    let plan = stdout(
        repo.arc(&repo.root)
            .args(["integrate", "equal-verify", "--dry-run"]),
    );
    assert!(
        plan.contains("merge: none") && plan.contains("already contains"),
        "the plan names the containment instead of a merge it cannot make:\n{plan}"
    );

    repo.arc(&repo.root)
        .args(["integrate", "--tag", "series"])
        .assert()
        .success();

    let event = events(&repo, &verify_id)
        .into_iter()
        .find(|event| event["event_type"] == "change-integrated")
        .expect("the equal-head slice records an integration");
    assert_eq!(event["already_contained"], true, "{event}");
    assert_eq!(event["integrated_commit"], target.as_str());
    assert_eq!(event["target_before"], target.as_str());
    let tail_event = events(&repo, &tail_id)
        .into_iter()
        .find(|event| event["event_type"] == "change-integrated")
        .expect("the successor integrates after the contained closure");
    assert_ne!(tail_event["integrated_commit"], target.as_str());
}

#[test]
fn an_already_contained_head_still_owes_merged_tree_evidence() {
    let repo = repo_with_gates();
    let (first_id, first_wt) = approved_change(&repo, "ancestor-first", "first.txt", "first\n");
    let head = repo.head(&first_wt);
    let (mover_id, _) = approved_change_blocked_by(
        &repo,
        "ancestor-mover",
        "mover.txt",
        "mover\n",
        &[&first_id],
    );
    let (verify_id, _) = verification_only(&repo, "ancestor-verify", &head, &[&first_id]);

    // A target that moved past the approved head needs evidence at the tree a
    // merge would ship; being already contained does not answer that.
    repo.arc(&repo.root)
        .args(["integrate", &first_id, &mover_id])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &verify_id])
        .assert()
        .code(14)
        .stderr(predicates::str::contains("merged tree"));
    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", &verify_id, "--json"]),
    ))
    .unwrap();
    assert_eq!(status["state"], "open");

    repo.arc(&repo.root)
        .args(["verify", &verify_id, "--against", "master"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &verify_id])
        .assert()
        .success();
    let event = events(&repo, &verify_id)
        .into_iter()
        .find(|event| event["event_type"] == "change-integrated")
        .expect("the contained slice records an integration");
    assert_eq!(event["already_contained"], true, "{event}");
}

fn events(repo: &Repo, change_id: &str) -> Vec<serde_json::Value> {
    stdout(repo.arc(&repo.root).args(["events", "--change", change_id]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn an_already_contained_head_with_a_refusing_verdict_does_not_close() {
    let repo = repo_with_gates();
    let (first_id, _) = approved_change(&repo, "refused-first", "first.txt", "first\n");
    repo.arc(&repo.root)
        .args(["integrate", "refused-first"])
        .assert()
        .success();
    let target = repo.head(&repo.root);

    let mut args = vec![
        "begin",
        "refused-verify",
        "--tag",
        "series",
        "--base",
        &target,
        "--blocked-by",
        &first_id,
    ];
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(&args)));
    let worktree = repo.home.join(".worktrees").join("repo-refused-verify");
    stdout(repo.arc(&worktree).args(["snapshot", "refused-verify"]));
    repo.arc(&worktree)
        .args(["verify", "refused-verify", "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "review",
            "refused-verify",
            "--verdict",
            "changes-requested",
            "--cause",
            "brief",
        ])
        .assert()
        .success();
    args.clear();

    repo.arc(&repo.root)
        .args(["integrate", &change_id])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("approval"));
    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", &change_id, "--json"]),
    ))
    .unwrap();
    assert_eq!(status["state"], "open");
    assert!(!events(&repo, &change_id)
        .iter()
        .any(|event| event["event_type"] == "change-integrated"));
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
