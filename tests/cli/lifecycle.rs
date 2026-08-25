use super::common::*;

fn commit_composed_gates(repo: &Repo) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.alpha]\ncommand = \"true\"\n[gates.beta]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gates"]);
}

#[test]
fn verdict_body_round_trips_through_show_status_and_log() {
    let repo = Repo::new();
    let (_change_id, wt, _head) = change_with_patchset(&repo, "review-body");
    repo.arc(&wt)
        .args([
            "review",
            "review-body",
            "--verdict",
            "approved",
            "--body",
            "The implementation preserves the required invariant.\nNo findings remain.",
        ])
        .assert()
        .success();

    let show = stdout(repo.arc(&wt).args(["show", "review-body"]));
    assert!(show.contains("The implementation preserves the required invariant."));
    assert!(show.contains("No findings remain."));

    let status = json_stdout(repo.arc(&wt).args(["status", "review-body"]));
    assert_eq!(
        status["verdict"]["body"],
        "The implementation preserves the required invariant.\nNo findings remain."
    );

    let log = stdout(repo.arc(&wt).args(["log", "review-body"]));
    assert!(log.contains(
        "verdict-recorded  approved ps-01 — The implementation preserves the required invariant."
    ));
}

#[test]
fn verdict_without_body_omits_the_field() {
    let repo = Repo::new();
    let (change_id, wt, _head) = change_with_patchset(&repo, "review-no-body");
    repo.arc(&wt)
        .args(["review", "review-no-body", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "review-no-body"]));
    assert!(!status["verdict"].as_object().unwrap().contains_key("body"));

    let verdict = fs::read_dir(event_dir(&repo, &change_id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find_map(|path| {
            let event: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            (event["event_type"] == "verdict-recorded").then_some(event)
        })
        .unwrap();
    assert!(!verdict.as_object().unwrap().contains_key("body"));
}

#[test]
fn verdict_body_and_body_file_conflict() {
    let repo = Repo::new();
    let (_change_id, wt, _head) = change_with_patchset(&repo, "review-body-conflict");
    repo.arc(&wt)
        .write_stdin("file body")
        .args([
            "review",
            "review-body-conflict",
            "--verdict",
            "approved",
            "--body",
            "inline body",
            "--body-file",
            "-",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--body and --body-file are mutually exclusive",
        ));
}

#[test]
fn snapshot_verify_records_patchset_and_selected_or_all_evidence_at_the_same_head() {
    for (slug, selection) in [
        ("snap-gates", vec!["--gate", "alpha", "--gate", "beta"]),
        ("snap-all", Vec::new()),
    ] {
        let repo = Repo::new();
        commit_composed_gates(&repo);
        stdout(repo.arc(&repo.root).args(["begin", slug]));
        let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
        repo.commit(&wt, "change.txt", "change\n", "feat: change");
        let head = repo.head(&wt);
        let mut args = vec!["snapshot", slug, "--verify"];
        args.extend(selection);
        repo.arc(&wt).args(args).assert().success();

        let status: serde_json::Value =
            serde_json::from_str(&stdout(repo.arc(&wt).args(["status", slug]))).unwrap();
        assert_eq!(status["latest_patchset"]["head"], head);
        let events = stdout(repo.arc(&wt).args([
            "events",
            "--change",
            slug,
            "--type",
            "verification-recorded",
        ]));
        let evidence = events
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(evidence.len(), 2);
        assert!(evidence.iter().all(|event| event["revision"] == head));
    }
}

#[test]
fn snapshot_records_brief_and_resnapshots_same_head_after_renegotiation() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "brief-bound-patchset"]));
    let worktree = repo.home.join(".worktrees/repo-brief-bound-patchset");
    repo.commit(
        &worktree,
        "implementation.txt",
        "implemented\n",
        "feat: implement contract",
    );

    let first = stdout(
        repo.arc(&worktree)
            .args(["brief", "brief-bound-patchset", "--body-file", "-"])
            .write_stdin("contract v1\n"),
    );
    let first_brief = first
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();
    repo.arc(&worktree)
        .args(["snapshot", "brief-bound-patchset"])
        .assert()
        .success()
        .stdout(predicates::str::contains("patchset: ps-01"));
    repo.arc(&worktree)
        .args(["review", "brief-bound-patchset", "--verdict", "approved"])
        .assert()
        .success();

    let second = stdout(
        repo.arc(&worktree)
            .args([
                "brief",
                "brief-bound-patchset",
                "--body-file",
                "-",
                "--cause-note",
                "fixture revision",
            ])
            .write_stdin("contract v2\n"),
    );
    let second_brief = second
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();
    let unchanged_head = repo.head(&worktree);
    repo.arc(&worktree)
        .args(["snapshot", "brief-bound-patchset"])
        .assert()
        .success()
        .stdout(predicates::str::contains("patchset: ps-02"))
        .stdout(predicates::str::contains("(unchanged)").not());

    let show = json_stdout(
        repo.arc(&worktree)
            .args(["show", "brief-bound-patchset", "--json"]),
    );
    assert_eq!(show["patchsets"][0]["head"], unchanged_head);
    assert_eq!(show["patchsets"][1]["head"], unchanged_head);
    assert_eq!(show["patchsets"][0]["brief_ref"]["event_id"], first_brief);
    assert_eq!(show["patchsets"][0]["brief_version"], 1);
    assert_eq!(show["patchsets"][1]["brief_ref"]["event_id"], second_brief);
    assert_eq!(show["patchsets"][1]["brief_version"], 2);
    let status = json_stdout(repo.arc(&worktree).args(["status", "brief-bound-patchset"]));
    assert_eq!(status["latest_patchset"]["brief_version"], 2);
    assert!(!status["verdict"]["valid_for_current_head"]
        .as_bool()
        .unwrap());

    let human = stdout(repo.arc(&worktree).args(["show", "brief-bound-patchset"]));
    assert!(
        human.contains(&format!("brief: v1 (`{first_brief}`)")),
        "{human}"
    );
    assert!(
        human.contains(&format!("brief: v2 (`{second_brief}`)")),
        "{human}"
    );

    repo.arc(&worktree)
        .args(["snapshot", "brief-bound-patchset", "--brief-version", "1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("patchset: ps-03"));
    let status = json_stdout(repo.arc(&worktree).args(["status", "brief-bound-patchset"]));
    assert_eq!(status["latest_patchset"]["brief_version"], 1);
}

#[test]
fn review_snapshot_approval_is_immediately_valid_for_the_fresh_patchset() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "review-snapshot"]));
    let wt = repo.home.join(".worktrees/repo-review-snapshot");
    repo.commit(&wt, "review.txt", "review\n", "feat: review");

    repo.arc(&wt)
        .args([
            "review",
            "review-snapshot",
            "--snapshot",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "review-snapshot"]))).unwrap();
    assert_eq!(status["head_matches_latest_patchset"], true);
    assert_eq!(status["verdict"]["patchset_id"], "ps-01");
    assert_eq!(status["verdict"]["valid_for_current_head"], true);
}

