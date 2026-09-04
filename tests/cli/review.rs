use super::common::*;

fn disposition_change(repo: &Repo, slug: &str) -> String {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add verification gate"]);
    opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        slug,
        "--no-worktree",
    ])))
}

fn finding_target(repo: &Repo, slug: &str) -> (String, String) {
    let output =
        stdout(
            repo.arc(&repo.root)
                .args(["finding", slug, "--summary", "the recorded finding"]),
        );
    let finding_id = output
        .lines()
        .find_map(|line| line.strip_prefix("finding: "))
        .unwrap()
        .to_string();
    let finding_event_id = output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();
    (finding_id, finding_event_id)
}

fn verification_event_id(repo: &Repo, slug: &str) -> String {
    repo.arc(&repo.root)
        .args(["verify", slug, "--gate", "unit"])
        .assert()
        .success();
    let event = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        slug,
        "--type",
        "verification-recorded",
    ]))
    .lines()
    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
    .next()
    .unwrap();
    event["event_id"].as_str().unwrap().to_string()
}

#[test]
fn disposition_evidence_event_round_trips_through_replay_and_timeline() {
    let repo = Repo::new();
    let slug = "disposition-evidence";
    disposition_change(&repo, slug);
    let (finding_id, _) = finding_target(&repo, slug);
    let evidence_event_id = verification_event_id(&repo, slug);

    repo.arc(&repo.root)
        .args([
            "resolve",
            slug,
            &finding_id,
            "--status",
            "resolved",
            "--evidence",
            "the passing gate supports this disposition",
            "--evidence-event",
            &evidence_event_id,
        ])
        .assert()
        .success();

    let state = json_stdout(repo.arc(&repo.root).args(["show", slug, "--json"]));
    assert_eq!(
        state["findings"][finding_id.as_str()]["dispositions"][0]["evidence_event_id"],
        evidence_event_id
    );
    assert_eq!(
        state["findings"][finding_id.as_str()]["dispositions"][0]["evidence"],
        "the passing gate supports this disposition"
    );

    let timeline = stdout(repo.arc(&repo.root).args(["log", slug]));
    assert!(
        timeline.contains(&format!("evidence event {evidence_event_id}")),
        "{timeline}"
    );
}

#[test]
fn disposition_refuses_a_nonverification_evidence_event() {
    let repo = Repo::new();
    let slug = "disposition-bad-kind";
    let change_id = disposition_change(&repo, slug);
    let (finding_id, finding_event_id) = finding_target(&repo, slug);
    let before = event_count(&repo, &change_id);

    repo.arc(&repo.root)
        .args([
            "resolve",
            slug,
            &finding_id,
            "--status",
            "resolved",
            "--evidence-event",
            &finding_event_id,
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("finding-added"))
        .stderr(predicates::str::contains("not a verification"));

    assert_eq!(event_count(&repo, &change_id), before);
}

#[test]
fn disposition_refuses_an_evidence_event_absent_from_the_change() {
    let repo = Repo::new();
    let slug = "disposition-missing-event";
    let change_id = disposition_change(&repo, slug);
    let (finding_id, _) = finding_target(&repo, slug);
    let before = event_count(&repo, &change_id);
    let absent_event_id = "01J00000000000000000000000";

    repo.arc(&repo.root)
        .args([
            "resolve",
            slug,
            &finding_id,
            "--status",
            "resolved",
            "--evidence-event",
            absent_event_id,
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no event"))
        .stderr(predicates::str::contains("on change"))
        .stderr(predicates::str::contains("--evidence-event"));

    assert_eq!(event_count(&repo, &change_id), before);
}

#[test]
fn read_view_prints_verdict_history_and_body() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "review-read");
    repo.arc(&worktree)
        .args([
            "review",
            "review-read",
            "--verdict",
            "approved",
            "--body",
            "The implementation is sound.",
        ])
        .assert()
        .success();

    repo.arc(&worktree)
        .args(["review", "review-read"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Approved on `ps-01`"))
        .stdout(predicates::str::contains("The implementation is sound."));
}

#[test]
fn read_view_plainly_reports_no_verdict() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "review-empty");

    repo.arc(&worktree)
        .args(["review", "review-empty"])
        .assert()
        .success()
        .stdout(predicates::str::contains("No verdicts recorded."))
        .stdout(predicates::str::contains(
            "Valid approval for current head: no",
        ));
}

#[test]
fn read_view_marks_approval_stale_after_new_snapshot() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "review-stale");
    repo.arc(&worktree)
        .args(["review", "review-stale", "--verdict", "approved"])
        .assert()
        .success();
    repo.commit(
        &worktree,
        "later.txt",
        "later\n",
        "test: add later revision",
    );
    stdout(repo.arc(&worktree).args(["snapshot", "review-stale"]));

    repo.arc(&worktree)
        .args(["review", "review-stale"])
        .assert()
        .success()
        .stdout(predicates::str::contains("STALE for current head"));
}

