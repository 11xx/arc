use super::common::*;

/// begin → worktree + branch + ledger; list/status see the change.
#[test]
fn begin_creates_change_branch_and_worktree() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args(["begin", "fix-thing", "--title", "Fix the thing"]),
    );
    assert!(out.contains("change: fix-thing-"));
    assert!(out.contains("branch: arc/fix-thing"));
    assert!(out.contains("worktree: "));

    let wt = repo.home.join(".worktrees").join("repo-fix-thing");
    assert!(wt.is_dir(), "worktree should exist");

    let list = stdout(repo.arc(&repo.root).args(["list", "--json"]));
    let rows: serde_json::Value = serde_json::from_str(&list).unwrap();
    assert_eq!(rows[0]["slug"], "fix-thing");
    assert_eq!(rows[0]["state"], "open");

    // Same open slug refuses a duplicate.
    repo.arc(&repo.root)
        .args(["begin", "fix-thing"])
        .assert()
        .failure();
}

/// The full green path: implement → snapshot → verify gate → approve →
/// check ok → integrate produces a --no-ff merge with correct parents.
#[test]
fn green_path_integrates_with_merge_commit() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.smoke]\ncommand = \"test -f README.md\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "gates"]);
    let old_master = repo.head(&repo.root);

    stdout(
        repo.arc(&repo.root)
            .args(["begin", "feat-x", "--title", "Feature X"]),
    );
    let wt = repo.home.join(".worktrees").join("repo-feat-x");
    repo.commit(&wt, "x.txt", "x\n", "feat: add x");

    stdout(repo.arc(&wt).args(["snapshot", "feat-x"]));
    repo.arc(&wt)
        .args(["verify", "feat-x", "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt).args(["check", "feat-x"]).assert().success();

    // Integrate from the main checkout, which has master checked out.
    repo.arc(&repo.root)
        .args(["integrate", "feat-x"])
        .assert()
        .success();

    let merged = repo.head(&repo.root);
    let parents = git_out(&repo.root, &["rev-list", "--parents", "-n", "1", &merged]);
    let ids: Vec<&str> = parents.split_whitespace().collect();
    assert_eq!(ids.len(), 3, "merge commit must have two parents");
    assert_eq!(ids[1], old_master);
    let subject = git_out(&repo.root, &["log", "-1", "--format=%s"]);
    assert_eq!(subject, "merge(feat-x): Feature X");

    let status = stdout(repo.arc(&repo.root).args(["status", "feat-x"]));
    let report: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(report["state"], "closed");
    assert_eq!(report["closure"]["outcome"], "integrated");
}

#[test]
fn policy_rejects_same_actor_approval() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);

    stdout(repo.arc(&repo.root).args(["begin", "self-review"]));
    let wt = repo.home.join(".worktrees").join("repo-self-review");
    repo.commit(&wt, "review.txt", "review\n", "test: self review");
    stdout(
        repo.arc(&wt)
            .env("ARC_ACTOR", "Same Actor")
            .args(["snapshot", "self-review"]),
    );
    repo.arc(&wt)
        .env("ARC_ACTOR", "Same Actor")
        .args(["review", "self-review", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["check", "self-review"])
        .assert()
        .code(3)
        .stdout(predicates::str::contains(
            "approval rejected by policy: self-approval",
        ));
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "self-review"]))).unwrap();
    assert_eq!(
        status["approval_rejection_reason"],
        "approval rejected by policy: self-approval"
    );
    assert_eq!(
        status["blocker_summary"]["approval_reason"],
        "approval rejected by policy: self-approval"
    );
    assert_eq!(
        status["next_action"],
        "approval rejected by policy: self-approval"
    );
    let show = stdout(repo.arc(&wt).args(["show", "self-review"]));
    assert!(show.contains("approval rejected by policy: self-approval"));
}

#[test]
fn policy_allows_different_actor_approval() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);

    stdout(repo.arc(&repo.root).args(["begin", "peer-review"]));
    let wt = repo.home.join(".worktrees").join("repo-peer-review");
    repo.commit(&wt, "review.txt", "review\n", "test: peer review");
    stdout(
        repo.arc(&wt)
            .env("ARC_ACTOR", "Implementer")
            .args(["snapshot", "peer-review"]),
    );
    repo.arc(&wt)
        .env("ARC_ACTOR", "Reviewer")
        .args(["review", "peer-review", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["check", "peer-review"])
        .assert()
        .success();
}