#[test]
fn status_reports_clean_dirty_and_missing_worktrees() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "worktree-state"]));
    let worktree = repo.home.join(".worktrees/repo-worktree-state");

    let clean: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&worktree).args(["status", "--json"]))).unwrap();
    assert_eq!(clean["worktree_dirty"], false);

    fs::write(worktree.join("untracked.txt"), "uncommitted\n").unwrap();
    let dirty: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&worktree).args(["status", "--json"]))).unwrap();
    assert_eq!(dirty["worktree_dirty"], true);

    fs::remove_file(worktree.join("untracked.txt")).unwrap();
    git(
        &repo.root,
        &["worktree", "remove", worktree.to_str().unwrap()],
    );
    let missing: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "status",
        "worktree-state",
        "--json",
    ])))
    .unwrap();
    assert_eq!(missing["worktree_dirty"], serde_json::Value::Null);
}

#[test]
fn done_records_stage_patchset_and_evidence_then_returns_check_code() {
    let repo = Repo::new();
    commit_composed_gates(&repo);
    stdout(repo.arc(&repo.root).args(["begin", "done-sequence"]));
    let wt = repo.home.join(".worktrees/repo-done-sequence");
    repo.arc(&wt)
        .args(["claim", "done-sequence"])
        .assert()
        .success();
    repo.commit(&wt, "done.txt", "done\n", "feat: done");

    repo.arc(&wt)
        .args(["done", "done-sequence"])
        .assert()
        .code(3)
        .stdout(predicates::str::contains("missing or stale approval"));

    let events = stdout(repo.arc(&wt).args(["events", "--change", "done-sequence"]));
    let types = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .map(|event| event["event_type"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(types.iter().any(|kind| kind == "stage-set"));
    assert!(types.iter().any(|kind| kind == "patchset-added"));
    assert_eq!(
        types
            .iter()
            .filter(|kind| kind.as_str() == "verification-recorded")
            .count(),
        2
    );
}

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
            "--cause",
            "executor",
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
    assert_eq!(status["schema"], "arc-status/11");
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

    let held = stdout(
        repo.arc(&wt)
            .args(["hold", "fix-h", "--reason", "manual testing first"]),
    );
    let hold_id = held
        .split_whitespace()
        .nth(1)
        .expect("hold prints the event that identifies it")
        .to_string();
    repo.arc(&wt).args(["check", "fix-h"]).assert().code(4);
    repo.arc(&repo.root)
        .args(["integrate", "fix-h"])
        .assert()
        .code(4);

    repo.arc(&wt)
        .args(["release-hold", "fix-h", &hold_id])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-h"]).assert().success();
}

/// Holds are coordination, not a single switch. Two collaborators must be able
/// to hold the same change for unrelated reasons, and either one releasing must
/// leave the other's in force — otherwise the second hold silently erased the
/// first, and releasing erased both.
#[test]
fn independent_holds_release_by_event_id() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "two-holds"]));
    let hold_id = |out: String| {
        out.split_whitespace()
            .nth(1)
            .expect("hold prints the event that identifies it")
            .to_string()
    };
    let reviewer = hold_id(stdout(repo.arc(&repo.root).args([
        "hold",
        "two-holds",
        "--reason",
        "reviewer waiting on the user",
    ])));
    let release_manager = hold_id(stdout(repo.arc(&repo.root).args([
        "hold",
        "two-holds",
        "--reason",
        "release manager waiting on a dependency",
    ])));
    assert_ne!(reviewer, release_manager);

    // The printed identity is the HoldSet event itself, not a key arc made up.
    let set_events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "two-holds",
        "--type",
        "hold-set",
    ]));
    let ids: std::collections::BTreeSet<String> = set_events
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert!(ids.contains(&reviewer), "{set_events}");
    assert!(ids.contains(&release_manager), "{set_events}");
    assert_eq!(ids.len(), 2, "{set_events}");

    // An unset shell variable expands to an empty string, which is a prefix of
    // every hold. Releasing one by accident is what identity exists to stop.
    repo.arc(&repo.root)
        .args(["release-hold", "two-holds", ""])
        .assert()
        .failure();

    // With something to integrate, the hold is what the next action names —
    // and it names which hold, because there are two.
    repo.commit(
        &repo.home.join(".worktrees/repo-two-holds"),
        "work.rs",
        "done\n",
        "feat: work",
    );
    stdout(
        repo.arc(&repo.home.join(".worktrees/repo-two-holds"))
            .args(["snapshot", "two-holds"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["review", "two-holds", "--verdict", "approved"]),
    );
    // Which hold is named first is the ledger's ordering, not this test's
    // business; that it names one of the two active holds is the contract.
    let status = json_stdout(repo.arc(&repo.root).args(["status", "two-holds"]));
    let next = status["next_action"].as_str().unwrap().to_string();
    assert!(
        next == format!("release_hold:{reviewer}")
            || next == format!("release_hold:{release_manager}"),
        "{status}"
    );

    // The text inbox names the holds too, or the row cannot be acted on.
    let inbox_text = stdout(repo.arc(&repo.root).args(["inbox"]));
    assert!(inbox_text.contains(&reviewer), "{inbox_text}");
    assert!(
        inbox_text.contains("release manager waiting on a dependency"),
        "{inbox_text}"
    );
    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    let held = inbox["held"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["change_id"].as_str().unwrap().starts_with("two-holds"))
        .expect("held row");
    assert_eq!(held["holds"].as_array().unwrap().len(), 2, "{inbox}");

    let status = json_stdout(repo.arc(&repo.root).args(["status", "two-holds"]));
    assert_eq!(status["holds"].as_array().unwrap().len(), 2, "{status}");
    assert_eq!(
        status["blocker_summary"]["hold"]["active"], true,
        "{status}"
    );
    assert_eq!(
        status["blocker_summary"]["hold"]["reasons"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "{status}"
    );

    // Releasing one leaves the other in force, and the change stays held.
    repo.arc(&repo.root)
        .args(["release-hold", "two-holds", &reviewer])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "two-holds"]));
    let holds = status["holds"].as_array().unwrap();
    assert_eq!(holds.len(), 1, "{status}");
    assert_eq!(holds[0]["hold_event_id"], release_manager, "{status}");
    assert_eq!(
        holds[0]["reason"], "release manager waiting on a dependency",
        "{status}"
    );
    let check = String::from_utf8(
        repo.arc(&repo.root)
            .args(["check", "two-holds"])
            .assert()
            .failure()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(check.contains(&release_manager), "{check}");
    assert!(!check.contains(&reviewer), "{check}");

    // A release naming a hold that is not active is refused rather than
    // guessing which one was meant.
    repo.arc(&repo.root)
        .args(["release-hold", "two-holds", &reviewer])
        .assert()
        .failure();

    repo.arc(&repo.root)
        .args(["release-hold", "two-holds", &release_manager])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "two-holds"]));
    assert!(status["holds"].as_array().unwrap().is_empty(), "{status}");
    assert_eq!(
        status["blocker_summary"]["hold"]["active"], false,
        "{status}"
    );
}

