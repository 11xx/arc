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
    assert_eq!(view["schema"], "arc-review/5");
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

/// A findings batch and a requested-rework round refuse on rules a reviewer
/// writing them has to know in advance, so `--help` states each rule the
/// refusal enforces and the refusal points back at it.
#[test]
fn review_help_states_the_findings_shape_and_the_cause_rule_its_refusals_enforce() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "review-contract");
    let normalize = |text: String| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let help = normalize(stdout(repo.arc(&worktree).args(["review", "--help"])));
    for rule in [
        "Required with `--verdict changes-requested` and refused with any other verdict",
        "a required `severity` (critical, major, minor, or note) and `summary` (string)",
        "optional `blocking` (bool, default false), `body` (string), and `anchor`",
        "a required `path` and optional `side` (base or head, default head), `line_start`, `line_end`, and `context`",
        "`line` for `line_start`, `lines` for `line_start` and `line_end`",
    ] {
        assert!(help.contains(rule), "missing {rule:?} in: {help}");
    }

    let before = event_count(&repo, &change_id);
    repo.arc(&worktree)
        .args([
            "review",
            "review-contract",
            "--verdict",
            "comment-only",
            "--findings-json",
            "-",
        ])
        .write_stdin(r#"[{"severity": "blocking", "summary": "a defect"}]"#)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "malformed findings JSON; `arc review --help` states the shape",
        ))
        .stderr(predicates::str::contains(
            "expected one of `critical`, `major`, `minor`, `note`",
        ));
    assert_eq!(event_count(&repo, &change_id), before);

    repo.arc(&worktree)
        .args([
            "review",
            "review-contract",
            "--verdict",
            "comment-only",
            "--findings-json",
            "-",
        ])
        .write_stdin(r#"[{"severity": "major", "summary": "a defect", "blocking": true}]"#)
        .assert()
        .success();
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
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_SESSION")
            .args(["snapshot", "assumed-verdict"]),
    );
    let reviewed = repo
        .arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION")
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

/// A findings batch is read for named fields, so a misspelled `blocking`
/// would record a non-blocking finding that an approval could carry. A field
/// that looks like a misspelling of one the finding omits refuses the batch;
/// any other unknown field is ignored with a warning; a supplied `id` is
/// ignored silently because arc assigns finding IDs.
#[test]
fn findings_json_refuses_a_misspelled_field_and_warns_on_other_unknown_fields() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "findings-fields");
    let review = |verdict: &str, batch: &str| {
        let mut command = repo.arc(&worktree);
        command.args(["review", "findings-fields", "--verdict", verdict]);
        if verdict == "changes-requested" {
            command.args(["--cause", "executor"]);
        }
        command
            .args(["--findings-json", "-"])
            .write_stdin(batch.to_string());
        command
    };

    let before = event_count(&repo, &change_id);
    review(
        "approved",
        r#"[{"severity": "major", "summary": "a defect", "blocker": true}]"#,
    )
    .assert()
    .failure()
    .stderr(predicates::str::contains(
        "finding 1 has unknown field `blocker`, which looks like a misspelling of `blocking`",
    ));
    assert_eq!(event_count(&repo, &change_id), before);

    review(
        "changes-requested",
        r#"[{"severity": "minor", "summary": "noted", "tool": "lint"}]"#,
    )
    .assert()
    .success()
    .stderr(predicates::str::contains(
        "warning: finding 1 has unknown field `tool`, which arc ignores (a finding reads blocking, severity, summary, body, anchor)",
    ));
    assert_eq!(event_count(&repo, &change_id), before + 1);
    let findings =
        json_stdout(
            repo.arc(&worktree)
                .args(["findings", "findings-fields", "--format", "json"]),
        );
    assert_eq!(findings["findings"][0]["summary"], "noted", "{findings}");

    review(
        "comment-only",
        r#"[{"id": "F-1", "blocking": false, "severity": "note", "summary": "plain"}]"#,
    )
    .assert()
    .success()
    .stderr(predicates::str::contains("warning").not());
    assert_eq!(event_count(&repo, &change_id), before + 2);
}

