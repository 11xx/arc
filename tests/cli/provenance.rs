use crate::common::*;

fn repo_with_self_approval_policy() -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);
    repo
}

#[test]
fn on_behalf_of_round_trips_through_status_json() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "feat-x"]));
    let wt = repo.home.join(".worktrees").join("repo-feat-x");
    repo.commit(&wt, "feat-x.txt", "x\n", "feat: x");
    // A lead snapshots on behalf of an executor who authored the work.
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .args(["snapshot", "feat-x", "--on-behalf-of", "Executor"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "feat-x"]));
    assert_eq!(status["latest_patchset"]["actor"], "Lead");
    assert_eq!(status["latest_patchset"]["on_behalf_of"], "Executor");
}

#[test]
fn lead_snapshot_then_lead_approval_is_not_self_approval() {
    let repo = repo_with_self_approval_policy();
    stdout(repo.arc(&repo.root).args(["begin", "feat-x"]));
    let wt = repo.home.join(".worktrees").join("repo-feat-x");
    repo.commit(&wt, "feat-x.txt", "x\n", "feat: x");
    // Lead snapshots for the executor, then approves as itself: distinct
    // effective authors (Executor vs Lead), so policy permits it.
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .args(["snapshot", "feat-x", "--on-behalf-of", "Executor"])
        .assert()
        .success();
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt).args(["check", "feat-x"]).assert().success();
}

#[test]
fn approval_on_behalf_of_the_snapshot_subject_is_self_approval() {
    let repo = repo_with_self_approval_policy();
    stdout(repo.arc(&repo.root).args(["begin", "feat-x"]));
    let wt = repo.home.join(".worktrees").join("repo-feat-x");
    repo.commit(&wt, "feat-x.txt", "x\n", "feat: x");
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .args(["snapshot", "feat-x", "--on-behalf-of", "Executor"])
        .assert()
        .success();
    // Approving on behalf of the same executor makes both effective authors
    // Executor: that is self-approval and the policy rejects it.
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .args([
            "review",
            "feat-x",
            "--verdict",
            "approved",
            "--on-behalf-of",
            "Executor",
        ])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["check", "feat-x"])
        .assert()
        .code(3)
        .stdout(predicates::str::contains(
            "approval rejected by policy: self-approval",
        ));
}

#[test]
fn claims_match_ownership_by_invoker_not_subject() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "feat-x"]));
    let wt = repo.home.join(".worktrees").join("repo-feat-x");
    // A lead claims on behalf of an executor; ownership is the invoker tuple.
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", "lead-session")
        .args(["claim", "feat-x", "--on-behalf-of", "Executor"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "feat-x"]));
    assert_eq!(status["claim"]["owner"]["actor"], "Lead");
    assert_eq!(status["claim"]["owner"]["session"], "lead-session");

    // The same invoker tuple may release its own claim.
    repo.arc(&wt)
        .env("ARC_ACTOR", "Lead")
        .env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", "lead-session")
        .args(["release-claim", "feat-x"])
        .assert()
        .success();
}

/// An identity nobody claimed is not evidence of who acted, and the ledger is
/// append-only, so the substitution is announced when it happens and recorded
/// as what it is.
#[test]
fn an_assumed_actor_is_announced_and_recorded_as_assumed() {
    let repo = Repo::new();
    let opened = repo
        .arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .args(["begin", "assumed", "--no-worktree"])
        .output()
        .unwrap();
    assert!(opened.status.success());
    let stderr = String::from_utf8_lossy(&opened.stderr);
    assert!(stderr.contains("nobody declared one"), "{stderr}");
    assert!(stderr.contains("--actor"), "{stderr}");

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "assumed",
        "--type",
        "change-opened",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.trim()).unwrap();
    assert_eq!(event["actor_source"], "git-fallback", "{event}");

    // A declared identity records as declared and says nothing.
    let declared = repo
        .arc(&repo.root)
        .args(["begin", "declared", "--no-worktree"])
        .output()
        .unwrap();
    assert!(declared.status.success());
    assert!(
        !String::from_utf8_lossy(&declared.stderr).contains("nobody declared one"),
        "{:?}",
        declared.stderr
    );
    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "declared",
        "--type",
        "change-opened",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.trim()).unwrap();
    assert_eq!(event["actor_source"], "env", "{event}");
}

