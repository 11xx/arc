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

/// A closed change can retain later patchsets in its history, but the review
/// subject must stay bound to the patchset the closure says shipped.
#[test]
fn closed_status_review_subject_names_the_shipped_patchset() {
    let repo = Repo::new();
    let slug = "closed-subject";
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));

    repo.commit(&worktree, "first.txt", "first\n", "feat: first");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Lead")
        .args(["snapshot", slug, "--on-behalf-of", "executor-a"])
        .assert()
        .success();
    let shipped_head = repo.head(&worktree);

    repo.commit(&worktree, "second.txt", "second\n", "feat: second");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Lead")
        .args(["snapshot", slug, "--on-behalf-of", "executor-b"])
        .assert()
        .success();

    git(&repo.root, &["merge", "--ff-only", &shipped_head]);
    repo.arc(&repo.root)
        .args([
            "close",
            slug,
            "--assert-integrated",
            &shipped_head,
            "--patchset",
            "ps-01",
            "--into",
            "master",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["debt", slug, "--reason", "review after integration"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", slug]));
    assert_eq!(status["latest_patchset"]["id"], "ps-02", "{status}");
    assert_eq!(status["review_subject"]["patchset_id"], "ps-01", "{status}");
    assert_eq!(
        status["review_subject"]["effective_author"], "executor-a",
        "{status}"
    );
    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(
        catchup.contains(
            "review subject: `ps-01` compares reviewer against contributors [executor-a]"
        ),
        "{catchup}"
    );
    assert!(
        !catchup.contains(
            "review subject: `ps-02` compares reviewer against contributors [executor-b]"
        ),
        "{catchup}"
    );
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
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_SESSION")
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

/// A known harness session names the acting agent better than the checkout's
/// Git identity, which is kept as the operator. The derived actor is still
/// nobody's claim, so it is announced like any other assumed identity.
#[test]
fn an_undeclared_actor_with_a_known_session_is_derived_from_it() {
    let repo = Repo::new();
    let opened = with_uncommitted_worktree(&repo, || {
        repo.arc(&repo.root)
            .env_remove("ARC_ACTOR")
            .args(["begin", "derived", "--no-worktree"])
            .output()
            .unwrap()
    });
    assert!(opened.status.success());
    let stderr = String::from_utf8_lossy(&opened.stderr);
    assert!(
        stderr.contains(
            "recording actor \"test:session-a\" from the harness session; nobody declared one"
        ),
        "{stderr}"
    );

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "derived",
        "--type",
        "change-opened",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.trim()).unwrap();
    assert_eq!(event["actor"], "test:session-a", "{event}");
    assert_eq!(event["actor_source"], "derived", "{event}");
    assert_eq!(event["operator"], "Tester", "{event}");
}

/// An identity nobody declared is derived from detection, so what detection
/// claimed about the session is part of the record. A session id the harness's
/// own store does not hold is marked as such rather than left to be inferred
/// from an absent model; a session the caller declared is never looked up and
/// makes no claim.
#[test]
fn an_event_records_whether_the_harness_store_backed_the_session() {
    let repo = Repo::new();
    enable_identity_detection(&repo);
    let session = "55555555-6666-7777-8888-999999999999";

    let opened = |slug: &str| {
        assert!(repo
            .arc(&repo.root)
            .env_remove("ARC_ACTOR")
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_SESSION")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("OPENCODE_SESSION")
            .env_remove("PI_SESSION_ID")
            .env("CODEX_THREAD_ID", session)
            .args(["begin", slug])
            .output()
            .unwrap()
            .status
            .success());
        let events = stdout(repo.arc(&repo.root).args([
            "events",
            "--change",
            slug,
            "--type",
            "change-opened",
        ]));
        serde_json::from_str::<serde_json::Value>(events.trim()).unwrap()
    };

    let event = opened("store-less-session");
    assert_eq!(event["session"], session, "{event}");
    assert_eq!(event["session_resolution"], "uncorroborated", "{event}");

    let day = repo.home.join(".codex/sessions/2026/09/18");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join(format!("rollout-2026-09-18T00-00-00-{session}.jsonl")),
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-sol\"}}\n",
    )
    .unwrap();
    let event = opened("store-backed-session");
    assert_eq!(event["session_resolution"], "corroborated", "{event}");

    // The harness and session come from the declared environment here, so
    // detection never ran and nothing was asked of any store.
    assert!(repo
        .arc(&repo.root)
        .args(["begin", "declared-session"])
        .output()
        .unwrap()
        .status
        .success());
    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "declared-session",
        "--type",
        "change-opened",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.trim()).unwrap();
    assert_eq!(event["session"], "session-a", "{event}");
    assert!(
        event.get("session_resolution").is_none(),
        "a declared session's events carry no store report: {event}"
    );
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

/// A declared reviewer is a claim somebody made, and it stands against an
/// authoring identity arc invented: the reviewer said it is somebody else.
/// Every independence surface reads that the same way, so the approval carries
/// the merge and an audit of the shipped work accepts the same reviewer.
#[test]
fn a_declared_reviewer_is_independent_of_an_assumed_author() {
    let repo = repo_with_self_approval_policy();
    stdout(repo.arc(&repo.root).args(["begin", "assumed-author"]));
    let wt = repo.home.join(".worktrees/repo-assumed-author");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    repo.arc(&wt)
        .env_remove("ARC_ACTOR")
        .args(["snapshot", "assumed-author"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "--actor",
            "someone-else",
            "review",
            "assumed-author",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "assumed-author", "--json"]));
    assert_eq!(
        status["verdict"]["valid_for_current_head"], true,
        "{status}"
    );
    assert!(status["approval_rejection_reason"].is_null(), "{status}");
    let row = &status["review_map"][0];
    assert_eq!(row["reviewer"], "someone-else", "{status}");
    assert_eq!(row["is_author"], false, "{status}");
    assert_eq!(row["attribution_unknown"], false, "{status}");

    repo.arc(&repo.root)
        .args(["integrate", "assumed-author"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "--actor",
            "someone-else",
            "audit",
            "assumed-author",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
}

/// An identity arc invented from git configuration names nobody in
/// particular, so it cannot be the second party independence needs. Coverage
/// reports it as attribution nobody can place rather than as a review by
/// somebody else.
#[test]
fn an_assumed_reviewer_is_neither_independent_nor_self_review() {
    let repo = repo_with_self_approval_policy();
    stdout(repo.arc(&repo.root).args(["begin", "assumed-reviewer"]));
    let wt = repo.home.join(".worktrees/repo-assumed-reviewer");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    repo.arc(&wt)
        .args(["--actor", "author", "snapshot", "assumed-reviewer"])
        .assert()
        .success();
    repo.arc(&wt)
        .env_remove("ARC_ACTOR")
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION")
        .args(["review", "assumed-reviewer", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "assumed-reviewer", "--json"]));
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
    let row = &status["review_map"][0];
    assert_eq!(row["reviewer"], "Tester", "{status}");
    assert_eq!(row["is_author"], false, "{status}");
    assert_eq!(row["attribution_unknown"], true, "{status}");
    let advisories = status["advisories"].as_array().unwrap();
    assert!(
        advisories
            .iter()
            .any(|advisory| advisory["code"] == "reviewer-attribution-unknown"),
        "{advisories:?}"
    );
}

/// A derived actor names a real harness session, but arc inferred it, so it is
/// no more a declared second party than a name taken from git configuration.
#[test]
fn a_derived_reviewer_is_not_independent() {
    let repo = repo_with_self_approval_policy();
    stdout(repo.arc(&repo.root).args(["begin", "derived-reviewer"]));
    let wt = repo.home.join(".worktrees/repo-derived-reviewer");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    repo.arc(&wt)
        .args(["--actor", "author", "snapshot", "derived-reviewer"])
        .assert()
        .success();
    repo.arc(&wt)
        .env_remove("ARC_ACTOR")
        .args(["review", "derived-reviewer", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "derived-reviewer", "--json"]));
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
    assert_eq!(
        status["review_map"][0]["reviewer"], "test:session-a",
        "{status}"
    );
}