/// The review map makes review-only-by-the-brief-author visible after the
/// fact; saying it before integration is the point of an advisory. It must
/// stay an advisory: an orchestrator's review is a valid review unless a
/// project's policy says otherwise, so this never moves readiness or the exit
/// code.
#[test]
fn brief_author_only_review_warns_but_remains_integrate_ready() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "briefed"]));
    let worktree = repo.home.join(".worktrees/repo-briefed");
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Lead").args([
        "brief",
        "briefed",
        "--body-file",
        "-",
    ]));
    repo.commit(&worktree, "work.rs", "done\n", "feat: work");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Executor")
            .args(["snapshot", "briefed"]),
    );
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Lead").args([
        "review",
        "briefed",
        "--verdict",
        "approved",
    ]));

    let status = json_stdout(repo.arc(&repo.root).args(["status", "briefed", "--json"]));
    let advisories = status["advisories"].as_array().unwrap();
    assert!(
        advisories
            .iter()
            .any(|advisory| advisory["code"] == "brief-author-only-review"
                && advisory["detail"].as_str().unwrap().contains("Lead")),
        "{status}"
    );
    // The identities differ, so nothing here claims the review was not
    // independent — only that one identity wrote the brief and the verdict.
    assert_eq!(status["integrate_ready"], true, "{status}");

    let check = json_stdout(repo.arc(&repo.root).args(["check", "briefed", "--json"]));
    assert_eq!(check["schema"], "arc-check/2", "{check}");
    assert_eq!(check["ready"], true, "{check}");
    assert_eq!(check["exit_code"], 0, "{check}");
    assert!(
        check["advisories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|advisory| advisory["code"] == "brief-author-only-review"),
        "{check}"
    );
    repo.arc(&repo.root)
        .args(["check", "briefed"])
        .assert()
        .code(0);

    // A brief version recorded after the snapshot describes work this patchset
    // is not, so it cannot change who briefed what shipped.
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Someone Else").args([
        "brief",
        "briefed",
        "--body-file",
        "-",
        "--cause-note",
        "a later correction nobody re-snapshotted against",
    ]));
    let status = json_stdout(repo.arc(&repo.root).args(["status", "briefed", "--json"]));
    assert!(
        status["advisories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|advisory| advisory["code"] == "brief-author-only-review"
                && advisory["detail"].as_str().unwrap().contains("Lead")),
        "{status}"
    );

    // A reviewer who filed only a finding on this patchset has approved
    // nothing here, so it neither silences the advisory nor lets it claim a
    // verdict that does not exist — whatever that reviewer approved earlier.
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Finder").args([
        "finding",
        "briefed",
        "--summary",
        "a note, not a verdict",
        "--severity",
        "minor",
    ]));
    let status = json_stdout(repo.arc(&repo.root).args(["status", "briefed", "--json"]));
    assert!(
        status["advisories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|advisory| advisory["code"] == "brief-author-only-review"),
        "{status}"
    );

    // A tagged preflight is read before `integrate --tag`, so it carries the
    // advisories too.
    stdout(
        repo.arc(&repo.root)
            .args(["metadata", "briefed", "--tag", "series"]),
    );
    let tagged = stdout(repo.arc(&repo.root).args(["check", "--tag", "series"]));
    assert!(tagged.contains("brief-author-only-review"), "{tagged}");

    repo.arc(&repo.root)
        .args(["integrate", "briefed"])
        .assert()
        .success();
}

/// `integrate` computes readiness from the invocation worktree's gate and
/// policy files, the latest verdict, exact-head evidence, findings,
/// dependencies and hold state — and before this, the closure it wrote
/// retained none of it. An auditor had to replay preceding events and recover
/// the contemporaneous config from Git, and uncommitted policy state was
/// unrecoverable entirely. So the probe changes both files without committing
/// them, and asserts the event holds those exact values.
#[test]
fn guarded_integration_records_exact_authorization_basis() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare a gate"]);

    let prerequisite = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "first",
        "--no-worktree",
    ])));
    let dependent = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "second",
        "--blocked-by",
        "first",
    ])));

    // The prerequisite integrates first, so the dependent's basis has a
    // closure to name.
    let first_worktree = repo.home.join(".worktrees/repo-first");
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            first_worktree.to_str().unwrap(),
            "arc/first",
        ],
    );
    repo.commit(&first_worktree, "first.rs", "one\n", "feat: first");
    stdout(repo.arc(&first_worktree).args(["snapshot", "first"]));
    repo.arc(&first_worktree)
        .args(["verify", "first", "--gate", "unit"])
        .assert()
        .success();
    stdout(
        repo.arc(&repo.root)
            .args(["review", "first", "--verdict", "approved"]),
    );
    repo.arc(&repo.root)
        .args(["integrate", "first"])
        .assert()
        .success();

    let worktree = repo.home.join(".worktrees/repo-second");
    repo.commit(&worktree, "second.rs", "two\n", "feat: second");
    git(&worktree, &["merge", "--no-edit", "master"]);
    // This change declares the gate with a timeout the target does not. The
    // evidence must run under the declaration it will be recorded against —
    // a gate is green for what it ran, timeout included.
    fs::create_dir_all(worktree.join(".arc")).unwrap();
    fs::write(
        worktree.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\ntimeout = \"90s\"\n",
    )
    .unwrap();
    git(&worktree, &["add", ".arc/gates.toml"]);
    git(
        &worktree,
        &["commit", "-m", "chore: declare a gate timeout"],
    );
    stdout(repo.arc(&worktree).args(["snapshot", "second"]));
    repo.arc(&worktree)
        .args(["verify", "second", "--gate", "unit"])
        .assert()
        .success();
    let verdict_event =
        stdout(
            repo.arc(&repo.root)
                .args(["review", "second", "--verdict", "approved"]),
        );
    let verdict_event = verdict_event
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .expect("review prints its event")
        .to_string();

    // Uncommitted policy in the *invocation* worktree: exactly the state Git
    // cannot recover afterwards, and the state readiness is computed from.
    // The target worktree must stay clean, so the edit belongs here.
    fs::write(
        worktree.join(".arc/policy.toml"),
        "[policy]\nrequire_declared_actor = false\nforbid_self_approval = false\n\n[provenance]\ngit_identity = \"shared\"\n",
    )
    .unwrap();

    // The dry run prints the basis it would record, and writes nothing.
    let dry_out = repo
        .arc(&worktree)
        .args(["integrate", "second", "--dry-run"])
        .assert()
        .get_output()
        .clone();
    let dry = format!(
        "{}{}",
        String::from_utf8_lossy(&dry_out.stdout),
        String::from_utf8_lossy(&dry_out.stderr)
    );
    assert!(
        dry.contains("authorization basis it would record"),
        "dry run output was: {dry}"
    );
    assert!(dry.contains(&verdict_event), "{dry}");
    assert!(dry.contains("git_identity=shared"), "{dry}");

    repo.arc(&worktree)
        .args(["integrate", "second", "--into", "master"])
        .assert()
        .success();

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "second",
        "--type",
        "change-integrated",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.trim()).unwrap();
    let basis = &event["authorization"];
    assert_eq!(basis["verdict_event_id"], verdict_event, "{event}");
    // The exact evidence event, not merely that some evidence existed.
    let recorded = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "second",
        "--type",
        "verification-recorded",
    ]));
    let evidence_id = recorded
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["gate"] == "unit")
        .map(|event| event["event_id"].as_str().unwrap().to_string())
        .next_back()
        .expect("a recorded gate verification");
    assert_eq!(basis["gate_evidence"]["unit"], evidence_id, "{event}");
    assert_eq!(
        basis["prerequisites"][0]["change_id"], prerequisite,
        "{event}"
    );
    assert!(
        basis["prerequisites"][0]["integrated_commit"]
            .as_str()
            .is_some(),
        "{event}"
    );
    assert_eq!(basis["blocking_findings"], serde_json::json!([]), "{event}");
    assert_eq!(basis["holds"], serde_json::json!([]), "{event}");
    // The uncommitted values, not the committed ones.
    assert_eq!(basis["gates"]["unit"]["timeout"], 90, "{event}");
    assert_eq!(
        basis["policy"]["provenance_git_identity"], "shared",
        "{event}"
    );
    assert_eq!(event["target_branch"], "master", "{event}");
    assert!(!dependent.is_empty());
    // No waiver was involved, so none is claimed.
    assert!(basis.get("audit_debt_event_id").is_none(), "{event}");
}

