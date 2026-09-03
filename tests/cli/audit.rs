use super::common::*;

/// The policy fixture every test here needs: self-approval refused, so the
/// debt path is the only way a single actor can ship.
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

fn integrated_debt(repo: &Repo, slug: &str, path: &str, content: &str, reason: &str) -> String {
    let opened = stdout(repo.arc(&repo.root).args(["begin", slug]));
    let change_id = opened_change_id(&opened);
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&worktree, path, content, &format!("feat: {slug}"));
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&repo.root)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", slug, "--debt", reason])
        .assert()
        .success();
    change_id
}

fn snapshotted_change(repo: &Repo, slug: &str) -> PathBuf {
    snapshotted_change_with_id(repo, slug).1
}

fn snapshotted_change_with_id(repo: &Repo, slug: &str) -> (String, PathBuf) {
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    (change_id, worktree)
}

/// The write path evaluates the same policy `check` does, so an approval that
/// cannot gate says so when it is recorded rather than one command later.
/// An import is the one path that does not go through the CLI's refusals, so
/// it must refuse the same contradictions itself: a change closed twice, or
/// review recorded after an abandonment, which no audit domain covers.
#[test]
fn import_refuses_a_history_that_contradicts_itself() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "twice", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["close", "twice", "--abandoned"])
        .assert()
        .success();
    let bundle = repo.home.join("twice.json");
    repo.arc(&repo.root)
        .args(["export", "twice", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let events = value["events"].as_array().unwrap().clone();
    let closure = events
        .iter()
        .find(|event| event["event_type"] == "change-closed")
        .unwrap()
        .clone();
    let mut second = closure.clone();
    second["event_id"] = serde_json::json!("01ZZZZZZZZZZZZZZZZZZZZZZZZ");
    let tampered: Vec<serde_json::Value> = events
        .iter()
        .cloned()
        .chain(std::iter::once(second))
        .collect();
    // The checksum has to match, or both commands refuse the bundle for being
    // corrupt and this test passes without reaching the contradiction it is
    // about.
    let mut digest = <sha2::Sha256 as sha2::Digest>::new();
    for event in &tampered {
        sha2::Digest::update(&mut digest, serde_json::to_vec(event).unwrap());
        sha2::Digest::update(&mut digest, b"\n");
    }
    value["events_sha256"] = serde_json::json!(hex::encode(sha2::Digest::finalize(digest)));
    value["event_count"] = serde_json::json!(tampered.len());
    value["events"] = serde_json::json!(tampered);
    fs::write(&bundle, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

    let other = Repo::new();
    // The dry run must reach the same conclusion as the import: a preflight
    // that reports success for a bundle the real path refuses is worse than
    // no preflight, because it is believed.
    other
        .arc(&other.root)
        .args(["import", bundle.to_str().unwrap(), "--dry-run"])
        .assert()
        .failure();
    other
        .arc(&other.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .failure();
}

/// An audit reviews what reached the target, which is the range the
/// integration recorded — not a patchset range, which describes the work.
/// A closure that recorded no range cannot have one guessed for it.
#[test]
fn audit_diff_integrated_uses_the_recorded_range() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "ranged"]));
    let worktree = repo.home.join(".worktrees/repo-ranged");

    // The target moves while the change is in flight, and the change picks
    // that work up by merging it. Recorded against the older base — the
    // stacked shape, where a patchset spans its whole ancestry — the patchset
    // range contains the target's work and the integration range does not.
    // A fixture where the two coincide proves nothing about which one is used.
    let original_base = repo.head(&repo.root);
    repo.commit(
        &repo.root,
        "target-work.rs",
        "target\n",
        "chore: target work",
    );
    repo.commit(&worktree, "ranged.rs", "done\n", "feat: ranged");
    git(&worktree, &["merge", "--no-edit", "master"]);
    stdout(
        repo.arc(&worktree)
            .args(["snapshot", "ranged", "--base", &original_base]),
    );
    let target_before = repo.head(&repo.root);
    assert_ne!(
        target_before, original_base,
        "the fixture must move the target, or the two ranges coincide"
    );
    stdout(
        repo.arc(&repo.root)
            .args(["review", "ranged", "--verdict", "approved"]),
    );
    repo.arc(&repo.root)
        .args(["integrate", "ranged"])
        .assert()
        .success();

    // The range is the merge against what the target was, so the file the
    // change added appears in it.
    let rendered = stdout(
        repo.arc(&repo.root)
            .args(["diff", "ranged", "--integrated"]),
    );
    assert!(rendered.contains("ranged.rs"), "{rendered}");
    let stat = stdout(
        repo.arc(&repo.root)
            .args(["diff", "ranged", "--integrated", "--stat"]),
    );
    assert!(stat.contains("ranged.rs"), "{stat}");

    // It is the recorded base, not the patchset base: the target moved on,
    // and the range still names where it stood at integration.
    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "ranged",
        "--type",
        "change-integrated",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.trim()).unwrap();
    assert_eq!(event["target_before"], target_before, "{event}");

    // The target's own work is in the base, so it is not in the range —
    // while `diff ranged` (the patchset range) does contain it.
    assert!(!rendered.contains("target-work.rs"), "{rendered}");
    let patchset_range = stdout(repo.arc(&repo.root).args(["diff", "ranged"]));
    assert!(
        patchset_range.contains("target-work.rs"),
        "{patchset_range}"
    );
    assert!(patchset_range.contains("ranged.rs"), "{patchset_range}");

    // Selectors that describe a different range are refused rather than
    // silently ignored — including --findings, whose anchors are a patchset
    // question, and --base, which would replace a range that was recorded.
    for selector in [
        vec!["diff", "ranged", "--integrated", "--since-approved"],
        vec!["diff", "ranged", "--integrated", "--findings"],
        vec!["diff", "ranged", "--integrated", "--base", "HEAD"],
    ] {
        repo.arc(&repo.root).args(selector).assert().failure();
    }

    // An abandoned change has no integration range, and says so.
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "dropped", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["close", "dropped", "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["diff", "dropped", "--integrated"])
        .assert()
        .failure();

    // A change that never integrated has no range to render.
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "unmerged", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["diff", "unmerged", "--integrated"])
        .assert()
        .failure();
}

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
        .stdout(predicates::str::contains("--debt"));
}

/// Declaring the obligation is what unblocks integration. The requirement is
/// carried forward, not waived.
#[test]
fn declared_debt_lets_a_self_approved_change_integrate() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "owed");

    repo.arc(&worktree).args(["check", "owed"]).assert().code(3);

    repo.arc(&worktree)
        .args(["debt", "owed", "--reason", "no second actor reachable"])
        .assert()
        .success();
    repo.arc(&worktree).args(["check", "owed"]).assert().code(0);
    repo.arc(&repo.root)
        .args(["integrate", "owed"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "owed", "--json"]));
    assert_eq!(status["debt_outstanding"], true);
    assert_eq!(status["debt"]["reason"], "no second actor reachable");
}

/// A waiver authorizes a merge only when it is what let the approval stand.
/// Declared beside an approval that needed no waiver, it changed nothing, and
/// recording it would claim the merge rested on something it did not.
#[test]
fn the_basis_records_a_waiver_only_when_it_authorized_the_merge() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "owed-basis");
    repo.arc(&repo.root)
        .args(["integrate", "owed-basis", "--debt", "no reviewer reachable"])
        .assert()
        .success();
    let event: serde_json::Value = serde_json::from_str(
        stdout(repo.arc(&repo.root).args([
            "events",
            "--change",
            "owed-basis",
            "--type",
            "change-integrated",
        ]))
        .trim(),
    )
    .unwrap();
    assert!(
        event["authorization"]["audit_debt_event_id"]
            .as_str()
            .is_some(),
        "the waiver is what let this one ship: {event}"
    );
}

/// Only an approval can be waived into validity. A waiver declared beside a
/// verdict that approves nothing authorized nothing.
#[test]
fn a_waiver_beside_a_non_approval_authorizes_nothing() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "not-approved");
    stdout(
        repo.arc(&repo.root)
            .args(["review", "not-approved", "--verdict", "comment-only"]),
    );
    repo.arc(&repo.root)
        .args(["debt", "not-approved", "--reason", "none reachable"])
        .assert()
        .success();
    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "not-approved", "--json"]),
    );
    assert!(
        status
            .get("approval_waived_by_debt")
            .is_none_or(|waived| waived == false),
        "{status}"
    );
}

#[test]
fn integrate_declares_the_debt_in_one_step() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "onestep");
    repo.arc(&repo.root)
        .args(["integrate", "onestep", "--debt", "quota exhausted"])
        .assert()
        .success();
    let ids = stdout(repo.arc(&repo.root).args(["query", "--debt"]));
    assert!(ids.contains("onestep"), "{ids}");
}

/// Selection errors must be found before a policy-bearing waiver is written.
/// A refused command that leaves the self-approval gate open is worse than a
/// partial merge because its side effect is easy to miss.
#[test]
fn invalid_integrate_selection_does_not_declare_debt() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "bad-selection");

    repo.arc(&repo.root)
        .args([
            "integrate",
            "bad-selection",
            "--tag",
            "#bad-selection",
            "--debt",
            "must not persist",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("provide a change or --tag"));

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "bad-selection", "--json"]),
    );
    assert!(status["debt"].is_null(), "{}", status["debt"]);
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
        .args(["debt", "role-boundary", "--reason", "implementer waiver"])
        .assert()
        .code(9);
    repo.arc(&worktree)
        .env("ARC_ROLE", "reviewer")
        .args(["debt", "role-boundary", "--reason", "reviewer waiver"])
        .assert()
        .code(9);

    repo.arc(&repo.root)
        .args(["integrate", "role-boundary", "--debt", "lead waiver"])
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
    assert_eq!(status["debt_outstanding"], true);
}

