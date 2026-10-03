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
    approved_change_based_on(repo, slug, file, content, blocked_by, None)
}

fn approved_change_based_on(
    repo: &Repo,
    slug: &str,
    file: &str,
    content: &str,
    blocked_by: &[&str],
    base: Option<&str>,
) -> (String, PathBuf) {
    let mut args = vec!["begin", slug, "--tag", "series"];
    if let Some(base) = base {
        args.extend(["--base", base]);
    }
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

    let out =
        stdout(
            repo.arc(&repo.root)
                .args(["integrate", "--shape", "no-ff", "--tag", "series"]),
        );
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
    assert_eq!(event["shape"], "already-contained");
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
    assert_eq!(event["shape"], "already-contained");
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
        .args(["integrate", "--shape", "no-ff", &first_id, &mover_id])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "--shape", "no-ff", &verify_id])
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
        .args(["integrate", "--shape", "no-ff", &verify_id])
        .assert()
        .success();
    let event = events(&repo, &verify_id)
        .into_iter()
        .find(|event| event["event_type"] == "change-integrated")
        .expect("the contained slice records an integration");
    assert_eq!(event["already_contained"], true, "{event}");
    assert_eq!(event["shape"], "already-contained");
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
        2
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
        .stderr(predicates::str::contains("tracked modifications"))
        .stderr(predicates::str::contains("stash").not());
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

/// A change opened with `begin --no-worktree` from a clean checkout on its
/// target: the checkout itself is the change's worktree.
fn in_place_change(repo: &Repo, slug: &str) -> String {
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        slug,
        "--no-worktree",
    ])));
    repo.commit(&repo.root, &format!("{slug}.txt"), "in place\n", "feat");
    stdout(repo.arc(&repo.root).args(["snapshot", slug]));
    repo.arc(&repo.root)
        .args(["verify", slug, "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    change_id
}

#[test]
fn a_no_worktree_change_merges_in_its_own_checkout() {
    let repo = repo_with_gates();
    fs::write(repo.root.join(".gitignore"), "ignored.bin\n").unwrap();
    git(&repo.root, &["add", ".gitignore"]);
    git(&repo.root, &["commit", "-m", "test: ignore local output"]);
    let change_id = in_place_change(&repo, "stranded-in-place");
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "arc/stranded-in-place"
    );
    let old_master = git_out(&repo.root, &["rev-parse", "master"]);
    let approved_head = repo.head(&repo.root);
    fs::write(repo.root.join("untracked.bin"), "untracked bytes\n").unwrap();
    fs::write(repo.root.join("ignored.bin"), "ignored bytes\n").unwrap();

    let plan = stdout(
        repo.arc(&repo.root)
            .args(["integrate", "stranded-in-place", "--dry-run"]),
    );
    assert!(
        plan.contains("would check out master in")
            && plan.contains(&repo.root.display().to_string()),
        "the plan reports the checkout it would take over:\n{plan}"
    );

    repo.arc(&repo.root)
        .args(["integrate", "stranded-in-place"])
        .assert()
        .success();
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "master",
        "the checkout is left on the target, as it stood before begin"
    );
    assert_eq!(
        fs::read_to_string(repo.root.join("untracked.bin")).unwrap(),
        "untracked bytes\n"
    );
    assert_eq!(
        fs::read_to_string(repo.root.join("ignored.bin")).unwrap(),
        "ignored bytes\n"
    );
    let merged = repo.head(&repo.root);
    assert_eq!(merged, approved_head);
    let parents = git_out(&repo.root, &["rev-list", "--parents", "-n", "1", &merged])
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(parents.len(), 2);
    assert_eq!(parents[1], old_master);
    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", &change_id, "--json"]),
    ))
    .unwrap();
    assert_eq!(status["state"], "closed");
}

#[test]
fn a_dirty_no_worktree_checkout_keeps_the_refusal() {
    let repo = repo_with_gates();
    let _change_id = in_place_change(&repo, "stranded-dirty");
    let old_master = git_out(&repo.root, &["rev-parse", "master"]);
    fs::write(repo.root.join("README.md"), "edited in place\n").unwrap();

    repo.arc(&repo.root)
        .args(["integrate", "stranded-dirty"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("tracked modifications"));
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "arc/stranded-dirty",
        "the refused checkout is left alone"
    );
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), old_master);
}

#[test]
fn a_change_with_no_checkout_keeps_the_missing_worktree_refusal() {
    let repo = Repo::new();
    git(
        &repo.root,
        &["checkout", "-q", "-b", "arc/stranded-nowhere"],
    );
    repo.commit(&repo.root, "nowhere.txt", "nowhere\n", "feat: nowhere");
    git(&repo.root, &["checkout", "-q", "master"]);
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "stranded-nowhere",
        "--adopt",
        "arc/stranded-nowhere",
    ])));
    stdout(repo.arc(&repo.root).args(["snapshot", &change_id]));
    repo.arc(&repo.root)
        .args(["review", &change_id, "--verdict", "approved"])
        .assert()
        .success();
    // With nobody on master and no recorded checkout, there is nowhere to
    // merge, and the refusal is the one that says so.
    git(&repo.root, &["checkout", "-q", "--detach", "master"]);

    repo.arc(&repo.root)
        .args(["integrate", &change_id])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no worktree has"));
}

#[test]
fn the_switch_onto_the_target_does_not_overwrite_an_ignored_file() {
    let repo = repo_with_gates();
    repo.commit(
        &repo.root,
        "kept.txt",
        "tracked on the target\n",
        "test: add kept.txt",
    );
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "stranded-guard",
        "--no-worktree",
    ])));
    git(&repo.root, &["rm", "-q", "kept.txt"]);
    git(&repo.root, &["commit", "-qm", "feat: drop kept"]);
    stdout(repo.arc(&repo.root).args(["snapshot", "stranded-guard"]));
    repo.arc(&repo.root)
        .args(["verify", "stranded-guard", "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["review", "stranded-guard", "--verdict", "approved"])
        .assert()
        .success();
    // The checkout ignores a local file exactly where the switch onto the
    // target would write the target's copy. Git checkout would overwrite it
    // without a word.
    let exclude = repo.root.join(".git/info/exclude");
    let mut text = fs::read_to_string(&exclude).unwrap_or_default();
    text.push_str("kept.txt\n");
    fs::write(&exclude, text).unwrap();
    fs::write(repo.root.join("kept.txt"), b"local ignored bytes\n").unwrap();
    let before = git_out(&repo.root, &["rev-parse", "master"]);

    repo.arc(&repo.root)
        .args(["integrate", "stranded-guard", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("kept.txt"));
    repo.arc(&repo.root)
        .args(["integrate", "stranded-guard"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("kept.txt"));
    assert_eq!(
        fs::read(repo.root.join("kept.txt")).unwrap(),
        b"local ignored bytes\n"
    );
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), before);
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "arc/stranded-guard"
    );
    assert!(!events(&repo, &change_id)
        .iter()
        .any(|event| event["event_type"] == "change-integrated"));
}

/// A change in a repository that declares no gate: approval is the only
/// evidence its profile can carry, because nothing was ever evaluated.
fn ungated_change(repo: &Repo, slug: &str) -> String {
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        slug,
        "--no-worktree",
    ])));
    repo.commit(&repo.root, &format!("{slug}.txt"), "content\n", "feat");
    stdout(repo.arc(&repo.root).args(["snapshot", slug]));
    change_id
}

fn approve(repo: &Repo, change_id: &str) {
    repo.arc(&repo.root)
        .args(["review", change_id, "--verdict", "approved"])
        .assert()
        .success();
}

fn output_of(assertion: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&assertion.get_output().stdout).into_owned()
}

#[test]
fn done_without_declared_gates_reports_the_check_state() {
    let repo = Repo::new();
    let change_id = ungated_change(&repo, "ungated-done");

    // The first `done` snapshots and prints where the change actually
    // stands, rather than dying on the missing declaration.
    let assertion = repo
        .arc(&repo.root)
        .args(["done", "ungated-done"])
        .assert()
        .code(3);
    let out = output_of(&assertion);
    assert!(
        out.contains("no gates declared for profile local; nothing was run"),
        "{out}"
    );
    assert!(out.contains("missing or stale approval"), "{out}");

    approve(&repo, &change_id);
    let assertion = repo
        .arc(&repo.root)
        .args(["done", "ungated-done"])
        .assert()
        .success();
    let out = output_of(&assertion);
    assert!(
        out.contains("no gates declared for profile local"),
        "done says no gate exists:\n{out}"
    );
    assert!(
        !out.contains("all integration gates pass"),
        "done must not report gates passing where none exist:\n{out}"
    );
}

#[test]
fn check_and_show_name_the_missing_gate_rather_than_gates_passing() {
    let repo = Repo::new();
    let change_id = ungated_change(&repo, "ungated-check");
    approve(&repo, &change_id);

    let assertion = repo
        .arc(&repo.root)
        .args(["check", "ungated-check"])
        .assert()
        .success();
    let out = output_of(&assertion);
    assert!(
        out.contains("ready: no gates declared for profile local"),
        "{out}"
    );
    assert!(!out.contains("all integration gates pass"), "{out}");
    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "ungated-check"]),
    ))
    .unwrap();
    assert_eq!(
        status["ready_reason"],
        "no gates declared for profile local"
    );

    let assertion = repo
        .arc(&repo.root)
        .args(["check", "ungated-check", "--explain"])
        .assert()
        .success();
    let out = output_of(&assertion);
    assert!(out.contains("no gates declared for profile local"), "{out}");
    assert!(!out.contains("required gates green"), "{out}");

    let assertion = repo
        .arc(&repo.root)
        .args(["show", &change_id])
        .assert()
        .success();
    let out = output_of(&assertion);
    assert!(out.contains("none declared for profile local"), "{out}");
}

#[test]
fn verify_against_without_declared_gates_still_refuses_and_integrate_lands() {
    let repo = Repo::new();
    let change_id = ungated_change(&repo, "ungated-keep");
    approve(&repo, &change_id);
    let old_master = git_out(&repo.root, &["rev-parse", "master"]);

    repo.arc(&repo.root)
        .args(["verify", "ungated-keep", "--against", "master"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "no gates declared for profile local",
        ));

    repo.arc(&repo.root)
        .args(["integrate", "ungated-keep"])
        .assert()
        .success();
    let merged = repo.head(&repo.root);
    let parents = git_out(&repo.root, &["rev-list", "--parents", "-n", "1", &merged])
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(parents.len(), 2);
    assert_eq!(parents[1], old_master);
}

#[test]
fn rebase_without_declared_gates_names_the_missing_gate() {
    let repo = Repo::new();
    ungated_change(&repo, "ungated-rebase");

    // A second change moves the target under the first.
    let mover = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "ungated-mover",
        "--target",
        "master",
    ])));
    let mover_wt = repo.home.join(".worktrees").join("repo-ungated-mover");
    repo.commit(&mover_wt, "mover.txt", "mover\n", "feat: mover");
    stdout(repo.arc(&mover_wt).args(["snapshot", "ungated-mover"]));
    repo.arc(&mover_wt)
        .args(["review", "ungated-mover", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &mover])
        .assert()
        .success();

    let assertion = repo
        .arc(&repo.root)
        .args(["rebase", "ungated-rebase"])
        .assert()
        .success();
    let out = output_of(&assertion);
    assert!(out.contains("no gates declared for profile local"), "{out}");
    assert!(
        !out.contains("every required gate is green at head"),
        "a rebase must not report gates passing where none exist:\n{out}"
    );
}

#[test]
fn a_merged_file_over_an_ignored_directory_is_refused_by_name() {
    let repo = repo_with_gates();
    fs::write(repo.root.join(".gitignore"), "packed/\n").unwrap();
    git(&repo.root, &["add", ".gitignore"]);
    git(&repo.root, &["commit", "-m", "test: ignore packed/"]);
    let (_change, _wt) = approved_change(&repo, "file-over-dir", "packed", "now a file\n");
    fs::create_dir_all(repo.root.join("packed")).unwrap();
    fs::write(repo.root.join("packed/local.bin"), b"local ignored bytes\n").unwrap();

    repo.arc(&repo.root)
        .args(["integrate", "file-over-dir"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("untracked or ignored: packed"));
    assert_eq!(
        fs::read(repo.root.join("packed/local.bin")).unwrap(),
        b"local ignored bytes\n"
    );
}

#[test]
fn cleanup_after_an_already_contained_closure_removes_the_change_checkout() {
    let repo = repo_with_gates();
    let (first_id, _) = approved_change(&repo, "cleanup-first", "first.txt", "first\n");
    repo.arc(&repo.root)
        .args(["integrate", "cleanup-first"])
        .assert()
        .success();
    let target = repo.head(&repo.root);
    let (verify_id, verify_wt) = verification_only(&repo, "cleanup-verify", &target, &[&first_id]);

    let state = json_stdout(repo.arc(&repo.root).args(["show", &verify_id, "--json"]));
    assert_eq!(state["worktree_created"], true);
    repo.arc(&repo.root)
        .args(["integrate", &verify_id, "--cleanup"])
        .assert()
        .success();
    assert!(
        !verify_wt.exists(),
        "the contained change's checkout is removed"
    );
    assert!(git_out(&repo.root, &["branch", "--list", "arc/cleanup-verify"]).is_empty());
}

#[test]
fn cleanup_after_a_take_over_keeps_the_checkout_and_drops_the_branch() {
    let repo = repo_with_gates();
    let _change = in_place_change(&repo, "stranded-cleanup");
    let state = json_stdout(
        repo.arc(&repo.root)
            .args(["show", "stranded-cleanup", "--json"]),
    );
    assert_eq!(state["worktree_created"], false);

    repo.arc(&repo.root)
        .args(["integrate", "stranded-cleanup", "--cleanup"])
        .assert()
        .success();
    assert!(
        repo.root.exists(),
        "the repository checkout is never removed"
    );
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "master"
    );
    assert!(git_out(&repo.root, &["branch", "--list", "arc/stranded-cleanup"]).is_empty());
    assert!(repo.root.join("stranded-cleanup.txt").exists());
}