/// The basis records what authorized the merge. Editing a gate's declaration
/// after its evidence was recorded means the declared check has not run, so
/// the gate is not green — and the basis can never pair a command with
/// evidence for a different one.
#[test]
fn changing_a_gate_declaration_ungreens_its_recorded_evidence() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare a gate"]);
    stdout(repo.arc(&repo.root).args(["begin", "redeclared"]));
    let worktree = repo.home.join(".worktrees/repo-redeclared");
    repo.commit(&worktree, "work.rs", "done\n", "feat: work");
    git(&worktree, &["merge", "--no-edit", "master"]);
    stdout(repo.arc(&worktree).args(["snapshot", "redeclared"]));
    repo.arc(&worktree)
        .args(["verify", "redeclared", "--gate", "unit"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&worktree).args(["status", "redeclared", "--json"]));
    assert_eq!(status["gates"][0]["green_at_head"], true, "{status}");

    // A different command is a different check, which nothing has run.
    fs::write(
        worktree.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true # a different check\"\n",
    )
    .unwrap();
    let status = json_stdout(repo.arc(&worktree).args(["status", "redeclared", "--json"]));
    assert_eq!(status["gates"][0]["green_at_head"], false, "{status}");
    assert_eq!(status["gates"][0]["declaration_changed"], true, "{status}");
    assert_eq!(status["integrate_ready"], false, "{status}");
    // And the reader is told which repair this is: the evidence's provenance
    // is fine, the declaration moved.
    let resume = stdout(repo.arc(&worktree).args(["resume", "redeclared"]));
    assert!(
        resume.contains("the gate declaration changed since this evidence was recorded"),
        "{resume}"
    );

    // A timeout is part of the declaration too: the same command under a
    // laxer timeout is not evidence for a stricter one.
    fs::write(
        worktree.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\ntimeout = \"1s\"\n",
    )
    .unwrap();
    let status = json_stdout(repo.arc(&worktree).args(["status", "redeclared", "--json"]));
    assert_eq!(status["gates"][0]["declaration_changed"], true, "{status}");
    assert_eq!(status["gates"][0]["green_at_head"], false, "{status}");

    // Reuse honours the same rule: the same command under a stricter timeout
    // is a declaration nothing has run.
    let reused =
        stdout(
            repo.arc(&worktree)
                .args(["verify", "redeclared", "--all", "--skip-green"]),
        );
    assert!(!reused.contains("skipped (green at head)"), "{reused}");

    // Back to a command nothing has run, for the reuse check below.
    fs::write(
        worktree.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true # a different check\"\n",
    )
    .unwrap();

    // Reuse is reuse of a run: --skip-green must not report the old pass as
    // satisfying a declaration nothing has run.
    let reused =
        stdout(
            repo.arc(&worktree)
                .args(["verify", "redeclared", "--all", "--skip-green"]),
        );
    assert!(!reused.contains("skipped (green at head)"), "{reused}");
    // It ran the declaration that is current, and the evidence says so. It is
    // still not green, because the edited declaration is uncommitted and the
    // tree is therefore dirty — a separate rule, and the honest one.
    let status = json_stdout(repo.arc(&worktree).args(["status", "redeclared", "--json"]));
    assert_eq!(
        status["gates"][0]["command"], "true # a different check",
        "{status}"
    );
    assert_eq!(status["gates"][0]["worktree_dirty"], true, "{status}");
}