/// An anchor is read for named fields too, so `line` for `line_start` would
/// record an anchor with no line. A field that looks like a misspelling of one
/// the anchor omits refuses the batch; any other unknown anchor field is
/// ignored with a warning naming the finding.
#[test]
fn findings_json_refuses_a_misspelled_anchor_field_and_warns_on_other_unknown_ones() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "anchor-fields");
    let review = |anchor: &str| {
        let mut command = repo.arc(&worktree);
        command
            .args(["review", "anchor-fields", "--verdict", "comment-only"])
            .args(["--findings-json", "-"])
            .write_stdin(format!(
                r#"[{{"severity": "note", "summary": "anchored", "anchor": {anchor}}}]"#
            ));
        command
    };

    let before = event_count(&repo, &change_id);
    review(r#"{"path": "anchor-fields.txt", "line": 1}"#)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "the anchor of finding 1 has unknown field `line`, which looks like a misspelling of `line_start`; rename or remove it (an anchor reads path, side, line_start, line_end, context)",
        ));
    review(r#"{"path": "anchor-fields.txt", "lines": "1-2"}"#)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "which looks like a misspelling of `line_start` and `line_end`",
        ));
    assert_eq!(event_count(&repo, &change_id), before);

    review(r#"{"path": "anchor-fields.txt", "line_start": 1, "note": "first line"}"#)
        .assert()
        .success()
        .stderr(predicates::str::contains(
            "warning: the anchor of finding 1 has unknown field `note`, which arc ignores (an anchor reads path, side, line_start, line_end, context)",
        ));
    assert_eq!(event_count(&repo, &change_id), before + 1);
    let findings =
        json_stdout(
            repo.arc(&worktree)
                .args(["findings", "anchor-fields", "--format", "json"]),
        );
    let anchor = &findings["findings"][0]["anchor"];
    assert_eq!(anchor["path"], "anchor-fields.txt", "{findings}");
    assert_eq!(anchor["line_start"], 1, "{findings}");

    review(r#"{"path": "anchor-fields.txt", "side": "head", "line_start": 1, "line_end": 1, "context": "anchor-fields"}"#)
        .assert()
        .success()
        .stderr(predicates::str::contains("warning").not());
    assert_eq!(event_count(&repo, &change_id), before + 2);
}

/// What a refused review must leave as it was: the change's events, its latest
/// patchset with its contributors, whether an approval stands, and the
/// retention refs.
fn recorded(
    repo: &Repo,
    change_id: &str,
    slug: &str,
) -> (usize, serde_json::Value, serde_json::Value, String) {
    let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
    (
        event_count(repo, change_id),
        status["latest_patchset"].clone(),
        status["has_valid_approval"].clone(),
        git_out(
            &repo.root,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/arc/",
            ],
        ),
    )
}

/// `review --snapshot` validates the findings batch and the verdict before it
/// records anything: a refused review leaves the patchsets, their
/// contributors, the standing approval, and the retention refs as they were,
/// even when its attribution would have recorded a new patchset.
#[test]
fn a_refused_review_snapshot_records_no_patchset() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "refused-snapshot");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args(["review", "refused-snapshot", "--verdict", "approved"])
        .assert()
        .success();
    let observed = || recorded(&repo, &change_id, "refused-snapshot");
    let before = observed();
    assert_eq!(before.1["id"], "ps-01", "{}", before.1);
    assert_eq!(before.2, true, "the fixture's approval must stand");

    for (batch, refusal) in [
        (
            r#"[{"severity": "major", "summary": "a defect", "blocker": true}]"#,
            "which looks like a misspelling of `blocking`",
        ),
        (
            r#"[{"severity": "major", "summary": "a defect", "blocking": true}]"#,
            "cannot approve while recording blocking findings",
        ),
    ] {
        repo.arc(&worktree)
            .env("ARC_ACTOR", "reviewer")
            .args([
                "review",
                "refused-snapshot",
                "--snapshot",
                "--contributors",
                "codex-luna",
                "--verdict",
                "approved",
                "--findings-json",
                "-",
            ])
            .write_stdin(batch.to_string())
            .assert()
            .failure()
            .stderr(predicates::str::contains(refusal));
        assert_eq!(observed(), before, "{batch}");
    }
}