/// A closed change answers for the head its closure recorded. Neither a
/// branch deleted on integration nor a target that moved on afterwards
/// changes what shipped or the evidence it shipped on.
#[test]
fn a_closed_change_reports_the_head_it_shipped_after_its_branch_is_deleted() {
    let repo = repo_with_gates();
    let (change_id, worktree) = approved_change(&repo, "shipped-head", "shipped.txt", "shipped\n");
    let shipped = repo.head(&worktree);
    repo.arc(&repo.root)
        .args(["integrate", "shipped-head", "--cleanup"])
        .assert()
        .success();
    assert!(git_out(&repo.root, &["branch", "--list", "arc/shipped-head"]).is_empty());
    fs::write(repo.root.join("later.txt"), "later\n").unwrap();
    git(&repo.root, &["add", "later.txt"]);
    git(
        &repo.root,
        &["commit", "-m", "feat: later work on the target"],
    );

    let show = stdout(repo.arc(&repo.root).args(["show", "shipped-head"]));
    assert!(
        show.contains("Worktree state: head matches shipped patchset ps-01"),
        "{show}"
    );
    assert!(show.contains("green at head"), "{show}");
    assert!(!show.contains("NOT green at head"), "{show}");

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", &change_id, "--json"]),
    ))
    .unwrap();
    assert_eq!(status["state"], "closed", "{status}");
    assert_eq!(status["current_head"], shipped.as_str(), "{status}");
    assert_eq!(status["head_matches_latest_patchset"], true, "{status}");
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
    assert_eq!(
        status["blockers"],
        serde_json::json!(["closed"]),
        "{status}"
    );
}

/// A closed change answers to the declarations it closed under. A gate the
/// target declares afterwards is not owed by work that already shipped, and
/// declarations the target breaks afterwards cannot make its report fail.
#[test]
fn a_closed_change_reports_the_declarations_it_closed_under() {
    let repo = repo_with_gates();
    let (change_id, worktree) = approved_change(&repo, "shipped-gates", "gated.txt", "gated\n");
    repo.arc(&repo.root)
        .args(["integrate", "shipped-gates"])
        .assert()
        .success();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.smoke]\ncommand = \"test -f README.md\"\n\n[gates.later]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["commit", "-am", "test: require a later gate"]);

    let status = json_stdout(repo.arc(&repo.root).args(["status", &change_id, "--json"]));
    let recorded: Vec<&String> = status["closure"]["authorization"]["gates"]
        .as_object()
        .expect("a guarded closure records the gates it consumed")
        .keys()
        .collect();
    let reported: Vec<&str> = status["gates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|gate| gate["name"].as_str().unwrap())
        .collect();
    assert_eq!(recorded, ["smoke"], "{status}");
    assert_eq!(reported, ["smoke"], "{status}");
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
    assert_eq!(
        status["blockers"],
        serde_json::json!(["closed"]),
        "{status}"
    );
    let show = stdout(repo.arc(&repo.root).args(["show", "shipped-gates"]));
    assert!(!show.contains("later"), "{show}");

    fs::write(repo.root.join(".arc/gates.toml"), "[gates.smoke\n").unwrap();
    fs::write(repo.root.join(".arc/policy.toml"), "[policy\n").unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(
        &repo.root,
        &["commit", "-m", "test: break the declarations"],
    );
    let status = json_stdout(repo.arc(&worktree).args(["status", &change_id, "--json"]));
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
    repo.arc(&worktree)
        .args(["show", "shipped-gates"])
        .assert()
        .success();
}

/// The target checkout's own broken policy cannot fail a closed change's
/// report run from that checkout, because reporting never parses the append
/// policy. Recording anything there still parses it, and refuses.
#[test]
fn a_closed_change_reports_from_a_target_whose_policy_is_broken() {
    let repo = repo_with_gates();
    let (change_id, worktree) = approved_change(&repo, "shipped-policy", "policy.txt", "p\n");
    repo.arc(&worktree)
        .args([
            "debt",
            "shipped-policy",
            "--reason",
            "no second actor reachable",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "shipped-policy"])
        .assert()
        .success();
    fs::write(repo.root.join(".arc/policy.toml"), "[policy\n").unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: break the policy"]);

    let status = json_stdout(repo.arc(&repo.root).args(["status", &change_id, "--json"]));
    assert_eq!(status["state"], "closed", "{status}");
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
    repo.arc(&repo.root)
        .args(["show", "shipped-policy"])
        .assert()
        .success();
    let check = json_stdout_any_status(repo.arc(&repo.root).args(["check", &change_id, "--json"]));
    assert_eq!(check["ready"], false, "{check}");
    assert_eq!(check["blockers"][0]["blocker"], "closed", "{check}");
    let advisories = check["advisories"].to_string();
    assert!(advisories.contains("debt-summary"), "{check}");
    for report in ["explain", "findings", "log"] {
        repo.arc(&repo.root)
            .args([report, "shipped-policy"])
            .assert()
            .success();
    }

    let refused = repo
        .arc(&repo.root)
        .args(["begin", "after-broken-policy", "--no-worktree"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("policy.toml"), "{stderr}");
    let listed = stdout(repo.arc(&worktree).args(["list"]));
    assert!(!listed.contains("after-broken-policy"), "{listed}");
}

/// Every command that records nothing reads the ledger without the append
/// policy, so the target checkout's broken policy fails none of them: the
/// tagged and aggregate views as much as a single change's report.
#[test]
fn reports_over_many_changes_read_past_a_broken_policy() {
    let repo = repo_with_gates();
    let (change_id, _worktree) = approved_change(&repo, "tagged-shipped", "tagged.txt", "t\n");
    repo.arc(&repo.root)
        .args(["integrate", "tagged-shipped"])
        .assert()
        .success();
    fs::write(repo.root.join(".arc/policy.toml"), "[policy\n").unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: break the policy"]);

    let shown = stdout(repo.arc(&repo.root).args(["show", "--tag", "series"]));
    assert!(shown.contains(&change_id), "{shown}");
    let checked = stdout(repo.arc(&repo.root).args(["check", "--tag", "series"]));
    assert!(
        checked.contains(&format!("{change_id}: integrated")),
        "{checked}"
    );
    let listed = stdout(repo.arc(&repo.root).args(["list"]));
    assert!(listed.contains(&change_id), "{listed}");
    let queried = stdout(repo.arc(&repo.root).args(["query", "--tag", "series"]));
    assert!(queried.contains(&change_id), "{queried}");
    let blockers = json_stdout(repo.arc(&repo.root).args(["blocker-status", &change_id]));
    assert_eq!(blockers["blocked"], false, "{blockers}");
    repo.arc(&repo.root)
        .args(["is-blocked", &change_id])
        .assert()
        .success();
    repo.arc(&repo.root).args(["inbox"]).assert().success();
}

/// The guide's read-only list is the contract: every command on it, in each
/// form that reads something different, runs from a target checkout whose
/// committed policy cannot be parsed. A command added to the list without a
/// form here fails the test, so the list cannot promise what nothing checks.
#[test]
fn every_read_only_command_reads_past_a_broken_policy() {
    let repo = repo_with_gates();
    let (change_id, worktree) = approved_change(&repo, "read-only", "read.txt", "r\n");
    repo.arc(&worktree)
        .args([
            "message",
            "read-only",
            "--type",
            "status",
            "--summary",
            "shipping",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["debt", "read-only", "--reason", "no second actor reachable"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "read-only"])
        .assert()
        .success();
    let merged = repo.head(&repo.root);
    let closed = json_stdout(repo.arc(&repo.root).args(["status", &change_id, "--json"]));
    let closure_event = closed["closure"]["event_id"]
        .as_str()
        .expect("a closed change names its closure event")
        .to_string();
    fs::write(repo.root.join(".arc/policy.toml"), "[policy\n").unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: break the policy"]);

    let mut failures = Vec::new();
    for args in [
        vec!["stats", "--json"],
        vec!["diff", &change_id, "--integrated"],
        vec!["resume", &change_id, "--json"],
        vec!["prompt", &change_id],
    ] {
        let out = repo.arc(&repo.root).args(&args).output().unwrap();
        if !out.status.success() {
            failures.push(format!(
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    let guide = stdout(&mut repo.arc(&repo.root));
    let listed: std::collections::BTreeSet<String> = guide
        .lines()
        .skip_while(|line| !line.ends_with("These commands record nothing, in any form or flag:"))
        .skip(1)
        .take_while(|line| line.starts_with("    "))
        .flat_map(|line| line.trim().split("  ").map(str::to_string))
        .collect();
    assert!(listed.contains("status"), "{guide}");

    let id = change_id.as_str();
    // Each form with the exit code it reports for a change that has shipped:
    // `check` answers that a closed change is not ready to integrate.
    let forms: Vec<(Vec<&str>, i32)> = vec![
        (vec!["status", id, "--json"], 0),
        (vec!["status", "read-only", "--at", &closure_event], 0),
        (vec!["--change", id, "status", "read-only"], 0),
        (vec!["show", "read-only"], 0),
        (vec!["show", "read-only", "--json"], 0),
        (vec!["show", "read-only", "--at", &closure_event], 0),
        (vec!["show", "--tag", "series"], 0),
        (vec!["check", id, "--json"], 6),
        (vec!["check", id, "--explain"], 6),
        (vec!["check", "--tag", "series"], 0),
        (vec!["explain", "read-only"], 0),
        (vec!["explain", "read-only", "--json"], 0),
        (vec!["findings", "read-only"], 0),
        (vec!["findings", "read-only", "--audit"], 0),
        (vec!["log", "read-only"], 0),
        (vec!["log", "read-only", "--reverse"], 0),
        (vec!["list"], 0),
        (vec!["list", "--json"], 0),
        (vec!["query", "--tag", "series"], 0),
        (vec!["query", "--debt", "--json"], 0),
        (vec!["query", "--commit", &merged], 0),
        (vec!["blocker-status", id], 0),
        (vec!["is-blocked", id], 0),
        (vec!["messages"], 0),
        (vec!["messages", "--change", "read-only", "--json"], 0),
        (vec!["inbox"], 0),
        (vec!["inbox", "--json"], 0),
        (vec!["catchup"], 0),
        (vec!["catchup", "--json"], 0),
    ];
    let manual_dir = repo.home.join("manual");
    let manual_dir = manual_dir.to_str().unwrap();
    // Missing subjects exercise read errors without claiming a successful lookup.
    let extra_forms = vec![
        vec!["stats", "--json"],
        vec!["stats", "--by-model", "--json"],
        vec!["stats", "--provenance", "--json"],
        vec!["diff", id, "--integrated"],
        vec!["resume", id, "--json"],
        vec!["prompt", id],
        vec!["chain", "series", "--json"],
        vec!["events", "--change", id],
        vec!["watch", id, "--until", "integrated", "--json"],
        vec!["export", id, "--output", "-"],
        vec!["replica", "id", "--json"],
        vec!["replica", "status", "--json"],
        vec!["replica", "export", "--output", "-"],
        vec!["env"],
        vec!["completions", "bash"],
        vec!["mangen", manual_dir],
        vec!["fork", "list", "--json"],
        vec!["fork", "thread", "missing"],
        vec!["history", "resolve", &merged],
        vec!["candidate", "show", "missing", "--json"],
        vec!["candidate", "list", "--json"],
        vec!["pass", "list", "--json"],
        vec!["run", "list", "--json"],
        vec!["policy", "path"],
        vec!["config", "--check-writable", "--json"],
        vec!["sandbox", "diff", manual_dir, "--json"],
        vec!["doctor", "--json"],
        vec!["hooks", "status"],
        vec!["instructions", "git"],
        vec!["workspace", "list", "--json"],
        vec!["workspace", "inbox", "--json"],
        vec!["workspace", "inventory", "--json"],
        vec!["workspace", "backlog", "--json"],
        vec!["workspace", "report", "--json"],
        vec!["restack", id, "--advise"],
        vec!["journal", "dir"],
        vec!["journal", "doctor", "--json"],
        vec![
            "journal",
            "source",
            "--harness",
            "test",
            "--session",
            "session-a",
        ],
        vec!["journal", "scaffolds", "--json"],
        vec!["journal", "questions", "--json"],
        vec!["journal", "events"],
        vec!["journal", "catchup", "--json"],
        vec!["journal", "memories", "--json"],
        vec!["journal", "open", "--json"],
        vec!["journal", "list", "--json"],
        vec!["journal", "inventory", "--json"],
        vec!["journal", "show", "missing.md", "--json"],
        vec!["journal", "latest", "missing", "--json"],
        vec!["journal", "discussion", "missing.md", "--json"],
        vec!["journal", "stamp"],
        vec!["journal", "lane", "list", "--json"],
        vec!["journal", "export", "missing.md", "--output", "-"],
    ];
    let exercised: std::collections::BTreeSet<String> = forms
        .iter()
        .map(|(args, _)| {
            args.iter()
                .find(|arg| listed.contains(**arg))
                .map_or_else(|| args[0].to_string(), |name| name.to_string())
        })
        .collect();
    let mut exercised = exercised;
    for args in &extra_forms {
        let path = if listed.contains(args[0]) {
            args[0].to_string()
        } else if args[..2] == ["journal", "lane"] {
            args[..3].join(" ")
        } else {
            args[..2].join(" ")
        };
        exercised.insert(path);
    }
    // Inspection parses the policy inputs whose contents it reports.
    exercised.insert("policy show".into());
    exercised.insert("config".into());
    assert_eq!(exercised, listed, "every listed command has a form here");
    let policy = repo
        .arc(&repo.root)
        .args(["policy", "show"])
        .output()
        .unwrap();
    assert!(!policy.status.success());
    assert!(String::from_utf8_lossy(&policy.stderr).contains("policy.toml"));
    repo.arc(&repo.root)
        .arg("config")
        .assert()
        .failure()
        .stderr(predicates::str::contains("policy.toml"));
    for args in extra_forms {
        let out = repo.arc(&repo.root).args(&args).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("policy.toml"), "{args:?}\n{stderr}");
        if !args.starts_with(&["history", "resolve"]) {
            assert_ne!(out.status.code(), Some(2), "{args:?}\n{stderr}");
        }
    }

    for (args, code) in forms {
        let out = repo.arc(&repo.root).args(&args).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(code), "{args:?}\n{stderr}");
        assert!(!stderr.contains("policy.toml"), "{args:?}\n{stderr}");
    }
}

fn closed_earlier_patchset_fixture() -> (Repo, String) {
    let repo = repo_with_gates();
    stdout(repo.arc(&repo.root).args(["begin", "earlier-shipped"]));
    let worktree = repo.home.join(".worktrees").join("repo-earlier-shipped");
    repo.commit(&worktree, "first.txt", "first\n", "feat: first");
    stdout(repo.arc(&worktree).args(["snapshot", "earlier-shipped"]));
    let shipped = repo.head(&worktree);
    repo.commit(&worktree, "second.txt", "second\n", "feat: second");
    stdout(repo.arc(&worktree).args(["snapshot", "earlier-shipped"]));
    git(
        &repo.root,
        &[
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            "external merge",
            &shipped,
        ],
    );
    let merge = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args([
            "close",
            "earlier-shipped",
            "--assert-integrated",
            &merge,
            "--patchset",
            "ps-01",
        ])
        .assert()
        .success();

    (repo, shipped)
}

#[test]
fn closed_earlier_patchset_show_names_the_shipped_patchset() {
    let (repo, _) = closed_earlier_patchset_fixture();
    let show = stdout(repo.arc(&repo.root).args(["show", "earlier-shipped"]));
    assert!(
        show.contains("Worktree state: head matches shipped patchset ps-01"),
        "{show}"
    );
}

/// Replaying a closed change answers for the head its closure recorded, as
/// the live report does: an asserted integration that shipped an earlier
/// patchset is judged at that patchset's head, not at a later one.
#[test]
fn a_replayed_closure_reports_the_head_it_recorded() {
    let (repo, shipped) = closed_earlier_patchset_fixture();

    let live = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "earlier-shipped", "--json"]),
    );
    assert_eq!(live["current_head"], shipped.as_str(), "{live}");
    assert_eq!(live["latest_patchset"]["id"], "ps-02", "{live}");
    assert_eq!(live["head_matches_latest_patchset"], false, "{live}");
    assert_eq!(live["comparison_patchset"], "ps-01", "{live}");
    assert_eq!(live["head_matches_comparison_patchset"], true, "{live}");
    let closure = live["closure"]["event_id"].as_str().unwrap().to_string();
    let replayed = json_stdout(repo.arc(&repo.root).args([
        "status",
        "earlier-shipped",
        "--at",
        &closure,
        "--json",
    ]));
    assert_eq!(replayed["current_head"], shipped.as_str(), "{replayed}");
    assert_eq!(
        replayed["head_matches_latest_patchset"], live["head_matches_latest_patchset"],
        "{replayed}"
    );

    let worktree_state = |show: &str| {
        show.lines()
            .find(|line| line.starts_with("- Worktree state:"))
            .map(str::to_owned)
    };
    let live_show = stdout(repo.arc(&repo.root).args(["show", "earlier-shipped"]));
    let replayed_show =
        stdout(
            repo.arc(&repo.root)
                .args(["show", "earlier-shipped", "--at", &closure]),
        );
    assert!(
        live_show.contains("Worktree state: head matches shipped patchset ps-01"),
        "{live_show}"
    );
    assert_eq!(replayed["comparison_patchset"], "ps-01", "{replayed}");
    assert_eq!(
        replayed["head_matches_comparison_patchset"], true,
        "{replayed}"
    );
    assert_eq!(
        worktree_state(&replayed_show),
        worktree_state(&live_show),
        "{replayed_show}"
    );
}

/// The plan the dry run prints is the one the merge carries out: the basis
/// the integration event records, at the target revision the plan names.
#[test]
fn dry_run_json_prints_the_integration_plan() {
    let repo = repo_with_gates();
    let (change_id, worktree) = approved_change(&repo, "plan-json", "plan.txt", "plan\n");
    let target = repo.head(&repo.root);
    let head = repo.head(&worktree);

    let out = stdout(
        repo.arc(&repo.root)
            .args(["integrate", "plan-json", "--dry-run", "--json"]),
    );
    let plan: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(plan["schema"], "arc-integration-plan/1", "{plan}");
    assert_eq!(plan["change_id"], change_id.as_str());
    assert_eq!(plan["records"], "change-integrated");
    assert_eq!(plan["target"], "master");
    assert_eq!(plan["target_revision"], target.as_str());
    assert_eq!(plan["approved_head"], head.as_str());
    assert_eq!(plan["merge_commit"], false);
    assert_eq!(plan["shape"], "fast-forward");
    assert!(plan["evaluated_tree"].is_string(), "{plan}");
    assert!(plan["authorization"]["gate_evidence"]["smoke"].is_string());
    assert_eq!(repo.head(&repo.root), target, "a dry run merges nothing");

    // A queue has no single plan to print.
    repo.arc(&repo.root)
        .args(["integrate", "plan-json", "plan-json", "--dry-run", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "only valid when integrating one change",
        ));

    stdout(repo.arc(&repo.root).args(["integrate", "plan-json"]));
    let event = events(&repo, &change_id)
        .into_iter()
        .find(|event| event["event_type"] == "change-integrated")
        .expect("the change integrates");
    assert_eq!(event["authorization"], plan["authorization"]);
    assert_eq!(event["target_before"], plan["target_revision"]);
    assert_eq!(event["source_head"], plan["approved_head"]);
    assert_eq!(event["source_patchset_id"], plan["approved_patchset_id"]);
}

/// A target moved after the dry run is named beside the fresh decision and
/// never changes it: a change it leaves ready still lands, with a warning,
/// and one it makes unready is refused by its ordinary blocker.
#[test]
fn expected_basis_names_what_moved() {
    let repo = repo_with_gates();
    approved_change(&repo, "moves-steady", "steady.txt", "steady\n");
    let dry_run = |slug: &str| {
        let path = repo.root.join(format!("../{slug}-plan.json"));
        fs::write(
            &path,
            stdout(repo.arc(&repo.root).args([
                "integrate",
                "--shape",
                "no-ff",
                slug,
                "--dry-run",
                "--json",
            ])),
        )
        .unwrap();
        path
    };

    // An unchanged basis adds nothing.
    let steady_plan = dry_run("moves-steady");
    let steady = repo
        .arc(&repo.root)
        .args([
            "integrate",
            "--shape",
            "no-ff",
            "moves-steady",
            "--expect-basis",
        ])
        .arg(&steady_plan)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&steady.get_output().stderr).to_string();
    assert!(!stderr.contains("you checked"), "{stderr}");

    approved_change(&repo, "moves-ready", "ready.txt", "ready\n");
    approved_change(&repo, "moves-unready", "unready.txt", "unready\n");
    let ready_plan = dry_run("moves-ready");
    let unready_plan = dry_run("moves-unready");
    let checked = repo.head(&repo.root);

    // An empty commit moves the target without moving the tree any merge
    // would ship, so the ready change stays ready.
    git(
        &repo.root,
        &["commit", "--allow-empty", "-m", "chore: move the target"],
    );
    let moved = repo.head(&repo.root);
    let landed = repo
        .arc(&repo.root)
        .args([
            "integrate",
            "--shape",
            "no-ff",
            "moves-ready",
            "--expect-basis",
        ])
        .arg(&ready_plan)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&landed.get_output().stderr).to_string();
    assert!(
        stderr.contains(&format!(
            "warning: the target you checked moved from {checked} to {moved}"
        )),
        "{stderr}"
    );
    assert!(output_of(&landed).contains("integrated: "));

    // The merge moved the target again, onto a tree the other change's gates
    // never ran against: the fresh decision refuses, and the refusal names
    // the move beside its blocker.
    let now = repo.head(&repo.root);
    let refused = repo
        .arc(&repo.root)
        .args([
            "integrate",
            "--shape",
            "no-ff",
            "moves-unready",
            "--expect-basis",
        ])
        .arg(&unready_plan)
        .assert()
        .code(14);
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr).to_string();
    assert!(stderr.contains("merged tree"), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "the target you checked moved from {checked} to {now}"
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("warning:"), "{stderr}");
    assert_eq!(repo.head(&repo.root), now, "nothing merged");

    // A plan for another change is refused rather than compared.
    repo.arc(&repo.root)
        .args([
            "integrate",
            "--shape",
            "no-ff",
            "moves-unready",
            "--expect-basis",
        ])
        .arg(&ready_plan)
        .assert()
        .failure()
        .stderr(predicates::str::contains("the expected basis describes"));
}