/// An audit is a distinct event, so it never rewrites what shipped with what
/// review: the pre-closure verdict and the post-integration audit stay apart.
#[test]
fn audit_discharges_the_debt_without_rewriting_the_shipped_verdict() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "audited");
    repo.arc(&repo.root)
        .args(["integrate", "audited", "--debt", "quota exhausted"])
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
    assert_eq!(status["debt_outstanding"], false);
    assert_eq!(status["audit_verdicts"][0]["actor"], "Reviewer");
    assert_eq!(status["audit_verdicts"][0]["verdict"], "approved");
    // The shipped verdict is untouched: it is still the author's, on a patchset.
    assert_eq!(status["verdict"]["actor"], "Solo");
    assert!(status["verdict"]["patchset_id"].is_string());

    let remaining = stdout(repo.arc(&repo.root).args(["query", "--debt"]));
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
        .args(["integrate", "withfindings", "--debt", "quota"])
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
/// participated, approved an early patchset, and never saw what shipped. Any
/// identity comparison reads clean here; coverage does not.
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

    let advisories = status["advisories"].as_array().unwrap();
    assert!(
        advisories.iter().any(|advisory| {
            advisory["code"] == "reviewer-behind-final-patchset"
                && advisory["detail"]
                    .as_str()
                    .unwrap()
                    .contains("Reviewer last saw ps-01")
        }),
        "{advisories:?}"
    );

    // Advisory only: thin coverage never becomes a blocker.
    let check = json_stdout(repo.arc(&repo.root).args(["check", "drifted", "--json"]));
    assert_eq!(check["schema"], "arc-check/3", "{check}");
    assert!(check["advisories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|advisory| advisory["detail"].as_str().unwrap().contains("ps-01")));
    let text = stdout(repo.arc(&repo.root).args(["check", "drifted"]));
    assert!(text.contains("Advisories (never blocking):"), "{text}");
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
    let advisories = status["advisories"].as_array().unwrap();
    assert!(
        advisories.iter().any(|advisory| {
            advisory["code"] == "reviewer-attribution-unknown"
                && advisory["detail"]
                    .as_str()
                    .unwrap()
                    .contains("distinguishable")
        }),
        "{advisories:?}"
    );
}

/// `--dry-run` promises to write nothing, and a debt declaration is a write.
#[test]
fn dry_run_integrate_declares_no_debt() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "dry");
    // Blocked, because the debt that would have unblocked it was not written.
    repo.arc(&repo.root)
        .args(["integrate", "dry", "--dry-run", "--debt", "quota"])
        .assert()
        .code(3);
    let status = json_stdout(repo.arc(&repo.root).args(["status", "dry", "--json"]));
    assert_eq!(status["debt_outstanding"], false);
    assert!(status["debt"].is_null(), "{}", status["debt"]);
}

/// The waiver expires the way an approval expires.
///
/// A debt declared for one patchset must not excuse a self-approval on the
/// next one; otherwise a single declaration disables the policy for the rest
/// of the change's life, and nothing about the second integration looks wrong.
#[test]
fn debt_stops_waiving_once_a_new_patchset_lands() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "expiring");
    repo.arc(&worktree)
        .args(["debt", "expiring", "--reason", "no reviewer"])
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
        .args(["debt", "expiring", "--reason", "still no reviewer"])
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
        .args(["debt", "afterwards", "--reason", "found later"])
        .assert()
        .success();
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "afterwards", "--json"]),
    );
    assert_eq!(status["debt_outstanding"], true);
    assert!(
        status["debt"]["patchset_id"].is_null(),
        "{}",
        status["debt"]
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
        .args(["integrate", "queued", "--debt", "quota exhausted"])
        .assert()
        .success();

    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    assert_eq!(inbox["schema"], "arc-inbox/7");
    let owed = inbox["debt-owed"].as_array().unwrap();
    assert_eq!(owed.len(), 1, "{owed:?}");
    assert_eq!(owed[0]["next_actor"], "reviewer");

    let text = stdout(repo.arc(&repo.root).args(["inbox"]));
    assert!(text.contains("## debt-owed"), "{text}");

    let filtered = json_stdout(repo.arc(&repo.root).args([
        "inbox",
        "--assigned-to",
        "somebody-else",
        "--json",
    ]));
    assert!(
        filtered["debt-owed"].as_array().unwrap().is_empty(),
        "{}",
        filtered["debt-owed"]
    );

    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(catchup.contains("debt-owed (1):"), "{catchup}");
    assert!(catchup.contains("1 outstanding"), "{catchup}");
    assert!(catchup.contains("surfaces (1): work.txt"), "{catchup}");
    assert!(catchup.contains("arc audit"), "{catchup}");

    // Discharging it empties the queue.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args(["audit", "queued", "--verdict", "approved"])
        .assert()
        .success();
    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    assert!(inbox["debt-owed"].as_array().unwrap().is_empty());
}

#[test]
fn doctor_reports_an_undischarged_obligation() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "unaudited");
    repo.arc(&repo.root)
        .args(["integrate", "unaudited", "--debt", "no reviewer"])
        .assert()
        .success();
    let report = json_stdout(repo.arc(&repo.root).args(["doctor", "--json"]));
    assert_eq!(report["schema"], "arc-doctor/2", "{report}");
    let codes: Vec<&str> = report["advice"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"debt-outstanding"), "{codes:?}");
}

#[test]
fn catchup_and_doctor_aggregate_outstanding_debt() {
    let repo = Repo::new();
    for index in 1..=10 {
        integrated_debt(
            &repo,
            &format!("summary-{index}"),
            &format!("surface-{index}.rs"),
            &format!("surface {index}\n"),
            &format!("reason {index}"),
        );
    }

    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert_eq!(
        catchup
            .lines()
            .filter(|line| line.starts_with("debt-owed ("))
            .count(),
        1,
        "{catchup}"
    );
    assert!(catchup.contains("10 outstanding"), "{catchup}");
    assert!(catchup.contains("oldest"), "{catchup}");
    assert!(catchup.contains("surfaces (10)"), "{catchup}");
    assert!(!catchup.contains("reason 1"), "{catchup}");

    let report = json_stdout(repo.arc(&repo.root).args(["doctor", "--json"]));
    let debts = report["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["code"] == "debt-outstanding")
        .collect::<Vec<_>>();
    assert_eq!(debts.len(), 1, "{report}");
    assert!(debts[0]["detail"]
        .as_str()
        .unwrap()
        .contains("10 outstanding"));
}

#[test]
fn debt_summary_threshold_is_strict_and_opt_in() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\ndebt_count_threshold = 5\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);

    for index in 1..=5 {
        integrated_debt(
            &repo,
            &format!("threshold-{index}"),
            &format!("threshold-{index}.rs"),
            &format!("threshold {index}\n"),
            "threshold debt",
        );
    }
    let at_limit = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(!at_limit.contains("priority: advisory"), "{at_limit}");
    let at_limit_doctor = json_stdout(repo.arc(&repo.root).args(["doctor", "--json"]));
    assert!(!at_limit_doctor.to_string().contains("priority: advisory"));

    integrated_debt(
        &repo,
        "threshold-6",
        "threshold-6.rs",
        "threshold 6\n",
        "threshold debt",
    );
    let over_limit = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(over_limit.contains("priority: advisory"), "{over_limit}");
    let over_limit_doctor = json_stdout(repo.arc(&repo.root).args(["doctor", "--json"]));
    assert!(over_limit_doctor.to_string().contains("priority: advisory"));

    let no_threshold = Repo::new();
    integrated_debt(
        &no_threshold,
        "no-threshold",
        "no-threshold.rs",
        "no threshold\n",
        "ordinary debt",
    );
    let ordinary = stdout(no_threshold.arc(&no_threshold.root).args(["catchup"]));
    assert!(!ordinary.contains("priority: advisory"), "{ordinary}");

    let age_threshold = Repo::new();
    fs::create_dir_all(age_threshold.root.join(".arc")).unwrap();
    fs::write(
        age_threshold.root.join(".arc/policy.toml"),
        "[policy]\ndebt_age_threshold_seconds = 5\n",
    )
    .unwrap();
    git(&age_threshold.root, &["add", ".arc/policy.toml"]);
    git(&age_threshold.root, &["commit", "-m", "policy"]);
    let aged = integrated_debt(
        &age_threshold,
        "age-threshold",
        "age-threshold.rs",
        "aged\n",
        "aged debt",
    );
    // Integration writes the typed record; the untyped one is only what
    // earlier builds left behind.
    age_event(&age_threshold, &aged, "debt-declared", 10);
    let over_age = stdout(age_threshold.arc(&age_threshold.root).args(["catchup"]));
    assert!(over_age.contains("priority: advisory"), "{over_age}");
}