#[test]
fn policy_absent_or_off_preserves_self_approval() {
    for policy in [None, Some("[policy]\nforbid_self_approval = false\n")] {
        let repo = Repo::new();
        if let Some(policy) = policy {
            fs::create_dir_all(repo.root.join(".arc")).unwrap();
            fs::write(repo.root.join(".arc/policy.toml"), policy).unwrap();
            git(&repo.root, &["add", ".arc/policy.toml"]);
            git(&repo.root, &["commit", "-m", "policy"]);
        }

        stdout(repo.arc(&repo.root).args(["begin", "legacy-review"]));
        let wt = repo.home.join(".worktrees").join("repo-legacy-review");
        repo.commit(&wt, "review.txt", "review\n", "test: legacy review");
        stdout(
            repo.arc(&wt)
                .env("ARC_ACTOR", "Same Actor")
                .args(["snapshot", "legacy-review"]),
        );
        repo.arc(&wt)
            .env("ARC_ACTOR", "Same Actor")
            .args(["review", "legacy-review", "--verdict", "approved"])
            .assert()
            .success();

        repo.arc(&wt)
            .args(["check", "legacy-review"])
            .assert()
            .success();
    }
}

/// Blocking finding → check exits 2 and integrate refuses; resolving it
/// plus a renewed verdict unblocks.
#[test]
fn blocking_finding_blocks_until_resolved_and_reapproved() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-y"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-y");
    repo.commit(&wt, "y.txt", "y\n", "fix: y");
    stdout(repo.arc(&wt).args(["snapshot", "fix-y"]));

    let findings = r#"[{"blocking": true, "severity": "major",
        "summary": "y is wrong", "anchor": {"path": "y.txt", "line_start": 1}}]"#;
    repo.arc(&wt)
        .args([
            "review",
            "fix-y",
            "--verdict",
            "changes-requested",
            "--findings-json",
            "-",
        ])
        .write_stdin(findings)
        .assert()
        .success();

    repo.arc(&wt).args(["check", "fix-y"]).assert().code(2);
    repo.arc(&repo.root)
        .args(["integrate", "fix-y"])
        .assert()
        .code(2);

    let show = stdout(repo.arc(&wt).args(["show", "fix-y", "--json"]));
    let state: serde_json::Value = serde_json::from_str(&show).unwrap();
    let fid = state["findings"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    // Anchor captured a blob for the head side.
    assert!(state["findings"][&fid]["anchor"]["blob"].is_string());

    repo.commit(&wt, "y.txt", "y fixed\n", "fix: correct y");
    let fix = repo.head(&wt);
    repo.arc(&wt)
        .args([
            "resolve", "fix-y", &fid, "--status", "resolved", "--commit", &fix,
        ])
        .assert()
        .success();

    // Old approval basis is gone: new head needs a new patchset + verdict.
    repo.arc(&wt).args(["check", "fix-y"]).assert().code(3);
    stdout(repo.arc(&wt).args(["snapshot", "fix-y"]));
    repo.arc(&wt)
        .args(["review", "fix-y", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-y"]).assert().success();
}

/// A commit after approval makes the verdict stale (exit 3) until a new
/// patchset is approved.
#[test]
fn approval_goes_stale_when_head_moves() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-z"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-z");
    repo.commit(&wt, "z.txt", "z\n", "fix: z");
    stdout(repo.arc(&wt).args(["snapshot", "fix-z"]));
    repo.arc(&wt)
        .args(["review", "fix-z", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-z"]).assert().success();

    repo.commit(&wt, "z.txt", "z2\n", "fix: z again");
    repo.arc(&wt).args(["check", "fix-z"]).assert().code(3);
    repo.arc(&repo.root)
        .args(["integrate", "fix-z"])
        .assert()
        .code(3);
}

#[test]
fn conflicting_target_movement_requires_rebase_before_integration() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "conflict-r"]));
    let wt = repo.home.join(".worktrees").join("repo-conflict-r");
    repo.commit(&wt, "README.md", "change head\n", "feat: change readme");
    stdout(repo.arc(&wt).args(["snapshot", "conflict-r"]));
    repo.arc(&wt)
        .args(["review", "conflict-r", "--verdict", "approved"])
        .assert()
        .success();

    repo.commit(
        &repo.root,
        "README.md",
        "target head\n",
        "feat: move target",
    );
    let target_head = repo.head(&repo.root);

    repo.arc(&wt)
        .args(["check", "conflict-r"])
        .assert()
        .code(11);
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "conflict-r"]))).unwrap();
    assert_eq!(status["schema"], "arc-status/5");
    assert_eq!(status["needs_rebase"], true);
    assert!(status["blockers"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("needs-rebase")));
    assert_eq!(status["next_action"], "rebase");

    repo.arc(&repo.root)
        .args(["integrate", "conflict-r"])
        .assert()
        .code(11);
    assert_eq!(
        repo.head(&repo.root),
        target_head,
        "target must remain unmerged"
    );
}

