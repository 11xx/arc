use super::common::*;

fn commit_gates(repo: &Repo) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.alpha]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gates"]);
}

fn change_id_of(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .expect("begin prints the change id")
        .to_string()
}

/// Open a change whose branch commits one file, and move the target so a
/// rebase has something to replay.
fn diverged(repo: &Repo, slug: &str, branch_file: &str, target_content: &str) -> (String, PathBuf) {
    let change_id = change_id_of(&stdout(repo.arc(&repo.root).args(["begin", slug])));
    let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&wt, branch_file, "branch\n", "feat: branch work");
    stdout(repo.arc(&wt).args(["snapshot", slug]));
    repo.commit(&repo.root, "README.md", target_content, "feat: move target");
    (change_id, wt)
}

#[test]
fn clean_rebase_snapshots_the_replayed_head_and_names_the_owed_gates() {
    let repo = Repo::new();
    commit_gates(&repo);
    let (change_id, wt) = diverged(&repo, "clean-replay", "branch.txt", "target\n");
    let before = repo.head(&wt);
    let target_head = repo.head(&repo.root);

    let out = stdout(repo.arc(&wt).args(["rebase", "clean-replay"]));
    let replayed = repo.head(&wt);
    assert_ne!(replayed, before, "the branch must be replayed");
    assert!(
        out.contains(&format!(
            "rebased arc/clean-replay onto master at {replayed}"
        )),
        "{out}"
    );
    assert!(out.contains("gates owed:"), "{out}");
    assert!(
        out.contains("`alpha` at head: no evidence at head"),
        "{out}"
    );

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "clean-replay"]))).unwrap();
    assert_eq!(status["needs_rebase"], false, "{status}");
    assert_eq!(status["head_matches_latest_patchset"], true, "{status}");
    assert_eq!(status["latest_patchset"]["head"], replayed, "{status}");
    assert!(
        git_out(
            &wt,
            &["rev-list", "--count", &format!("{target_head}..HEAD")]
        ) == "1",
        "the replayed branch sits directly on the target"
    );
    assert!(change_id.starts_with("clean-replay"), "{change_id}");
}

#[test]
fn rebase_verify_runs_the_required_gates_at_the_replayed_head() {
    let repo = Repo::new();
    commit_gates(&repo);
    let (_, wt) = diverged(&repo, "verify-replay", "branch.txt", "target\n");

    let out = stdout(repo.arc(&wt).args(["rebase", "verify-replay", "--verify"]));
    assert!(
        out.contains("gates: every required gate is green at head"),
        "{out}"
    );
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "verify-replay"]))).unwrap();
    assert_eq!(status["gates"][0]["name"], "alpha", "{status}");
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
}

#[test]
fn a_conflicting_rebase_stops_in_progress_and_names_the_conflicting_files() {
    let repo = Repo::new();
    let change_id = change_id_of(&stdout(
        repo.arc(&repo.root).args(["begin", "conflict-replay"]),
    ));
    let wt = repo.home.join(".worktrees").join("repo-conflict-replay");
    repo.commit(&wt, "README.md", "branch head\n", "feat: change readme");
    stdout(repo.arc(&wt).args(["snapshot", "conflict-replay"]));
    repo.commit(
        &repo.root,
        "README.md",
        "target head\n",
        "feat: move target",
    );
    let before = repo.head(&wt);

    let assertion = repo
        .arc(&wt)
        .args(["rebase", "conflict-replay"])
        .assert()
        .code(11);
    let out = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    assert!(out.contains("stopped on a conflict"), "{out}");
    assert!(out.contains("  - README.md"), "{out}");
    assert!(out.contains("rebase --continue"), "{out}");
    assert!(out.contains(&format!("arc snapshot {change_id}")), "{out}");

    // The partial resolution is the operator's work: arc must leave it standing.
    assert_eq!(
        git_out(&wt, &["diff", "--name-only", "--diff-filter=U"]),
        "README.md",
        "the conflict is still unresolved in the index"
    );

    // A second invocation names the state rather than restarting the replay.
    let second = repo
        .arc(&wt)
        .args(["rebase", "conflict-replay"])
        .assert()
        .code(11);
    let err = String::from_utf8_lossy(&second.get_output().stderr).into_owned();
    assert!(err.contains("already mid-rebase"), "{err}");

    // No patchset was recorded for a head nobody produced.
    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "conflict-replay"]),
    ))
    .unwrap();
    assert_eq!(status["latest_patchset"]["head"], before, "{status}");

    // The printed recovery finishes the replay and clears the blocker.
    fs::write(wt.join("README.md"), "resolved\n").unwrap();
    git(&wt, &["add", "README.md"]);
    git(&wt, &["rebase", "--continue"]);
    repo.arc(&wt)
        .args(["snapshot", "conflict-replay"])
        .assert()
        .success();
    let cleared: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "conflict-replay"]))).unwrap();
    assert_eq!(cleared["needs_rebase"], false, "{cleared}");
    assert_eq!(
        cleared["latest_patchset"]["head"],
        repo.head(&wt),
        "{cleared}"
    );
}

