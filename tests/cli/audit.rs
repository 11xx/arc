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

/// Selection errors must be found before a policy-bearing waiver is written.
/// A refused command that leaves the self-approval gate open is worse than a
/// partial merge because its side effect is easy to miss.
#[test]
fn invalid_integrate_selection_does_not_declare_audit_debt() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "bad-selection");

    repo.arc(&repo.root)
        .args([
            "integrate",
            "bad-selection",
            "--tag",
            "#bad-selection",
            "--audit-debt",
            "must not persist",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("provide a change or --tag"));

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "bad-selection", "--json"]),
    );
    assert!(status["audit_debt"].is_null(), "{}", status["audit_debt"]);
    repo.arc(&repo.root)
        .args(["check", "bad-selection"])
        .assert()
        .code(3);
}

#[test]
fn execution_roles_protect_audit_waivers_and_verdicts() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "role-boundary");

    repo.arc(&worktree)
        .env("ARC_ROLE", "implementer")
        .args([
            "audit-debt",
            "role-boundary",
            "--reason",
            "implementer waiver",
        ])
        .assert()
        .code(9);
    repo.arc(&worktree)
        .env("ARC_ROLE", "reviewer")
        .args(["audit-debt", "role-boundary", "--reason", "reviewer waiver"])
        .assert()
        .code(9);

    repo.arc(&repo.root)
        .args(["integrate", "role-boundary", "--audit-debt", "lead waiver"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ROLE", "implementer")
        .env("ARC_ACTOR", "Reviewer")
        .args(["audit", "role-boundary", "--verdict", "approved"])
        .assert()
        .code(9);

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "role-boundary", "--json"]),
    );
    assert_eq!(status["audit_debt_outstanding"], true);
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

#[test]
fn review_map_attributes_findings_to_their_recorded_subject() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "attributed-findings"]));
    let worktree = repo
        .home
        .join(".worktrees")
        .join("repo-attributed-findings");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Author")
            .args(["snapshot", "attributed-findings"]),
    );
    let path = repo.home.join("attributed-findings.json");
    fs::write(
        &path,
        json_file_bytes(&serde_json::json!([{
            "blocking": false,
            "severity": "minor",
            "summary": "inline observation"
        }])),
    )
    .unwrap();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Lead")
        .args([
            "review",
            "attributed-findings",
            "--on-behalf-of",
            "Reviewer",
            "--verdict",
            "approved",
            "--findings-json",
            path.to_str().unwrap(),
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Lead")
        .args([
            "finding",
            "attributed-findings",
            "--on-behalf-of",
            "Reviewer",
            "--summary",
            "standalone observation",
            "--severity",
            "minor",
        ])
        .assert()
        .success();

    let status =
        json_stdout(
            repo.arc(&repo.root)
                .args(["status", "attributed-findings", "--json"]),
        );
    let rows = status["review_map"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["reviewer"], "Reviewer");
    assert_eq!(rows[0]["verdicts"], 1);
    assert_eq!(rows[0]["findings"], 2);
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

/// `--dry-run` promises to write nothing, and a debt declaration is a write.
#[test]
fn dry_run_integrate_declares_no_debt() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "dry");
    // Blocked, because the debt that would have unblocked it was not written.
    repo.arc(&repo.root)
        .args(["integrate", "dry", "--dry-run", "--audit-debt", "quota"])
        .assert()
        .code(3);
    let status = json_stdout(repo.arc(&repo.root).args(["status", "dry", "--json"]));
    assert_eq!(status["audit_debt_outstanding"], false);
    assert!(status["audit_debt"].is_null(), "{}", status["audit_debt"]);
}

/// The waiver expires the way an approval expires.
///
/// A debt declared for one patchset must not excuse a self-approval on the
/// next one; otherwise a single declaration disables the policy for the rest
/// of the change's life, and nothing about the second integration looks wrong.
#[test]
fn audit_debt_stops_waiving_once_a_new_patchset_lands() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "expiring");
    repo.arc(&worktree)
        .args(["audit-debt", "expiring", "--reason", "no reviewer"])
        .assert()
        .success()
        .stdout(predicates::str::contains("declared for ps-01"));
    repo.arc(&worktree)
        .args(["check", "expiring"])
        .assert()
        .code(0);

    // New work lands and is self-approved again. The old waiver is spent.
    repo.commit(&worktree, "more.txt", "more\n", "feat: more");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Solo")
            .args(["snapshot", "expiring"]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args(["review", "expiring", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["check", "expiring"])
        .assert()
        .code(3)
        .stdout(predicates::str::contains("self-approval"));

    // Re-declaring against the new patchset is a deliberate act, and works.
    repo.arc(&worktree)
        .args(["audit-debt", "expiring", "--reason", "still no reviewer"])
        .assert()
        .success()
        .stdout(predicates::str::contains("declared for ps-02"));
    repo.arc(&worktree)
        .args(["check", "expiring"])
        .assert()
        .code(0);
}

/// A debt discovered after integration records the obligation without
/// retroactively waiving anything.
#[test]
fn debt_declared_after_integration_carries_no_patchset() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "afterwards"]));
    complete_change(&repo, "afterwards");
    repo.arc(&repo.root)
        .args(["audit-debt", "afterwards", "--reason", "found later"])
        .assert()
        .success();
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "afterwards", "--json"]),
    );
    assert_eq!(status["audit_debt_outstanding"], true);
    assert!(
        status["audit_debt"]["patchset_id"].is_null(),
        "{}",
        status["audit_debt"]
    );
}