#[test]
fn touched_debt_is_named_on_check_and_catchup_only_for_intersecting_diffs() {
    let repo = Repo::new();
    let debt = integrated_debt(
        &repo,
        "debt-source",
        "shared.rs",
        "source\n",
        "deferred shared invariant",
    );

    let touched = opened_change_id(&stdout(
        repo.arc(&repo.root).args(["begin", "touches-debt"]),
    ));
    let touched_worktree = repo.home.join(".worktrees/repo-touches-debt");
    repo.commit(
        &touched_worktree,
        "shared.rs",
        "source\ncandidate\n",
        "feat: touches debt",
    );
    stdout(
        repo.arc(&touched_worktree)
            .args(["snapshot", "touches-debt"]),
    );
    let touched_check = stdout(repo.arc(&touched_worktree).args(["check", "touches-debt"]));
    assert!(touched_check.contains(&touched), "{touched_check}");
    assert!(
        touched_check.contains("deferred shared invariant"),
        "{touched_check}"
    );
    assert!(touched_check.contains(&debt), "{touched_check}");

    let untouched = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "untouched"])));
    let untouched_worktree = repo.home.join(".worktrees/repo-untouched");
    repo.commit(
        &untouched_worktree,
        "other.rs",
        "unrelated\n",
        "feat: untouched",
    );
    stdout(
        repo.arc(&untouched_worktree)
            .args(["snapshot", "untouched"]),
    );
    let untouched_check = stdout(repo.arc(&untouched_worktree).args(["check", "untouched"]));
    assert!(
        untouched_check.contains("1 outstanding"),
        "{untouched_check}"
    );
    assert!(
        !untouched_check.contains("deferred shared invariant"),
        "{untouched_check}"
    );
    assert!(!untouched_check.contains(&debt), "{untouched_check}");

    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(catchup.contains(&format!("debt {debt}")), "{catchup}");
    assert!(catchup.contains("deferred shared invariant"), "{catchup}");
    let untouched_line = catchup
        .lines()
        .position(|line| line.contains(&untouched))
        .expect("untouched change should be listed");
    assert!(!catchup
        .lines()
        .skip(untouched_line)
        .take(2)
        .any(|line| line.contains("deferred shared invariant")));
}

/// Audit findings must be readable, or an audit that raises them is
/// write-only. They stay out of the shipped set and are reachable by name.
#[test]
fn audit_findings_are_listed_separately_and_pointed_at() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "readable");
    repo.arc(&repo.root)
        .args(["integrate", "readable", "--debt", "quota"])
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
        .args(["integrate", "shipped-finding", "--debt", "quota"])
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
fn an_author_cannot_discharge_its_own_debt_by_approving() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "selfaudit");
    repo.arc(&repo.root)
        .args(["integrate", "selfaudit", "--debt", "quota"])
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
    assert_eq!(status["debt_outstanding"], true);

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
    assert_eq!(status["debt_outstanding"], false);
}

/// Bundle import must classify audit events as typed so replay validation
/// includes them instead of reporting them as opaque future events.
#[test]
fn audit_events_survive_a_bundle_round_trip() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "roundtrip");
    repo.arc(&repo.root)
        .args(["integrate", "roundtrip", "--debt", "quota"])
        .assert()
        .success();
    let bundle = repo.home.join("bundle.json");
    repo.arc(&repo.root)
        .args(["export", "roundtrip", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    // Unknown tags are preserved verbatim but excluded from import-time typed
    // replay validation. Same-build loading may still recognize the raw event,
    // so transferred state alone does not prove that import classified it.
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
    assert_eq!(status["debt_outstanding"], true);
    assert_eq!(status["debt"]["reason"], "quota");
    assert_eq!(status["debt"]["patchset_id"], "ps-01");
    // A debt written today round-trips as the typed record, not as the
    // untyped one earlier builds wrote.
    let log = stdout(
        destination
            .arc(&destination.root)
            .args(["log", "roundtrip"]),
    );
    assert!(log.contains("debt-declared"), "{log}");
    assert!(!log.contains("audit-debt-declared"), "{log}");
}

/// A debt on an open change is a pending waiver, not owed work: `arc audit`
/// refuses an open change, so queueing one would offer an item that cannot be
/// actioned.
#[test]
fn an_open_change_with_a_debt_is_not_yet_owed_work() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "pending");
    repo.arc(&worktree)
        .args(["debt", "pending", "--reason", "no reviewer"])
        .assert()
        .success();

    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    assert!(
        inbox["debt-owed"].as_array().unwrap().is_empty(),
        "{}",
        inbox["debt-owed"]
    );
    assert!(stdout(repo.arc(&repo.root).args(["query", "--debt"]))
        .trim()
        .is_empty());

    // The waiver is still visible where it matters: it is why check passes.
    let status = json_stdout(repo.arc(&worktree).args(["status", "pending", "--json"]));
    assert_eq!(status["debt"]["patchset_id"], "ps-01");
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
    assert_eq!(inbox["debt-owed"].as_array().unwrap().len(), 1);
}

/// A change with no verdict at all, so the gate is unmet for want of a review
/// rather than because a reviewer refused.
fn unreviewed_change(repo: &Repo, slug: &str) -> PathBuf {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Solo")
            .args(["snapshot", slug]),
    );
    worktree
}

/// The waiver stands in for a verdict nobody recorded, in the same invocation
/// that declares it. Before this, `integrate --debt` recorded the
/// obligation and refused anyway, so the only way through was to first record a
/// self-approval nobody believed and waive that — a worse record than none.
#[test]
fn a_declared_debt_stands_in_for_a_verdict_nobody_recorded() {
    let repo = repo_forbidding_self_approval();
    let worktree = unreviewed_change(&repo, "unreviewed");

    // Blocked for want of an approval, which is the gate the waiver addresses.
    repo.arc(&worktree)
        .args(["check", "unreviewed"])
        .assert()
        .code(3);

    // One invocation: the merge happens and the obligation is recorded.
    repo.arc(&repo.root)
        .args(["integrate", "unreviewed", "--debt", "no reviewer reachable"])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "unreviewed", "--json"]),
    );
    assert_eq!(status["debt_outstanding"], true, "{status}");
    assert_eq!(
        status["debt"]["reason"], "no reviewer reachable",
        "{status}"
    );
    // The merge rested on the waiver and the status says so, so a reader cannot
    // mistake it for a change that was independently approved.
    assert_eq!(status["approval_waived_by_debt"], true, "{status}");
}

/// A waiver defers a review nobody has done. It does not overrule one that was
/// done and came back negative: letting the author waive past `changes-requested`
/// would turn the mechanism into a way to ignore review rather than defer it.
#[test]
fn a_declared_debt_does_not_overrule_a_reviewer_who_refused() {
    let repo = repo_forbidding_self_approval();
    let worktree = unreviewed_change(&repo, "refused");

    repo.arc(&worktree)
        .env("ARC_ACTOR", "Reviewer")
        .args([
            "review",
            "refused",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args(["debt", "refused", "--reason", "shipping anyway"])
        .assert()
        .success();

    // Still blocked: the gate is unmet because someone read this patchset and
    // asked for changes, which is not a missing verdict.
    repo.arc(&worktree)
        .args(["check", "refused"])
        .assert()
        .code(3);
    repo.arc(&repo.root)
        .args(["integrate", "refused"])
        .assert()
        .failure();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "refused", "--json"]));
    // Absent or false — either way the waiver authorized nothing here.
    assert_ne!(status["approval_waived_by_debt"], true, "{status}");
    assert_eq!(status["ready_reason"], "no-valid-approval", "{status}");
}

/// A waiver binds to the committed head inside its patchset, not only to the
/// patchset label. Moving the branch without snapshotting must stale both the
/// approval and the debt rather than integrating the older revision.
#[test]
fn a_declared_debt_does_not_overrule_a_stale_approval() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "stale-waiver");
    repo.arc(&repo.root)
        .args([
            "debt",
            "stale-waiver",
            "--reason",
            "review the recorded patchset later",
        ])
        .assert()
        .success();

    repo.commit(
        &worktree,
        "work.txt",
        "moved after snapshot\n",
        "test: move past waived patchset",
    );

    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "stale-waiver", "--json"]),
    );
    assert_eq!(status["head_matches_latest_patchset"], false, "{status}");
    assert_ne!(status["approval_waived_by_debt"], true, "{status}");
    assert_eq!(status["ready_reason"], "no-valid-approval", "{status}");
    repo.arc(&worktree)
        .args(["check", "stale-waiver"])
        .assert()
        .code(3);
    repo.arc(&repo.root)
        .args(["integrate", "stale-waiver"])
        .assert()
        .code(3);
}

fn repo_with_danger(paths: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        format!("[policy]\nforbid_self_approval = true\n\n[danger]\npaths = [{paths}]\n"),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);
    repo
}

/// The integration keeps the danger determination that made its review gate
/// necessary, even after the repository changes its declaration.
#[test]
fn integration_records_and_preserves_its_danger_determination() {
    let repo = repo_with_danger("\"dangerous.txt\"");
    stdout(repo.arc(&repo.root).args(["begin", "dangerous-change"]));
    let worktree = repo.home.join(".worktrees").join("repo-dangerous-change");
    repo.commit(
        &worktree,
        "dangerous.txt",
        "dangerous\n",
        "feat: dangerous change",
    );
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "author")
            .args(["snapshot", "dangerous-change"]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args(["review", "dangerous-change", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "dangerous-change"])
        .assert()
        .success();

    let integrated = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "dangerous-change",
        "--type",
        "change-integrated",
    ]));
    let event: serde_json::Value = serde_json::from_str(integrated.trim()).unwrap();
    assert_eq!(
        event["authorization"]["danger"],
        serde_json::json!({
            "dangerous": true,
            "rule": "declared-path",
            "paths": ["dangerous.txt"]
        }),
        "{event}"
    );

    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n\n[danger]\npaths = [\"other.txt\"]\n",
    )
    .unwrap();
    let reread = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "dangerous-change",
        "--type",
        "change-integrated",
    ]));
    let reread: serde_json::Value = serde_json::from_str(reread.trim()).unwrap();
    assert_eq!(
        reread["authorization"]["danger"]["paths"],
        serde_json::json!(["dangerous.txt"]),
        "{reread}"
    );
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "dangerous-change", "--json"]),
    );
    assert_eq!(
        status["closure"]["authorization"]["danger"]["rule"], "declared-path",
        "{status}"
    );
}