/// A change whose only reviewer left a comment-only verdict: green, and
/// refused for want of an approval.
fn comment_only_change(repo: &Repo, slug: &str) -> PathBuf {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&worktree)
        .args(["verify", slug, "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Reviewer")
        .args(["review", slug, "--verdict", "comment-only"])
        .assert()
        .success();
    worktree
}

/// A refusal says why on its first line, and every blocker precedes the next
/// step: a reader who keeps only the headline, or filters for the next step,
/// still learns what stands in the way.
#[test]
fn a_refused_integration_leads_with_its_blocker() {
    let repo = repo_with_gates();
    comment_only_change(&repo, "refused");

    let out = repo
        .arc(&repo.root)
        .args(["integrate", "refused"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let refusal = String::from_utf8_lossy(&out.stderr);
    let headline = refusal.lines().next().unwrap_or_default();
    assert!(
        headline.starts_with("Cannot integrate refused-"),
        "{refusal}"
    );
    assert!(
        headline.ends_with(": missing or stale approval"),
        "{refusal}"
    );
    let blocker = refusal.find("Blocker 1: missing or stale approval");
    let next_step = refusal.find("Next step:");
    assert!(
        blocker.is_some() && next_step.is_some() && blocker < next_step,
        "{refusal}"
    );
}

/// `check` reports what blocks before anything that never blocks, however
/// many lines the advisories run to.
#[test]
fn check_prints_its_blockers_before_its_advisories() {
    let repo = repo_with_gates();
    stdout(repo.arc(&repo.root).args(["begin", "owed"]));
    let owed = repo.home.join(".worktrees").join("repo-owed");
    repo.commit(&owed, "work.txt", "owed\n", "feat: owed");
    stdout(repo.arc(&owed).args(["snapshot", "owed"]));
    repo.arc(&owed)
        .args(["verify", "owed", "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "owed", "--debt", "nobody read it"])
        .assert()
        .success();
    let worktree = comment_only_change(&repo, "touching");

    let out = repo
        .arc(&worktree)
        .args(["check", "touching"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.starts_with("Cannot integrate touching-"), "{report}");
    let blocker = report.find("Blocker 1: missing or stale approval");
    let advisory = report.find("debt-touched:");
    assert!(
        blocker.is_some() && advisory.is_some() && blocker < advisory,
        "{report}"
    );
}

/// What `check` and a refused `integrate` print as the next step for `change`,
/// after asserting that `status --json` still carries the bare code.
fn printed_next_step(repo: &Repo, change: &str, code: &str) -> (String, String) {
    let status = json_stdout(repo.arc(&repo.root).args(["status", change]));
    assert_eq!(status["next_action"], code, "{status}");
    let next_step = |text: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix("Next step: "))
            .unwrap_or_else(|| panic!("no next step in:\n{text}"))
            .to_string()
    };
    let check = repo
        .arc(&repo.root)
        .args(["check", change])
        .output()
        .unwrap();
    assert!(!check.status.success());
    let integrate = repo
        .arc(&repo.root)
        .args(["integrate", change])
        .output()
        .unwrap();
    assert!(!integrate.status.success());
    (
        next_step(&String::from_utf8_lossy(&check.stdout)),
        next_step(&String::from_utf8_lossy(&integrate.stderr)),
    )
}

/// `Next step:` is a command the reader can run with the change filled in,
/// followed by the code it renders; `check` and a refused `integrate` print
/// the same line.
#[test]
fn next_step_prints_a_command_the_reader_can_run() {
    let repo = repo_with_gates();
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "steps"])));
    let worktree = repo.home.join(".worktrees").join("repo-steps");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(repo.arc(&worktree).args(["snapshot", "steps"]));

    let (check, refusal) = printed_next_step(&repo, &change, "run_gate:smoke");
    assert_eq!(
        check,
        format!("arc verify {change} --gate smoke (run_gate:smoke)")
    );
    assert_eq!(refusal, check);

    repo.arc(&worktree)
        .args(["verify", "steps", "--gate", "smoke"])
        .assert()
        .success();
    let (check, refusal) = printed_next_step(&repo, &change, "declare_debt");
    assert_eq!(
        check,
        format!(
            "arc debt {change} --reason \"<what was read and what review is owed>\" (declare_debt)"
        )
    );
    assert_eq!(refusal, check);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "Reviewer")
        .args(["review", "steps", "--verdict", "comment-only"])
        .assert()
        .success();
    let (check, refusal) = printed_next_step(&repo, &change, "comment-only");
    assert_eq!(
        check,
        format!(
            "address the comment-only verdict, record any new commits with arc snapshot {change}, then ask for a fresh verdict: arc review {change} --verdict <verdict> (comment-only)"
        )
    );
    assert_eq!(refusal, check);
}

/// A change policy will not let its author approve names the review it owes
/// and who must give it.
#[test]
fn next_step_names_the_review_a_required_review_owes() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "owed"])));
    let worktree = repo.home.join(".worktrees").join("repo-owed");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(repo.arc(&worktree).args(["snapshot", "owed"]));

    let (check, refusal) = printed_next_step(&repo, &change, "request_review");
    assert_eq!(
        check,
        format!(
            "ask a reviewer independent of the contributors to run: arc review {change} --verdict <verdict> (request_review)"
        )
    );
    assert_eq!(refusal, check);
    let status = stdout(repo.arc(&repo.root).args(["show", &change]));
    assert!(
        status.contains(&format!("- Next step: {check}")),
        "{status}"
    );
}