/// The queue a session returns to when a reviewer becomes available. The
/// obligation belongs to an integrated change, so it must survive the closure
/// filter every other bucket applies.
#[test]
fn outstanding_debt_appears_in_the_inbox_and_catchup_after_closure() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "queued");
    repo.arc(&repo.root)
        .args(["integrate", "queued", "--audit-debt", "quota exhausted"])
        .assert()
        .success();

    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    assert_eq!(inbox["schema"], "arc-inbox/3");
    let owed = inbox["audit-owed"].as_array().unwrap();
    assert_eq!(owed.len(), 1, "{owed:?}");
    assert_eq!(owed[0]["next_actor"], "reviewer");

    let text = stdout(repo.arc(&repo.root).args(["inbox"]));
    assert!(text.contains("## audit-owed"), "{text}");

    let filtered = json_stdout(repo.arc(&repo.root).args([
        "inbox",
        "--assigned-to",
        "somebody-else",
        "--json",
    ]));
    assert!(
        filtered["audit-owed"].as_array().unwrap().is_empty(),
        "{}",
        filtered["audit-owed"]
    );

    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(catchup.contains("audit-owed (1):"), "{catchup}");
    assert!(catchup.contains("quota exhausted"), "{catchup}");
    assert!(catchup.contains("arc audit"), "{catchup}");

    // Discharging it empties the queue.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args(["audit", "queued", "--verdict", "approved"])
        .assert()
        .success();
    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    assert!(inbox["audit-owed"].as_array().unwrap().is_empty());
}

#[test]
fn doctor_reports_an_undischarged_obligation() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "unaudited");
    repo.arc(&repo.root)
        .args(["integrate", "unaudited", "--audit-debt", "no reviewer"])
        .assert()
        .success();
    let report = json_stdout(repo.arc(&repo.root).args(["doctor", "--json"]));
    let codes: Vec<&str> = report["advice"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"audit-debt-outstanding"), "{codes:?}");
}