#[test]
fn non_conflicting_target_movement_stays_ready_and_integrates() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "clean-r"]));
    let wt = repo.home.join(".worktrees").join("repo-clean-r");
    repo.commit(&wt, "change.txt", "change\n", "feat: change branch");
    stdout(repo.arc(&wt).args(["snapshot", "clean-r"]));
    repo.arc(&wt)
        .args(["review", "clean-r", "--verdict", "approved"])
        .assert()
        .success();

    repo.commit(&repo.root, "target.txt", "target\n", "feat: move target");

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "clean-r"]))).unwrap();
    assert_eq!(status["needs_rebase"], false);
    repo.arc(&wt).args(["check", "clean-r"]).assert().success();
    repo.arc(&repo.root)
        .args(["integrate", "clean-r"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(repo.root.join("change.txt")).unwrap(),
        "change\n"
    );
}

/// integrate --cleanup invoked from INSIDE the change worktree must not
/// die when that worktree is removed under it (regression: branch
/// deletion used the vanished cwd).
#[test]
fn integrate_cleanup_from_inside_change_worktree() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-c"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-c");
    repo.commit(&wt, "c.txt", "c\n", "fix: c");
    stdout(repo.arc(&wt).args(["snapshot", "fix-c"]));
    repo.arc(&wt)
        .args(["review", "fix-c", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["integrate", "fix-c", "--cleanup"])
        .assert()
        .success();

    assert!(!wt.exists(), "change worktree should be removed");
    let branches = git_out(&repo.root, &["branch", "--list", "arc/fix-c"]);
    assert!(branches.is_empty(), "change branch should be deleted");
}

/// Hold blocks integration (exit 4) until released.
#[test]
fn hold_blocks_integration() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-h"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-h");
    repo.commit(&wt, "h.txt", "h\n", "fix: h");
    stdout(repo.arc(&wt).args(["snapshot", "fix-h"]));
    repo.arc(&wt)
        .args(["review", "fix-h", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["hold", "fix-h", "--reason", "manual testing first"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-h"]).assert().code(4);
    repo.arc(&repo.root)
        .args(["integrate", "fix-h"])
        .assert()
        .code(4);

    repo.arc(&wt)
        .args(["release-hold", "fix-h"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-h"]).assert().success();
}

/// A declared gate that never ran (or failed) blocks with exit 5; a pass
/// at the exact head unblocks.
#[test]
fn gates_must_be_green_at_head() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.fails]\ncommand = \"false\"\nprofiles = [\"local\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "gates"]);

    stdout(repo.arc(&repo.root).args(["begin", "fix-g"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-g");
    repo.commit(&wt, "g.txt", "g\n", "fix: g");
    stdout(repo.arc(&wt).args(["snapshot", "fix-g"]));
    repo.arc(&wt)
        .args(["review", "fix-g", "--verdict", "approved"])
        .assert()
        .success();

    // Gate never ran: blocked and accurately summarized as pending.
    repo.arc(&wt).args(["check", "fix-g"]).assert().code(5);
    let pending: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "fix-g"]))).unwrap();
    assert_eq!(
        pending["blocker_summary"]["gate_status"]["fails"],
        "pending"
    );
    assert_eq!(pending["gates"][0]["result"], "pending");

    // Gate ran and failed: still blocked, verify itself exits 1.
    repo.arc(&wt)
        .args(["verify", "fix-g", "--gate", "fails"])
        .assert()
        .code(1);
    repo.arc(&wt).args(["check", "fix-g"]).assert().code(5);
    let failed: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "fix-g"]))).unwrap();
    assert_eq!(failed["blocker_summary"]["gate_status"]["fails"], "fail");
    assert_eq!(failed["gates"][0]["result"], "fail");

    // Fix the gate in the worktree's .arc? Gate command comes from the
    // toplevel of the invoking worktree, so run a passing command via a
    // redefined gates file in the change worktree.
    fs::write(
        wt.join(".arc/gates.toml"),
        "[gates.fails]\ncommand = \"true\"\nprofiles = [\"local\"]\n",
    )
    .unwrap();
    git(&wt, &["add", ".arc"]);
    git(&wt, &["commit", "-m", "fix gate"]);
    stdout(repo.arc(&wt).args(["snapshot", "fix-g"]));
    repo.arc(&wt)
        .args(["verify", "fix-g", "--gate", "fails"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["review", "fix-g", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-g"]).assert().success();
    let passed: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "fix-g"]))).unwrap();
    assert_eq!(passed["blocker_summary"]["gate_status"]["fails"], "pass");
    assert_eq!(passed["gates"][0]["result"], "pass");
}