fn retention_read_failure_after_merge(operation: &str, queue: bool) {
    use std::os::unix::fs::PermissionsExt;

    let repo = repo_with_gates();
    let (id, _) = approved_change(&repo, "retention-read", "feature.txt", "feature\n");
    if queue {
        approved_change(&repo, "retention-next", "next.txt", "next\n");
    }
    let before = repo.head(&repo.root);
    let pins = git_out(&repo.root, &["for-each-ref", "refs/arc/"]);
    let real_git =
        String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout).unwrap();
    let bin = repo.home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
if [ "$('{git}' rev-parse master)" != '{before}' ]; then
    case "$*" in
        {operation}) echo 'injected retention read failure' >&2; exit 1 ;;
    esac
fi
exec '{git}' "$@"
"#,
            git = real_git.trim()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut cmd = repo.arc(&repo.root);
    cmd.args(["integrate", "retention-read"]);
    if queue {
        cmd.args(["--shape", "no-ff", "retention-next"]);
    }
    cmd.env("PATH", path);
    let output = cmd.assert().success().get_output().clone();
    let out = String::from_utf8(output.stdout).unwrap();
    let err = String::from_utf8(output.stderr).unwrap();
    assert!(out.contains("integrated: "), "{out}");
    assert!(out.contains("event: "), "{out}");
    assert!(err.contains("warning:"), "{err}");
    assert!(err.contains(&format!("refs/arc/keep/{id}/")), "{err}");
    assert!(err.contains(&format!("refs/arc/tree/{id}/")), "{err}");
    assert!(err.contains("injected retention read failure"), "{err}");
    assert_ne!(before, repo.head(&repo.root));
    let state = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "retention-read", "--json"]),
    );
    assert_eq!(state["state"], "closed");
    assert_eq!(state["closure"]["outcome"], "integrated");
    assert!(out.contains(state["closure"]["event_id"].as_str().unwrap()));
    if queue {
        let next = json_stdout(
            repo.arc(&repo.root)
                .args(["status", "retention-next", "--json"]),
        );
        assert_eq!(next["closure"]["outcome"], "integrated");
        assert_eq!(out.matches("integrated: ").count(), 2);
    }
    let remaining = git_out(&repo.root, &["for-each-ref", "refs/arc/"]);
    let retained_prefix = if operation.contains("rev-list") {
        format!("refs/arc/tree/{id}/")
    } else {
        format!("refs/arc/keep/{id}/")
    };
    let retained = pins
        .lines()
        .find(|line| line.contains(&retained_prefix))
        .unwrap();
    assert!(remaining.lines().any(|line| line == retained));
}

#[test]
fn integrate_warns_on_retention_listing_failure_after_closure() {
    retention_read_failure_after_merge("'for-each-ref '*", false);
}

#[test]
fn integrate_warns_on_retention_object_walk_failure_after_closure() {
    retention_read_failure_after_merge("'rev-list --objects '*", false);
}

#[test]
fn integrate_warns_on_retention_failure_and_continues_queue() {
    retention_read_failure_after_merge("'for-each-ref '*", true);
}

fn retention_deletion_failure_after_merge(tree: bool, lock: bool, queue: bool) {
    use std::os::unix::fs::PermissionsExt;

    let repo = repo_with_gates();
    let (id, _) = approved_change(&repo, "retention-delete", "feature.txt", "feature\n");
    if queue {
        approved_change(&repo, "retention-next", "next.txt", "next\n");
    }
    let prefix = if tree {
        format!("refs/arc/tree/{id}/")
    } else {
        format!("refs/arc/keep/{id}/")
    };
    let pins = git_out(
        &repo.root,
        &["for-each-ref", "--format=%(refname) %(objectname)", &prefix],
    );
    let (name, oid) = pins.lines().next().unwrap().split_once(' ').unwrap();
    if lock {
        fs::write(
            repo.root.join(".git").join(format!("{name}.lock")),
            "locked\n",
        )
        .unwrap();
    } else {
        let hook = repo.root.join(".git/hooks/reference-transaction");
        fs::write(
            &hook,
            format!(
                r#"#!/bin/sh
[ "$1" = prepared ] || exit 0
while read old new ref; do
    if [ "$ref" = '{name}' ] && [ "$new" = 0000000000000000000000000000000000000000 ]; then
        echo 'retention deletion rejected' >&2
        exit 1
    fi
done
"#
            ),
        )
        .unwrap();
        fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut cmd = repo.arc(&repo.root);
    cmd.args(["integrate", "retention-delete"]);
    if queue {
        cmd.args(["--shape", "no-ff", "retention-next"]);
    }
    let output = cmd.assert().success().get_output().clone();
    let out = String::from_utf8(output.stdout).unwrap();
    let err = String::from_utf8(output.stderr).unwrap();
    assert!(
        err.contains("warning: integration recorded; retention cleanup failed:"),
        "{err}"
    );
    assert!(err.contains(name), "{err}");
    assert!(err.contains(&format!("refs/arc/keep/{id}/")), "{err}");
    assert!(err.contains(&format!("refs/arc/tree/{id}/")), "{err}");
    assert_eq!(git_out(&repo.root, &["rev-parse", name]).trim(), oid);
    let state = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "retention-delete", "--json"]),
    );
    assert_eq!(state["closure"]["outcome"], "integrated");
    assert!(out.contains(state["closure"]["event_id"].as_str().unwrap()));
    if queue {
        let next = json_stdout(
            repo.arc(&repo.root)
                .args(["status", "retention-next", "--json"]),
        );
        assert_eq!(next["closure"]["outcome"], "integrated");
        assert_eq!(out.matches("integrated: ").count(), 2);
    }
}

#[test]
fn integrate_warns_on_retention_head_ref_lock_after_closure() {
    retention_deletion_failure_after_merge(false, true, false);
}

#[test]
fn integrate_warns_on_retention_tree_ref_lock_after_closure() {
    retention_deletion_failure_after_merge(true, true, false);
}

#[test]
fn integrate_warns_on_retention_head_deletion_hook_and_continues_queue() {
    retention_deletion_failure_after_merge(false, false, true);
}

#[test]
fn integrate_warns_on_retention_tree_deletion_hook_after_closure() {
    retention_deletion_failure_after_merge(true, false, false);
}

#[test]
fn default_fast_forward_records_guarded_shape() {
    let repo = repo_with_gates();
    let before = repo.head(&repo.root);
    let (id, wt) = approved_change(&repo, "ff-shape", "ff.txt", "ff\n");
    let head = repo.head(&wt);
    stdout(repo.arc(&repo.root).args(["integrate", &id]));
    assert_eq!(repo.head(&repo.root), head);
    let event = events(&repo, &id)
        .into_iter()
        .find(|e| e["event_type"] == "change-integrated")
        .unwrap();
    assert_eq!(event["shape"], "fast-forward");
    assert_eq!(event["target_before"], before);
    assert_eq!(event["integrated_commit"], head);
    assert_ne!(event["already_contained"], true);
    assert!(event["authorization"].is_object());
}

#[test]
fn default_fast_forward_refuses_divergence_without_writes() {
    let repo = repo_with_gates();
    let (id, _) = approved_change(&repo, "ff-diverged", "ff.txt", "ff\n");
    fs::write(repo.root.join("other.txt"), "target\n").unwrap();
    git(&repo.root, &["add", "other.txt"]);
    git(&repo.root, &["commit", "-m", "feat: target"]);
    let before = repo.head(&repo.root);
    let recorded = events(&repo, &id);
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .failure()
        .stderr(predicates::str::contains(format!(
            "arc rebase {id} --verify"
        )))
        .stderr(predicates::str::contains("review"));
    assert_eq!(repo.head(&repo.root), before);
    assert_eq!(events(&repo, &id), recorded);
}

#[test]
fn fast_forward_message_refuses_before_debt_write() {
    let repo = repo_with_gates();
    let (id, _) = approved_change(&repo, "ff-message", "ff.txt", "ff\n");
    let recorded = events(&repo, &id);
    repo.arc(&repo.root)
        .args(["integrate", &id, "--message", "merge", "--debt", "owed"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--shape no-ff"));
    assert_eq!(events(&repo, &id), recorded);
}

#[test]
fn explicit_no_ff_records_merge_shape() {
    let repo = repo_with_gates();
    let before = repo.head(&repo.root);
    let (id, wt) = approved_change(&repo, "merge-shape", "merge.txt", "merge\n");
    let head = repo.head(&wt);
    let plan = json_stdout(repo.arc(&repo.root).args([
        "integrate",
        &id,
        "--shape",
        "no-ff",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(plan["shape"], "merge");
    assert_eq!(plan["merge_commit"], true);
    stdout(
        repo.arc(&repo.root)
            .args(["integrate", &id, "--shape", "no-ff"]),
    );
    let merged = repo.head(&repo.root);
    assert_ne!(merged, head);
    assert_eq!(
        git_out(&repo.root, &["rev-list", "--parents", "-n", "1", &merged]).trim(),
        format!("{merged} {before} {head}")
    );
    let event = events(&repo, &id)
        .into_iter()
        .find(|e| e["event_type"] == "change-integrated")
        .unwrap();
    assert_eq!(event["shape"], "merge");
}

#[test]
fn fast_forward_queue_stops_at_first_non_stacked_member() {
    let repo = repo_with_gates();
    let (first, first_wt) = approved_change(&repo, "ff-first", "first.txt", "first\n");
    let (second, _) =
        approved_change_blocked_by(&repo, "ff-second", "second.txt", "second\n", &[&first]);
    let (tail, _) = approved_change_blocked_by(&repo, "ff-tail", "tail.txt", "tail\n", &[&second]);
    let expected = repo.head(&first_wt);
    let second_events = events(&repo, &second);
    let tail_events = events(&repo, &tail);
    let out = stdout_any_status(
        repo.arc(&repo.root)
            .args(["integrate", &first, &second, &tail]),
    );
    assert!(out.contains(&format!("landed: {first}")), "{out}");
    assert!(out.contains(&format!("stopped: {second}")), "{out}");
    assert!(out.contains(&format!("not attempted: {tail}")), "{out}");
    assert_eq!(repo.head(&repo.root), expected);
    assert_eq!(events(&repo, &second), second_events);
    assert_eq!(events(&repo, &tail), tail_events);
}

#[test]
fn fast_forward_expected_old_refuses_target_race() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo_with_gates();
    let (id, wt) = approved_change(&repo, "ff-race", "ff.txt", "ff\n");
    let before = repo.head(&repo.root);
    let tree = git_out(&repo.root, &["rev-parse", "master^{tree}"]);
    let racer = git_out(
        &repo.root,
        &["commit-tree", &tree, "-p", &before, "-m", "race"],
    );
    let recorded = events(&repo, &id);
    let real_git =
        String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout).unwrap();
    let bin = repo.home.join("race-bin");
    fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
if [ "$1" = update-ref ] && [ "$2" = -m ]; then
    '{git}' update-ref refs/heads/master '{racer}' '{before}' || exit 1
fi
exec '{git}' "$@"
"#,
            git = real_git.trim()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "no integration event was recorded",
        ));
    assert_eq!(repo.head(&repo.root), racer);
    assert_ne!(repo.head(&repo.root), repo.head(&wt));
    assert_eq!(events(&repo, &id), recorded);
}

#[test]
fn fast_forward_checks_target_after_reference_transaction_hook() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo_with_gates();
    let (id, wt) = approved_change(&repo, "ff-hook-race", "ff.txt", "ff\n");
    let before = repo.head(&repo.root);
    let tree = git_out(&repo.root, &["rev-parse", "master^{tree}"]);
    let racer = git_out(
        &repo.root,
        &["commit-tree", &tree, "-p", &before, "-m", "race"],
    );
    let recorded = events(&repo, &id);
    let hook = repo.root.join(".git/hooks/reference-transaction");
    fs::write(
        &hook,
        format!(
            r#"#!/bin/sh
[ "$1" = committed ] || exit 0
while read old new ref; do
    if [ "$ref" = refs/heads/master ]; then
        printf '%s\n' '{racer}' > '{target}'
    fi
done
"#,
            target = repo.root.join(".git/refs/heads/master").display()
        ),
    )
    .unwrap();
    fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .failure()
        .stderr(predicates::str::contains("target moved during integration"))
        .stderr(predicates::str::contains(format!(
            "refs/heads/master={racer}"
        )))
        .stderr(predicates::str::contains(format!("checkout HEAD={racer}")))
        .stderr(predicates::str::contains(
            "no integration event was recorded",
        ))
        .stderr(predicates::str::contains(
            "git restore --source=refs/heads/master",
        ));
    assert_eq!(repo.head(&repo.root), racer);
    assert_eq!(
        git_out(&repo.root, &["write-tree"]),
        git_out(&wt, &["rev-parse", "HEAD^{tree}"])
    );
    assert_eq!(events(&repo, &id), recorded);
}

#[test]
fn fast_forward_queue_lands_stacked_prefix() {
    let repo = repo_with_gates();
    let (first, first_wt) = approved_change(&repo, "stack-first", "first.txt", "first\n");
    let first_head = repo.head(&first_wt);
    let (second, second_wt) = approved_change_based_on(
        &repo,
        "stack-second",
        "second.txt",
        "second\n",
        &[&first],
        Some(&first_head),
    );
    let second_head = repo.head(&second_wt);
    let (third, _) =
        approved_change_blocked_by(&repo, "stack-third", "third.txt", "third\n", &[&second]);
    repo.arc(&repo.root)
        .args(["integrate", &first, &second, &third, "--dry-run"])
        .assert()
        .code(11)
        .stdout(predicates::str::contains(format!("stopped: {third}")));
    repo.arc(&repo.root)
        .args(["integrate", &first, &second, &third])
        .assert()
        .code(11)
        .stderr(predicates::str::contains(format!(
            "arc rebase {third} --verify"
        )));
    assert_eq!(repo.head(&repo.root), second_head);
    for (id, head) in [(&first, &first_head), (&second, &second_head)] {
        let event = events(&repo, id)
            .into_iter()
            .find(|e| e["event_type"] == "change-integrated")
            .unwrap();
        assert_eq!(event["shape"], "fast-forward");
        assert_eq!(event["source_head"], head.as_str());
        assert_eq!(event["integrated_commit"], head.as_str());
    }
}