/// A uniform gate over non-uniform risk produces a uniform workaround, used
/// most where it matters least. A change touching nothing the project called
/// dangerous ships on a self-recorded verdict.
#[test]
fn a_change_touching_no_declared_surface_ships_on_a_self_verdict() {
    let repo = repo_with_danger("\"src/store.rs\"");
    stdout(repo.arc(&repo.root).args(["begin", "docs-only"]));
    let worktree = repo.home.join(".worktrees").join("repo-docs-only");
    repo.commit(&worktree, "README.md", "docs\n", "docs: readme");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Solo")
            .args(["snapshot", "docs-only"]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args(["review", "docs-only", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "docs-only", "--json"]));
    assert_eq!(status["danger"]["dangerous"], false);
    assert_eq!(status["danger"]["rule"], "untouched");
    assert_eq!(
        status["verdict"]["valid_for_current_head"], true,
        "a self-verdict satisfies the gate off the declared surfaces"
    );
    repo.arc(&repo.root)
        .args(["integrate", "docs-only"])
        .assert()
        .success();
    let event: serde_json::Value = serde_json::from_str(
        stdout(repo.arc(&repo.root).args([
            "events",
            "--change",
            "docs-only",
            "--type",
            "change-integrated",
        ]))
        .trim(),
    )
    .unwrap();
    assert_eq!(
        event["authorization"]["danger"]["dangerous"], false,
        "{event}"
    );
    assert_eq!(
        event["authorization"]["danger"]["rule"], "untouched",
        "{event}"
    );
}

/// On a declared surface the rule still binds, and the advisory names which
/// path put the change in scope.
#[test]
fn a_change_touching_a_declared_surface_still_needs_an_independent_verdict() {
    let repo = repo_with_danger("\"*.rs\"");
    stdout(repo.arc(&repo.root).args(["begin", "touches-core"]));
    let worktree = repo.home.join(".worktrees").join("repo-touches-core");
    repo.commit(&worktree, "store.rs", "core\n", "feat: core");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Solo")
            .args(["snapshot", "touches-core"]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args(["review", "touches-core", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "touches-core", "--json"]),
    );
    assert_eq!(status["danger"]["dangerous"], true);
    assert_eq!(status["danger"]["rule"], "declared-path");
    assert_eq!(status["danger"]["paths"][0], "store.rs");
    assert_eq!(
        status["verdict"]["valid_for_current_head"], false,
        "a self-approval on a declared surface is still rejected"
    );
}

/// Escalation is one-way: a change may raise itself, and nothing about what
/// it happens to touch lowers it again.
#[test]
fn begin_dangerous_raises_a_change_that_touches_nothing_declared() {
    let repo = repo_with_danger("\"src/store.rs\"");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "raised", "--dangerous"]),
    );
    let worktree = repo.home.join(".worktrees").join("repo-raised");
    repo.commit(&worktree, "README.md", "docs\n", "docs: readme");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Solo")
            .args(["snapshot", "raised"]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args(["review", "raised", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "raised", "--json"]));
    assert_eq!(status["danger"]["rule"], "escalated");
    assert_eq!(
        status["verdict"]["valid_for_current_head"], false,
        "a change that raised itself cannot then self-approve"
    );
}

/// Adopting the feature must be opt-in: a repository that declares no
/// dangerous surfaces keeps the uniform gate it had before.
#[test]
fn an_undeclared_danger_list_keeps_the_uniform_gate() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "uniform");
    let status = json_stdout(repo.arc(&repo.root).args(["status", "uniform", "--json"]));
    assert_eq!(status["danger"]["rule"], "not-declared");
    assert_eq!(status["danger"]["dangerous"], true);
    assert_eq!(status["verdict"]["valid_for_current_head"], false);
}

/// A declared literal that names nothing reads as coverage while leaving the
/// surface on a self-verdict. A rename is enough to cause it, and nothing
/// else in the tool would ever say so.
#[test]
fn doctor_reports_a_declared_danger_path_that_matches_nothing() {
    let repo = repo_with_danger("\"gone.rs\", \"*.missing\"");
    let out = repo
        .arc(&repo.root)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let problems = report["problems"].as_array().unwrap();
    let hits: Vec<_> = problems
        .iter()
        .filter(|p| p["code"] == "danger-path-matches-nothing")
        .collect();
    assert_eq!(hits.len(), 1, "only literals are checked: {problems:?}");
    assert!(
        hits[0]["detail"].as_str().unwrap().starts_with("gone.rs"),
        "{:?}",
        hits[0]
    );
}

/// Without a declared root the list is open-world, and a file nobody
/// classified matches nothing, looks healthy, and lands on a self-verdict.
/// Both observed misses failed in that direction, so `doctor` refuses a
/// tracked file inside a declared root that carries no classification — and
/// refuses one carrying both, where one of the two claims must be wrong.
#[test]
fn doctor_refuses_a_file_that_is_unclassified_or_classified_twice() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::create_dir_all(repo.root.join("src")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n\n[danger]\n\
         paths = [\"src/gate.rs\", \"src/both.rs\"]\n\
         source_roots = [\"src/\"]\n\
         acknowledged_safe = [\"src/view.rs\", \"src/both.rs\"]\n",
    )
    .unwrap();
    for name in ["gate.rs", "view.rs", "both.rs", "stray.rs"] {
        fs::write(repo.root.join("src").join(name), "// file\n").unwrap();
    }
    // A file outside the declared root is not part of the closed world.
    fs::write(repo.root.join("elsewhere.rs"), "// file\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "policy and sources"]);

    let out = repo
        .arc(&repo.root)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let problems = report["problems"].as_array().unwrap();

    let unclassified: Vec<_> = problems
        .iter()
        .filter(|p| p["code"] == "danger-unclassified")
        .collect();
    assert_eq!(unclassified.len(), 1, "{problems:?}");
    assert!(
        unclassified[0]["detail"]
            .as_str()
            .unwrap()
            .starts_with("src/stray.rs"),
        "{:?}",
        unclassified[0]
    );

    let conflicts: Vec<_> = problems
        .iter()
        .filter(|p| p["code"] == "danger-classification-conflict")
        .collect();
    assert_eq!(conflicts.len(), 1, "{problems:?}");
    assert!(
        conflicts[0]["detail"]
            .as_str()
            .unwrap()
            .starts_with("src/both.rs"),
        "{:?}",
        conflicts[0]
    );
}

/// `git ls-files` lists the subtree it runs from, under names relative to it,
/// so a scan run from anywhere but the toplevel checks a subset against roots
/// that match nothing — and reports a clean bill of health for a world it
/// never looked at. The failure is silent by construction, so the regression
/// runs `doctor` from a subdirectory rather than asserting the call site.
#[test]
fn classification_is_checked_from_a_subdirectory_too() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::create_dir_all(repo.root.join("src/commands")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n\n[danger]\n\
         paths = [\"src/gate.rs\"]\n\
         source_roots = [\"src/\"]\n\
         acknowledged_safe = [\"src/view.rs\"]\n",
    )
    .unwrap();
    for path in ["src/gate.rs", "src/view.rs", "src/stray.rs"] {
        fs::write(repo.root.join(path), "// file\n").unwrap();
    }
    fs::write(repo.root.join("src/commands/deep.rs"), "// file\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "policy and sources"]);

    for dir in [
        repo.root.clone(),
        repo.root.join("src"),
        repo.root.join("src/commands"),
    ] {
        let out = repo.arc(&dir).args(["doctor", "--json"]).output().unwrap();
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let unclassified: Vec<&str> = report["problems"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["code"] == "danger-unclassified")
            .map(|p| p["detail"].as_str().unwrap())
            .collect();
        // The same two unclassified files, wherever the command was run.
        assert_eq!(unclassified.len(), 2, "from {}: {report}", dir.display());
        assert!(
            unclassified
                .iter()
                .any(|detail| detail.starts_with("src/stray.rs")),
            "from {}: {report}",
            dir.display()
        );
        assert!(
            unclassified
                .iter()
                .any(|detail| detail.starts_with("src/commands/deep.rs")),
            "from {}: {report}",
            dir.display()
        );
    }
}

/// Closing the world is opt-in: a project that declares no source root keeps
/// the open-world list it had, and `doctor` says nothing about files nobody
/// classified.
#[test]
fn an_undeclared_source_root_leaves_classification_open() {
    let repo = repo_with_danger("\"src/gate.rs\"");
    fs::create_dir_all(repo.root.join("src")).unwrap();
    fs::write(repo.root.join("src/gate.rs"), "// file\n").unwrap();
    fs::write(repo.root.join("src/stray.rs"), "// file\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "sources"]);

    let out = repo
        .arc(&repo.root)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let problems = report["problems"].as_array().unwrap();
    assert!(
        !problems.iter().any(|p| p["code"] == "danger-unclassified"),
        "{problems:?}"
    );
}

/// The debt records that no verdict existed, not that `arc audit`
/// specifically must supply one. An operator who reviewed before merging
/// otherwise had no honest move: leave a debt standing for a review that
/// happened, or file a post-integration audit that did not.
#[test]
fn an_independent_verdict_on_the_shipped_patchset_discharges_the_debt() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "reviewed-early");

    // The debt is declared while no reviewer is reachable.
    repo.arc(&repo.root)
        .args(["debt", "reviewed-early", "--reason", "none reachable"])
        .assert()
        .success();

    // One then becomes reachable and reviews the same patchset, before merge.
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Reviewer")
        .args([
            "--model",
            "gpt-5.6-premerge#high",
            "review",
            "reviewed-early",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "reviewed-early"])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "reviewed-early", "--json"]),
    );
    assert_eq!(
        status["debt_outstanding"], false,
        "an independent verdict on the shipped patchset is the review the debt owed"
    );
    assert_eq!(
        status["debt"]["discharged_by"]["model"], "gpt-5.6-premerge#high",
        "{status}"
    );
    assert!(
        stdout(repo.arc(&repo.root).args(["show", "reviewed-early"]))
            .contains("Discharged by: Reviewer@high (gpt-5.6-premerge#high)")
    );
    let remaining = stdout(repo.arc(&repo.root).args(["query", "--debt"]));
    assert!(!remaining.contains("reviewed-early"), "{remaining}");
}