#[test]
fn read_view_json_has_versioned_schema() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "review-json");
    let output = repo
        .arc(&worktree)
        .args(["review", "review-json", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["schema"], "arc-review/2");
}

#[test]
fn review_write_path_still_records_a_verdict() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "review-write");

    repo.arc(&worktree)
        .args(["review", "review-write", "--verdict", "approved"])
        .assert()
        .success()
        .stdout(predicates::str::contains("verdict: Approved on ps-01"));
}

#[test]
fn changes_requested_requires_typed_causes_and_stats_tallies_them() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "review-causes");
    let before = event_count(&repo, &change_id);

    repo.arc(&worktree)
        .args(["review", "review-causes", "--verdict", "changes-requested"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--cause"));
    assert_eq!(event_count(&repo, &change_id), before);

    repo.arc(&worktree)
        .args([
            "review",
            "review-causes",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
            "--cause",
            "brief",
            "--cause",
            "executor",
        ])
        .assert()
        .success();
    assert_eq!(event_count(&repo, &change_id), before + 1);

    let review = json_stdout(
        repo.arc(&worktree)
            .args(["review", "review-causes", "--json"]),
    );
    assert_eq!(
        review["verdicts"][0]["causes"],
        serde_json::json!(["brief", "executor"])
    );

    let stats =
        json_stdout(
            repo.arc(&repo.root)
                .args(["stats", "--change", "review-causes", "--json"]),
        );
    assert_eq!(
        stats["changes"][0]["review_rounds_by_cause"],
        serde_json::json!({"brief": 1, "executor": 1})
    );
    assert_eq!(
        stats["aggregate"]["review_rounds_by_cause"],
        serde_json::json!({"brief": 1, "executor": 1})
    );

    for verdict in ["approved", "comment-only"] {
        repo.arc(&worktree)
            .args([
                "review",
                "review-causes",
                "--verdict",
                verdict,
                "--cause",
                "brief",
            ])
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "--cause is only valid with --verdict changes-requested",
            ));
    }
    assert_eq!(event_count(&repo, &change_id), before + 1);
}

/// A reviewer reports on a revision, not on arc's patchset numbering. Making
/// the lead translate by hand is where a verdict gets bound to work nobody
/// reviewed, so a revision names its patchset directly.
#[test]
fn a_verdict_can_name_the_revision_that_was_reviewed() {
    let repo = Repo::new();
    let (_, worktree, first_head) = change_with_patchset(&repo, "review-by-revision");

    // A second patchset lands before the verdict for the first is recorded.
    repo.commit(&worktree, "later.txt", "later\n", "feat: later");
    repo.arc(&worktree)
        .args(["snapshot", "review-by-revision"])
        .assert()
        .success();

    repo.arc(&worktree)
        .args([
            "review",
            "review-by-revision",
            "--verdict",
            "approved",
            "--patchset",
            &first_head[..8],
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("ps-01"));

    // Recorded against what was read, so the newer patchset is still unapproved.
    let status = json_stdout(repo.arc(&worktree).args(["status", "review-by-revision"]));
    assert_eq!(status["verdict"]["patchset_id"], "ps-01");
    assert!(!status["verdict"]["valid_for_current_head"]
        .as_bool()
        .unwrap());
}

/// An unknown revision is refused rather than silently falling back to the
/// latest, which is the failure this flag exists to prevent.
#[test]
fn an_unknown_revision_is_refused_not_defaulted() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "review-bad-revision");
    repo.arc(&worktree)
        .args([
            "review",
            "review-bad-revision",
            "--verdict",
            "approved",
            "--patchset",
            "deadbeef",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "no patchset has that id or revision",
        ));
}