#[test]
fn already_contained_plan_and_state_report_shape() {
    let repo = repo_with_gates();
    let (id, _) = verification_only(&repo, "shape-contained", "master", &[]);
    let before = repo.head(&repo.root);
    let plan = json_stdout(
        repo.arc(&repo.root)
            .args(["integrate", &id, "--dry-run", "--json"]),
    );
    assert_eq!(plan["shape"], "already-contained");
    assert_eq!(plan["merge_commit"], false);
    stdout(repo.arc(&repo.root).args(["integrate", &id]));
    let state = json_stdout(repo.arc(&repo.root).args(["show", &id, "--json"]));
    assert_eq!(state["closure"]["shape"], "already-contained");
    assert_eq!(state["closure"]["target_before"], before);
    assert_eq!(state["closure"]["integrated_commit"], before);
    assert!(stdout(repo.arc(&repo.root).args(["log", &id])).contains("already contained"));
}

#[test]
fn fast_forward_unrelated_target_refuses_with_rebase_guidance() {
    let repo = repo_with_gates();
    let (id, _) = approved_change(&repo, "ff-unrelated", "ff.txt", "ff\n");
    let tree = git_out(&repo.root, &["rev-parse", "master^{tree}"]);
    let root = git_out(&repo.root, &["commit-tree", &tree, "-m", "unrelated root"]);
    git(&repo.root, &["update-ref", "refs/heads/master", &root]);
    let recorded = events(&repo, &id);
    for flags in [vec![], vec!["--dry-run"], vec!["--dry-run", "--json"]] {
        repo.arc(&repo.root)
            .args(["integrate", &id])
            .args(flags)
            .assert()
            .code(11)
            .stderr(predicates::str::contains(format!(
                "arc rebase {id} --verify"
            )));
        assert_eq!(repo.head(&repo.root), root);
        assert_eq!(events(&repo, &id), recorded);
    }
}

fn style_policy(repo: &Repo, text: &str) {
    fs::write(repo.root.join(".arc/policy.toml"), text).unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "style policy"]);
}