/// Discharge requires independence, not merely a verdict: the author's own
/// approval is what the debt was declared over in the first place.
#[test]
fn the_authors_own_verdict_does_not_discharge_the_debt() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "self-only");
    repo.arc(&repo.root)
        .args(["integrate", "self-only", "--debt", "none reachable"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "self-only", "--json"]));
    assert_eq!(
        status["debt_outstanding"], true,
        "the author's approval is precisely what the debt stands in for"
    );
}

/// A name arc took from `git config` is not a claim anybody made, so it cannot
/// answer the independence question either way. It has to fail it rather than
/// pass by happening not to match a contributor.
#[test]
fn an_assumed_identity_does_not_discharge_the_debt() {
    let repo = repo_forbidding_self_approval();
    self_approved_change(&repo, "assumed-auditor");
    repo.arc(&repo.root)
        .args(["integrate", "assumed-auditor", "--debt", "none reachable"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .args([
            "audit",
            "assumed-auditor",
            "--verdict",
            "changes-requested",
            "--body",
            "found a problem",
        ])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "assumed-auditor", "--json"]),
    );
    assert_eq!(
        status["debt_outstanding"], true,
        "an identity arc invented cannot settle a debt owed an independent review"
    );

    // A declared identity from outside the contributor set still can.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "outsider")
        .args([
            "audit",
            "assumed-auditor",
            "--verdict",
            "approved",
            "--body",
            "read it",
        ])
        .assert()
        .success();
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "assumed-auditor", "--json"]),
    );
    assert_eq!(status["debt_outstanding"], false, "{status}");
}

/// A verdict on an earlier draft judged something other than what shipped.
#[test]
fn a_verdict_on_a_superseded_patchset_does_not_discharge_the_debt() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "moved-on");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Reviewer")
        .args(["review", "moved-on", "--verdict", "comment-only"])
        .assert()
        .success();

    // New work lands and is snapshotted; the reviewer never saw it.
    repo.commit(&worktree, "more.txt", "more\n", "feat: more");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Solo")
            .args(["snapshot", "moved-on"]),
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args(["review", "moved-on", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "moved-on", "--debt", "shipping now"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "moved-on", "--json"]));
    assert_eq!(
        status["debt_outstanding"], true,
        "the independent verdict judged an earlier revision than the one that shipped"
    );
}

/// Independence is a fact about the patchset a reviewer read. Judging it
/// against whatever is newest lets a later snapshot by someone else
/// retroactively launder a self-review into an independent one.
#[test]
fn a_later_patchset_by_another_author_does_not_relabel_an_earlier_self_review() {
    let repo = repo_forbidding_self_approval();
    let worktree = self_approved_change(&repo, "relabel");

    // Somebody else snapshots on top. The earlier verdict is still Solo's own.
    repo.commit(&worktree, "other.txt", "other\n", "feat: other");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Other")
            .args(["snapshot", "relabel"]),
    );

    let status = json_stdout(repo.arc(&repo.root).args(["status", "relabel", "--json"]));
    let solo = status["review_map"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["reviewer"] == "Solo")
        .expect("the author's verdict is still in the review map");
    assert_eq!(
        solo["is_author"], true,
        "Solo authored the patchset Solo reviewed; a newer snapshot cannot change that"
    );
}

/// Declared paths are matched against `git diff --name-only`, which names
/// files. A bare directory exists, passes an existence check, and still
/// matches nothing — the same silent widening the dead-path check prevents,
/// one level down.
#[test]
fn doctor_reports_a_declared_directory_that_can_never_match() {
    let repo = repo_with_danger("\"sub\", \"sub/\"");
    fs::create_dir_all(repo.root.join("sub")).unwrap();
    fs::write(repo.root.join("sub/file.rs"), "x\n").unwrap();
    git(&repo.root, &["add", "sub"]);
    git(&repo.root, &["commit", "-m", "sub"]);

    let out = repo
        .arc(&repo.root)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let hits: Vec<&str> = report["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["code"] == "danger-path-matches-nothing")
        .map(|p| p["detail"].as_str().unwrap())
        .collect();
    assert_eq!(hits.len(), 1, "the trailing-slash form is fine: {hits:?}");
    assert!(hits[0].starts_with("sub is a directory"), "{hits:?}");
}

/// A report derived from the ledger alone still resolves the danger scope.
/// `git diff` needs the objects the recorded base and head name, not a working
/// tree, so assuming dangerous there over-reported the requirement.
#[test]
fn a_ledger_only_report_resolves_the_danger_scope() {
    let repo = repo_with_danger("\"*.rs\"");
    stdout(repo.arc(&repo.root).args(["begin", "ledger-only"]));
    let worktree = repo.home.join(".worktrees").join("repo-ledger-only");
    repo.commit(&worktree, "README.md", "docs\n", "docs: readme");
    let snapshot_out = stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Solo")
            .args(["snapshot", "ledger-only"]),
    );

    // `--at` replays from the ledger alone, consulting no working tree.
    let snapshot = snapshot_out
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .expect("snapshot reports its event id")
        .trim()
        .to_string();
    let as_of =
        json_stdout(
            repo.arc(&repo.root)
                .args(["status", "ledger-only", "--at", &snapshot]),
        );
    assert_eq!(
        as_of["danger"]["rule"], "untouched",
        "a ledger-only report must resolve the scope, not assume it: {:?}",
        as_of["danger"]
    );
    assert_eq!(as_of["danger"]["dangerous"], false);
}

/// A verdict answers what the reviewer concluded. Whether that conclusion
/// should be relied on yet is a different question, and collapsing the two
/// let a reviewer nobody had validated discharge the gate exactly as one
/// whose judgment had been.
#[test]
fn a_provisional_approval_gates_while_recording_what_it_still_owes() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "provisional");

    repo.arc(&worktree)
        .env("ARC_ACTOR", "unproven-reviewer")
        .args([
            "review",
            "provisional",
            "--verdict",
            "approved",
            "--provisional",
            "reviewer is an unmeasured model",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "provisional: reviewer is an unmeasured model",
        ));

    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "provisional", "--json"]),
    );
    let report = &status;
    assert_eq!(
        report["verdict"]["provisional"],
        "reviewer is an unmeasured model"
    );
    assert_eq!(report["provisional_approval_outstanding"], true);
    // It gates: the obligation is recorded, not a blocker.
    assert_eq!(report["integrate_ready"], true, "{report}");

    let check = stdout(repo.arc(&worktree).args(["check", "provisional"]));
    assert!(check.contains("provisional-approval"), "{check}");
    assert!(check.contains("reviewer is an unmeasured model"), "{check}");

    let owed = stdout(repo.arc(&repo.root).args(["query", "--provisional"]));
    assert!(owed.contains(&change_id), "{owed}");
}

/// `arc review` is where a lead reads what the review state actually is, and a
/// provisional approval that gates while owing corroboration must be
/// distinguishable there from an unqualified one. Reported once when the
/// verdict is recorded and never again, the qualification reaches only the
/// person who already knew it.
#[test]
fn review_history_names_a_provisional_approval_and_its_outstanding_corroboration() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "provisional");

    repo.arc(&worktree)
        .env("ARC_ACTOR", "unproven-reviewer")
        .args([
            "review",
            "provisional",
            "--verdict",
            "approved",
            "--provisional",
            "reviewer is an unmeasured model",
        ])
        .assert()
        .success();

    let history = stdout(repo.arc(&worktree).args(["review", "provisional"]));
    assert!(
        history.contains("provisional, corroboration outstanding"),
        "{history}"
    );
    assert!(
        history.contains("reviewer is an unmeasured model"),
        "{history}"
    );

    let view = json_stdout(
        repo.arc(&worktree)
            .args(["review", "provisional", "--json"]),
    );
    assert_eq!(
        view["verdicts"][0]["provisional"],
        "reviewer is an unmeasured model"
    );
    assert_eq!(view["verdicts"][0]["provisional_outstanding"], true);
}

