use crate::common::*;

#[test]
fn stats_json_carries_schema_and_reports_selected_change() {
    let repo = Repo::new();
    let (_id, wt, _head) = change_with_patchset(&repo, "feat-x");
    repo.arc(&wt)
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "feat-x"])
        .assert()
        .success();

    let report = json_stdout(repo.arc(&repo.root).args(["stats", "--all", "--json"]));
    assert_eq!(report["schema"], "arc-stats/1");

    let changes = report["changes"].as_array().unwrap();
    let feat = changes
        .iter()
        .find(|change| change["slug"] == "feat-x")
        .expect("completed change should appear in stats");
    assert_eq!(feat["state"], "closed");
    // An integrated change has a measured open→integrated wall time.
    assert!(feat["wall_time_seconds"].is_number());
    assert_eq!(feat["patchset_count"], 1);
    assert!(report["aggregate"]["changes"].as_u64().unwrap() >= 1);
}

#[test]
fn rework_requires_changes_requested_then_new_patchset_then_approval() {
    let repo = Repo::new();

    let (_, first_pass, _) = change_with_patchset(&repo, "first-pass");
    repo.arc(&first_pass)
        .args(["review", "first-pass", "--verdict", "approved"])
        .assert()
        .success();

    let (_, reversal, _) = change_with_patchset(&repo, "same-patchset-reversal");
    repo.arc(&reversal)
        .args([
            "review",
            "same-patchset-reversal",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();
    repo.arc(&reversal)
        .args(["review", "same-patchset-reversal", "--verdict", "approved"])
        .assert()
        .success();

    let (_, reworked, _) = change_with_patchset(&repo, "two-rounds");
    repo.arc(&reworked)
        .args([
            "review",
            "two-rounds",
            "--verdict",
            "changes-requested",
            "--cause",
            "brief",
        ])
        .assert()
        .success();
    repo.commit(&reworked, "round-2.txt", "two\n", "fix: address round one");
    repo.arc(&reworked)
        .args(["snapshot", "two-rounds"])
        .assert()
        .success();
    repo.arc(&reworked)
        .args([
            "review",
            "two-rounds",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();
    repo.commit(
        &reworked,
        "round-3.txt",
        "three\n",
        "fix: address round two",
    );
    repo.arc(&reworked)
        .args(["snapshot", "two-rounds"])
        .assert()
        .success();
    repo.arc(&reworked)
        .args(["review", "two-rounds", "--verdict", "approved"])
        .assert()
        .success();

    let report = json_stdout(repo.arc(&repo.root).args(["stats", "--all", "--json"]));
    let changes = report["changes"].as_array().unwrap();
    let by_slug = |slug| {
        changes
            .iter()
            .find(|change| change["slug"] == slug)
            .unwrap()
    };

    assert_eq!(by_slug("first-pass")["changes_requested_rounds"], 0);
    assert_eq!(by_slug("first-pass")["completed_rework_rounds"], 0);
    assert_eq!(by_slug("first-pass")["reworked"], false);
    assert_eq!(by_slug("first-pass")["first_pass_approval"], true);

    assert_eq!(
        by_slug("same-patchset-reversal")["changes_requested_rounds"],
        1
    );
    assert_eq!(
        by_slug("same-patchset-reversal")["completed_rework_rounds"],
        0
    );
    assert_eq!(by_slug("same-patchset-reversal")["reworked"], false);
    assert_eq!(
        by_slug("same-patchset-reversal")["first_pass_approval"],
        false
    );

    assert_eq!(by_slug("two-rounds")["changes_requested_rounds"], 2);
    assert_eq!(by_slug("two-rounds")["completed_rework_rounds"], 2);
    assert_eq!(by_slug("two-rounds")["reworked"], true);
    assert_eq!(by_slug("two-rounds")["first_pass_approval"], false);

    assert_eq!(report["aggregate"]["changes_reworked"], 1);
    assert_eq!(report["aggregate"]["first_pass_approvals"], 1);
    assert_eq!(report["aggregate"]["completed_rework_rounds"], 2);
}

/// Reviewers add feedback in more than one sitting, so a patchset can collect
/// several changes-requested verdicts before the author answers. One revision
/// answers them all, so they are one round — counting verdict events instead
/// would inflate every rework figure a lead reads to judge delegation.
#[test]
fn several_changes_requested_on_one_patchset_are_one_round() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "piled-up");
    // The same cause twice, plus a second cause on the later verdict: the
    // repeated one must collapse to its single round while the other still
    // registers, so a per-verdict tally cannot pass this.
    repo.arc(&worktree)
        .args([
            "review",
            "piled-up",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "review",
            "piled-up",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
            "--cause",
            "brief",
        ])
        .assert()
        .success();
    repo.commit(&worktree, "answer.txt", "one\n", "fix: answer both rounds");
    repo.arc(&worktree)
        .args(["snapshot", "piled-up"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "piled-up", "--verdict", "approved"])
        .assert()
        .success();

    let report = json_stdout(repo.arc(&repo.root).args(["stats", "--all", "--json"]));
    let change = report["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["slug"] == "piled-up")
        .unwrap();
    assert_eq!(change["changes_requested_rounds"], 1);
    assert_eq!(change["completed_rework_rounds"], 1);
    assert_eq!(change["reworked"], true);
    assert_eq!(change["first_pass_approval"], false);
    // Causes are attributed to the round, so the repeated one counts once and
    // no cause tally can exceed the round count it explains.
    assert_eq!(change["review_rounds_by_cause"]["executor"], 1);
    assert_eq!(change["review_rounds_by_cause"]["brief"], 1);
    assert_eq!(report["aggregate"]["completed_rework_rounds"], 1);
    assert_eq!(report["aggregate"]["review_rounds_by_cause"]["executor"], 1);
}

/// `arc stats` knows a change took six rework rounds and not who caused them.
/// The identity is already on the ledger — a lead runs the ceremony on an
/// executor's behalf — so the rows are keyed on the subject, never the actor.
#[test]
fn stats_by_model_attributes_patchsets_and_rework_to_the_subject() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "delegated"]));
    let wt = repo.home.join(".worktrees/repo-delegated");

    // Round one: the executor's work is sent back.
    repo.commit(&wt, "one.rs", "first\n", "feat: first");
    repo.arc(&wt)
        .args(["snapshot", "delegated", "--on-behalf-of", "sol#high"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "review",
            "delegated",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
            "--on-behalf-of",
            "reviewer-model",
        ])
        .assert()
        .success();

    // Round two: a different identity writes the revision that answers it.
    repo.commit(&wt, "two.rs", "second\n", "feat: second");
    repo.arc(&wt)
        .args(["snapshot", "delegated", "--on-behalf-of", "terra#high"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "review",
            "delegated",
            "--verdict",
            "approved",
            "--on-behalf-of",
            "reviewer-model",
        ])
        .assert()
        .success();

    // A patchset nobody delegated for: counted apart, not credited to the lead.
    repo.commit(&wt, "three.rs", "third\n", "feat: third");
    repo.arc(&wt)
        .args(["snapshot", "delegated"])
        .assert()
        .success();

    let report = json_stdout(repo.arc(&wt).args(["stats", "--by-model", "--json"]));
    assert_eq!(report["schema"], "arc-stats-by-model/1");
    let rows = report["models"].as_array().unwrap();
    let row = |identity: &str| {
        rows.iter()
            .find(|row| row["identity"] == identity)
            .unwrap_or_else(|| panic!("no row for {identity}: {report}"))
            .clone()
    };

    // The round is charged to the work that was sent back, not to the
    // revision that answered it.
    let executor = row("sol#high");
    assert_eq!(executor["patchsets"], 1, "{report}");
    assert_eq!(executor["rework_rounds_caused"], 1, "{report}");
    assert_eq!(executor["verdicts"], 0, "{report}");
    assert_eq!(executor["changes"], 1, "{report}");

    let fixer = row("terra#high");
    assert_eq!(fixer["patchsets"], 1, "{report}");
    assert_eq!(fixer["rework_rounds_caused"], 0, "{report}");

    let reviewer = row("reviewer-model");
    assert_eq!(reviewer["verdicts"], 2, "{report}");
    assert_eq!(reviewer["patchsets"], 0, "{report}");
    assert_eq!(reviewer["rework_rounds_caused"], 0, "{report}");

    let unknown = row("(unattributed)");
    assert_eq!(unknown["patchsets"], 1, "{report}");

    // An identity that only filed a finding still has a row.
    repo.arc(&wt)
        .args([
            "finding",
            "delegated",
            "--summary",
            "spotted",
            "--on-behalf-of",
            "finder-model",
        ])
        .assert()
        .success();
    let report = json_stdout(repo.arc(&wt).args(["stats", "--by-model", "--json"]));
    let rows = report["models"].as_array().unwrap();
    let finder = rows
        .iter()
        .find(|row| row["identity"] == "finder-model")
        .unwrap_or_else(|| panic!("no row for finder-model: {report}"));
    assert_eq!(finder["changes"], 1, "{report}");
    assert_eq!(finder["patchsets"], 0, "{report}");

    let text = stdout(repo.arc(&wt).args(["stats", "--by-model"]));
    assert!(text.contains("sol#high"), "{text}");
    assert!(text.contains("(unattributed)"), "{text}");

    // An audit is a review that happened, and counts as one.
    repo.arc(&wt)
        .args([
            "review",
            "delegated",
            "--verdict",
            "approved",
            "--on-behalf-of",
            "reviewer-model",
        ])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["integrate", "delegated", "--debt", "nobody reachable"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "audit",
            "delegated",
            "--verdict",
            "approved",
            "--actor",
            "auditor",
            "--on-behalf-of",
            "auditor-model",
        ])
        .assert()
        .success();
    let report = json_stdout(repo.arc(&wt).args(["stats", "--by-model", "--json"]));
    let rows = report["models"].as_array().unwrap();
    let auditor = rows
        .iter()
        .find(|row| row["identity"] == "auditor-model")
        .unwrap_or_else(|| panic!("no row for auditor-model: {report}"));
    assert_eq!(auditor["verdicts"], 1, "{report}");

    // A selection and --all cannot both be asked for, however --change is
    // spelled.
    repo.arc(&wt)
        .args(["--change", "delegated", "stats", "--by-model", "--all"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be combined"));
}

/// Write one journal artifact of `kind` and return the filename it got.
fn journal_artifact(repo: &Repo, topic: &str, kind: &str) -> String {
    let out = stdout(
        repo.arc(&repo.root)
            .args(["journal", "note", topic, "--kind", kind, "--body-file", "-"])
            .write_stdin(format!("# {topic}\n")),
    );
    Path::new(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string()
}

fn keep(repo: &Repo, slug: &str, kind: &str, cites: &[&str]) -> String {
    let mut command = repo.arc(&repo.root);
    command.args(["keep", slug, "--kind", kind, "--body", "fixture fact"]);
    for cited in cites {
        command.args(["--cites", cited]);
    }
    let output = command.assert().success().get_output().stdout.clone();
    String::from_utf8(output)
        .unwrap()
        .split_whitespace()
        .last()
        .unwrap()
        .to_string()
}

fn verify_command(repo: &Repo, slug: &str, command: &str, extra: &[&str]) -> bool {
    repo.arc(&repo.root)
        .args(["verify", slug, "--command", command])
        .args(extra)
        .output()
        .unwrap()
        .status
        .success()
}

fn newest_failure(repo: &Repo, slug: &str) -> String {
    stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        slug,
        "--type",
        "verification-recorded",
    ]))
    .lines()
    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
    .rfind(|event| event["result"] == "fail")
    .unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn integrate(repo: &Repo, slug: &str) {
    repo.arc(&repo.root)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", slug])
        .assert()
        .success();
}