#[test]
fn integration_style_uses_target_defaults_and_flag() {
    let repo = repo_with_gates();
    style_policy(&repo, "[defaults.integration]\nshape = 'no-ff'\n");
    let (id, wt) = approved_change(&repo, "target-style", "style.txt", "style\n");
    // The change's policy does not supply the target's style.
    fs::write(
        wt.join(".arc/policy.toml"),
        "[defaults.integration]\nshape = 'ff-only'\n",
    )
    .unwrap();
    git(&wt, &["add", ".arc/policy.toml"]);
    git(&wt, &["commit", "-m", "change policy"]);
    stdout(repo.arc(&wt).args(["snapshot", &id]));
    stdout(repo.arc(&wt).args(["verify", &id, "--gate", "smoke"]));
    stdout(repo.arc(&wt).args(["review", &id, "--verdict", "approved"]));
    let plan = json_stdout(
        repo.arc(&repo.root)
            .args(["integrate", &id, "--dry-run", "--json"]),
    );
    assert_eq!(plan["shape"], "merge");
    let flag = json_stdout(repo.arc(&repo.root).args([
        "integrate",
        &id,
        "--shape",
        "ff-only",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(flag["shape"], "fast-forward");
    stdout(
        repo.arc(&repo.root)
            .args(["integrate", &id, "--message", "style merge"]),
    );
    assert_ne!(repo.head(&repo.root), repo.head(&wt));
}

#[test]
fn integration_style_queue_resolves_each_target() {
    let repo = repo_with_gates();
    style_policy(&repo, "[defaults.integration]\nshape = 'no-ff'\n");
    git(&repo.root, &["branch", "other"]);
    style_policy(&repo, "[defaults.integration]\nshape = 'ff-only'\n");
    let (first, _) = approved_change(&repo, "master-style", "first-style.txt", "first\n");
    let other = repo.home.join("other-target");
    git(
        &repo.root,
        &["worktree", "add", other.to_str().unwrap(), "other"],
    );
    let second = opened_change_id(&stdout(repo.arc(&other).args([
        "begin",
        "other-style",
        "--target",
        "other",
    ])));
    let state = json_stdout(repo.arc(&other).args(["show", &second, "--json"]));
    let wt = PathBuf::from(state["worktree"].as_str().unwrap());
    fs::write(wt.join("other-style.txt"), "other\n").unwrap();
    git(&wt, &["add", "other-style.txt"]);
    git(&wt, &["commit", "-m", "other"]);
    stdout(repo.arc(&wt).args(["snapshot", &second]));
    stdout(repo.arc(&wt).args(["verify", &second, "--gate", "smoke"]));
    stdout(
        repo.arc(&wt)
            .args(["review", &second, "--verdict", "approved"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["integrate", &first, &second, "--dry-run"]),
    );
    stdout(repo.arc(&repo.root).args(["integrate", &first, &second]));
    for (id, shape) in [(&first, "fast-forward"), (&second, "merge")] {
        let integrated = events(&repo, id)
            .into_iter()
            .find(|e| e["event_type"] == "change-integrated")
            .unwrap();
        assert_eq!(integrated["shape"], shape);
    }
}

#[test]
fn integration_style_flag_outside_constraints_refuses() {
    let repo = repo_with_gates();
    style_policy(&repo, "[integration]\nshapes = ['ff-only']\n");
    let (id, _) = approved_change(&repo, "constraint-style", "constraint.txt", "constraint\n");
    fs::write(
        repo.root.join(".git/arc/operator-policy.toml"),
        "[integration]\nshapes = ['ff-only', 'no-ff']\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["integrate", &id, "--shape", "no-ff", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("integration.shape"))
        .stderr(predicates::str::contains("flag"))
        .stderr(predicates::str::contains("project:"))
        .stderr(predicates::str::contains("operator:"));
}

#[test]
fn integration_history_squash_refuses_multiple_commits_locally() {
    let repo = repo_with_gates();
    style_policy(&repo, "[integration]\nhistory = 'squash'\n");
    let (id, wt) = approved_change(&repo, "history-style", "history.txt", "one\n");
    fs::write(wt.join("history.txt"), "two\n").unwrap();
    git(&wt, &["add", "history.txt"]);
    git(&wt, &["commit", "-m", "second"]);
    stdout(repo.arc(&wt).args(["snapshot", &id]));
    stdout(repo.arc(&wt).args(["verify", &id, "--gate", "smoke"]));
    stdout(repo.arc(&wt).args(["review", &id, "--verdict", "approved"]));
    repo.arc(&repo.root)
        .args(["integrate", &id, "--dry-run"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("integration history is squash"));
}

#[test]
fn integration_history_one_commit_remains_local() {
    let repo = repo_with_gates();
    style_policy(&repo, "[integration]\nhistory='squash'\n");
    let (id, wt) = approved_change(&repo, "local-history", "local.txt", "local\n");
    let head = repo.head(&wt);
    stdout(repo.arc(&repo.root).args(["integrate", &id]));
    assert_eq!(repo.head(&repo.root), head);
    assert!(events(&repo, &id)
        .iter()
        .any(|e| e["event_type"] == "change-integrated"));
}

#[test]
fn integration_history_folds_operator_and_contribution_alias() {
    for (project, operator, expected) in [
        (
            "[integration]\nhistory='preserve'\n",
            "[integration]\nhistory='squash'\n",
            "integration history",
        ),
        (
            "[contribution]\nhistory='preserve'\n",
            "[integration]\nhistory='squash'\n",
            "contribution history",
        ),
        (
            "[integration]\nhistory='preserve'\n",
            "[contribution]\nhistory='squash'\n",
            "contribution history",
        ),
    ] {
        let repo = repo_with_gates();
        style_policy(&repo, project);
        fs::create_dir_all(repo.root.join(".git/arc")).unwrap();
        fs::write(repo.root.join(".git/arc/operator-policy.toml"), operator).unwrap();
        let (id, wt) = approved_change(&repo, "fold-history", "fold.txt", "one\n");
        fs::write(wt.join("fold.txt"), "two\n").unwrap();
        git(&wt, &["add", "fold.txt"]);
        git(&wt, &["commit", "-m", "second"]);
        stdout(repo.arc(&wt).args(["snapshot", &id]));
        stdout(repo.arc(&wt).args(["verify", &id, "--gate", "smoke"]));
        stdout(repo.arc(&wt).args(["review", &id, "--verdict", "approved"]));
        repo.arc(&repo.root)
            .args(["integrate", &id, "--dry-run"])
            .assert()
            .failure()
            .stderr(predicates::str::contains(expected));
    }
}

#[test]
fn cleanup_safety_ignored_content_keeps_worktree() {
    let repo = repo_with_gates();
    repo.commit(
        &repo.root,
        ".gitignore",
        ".env\n",
        "test: ignore local credentials",
    );
    let (id, wt) = approved_change(&repo, "ignored-cleanup", "change.txt", "change\n");
    fs::write(wt.join(".env"), "local bytes\n").unwrap();
    let out = stdout(repo.arc(&repo.root).args(["integrate", &id, "--cleanup"]));
    assert!(wt.exists(), "{out}");
    assert_eq!(
        fs::read_to_string(wt.join(".env")).unwrap(),
        "local bytes\n"
    );
    assert!(
        out.contains("holds local content not in the branch: .env"),
        "{out}"
    );
    assert!(out.contains("kept branch"), "{out}");
}

#[test]
fn cleanup_safety_replacement_worktree_is_kept() {
    let repo = repo_with_gates();
    let (id, wt) = approved_change(&repo, "replacement-cleanup", "change.txt", "change\n");
    let moved = repo.home.join("moved-checkout");
    git(
        &repo.root,
        &[
            "worktree",
            "move",
            wt.to_str().unwrap(),
            moved.to_str().unwrap(),
        ],
    );
    git(&moved, &["checkout", "--detach"]);
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            wt.to_str().unwrap(),
            "arc/replacement-cleanup",
        ],
    );
    let out = stdout(repo.arc(&repo.root).args(["integrate", &id, "--cleanup"]));
    assert!(wt.exists(), "{out}");
    assert!(out.contains("ownership cannot be established"), "{out}");
    assert!(moved.exists());
    assert!(out.contains("kept branch"), "{out}");
}

#[test]
fn cleanup_safety_tracked_edits_keep_branch_and_closure() {
    dirty_cleanup("README.md", "tracked edit\n");
}

#[test]
fn cleanup_safety_untracked_content_keeps_branch_and_closure() {
    dirty_cleanup("untracked.txt", "untracked\n");
}

fn dirty_cleanup(file: &str, content: &str) {
    let repo = repo_with_gates();
    let (id, wt) = approved_change(&repo, "dirty-cleanup", "change.txt", "change\n");
    fs::write(wt.join(file), content).unwrap();
    let out = stdout(repo.arc(&repo.root).args(["integrate", &id, "--cleanup"]));
    assert!(
        out.contains("holds local content not in the branch"),
        "{out}"
    );
    assert!(out.contains("kept branch"), "{out}");
    assert!(wt.exists());
    assert!(!git_out(&repo.root, &["branch", "--list", "arc/dirty-cleanup"]).is_empty());
    let status = json_stdout(repo.arc(&repo.root).args(["status", &id]));
    assert_eq!(status["closure"]["outcome"], "integrated");
}

#[test]
fn cleanup_safety_adopted_worktree_is_kept() {
    let repo = repo_with_gates();
    let wt = repo.home.join("adopted");
    git(
        &repo.root,
        &["worktree", "add", "-b", "adopted", wt.to_str().unwrap()],
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "adopted", "--adopt", "adopted"]),
    );
    repo.commit(&wt, "adopted.txt", "adopted\n", "fix: adopted");
    stdout(repo.arc(&wt).args(["snapshot", "adopted", "--verify"]));
    stdout(
        repo.arc(&wt)
            .args(["review", "adopted", "--verdict", "approved"]),
    );
    let out = stdout(
        repo.arc(&repo.root)
            .args(["integrate", "adopted", "--cleanup"]),
    );
    assert!(out.contains("not created by arc"), "{out}");
    assert!(out.contains("kept branch"), "{out}");
    assert!(wt.exists());
}

#[test]
fn cleanup_safety_branch_failure_warns_and_queue_continues() {
    let repo = repo_with_gates();
    let (first, _) = approved_change(&repo, "locked-first", "first.txt", "first\n");
    let (second, _) = approved_change_based_on(
        &repo,
        "locked-second",
        "second.txt",
        "second\n",
        &[&first],
        Some("arc/locked-first"),
    );
    let lock = repo.root.join(".git/refs/heads/arc/locked-first.lock");
    fs::write(&lock, "").unwrap();
    let output = repo
        .arc(&repo.root)
        .args(["integrate", "--tag", "series", "--cleanup"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("warning: integration recorded; cleanup"),
        "{err}"
    );
    for id in [first, second] {
        let status = json_stdout(repo.arc(&repo.root).args(["status", &id]));
        assert_eq!(status["closure"]["outcome"], "integrated");
    }
}

#[test]
fn cleanup_safety_legacy_worktree_is_kept() {
    let repo = repo_with_gates();
    let (id, wt) = approved_change(&repo, "legacy-cleanup", "legacy.txt", "legacy\n");
    let dir = repo.root.join(".git/arc/changes").join(&id).join("events");
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let mut event: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] == "change-opened" {
            event.as_object_mut().unwrap().remove("worktree_created");
            fs::write(path, serde_json::to_vec(&event).unwrap()).unwrap();
        }
    }
    let out = stdout(repo.arc(&repo.root).args(["integrate", &id, "--cleanup"]));
    assert!(out.contains("not created by arc"), "{out}");
    assert!(wt.exists());
    let state = json_stdout(repo.arc(&repo.root).args(["show", &id, "--json"]));
    assert_eq!(state["worktree_created"], false);
}

#[test]
fn cleanup_safety_single_branch_failure_warns_after_closure() {
    let repo = repo_with_gates();
    let (id, wt) = approved_change(&repo, "locked-single", "single.txt", "single\n");
    fs::write(repo.root.join(".git/refs/heads/arc/locked-single.lock"), "").unwrap();
    let output = repo
        .arc(&repo.root)
        .args(["integrate", &id, "--cleanup"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("warning: integration recorded; cleanup branch"),
        "{err}"
    );
    assert!(!wt.exists());
    let status = json_stdout(repo.arc(&repo.root).args(["status", &id]));
    assert_eq!(status["closure"]["outcome"], "integrated");
}

#[test]
fn cleanup_safety_outside_sandbox_keeps_worktree_and_branch() {
    let repo = repo_with_gates();
    let (id, wt) = approved_change(&repo, "outside-cleanup", "outside.txt", "outside\n");
    let out = stdout(repo.arc(&repo.root).env("ARC_SANDBOX", &repo.root).args([
        "integrate",
        &id,
        "--cleanup",
    ]));
    assert!(out.contains("outside the sandbox"), "{out}");
    assert!(out.contains("kept branch"), "{out}");
    assert!(wt.exists());
}

fn cleanup_default(repo: &Repo, source: &str, enabled: bool) {
    let user = format!("[change]\ncheckout='worktree'\n[integration]\ncleanup={enabled}\n");
    let defaults = format!("[defaults.integration]\ncleanup={enabled}\n");
    match source {
        "user" => fs::write(repo.home.join(".local/ai/arc/config.toml"), user).unwrap(),
        "project" => {
            fs::write(repo.root.join(".arc/policy.toml"), defaults).unwrap();
            git(&repo.root, &["add", ".arc/policy.toml"]);
            git(
                &repo.root,
                &["commit", "-m", "test: declare cleanup default"],
            );
        }
        "operator" => {
            fs::create_dir_all(repo.root.join(".git/arc")).unwrap();
            fs::write(repo.root.join(".git/arc/operator-policy.toml"), defaults).unwrap();
        }
        _ => panic!("unknown fixture source"),
    }
}

fn assert_cleanup_default(source: &str, enabled: bool, flag: Option<&str>, removed: bool) {
    let repo = repo_with_gates();
    cleanup_default(&repo, source, enabled);
    let (id, wt) = approved_change(&repo, "default-cleanup", "change.txt", "change\n");
    let mut command = repo.arc(&repo.root);
    command.args(["integrate", &id]);
    if let Some(flag) = flag {
        command.arg(flag);
    }
    stdout(&mut command);
    assert_eq!(!wt.exists(), removed);
    assert_eq!(
        git_out(&repo.root, &["branch", "--list", "arc/default-cleanup"]).is_empty(),
        removed
    );
}

#[test]
fn cleanup_default_user() {
    assert_cleanup_default("user", true, None, true);
}

#[test]
fn cleanup_default_project() {
    assert_cleanup_default("project", true, None, true);
}

#[test]
fn cleanup_default_operator() {
    assert_cleanup_default("operator", true, None, true);
}

#[test]
fn cleanup_default_no_cleanup_overrides_true() {
    assert_cleanup_default("operator", true, Some("--no-cleanup"), false);
}

#[test]
fn cleanup_default_cleanup_overrides_false() {
    assert_cleanup_default("operator", false, Some("--cleanup"), true);
}

#[test]
fn cleanup_default_queue_resolves_each_target() {
    let repo = repo_with_gates();
    cleanup_default(&repo, "project", true);
    git(&repo.root, &["branch", "clean-target"]);
    cleanup_default(&repo, "project", false);
    let (keep_id, keep) = approved_change(&repo, "queue-keep", "keep.txt", "keep\n");
    let target_checkout = repo.home.join("clean-target-checkout");
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            target_checkout.to_str().unwrap(),
            "clean-target",
        ],
    );
    let clean_id = opened_change_id(&stdout(repo.arc(&target_checkout).args([
        "begin",
        "queue-clean",
        "--target",
        "clean-target",
    ])));
    let state = json_stdout(
        repo.arc(&target_checkout)
            .args(["show", &clean_id, "--json"]),
    );
    let clean = PathBuf::from(state["worktree"].as_str().unwrap());
    fs::write(clean.join("clean.txt"), "clean\n").unwrap();
    git(&clean, &["add", "clean.txt"]);
    git(&clean, &["commit", "-m", "test: clean target change"]);
    stdout(repo.arc(&clean).args(["snapshot", &clean_id]));
    stdout(
        repo.arc(&clean)
            .args(["verify", &clean_id, "--gate", "smoke"]),
    );
    stdout(
        repo.arc(&clean)
            .args(["review", &clean_id, "--verdict", "approved"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["integrate", &keep_id, &clean_id]),
    );
    assert!(!clean.exists());
    assert!(keep.exists());
    assert!(git_out(&repo.root, &["branch", "--list", "arc/queue-clean"]).is_empty());
    assert!(!git_out(&repo.root, &["branch", "--list", "arc/queue-keep"]).is_empty());
}

#[test]
fn cleanup_default_flags_conflict() {
    let repo = repo_with_gates();
    repo.arc(&repo.root)
        .args(["integrate", "example", "--cleanup", "--no-cleanup"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be used with"));
}

#[test]
fn fast_forward_index_lock_preserves_target_and_checkout() {
    let repo = repo_with_gates();
    let (id, _) = approved_change(&repo, "ff-index-lock", "ff.txt", "ff\n");
    let before = repo.head(&repo.root);
    let index = fs::read(repo.root.join(".git/index")).unwrap();
    let recorded = events(&repo, &id);
    fs::write(repo.root.join(".git/index.lock"), "operator lock").unwrap();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .failure();
    assert_eq!(repo.head(&repo.root), before);
    assert_eq!(fs::read(repo.root.join(".git/index")).unwrap(), index);
    assert_eq!(events(&repo, &id), recorded);
    assert!(!repo.root.join("ff.txt").exists());
}

#[test]
fn fast_forward_preflights_agree_on_clean_divergence() {
    let repo = repo_with_gates();
    let (id, wt) = approved_change(&repo, "ff-entrypoints", "ff.txt", "ff\n");
    stdout(
        repo.arc(&wt)
            .args(["review", &id, "--verdict", "approved"])
            .env("ARC_ACTOR", "independent-reviewer")
            .env("ARC_SESSION", "review-session"),
    );
    let (second, _) =
        approved_change_blocked_by(&repo, "ff-entrypoints-tail", "tail.txt", "tail\n", &[&id]);
    fs::write(repo.root.join("sibling.txt"), "target\n").unwrap();
    git(&repo.root, &["add", "sibling.txt"]);
    git(&repo.root, &["commit", "-m", "feat: sibling"]);
    stdout(
        repo.arc(&repo.root)
            .args(["verify", &id, "--against", "master"]),
    );
    let before = repo.head(&repo.root);
    let recorded = events(&repo, &id);
    let status = json_stdout(repo.arc(&repo.root).args(["status", &id, "--json"]));
    assert_eq!(status["ready_to_integrate"], false);
    assert_eq!(status["needs_rebase"], true);
    assert_eq!(status["next_action"], "rebase");
    for args in [
        vec!["check", &id],
        vec!["integrate", &id, &second],
        vec!["integrate", &id, &second, "--dry-run"],
        vec!["integrate", &id],
        vec!["integrate", &id, "--dry-run"],
        vec!["integrate", &id, "--dry-run", "--json"],
        vec!["integrate", &id, "--debt", "owed"],
    ] {
        let output = repo.arc(&repo.root).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(11));
        let messages = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            messages.contains(&format!("arc rebase {id} --verify")),
            "{messages}"
        );
        assert_eq!(repo.head(&repo.root), before);
        assert_eq!(events(&repo, &id), recorded);
    }
    stdout(
        repo.arc(&repo.root)
            .args(["check", &id, "--shape", "no-ff"]),
    );
}

#[test]
fn fast_forward_queue_dry_run_stops_before_projecting_held_member() {
    let repo = repo_with_gates();
    let (first, _) = approved_change(&repo, "ff-held", "first.txt", "first\n");
    let (second, _) = approved_change_blocked_by(
        &repo,
        "ff-held-sibling",
        "second.txt",
        "second\n",
        &[&first],
    );
    stdout(
        repo.arc(&repo.root)
            .args(["hold", &first, "--reason", "wait"]),
    );
    let out =
        stdout_any_status(
            repo.arc(&repo.root)
                .args(["integrate", &first, &second, "--dry-run"]),
        );
    assert!(out.contains(&format!("stopped: {first}")), "{out}");
    assert!(!out.contains(&format!("would land: {first}")), "{out}");
    assert!(out.contains(&format!("not attempted: {second}")), "{out}");
}

#[test]
fn fast_forward_debt_expected_basis_names_moved_target() {
    let repo = repo_with_gates();
    let (id, _) = approved_change(&repo, "ff-debt-basis", "ff.txt", "ff\n");
    let plan = stdout(
        repo.arc(&repo.root)
            .args(["integrate", &id, "--dry-run", "--json"]),
    );
    let path = repo.home.join("plan.json");
    fs::write(&path, plan).unwrap();
    fs::write(repo.root.join("sibling.txt"), "target\n").unwrap();
    git(&repo.root, &["add", "sibling.txt"]);
    git(&repo.root, &["commit", "-m", "feat: sibling"]);
    let recorded = events(&repo, &id);
    repo.arc(&repo.root)
        .args([
            "integrate",
            &id,
            "--debt",
            "owed",
            "--expect-basis",
            path.to_str().unwrap(),
        ])
        .assert()
        .code(11)
        .stderr(predicates::str::contains("target you checked moved"));
    assert_eq!(events(&repo, &id), recorded);
}

#[test]
fn fast_forward_into_divergence_names_selected_destination() {
    let repo = repo_with_gates();
    let (id, _) = approved_change(&repo, "ff-into-guidance", "ff.txt", "ff\n");
    git(&repo.root, &["checkout", "-b", "release"]);
    fs::write(repo.root.join("sibling.txt"), "target\n").unwrap();
    git(&repo.root, &["add", "sibling.txt"]);
    git(&repo.root, &["commit", "-m", "feat: sibling"]);
    repo.arc(&repo.root)
        .args(["integrate", &id, "--into", "release"])
        .assert()
        .code(11)
        .stderr(predicates::str::contains("git rebase release"))
        .stderr(predicates::str::contains(format!(
            "arc snapshot {id} --verify"
        )));
}

#[test]
fn fast_forward_debt_checks_divergence_after_waiting_for_target_lock() {
    let repo = repo_with_gates();
    let (id, _) = approved_change(&repo, "ff-debt-lock", "ff.txt", "ff\n");
    let recorded = events(&repo, &id);
    let lock = hold_target_lock(&repo, "master");
    let mut child = spawn_arc(&repo, &repo.root, &["integrate", &id, "--debt", "owed"]);
    assert_waiting_on_transition_lock(&mut [&mut child]);
    repo.commit(&repo.root, "sibling.txt", "target\n", "feat: sibling");
    let target = repo.head(&repo.root);
    lock.unlock().unwrap();
    assert_eq!(wait_for_exit(&mut child).code(), Some(11));
    assert_eq!(repo.head(&repo.root), target);
    assert_eq!(events(&repo, &id), recorded);
}

#[test]
fn audit_fix_stacked_squash_queue_dry_run() {
    for shape in ["ff-only", "no-ff"] {
        let repo = repo_with_gates();
        style_policy(
            &repo,
            &format!("[integration]\nhistory='squash'\n[defaults.integration]\nshape='{shape}'\n"),
        );
        let (first, wt) = approved_change(&repo, "squash-first", "first.txt", "first\n");
        let head = repo.head(&wt);
        let (second, _) = approved_change_based_on(
            &repo,
            "squash-second",
            "second.txt",
            "second\n",
            &[&first],
            Some(&head),
        );
        let out = stdout(
            repo.arc(&repo.root)
                .args(["integrate", &first, &second, "--dry-run"]),
        );
        assert!(out.contains(&format!("would land: {second}")), "{out}");
        assert_eq!(
            repo.head(&repo.root),
            git_out(&wt, &["rev-parse", "master"])
        );
        stdout(repo.arc(&repo.root).args(["integrate", &first, &second]));
    }
}

#[test]
fn audit_fix_queue_shape_refusal_reports_prefix_and_tail() {
    for dry in [false, true] {
        let repo = repo_with_gates();
        style_policy(&repo, "[integration]\nshapes=['ff-only']\n");
        let (first, _) = approved_change(
            &repo,
            "constraint-first",
            ".arc/policy.toml",
            "[integration]\nshapes=['no-ff']\n",
        );
        let (second, _) = approved_change_blocked_by(
            &repo,
            "constraint-second",
            "second.txt",
            "second\n",
            &[&first],
        );
        let (third, _) = approved_change_blocked_by(
            &repo,
            "constraint-third",
            "third.txt",
            "third\n",
            &[&second],
        );
        let mut cmd = repo.arc(&repo.root);
        cmd.args(["integrate", &first, &second, &third]);
        if dry {
            cmd.arg("--dry-run");
        }
        let out = stdout_any_status(&mut cmd);
        let prefix = if dry { "would land" } else { "landed" };
        assert!(out.contains(&format!("{prefix}: {first}")), "{out}");
        assert!(out.contains(&format!("stopped: {second}")), "{out}");
        assert!(out.contains(&format!("not attempted: {third}")), "{out}");
        assert!(out.contains("outside allowed values"), "{out}");
    }
}

#[test]
fn audit_fix_malformed_constraint_tables_refuse_config_and_integration() {
    for (text, key) in [
        ("[[integration]]\nshapes=['no-ff']\n", "integration"),
        ("integration=7\n", "integration"),
        ("contribution=[]\n", "contribution"),
        ("defaults=[]\n", "defaults"),
        ("[defaults]\nintegration=[]\n", "defaults.integration"),
        ("[defaults]\nchange=false\n", "defaults.change"),
        ("[integration]\nshapes='no-ff'\n", "integration.shapes"),
        ("[integration]\nhistory=7\n", "integration.history"),
    ] {
        let repo = repo_with_gates();
        let (id, _) = approved_change(&repo, "malformed-style", "work.txt", "work\n");
        fs::write(repo.root.join(".git/arc/operator-policy.toml"), text).unwrap();
        for args in [vec!["config"], vec!["integrate", &id, "--dry-run"]] {
            repo.arc(&repo.root)
                .args(args)
                .assert()
                .failure()
                .stderr(predicates::str::contains("operator-policy.toml"))
                .stderr(predicates::str::contains(format!("key {key}")));
        }
    }
}

#[test]
fn audit_fix_guarded_shape_revalidates_operator_snapshot() {
    let repo = repo_with_gates();
    let arm = repo.root.join("arm-policy-change");
    let policy = repo.root.join(".git/arc/operator-policy.toml");
    let command = format!("if test -f '{}'; then printf '[integration]\\nshapes=[\"no-ff\"]\\n' > '{}'; fi; printf stable", arm.display(), policy.display());
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!(
            "[gates.smoke]\ncommand = 'test -f README.md'\nenvironment = {}\n",
            serde_json::to_string(&command).unwrap(),
        ),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "declare environment probe"]);
    let (id, _) = approved_change(&repo, "guarded-style", "work.txt", "work\n");
    let before = repo.head(&repo.root);
    fs::write(arm, "armed").unwrap();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .failure()
        .stderr(predicates::str::contains("configuration changed"));
    assert_eq!(repo.head(&repo.root), before);
    assert!(!events(&repo, &id)
        .iter()
        .any(|event| event["event_type"] == "change-integrated"));
}

