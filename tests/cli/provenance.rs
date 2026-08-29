use crate::common::*;
use predicates::prelude::PredicateBooleanExt;

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
fn ledger_events_record_optional_model_identity_and_render_it_in_log() {
    let repo = Repo::new();
    let output = stdout(repo.arc(&repo.root).args([
        "--model",
        "gpt-5.6-sol#high",
        "begin",
        "model-identity",
        "--no-worktree",
    ]));
    let change_id = opened_change_id(&output);

    repo.arc(&repo.root)
        .args(["comment", &change_id, "--body", "no model declared"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_MODEL", "gpt-5.6-sol#medium")
        .args(["comment", &change_id, "--body", "model from environment"])
        .assert()
        .success();

    let event_values = fs::read_dir(event_dir(&repo, &change_id))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let raw = fs::read_to_string(&path).unwrap();
            let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
            (
                value["event_type"].as_str().unwrap().to_string(),
                raw,
                value,
            )
        })
        .collect::<Vec<_>>();
    let event = |event_type: &str| {
        event_values
            .iter()
            .find(|(kind, _, _)| kind == event_type)
            .unwrap_or_else(|| panic!("missing {event_type} event"))
    };

    let (kind, raw, value) = event("change-opened");
    assert_eq!(kind, "change-opened");
    assert!(raw.contains("\"model\""), "{raw}");
    assert_eq!(value["model"], "gpt-5.6-sol#high", "{value}");

    let comment = |body: &str| {
        event_values
            .iter()
            .find(|(_, _, value)| value["body"] == body)
            .unwrap_or_else(|| panic!("missing comment {body:?}"))
    };
    let (_, raw, value) = comment("no model declared");
    assert!(!raw.contains("\"model\""), "{raw}");
    assert!(value.get("model").is_none(), "{value}");

    let (_, _, value) = comment("model from environment");
    assert_eq!(value["model"], "gpt-5.6-sol#medium", "{value}");

    let comment_count = event_values
        .iter()
        .filter(|(kind, _, _)| kind == "comment-added")
        .count();
    assert_eq!(comment_count, 2);

    let log = stdout(repo.arc(&repo.root).args(["log", &change_id]));
    assert!(log.contains("tester@test (gpt-5.6-sol#high)"), "{log}");
    assert!(
        log.contains("tester@test  comment-added  no model declared"),
        "{log}"
    );
    assert!(log.contains("tester@test (gpt-5.6-sol#medium)"), "{log}");
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
    let (opened, declared) = with_uncommitted_worktree(&repo, || {
        let opened = repo
            .arc(&repo.root)
            .env_remove("ARC_ACTOR")
            .args(["begin", "assumed", "--no-worktree"])
            .output()
            .unwrap();
        // A declared identity records as declared and says nothing.
        let declared = repo
            .arc(&repo.root)
            .args(["begin", "declared", "--no-worktree"])
            .output()
            .unwrap();
        (opened, declared)
    });
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
    with_uncommitted_worktree(&repo, || {
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
    });
}