/// Audit findings must be readable, or an audit that raises them is
/// write-only. They stay out of the shipped set and are reachable by name.
#[test]
fn audit_findings_are_listed_separately_and_pointed_at() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "readable");
    repo.arc(&repo.root)
        .args(["integrate", "readable", "--audit-debt", "quota"])
        .assert()
        .success();
    let path = repo.home.join("f.json");
    fs::write(
        &path,
        json_file_bytes(&serde_json::json!([{
            "blocking": true, "severity": "major", "summary": "missed edge case"
        }])),
    )
    .unwrap();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args([
            "audit",
            "readable",
            "--verdict",
            "changes-requested",
            "--findings-json",
            path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // The shipped list stays clean but says where the others are.
    let shipped = stdout(repo.arc(&repo.root).args(["findings", "readable"]));
    assert!(!shipped.contains("missed edge case"), "{shipped}");
    assert!(shipped.contains("--audit"), "{shipped}");

    let audit = stdout(
        repo.arc(&repo.root)
            .args(["findings", "readable", "--audit"]),
    );
    assert!(audit.contains("missed edge case"), "{audit}");

    let json = json_stdout(
        repo.arc(&repo.root)
            .args(["findings", "readable", "--audit", "--format", "json"]),
    );
    assert_eq!(json["audit"], true);
    assert_eq!(json["findings"].as_array().unwrap().len(), 1);

    let finding_id = json["findings"][0]["id"].as_str().unwrap();
    repo.arc(&repo.root)
        .args([
            "reply",
            "readable",
            finding_id,
            "--body",
            "tracked in the repair change",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "resolve", "readable", finding_id, "--status", "resolved", "--commit", "HEAD",
        ])
        .assert()
        .success();

    let resolved = json_stdout(
        repo.arc(&repo.root)
            .args(["findings", "readable", "--audit", "--format", "json"]),
    );
    assert_eq!(
        resolved["findings"][0]["replies"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        resolved["findings"][0]["dispositions"][0]["status"],
        "resolved"
    );
    let log = stdout(repo.arc(&repo.root).args(["log", "readable"]));
    assert!(log.contains("audit-disposition-recorded"), "{log}");
}

#[test]
fn audit_dispositions_do_not_reopen_shipped_findings_after_integration() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "shipped-finding");
    let output = stdout(repo.arc(&worktree).args([
        "finding",
        "shipped-finding",
        "--summary",
        "known before integration",
        "--severity",
        "minor",
    ]));
    let finding_id = output
        .lines()
        .find_map(|line| line.strip_prefix("finding: "))
        .unwrap();
    repo.arc(&repo.root)
        .args(["integrate", "shipped-finding", "--audit-debt", "quota"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args([
            "resolve",
            "shipped-finding",
            finding_id,
            "--status",
            "resolved",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("event is open-only"));
}

/// Without this the mechanism is decorative: a change ships on a
/// self-approval and then clears its own obligation.
#[test]
fn an_author_cannot_discharge_its_own_audit_debt_by_approving() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "selfaudit");
    repo.arc(&repo.root)
        .args(["integrate", "selfaudit", "--audit-debt", "quota"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Solo")
        .args(["audit", "selfaudit", "--verdict", "approved"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("discharge its own obligation"))
        // The refusal is also where identity gets taught, so it must name the
        // exact command that records the reviewer who actually looked.
        .stderr(predicates::str::contains("--actor"));

    // The obligation survives the refusal.
    let status = json_stdout(repo.arc(&repo.root).args(["status", "selfaudit", "--json"]));
    assert_eq!(status["audit_debt_outstanding"], true);

    // Raising problems needs no independence.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Solo")
        .args(["audit", "selfaudit", "--verdict", "changes-requested"])
        .assert()
        .success();

    // And an independent identity can approve.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args(["audit", "selfaudit", "--verdict", "approved"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "selfaudit", "--json"]));
    assert_eq!(status["audit_debt_outstanding"], false);
}

/// Audit events must survive an export/import round trip like every other
/// ledger fact, or the obligation vanishes when a change moves machines.
#[test]
fn audit_events_survive_a_bundle_round_trip() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "roundtrip");
    repo.arc(&repo.root)
        .args(["integrate", "roundtrip", "--audit-debt", "quota"])
        .assert()
        .success();
    let bundle = repo.home.join("bundle.json");
    repo.arc(&repo.root)
        .args(["export", "roundtrip", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    // Two different properties, and only one of them guards `known_event_type`.
    //
    // Classification is observable exactly once, in the import report: an
    // unclassified event is written to disk verbatim and reported as unknown,
    // and it is excluded from the import-time replayability check. It is *not*
    // lost — a destination running this same build still deserializes the
    // payload by its serde tag — so the derived-state assertions below pass
    // with or without the audit arms and cannot be the regression guard. The
    // original text-only assertion missed the defect for the same reason:
    // presence in the file was never the property in question.
    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("unknown event type").not());

    // And the obligation itself survives the transfer into derived state.
    let status =
        json_stdout(
            destination
                .arc(&destination.root)
                .args(["status", "roundtrip", "--json"]),
        );
    assert_eq!(status["audit_debt_outstanding"], true);
    assert_eq!(status["audit_debt"]["reason"], "quota");
    assert_eq!(status["audit_debt"]["patchset_id"], "ps-01");
    assert!(stdout(
        destination
            .arc(&destination.root)
            .args(["log", "roundtrip"])
    )
    .contains("audit-debt-declared"));
}

/// A debt on an open change is a pending waiver, not owed work: `arc audit`
/// refuses an open change, so queueing one would offer an item that cannot be
/// actioned.
#[test]
fn an_open_change_with_a_debt_is_not_yet_owed_work() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "pending");
    repo.arc(&worktree)
        .args(["audit-debt", "pending", "--reason", "no reviewer"])
        .assert()
        .success();

    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    assert!(
        inbox["audit-owed"].as_array().unwrap().is_empty(),
        "{}",
        inbox["audit-owed"]
    );
    assert!(stdout(repo.arc(&repo.root).args(["query", "--audit-debt"]))
        .trim()
        .is_empty());

    // The waiver is still visible where it matters: it is why check passes.
    let status = json_stdout(repo.arc(&worktree).args(["status", "pending", "--json"]));
    assert_eq!(status["audit_debt"]["patchset_id"], "ps-01");
    repo.arc(&worktree)
        .args(["check", "pending"])
        .assert()
        .code(0);

    // Once it ships, the obligation becomes actionable.
    repo.arc(&repo.root)
        .args(["integrate", "pending"])
        .assert()
        .success();
    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    assert_eq!(inbox["audit-owed"].as_array().unwrap().len(), 1);
}