#[test]
fn audit_fix_shape_revalidates_after_waiting_for_target_lock() {
    for policy in [
        "[integration]\nshapes=['no-ff']\n",
        "[defaults.integration]\nshape='no-ff'\n",
    ] {
        let repo = repo_with_gates();
        let (id, _) = approved_change(&repo, "locked-style", "work.txt", "work\n");
        let recorded = events(&repo, &id);
        let before = repo.head(&repo.root);
        let lock = hold_target_lock(&repo, "master");
        let mut child =
            spawn_arc_keeping_stderr(&repo, &repo.root, &["integrate", &id], "session-a");
        assert_waiting_on_transition_lock(&mut [&mut child]);
        fs::write(repo.root.join(".git/arc/operator-policy.toml"), policy).unwrap();
        lock.unlock().unwrap();
        assert_eq!(wait_for_exit(&mut child).code(), Some(1));
        assert!(child_stderr(&mut child).contains("configuration changed"));
        assert_eq!(repo.head(&repo.root), before);
        assert_eq!(events(&repo, &id), recorded);
    }
}

#[test]
fn audit_fix_original_shape_flag_overrides_changed_default_under_lock() {
    let repo = repo_with_gates();
    let (id, wt) = approved_change(&repo, "locked-flag", "work.txt", "work\n");
    let head = repo.head(&wt);
    let lock = hold_target_lock(&repo, "master");
    let mut child = spawn_arc(&repo, &repo.root, &["integrate", &id, "--shape", "ff-only"]);
    assert_waiting_on_transition_lock(&mut [&mut child]);
    fs::write(
        repo.root.join(".git/arc/operator-policy.toml"),
        "[defaults.integration]\nshape='no-ff'\n",
    )
    .unwrap();
    lock.unlock().unwrap();
    assert_eq!(wait_for_exit(&mut child).code(), Some(0));
    assert_eq!(repo.head(&repo.root), head);
}

#[test]
fn audit_fix_queue_history_refusal_reports_prefix_and_tail() {
    for dry in [false, true] {
        let repo = repo_with_gates();
        style_policy(&repo, "[integration]\nhistory='squash'\n");
        let (first, wt) = approved_change(&repo, "history-first", "first.txt", "first\n");
        let head = repo.head(&wt);
        let (second, wt) = approved_change_based_on(
            &repo,
            "history-second",
            "second.txt",
            "second\n",
            &[&first],
            Some(&head),
        );
        repo.commit(&wt, "extra.txt", "extra\n", "feat: extra");
        stdout(repo.arc(&wt).args(["snapshot", &second]));
        stdout(repo.arc(&wt).args(["verify", &second, "--gate", "smoke"]));
        stdout(
            repo.arc(&wt)
                .args(["review", &second, "--verdict", "approved"]),
        );
        let (third, _) =
            approved_change_blocked_by(&repo, "history-third", "third.txt", "third\n", &[&second]);
        let mut cmd = repo.arc(&repo.root);
        cmd.args(["integrate", &first, &second, &third]);
        if dry {
            cmd.arg("--dry-run");
        }
        let out = stdout_any_status(&mut cmd);
        let prefix = if dry { "would land" } else { "landed" };
        assert!(out.contains(&format!("{prefix}: {first}")), "{out}");
        assert!(out.contains(&format!("stopped: {second}")), "{out}");
        assert!(out.contains(&format!("not attempted: {third}")), "{out}");
        assert!(out.contains("integration history is squash"), "{out}");
    }
}

#[test]
fn cleanup_default_keeps_operator_in_place_checkout() {
    let repo = repo_with_gates();
    fs::write(
        repo.home.join(".local/ai/arc/config.toml"),
        "[integration]\ncleanup=true\n",
    )
    .unwrap();
    let wt = repo.home.join("operator-checkout");
    git(
        &repo.root,
        &["worktree", "add", "-b", "operator", wt.to_str().unwrap()],
    );
    let id = opened_change_id(&stdout(repo.arc(&wt).args([
        "begin",
        "operator-in-place",
        "--target",
        "master",
    ])));
    repo.commit(&wt, "change.txt", "change\n", "feat: change");
    stdout(repo.arc(&wt).args(["snapshot", &id]));
    stdout(repo.arc(&wt).args(["verify", &id, "--gate", "smoke"]));
    stdout(repo.arc(&wt).args(["review", &id, "--verdict", "approved"]));
    let out = stdout(repo.arc(&repo.root).args(["integrate", &id]));
    assert!(wt.exists(), "{out}");
    assert!(out.contains("not created by arc"), "{out}");
}

#[test]
fn cleanup_safety_unestablished_instance_ownership_is_kept() {
    for fault in ["missing", "token", "change", "malformed", "legacy"] {
        let repo = repo_with_gates();
        let (id, wt) = approved_change(&repo, "unowned", "change.txt", "change\n");
        let admin = PathBuf::from(git_out(&wt, &["rev-parse", "--absolute-git-dir"]));
        let path = admin.join("arc-owner");
        match fault {
            "missing" => fs::remove_file(&path).unwrap(),
            "malformed" => fs::write(&path, "invalid").unwrap(),
            "legacy" => {
                let events = repo.root.join(".git/arc/changes").join(&id).join("events");
                for entry in fs::read_dir(events).unwrap() {
                    let path = entry.unwrap().path();
                    let mut event: serde_json::Value =
                        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    if event["event_type"] == "change-opened" {
                        event
                            .as_object_mut()
                            .unwrap()
                            .remove("worktree_owner_token");
                        fs::write(path, serde_json::to_vec(&event).unwrap()).unwrap();
                    }
                }
            }
            _ => {
                let mut owner: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                let field = if fault == "token" {
                    "token"
                } else {
                    "change_id"
                };
                owner[field] = "other".into();
                fs::write(path, serde_json::to_vec(&owner).unwrap()).unwrap();
            }
        }
        let out = stdout(repo.arc(&repo.root).args(["integrate", &id, "--cleanup"]));
        assert!(wt.exists(), "{fault}: {out}");
        assert!(
            out.contains("ownership cannot be established"),
            "{fault}: {out}"
        );
        assert!(out.contains("kept branch"), "{fault}: {out}");
    }
}

#[test]
fn fast_forward_ignores_merge_shaping_configuration() {
    for (key, value) in [
        ("branch.master.mergeOptions", "-s ours"),
        ("merge.ff", "false"),
        ("branch.master.mergeOptions", "--squash"),
    ] {
        let repo = repo_with_gates();
        let before = repo.head(&repo.root);
        let (id, wt) = approved_change(&repo, "configured-ff", "ff.txt", "ff\n");
        let approved = repo.head(&wt);
        git(&repo.root, &["config", key, value]);
        stdout(repo.arc(&repo.root).args(["integrate", &id]));
        assert_eq!(repo.head(&repo.root), approved, "{key}={value}");
        assert_eq!(
            fs::read_to_string(repo.root.join("ff.txt")).unwrap(),
            "ff\n"
        );
        assert_eq!(
            git_out(&repo.root, &["write-tree"]),
            git_out(&wt, &["write-tree"])
        );
        let closure = events(&repo, &id)
            .into_iter()
            .find(|e| e["event_type"] == "change-integrated")
            .unwrap();
        assert_eq!(closure["target_before"], before);
    }
}

#[test]
fn no_ff_ignores_merge_shaping_configuration() {
    let repo = repo_with_gates();
    let before = repo.head(&repo.root);
    let (id, wt) = approved_change(&repo, "configured-merge", "ff.txt", "ff\n");
    let approved = repo.head(&wt);
    git(
        &repo.root,
        &["config", "branch.master.mergeOptions", "--squash -s ours"],
    );
    git(&repo.root, &["config", "merge.ff", "only"]);
    stdout(
        repo.arc(&repo.root)
            .args(["integrate", &id, "--shape", "no-ff"]),
    );
    assert_eq!(
        git_out(&repo.root, &["show", "-s", "--format=%P", "HEAD"]),
        format!("{before} {approved}")
    );
    assert_eq!(
        fs::read_to_string(repo.root.join("ff.txt")).unwrap(),
        "ff\n"
    );
}

#[test]
fn fast_forward_same_tree_intermediate_race_refuses() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo_with_gates();
    let before = repo.head(&repo.root);
    let (id, wt) = approved_change(&repo, "same-tree-race", "ff.txt", "ff\n");
    let approved = repo.head(&wt);
    let approved_tree = git_out(&wt, &["rev-parse", "HEAD^{tree}"]);
    let target_tree = git_out(&repo.root, &["rev-parse", "HEAD^{tree}"]);
    let intermediate = git_out(
        &wt,
        &[
            "commit-tree",
            &target_tree,
            "-p",
            &before,
            "-m",
            "intermediate",
        ],
    );
    let successor = git_out(
        &wt,
        &[
            "commit-tree",
            &approved_tree,
            "-p",
            &intermediate,
            "-m",
            "successor",
        ],
    );
    git(&wt, &["reset", "--hard", &successor]);
    stdout(repo.arc(&wt).args(["snapshot", &id, "--verify"]));
    approve(&repo, &id);
    assert_ne!(approved, successor);
    let recorded = events(&repo, &id);
    let real_git =
        String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout).unwrap();
    let bin = repo.home.join("race-bin");
    fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(&wrapper, format!(r#"#!/bin/sh
if {{ [ "$1" = merge ] && [ "$2" = --ff-only ]; }} || {{ [ "$1" = update-ref ] && [ "$2" = -m ]; }}; then
    '{git}' update-ref refs/heads/master '{intermediate}' '{before}' || exit 1
fi
exec '{git}' "$@"
"#, git = real_git.trim())).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "no integration event was recorded",
        ));
    assert_eq!(repo.head(&repo.root), intermediate);
    assert_eq!(events(&repo, &id), recorded);
}

#[test]
fn fast_forward_closure_publication_failure_reports_recovery() {
    closure_publication_failure("unknown");
}

#[test]
fn fast_forward_closure_fsync_failure_reports_published_event() {
    closure_publication_failure("published");
}

#[test]
fn fast_forward_closure_append_failure_reports_absent_event() {
    closure_publication_failure("absent");
}

fn closure_publication_failure(publication: &str) {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo_with_gates();
    let (id, wt) = approved_change(&repo, "publication-failure", "ff.txt", "ff\n");
    let approved = repo.head(&wt);
    let dir = repo.root.join(".git/arc/changes").join(&id).join("events");
    let backup = repo.home.join("saved-events");
    let real_git =
        String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout).unwrap();
    let action = match publication {
        "unknown" => format!(
            "mv '{}' '{}' && printf blocked > '{}'",
            dir.display(),
            backup.display(),
            dir.display()
        ),
        "published" => format!("chmod 300 '{}'", dir.display()),
        "absent" => format!("chmod 500 '{}'", dir.display()),
        _ => unreachable!(),
    };
    let bin = repo.home.join("publication-bin");
    fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
if {{ [ "$1" = merge ] && [ "$2" = --ff-only ]; }} || [ "$1" = read-tree ]; then
    '{git}' "$@" || exit $?
    {action} || exit 1
    exit 0
fi
exec '{git}' "$@"
"#,
            git = real_git.trim(),
            action = action
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .assert()
        .failure()
        .stderr(predicates::str::contains("closure publication"))
        .stderr(predicates::str::contains(format!(
            "refs/heads/master={approved}"
        )))
        .stderr(predicates::str::contains(format!(
            "publication state={publication}"
        )));
    assert_eq!(repo.head(&repo.root), approved);
    if publication == "unknown" {
        fs::remove_file(&dir).unwrap();
        fs::rename(&backup, &dir).unwrap();
    } else {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(
        events(&repo, &id)
            .iter()
            .any(|e| e["event_type"] == "change-integrated"),
        publication == "published"
    );
}