#[test]
fn a_dirty_worktree_is_refused_by_name() {
    let repo = Repo::new();
    let (_, wt) = diverged(&repo, "dirty-replay", "branch.txt", "target\n");
    fs::write(wt.join("branch.txt"), "uncommitted\n").unwrap();
    let before = repo.head(&wt);

    let assertion = repo
        .arc(&wt)
        .args(["rebase", "dirty-replay"])
        .assert()
        .code(11);
    let err = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();
    assert!(err.contains("uncommitted changes"), "{err}");
    assert_eq!(repo.head(&wt), before, "nothing may be replayed");
}

#[test]
fn a_branch_already_on_its_target_is_a_no_op() {
    let repo = Repo::new();
    let change_id = change_id_of(&stdout(
        repo.arc(&repo.root).args(["begin", "already-there"]),
    ));
    let wt = repo.home.join(".worktrees").join("repo-already-there");
    repo.commit(&wt, "branch.txt", "branch\n", "feat: branch work");
    stdout(repo.arc(&wt).args(["snapshot", "already-there"]));
    let head = repo.head(&wt);

    let out = stdout(repo.arc(&wt).args(["rebase", "already-there"]));
    assert!(out.contains("nothing to replay"), "{out}");
    assert!(!out.contains("gates owed"), "{out}");
    assert_eq!(repo.head(&wt), head, "a no-op replays nothing");

    let events = stdout(repo.arc(&wt).args(["events", "--change", &change_id]));
    assert_eq!(
        events.matches("patchset-added").count(),
        1,
        "a no-op records no second patchset: {events}"
    );
}

#[test]
fn the_needs_rebase_blocker_names_the_command_that_clears_it() {
    let repo = Repo::new();
    let change_id = change_id_of(&stdout(
        repo.arc(&repo.root).args(["begin", "blocked-replay"]),
    ));
    let wt = repo.home.join(".worktrees").join("repo-blocked-replay");
    repo.commit(&wt, "README.md", "branch head\n", "feat: change readme");
    stdout(repo.arc(&wt).args(["snapshot", "blocked-replay"]));
    repo.commit(
        &repo.root,
        "README.md",
        "target head\n",
        "feat: move target",
    );

    let assertion = repo
        .arc(&wt)
        .args(["check", "blocked-replay"])
        .assert()
        .code(11);
    let out = String::from_utf8_lossy(&assertion.get_output().stdout).into_owned();
    assert!(out.contains(&format!("arc rebase {change_id}")), "{out}");
}

#[test]
fn rebase_verify_from_another_checkout_gates_the_changes_worktree() {
    let repo = Repo::new();
    commit_gates(&repo);
    let (_, wt) = diverged(&repo, "anchor-replay", "branch.txt", "target\n");

    let out = stdout(
        repo.arc(&repo.root)
            .args(["rebase", "anchor-replay", "--verify"]),
    );
    assert!(
        out.contains(&format!("running in {}", wt.display())),
        "the gate run names the checkout it happened in: {out}"
    );
    assert!(
        out.contains("gates: every required gate is green at head"),
        "{out}"
    );

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "anchor-replay"]),
    ))
    .unwrap();
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");
    assert_eq!(
        status["latest_patchset"]["head"],
        repo.head(&wt),
        "{status}"
    );
}