/// Two identities nobody declared cannot show that two people acted.
#[test]
fn self_approval_fails_closed_on_an_assumed_identity() {
    let repo = repo_with_self_approval_policy();
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
fn done_over_a_foreign_claim_accepts_the_attribution_its_refusal_names() {
    let repo = Repo::new();
    let (change_id, worktree, claim_id) = claimed_work(&repo, "claimed-done", false);
    let before = event_count(&repo, &change_id);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args(["done", "claimed-done"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(&claim_id))
        .stderr(predicates::str::contains("--contributors or --solo"))
        .stderr(predicates::str::contains("`arc done`"));
    assert_eq!(event_count(&repo, &change_id), before);

    let output = repo
        .arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args(["done", "claimed-done", "--contributors", "codex-luna"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("patchset: ps-01"), "{stdout}\n{stderr}");
    assert!(!stderr.contains("active claim"), "{stderr}");
    let status = json_stdout(repo.arc(&repo.root).args(["status", "claimed-done"]));
    assert_eq!(
        status["latest_patchset"]["contributors"],
        serde_json::json!(["codex-luna"]),
        "{status}"
    );
}

/// `done` moves its caller's claim to `verifying` only after the attribution
/// it was given is accepted, so a declaration it refuses changes nothing.
#[test]
fn done_refuses_blank_contributors_before_moving_the_claim_stage() {
    let repo = Repo::new();
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "blank-done"])));
    let worktree = repo.home.join(".worktrees").join("repo-blank-done");
    repo.arc(&worktree)
        .args(["claim", "blank-done"])
        .assert()
        .success();
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    let observed = || {
        let status = json_stdout(
            repo.arc(&repo.root)
                .args(["status", "blank-done", "--json"]),
        );
        let claim = &status["claim"];
        (
            event_count(&repo, &change_id),
            claim["stage"].clone(),
            claim["stage_started_at"].clone(),
        )
    };
    let before = observed();
    assert_eq!(before.1, "launch", "{before:?}");

    repo.arc(&worktree)
        .args(["done", "blank-done", "--contributors", " "])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--contributors must name nonempty actors",
        ));
    assert_eq!(observed(), before);
}