fn ratio(count: u64, of: u64) -> serde_json::Value {
    serde_json::json!({ "count": count, "of": of })
}

#[test]
fn provenance_counts_every_class_on_one_fixture() {
    let repo = Repo::new();
    let opening = journal_artifact(&repo, "opening", "todo");
    let plan = journal_artifact(&repo, "slicing", "plan");

    // alpha: opened from a journal artifact, its brief revised to name a
    // plan; an inferred pass, a pass with no prior failure, a patchset linked
    // by default, and a rejected fact that a later fact cites.
    repo.arc(&repo.root)
        .args(["begin", "alpha", "--from-journal", &opening])
        .assert()
        .success();
    let alpha = repo.home.join(".worktrees/repo-alpha");
    repo.arc(&repo.root)
        .args([
            "brief",
            "alpha",
            "--cause-note",
            "fixture revision",
            "--plan-ref",
            &plan,
            "--plan-slice",
            "slice",
            "--body-file",
            "-",
        ])
        .write_stdin("# Contract\n")
        .assert()
        .success();
    assert!(!verify_command(&repo, "alpha", "test -f marker", &[]));
    repo.commit(&alpha, "marker", "", "test: add marker");
    assert!(verify_command(&repo, "alpha", "test -f marker", &[]));
    assert!(verify_command(&repo, "alpha", "true", &[]));
    stdout(repo.arc(&alpha).args(["snapshot", "alpha"]));
    let rejected = keep(&repo, "alpha", "rejected", &[]);
    keep(&repo, "alpha", "verified", &[&rejected]);
    integrate(&repo, "alpha");

    // beta: opened bare, with a brief naming no plan; a declared pass, a pass
    // recorded before arc derived the inference, an unlinked patchset, and a
    // flagged one whose second link predates `via`.
    let beta_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "beta"])));
    let beta = repo.home.join(".worktrees/repo-beta");
    repo.arc(&repo.root)
        .args(["brief", "beta", "--body-file", "-"])
        .write_stdin("# Contract\n")
        .assert()
        .success();
    repo.commit(&beta, "beta.txt", "beta\n", "test: beta");
    stdout(repo.arc(&beta).args(["snapshot", "beta"]));
    assert!(!verify_command(&repo, "beta", "test -f declared", &[]));
    let failure = newest_failure(&repo, "beta");
    repo.commit(&beta, "declared", "", "test: add declared");
    assert!(verify_command(
        &repo,
        "beta",
        "test -f declared",
        &["--falsified-by", &failure, "--predicted", "file absent"],
    ));
    assert!(!verify_command(&repo, "beta", "test -f legacy", &[]));
    repo.commit(&beta, "legacy", "", "test: add legacy");
    assert!(verify_command(&repo, "beta", "test -f legacy", &[]));
    rewrite_event(&repo, &beta_id, "verification-recorded", |event| {
        assert!(event["falsification_inferred"].is_object(), "{event}");
        event
            .as_object_mut()
            .unwrap()
            .remove("falsification_inferred");
    });
    stdout(repo.arc(&beta).args([
        "snapshot",
        "beta",
        "--journal-ref",
        &opening,
        "--journal-ref",
        &plan,
    ]));
    rewrite_event(&repo, &beta_id, "patchset-added", |event| {
        assert_eq!(event["journal_refs"][1]["via"], "flag", "{event}");
        event["journal_refs"][1]
            .as_object_mut()
            .unwrap()
            .remove("via");
    });
    keep(&repo, "beta", "constraint", &[]);
    integrate(&repo, "beta");

    // gamma: open, briefless, holding a rejected fact that no integration
    // counts.
    repo.arc(&repo.root)
        .args(["begin", "gamma"])
        .assert()
        .success();
    keep(&repo, "gamma", "rejected", &[]);

    let report = json_stdout(
        repo.arc(&repo.root)
            .args(["stats", "--provenance", "--json"]),
    );
    assert_eq!(report["schema"], "arc-stats-provenance/1", "{report}");
    assert_eq!(report["changes"], 3, "{report}");

    let falsification = &report["falsification"];
    assert_eq!(falsification["declared"], ratio(1, 4), "{report}");
    assert_eq!(falsification["inferred"], ratio(1, 4), "{report}");
    assert_eq!(falsification["none"], ratio(2, 4), "{report}");
    assert_eq!(falsification["none_after_failure"], ratio(1, 2), "{report}");

    let refs = &report["journal_refs"];
    assert_eq!(refs["patchsets_with_refs"], ratio(2, 3), "{report}");
    assert_eq!(refs["opened_from_journal"], ratio(1, 1), "{report}");
    assert_eq!(refs["opened_without_journal"], ratio(1, 2), "{report}");
    for via in ["begin", "brief", "flag", "unrecorded"] {
        assert_eq!(refs["via"][via], ratio(1, 4), "{via}: {report}");
    }

    assert_eq!(report["rejected_alternatives"], ratio(1, 2), "{report}");
    assert_eq!(
        report["briefs"]["versions_with_plan_ref"],
        ratio(1, 3),
        "{report}"
    );
    assert_eq!(
        report["briefs"]["changes_with_plan_ref"],
        ratio(1, 3),
        "{report}"
    );
    assert_eq!(report["cited_kept_facts"], ratio(1, 4), "{report}");

    // Text is one line per class.
    let text = stdout(repo.arc(&repo.root).args(["stats", "--provenance"]));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "{text}");
    for (line, class) in lines.iter().zip([
        "falsification:",
        "journal refs:",
        "rejected alternatives:",
        "plan-linked briefs:",
        "cited kept facts:",
    ]) {
        assert!(line.starts_with(class), "{text}");
    }
    assert!(lines[4].contains("1 of 4"), "{text}");
}