/// A close attempted between the patchset `review --snapshot` records and the
/// verdict it records cannot strand that patchset: either the close finds the
/// change busy and the review records both, or the review refuses having
/// recorded neither.
#[test]
fn a_close_attempted_while_review_snapshots_cannot_strand_a_patchset() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "raced-review");
    repo.commit(&worktree, "more.txt", "more\n", "test: move the head");
    let before = recorded(&repo, &change_id, "raced-review");
    let release = repo.home.join("release-review");

    let (out, closed) = thread::scope(|scope| {
        let closer = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(30);
            while event_count(&repo, &change_id) == before.0 {
                assert!(Instant::now() < deadline, "the review recorded no patchset");
                thread::sleep(Duration::from_millis(10));
            }
            let closed = repo
                .arc(&repo.root)
                .env("ARC_ACTOR", "codex-luna")
                .args(["close", "raced-review", "--abandoned"])
                .output()
                .unwrap();
            fs::write(&release, "").unwrap();
            closed
        });
        let out = repo
            .arc(&worktree)
            .env("ARC_ACTOR", "reviewer")
            .env("ARC_REVIEW_PAUSE", &release)
            .args([
                "review",
                "raced-review",
                "--snapshot",
                "--verdict",
                "comment-only",
            ])
            .output()
            .unwrap();
        (out, closer.join().unwrap())
    });

    let stderr = String::from_utf8_lossy(&out.stderr);
    let after = recorded(&repo, &change_id, "raced-review");
    if out.status.success() {
        assert!(
            !closed.status.success(),
            "the change closed between the snapshot and the verdict"
        );
        assert_eq!(
            after.1["head"],
            repo.head(&worktree).as_str(),
            "{}",
            after.1
        );
        let status = json_stdout(
            repo.arc(&repo.root)
                .args(["status", "raced-review", "--json"]),
        );
        assert_eq!(status["verdict"]["patchset_id"], after.1["id"], "{status}");
    } else {
        assert_eq!(
            after.1, before.1,
            "a refused review recorded a patchset: {stderr}"
        );
        assert_eq!(
            after.3, before.3,
            "a refused review left a retention ref: {stderr}"
        );
    }
}

/// The type of every event recorded on `change_id`, oldest first. Read from
/// the ledger files, so it holds when the target's declarations do not parse.
fn event_types(repo: &Repo, change_id: &str) -> Vec<String> {
    let mut paths: Vec<_> = fs::read_dir(event_dir(repo, change_id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let event: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            event["event_type"].as_str().unwrap().to_string()
        })
        .collect()
}

/// What a review prints after it records is advice about a verdict that
/// already stands. A target policy that does not parse leaves that advice
/// unknown, not the review refused: the patchset, its retention ref, and the
/// verdict stay recorded, the command says so, and it succeeds.
#[test]
fn an_unreadable_target_policy_warns_after_review_records() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "advisory-policy");
    repo.commit(&worktree, "more.txt", "more\n", "test: move the head");
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    repo.commit(
        &repo.root,
        ".arc/policy.toml",
        "[policy\n",
        "test: break the target policy",
    );
    let before = event_types(&repo, &change_id);

    let out = repo
        .arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args([
            "review",
            "advisory-policy",
            "--snapshot",
            "--verdict",
            "comment-only",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("patchset: ps-02"), "{stdout}");
    assert!(stdout.contains("verdict: CommentOnly on ps-02"), "{stdout}");
    assert!(stderr.contains("warning: "), "{stderr}");
    assert!(stderr.contains(".arc/policy.toml"), "{stderr}");
    let after = event_types(&repo, &change_id);
    assert_eq!(
        after[before.len()..],
        ["patchset-added".to_string(), "verdict-recorded".to_string()],
        "{after:?}"
    );
    assert!(
        git_out(&repo.root, &["for-each-ref", "refs/arc/"]).contains("ps-02"),
        "the recorded patchset lost its retention ref"
    );
}

/// A misspelled anchor field refuses the batch before `--snapshot` records the
/// head that moved, whatever the verdict.
#[test]
fn a_misspelled_anchor_field_refuses_review_snapshot_before_it_records() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "anchor-snapshot");
    repo.commit(&worktree, "more.txt", "more\n", "test: move the head");
    let before = recorded(&repo, &change_id, "anchor-snapshot");
    assert_eq!(before.1["id"], "ps-01", "{}", before.1);

    repo.arc(&worktree)
        .args([
            "review",
            "anchor-snapshot",
            "--snapshot",
            "--verdict",
            "comment-only",
            "--findings-json",
            "-",
        ])
        .write_stdin(
            r#"[{"severity": "minor", "summary": "a note", "anchor": {"path": "more.txt", "line": 1}}]"#,
        )
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "the anchor of finding 1 has unknown field `line`, which looks like a misspelling of `line_start`",
        ));
    assert_eq!(recorded(&repo, &change_id, "anchor-snapshot"), before);
}