#[test]
fn review_snapshot_over_a_foreign_claim_accepts_the_attribution_its_refusal_names() {
    let repo = Repo::new();
    let (change_id, worktree, claim_id) = claimed_work(&repo, "claimed-review", false);
    let before = event_count(&repo, &change_id);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args([
            "review",
            "claimed-review",
            "--snapshot",
            "--verdict",
            "approved",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(&claim_id))
        .stderr(predicates::str::contains("`arc review --snapshot`"));
    assert_eq!(event_count(&repo, &change_id), before);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args([
            "review",
            "claimed-review",
            "--snapshot",
            "--contributors",
            "codex-luna",
            "--verdict",
            "approved",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("patchset: ps-01"));
    let status = json_stdout(repo.arc(&repo.root).args(["status", "claimed-review"]));
    assert_eq!(
        status["latest_patchset"]["contributors"],
        serde_json::json!(["codex-luna"]),
        "{status}"
    );
    assert_eq!(status["has_valid_approval"], true, "{status}");
}

#[test]
fn review_attribution_without_snapshot_is_refused_before_anything_is_recorded() {
    let repo = Repo::new();
    let (change_id, worktree, _) = claimed_work(&repo, "claimed-unsnapped", false);
    let before = event_count(&repo, &change_id);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args([
            "review",
            "claimed-unsnapped",
            "--solo",
            "--verdict",
            "approved",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--snapshot"));
    assert_eq!(event_count(&repo, &change_id), before);
}

#[test]
fn squash_over_a_foreign_claim_refuses_before_the_branch_moves() {
    let repo = Repo::new();
    let (change_id, worktree, claim_id) = claimed_work(&repo, "claimed-squash", false);
    repo.commit(&worktree, "more.txt", "more\n", "feat: more claimed work");
    let head = git_out(&worktree, &["rev-parse", "HEAD"]);
    let before = event_count(&repo, &change_id);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args(["squash", "claimed-squash", "-m", "feat: claimed work"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(&claim_id))
        .stderr(predicates::str::contains("`arc squash`"));
    assert_eq!(git_out(&worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(event_count(&repo, &change_id), before);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args([
            "squash",
            "claimed-squash",
            "-m",
            "feat: claimed work",
            "--contributors",
            "codex-luna",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("patchset: ps-01"));
    assert_eq!(
        git_out(&worktree, &["rev-list", "--count", "master..HEAD"]).trim(),
        "1"
    );
    let status = json_stdout(repo.arc(&repo.root).args(["status", "claimed-squash"]));
    assert_eq!(
        status["latest_patchset"]["contributors"],
        serde_json::json!(["codex-luna"]),
        "{status}"
    );
}

/// The single commit can stay open for as long as a signing prompt or a hook
/// takes. A claim attempted in that window cannot land between the squash's
/// claim check and the patchset it records, so a squash either records the
/// commit it made or leaves the branch where it was.
#[test]
fn a_claim_attempted_while_squash_commits_cannot_strand_the_rewritten_branch() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "raced-squash"]));
    let worktree = repo.home.join(".worktrees").join("repo-raced-squash");
    repo.commit(&worktree, "one.txt", "one\n", "feat: one");
    repo.commit(&worktree, "two.txt", "two\n", "feat: two");
    let head = repo.head(&worktree);
    let outcome = repo.home.join("hook-claim");
    let hook = repo.root.join(".git/hooks/pre-commit");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\n\
             if env -u GIT_INDEX_FILE ARC_ACTOR=codex-luna ARC_HARNESS=codex ARC_SESSION=codex-session \
             '{arc}' claim raced-squash >/dev/null 2>&1\n\
             then echo taken > '{outcome}'\n\
             else echo refused > '{outcome}'\n\
             fi\n\
             exit 0\n",
            arc = env!("CARGO_BIN_EXE_arc"),
            outcome = outcome.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();

    let out = repo
        .arc(&worktree)
        .args(["squash", "raced-squash", "-m", "feat: one and two"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        assert_eq!(
            repo.head(&worktree),
            head,
            "a refused squash moved the branch: {stderr}"
        );
    }
    assert!(out.status.success(), "{stderr}");
    assert_eq!(fs::read_to_string(&outcome).unwrap().trim(), "refused");
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "raced-squash", "--json"]),
    );
    assert_eq!(
        status["latest_patchset"]["head"],
        repo.head(&worktree).as_str(),
        "{status}"
    );
    assert!(status["claim"].is_null(), "{status}");
}

#[test]
fn rebase_over_a_foreign_claim_refuses_before_the_branch_moves() {
    let repo = Repo::new();
    let (change_id, worktree, claim_id) = claimed_work(&repo, "claimed-rebase", false);
    repo.commit(&repo.root, "README.md", "target\n", "feat: move target");
    let target = git_out(&repo.root, &["rev-parse", "HEAD"]);
    let head = git_out(&worktree, &["rev-parse", "HEAD"]);
    let before = event_count(&repo, &change_id);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args(["rebase", "claimed-rebase"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(&claim_id))
        .stderr(predicates::str::contains("`arc rebase`"));
    assert_eq!(git_out(&worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(event_count(&repo, &change_id), before);

    repo.arc(&worktree)
        .env("ARC_ACTOR", "claude-lead")
        .args(["rebase", "claimed-rebase", "--contributors", "codex-luna"])
        .assert()
        .success()
        .stdout(predicates::str::contains("patchset: ps-01"));
    let replayed = git_out(&worktree, &["rev-parse", "HEAD"]);
    assert_ne!(replayed, head);
    assert_eq!(git_out(&worktree, &["rev-parse", "HEAD~1"]), target);
    let status = json_stdout(repo.arc(&repo.root).args(["status", "claimed-rebase"]));
    assert_eq!(status["latest_patchset"]["head"], replayed, "{status}");
    assert_eq!(
        status["latest_patchset"]["contributors"],
        serde_json::json!(["codex-luna"]),
        "{status}"
    );
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

/// The advisory codes a status report carries, which are absent altogether
/// when there are none.
fn advisory_codes(status: &serde_json::Value) -> Vec<String> {
    status["advisories"]
        .as_array()
        .map(|advisories| {
            advisories
                .iter()
                .filter_map(|advisory| advisory["code"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A reviewer known only from findings is placed by the same rule as one that
/// cast a verdict: an identity arc took from git configuration is a name
/// nobody claimed, so coverage reports it as unplaceable and the change still
/// wants an independent reader.
#[test]
fn a_findings_only_reviewer_with_an_assumed_identity_is_unplaceable() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "assumed-finder"]));
    let wt = repo.home.join(".worktrees/repo-assumed-finder");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    repo.arc(&wt)
        .args(["--actor", "author", "snapshot", "assumed-finder"])
        .assert()
        .success();
    repo.arc(&wt)
        .env_remove("ARC_ACTOR")
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION")
        .args(["finding", "assumed-finder", "--summary", "a defect"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "assumed-finder", "--json"]));
    let row = &status["review_map"][0];
    assert_eq!(row["reviewer"], "Tester", "{status}");
    assert_eq!(row["findings"], 1, "{status}");
    assert_eq!(row["verdicts"], 0, "{status}");
    assert_eq!(row["is_author"], false, "{status}");
    assert_eq!(row["attribution_unknown"], true, "{status}");
    assert!(
        advisory_codes(&status).contains(&"reviewer-attribution-unknown".to_string()),
        "{status}"
    );
}

/// The invariant the rule above is bounded by, asserted structurally rather
/// than as a regression: a declared identity is placeable whether it filed a
/// finding or cast a verdict, and it is the second party independence needs.
#[test]
fn a_findings_only_reviewer_that_declared_itself_is_placeable() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "declared-finder"]));
    let wt = repo.home.join(".worktrees/repo-declared-finder");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    repo.arc(&wt)
        .args(["--actor", "author", "snapshot", "declared-finder"])
        .assert()
        .success();
    repo.arc(&wt)
        .env("ARC_ACTOR", "reviewer")
        .args(["finding", "declared-finder", "--summary", "a defect"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&wt).args(["status", "declared-finder", "--json"]));
    let row = &status["review_map"][0];
    assert_eq!(row["reviewer"], "reviewer", "{status}");
    assert_eq!(row["findings"], 1, "{status}");
    assert_eq!(row["is_author"], false, "{status}");
    assert_eq!(row["attribution_unknown"], false, "{status}");
    assert!(
        !advisory_codes(&status).contains(&"reviewer-attribution-unknown".to_string()),
        "{status}"
    );
}

/// The review subject: the exact identities the independence check compares,
/// projected from the same Patchset methods the check calls. Driven through
/// the CLI for explicit executor-only, explicit executor-plus-lead, legacy
/// on-behalf-of, and legacy invoker patchsets.
#[test]
fn status_names_the_review_subject_for_every_attribution_shape() {
    let policy = |repo: &Repo| {
        fs::create_dir_all(repo.root.join(".arc")).unwrap();
        fs::write(
            repo.root.join(".arc/policy.toml"),
            "[policy]\nforbid_self_approval = true\n",
        )
        .unwrap();
        git(&repo.root, &["add", ".arc/policy.toml"]);
        git(&repo.root, &["commit", "-m", "policy"]);
    };
    let snap = |repo: &Repo, slug: &str, args: &[&str]| {
        let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
        repo.commit(
            &wt,
            &format!("{slug}.txt"),
            "work\n",
            &format!("feat: {slug}"),
        );
        let mut cmd = repo.arc(&wt);
        cmd.args(["snapshot", slug]);
        cmd.args(args);
        cmd.assert().success();
        wt
    };

    // Explicit executor-only: a lead snapshots naming only the executor.
    let repo = Repo::new();
    policy(&repo);
    stdout(repo.arc(&repo.root).args(["begin", "executor-only"]));
    snap(
        &repo,
        "executor-only",
        &[
            "--contributors",
            "executor-a",
            "--on-behalf-of",
            "executor-a",
        ],
    );
    let status = json_stdout(repo.arc(&repo.root).args(["status", "executor-only"]));
    let subject = &status["review_subject"];
    assert_eq!(subject["effective_author"], "executor-a", "{subject}");
    assert_eq!(subject["contributors"], serde_json::json!(["executor-a"]));
    assert_eq!(subject["basis"], "explicit-contributors");
    assert_eq!(subject["invoker"], "tester");
    // The patchset's own recorded fields are untouched.
    assert_eq!(
        status["latest_patchset"]["contributors"],
        serde_json::json!(["executor-a"])
    );
    // The executor-only lead approval passes the independence gate.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Lead")
        .args(["review", "executor-only", "--verdict", "approved"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "executor-only"]));
    assert!(status["approval_rejection_reason"].is_null(), "{status}");

    // Explicit executor-plus-lead: the lead is in the set, so a dangerous
    // change rejects the lead's own approval and names the comparison. The
    // escalation makes the change dangerous without a declared paths table.
    let repo = Repo::new();
    policy(&repo);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "executor-lead", "--dangerous"]),
    );
    // Repeatable flags: each names one contributor.
    snap(
        &repo,
        "executor-lead",
        &["--contributors", "executor-a", "--contributors", "Lead"],
    );
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Lead")
        .args(["review", "executor-lead", "--verdict", "approved"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "executor-lead"]));
    assert_eq!(status["review_subject"]["basis"], "explicit-contributors");
    // One flag value names both contributors, space-separated in the CLI
    // surface, so the effective set is the two of them.
    // The set is the two contributors, sorted by the declaration normalizer.
    assert_eq!(
        status["review_subject"]["contributors"],
        serde_json::json!(["Lead", "executor-a"])
    );
    let reason = status["approval_rejection_reason"].as_str().unwrap();
    assert!(reason.contains("Lead"), "{reason}");
    // The text projection names the same identities beside the rejection.
    let show = stdout(repo.arc(&repo.root).args(["show", "executor-lead"]));
    assert!(show.contains("review subject:"), "{show}");
    assert!(show.contains("Lead, executor-a"), "{show}");
    assert!(show.contains("explicit-contributors"), "{show}");
    let check = stdout_any_status(repo.arc(&repo.root).args(["check", "executor-lead"]));
    assert!(check.contains("review subject:"), "{check}");

    // Legacy on-behalf-of: no contributor set, so the subject is the
    // synthesized fallback of the effective author.
    let repo = Repo::new();
    policy(&repo);
    stdout(repo.arc(&repo.root).args(["begin", "legacy-obo"]));
    snap(&repo, "legacy-obo", &["--on-behalf-of", "Executor"]);
    let status = json_stdout(repo.arc(&repo.root).args(["status", "legacy-obo"]));
    let subject = &status["review_subject"];
    assert_eq!(subject["effective_author"], "Executor", "{subject}");
    assert_eq!(subject["contributors"], serde_json::json!(["Executor"]));
    assert_eq!(subject["basis"], "effective-author");

    // Legacy invoker: no set, no subject; the invoker is both.
    let repo = Repo::new();
    policy(&repo);
    stdout(repo.arc(&repo.root).args(["begin", "legacy-invoker"]));
    snap(&repo, "legacy-invoker", &[]);
    let status = json_stdout(repo.arc(&repo.root).args(["status", "legacy-invoker"]));
    let subject = &status["review_subject"];
    assert_eq!(subject["effective_author"], "tester", "{subject}");
    assert_eq!(subject["contributors"], serde_json::json!(["tester"]));
    assert_eq!(subject["basis"], "effective-author");

    // An unsnapshotted change has no author set to report.
    let repo = Repo::new();
    policy(&repo);
    stdout(repo.arc(&repo.root).args(["begin", "unsnapshotted"]));
    let status = json_stdout(repo.arc(&repo.root).args(["status", "unsnapshotted"]));
    assert!(status.get("review_subject").is_none(), "{status}");
}

