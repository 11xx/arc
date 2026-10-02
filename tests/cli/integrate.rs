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
    let change_id = in_place_change(&repo, "stranded-in-place");
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "arc/stranded-in-place"
    );
    let old_master = git_out(&repo.root, &["rev-parse", "master"]);

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
    let merged = repo.head(&repo.root);
    let parents = git_out(&repo.root, &["rev-list", "--parents", "-n", "1", &merged])
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(parents.len(), 3);
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
    assert_eq!(parents.len(), 3);
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
        show.contains("Worktree state: head matches newest approved/snapshotted head"),
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

/// Replaying a closed change answers for the head its closure recorded, as
/// the live report does: an asserted integration that shipped an earlier
/// patchset is judged at that patchset's head, not at a later one.
#[test]
fn a_replayed_closure_reports_the_head_it_recorded() {
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

    let live = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "earlier-shipped", "--json"]),
    );
    assert_eq!(live["current_head"], shipped.as_str(), "{live}");
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
    assert_eq!(plan["merge_commit"], true);
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
            stdout(
                repo.arc(&repo.root)
                    .args(["integrate", slug, "--dry-run", "--json"]),
            ),
        )
        .unwrap();
        path
    };

    // An unchanged basis adds nothing.
    let steady_plan = dry_run("moves-steady");
    let steady = repo
        .arc(&repo.root)
        .args(["integrate", "moves-steady", "--expect-basis"])
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
        .args(["integrate", "moves-ready", "--expect-basis"])
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
        .args(["integrate", "moves-unready", "--expect-basis"])
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
        .args(["integrate", "moves-unready", "--expect-basis"])
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