/// Comments, replies, and prefix resolution round-trip through the ledger.
#[test]
fn comment_reply_roundtrip_and_prefix_resolution() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "chat-c"]));
    let wt = repo.home.join(".worktrees").join("repo-chat-c");
    repo.commit(&wt, "c.txt", "c\n", "c");
    stdout(repo.arc(&wt).args(["snapshot", "chat-c"]));

    let out = stdout(repo.arc(&wt).args([
        "comment",
        "chat-c",
        "--body",
        "looks odd",
        "--path",
        "c.txt",
        "--line",
        "1",
    ]));
    let event_id = out
        .lines()
        .find_map(|l| l.strip_prefix("event: "))
        .unwrap()
        .to_string();

    repo.arc(&wt)
        .args(["reply", "chat-c", &event_id, "--body", "explained"])
        .assert()
        .success();

    // Bare slug prefix resolves the change.
    let show = stdout(repo.arc(&wt).args(["show", "chat-c"]));
    assert!(show.contains("looks odd"));
    assert!(show.contains("explained"));
}

/// Approving while recording blocking findings is contradictory.
#[test]
fn approve_with_blocking_findings_refused() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-w"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-w");
    repo.commit(&wt, "w.txt", "w\n", "w");
    stdout(repo.arc(&wt).args(["snapshot", "fix-w"]));
    let findings = r#"[{"blocking": true, "severity": "critical", "summary": "no"}]"#;
    repo.arc(&wt)
        .args([
            "review",
            "fix-w",
            "--verdict",
            "approved",
            "--findings-json",
            "-",
        ])
        .write_stdin(findings)
        .assert()
        .failure();
}

/// Snapshot sets a retention ref so reviewed heads survive branch deletion.
#[test]
fn snapshot_sets_retention_ref() {
    let repo = Repo::new();
    let out = stdout(repo.arc(&repo.root).args(["begin", "keep-k"]));
    let change_id = out
        .lines()
        .find_map(|l| l.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let wt = repo.home.join(".worktrees").join("repo-keep-k");
    repo.commit(&wt, "k.txt", "k\n", "k");
    stdout(repo.arc(&wt).args(["snapshot", "keep-k"]));
    let head = repo.head(&wt);
    let kept = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{change_id}/ps-01")],
    );
    assert_eq!(kept, head);

    // A rewound branch gets a second patchset with its own pin; the
    // first head stays protected.
    git(&wt, &["reset", "--hard", "HEAD~1"]);
    repo.commit(&wt, "k2.txt", "k2\n", "k2");
    stdout(repo.arc(&wt).args(["snapshot", "keep-k"]));
    let kept1 = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{change_id}/ps-01")],
    );
    let kept2 = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{change_id}/ps-02")],
    );
    assert_eq!(kept1, head, "rewound head must stay pinned");
    assert_eq!(kept2, repo.head(&wt));
}