/// A refusal after the Git work has happened is worse than either answer on
/// its own, so the commands that act before they record check first.
#[test]
fn require_declared_actor_refuses_before_git_work_happens() {
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

    // begin creates a branch and a worktree before it records anything.
    repo.arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .args(["begin", "unnamed"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "policy requires a declared actor",
        ));
    let branches = String::from_utf8(
        std::process::Command::new("git")
            .args(["branch", "--list", "arc/unnamed"])
            .current_dir(&repo.root)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(branches.trim().is_empty(), "{branches}");

    // integrate merges before it records the integration.
    stdout(
        repo.arc(&repo.root)
            .args(["--actor", "author", "begin", "named"]),
    );
    let wt = repo.home.join(".worktrees/repo-named");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    repo.arc(&wt)
        .args(["--actor", "author", "snapshot", "named"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "--actor",
            "reviewer",
            "review",
            "named",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    let before = repo.head(&repo.root);
    repo.arc(&wt)
        .env_remove("ARC_ACTOR")
        .args(["integrate", "named"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "policy requires a declared actor",
        ));
    assert_eq!(repo.head(&repo.root), before);

    // An empty identity is no identity.
    repo.arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .args(["--actor", "", "begin", "blank", "--no-worktree"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "policy requires a declared actor",
        ));
}

/// An audit discharges the review obligation an integration left behind, so an
/// auditor arc named for itself cannot give it. The authoring identity is a
/// different case: it is already on the ledger, and refusing there would make
/// the debt undischargeable rather than making anyone independent.
#[test]
fn an_audit_refuses_an_assumed_auditor() {
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
        .args(["integrate", "owed", "--debt", "no reviewer reachable"])
        .assert()
        .success();

    // An auditor arc named for itself cannot show independence, and can fix
    // that by declaring itself.
    repo.arc(&wt)
        .env_remove("ARC_ACTOR")
        .args(["audit", "owed", "--verdict", "approved"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "arc assumed the auditing identity",
        ));

    // A declared auditor may discharge the debt even though the authoring
    // identity was assumed: that identity is on the ledger and cannot be
    // corrected, and refusing would leave the debt undischargeable forever.
    // What the audit is worth is what its recorded provenance says.
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
        .success()
        // Said out loud, or debt would look like a way around the rule
        // rather than a way of carrying it.
        .stderr(predicates::str::contains(
            "shows that a review happened and not that it was independent",
        ));
    let events = stdout(repo.arc(&wt).args([
        "events",
        "--change",
        "owed",
        "--type",
        "audit-verdict-recorded",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.trim()).unwrap();
    assert_eq!(event["actor_source"], "flag", "{event}");
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

fn claimed_work(repo: &Repo, slug: &str, dangerous: bool) -> (String, PathBuf, String) {
    let mut begin = vec!["begin", slug];
    if dangerous {
        begin.push("--dangerous");
    }
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(begin)));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.arc(&worktree)
        .env("ARC_ACTOR", "codex-luna")
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", "codex-session")
        .args(["claim", slug])
        .assert()
        .success();
    let claim_id = json_stdout(repo.arc(&repo.root).args(["status", slug]))["claim"]["claim_id"]
        .as_str()
        .unwrap()
        .to_string();
    repo.commit(&worktree, "work.txt", "work\n", "feat: claimed work");
    (change_id, worktree, claim_id)
}

#[test]
fn foreign_claim_requires_contributors_and_then_accepts_an_independent_lead_review() {
    let repo = repo_with_self_approval_policy();
    let (change_id, worktree, claim_id) = claimed_work(&repo, "claimed-attribution", true);
    let before = event_count(&repo, &change_id);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", "lead-session")
        .args(["snapshot", "claimed-attribution"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("active claim"))
        .stderr(predicates::str::contains(&claim_id))
        .stderr(predicates::str::contains("--contributors"));
    assert_eq!(event_count(&repo, &change_id), before);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", "lead-session")
        .args([
            "snapshot",
            "claimed-attribution",
            "--contributors",
            "codex-luna",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args(["review", "claimed-attribution", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "claimed-attribution"]));
    assert_eq!(
        status["latest_patchset"]["contributors"],
        serde_json::json!(["codex-luna"]),
        "{status}"
    );
    let row = status["review_map"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["reviewer"] == "claude-lead")
        .unwrap();
    assert_eq!(row["is_author"], false, "{status}");
    assert!(row["matched_contributor"].is_null(), "{status}");
    repo.arc(&repo.root)
        .args(["check", "claimed-attribution"])
        .assert()
        .success();
}

#[test]
fn a_reviewer_matching_a_declared_contributor_is_reported_by_name() {
    let repo = repo_with_self_approval_policy();
    let (_, worktree, _) = claimed_work(&repo, "claimed-self", true);
    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args([
            "snapshot",
            "claimed-self",
            "--contributors",
            "claude-lead,codex-luna",
        ])
        .assert()
        .success();

    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args(["review", "claimed-self", "--verdict", "approved"])
        .assert()
        .success()
        .stdout(predicates::str::contains("claude-lead"));

    let status = json_stdout(repo.arc(&repo.root).args(["status", "claimed-self"]));
    let row = &status["review_map"][0];
    assert_eq!(row["is_author"], true, "{status}");
    assert_eq!(row["matched_contributor"], "claude-lead", "{status}");
    assert!(
        status["approval_rejection_reason"]
            .as_str()
            .unwrap()
            .contains("claude-lead"),
        "{status}"
    );
    repo.arc(&repo.root)
        .args(["check", "claimed-self"])
        .assert()
        .code(3)
        .stdout(predicates::str::contains("claude-lead"));
}

#[test]
fn solo_declares_the_invoker_on_a_foreign_claim() {
    let repo = Repo::new();
    let (_, worktree, _) = claimed_work(&repo, "claimed-solo", false);
    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args(["snapshot", "claimed-solo", "--solo"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "claimed-solo"]));
    assert_eq!(
        status["latest_patchset"]["contributors"],
        serde_json::json!(["claude-lead"]),
        "{status}"
    );
}

#[test]
fn an_unclaimed_snapshot_keeps_the_legacy_invoker_attribution() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "unclaimed-attribution"]),
    );
    let worktree = repo.home.join(".worktrees/repo-unclaimed-attribution");
    repo.commit(&worktree, "work.txt", "work\n", "feat: unclaimed work");
    repo.arc(&worktree)
        .args(["snapshot", "unclaimed-attribution"])
        .assert()
        .success();

    let event = serde_json::from_str::<serde_json::Value>(
        stdout(repo.arc(&repo.root).args([
            "events",
            "--change",
            "unclaimed-attribution",
            "--type",
            "patchset-added",
        ]))
        .trim(),
    )
    .unwrap();
    assert!(event.get("contributors").is_none(), "{event}");
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "unclaimed-attribution"]),
    );
    assert!(
        status["latest_patchset"].get("contributors").is_none(),
        "{status}"
    );
}