/// A repository may require every writer to declare itself. Reading is
/// unaffected: it records nothing that could be mistaken for evidence.
#[test]
fn require_declared_actor_refuses_the_git_fallback() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nrequire_declared_actor = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: require a declared actor"],
    );

    repo.arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .args(["begin", "refused", "--no-worktree"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "policy requires a declared actor",
        ));
    let listed = stdout(repo.arc(&repo.root).args(["list"]));
    assert!(!listed.contains("refused"), "{listed}");

    // Reading still works, and declaring an identity is all it takes to write.
    repo.arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .args(["list"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .args(["journal", "list"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .args(["--actor", "someone", "begin", "allowed", "--no-worktree"])
        .assert()
        .success();
    // A delegated subject is somebody's claim, so a lead running ceremony for
    // one satisfies the policy.
    repo.arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .args([
            "--on-behalf-of",
            "executor",
            "begin",
            "delegated",
            "--no-worktree",
        ])
        .assert()
        .success();
}

/// An audit discharges the review obligation an integration left behind, so
/// it answers to the same independence rule the pre-integration guard applies.
#[test]
fn an_audit_refuses_an_assumed_identity() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: forbid self approval"]);
    stdout(repo.arc(&repo.root).args(["begin", "owed"]));
    let wt = repo.home.join(".worktrees/repo-owed");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    repo.arc(&wt)
        .env_remove("ARC_ACTOR")
        .args(["snapshot", "owed"])
        .assert()
        .success();
    repo.arc(&wt)
        .env_remove("ARC_ACTOR")
        .args(["review", "owed", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["integrate", "owed", "--audit-debt", "no reviewer reachable"])
        .assert()
        .success();

    // A differently named auditor does not establish independence when the
    // authoring identity was one arc invented.
    repo.arc(&wt)
        .args([
            "--actor",
            "auditor",
            "audit",
            "owed",
            "--verdict",
            "approved",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "assumed the auditing or the authoring identity",
        ));

    // Raising problems needs no independence.
    repo.arc(&wt)
        .args([
            "--actor",
            "auditor",
            "audit",
            "owed",
            "--verdict",
            "changes-requested",
        ])
        .assert()
        .success();
}

/// A ledger written before arc recorded provenance says nothing about who
/// declared what. Reading that silence as an invention would strand every
/// existing repository that uses the self-approval policy.
#[test]
fn a_ledger_without_provenance_keeps_comparing_names() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: forbid self approval"]);
    stdout(repo.arc(&repo.root).args(["begin", "legacy"]));
    let wt = repo.home.join(".worktrees/repo-legacy");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    repo.arc(&wt)
        .args(["--actor", "author", "snapshot", "legacy"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "--actor",
            "reviewer",
            "review",
            "legacy",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();

    // Strip the provenance arc now records, leaving events shaped like the
    // ones written before it did.
    let changes = repo.root.join(".git/arc/changes");
    for change in fs::read_dir(&changes).unwrap() {
        let events = change.unwrap().path().join("events");
        let Ok(entries) = fs::read_dir(&events) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let mut event: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            event.as_object_mut().unwrap().remove("actor_source");
            fs::write(&path, serde_json::to_string_pretty(&event).unwrap()).unwrap();
        }
    }

    let status = json_stdout(repo.arc(&wt).args(["status", "legacy", "--json"]));
    assert_eq!(status["verdict"]["author_assumed"], false, "{status}");
    assert_eq!(
        status["verdict"]["valid_for_current_head"], true,
        "{status}"
    );
}

/// Two identities nobody declared cannot show that two people acted, so the
/// self-approval guard treats an assumed identity as unproven rather than as a
/// name that happens to differ.
#[test]
fn self_approval_fails_closed_on_an_assumed_identity() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: forbid self approval"]);
    stdout(repo.arc(&repo.root).args(["begin", "unproven"]));
    let wt = repo.home.join(".worktrees/repo-unproven");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");

    // Snapshot with an assumed identity, review with a declared one that
    // happens to differ: independence is still unproven.
    repo.arc(&wt)
        .env_remove("ARC_ACTOR")
        .args(["snapshot", "unproven"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "--actor",
            "someone-else",
            "review",
            "unproven",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "unproven", "--json"]));
    assert_eq!(
        status["verdict"]["valid_for_current_head"], false,
        "{status}"
    );
    assert!(
        status["approval_rejection_reason"]
            .as_str()
            .unwrap()
            .contains("independence is unproven"),
        "{status}"
    );

    // Naming the same author is the more specific fact, so it is the one
    // reported even when the identity was also assumed.
    stdout(repo.arc(&repo.root).args(["begin", "same-author"]));
    let wt = repo.home.join(".worktrees/repo-same-author");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    repo.arc(&wt)
        .env_remove("ARC_ACTOR")
        .args(["snapshot", "same-author"])
        .assert()
        .success();
    repo.arc(&wt)
        .env_remove("ARC_ACTOR")
        .args(["review", "same-author", "--verdict", "approved"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "same-author", "--json"]));
    assert!(
        status["approval_rejection_reason"]
            .as_str()
            .unwrap()
            .contains("self-approval"),
        "{status}"
    );
}