/// The obligation is legible from the record of the merge itself. An auditor
/// reading an authorization basis that names a verdict event must be able to
/// tell a validated reviewer from an unvalidated one without following the
/// pointer.
#[test]
fn the_authorization_basis_records_that_the_merge_rested_on_a_provisional_verdict() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "basis-provisional");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "unproven-reviewer")
        .args([
            "review",
            "basis-provisional",
            "--verdict",
            "approved",
            "--provisional",
            "third-party benchmark only",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "basis-provisional"])
        .assert()
        .success();

    let closed = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        &change_id,
        "--type",
        "change-integrated",
    ]));
    let event: serde_json::Value = serde_json::from_str(closed.trim()).unwrap();
    let basis = &event["authorization"];
    assert!(basis["verdict_event_id"].is_string(), "{event}");
    assert_eq!(basis["verdict_provisional"], "third-party benchmark only");
}

/// An ordinary verdict carries none of this, and the schema stays quiet about
/// an obligation nobody declared.
#[test]
fn an_ordinary_verdict_records_no_obligation() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "ordinary");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args(["review", "ordinary", "--verdict", "approved"])
        .assert()
        .success()
        .stdout(predicates::str::contains("provisional").not());

    let status = json_stdout(repo.arc(&worktree).args(["status", "ordinary", "--json"]));
    assert!(status["verdict"]["provisional"].is_null());
    assert_eq!(status["provisional_approval_outstanding"], false);
    assert!(!stdout(repo.arc(&repo.root).args(["query", "--provisional"])).contains(&change_id));
}

/// An audit discharges a provisional approval exactly as it discharges debt:
/// one obligation, one discharge, so a caller never has to learn two.
#[test]
fn an_audit_discharges_a_provisional_approval() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "discharged");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "unproven-reviewer")
        .args([
            "review",
            "discharged",
            "--verdict",
            "approved",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "discharged"])
        .assert()
        .success();
    assert!(stdout(repo.arc(&repo.root).args(["query", "--provisional"])).contains(&change_id));

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "proven-auditor")
        .args(["audit", "discharged", "--verdict", "approved"])
        .assert()
        .success();

    let owed = stdout(repo.arc(&repo.root).args(["query", "--provisional"]));
    assert!(!owed.contains(&change_id), "{owed}");
}

/// An obligation nobody can knowingly discharge is worse than none: it reads
/// as tracked while saying nothing about what is owed.
#[test]
fn a_provisional_verdict_must_say_why() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "empty-reason");
    repo.arc(&worktree)
        .args([
            "review",
            "empty-reason",
            "--verdict",
            "approved",
            "--provisional",
            "   ",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "must say why this verdict is owed corroboration",
        ));
}

/// The obligation must survive a later verdict of any kind. Deriving it from
/// the newest verdict let any subsequent record mask a provisional approval
/// that was still the one gating the change — the obligation vanished with no
/// audit, which is the silent under-report this whole surface exists to end.
#[test]
fn a_later_verdict_does_not_mask_a_provisional_approval_that_still_gates() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "masked");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args([
            "review",
            "masked",
            "--verdict",
            "approved",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .success();
    // A finding recorded by anyone is not corroboration of anything, and must
    // not clear the obligation the way any later verdict once did.
    repo.arc(&worktree)
        .env("ARC_ACTOR", "dave")
        .args(["finding", "masked", "--summary", "a passing note"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&worktree).args(["status", "masked", "--json"]));
    assert_eq!(status["provisional_approval_outstanding"], true, "{status}");
    assert!(stdout(repo.arc(&repo.root).args(["query", "--provisional"])).contains(&change_id));
    let check = stdout(repo.arc(&worktree).args(["check", "masked"]));
    assert!(check.contains("provisional-approval"), "{check}");
}

/// A provisional approval left behind by a new patchset gates nothing, so
/// reporting it as owed is an obligation nobody can act on. The JSON flag and
/// the advisory must agree about that, because they are one derivation.
#[test]
fn a_provisional_approval_stops_being_owed_once_a_new_patchset_strands_it() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "stranded");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args([
            "review",
            "stranded",
            "--verdict",
            "approved",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .success();
    repo.commit(&worktree, "more.txt", "more\n", "feat: more");
    stdout(repo.arc(&worktree).args(["snapshot", "stranded"]));

    let status = json_stdout(repo.arc(&worktree).args(["status", "stranded", "--json"]));
    assert_eq!(
        status["provisional_approval_outstanding"], false,
        "{status}"
    );
    assert!(!stdout(repo.arc(&repo.root).args(["query", "--provisional"])).contains(&change_id));
    let check = stdout(repo.arc(&worktree).args(["check", "stranded"]));
    assert!(!check.contains("provisional-approval"), "{check}");
}

/// A second independent approval of the same patchset is the corroboration
/// the obligation was for. Requiring an `arc audit` specifically would mean
/// the debt could not be discharged before the merge, which is exactly when
/// it is cheapest to discharge.
#[test]
fn an_independent_approval_of_the_same_patchset_corroborates() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "corroborated");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args([
            "review",
            "corroborated",
            "--verdict",
            "approved",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "dave")
        .args(["review", "corroborated", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "corroborated", "--json"]),
    );
    assert_eq!(
        status["provisional_approval_outstanding"], false,
        "{status}"
    );
    assert!(!stdout(repo.arc(&repo.root).args(["query", "--provisional"])).contains(&change_id));
}

/// A reviewer confirming its own unproven verdict is the one thing that
/// cannot be corroboration, whichever command it uses to do it.
#[test]
fn a_reviewer_cannot_corroborate_its_own_provisional_approval() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "self-corroborated");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args([
            "review",
            "self-corroborated",
            "--verdict",
            "approved",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args(["review", "self-corroborated", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "self-corroborated", "--json"]),
    );
    assert_eq!(status["provisional_approval_outstanding"], true, "{status}");
    assert!(stdout(repo.arc(&repo.root).args(["query", "--provisional"])).contains(&change_id));
}

/// Only an approval discharges the review gate, so only an approval can owe
/// corroboration for having done so. Recording the marker elsewhere would
/// leave it tracked and invisible.
#[test]
fn provisional_is_refused_on_a_verdict_that_gates_nothing() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "not-an-approval");
    repo.arc(&worktree)
        .args([
            "review",
            "not-an-approval",
            "--verdict",
            "comment-only",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--provisional is only valid with --verdict approved",
        ));
}

/// User-facing text is part of the contract. A hard-wrapped literal that
/// loses its line continuation prints a run of spaces mid-sentence, and a
/// `contains` assertion never notices.
#[test]
fn the_provisional_refusals_and_advisory_read_as_sentences() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "wrapping");
    let refusal = repo
        .arc(&worktree)
        .args([
            "review",
            "wrapping",
            "--verdict",
            "approved",
            "--provisional",
            "   ",
        ])
        .output()
        .unwrap();
    let refusal = String::from_utf8_lossy(&refusal.stderr).into_owned();
    assert!(!refusal.contains("  "), "double space in: {refusal}");

    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args([
            "review",
            "wrapping",
            "--verdict",
            "approved",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .success();
    let check = stdout(repo.arc(&worktree).args(["check", "wrapping"]));
    let advisory = check
        .lines()
        .find(|line| line.contains("provisional-approval"))
        .unwrap()
        .trim();
    assert!(!advisory.contains("  "), "double space in: {advisory}");
}

/// The author of the change cannot corroborate a verdict on it. Excluding
/// only the provisional reviewer let an author clear the obligation by
/// approving their own change — the silent drop this surface exists to end.
#[test]
fn the_change_author_cannot_corroborate_a_provisional_approval() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "author-corroborates");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args([
            "review",
            "author-corroborates",
            "--verdict",
            "approved",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .success();
    // `change_with_patchset` snapshots as the default test actor, so that
    // actor is the change's author.
    repo.arc(&worktree)
        .args(["review", "author-corroborates", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "author-corroborates", "--json"]),
    );
    assert_eq!(status["provisional_approval_outstanding"], true, "{status}");
    assert!(stdout(repo.arc(&repo.root).args(["query", "--provisional"])).contains(&change_id));
}

/// A superseding verdict leaves no approval gating the change, so there is no
/// approval left to owe corroboration. Reporting one would advise work on an
/// obligation that cannot be acted on.
#[test]
fn a_superseding_verdict_leaves_no_provisional_obligation() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "superseded");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args([
            "review",
            "superseded",
            "--verdict",
            "approved",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "dave")
        .args(["review", "superseded", "--verdict", "comment-only"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&worktree).args(["status", "superseded", "--json"]));
    assert_eq!(status["integrate_ready"], false, "{status}");
    assert_eq!(
        status["provisional_approval_outstanding"], false,
        "{status}"
    );
    assert!(!stdout(repo.arc(&repo.root).args(["query", "--provisional"])).contains(&change_id));
}

/// A reviewer cannot launder its own obligation by re-approving without the
/// flag: the second verdict is the same judgment, not a second one.
#[test]
fn re_approving_without_the_flag_does_not_clear_the_obligation() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "relaundered");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args([
            "review",
            "relaundered",
            "--verdict",
            "approved",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args(["review", "relaundered", "--verdict", "approved"])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "relaundered", "--json"]),
    );
    assert_eq!(status["provisional_approval_outstanding"], true, "{status}");
    assert!(stdout(repo.arc(&repo.root).args(["query", "--provisional"])).contains(&change_id));
}