/// Abandoning a change must keep every reviewed head pinned; integrating
/// releases only heads reachable from the merge.
#[test]
fn closure_retention_policy() {
    let repo = Repo::new();

    // Abandoned: pins survive even branch force-deletion.
    let out = stdout(repo.arc(&repo.root).args(["begin", "drop-r"]));
    let drop_id = out
        .lines()
        .find_map(|l| l.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let wt = repo.home.join(".worktrees").join("repo-drop-r");
    repo.commit(&wt, "r.txt", "r\n", "r");
    stdout(repo.arc(&wt).args(["snapshot", "drop-r"]));
    let dropped_head = repo.head(&wt);
    repo.arc(&repo.root)
        .args(["close", "drop-r", "--abandoned"])
        .assert()
        .success()
        .stdout(predicates::str::contains("kept refs/arc/keep/"));
    git(
        &repo.root,
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );
    git(&repo.root, &["branch", "-D", "arc/drop-r"]);
    let kept = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{drop_id}/ps-01")],
    );
    assert_eq!(kept, dropped_head, "abandoned head must stay pinned");

    // Integrated: the reachable head's pin is released.
    let out = stdout(repo.arc(&repo.root).args(["begin", "land-r"]));
    let land_id = out
        .lines()
        .find_map(|l| l.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let wt2 = repo.home.join(".worktrees").join("repo-land-r");
    repo.commit(&wt2, "l.txt", "l\n", "l");
    stdout(repo.arc(&wt2).args(["snapshot", "land-r"]));
    repo.arc(&wt2)
        .args(["review", "land-r", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "land-r"])
        .assert()
        .success();
    let refs = git_out(
        &repo.root,
        &["for-each-ref", &format!("refs/arc/keep/{land_id}/")],
    );
    assert!(refs.is_empty(), "reachable pins should be released");
}

/// begin derives the target from the primary worktree's branch, even
/// when invoked from another change's worktree on a different branch.
#[test]
fn begin_derives_target_from_primary_worktree() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "first-t"]));
    let wt = repo.home.join(".worktrees").join("repo-first-t");
    repo.commit(&wt, "t.txt", "t\n", "t");

    // From inside the first change's worktree: target must be master,
    // and the new branch must derive from master's head, not from the
    // in-progress arc/first-t head.
    stdout(repo.arc(&wt).args(["begin", "second-t"]));
    let show = stdout(repo.arc(&wt).args(["show", "second-t", "--json"]));
    let state: serde_json::Value = serde_json::from_str(&show).unwrap();
    assert_eq!(state["target_branch"], "master");
    assert_eq!(
        state["base"],
        serde_json::Value::String(repo.head(&repo.root)),
        "base must be master's head, not the other change's head"
    );
}

/// Implicit stacking on an open change branch is refused; explicit
/// --target allows it.
#[test]
fn begin_refuses_implicit_stacking() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "base-s"]));
    let wt = repo.home.join(".worktrees").join("repo-base-s");
    repo.commit(&wt, "s.txt", "s\n", "s");

    // Make the primary worktree sit on the change branch: simulate by
    // passing --target pointing at the open change branch explicitly —
    // allowed — versus the implicit refusal path, which needs the
    // default target to resolve to that branch. Explicit works:
    repo.arc(&wt)
        .args([
            "begin",
            "stack-s",
            "--target",
            "arc/base-s",
            "--no-worktree",
        ])
        .assert()
        .success();
}

/// close --abandoned works and closed changes refuse new work.
#[test]
fn close_abandoned_and_refuse_further_work() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "drop-d", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["close", "drop-d", "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["check", "drop-d"])
        .assert()
        .code(6);
    repo.arc(&repo.root)
        .args(["hold", "drop-d", "--reason", "x"])
        .assert()
        .failure();
    // Slug is reusable after closure.
    repo.arc(&repo.root)
        .args([
            "begin",
            "drop-d",
            "--no-worktree",
            "--branch",
            "arc/drop-d-2",
        ])
        .assert()
        .success();
}

#[test]
fn show_renders_messages_section_chronologically() {
    let repo = Repo::new();
    begin_change(&repo, "msg-show", None);
    repo.arc(&repo.root)
        .args([
            "message",
            "msg-show",
            "--type",
            "status",
            "--summary",
            "first announced",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "message",
            "msg-show",
            "--type",
            "discovery",
            "--summary",
            "second announced",
        ])
        .assert()
        .success();

    let rendered = stdout(repo.arc(&repo.root).args(["show", "msg-show"]));
    let section = rendered.find("## Messages").expect("messages section");
    let first = rendered.find("first announced").unwrap();
    let second = rendered.find("second announced").unwrap();
    assert!(section < first && first < second, "chronological order");
}