#[test]
fn stacked_base_floor() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "first"]));
    let first_worktree = repo.home.join(".worktrees/repo-first");
    repo.commit(
        &first_worktree,
        "predecessor.txt",
        "predecessor\n",
        "test: add predecessor",
    );
    let predecessor_head = repo.head(&first_worktree);

    stdout(
        repo.arc(&repo.root)
            .args(["begin", "second", "--base", "arc/first"]),
    );
    let second_worktree = repo.home.join(".worktrees/repo-second");
    repo.commit(
        &second_worktree,
        "member.txt",
        "member\n",
        "test: add member",
    );
    stdout(repo.arc(&second_worktree).args(["snapshot", "second"]));

    let state: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&second_worktree)
            .args(["show", "second", "--json"]),
    ))
    .unwrap();
    let patchset = state["patchsets"].as_array().unwrap().last().unwrap();
    assert_eq!(patchset["base"], predecessor_head);
    repo.arc(&second_worktree)
        .args(["diff", "second", "--stat"])
        .assert()
        .success()
        .stdout(predicates::str::contains("member.txt"))
        .stdout(predicates::str::contains("predecessor.txt").not());

    stdout(repo.arc(&repo.root).args(["begin", "ordinary"]));
    let ordinary_worktree = repo.home.join(".worktrees/repo-ordinary");
    repo.commit(
        &ordinary_worktree,
        "ordinary.txt",
        "ordinary\n",
        "test: add ordinary change",
    );
    stdout(repo.arc(&ordinary_worktree).args(["snapshot", "ordinary"]));

    let ordinary_state: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&ordinary_worktree)
            .args(["show", "ordinary", "--json"]),
    ))
    .unwrap();
    let ordinary_patchset = ordinary_state["patchsets"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    let ordinary_merge_base = git_out(&ordinary_worktree, &["merge-base", "HEAD", "master"]);
    assert_eq!(ordinary_patchset["base"], ordinary_merge_base);
}

/// An approval the gate lets stand still has to say what it is. Where the
/// reviewer is the identity that wrote the patchset — or one arc invented from
/// git config — the verdict is a review that happened, and naming the match at
/// the moment it is written is the only place a reader is guaranteed to see it.
#[test]
fn a_verdict_from_the_assumed_author_names_the_patchset_it_wrote() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "assumed-verdict"]));
    let worktree = repo.home.join(".worktrees").join("repo-assumed-verdict");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(
        repo.arc(&worktree)
            .env_remove("ARC_ACTOR")
            .args(["snapshot", "assumed-verdict"]),
    );
    let reviewed = repo
        .arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .args(["review", "assumed-verdict", "--verdict", "approved"])
        .assert()
        .success();
    let warning = String::from_utf8_lossy(&reviewed.get_output().stderr).into_owned();
    assert!(warning.contains("recorded as \"Tester\""), "{warning}");
    assert!(warning.contains("who is the author of ps-01"), "{warning}");
    assert!(
        warning.contains("identity assumed from git config"),
        "{warning}"
    );
}

/// The distinction the warning draws is between a reviewer and an author, so a
/// declared reviewer of work somebody else snapshotted hears nothing.
#[test]
fn a_verdict_from_a_declared_reviewer_of_someone_elses_work_is_not_warned_about() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "declared-verdict"]));
    let worktree = repo.home.join(".worktrees").join("repo-declared-verdict");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(repo.arc(&worktree).args(["snapshot", "declared-verdict"]));
    let reviewed = repo
        .arc(&repo.root)
        .env("ARC_ACTOR", "reviewer")
        .args(["review", "declared-verdict", "--verdict", "approved"])
        .assert()
        .success();
    let warning = String::from_utf8_lossy(&reviewed.get_output().stderr).into_owned();
    assert!(!warning.contains("recorded as"), "{warning}");
}