/// An audit by the reviewer whose verdict is owed corroboration is not
/// corroboration either — the audit path carries the same rule as the review
/// path, rather than a weaker one nobody noticed.
#[test]
fn an_audit_by_the_provisional_reviewer_does_not_discharge() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "self-audited");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args([
            "review",
            "self-audited",
            "--verdict",
            "approved",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "self-audited"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "carol")
        .args(["audit", "self-audited", "--verdict", "approved"])
        .assert()
        .success();

    assert!(stdout(repo.arc(&repo.root).args(["query", "--provisional"])).contains(&change_id));
}

/// Corroboration is of one patchset. An approval of a different patchset is a
/// judgment about different code.
#[test]
fn an_approval_of_another_patchset_does_not_corroborate() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "other-patchset");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "dave")
        .args(["review", "other-patchset", "--verdict", "approved"])
        .assert()
        .success();
    repo.commit(&worktree, "more.txt", "more\n", "feat: more");
    stdout(repo.arc(&worktree).args(["snapshot", "other-patchset"]));
    repo.arc(&worktree)
        .env("ARC_ACTOR", "carol")
        .args([
            "review",
            "other-patchset",
            "--verdict",
            "approved",
            "--provisional",
            "unmeasured model",
        ])
        .assert()
        .success();

    // Dave approved ps-01; the provisional approval covers ps-02.
    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "other-patchset", "--json"]),
    );
    assert_eq!(status["provisional_approval_outstanding"], true, "{status}");
    assert!(stdout(repo.arc(&repo.root).args(["query", "--provisional"])).contains(&change_id));
}

#[test]
fn integrate_debt_records_missing_review_and_model_coverage() {
    let repo = repo_forbidding_self_approval();
    let worktree = snapshotted_change(&repo, "typed-coverage");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args([
            "--model",
            "gpt-5.6-luna#max",
            "review",
            "typed-coverage",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "integrate",
            "typed-coverage",
            "--debt",
            "reviewer unavailable",
        ])
        .assert()
        .success();

    let event: serde_json::Value = serde_json::from_str(
        stdout(repo.arc(&repo.root).args([
            "events",
            "--change",
            "typed-coverage",
            "--type",
            "debt-declared",
        ]))
        .trim(),
    )
    .unwrap();
    assert_eq!(event["missing"], "independent-review", "{event}");
    assert_eq!(event["coverage"][0]["reviewer"], "Solo", "{event}");
    assert_eq!(event["coverage"][0]["model"], "gpt-5.6-luna#max", "{event}");

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "typed-coverage", "--json"]),
    );
    assert_eq!(
        status["debt"]["coverage"][0]["model"], "gpt-5.6-luna#max",
        "{status}"
    );
    let human = stdout(repo.arc(&repo.root).args(["show", "typed-coverage"]));
    assert!(human.contains("Missing: independent-review"), "{human}");
    assert!(human.contains("Solo@max (gpt-5.6-luna#max)"), "{human}");
}

#[test]
fn debt_coverage_preserves_an_unrecorded_model() {
    let repo = repo_forbidding_self_approval();
    let worktree = snapshotted_change(&repo, "unrecorded-model");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args(["review", "unrecorded-model", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "integrate",
            "unrecorded-model",
            "--debt",
            "reviewer unavailable",
        ])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "unrecorded-model", "--json"]),
    );
    let coverage = status["debt"]["coverage"].as_array().unwrap();
    assert_eq!(coverage.len(), 1, "{status}");
    assert_eq!(coverage[0]["reviewer"], "Solo", "{status}");
    assert!(coverage[0].get("model").is_none(), "{status}");
    let human = stdout(repo.arc(&repo.root).args(["show", "unrecorded-model"]));
    assert!(human.contains("Solo (model unrecorded)"), "{human}");
    assert!(!human.contains("Coverage: none"), "{human}");
}

#[test]
fn debt_without_verdicts_has_empty_coverage() {
    let repo = repo_forbidding_self_approval();
    snapshotted_change(&repo, "empty-coverage");
    repo.arc(&repo.root)
        .args([
            "integrate",
            "empty-coverage",
            "--debt",
            "no reviewer reached the change",
        ])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "empty-coverage", "--json"]),
    );
    assert_eq!(status["debt"]["missing"], "nothing-read", "{status}");
    assert!(
        status["debt"]["coverage"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{status}"
    );
    let human = stdout(repo.arc(&repo.root).args(["show", "empty-coverage"]));
    assert!(human.contains("Coverage: none"), "{human}");
}

#[test]
fn a_later_audit_discharge_records_its_model_beside_the_debt() {
    let repo = repo_forbidding_self_approval();
    snapshotted_change(&repo, "model-discharge");
    repo.arc(&repo.root)
        .args([
            "integrate",
            "model-discharge",
            "--debt",
            "reviewer unavailable",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args([
            "--model",
            "gpt-5.6-auditor#high",
            "audit",
            "model-discharge",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "model-discharge", "--json"]),
    );
    assert_eq!(status["debt_outstanding"], false, "{status}");
    assert_eq!(
        status["debt"]["discharged_by"]["reviewer"], "Reviewer",
        "{status}"
    );
    assert_eq!(
        status["debt"]["discharged_by"]["model"], "gpt-5.6-auditor#high",
        "{status}"
    );
    assert_eq!(
        status["audit_verdicts"][0]["model"], "gpt-5.6-auditor#high",
        "{status}"
    );
    let human = stdout(repo.arc(&repo.root).args(["show", "model-discharge"]));
    assert!(
        human.contains("Discharged by: Reviewer@high (gpt-5.6-auditor#high)"),
        "{human}"
    );
}

#[test]
fn a_legacy_debt_event_still_discharge_and_render_without_typed_fields() {
    let repo = repo_forbidding_self_approval();
    let (change_id, _) = snapshotted_change_with_id(&repo, "legacy-debt");
    repo.arc(&repo.root)
        .args(["integrate", "legacy-debt", "--debt", "reviewer unavailable"])
        .assert()
        .success();

    rewrite_event(&repo, &change_id, "debt-declared", |event| {
        event["event_type"] = serde_json::json!("audit-debt-declared");
        event.as_object_mut().unwrap().remove("missing");
        event.as_object_mut().unwrap().remove("coverage");
    });

    let before = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "legacy-debt", "--json"]),
    );
    assert_eq!(before["debt_outstanding"], true, "{before}");
    assert!(before["debt"].get("missing").is_none(), "{before}");
    assert!(before["debt"].get("coverage").is_none(), "{before}");
    let human_before = stdout(repo.arc(&repo.root).args(["show", "legacy-debt"]));
    assert!(human_before.contains("Record: legacy"), "{human_before}");
    assert!(!human_before.contains("Missing:"), "{human_before}");
    assert!(!human_before.contains("Coverage:"), "{human_before}");

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args([
            "--model",
            "gpt-5.6-legacy-auditor",
            "audit",
            "legacy-debt",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();

    let after = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "legacy-debt", "--json"]),
    );
    assert_eq!(after["debt_outstanding"], false, "{after}");
    assert!(after["debt"].get("missing").is_none(), "{after}");
    assert!(after["debt"].get("coverage").is_none(), "{after}");
    assert_eq!(
        after["debt"]["discharged_by"]["model"], "gpt-5.6-legacy-auditor",
        "{after}"
    );
    let human_after = stdout(repo.arc(&repo.root).args(["show", "legacy-debt"]));
    assert!(human_after.contains("Record: legacy"), "{human_after}");
    assert!(
        human_after.contains("Discharged by: Reviewer (gpt-5.6-legacy-auditor)"),
        "{human_after}"
    );
}

/// A snapshotted change whose file is its own, so successive integrations into
/// one repository do not leave a later change with nothing to commit.
fn snapshotted_own_file(repo: &Repo, slug: &str) -> PathBuf {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        "feat: work",
    );
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    worktree
}

/// A patchset by `tester`, with a brief recorded first by `Planner`.
fn briefed_change(repo: &Repo, slug: &str) -> PathBuf {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let brief = repo.home.join(format!("{slug}-brief.md"));
    fs::write(&brief, "do the thing\n").unwrap();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Planner")
        .env("ARC_MODEL", "gpt-5.6-sol#high")
        .args([
            "brief",
            slug,
            "--title",
            "Contract",
            "--body-file",
            brief.to_str().unwrap(),
        ])
        .assert()
        .success();
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        "feat: work",
    );
    stdout(
        repo.arc(&worktree)
            .env("ARC_MODEL", "gpt-5.6-luna#max")
            .args(["snapshot", slug]),
    );
    worktree
}

/// A debt says which obligation it carries, and each shape of ledger names a
/// different one. One count over every debt cannot answer what any of them
/// owes, which is the question a reviewer picking the next one is asking.
#[test]
fn a_derived_debt_kind_follows_the_shape_of_the_ledger() {
    let repo = repo_forbidding_self_approval();

    snapshotted_own_file(&repo, "unread");
    repo.arc(&repo.root)
        .args(["integrate", "unread", "--debt", "nobody looked"])
        .assert()
        .success();

    let repaired = snapshotted_own_file(&repo, "repaired");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args(["review", "repaired", "--verdict", "approved"])
        .assert()
        .success();
    repo.commit(&repaired, "more.txt", "more\n", "fix: repair");
    stdout(repo.arc(&repaired).args(["snapshot", "repaired"]));
    repo.arc(&repo.root)
        .args([
            "integrate",
            "repaired",
            "--debt",
            "no reviewer for the repair",
        ])
        .assert()
        .success();

    let own = snapshotted_own_file(&repo, "own-read");
    repo.arc(&own)
        .args(["review", "own-read", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "own-read", "--debt", "only the author read it"])
        .assert()
        .success();

    snapshotted_own_file(&repo, "read-once");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args(["review", "read-once", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "read-once", "--debt", "a second pass is owed"])
        .assert()
        .success();

    for (slug, kind) in [
        ("unread", "nothing-read"),
        ("repaired", "repair-unread"),
        ("own-read", "contributor-only"),
        ("read-once", "independent-review"),
    ] {
        let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
        assert_eq!(status["debt"]["missing"], kind, "{slug}: {status}");
    }
}

