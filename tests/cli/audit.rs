use super::common::*;

/// The policy fixture every test here needs: self-approval refused, so the
/// audit-debt path is the only way a single actor can ship.
fn repo_forbidding_self_approval() -> Repo {
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

fn self_approved_change(repo: &Repo, slug: &str) -> PathBuf {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Solo")
            .args(["snapshot", slug]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    worktree
}

/// The write path evaluates the same policy `check` does, so an approval that
/// cannot gate says so when it is recorded rather than one command later.
#[test]
fn review_reports_an_approval_that_cannot_gate() {
    let repo = repo_forbidding_self_approval();
    stdout(repo.arc(&repo.root).args(["begin", "inert"]));
    let worktree = repo.home.join(".worktrees").join("repo-inert");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Solo")
            .args(["snapshot", "inert"]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args(["review", "inert", "--verdict", "approved"])
        .assert()
        .success()
        .stdout(predicates::str::contains("does not gate"))
        .stdout(predicates::str::contains("--audit-debt"));
}

/// Declaring the obligation is what unblocks integration. The requirement is
/// carried forward, not waived.
#[test]
fn declared_audit_debt_lets_a_self_approved_change_integrate() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "owed");

    repo.arc(&worktree).args(["check", "owed"]).assert().code(3);

    repo.arc(&worktree)
        .args([
            "audit-debt",
            "owed",
            "--reason",
            "no second actor reachable",
        ])
        .assert()
        .success();
    repo.arc(&worktree).args(["check", "owed"]).assert().code(0);
    repo.arc(&repo.root)
        .args(["integrate", "owed"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "owed", "--json"]));
    assert_eq!(status["audit_debt_outstanding"], true);
    assert_eq!(status["audit_debt"]["reason"], "no second actor reachable");
}

#[test]
fn integrate_declares_the_debt_in_one_step() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "onestep");
    repo.arc(&repo.root)
        .args(["integrate", "onestep", "--audit-debt", "quota exhausted"])
        .assert()
        .success();
    let ids = stdout(repo.arc(&repo.root).args(["query", "--audit-debt"]));
    assert!(ids.contains("onestep"), "{ids}");
}

/// An audit is a distinct event, so it never rewrites what shipped with what
/// review: the pre-closure verdict and the post-integration audit stay apart.
#[test]
fn audit_discharges_the_debt_without_rewriting_the_shipped_verdict() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "audited");
    repo.arc(&repo.root)
        .args(["integrate", "audited", "--audit-debt", "quota exhausted"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args([
            "audit",
            "audited",
            "--verdict",
            "approved",
            "--body",
            "clean",
        ])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "audited", "--json"]));
    assert_eq!(status["audit_debt_outstanding"], false);
    assert_eq!(status["audit_verdicts"][0]["actor"], "Reviewer");
    assert_eq!(status["audit_verdicts"][0]["verdict"], "approved");
    // The shipped verdict is untouched: it is still the author's, on a patchset.
    assert_eq!(status["verdict"]["actor"], "Solo");
    assert!(status["verdict"]["patchset_id"].is_string());

    let remaining = stdout(repo.arc(&repo.root).args(["query", "--audit-debt"]));
    assert!(!remaining.contains("audited"), "{remaining}");
}

/// The audit event is refused before integration, so it cannot be used to
/// pre-empt the review it is meant to follow.
#[test]
fn audit_is_refused_while_the_change_is_open() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "tooearly");
    repo.arc(&repo.root)
        .args(["audit", "tooearly", "--verdict", "approved"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not closed"));
}

#[test]
fn audit_findings_are_recorded_and_kept_out_of_the_shipped_findings() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "withfindings");
    repo.arc(&repo.root)
        .args(["integrate", "withfindings", "--audit-debt", "quota"])
        .assert()
        .success();

    let findings = json_file_bytes(&serde_json::json!([{
        "blocking": true,
        "severity": "major",
        "summary": "missed edge case"
    }]));
    let path = repo.home.join("audit-findings.json");
    fs::write(&path, findings).unwrap();

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args([
            "audit",
            "withfindings",
            "--verdict",
            "changes-requested",
            "--findings-json",
            path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "withfindings", "--json"]),
    );
    assert!(
        status["findings"].as_array().unwrap().is_empty(),
        "audit findings must not join the shipped findings: {}",
        status["findings"]
    );
    let log = stdout(repo.arc(&repo.root).args(["log", "withfindings"]));
    assert!(log.contains("audit-verdict-recorded"), "{log}");
}

/// The failure the review map exists to catch: a genuine independent reviewer
/// participated, approved an early patchset, and never saw what shipped.
/// `non_self_verdict`-style identity comparison reads clean here; coverage
/// does not.
#[test]
fn review_map_names_the_reviewer_that_never_saw_the_final_patchset() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "drifted"]));
    let worktree = repo.home.join(".worktrees").join("repo-drifted");

    repo.commit(&worktree, "a.txt", "a\n", "feat: first");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Author")
            .args(["snapshot", "drifted"]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Reviewer")
        .args(["review", "drifted", "--verdict", "approved"])
        .assert()
        .success();

    // Corrections land after the review, and nobody looks again.
    repo.commit(&worktree, "b.txt", "b\n", "fix: correction");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Author")
            .args(["snapshot", "drifted"]),
    );

    let status = json_stdout(repo.arc(&repo.root).args(["status", "drifted", "--json"]));
    let row = status["review_map"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["reviewer"] == "Reviewer")
        .expect("reviewer row");
    assert_eq!(row["covers_final"], false);
    assert_eq!(row["last_patchset"], "ps-01");

    let warnings = status["coverage_warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("Reviewer last saw ps-01")),
        "{warnings:?}"
    );

    // Advisory only: thin coverage never becomes a blocker.
    let check = json_stdout(repo.arc(&repo.root).args(["check", "drifted", "--json"]));
    assert!(check["coverage_warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("ps-01")));
    let text = stdout(repo.arc(&repo.root).args(["check", "drifted"]));
    assert!(text.contains("Review coverage:"), "{text}");
}

/// A reviewer indistinguishable from the author is reported as unknown
/// attribution, not silently counted as either independent or self-review.
#[test]
fn unattributed_reviewer_is_reported_as_unknown_not_as_independence() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "bare"]));
    let worktree = repo.home.join(".worktrees").join("repo-bare");
    repo.commit(&worktree, "a.txt", "a\n", "feat: work");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Solo")
            .args(["snapshot", "bare"]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args(["review", "bare", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "bare", "--json"]));
    let row = &status["review_map"][0];
    assert_eq!(row["reviewer"], "Solo");
    assert_eq!(row["covers_final"], true);
    assert_eq!(row["is_author"], true);
    assert_eq!(row["attribution_unknown"], true);
    let warnings = status["coverage_warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("distinguishable")),
        "{warnings:?}"
    );
}