fn destination_policy(repo: &Repo, text: &str) {
    git(&repo.root, &["checkout", "-b", "destination"]);
    fs::write(repo.root.join(".arc/policy.toml"), text).unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "declare destination policy"]);
    git(&repo.root, &["checkout", "master"]);
}

#[test]
fn into_enforces_destination_declared_actor() {
    let repo = repo_with_gates();
    destination_policy(&repo, "[policy]\nrequire_declared_actor=true\n");
    let destination = git_out(&repo.root, &["rev-parse", "destination"]);
    let (id, _) = approved_change_based_on(
        &repo,
        "destination-actor",
        "ff.txt",
        "ff\n",
        &[],
        Some(&destination),
    );
    let recorded = events(&repo, &id);
    repo.arc(&repo.root)
        .args(["integrate", &id, "--into", "destination"])
        .env_remove("ARC_ACTOR")
        .assert()
        .failure()
        .stderr(predicates::str::contains("declared actor"));
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "destination"]),
        destination
    );
    assert_eq!(events(&repo, &id), recorded);
}

#[test]
fn into_uses_destination_contribution_and_basis() {
    let repo = repo_with_gates();
    destination_policy(
        &repo,
        "[contribution]\nhistory='preserve'\n[policy]\nrequire_declared_actor=true\n",
    );
    let destination = git_out(&repo.root, &["rev-parse", "destination"]);
    let (id, _) = approved_change_based_on(
        &repo,
        "destination-send",
        "ff.txt",
        "ff\n",
        &[],
        Some(&destination),
    );
    let plan = json_stdout(repo.arc(&repo.root).args([
        "integrate",
        &id,
        "--into",
        "destination",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(
        plan["authorization"]["policy"]["require_declared_actor"],
        true
    );
    stdout(
        repo.arc(&repo.root)
            .args(["integrate", &id, "--into", "destination"]),
    );
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "destination"]),
        destination
    );
    let send = events(&repo, &id)
        .into_iter()
        .find(|e| e["event_type"] == "ready-to-send")
        .unwrap();
    assert_eq!(
        send["authorization"]["policy"]["require_declared_actor"],
        true
    );
}

#[test]
fn queue_projects_supersession_successor_closure() {
    let repo = repo_with_gates();
    let (old, _) = approved_change(&repo, "superseded-base", "old.txt", "old\n");
    let (successor, wt) = approved_change(&repo, "successor-base", "first.txt", "first\n");
    stdout(
        repo.arc(&repo.root)
            .args(["close", &old, "--superseded", &successor]),
    );
    let head = repo.head(&wt);
    let (dependent, _) = approved_change_based_on(
        &repo,
        "supersession-dependent",
        "second.txt",
        "second\n",
        &[&old],
        Some(&head),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["integrate", &successor, &dependent, "--dry-run"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["integrate", &successor, &dependent]),
    );
}

#[test]
fn verify_against_uses_destination_gates_and_integration_basis() {
    let repo = repo_with_gates();
    git(&repo.root, &["checkout", "-b", "destination"]);
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.destination]\ncommand='true'\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "declare destination gates"]);
    let destination = repo.head(&repo.root);
    git(&repo.root, &["checkout", "master"]);
    let (id, wt) = approved_change_based_on(
        &repo,
        "destination-gates",
        "ff.txt",
        "ff\n",
        &[],
        Some(&destination),
    );
    repo.arc(&repo.root)
        .args(["integrate", &id, "--into", "destination"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("destination"));
    stdout(
        repo.arc(&wt)
            .args(["verify", &id, "--against", "destination"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["integrate", &id, "--into", "destination"]),
    );
    let closure = events(&repo, &id)
        .into_iter()
        .find(|e| e["event_type"] == "change-integrated")
        .unwrap();
    let gates = closure["authorization"]["gates"].as_object().unwrap();
    assert!(gates.contains_key("destination"), "{closure}");
    assert!(!gates.contains_key("smoke"), "{closure}");
}

#[test]
fn fast_forward_checkout_failure_preserves_competing_move() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo_with_gates();
    let before = repo.head(&repo.root);
    let index = git_out(&repo.root, &["write-tree"]);
    let (id, wt) = approved_change(&repo, "rollback-race", "ff.txt", "ff\n");
    let approved = repo.head(&wt);
    let competitor = git_out(
        &repo.root,
        &["commit-tree", &index, "-p", &before, "-m", "competing"],
    );
    let recorded = events(&repo, &id);
    let real_git =
        String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout).unwrap();
    let bin = repo.home.join("rollback-bin");
    fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
if [ "$1" = read-tree ]; then
    '{git}' update-ref refs/heads/master '{competitor}' '{approved}' || exit 1
    exit 1
fi
exec '{git}' "$@"
"#,
            git = real_git.trim()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .assert()
        .failure()
        .stderr(predicates::str::contains("guarded ref rollback failed"))
        .stderr(predicates::str::contains(format!(
            "refs/heads/master={competitor}"
        )))
        .stderr(predicates::str::contains(format!("index tree={index}")));
    assert_eq!(repo.head(&repo.root), competitor);
    assert_eq!(git_out(&repo.root, &["write-tree"]), index);
    assert!(!repo.root.join("ff.txt").exists());
    assert_eq!(events(&repo, &id), recorded);
}

#[test]
fn third_round_destination_tag_cannot_shadow_history() {
    let repo = repo_with_gates();
    style_policy(&repo, "[integration]\nhistory='squash'\n");
    let (id, wt) = approved_change(&repo, "shadow", "one.txt", "one\n");
    repo.commit(
        &wt,
        ".arc/policy.toml",
        "[integration]\nhistory='preserve'\nshapes=['no-ff']\n",
        "feat: two",
    );
    stdout(repo.arc(&wt).args(["snapshot", &id]));
    stdout(repo.arc(&wt).args(["verify", &id, "--gate", "smoke"]));
    stdout(repo.arc(&wt).args(["review", &id, "--verdict", "approved"]));
    let check = stdout_any_status(repo.arc(&repo.root).args(["check", &id, "--json"]));
    let target = repo.head(&repo.root);
    git(&repo.root, &["tag", "master", &repo.head(&wt)]);
    assert_eq!(
        check,
        stdout_any_status(repo.arc(&repo.root).args(["check", &id, "--json"]))
    );
    for args in [
        vec!["integrate", &id],
        vec!["integrate", &id, "--dry-run"],
        vec!["integrate", "--tag", "series"],
        vec!["integrate", "--tag", "series", "--dry-run"],
    ] {
        let out = repo.arc(&repo.root).args(args).output().unwrap();
        assert!(
            !out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text.contains("integration history is squash"), "{text}");
        assert_eq!(repo.head(&repo.root), target);
    }
}

#[test]
fn third_round_queue_history_preflight_preserves_conflicting_checkout() {
    let repo = repo_with_gates();
    style_policy(
        &repo,
        "[integration]\nhistory='squash'\n[defaults.integration]\nshape='no-ff'\n",
    );
    let (id, wt) = approved_change(&repo, "preflight", "README.md", "change\n");
    repo.commit(&wt, "extra.txt", "extra\n", "feat: extra");
    stdout(repo.arc(&wt).args(["snapshot", &id]));
    stdout(repo.arc(&wt).args(["review", &id, "--verdict", "approved"]));
    repo.commit(&repo.root, "README.md", "target\n", "feat: target");
    let head = repo.head(&wt);
    let before = events(&repo, &id);
    let refs = git_out(&repo.root, &["show-ref"]);
    let out = stdout_any_status(repo.arc(&repo.root).args(["integrate", "--tag", "series"]));
    assert!(out.contains("integration history is squash"), "{out}");
    assert_eq!(repo.head(&wt), head);
    assert_eq!(events(&repo, &id), before);
    assert_eq!(git_out(&repo.root, &["show-ref"]), refs);
    assert!(git_out(&wt, &["status", "--porcelain"]).is_empty());
    assert!(git_out(&wt, &["rev-parse", "--git-path", "rebase-merge"])
        .lines()
        .all(|p| !Path::new(p).exists()));
}

#[test]
fn third_round_divergent_queue_projection_is_conditional() {
    let repo = repo_with_gates();
    style_policy(
        &repo,
        "[integration]\nhistory='squash'\n[defaults.integration]\nshape='no-ff'\n",
    );
    let (a, _) = approved_change(&repo, "project-a", "a.txt", "a\n");
    let (b, wt) = approved_change_blocked_by(&repo, "project-b", "b.txt", "b\n", &[&a]);
    let (c, _) = approved_change_based_on(
        &repo,
        "project-c",
        "c.txt",
        "c\n",
        &[&b],
        Some(&repo.head(&wt)),
    );
    let out = stdout(
        repo.arc(&repo.root)
            .args(["integrate", &a, &b, &c, "--dry-run"]),
    );
    assert!(out.contains(&format!("would land: {c}")), "{out}");
    assert!(out.contains("conditional"), "{out}");
    stdout(repo.arc(&repo.root).args(["integrate", &a, &b, &c]));
}

#[test]
fn third_round_queue_history_preflight_prevents_gate_synthesis() {
    let repo = repo_with_gates();
    style_policy(
        &repo,
        "[integration]\nhistory='squash'\n[defaults.integration]\nshape='no-ff'\n",
    );
    let (id, wt) = approved_change(&repo, "no-synthesis", "one.txt", "one\n");
    repo.commit(&wt, "two.txt", "two\n", "feat: two");
    stdout(repo.arc(&wt).args(["snapshot", &id]));
    stdout(repo.arc(&wt).args(["review", &id, "--verdict", "approved"]));
    let before = events(&repo, &id);
    let refs = git_out(&repo.root, &["show-ref"]);
    let out = stdout_any_status(repo.arc(&repo.root).args(["integrate", "--tag", "series"]));
    assert!(out.contains("integration history is squash"), "{out}");
    assert_eq!(events(&repo, &id), before);
    assert_eq!(git_out(&repo.root, &["show-ref"]), refs);
}

#[test]
fn third_round_queue_merged_policy_checks_are_conditional() {
    let repo = repo_with_gates();
    style_policy(
        &repo,
        "[integration]\nhistory='squash'\n[defaults.integration]\nshape='no-ff'\n",
    );
    let (a, _) = approved_change(&repo, "policy-a", "a.txt", "a\n");
    let (b, wt) = approved_change_blocked_by(
        &repo,
        "policy-b",
        ".arc/policy.toml",
        "[integration]\nhistory='preserve'\n[defaults.integration]\nshape='no-ff'\n",
        &[&a],
    );
    let (c, wt) = approved_change_based_on(
        &repo,
        "policy-c",
        "c.txt",
        "c\n",
        &[&b],
        Some(&repo.head(&wt)),
    );
    repo.commit(&wt, "extra.txt", "extra\n", "feat: extra");
    stdout(repo.arc(&wt).args(["snapshot", &c]));
    stdout(repo.arc(&wt).args(["verify", &c, "--gate", "smoke"]));
    stdout(repo.arc(&wt).args(["review", &c, "--verdict", "approved"]));
    let out = stdout(
        repo.arc(&repo.root)
            .args(["integrate", &a, &b, &c, "--dry-run"]),
    );
    assert!(out.contains("merged policy"), "{out}");
    assert!(out.contains(&format!("would land: {c}")), "{out}");
    stdout(repo.arc(&repo.root).args(["integrate", &a, &b, &c]));
}

#[test]
fn third_round_destination_tag_allows_valid_branch_integration() {
    for shape in ["ff-only", "no-ff"] {
        for queue in [false, true] {
            let repo = repo_with_gates();
            style_policy(&repo, "[integration]\nhistory='squash'\n");
            let (id, wt) = approved_change(&repo, "valid-shadow", "one.txt", "one\n");
            let target = repo.head(&repo.root);
            let head = repo.head(&wt);
            git(&repo.root, &["tag", "master", &head]);
            stdout(repo.arc(&repo.root).args(["check", &id, "--shape", shape]));
            let plan = json_stdout(repo.arc(&repo.root).args([
                "integrate",
                &id,
                "--shape",
                shape,
                "--dry-run",
                "--json",
            ]));
            assert_eq!(plan["target_revision"], target);
            let selected = if queue {
                vec!["--tag", "series"]
            } else {
                vec![id.as_str()]
            };
            let mut args = vec!["integrate", "--shape", shape];
            args.extend(selected);
            let mut dry = args.clone();
            dry.push("--dry-run");
            stdout(repo.arc(&repo.root).args(dry));
            stdout(repo.arc(&repo.root).args(args));
            assert_eq!(
                git_out(&repo.root, &["symbolic-ref", "HEAD"]),
                "refs/heads/master"
            );
            git(
                &repo.root,
                &["merge-base", "--is-ancestor", &head, "refs/heads/master"],
            );
        }
    }
}

#[test]
fn third_round_queue_known_lifecycle_blockers_precede_preparation() {
    for blocker in ["hold", "iterating"] {
        let repo = repo_with_gates();
        let (id, wt) = approved_change(&repo, "lifecycle-preflight", "one.txt", "one\n");
        repo.commit(&wt, "two.txt", "two\n", "feat: two");
        stdout(repo.arc(&wt).args(["snapshot", &id]));
        stdout(repo.arc(&wt).args(["review", &id, "--verdict", "approved"]));
        if blocker == "hold" {
            stdout(repo.arc(&wt).args(["hold", &id, "--reason", "waiting"]));
        } else {
            stdout(repo.arc(&wt).args(["iterating", &id]));
        }
        let before = events(&repo, &id);
        let refs = git_out(&repo.root, &["show-ref"]);
        let out = repo
            .arc(&repo.root)
            .args(["integrate", "--tag", "series", "--shape", "no-ff"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert_eq!(events(&repo, &id), before);
        assert_eq!(git_out(&repo.root, &["show-ref"]), refs);
    }
}