/// A merge arc guarded and a merge somebody performed elsewhere are different
/// facts, and a ledger whose reason to exist is being authoritative about
/// integration cannot write them byte-identically. The asserted variant
/// deliberately carries no authorization; it records what was claimed.
#[test]
fn guarded_and_asserted_integrations_have_distinct_event_types_and_targets() {
    let repo = Repo::new();

    // Guarded: arc performs the merge under its own preconditions.
    stdout(repo.arc(&repo.root).args(["begin", "guarded"]));
    let worktree = repo.home.join(".worktrees/repo-guarded");
    repo.commit(&worktree, "guarded.rs", "done\n", "feat: guarded");
    stdout(repo.arc(&worktree).args(["snapshot", "guarded"]));
    let source_head = repo.head(&worktree);
    let target_before = repo.head(&repo.root);
    stdout(
        repo.arc(&repo.root)
            .args(["review", "guarded", "--verdict", "approved"]),
    );
    repo.arc(&repo.root)
        .args(["integrate", "guarded"])
        .assert()
        .success();

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "guarded",
        "--type",
        "change-integrated",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.trim()).unwrap();
    assert_eq!(event["source_head"], source_head, "{event}");
    assert_eq!(event["source_patchset_id"], "ps-01", "{event}");
    assert_eq!(event["target_branch"], "master", "{event}");
    assert_eq!(event["target_before"], target_before, "{event}");
    assert!(
        stdout(repo.arc(&repo.root).args([
            "events",
            "--change",
            "guarded",
            "--type",
            "change-closed",
        ]))
        .trim()
        .is_empty(),
        "a guarded merge writes no change-closed event"
    );
    let status = json_stdout(repo.arc(&repo.root).args(["status", "guarded", "--json"]));
    assert_eq!(status["closure"]["integration"], "guarded", "{status}");

    // The store now holds an event an older build would skip, so it says so.
    // A barrier nothing stamps protects only stores this build created.
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 3, "{config}");

    // Asserted: somebody else merged it, and says so afterwards.
    stdout(repo.arc(&repo.root).args(["begin", "asserted"]));
    let asserted_worktree = repo.home.join(".worktrees/repo-asserted");
    repo.commit(
        &asserted_worktree,
        "asserted.rs",
        "done\n",
        "feat: asserted",
    );
    stdout(repo.arc(&asserted_worktree).args(["snapshot", "asserted"]));
    let asserted_head = repo.head(&asserted_worktree);
    let before_external = repo.head(&repo.root);
    git(
        &repo.root,
        &[
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            "external merge",
            &asserted_head,
        ],
    );
    let external = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args([
            "close",
            "asserted",
            "--assert-integrated",
            &external,
            "--patchset",
            "ps-01",
            "--into",
            "master",
        ])
        .assert()
        .success();

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "asserted",
        "--type",
        "integration-asserted",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.trim()).unwrap();
    assert_eq!(event["integrated_commit"], external, "{event}");
    assert_eq!(event["source_head"], asserted_head, "{event}");
    assert_eq!(event["target_before"], before_external, "{event}");
    assert_eq!(event["source_patchset_id"], "ps-01", "{event}");
    assert_eq!(event["target_branch"], "master", "{event}");
    assert!(
        stdout(repo.arc(&repo.root).args([
            "events",
            "--change",
            "asserted",
            "--type",
            "change-closed",
        ]))
        .trim()
        .is_empty(),
        "an asserted integration writes no change-closed event either"
    );
    let status = json_stdout(repo.arc(&repo.root).args(["status", "asserted", "--json"]));
    assert_eq!(status["closure"]["integration"], "asserted", "{status}");

    // Both are integrated; the ledger says how each one got there, and the
    // human view says it too rather than rendering them identically.
    assert_eq!(status["state"], "closed", "{status}");
    let shown = stdout(repo.arc(&repo.root).args(["show", "asserted"]));
    assert!(shown.contains("asserted; arc did not guard it"), "{shown}");
    let shown = stdout(repo.arc(&repo.root).args(["show", "guarded"]));
    assert!(shown.contains("guarded by arc"), "{shown}");

    // A fast-forward has no merge commit, so its first parent is the change's
    // own previous commit — recording that as the prior target would put the
    // change's work outside the range it integrated. Absent is honest.
    stdout(repo.arc(&repo.root).args(["begin", "fast-forward"]));
    let ff_worktree = repo.home.join(".worktrees/repo-fast-forward");
    repo.commit(&ff_worktree, "ff-one.rs", "one\n", "feat: one");
    repo.commit(&ff_worktree, "ff-two.rs", "two\n", "feat: two");
    stdout(repo.arc(&ff_worktree).args(["snapshot", "fast-forward"]));
    let before_ff = repo.head(&repo.root);
    let ff_head = repo.head(&ff_worktree);
    git(&repo.root, &["merge", "--ff-only", &ff_head]);
    repo.arc(&repo.root)
        .args(["close", "fast-forward", "--assert-integrated", &ff_head])
        .assert()
        .success();
    let event: serde_json::Value = serde_json::from_str(
        stdout(repo.arc(&repo.root).args([
            "events",
            "--change",
            "fast-forward",
            "--type",
            "integration-asserted",
        ]))
        .trim(),
    )
    .unwrap();
    assert!(
        event.get("target_before").is_none(),
        "a fast-forward has no prior target to read: {event}"
    );

    // The caller can name it, and then it is recorded.
    stdout(repo.arc(&repo.root).args(["begin", "named-base"]));
    let named_worktree = repo.home.join(".worktrees/repo-named-base");
    repo.commit(&named_worktree, "named.rs", "x\n", "feat: named");
    stdout(repo.arc(&named_worktree).args(["snapshot", "named-base"]));
    let named_head = repo.head(&named_worktree);
    git(&repo.root, &["merge", "--ff-only", &named_head]);
    repo.arc(&repo.root)
        .args([
            "close",
            "named-base",
            "--assert-integrated",
            &named_head,
            "--target-before",
            &before_ff,
        ])
        .assert()
        .success();
    let event: serde_json::Value = serde_json::from_str(
        stdout(repo.arc(&repo.root).args([
            "events",
            "--change",
            "named-base",
            "--type",
            "integration-asserted",
        ]))
        .trim(),
    )
    .unwrap();
    assert_eq!(event["target_before"], before_ff, "{event}");

    // A merge is its own witness: overriding its first parent would record a
    // range the merge did not integrate.
    repo.arc(&repo.root)
        .args([
            "close",
            "asserted",
            "--assert-integrated",
            &external,
            "--target-before",
            &before_external,
        ])
        .assert()
        .failure();

    // An assertion arc did not guard is still checked against Git: it must
    // name a branch that exists, a revision that contains the patchset head,
    // and one that is actually on that branch.
    stdout(repo.arc(&repo.root).args(["begin", "unrelated"]));
    let unrelated_worktree = repo.home.join(".worktrees/repo-unrelated");
    repo.commit(&unrelated_worktree, "other.rs", "x\n", "feat: unrelated");
    stdout(
        repo.arc(&unrelated_worktree)
            .args(["snapshot", "unrelated"]),
    );
    repo.arc(&repo.root)
        .args([
            "close",
            "unrelated",
            "--assert-integrated",
            &external,
            "--into",
            "master",
        ])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args([
            "close",
            "unrelated",
            "--assert-integrated",
            &repo.head(&unrelated_worktree),
            "--into",
            "no-such-branch",
        ])
        .assert()
        .failure();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "unrelated", "--json"]));
    assert_eq!(status["state"], "open", "{status}");
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