/// Names are not comparable across the two namespaces — a contributor is a
/// declared actor and a Git author is whatever a checkout's config holds — so
/// an honest snapshot must say nothing. What is comparable is how many hands
/// the commits carry against how many the declaration names.
#[test]
fn one_declared_contributor_over_one_git_author_says_nothing() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "author-agreement"]));
    let worktree = repo.home.join(".worktrees/repo-author-agreement");
    git(&worktree, &["config", "user.name", "Git Committer"]);
    git(
        &worktree,
        &["config", "user.email", "git-committer@example.invalid"],
    );
    repo.commit(&worktree, "work.txt", "work\n", "feat: one hand");

    repo.arc(&worktree)
        .env("ARC_ACTOR", "declared-contributor")
        .args([
            "snapshot",
            "author-agreement",
            "--contributors",
            "declared-contributor",
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains("warning:").not());
}

/// More hands in the range than the declaration names means somebody who
/// touched the patchset is outside the set that decides who may review it.
#[test]
fn more_git_authors_than_declared_contributors_warns_without_blocking() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "author-disagreement"]));
    let worktree = repo.home.join(".worktrees/repo-author-disagreement");
    git(&worktree, &["config", "user.name", "First Hand"]);
    git(
        &worktree,
        &["config", "user.email", "first@example.invalid"],
    );
    repo.commit(&worktree, "work.txt", "work\n", "feat: first hand");
    git(&worktree, &["config", "user.name", "Second Hand"]);
    git(
        &worktree,
        &["config", "user.email", "second@example.invalid"],
    );
    repo.commit(&worktree, "more.txt", "more\n", "feat: second hand");

    repo.arc(&worktree)
        .env("ARC_ACTOR", "declared-contributor")
        .args([
            "snapshot",
            "author-disagreement",
            "--contributors",
            "declared-contributor",
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains("2 distinct Git authors"))
        .stderr(predicates::str::contains("1 contributor(s) were declared"));
}

#[test]
fn attribution_amendment_is_append_only_and_stops_after_a_verdict() {
    let repo = Repo::new();
    let change_id = opened_change_id(&stdout(
        repo.arc(&repo.root).args(["begin", "amend-attribution"]),
    ));
    let worktree = repo.home.join(".worktrees/repo-amend-attribution");
    repo.commit(&worktree, "work.txt", "work\n", "feat: amend attribution");
    repo.arc(&worktree)
        .args([
            "snapshot",
            "amend-attribution",
            "--contributors",
            "first-contributor",
        ])
        .assert()
        .success();
    let before = event_count(&repo, &change_id);
    repo.arc(&worktree)
        .args([
            "snapshot",
            "amend-attribution",
            "--amend",
            "ps-01",
            "--contributors",
            "corrected-contributor",
        ])
        .assert()
        .success();
    assert_eq!(event_count(&repo, &change_id), before + 1);
    let status = json_stdout(repo.arc(&repo.root).args(["status", "amend-attribution"]));
    assert_eq!(
        status["latest_patchset"]["contributors"],
        serde_json::json!(["corrected-contributor"]),
        "{status}"
    );

    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args(["review", "amend-attribution", "--verdict", "approved"])
        .assert()
        .success();
    let before = event_count(&repo, &change_id);
    repo.arc(&worktree)
        .args([
            "snapshot",
            "amend-attribution",
            "--amend",
            "ps-01",
            "--contributors",
            "late-contributor",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("after verdict"));
    assert_eq!(event_count(&repo, &change_id), before);
}

#[test]
fn a_legacy_patchset_without_contributors_keeps_the_same_gate_decision() {
    let repo = repo_with_self_approval_policy();
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "legacy-contributors",
        "--dangerous",
    ])));
    let worktree = repo.home.join(".worktrees/repo-legacy-contributors");
    repo.commit(&worktree, "work.txt", "work\n", "feat: legacy shape");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "legacy-author")
        .args(["snapshot", "legacy-contributors", "--solo"])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "legacy-author")
        .args(["review", "legacy-contributors", "--verdict", "approved"])
        .assert()
        .success();
    let before = repo
        .arc(&repo.root)
        .args(["check", "legacy-contributors"])
        .output()
        .unwrap();
    assert_eq!(before.status.code(), Some(3), "{before:?}");

    rewrite_event(&repo, &change_id, "patchset-added", |event| {
        event.as_object_mut().unwrap().remove("contributors");
    });
    let after = repo
        .arc(&repo.root)
        .args(["check", "legacy-contributors"])
        .output()
        .unwrap();
    assert_eq!(after.status.code(), Some(3), "{after:?}");
    let status = json_stdout(repo.arc(&repo.root).args(["status", "legacy-contributors"]));
    assert!(
        status["latest_patchset"].get("contributors").is_none(),
        "{status}"
    );
    assert_eq!(
        status["review_map"][0]["matched_contributor"],
        "legacy-author"
    );
}