#[test]
fn provenance_respects_change_and_tag_selection() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "cited"])
        .assert()
        .success();
    let grounds = keep(&repo, "cited", "constraint", &[]);
    keep(&repo, "cited", "verified", &[&grounds]);
    repo.arc(&repo.root)
        .args(["begin", "tagged", "--tag", "lane-h"])
        .assert()
        .success();
    keep(&repo, "tagged", "hypothesis", &[]);

    let facts = |selection: &[&str]| {
        let report = json_stdout(
            repo.arc(&repo.root)
                .args(["stats", "--provenance", "--json"])
                .args(selection),
        );
        (
            report["changes"].clone(),
            report["cited_kept_facts"].clone(),
        )
    };
    assert_eq!(facts(&[]), (2.into(), ratio(1, 3)));
    assert_eq!(facts(&["--all"]), (2.into(), ratio(1, 3)));
    assert_eq!(facts(&["--change", "cited"]), (1.into(), ratio(1, 2)));
    assert_eq!(facts(&["--tag", "lane-h"]), (1.into(), ratio(0, 1)));
    assert_eq!(facts(&["--tag", "absent"]), (0.into(), ratio(0, 0)));
}

#[test]
fn provenance_and_by_model_are_exclusive() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["stats", "--provenance", "--by-model"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be used with"));
    repo.arc(&repo.root)
        .args(["stats", "--provenance", "--by-model", "--json"])
        .assert()
        .failure()
        .stdout(predicates::str::is_empty());
}