#[test]
fn discussion_event_prefixes_resolve_or_list_ambiguous_candidates() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "prefixes"]));
    let wt = repo.home.join(".worktrees").join("repo-prefixes");
    let first = stdout(repo.arc(&wt).args([
        "finding",
        "prefixes",
        "--summary",
        "first",
        "--body",
        "first body",
    ]));
    let first_event = first
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();
    let second = stdout(repo.arc(&wt).args([
        "finding",
        "prefixes",
        "--summary",
        "second",
        "--body",
        "second body",
    ]));
    let second_event = second
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();

    let unique_prefix = (1..=first_event.len())
        .map(|length| &first_event[..length])
        .find(|prefix| !second_event.starts_with(*prefix))
        .unwrap();
    repo.arc(&wt)
        .args(["resolve", "prefixes", unique_prefix, "--status", "resolved"])
        .assert()
        .success();

    let shared_prefix = &first_event[..1];
    repo.arc(&wt)
        .args(["reply", "prefixes", shared_prefix, "--body", "ambiguous"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ambiguous discussion event"))
        .stderr(predicates::str::contains(&first_event))
        .stderr(predicates::str::contains(&second_event));
}

#[test]
fn status_projection_and_stage_note_file_read_stdin() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "projected"]));
    let wt = repo.home.join(".worktrees").join("repo-projected");

    repo.arc(&wt)
        .args(["status", "projected", "--get", "claim.owner.actor"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no value at claim.owner.actor"));
    repo.arc(&wt)
        .args(["status", "projected", "--get", "schema"])
        .assert()
        .success()
        .stdout("arc-status/11\n");

    repo.arc(&wt)
        .args([
            "stage",
            "projected",
            "implementing",
            "--claim",
            "--note-file",
            "-",
        ])
        .write_stdin("from stdin\n")
        .assert()
        .success();
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "projected"]))).unwrap();
    assert_eq!(status["claim"]["note"], "from stdin");
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
fn closed_change_append_policy_matches_command_families() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "closed-policy", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["claim", "closed-policy"])
        .assert()
        .success();
    let hold_id =
        stdout(
            repo.arc(&repo.root)
                .args(["hold", "closed-policy", "--reason", "operator pause"]),
        )
        .split_whitespace()
        .nth(1)
        .expect("hold prints the event that identifies it")
        .to_string();
    let comment = stdout(repo.arc(&repo.root).args([
        "comment",
        "closed-policy",
        "--body",
        "historical discussion",
    ]));
    let comment_event = comment
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();
    repo.arc(&repo.root)
        .args([
            "forge",
            "declare",
            "closed-policy",
            "--host",
            "example.invalid",
            "--base-repo",
            "owner/repo",
            "--base-ref",
            "master",
            "--head-repo",
            "owner/repo",
            "--head-ref",
            "arc/closed-policy",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["close", "closed-policy", "--abandoned"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args([
            "message",
            "closed-policy",
            "--type",
            "status",
            "--summary",
            "closure observed",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["comment", "closed-policy", "--body", "after closure"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "reply",
            "closed-policy",
            comment_event,
            "--body",
            "after closure",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["release-claim", "closed-policy"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "release-hold",
            "closed-policy",
            &hold_id,
            "--reason",
            "closure ended liveness",
        ])
        .assert()
        .success();

    let head = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args([
            "forge",
            "link",
            "closed-policy",
            "--pr",
            "1",
            "--url",
            "https://example.invalid/owner/repo/pulls/1",
            "--base-repo",
            "owner/repo",
            "--base-ref",
            "master",
            "--head-repo",
            "owner/repo",
            "--head-ref",
            "arc/closed-policy",
            "--head-sha",
            &head,
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "forge",
            "checks",
            "closed-policy",
            "--pr-head",
            &head,
            "--state",
            "passed",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["forge", "pr-state", "closed-policy", "--state", "open"])
        .assert()
        .success();

    for args in [
        vec!["hold", "closed-policy", "--reason", "new work"],
        vec![
            "forge",
            "declare",
            "closed-policy",
            "--host",
            "example.invalid",
            "--base-repo",
            "owner/repo",
            "--base-ref",
            "master",
            "--head-repo",
            "owner/repo",
            "--head-ref",
            "arc/closed-policy",
        ],
    ] {
        repo.arc(&repo.root).args(args).assert().failure();
    }
}

#[test]
fn append_policy_has_a_single_authority() {
    let commands = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
    let allowed = [
        "chain.rs::chain::state:ifstate.is_closed(){",
        "claims.rs::ready_candidate::!candidate.is_closed()",
        "gatekeeping.rs::check_tagged::ifstate.is_closed(){",
        "gatekeeping.rs::close::ifst.is_closed(){",
        "gatekeeping.rs::integrate_one::ifst.is_closed()&&matches!(closed_behavior,ClosedBehavior::SkipTagged){",
        "hooks.rs::change_for_branch::ifstate.is_closed(){",
        "audit.rs::declare_audit_debt::letpatchset_id=ifst.is_closed(){",
        "hooks.rs::post_commit::ifstate.is_closed(){",
        "hooks.rs::prepare_commit_msg::ifstate.is_closed(){",
        "lifecycle.rs::begin::ifst.is_closed(){",
        "lifecycle.rs::list::.filter(|state|!open_only||!state.is_closed())",
        "lifecycle.rs::list_row::\"state\":ifstate.is_closed(){\"closed\"}else{\"open\"},",
        "lifecycle.rs::status_matches::\"closed\"=>state.is_closed(),",
        "messaging.rs::collect_inbox::ifstate.is_closed(){",
        "mod.rs::find_unblocked_changes::&&!candidate.is_closed()",
        "stats.rs::change_stats::state:ifstate.is_closed(){\"closed\"}else{\"open\"},",
        "workspace.rs::restack::.filter(|candidate|!candidate.is_closed()&&candidate.blocked_by.contains(&change_id))",
        "workspace.rs::restack::if!state.is_closed(){",
        "workspace.rs::ledger_queues::if!state.is_closed()&&crate::inbox::needs_review(state){",
        "workspace.rs::workspace_inbox::ifstate.is_closed(){",
        "workspace.rs::workspace_list::.filter(|state|!state.is_closed())",
    ];
    let mut found = Vec::new();
    let mut bailing = Vec::new();

    for entry in fs::read_dir(commands).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        let file = path.file_name().unwrap().to_string_lossy();
        for (offset, _) in source.match_indices("is_closed()") {
            let function = source[..offset]
                .lines()
                .rev()
                .find_map(|line| {
                    let marker = line.find("fn ")?;
                    let name = &line[marker + 3..];
                    Some(name.split(['(', '<']).next().unwrap().trim().to_owned())
                })
                .unwrap();
            let line_start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
            let line_end = source[offset..]
                .find('\n')
                .map_or(source.len(), |index| offset + index);
            let compact_line = source[line_start..line_end]
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>();
            let site = format!("{file}::{function}::{compact_line}");
            found.push(site.clone());

            let before_check = &source[line_start..offset];
            if before_check.split_whitespace().any(|word| word == "if")
                || before_check.trim_end().ends_with("if")
            {
                let tail = &source[offset..];
                if let Some(open) = tail.find('{') {
                    let mut depth = 0usize;
                    let mut close = None;
                    for (index, character) in tail[open..].char_indices() {
                        match character {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    close = Some(open + index + 1);
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    let block = &tail[open..close.unwrap()];
                    let compact_block = block
                        .chars()
                        .filter(|character| !character.is_whitespace())
                        .collect::<String>();
                    let lifecycle_close = site == "gatekeeping.rs::close::ifst.is_closed(){"
                        && compact_block.contains(r#"bail!("change{change_id}isalreadyclosed");"#);
                    if compact_block.contains("bail!(") && !lifecycle_close {
                        bailing.push(site);
                    }
                }
            }
        }
    }

    found.sort();
    let mut allowed = allowed.into_iter().map(str::to_owned).collect::<Vec<_>>();
    allowed.sort();
    let unexpected = found
        .iter()
        .filter(|site| !allowed.contains(site))
        .collect::<Vec<_>>();
    let missing = allowed
        .iter()
        .filter(|site| !found.contains(site))
        .collect::<Vec<_>>();
    assert!(
        unexpected.is_empty() && missing.is_empty() && bailing.is_empty(),
        "closed-change append guards must use ensure_append_allowed; \
         unexpected sites: {unexpected:?}; missing allowlisted sites: {missing:?}; \
         bailing sites: {bailing:?}"
    );
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

#[test]
fn piped_output_dies_on_sigpipe_without_panicking() {
    use std::os::unix::process::ExitStatusExt;

    let repo = Repo::new();
    // Produce enough ledger events that `arc events` output overflows the OS
    // pipe buffer, so the child is guaranteed to still be writing when the
    // reader goes away.
    for i in 0..250 {
        repo.arc(&repo.root)
            .args(["begin", &format!("ch{i}"), "--no-worktree"])
            .assert()
            .success();
    }

    let binary = std::env::var_os("CARGO_BIN_EXE_arc").expect("cargo should provide arc binary");
    let mut child = Command::new(binary)
        .args(["events"])
        .current_dir(&repo.root)
        .env("HOME", &repo.home)
        .env("ARC_ACTOR", "tester")
        .env("ARC_HARNESS", "test")
        .env("ARC_SESSION", "session-a")
        .env_remove("ARC_ROLE")
        .env_remove("ARC_DATA_DIR")
        .env_remove("ARC_DATA_ROOT")
        .env_remove("ARC_WORKTREES_DIR")
        .env_remove("AI_HOME")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Read a single line, then drop the read end so the child's next write
    // lands on a broken pipe.
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    drop(reader);

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().unwrap();

    assert_eq!(
        status.signal(),
        Some(13),
        "arc should be terminated by SIGPIPE, got {status:?} (stderr: {stderr})"
    );
    assert!(
        !stderr.contains("panic"),
        "arc must not panic on a broken pipe: {stderr}"
    );
}

/// Help copy is the teaching surface for an agent-facing CLI, so an
/// undescribed flag or positional is a gap in the contract rather than a
/// cosmetic one. This walks every command *and every nested group*, and
/// fails naming what it found, so the class cannot come back one flag at a
/// time.
///
/// Walking only the top level was the first version's defect, and the region
/// it skipped held twenty-eight undescribed entries — including a bare
/// `--json`, the exact shape the sweep had fixed everywhere it looked.
#[test]
fn every_flag_and_positional_carries_a_description() {
    let repo = Repo::new();

    fn subcommands(help: &str) -> Vec<String> {
        help.lines()
            .skip_while(|line| !line.starts_with("Commands:"))
            .skip(1)
            .take_while(|line| !line.trim().is_empty() || false)
            .take_while(|line| !line.starts_with("Options:"))
            .filter(|line| line.starts_with("  ") && !line.starts_with("    "))
            .filter_map(|line| line.split_whitespace().next())
            .filter(|name| *name != "help")
            .map(str::to_string)
            .collect()
    }

    /// An item is described when text follows it — on the same line in
    /// clap's two-column layout, or on the next line in its long one.
    fn undescribed(help: &str) -> Vec<String> {
        let lines: Vec<&str> = help.lines().collect();
        let mut found = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let item = line.trim();
            // `[possible values: …]` and `[env: …]` annotate the line above.
            if item.contains(": ") {
                continue;
            }
            let head = item.split_whitespace().next().unwrap_or("");
            let is_flag = head.starts_with("--")
                && head
                    .trim_start_matches('-')
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-');
            let is_positional = head.starts_with('[') || head.starts_with('<');
            if !(is_flag || is_positional) {
                continue;
            }
            // Two-column layout: the description sits after the item.
            let rest = item[head.len()..].trim();
            let rest = rest.strip_prefix('<').map_or(rest, |_| {
                rest.split_once('>').map_or("", |(_, tail)| tail.trim())
            });
            if !rest.is_empty() {
                continue;
            }
            let next = lines.get(index + 1).map(|line| line.trim()).unwrap_or("");
            if next.is_empty()
                || next.starts_with('-')
                || next.ends_with(':')
                || next.starts_with('[')
                || next.starts_with('<')
            {
                found.push(item.to_string());
            }
        }
        found
    }

    let mut queue: Vec<Vec<String>> = subcommands(&stdout(repo.arc(&repo.root).arg("--help")))
        .into_iter()
        .map(|name| vec![name])
        .collect();
    assert!(
        queue.len() > 20,
        "expected the full command list: {queue:?}"
    );

    let mut gaps = Vec::new();
    let mut walked = 0usize;
    while let Some(path) = queue.pop() {
        let mut args: Vec<&str> = path.iter().map(String::as_str).collect();
        args.push("--help");
        let help = stdout(repo.arc(&repo.root).args(&args));
        let nested = subcommands(&help);
        if !nested.is_empty() {
            for name in nested {
                let mut child = path.clone();
                child.push(name);
                queue.push(child);
            }
            continue;
        }
        walked += 1;
        for item in undescribed(&help) {
            gaps.push(format!("{}: {item}", path.join(" ")));
        }
    }
    // The nested groups are the half that was missed; assert they were
    // reached rather than trusting the walk.
    assert!(
        walked > 50,
        "expected to reach every leaf command, saw {walked}"
    );
    assert!(
        gaps.is_empty(),
        "undescribed help entries:\n  {}",
        gaps.join("\n  ")
    );
}

/// Every kind is a verb under `arc journal`, so typing the kind at the top
/// level is the likeliest miss a cold session makes — and the error is the
/// cheapest place to teach it.
#[test]
fn a_top_level_kind_name_is_redirected_to_its_journal_verb() {
    let repo = Repo::new();
    for (typed, expected) in [
        ("todo", "arc journal todo"),
        ("handoff", "arc journal handoff"),
        ("decision", "arc journal decision"),
        ("questions", "arc journal questions"),
        ("feature-request", "arc journal feature-request"),
    ] {
        repo.arc(&repo.root)
            .args([typed, "some-topic"])
            .assert()
            .failure()
            .stderr(predicates::str::contains(expected));
    }
}

/// The redirect replaces clap's spelling guess rather than printing beside
/// it: clap answers `questions` with `completions`, and two tips would leave
/// the caller choosing between a right one and a wrong one.
#[test]
fn a_redirect_suppresses_the_similarity_guess_but_leaves_it_otherwise() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["questions", "x"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("completions").not());
    // An unrecognized name with no mapping keeps clap's own error and usage.
    repo.arc(&repo.root)
        .args(["nonsense", "x"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("Usage: arc"));
}

/// The guide is a self-contained teaching surface, so the two halves a
/// session needs but cannot infer — declaring identity before writing, and
/// what to file when it ends — have to be in it. The orient section it
/// already teaches is fed entirely by the session-end writes.
#[test]
fn the_guide_teaches_identity_and_how_a_session_ends() {
    let repo = Repo::new();
    let guide = stdout(&mut repo.arc(&repo.root));

    assert!(guide.contains("SAY WHO YOU ARE"), "{guide}");
    assert!(guide.contains("arc env"), "{guide}");
    assert!(guide.contains("ARC_ACTOR"), "{guide}");
    // The failure mode is the point: an undeclared write succeeds.
    assert!(guide.contains("an actor nobody claimed"), "{guide}");

    assert!(guide.contains("END A SESSION"), "{guide}");
    for verb in ["journal handoff", "journal conclusion", "journal memory"] {
        assert!(guide.contains(verb), "missing {verb}:\n{guide}");
    }
    // Ordering: identity comes before the orient verbs that write, and the
    // session-end stanza sits before profiles rather than after the guide.
    let identity = guide.find("SAY WHO YOU ARE").unwrap();
    let orient = guide.find("ORIENT").unwrap();
    let ending = guide.find("END A SESSION").unwrap();
    let profiles = guide.find("PROFILES").unwrap();
    assert!(identity < orient, "identity must precede orient");
    assert!(ending < profiles, "session end must precede profiles");
}

/// `arc env` exits non-zero whenever the harness exports no session variable,
/// which is ordinary rather than broken — so its help has to say which
/// variables it reads and what the non-zero exit means.
#[test]
fn env_help_explains_detection_and_its_non_zero_exit() {
    let repo = Repo::new();
    let help = stdout(repo.arc(&repo.root).args(["env", "--help"]));
    for expected in ["CLAUDE_SESSION_ID", "PI_SESSION_ID", "eval", "non-zero"] {
        assert!(help.contains(expected), "missing {expected}:\n{help}");
    }
}

/// A blocking finding is released by a disposition that releases it, not by
/// any disposition: `still-open` and `disputed` are recorded and leave the
/// gate shut. The help promised what `arc resolve` would not always deliver.
#[test]
fn the_blocking_flag_names_which_dispositions_release_the_gate() {
    let repo = Repo::new();
    let help = stdout(repo.arc(&repo.root).args(["finding", "--help"]));
    for expected in ["resolved", "accepted-risk", "obsolete"] {
        assert!(help.contains(expected), "missing {expected}:\n{help}");
    }
    assert!(!help.contains("until it is disposed of"), "{help}");
}

/// Compaction cannot rank what it has not been told matters. A session that
/// knows a premise was checked, or an approach abandoned, can say so while it
/// still knows — and `resume` is where a cold successor reads it back.
#[test]
fn kept_context_survives_into_a_cold_resume() {
    let repo = Repo::new();
    begin_change(&repo, "keeper", None);
    repo.arc(&repo.root)
        .args([
            "keep",
            "keeper",
            "--kind",
            "rejected",
            "--body",
            "Splicing both sides of a conflict cuts through function bodies.",
            "--evidence",
            "cargo fmt reported an unclosed delimiter",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "keep",
            "keeper",
            "--kind",
            "hypothesis",
            "--body",
            "The meter may weight cached input near zero.",
        ])
        .assert()
        .success();

    let resume = repo
        .arc(&repo.root)
        .args(["resume", "keeper"])
        .assert()
        .success();
    let text = String::from_utf8_lossy(&resume.get_output().stdout).to_string();
    assert!(text.contains("## Kept Context"), "{text}");
    assert!(
        text.contains("**rejected**") && text.contains("function bodies"),
        "a rejected approach is the fact a cold session most needs: {text}"
    );
    assert!(
        text.contains("evidence: cargo fmt reported an unclosed delimiter"),
        "{text}"
    );
    // A guess must not read back as a finding.
    assert!(text.contains("**hypothesis**"), "{text}");

    let status = json_stdout(repo.arc(&repo.root).args(["status", "keeper", "--json"]));
    assert_eq!(status["kept"].as_array().unwrap().len(), 2);
    assert_eq!(status["kept"][0]["kind"], "rejected");
    assert_eq!(status["kept"][1]["kind"], "hypothesis");
    assert!(
        status["kept"][1]["evidence"].is_null(),
        "no evidence offered"
    );
}

/// A change with nothing kept says so, rather than omitting the section and
/// leaving a reader unsure whether it was empty or unsupported.
#[test]
fn resume_names_the_absence_of_kept_context() {
    let repo = Repo::new();
    begin_change(&repo, "bare", None);
    let resume = repo
        .arc(&repo.root)
        .args(["resume", "bare"])
        .assert()
        .success();
    let text = String::from_utf8_lossy(&resume.get_output().stdout).to_string();
    assert!(text.contains("## Kept Context"), "{text}");
    assert!(text.contains("(none kept)"), "{text}");
}

/// A stored newline must not break resume's bullet list or inject a heading
/// into the section that follows: the ledger keeps the body verbatim and the
/// rendering flattens it.
#[test]
fn kept_context_with_newlines_renders_as_one_bullet() {
    let repo = Repo::new();
    begin_change(&repo, "flat", None);
    repo.arc(&repo.root)
        .args([
            "keep",
            "flat",
            "--kind",
            "constraint",
            "--body",
            "first line\nsecond line\n## Not A Heading",
        ])
        .assert()
        .success();
    let resume = repo
        .arc(&repo.root)
        .args(["resume", "flat"])
        .assert()
        .success();
    let text = String::from_utf8_lossy(&resume.get_output().stdout).to_string();
    assert!(
        text.contains("first line second line ## Not A Heading"),
        "{text}"
    );
    assert!(!text.contains("\n## Not A Heading"), "{text}");
}

/// A change that never kept anything keeps its serialized shape: `show --json`
/// carries no `kept` member, matching the status report's skip.
#[test]
fn unused_kept_context_is_absent_from_show_json() {
    let repo = Repo::new();
    begin_change(&repo, "bare-json", None);
    let show = json_stdout(repo.arc(&repo.root).args(["show", "bare-json", "--json"]));
    assert!(show.get("kept").is_none(), "{show}");
}