/// The safe repair path, driven through the CLI with one shared Git identity
/// and distinct declared actors: capture executor work with an explicit
/// contributor set, inspect the subject, amend the full set before any
/// verdict when needed, then have an actually independent identity review.
/// After a verdict or a terminal closure the amendment refuses with the
/// blocker named, changing neither ledger length nor contributor fields.
#[test]
fn attribution_amendment_is_repairable_only_before_a_verdict() {
    let policy = |repo: &Repo| {
        fs::create_dir_all(repo.root.join(".arc")).unwrap();
        fs::write(
            repo.root.join(".arc/policy.toml"),
            "[policy]\nforbid_self_approval = true\n",
        )
        .unwrap();
        git(&repo.root, &["add", ".arc/policy.toml"]);
        git(&repo.root, &["commit", "-m", "policy"]);
    };

    // Before any verdict: amendment appends exactly one event and replaces
    // the whole effective set.
    let repo = Repo::new();
    policy(&repo);
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "repairable"])));
    let worktree = repo.home.join(".worktrees").join("repo-repairable");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Lead")
        .args(["snapshot", "repairable", "--contributors", "executor-a"])
        .assert()
        .success();
    let before = event_count(&repo, &change_id);
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Lead")
        .args([
            "snapshot",
            "repairable",
            "--amend",
            "ps-01",
            "--contributors",
            "executor-a",
            "--contributors",
            "executor-b",
        ])
        .assert()
        .success();
    assert_eq!(event_count(&repo, &change_id), before + 1);
    let status = json_stdout(repo.arc(&repo.root).args(["status", "repairable"]));
    // The explicit set replaces the whole set rather than adding one member.
    assert_eq!(
        status["review_subject"]["contributors"],
        serde_json::json!(["executor-a", "executor-b"]),
        "{status}"
    );
    // The lead's own repair commits require the lead in the declared full
    // set: reviewing from an identity outside it still passes, because the
    // work itself is attributed to the executors.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args(["review", "repairable", "--verdict", "approved"])
        .assert()
        .success();

    // After the verdict: refusal names the verdict and changes nothing.
    let before = event_count(&repo, &change_id);
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Lead")
        .args([
            "snapshot",
            "repairable",
            "--amend",
            "ps-01",
            "--contributors",
            "executor-a",
        ])
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("cannot be amended after verdict")
                .and(predicates::str::contains("Reviewer")),
        );
    assert_eq!(event_count(&repo, &change_id), before);
    let status = json_stdout(repo.arc(&repo.root).args(["status", "repairable"]));
    assert_eq!(
        status["review_subject"]["contributors"],
        serde_json::json!(["executor-a", "executor-b"]),
        "refused amendment changed the set: {status}"
    );

    // After a terminal closure: refusal names the closure.
    let repo = Repo::new();
    policy(&repo);
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "closed-case"])));
    let worktree = repo.home.join(".worktrees").join("repo-closed-case");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Executor")
        .args(["snapshot", "closed-case"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["close", "closed-case", "--abandoned"])
        .assert()
        .success();
    let before = event_count(&repo, &change_id);
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Executor")
        .args([
            "snapshot",
            "closed-case",
            "--amend",
            "ps-01",
            "--contributors",
            "executor-a",
        ])
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("cannot be amended after the change is abandoned")
                .and(predicates::str::contains("debt")),
        );
    assert_eq!(event_count(&repo, &change_id), before);

    // Existing unknown-patchset and missing-contributors refusals stay.
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Executor")
        .args([
            "snapshot",
            "closed-case",
            "--amend",
            "ps-99",
            "--contributors",
            "executor-a",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown patchset"));
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Executor")
        .args(["snapshot", "closed-case", "--amend", "ps-01"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--contributors"));

    // A dispatch recorded after the snapshot changes no status subject.
    let repo = Repo::new();
    policy(&repo);
    stdout(repo.arc(&repo.root).args(["begin", "late-dispatch"]));
    let worktree = repo.home.join(".worktrees").join("repo-late-dispatch");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Executor")
        .args(["snapshot", "late-dispatch", "--contributors", "executor-a"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "run",
            "dispatch",
            "--route",
            "implement",
            "--worktree",
            worktree.to_str().unwrap(),
            "--change",
            "late-dispatch",
        ])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "late-dispatch"]));
    assert_eq!(
        status["review_subject"]["basis"], "explicit-contributors",
        "{status}"
    );
    assert_eq!(
        status["review_subject"]["contributors"],
        serde_json::json!(["executor-a"]),
        "a late dispatch rewrote the subject: {status}"
    );
    // The subject is unchanged: a dispatch proves an invocation, not
    // authorship, so recording one after the snapshot rewrites nothing.
}
