use crate::common::*;

fn repo_with_gates() -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.build]\ncommand = \"true\"\n[gates.test]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare gates"]);
    repo
}

/// A change with one commit, a recorded patchset, and green gates at its head.
fn snapshotted(repo: &Repo, slug: &str, file: &str, content: &str) -> (String, PathBuf) {
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&worktree, file, content, &format!("feat: {slug}"));
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&worktree)
        .args(["verify", slug, "--all"])
        .assert()
        .success();
    (change_id, worktree)
}

/// The same, approved, so nothing but the merge is left to do.
fn approved(repo: &Repo, slug: &str, file: &str, content: &str) -> (String, PathBuf) {
    let (change_id, worktree) = snapshotted(repo, slug, file, content);
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    (change_id, worktree)
}

fn events(repo: &Repo, change_id: &str) -> Vec<serde_json::Value> {
    stdout(repo.arc(&repo.root).args(["events", "--change", change_id]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn gate_runs(repo: &Repo, change_id: &str) -> usize {
    events(repo, change_id)
        .iter()
        .filter(|event| event["event_type"] == "verification-recorded" && event["gate"].is_string())
        .count()
}

#[test]
fn a_queue_lands_every_change_in_dependency_order_under_one_summary() {
    let repo = repo_with_gates();
    let (first, _) = approved(&repo, "queue-first", "first.txt", "first\n");
    let (second, _) = approved(&repo, "queue-second", "second.txt", "second\n");
    let (third, _) = approved(&repo, "queue-third", "third.txt", "third\n");

    // Named out of order: the queue orders itself, and unrelated members keep
    // the order they were opened in.
    let assertion = repo
        .arc(&repo.root)
        .args(["integrate", &third, &first, &second])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert_eq!(
        git_out(&repo.root, &["log", "--first-parent", "--format=%s", "-3"])
            .lines()
            .collect::<Vec<_>>(),
        [
            "merge(queue-third): queue third",
            "merge(queue-second): queue second",
            "merge(queue-first): queue first"
        ],
        "{out}"
    );

    assert!(
        out.contains("queue: 3 changes in dependency order"),
        "{out}"
    );
    assert!(out.contains("queue summary:"), "{out}");
    for change_id in [&first, &second, &third] {
        let landed = format!("  landed: {change_id} at ");
        assert!(out.contains(&landed), "{landed} missing from:\n{out}");
    }
    assert!(!out.contains("not attempted"), "{out}");

    // Each summary line names the merge that change actually produced.
    let merges = git_out(&repo.root, &["log", "--first-parent", "--format=%H", "-3"]);
    for revision in merges.lines() {
        assert!(out.contains(revision), "{revision} missing from:\n{out}");
    }
}

#[test]
fn a_conflicting_replay_stops_the_queue_and_leaves_the_rest_unattempted() {
    let repo = repo_with_gates();
    // The first two changes edit one file in incompatible ways, so the second
    // cannot merge once the first has landed.
    let (first, _) = approved(&repo, "queue-lands", "shared.txt", "from the first\n");
    let (blocked, blocked_worktree) =
        approved(&repo, "queue-conflicts", "shared.txt", "from the second\n");
    let (untouched, _) = approved(&repo, "queue-untouched", "untouched.txt", "untouched\n");

    let assertion = repo
        .arc(&repo.root)
        .args(["integrate", &first, &blocked, &untouched])
        .assert()
        .code(11);
    let out = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert!(out.contains(&format!("  landed: {first} at ")), "{out}");
    assert!(out.contains(&format!("  stopped: {blocked} —")), "{out}");
    assert!(
        out.contains(&format!("  not attempted: {untouched}")),
        "{out}"
    );

    // The replay's own recovery instructions are what a person continues from.
    assert!(out.contains("stopped on a conflict"), "{out}");
    assert!(out.contains("  - shared.txt"), "{out}");
    assert!(out.contains("rebase --continue"), "{out}");
    assert_eq!(
        git_out(
            &blocked_worktree,
            &["diff", "--name-only", "--diff-filter=U"]
        ),
        "shared.txt",
        "the partial resolution is the operator's work and must stand"
    );

    // Exactly one merge landed, and the changes behind the stop are untouched.
    assert_eq!(
        git_out(&repo.root, &["log", "--format=%s", "-1"]),
        "merge(queue-lands): queue lands"
    );
    for change_id in [&blocked, &untouched] {
        let status = json_stdout(repo.arc(&repo.root).args(["status", change_id, "--json"]));
        assert_eq!(status["state"], "open", "{status}");
    }
}

#[test]
fn a_change_without_a_verdict_stops_the_queue_by_its_own_blocker() {
    let repo = repo_with_gates();
    let (first, _) = approved(&repo, "verdict-first", "first.txt", "first\n");
    let (unreviewed, _) = snapshotted(&repo, "verdict-missing", "missing.txt", "missing\n");
    let (behind, _) = approved(&repo, "verdict-behind", "behind.txt", "behind\n");

    let assertion = repo
        .arc(&repo.root)
        .args(["integrate", &first, &unreviewed, &behind])
        .assert()
        .code(3);
    let out = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    let err = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();

    assert!(out.contains(&format!("  landed: {first} at ")), "{out}");
    assert!(out.contains(&format!("  stopped: {unreviewed} —")), "{out}");
    assert!(out.contains(&format!("  not attempted: {behind}")), "{out}");
    assert!(err.contains("approv"), "the blocker explains itself: {err}");

    assert_eq!(
        git_out(&repo.root, &["log", "--format=%s", "-1"]),
        "merge(verdict-first): verdict first"
    );
}

#[test]
fn a_dry_run_reports_the_plan_and_merges_nothing() {
    let repo = repo_with_gates();
    let (first, _) = approved(&repo, "plan-first", "first.txt", "first\n");
    let (second, _) = approved(&repo, "plan-second", "second.txt", "second\n");
    let before = repo.head(&repo.root);
    let counts = [events(&repo, &first).len(), events(&repo, &second).len()];

    let assertion = repo
        .arc(&repo.root)
        .args(["integrate", &first, &second, "--dry-run"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();

    assert!(
        out.contains(&format!("dry-run: would integrate {first} into master")),
        "{out}"
    );
    assert!(
        out.contains(&format!("  would land: {first} into master")),
        "{out}"
    );
    assert!(
        out.contains(&format!("  would land: {second} into master")),
        "{out}"
    );

    assert_eq!(repo.head(&repo.root), before, "a dry run merges nothing");
    assert_eq!(
        [events(&repo, &first).len(), events(&repo, &second).len()],
        counts,
        "a dry run writes no events"
    );
    for change_id in [&first, &second] {
        let status = json_stdout(repo.arc(&repo.root).args(["status", change_id, "--json"]));
        assert_eq!(status["state"], "open", "{status}");
    }
}

#[test]
fn a_gate_already_green_at_the_merged_tree_is_not_rerun_by_the_queue() {
    let repo = repo_with_gates();
    let (first, _) = approved(&repo, "reuse-first", "first.txt", "first\n");
    let (second, _) = approved(&repo, "reuse-second", "second.txt", "second\n");
    // Both changes are now behind their target, so what ships is content
    // neither branch committed and the head's evidence does not answer for it.
    repo.commit(&repo.root, "target.txt", "moved\n", "feat: move the target");

    // The first change is evaluated at that merged tree ahead of the queue.
    repo.arc(&repo.root)
        .args(["verify", &first, "--against", "master"])
        .assert()
        .success();
    let already_evaluated = gate_runs(&repo, &first);
    let unevaluated = gate_runs(&repo, &second);

    repo.arc(&repo.root)
        .args(["integrate", &first, &second])
        .assert()
        .success();

    assert_eq!(
        gate_runs(&repo, &first),
        already_evaluated,
        "a gate green at the tree the merge ships must not run again"
    );
    assert!(
        gate_runs(&repo, &second) > unevaluated,
        "a change with no evidence at its merged tree owes the gates a run"
    );
}

#[test]
fn verify_against_skip_green_reuses_the_evidence_at_the_merged_tree() {
    let repo = repo_with_gates();
    let (change_id, _) = snapshotted(&repo, "against-reuse", "work.txt", "work\n");
    repo.commit(&repo.root, "target.txt", "moved\n", "feat: move the target");

    repo.arc(&repo.root)
        .args(["verify", &change_id, "--against", "master"])
        .assert()
        .success();
    let ran = gate_runs(&repo, &change_id);

    let second = stdout(repo.arc(&repo.root).args([
        "verify",
        &change_id,
        "--against",
        "master",
        "--skip-green",
    ]));
    assert!(
        second.contains("gate build: skipped (green at the merged tree)"),
        "{second}"
    );
    assert!(
        second.contains("gate test: skipped (green at the merged tree)"),
        "{second}"
    );
    assert!(
        second.contains("gates: 2/2 pass at the merged tree"),
        "{second}"
    );
    assert_eq!(
        gate_runs(&repo, &change_id),
        ran,
        "reuse must record no new gate run"
    );
    assert_eq!(
        events(&repo, &change_id)
            .iter()
            .filter(|event| event["event_type"] == "verification-reused")
            .count(),
        2,
        "each skipped gate records what it reused"
    );
}

#[test]
fn a_queue_refuses_the_flags_that_name_one_merge() {
    let repo = repo_with_gates();
    let (first, _) = approved(&repo, "flags-first", "first.txt", "first\n");
    let (second, _) = approved(&repo, "flags-second", "second.txt", "second\n");

    repo.arc(&repo.root)
        .args(["integrate", &first, &second, "--into", "master"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--into is only valid"));
    repo.arc(&repo.root)
        .args(["integrate", &first, &second, "--message", "custom"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--message is only valid"));
    repo.arc(&repo.root)
        .args(["integrate", &first, &first])
        .assert()
        .failure()
        .stderr(predicates::str::contains("named more than once"));

    assert_eq!(
        git_out(&repo.root, &["log", "--format=%s", "-1"]),
        "test: declare gates",
        "a refused queue merges nothing"
    );
}