/// Arc cannot tell a merge resolution from a repair — both are authored work on
/// top of an approved patchset — so the caller's kind has to win.
#[test]
fn a_declared_debt_kind_beats_the_derived_one() {
    let repo = repo_forbidding_self_approval();

    snapshotted_own_file(&repo, "resolved");
    repo.arc(&repo.root)
        .args([
            "integrate",
            "resolved",
            "--debt",
            "the resolution went unread",
            "--kind",
            "merge-resolution-unread",
        ])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "resolved", "--json"]));
    assert_eq!(
        status["debt"]["missing"], "merge-resolution-unread",
        "{status}"
    );

    snapshotted_own_file(&repo, "late");
    repo.arc(&repo.root)
        .args(["integrate", "late"])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args(["review", "late", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "late"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "debt",
            "late",
            "--reason",
            "the resolution went unread",
            "--kind",
            "merge-resolution-unread",
        ])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "late", "--json"]));
    assert_eq!(
        status["debt"]["missing"], "merge-resolution-unread",
        "{status}"
    );
}

/// The effort a routed identity names rides inside the model string, where
/// nothing can read it without splitting a token. Coverage reads it out and
/// keeps the string whole, so neither reading loses the other.
#[test]
fn coverage_reads_the_effort_and_keeps_the_model_string_whole() {
    let repo = repo_forbidding_self_approval();
    snapshotted_change(&repo, "effort");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args([
            "--model",
            "gpt-5.6-sol#low",
            "review",
            "effort",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "effort", "--debt", "a second pass is owed"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "effort", "--json"]));
    let coverage = &status["debt"]["coverage"][0];
    assert_eq!(coverage["model"], "gpt-5.6-sol#low", "{status}");
    assert_eq!(coverage["effort"], "low", "{status}");
    assert!(coverage.get("route_version").is_none(), "{status}");
    let human = stdout(repo.arc(&repo.root).args(["show", "effort"]));
    assert!(human.contains("Reviewer@low (gpt-5.6-sol#low)"), "{human}");
}

/// A verdict says who read the work; the route version says which roster
/// produced them. Arc records it and joins it against nothing, and an absent
/// one means unrouted rather than unknown-and-guessable.
#[test]
fn a_route_version_reaches_coverage_from_review_and_from_audit() {
    let repo = repo_forbidding_self_approval();
    snapshotted_change(&repo, "routed");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args([
            "--model",
            "gpt-5.6-sol#low",
            "review",
            "routed",
            "--verdict",
            "approved",
            "--route-version",
            "2026.09",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "routed", "--debt", "a second pass is owed"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "routed", "--json"]));
    assert_eq!(status["debt"]["coverage"][0]["route_version"], "2026.09");
    let human = stdout(repo.arc(&repo.root).args(["show", "routed"]));
    assert!(
        human.contains("Reviewer@low [route 2026.09] (gpt-5.6-sol#low)"),
        "{human}"
    );

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Auditor")
        .args([
            "--model",
            "gpt-5.6-terra#high",
            "audit",
            "routed",
            "--verdict",
            "approved",
            "--route-version",
            "2026.10",
        ])
        .assert()
        .success();
    let after = json_stdout(repo.arc(&repo.root).args(["status", "routed", "--json"]));
    assert_eq!(after["debt"]["discharged_by"]["route_version"], "2026.10");
    assert_eq!(after["debt"]["discharged_by"]["effort"], "high");
}

/// Who set the contract and who answered it are separate facts, and a reader
/// weighing a debt needs both. Nothing here ranks either identity.
#[test]
fn production_names_the_planner_and_the_implementer() {
    let repo = repo_forbidding_self_approval();
    briefed_change(&repo, "briefed");
    repo.arc(&repo.root)
        .args(["integrate", "briefed", "--debt", "nobody looked"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "briefed", "--json"]));
    let production = &status["debt"]["production"];
    assert_eq!(production["planner"]["actor"], "Planner", "{status}");
    assert_eq!(
        production["planner"]["model"], "gpt-5.6-sol#high",
        "{status}"
    );
    assert_eq!(production["planner"]["effort"], "high", "{status}");
    assert_eq!(production["brief_version"], 1, "{status}");
    assert_eq!(production["implementer"]["actor"], "tester", "{status}");
    assert_eq!(production["implementer"]["effort"], "max", "{status}");
    assert_eq!(production["following_brief"], true, "{status}");
    let human = stdout(repo.arc(&repo.root).args(["show", "briefed"]));
    assert!(
        human.contains("Produced: planned by Planner@high (brief v1), implemented by tester@max"),
        "{human}"
    );

    snapshotted_own_file(&repo, "unbriefed");
    repo.arc(&repo.root)
        .args(["integrate", "unbriefed", "--debt", "nobody looked"])
        .assert()
        .success();
    let plain = json_stdout(repo.arc(&repo.root).args(["status", "unbriefed", "--json"]));
    let plain_production = &plain["debt"]["production"];
    assert!(plain_production.get("planner").is_none(), "{plain}");
    assert!(plain_production.get("brief_version").is_none(), "{plain}");
    assert_eq!(
        plain_production["implementer"]["actor"], "tester",
        "{plain}"
    );
    assert_eq!(plain_production["following_brief"], false, "{plain}");
}

/// A planner who implements their own brief is following nobody's contract but
/// their own, and recording otherwise would invent a delegation that never
/// happened.
#[test]
fn following_brief_is_false_when_the_brief_author_implemented() {
    let repo = repo_forbidding_self_approval();
    stdout(repo.arc(&repo.root).args(["begin", "solo-brief"]));
    let brief = repo.home.join("solo-brief.md");
    fs::write(&brief, "do the thing\n").unwrap();
    repo.arc(&repo.root)
        .args([
            "brief",
            "solo-brief",
            "--title",
            "Contract",
            "--body-file",
            brief.to_str().unwrap(),
        ])
        .assert()
        .success();
    let worktree = repo.home.join(".worktrees").join("repo-solo-brief");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(repo.arc(&worktree).args(["snapshot", "solo-brief"]));
    repo.arc(&repo.root)
        .args(["integrate", "solo-brief", "--debt", "nobody looked"])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "solo-brief", "--json"]),
    );
    let production = &status["debt"]["production"];
    assert_eq!(production["planner"]["actor"], "tester", "{status}");
    assert_eq!(production["following_brief"], false, "{status}");
}

/// An obligation recorded before production was kept says nothing about how the
/// work was produced, and reads as the independent review it always meant.
#[test]
fn a_debt_recorded_before_production_replays_without_inventing_one() {
    let repo = repo_forbidding_self_approval();
    let (change_id, _) = snapshotted_change_with_id(&repo, "pre-production");
    repo.arc(&repo.root)
        .args(["integrate", "pre-production", "--debt", "nobody looked"])
        .assert()
        .success();

    rewrite_event(&repo, &change_id, "debt-declared", |event| {
        event["missing"] = serde_json::json!("independent-review");
        event.as_object_mut().unwrap().remove("production");
    });

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "pre-production", "--json"]),
    );
    assert_eq!(status["debt"]["missing"], "independent-review", "{status}");
    assert!(status["debt"].get("production").is_none(), "{status}");
    let human = stdout(repo.arc(&repo.root).args(["show", "pre-production"]));
    assert!(human.contains("Missing: independent-review"), "{human}");
    assert!(!human.contains("Produced:"), "{human}");
}

/// A queue reporting one total says how much is owed and nothing about what any
/// of it owes, which is exactly what decides which obligation to take next.
#[test]
fn the_inbox_and_catchup_split_their_debt_count_by_kind() {
    let repo = repo_forbidding_self_approval();
    snapshotted_own_file(&repo, "none-read");
    repo.arc(&repo.root)
        .args(["integrate", "none-read", "--debt", "nobody looked"])
        .assert()
        .success();
    let own = snapshotted_own_file(&repo, "self-read");
    repo.arc(&own)
        .args(["review", "self-read", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "integrate",
            "self-read",
            "--debt",
            "only the author read it",
        ])
        .assert()
        .success();

    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    let split = inbox["debt-owed-by-kind"].as_array().unwrap();
    assert_eq!(split.len(), 2, "{inbox}");
    assert_eq!(split[0]["kind"], "nothing-read", "{inbox}");
    assert_eq!(split[0]["count"], 1, "{inbox}");
    assert_eq!(split[1]["kind"], "contributor-only", "{inbox}");
    assert_eq!(split[1]["count"], 1, "{inbox}");

    let text = stdout(repo.arc(&repo.root).args(["inbox"]));
    assert!(
        text.contains("by kind: nothing-read 1, contributor-only 1"),
        "{text}"
    );
    assert!(
        text.contains("debt: nothing-read; implemented by tester"),
        "{text}"
    );

    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(
        catchup.contains("2 outstanding (nothing-read 1, contributor-only 1)"),
        "{catchup}"
    );
}
