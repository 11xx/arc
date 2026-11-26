use super::common::*;

fn finding_with_reply_target(repo: &Repo, slug: &str) -> (std::path::PathBuf, String) {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&wt, "reviewed.rs", "broken\n", "test: add reviewed file");
    stdout(repo.arc(&wt).args(["snapshot", slug]));
    let output = stdout(repo.arc(&wt).args([
        "finding",
        slug,
        "--summary",
        "broken path",
        "--path",
        "reviewed.rs",
        "--line",
        "1",
    ]));
    let event_id = output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();
    (wt, event_id)
}

fn inline_findings(repo: &Repo, slug: &str) -> (std::path::PathBuf, String, Vec<String>, String) {
    let begin = stdout(repo.arc(&repo.root).args(["begin", slug]));
    let change_id = opened_change_id(&begin);
    let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&wt, "reviewed.rs", "broken\n", "test: add reviewed file");
    stdout(repo.arc(&wt).args(["snapshot", slug]));
    let output = repo
        .arc(&wt)
        .args([
            "review",
            slug,
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
            "--findings-json",
            "-",
        ])
        .write_stdin(
            r#"[{"severity":"major","summary":"first"},
                {"severity":"minor","summary":"second"}]"#,
        )
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    let finding_ids = output
        .lines()
        .filter_map(|line| line.strip_prefix("finding: "))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let verdict_event = output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_owned();
    (wt, change_id, finding_ids, verdict_event)
}

#[test]
fn finding_without_replies_omits_replies_member() {
    let repo = Repo::new();
    let (wt, _) = finding_with_reply_target(&repo, "finding-without-replies");
    let output =
        stdout(
            repo.arc(&wt)
                .args(["findings", "finding-without-replies", "--format", "json"]),
        );
    let findings: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert!(findings["findings"][0].get("replies").is_none());
}

#[test]
fn inline_finding_can_be_replied_to_by_finding_id() {
    let repo = Repo::new();
    let (wt, _, finding_ids, _) = inline_findings(&repo, "inline-finding-id-reply");
    repo.arc(&wt)
        .args([
            "reply",
            "inline-finding-id-reply",
            &finding_ids[0],
            "--body",
            "only the first finding",
        ])
        .assert()
        .success();

    let output =
        stdout(
            repo.arc(&wt)
                .args(["findings", "inline-finding-id-reply", "--format", "json"]),
        );
    let findings: serde_json::Value = serde_json::from_str(&output).unwrap();
    let findings = findings["findings"].as_array().unwrap();
    let first = findings
        .iter()
        .find(|finding| finding["id"] == finding_ids[0])
        .unwrap();
    let second = findings
        .iter()
        .find(|finding| finding["id"] == finding_ids[1])
        .unwrap();
    assert_eq!(first["replies"][0]["body"], "only the first finding");
    assert!(second.get("replies").is_none());
}

#[test]
fn inline_finding_prefixes_reject_ambiguity_without_writing() {
    let repo = Repo::new();
    let (wt, change_id, finding_ids, _) = inline_findings(&repo, "inline-finding-prefix-reply");
    let events = event_dir(&repo, &change_id);
    let event_count = fs::read_dir(&events).unwrap().count();

    repo.arc(&wt)
        .args([
            "reply",
            "inline-finding-prefix-reply",
            "f",
            "--body",
            "must not be written",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ambiguous discussion event"));
    assert_eq!(fs::read_dir(&events).unwrap().count(), event_count);

    let unique_prefix_len = (1..finding_ids[0].len())
        .find(|&len| !finding_ids[1].starts_with(&finding_ids[0][..len]))
        .unwrap();
    repo.arc(&wt)
        .args([
            "reply",
            "inline-finding-prefix-reply",
            &finding_ids[0][..unique_prefix_len],
            "--body",
            "unique prefix",
        ])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "reply",
            "inline-finding-prefix-reply",
            &finding_ids[1],
            "--body",
            "exact id",
        ])
        .assert()
        .success();

    let output = stdout(repo.arc(&wt).args([
        "findings",
        "inline-finding-prefix-reply",
        "--format",
        "json",
    ]));
    let findings: serde_json::Value = serde_json::from_str(&output).unwrap();
    let findings = findings["findings"].as_array().unwrap();
    assert_eq!(
        findings
            .iter()
            .find(|finding| finding["id"] == finding_ids[0])
            .unwrap()["replies"][0]["body"],
        "unique prefix"
    );
    assert_eq!(
        findings
            .iter()
            .find(|finding| finding["id"] == finding_ids[1])
            .unwrap()["replies"][0]["body"],
        "exact id"
    );
}

#[test]
fn shared_origin_event_reply_attaches_to_no_finding() {
    let repo = Repo::new();
    let (wt, change_id, finding_ids, verdict_event) = inline_findings(&repo, "shared-origin-reply");
    repo.arc(&wt)
        .args([
            "reply",
            "shared-origin-reply",
            &finding_ids[0],
            "--body",
            "ambiguous parent",
        ])
        .assert()
        .success();
    rewrite_event(&repo, &change_id, "reply-added", |event| {
        event["parent_event_id"] = verdict_event.clone().into();
    });

    let output =
        stdout(
            repo.arc(&wt)
                .args(["findings", "shared-origin-reply", "--format", "json"]),
        );
    let findings: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert!(findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|finding| finding.get("replies").is_none()));
}

#[test]
fn reply_replays_when_its_event_id_sorts_before_its_parent() {
    let repo = Repo::new();
    let begin = stdout(
        repo.arc(&repo.root)
            .args(["begin", "out-of-order-finding-reply"]),
    );
    let change_id = opened_change_id(&begin);
    let wt = repo
        .home
        .join(".worktrees")
        .join("repo-out-of-order-finding-reply");
    let finding = stdout(repo.arc(&wt).args([
        "finding",
        "out-of-order-finding-reply",
        "--summary",
        "late parent",
    ]));
    let finding_event = finding
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();
    stdout(repo.arc(&wt).args([
        "reply",
        "out-of-order-finding-reply",
        finding_event,
        "--body",
        "early reply",
    ]));

    let events = event_dir(&repo, &change_id);
    for path in fs::read_dir(&events)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>()
    {
        let mut event: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let (event_id, parent_event_id) = match event["event_type"].as_str().unwrap() {
            "reply-added" => ("event-1", Some("event-3")),
            "change-opened" => ("event-2", None),
            "finding-added" => ("event-3", None),
            other => panic!("unexpected event type {other}"),
        };
        event["event_id"] = event_id.into();
        if let Some(parent_event_id) = parent_event_id {
            event["parent_event_id"] = parent_event_id.into();
        }
        fs::remove_file(&path).unwrap();
        fs::write(
            events.join(format!("{event_id}.json")),
            serde_json::to_vec_pretty(&event).unwrap(),
        )
        .unwrap();
    }

    let output =
        stdout(
            repo.arc(&wt)
                .args(["findings", "out-of-order-finding-reply", "--format", "json"]),
        );
    let findings: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(findings["findings"][0]["replies"][0]["body"], "early reply");
}

#[test]
fn finding_replies_are_json_objects_in_ledger_order() {
    let repo = Repo::new();
    let (wt, finding_event) = finding_with_reply_target(&repo, "finding-json-replies");
    let first = stdout(repo.arc(&wt).args([
        "reply",
        "finding-json-replies",
        &finding_event,
        "--body",
        "first reply",
    ]));
    let first_event = first
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();
    let second = stdout(repo.arc(&wt).args([
        "reply",
        "finding-json-replies",
        &finding_event,
        "--body",
        "second reply",
    ]));
    let second_event = second
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();

    let output =
        stdout(
            repo.arc(&wt)
                .args(["findings", "finding-json-replies", "--format", "json"]),
        );
    let findings: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        findings["findings"][0]["replies"],
        serde_json::json!([
            {"event_id": first_event, "actor": "tester", "body": "first reply"},
            {"event_id": second_event, "actor": "tester", "body": "second reply"},
        ])
    );
}

#[test]
fn finding_replies_render_under_their_finding() {
    let repo = Repo::new();
    let (wt, finding_event) = finding_with_reply_target(&repo, "finding-show-replies");
    repo.arc(&wt)
        .args([
            "reply",
            "finding-show-replies",
            &finding_event,
            "--body",
            "rendered reply",
        ])
        .assert()
        .success();

    let show = stdout(repo.arc(&wt).args(["show", "finding-show-replies"]));
    let finding = show.find("broken path — open").unwrap();
    let reply = show.find("  - tester: rendered reply").unwrap();
    assert!(reply > finding);
}

#[test]
fn finding_replies_are_excluded_from_sarif() {
    let repo = Repo::new();
    let (wt, finding_event) = finding_with_reply_target(&repo, "finding-sarif-replies");
    repo.arc(&wt)
        .args([
            "reply",
            "finding-sarif-replies",
            &finding_event,
            "--body",
            "private discussion text",
        ])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["findings", "finding-sarif-replies", "--format", "sarif"])
        .assert()
        .success()
        .stdout(predicates::str::contains("private discussion text").not());
}

#[test]
fn comment_replies_still_render_under_their_comment() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "comment-reply-regression"]),
    );
    let wt = repo
        .home
        .join(".worktrees")
        .join("repo-comment-reply-regression");
    let output = stdout(repo.arc(&wt).args([
        "comment",
        "comment-reply-regression",
        "--body",
        "parent comment",
    ]));
    let comment_event = output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();
    repo.arc(&wt)
        .args([
            "reply",
            "comment-reply-regression",
            &comment_event,
            "--body",
            "comment reply",
        ])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["show", "comment-reply-regression"])
        .assert()
        .success()
        .stdout(predicates::str::contains("- tester: comment reply"));
}

#[test]
fn findings_sarif_and_reviewer_checklist_are_role_scoped() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[review]\nchecklist = [\"exercise the failure path\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: add review checklist"]);
    stdout(repo.arc(&repo.root).args(["begin", "sarif-view"]));
    let wt = repo.home.join(".worktrees/repo-sarif-view");
    repo.commit(&wt, "bug.rs", "broken\n", "feat: add reviewed file");
    stdout(repo.arc(&wt).args(["snapshot", "sarif-view"]));
    repo.arc(&wt)
        .args([
            "finding",
            "sarif-view",
            "--summary",
            "broken path",
            "--blocking",
            "--severity",
            "major",
            "--path",
            "bug.rs",
            "--line",
            "1",
        ])
        .assert()
        .success();
    let output = repo
        .arc(&wt)
        .args(["findings", "sarif-view", "--format", "sarif"])
        .output()
        .unwrap();
    let sarif: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(sarif["version"], "2.1.0");
    assert_eq!(sarif["runs"][0]["tool"]["driver"]["name"], "arc");
    assert_eq!(sarif["runs"][0]["results"][0]["level"], "error");
    repo.arc(&wt)
        .env("ARC_ROLE", "implementer")
        .args(["show", "sarif-view"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Review checklist").not());
    repo.arc(&wt)
        .env("ARC_ROLE", "lead")
        .args(["show", "sarif-view"])
        .assert()
        .success()
        .stdout(predicates::str::contains("exercise the failure path"));
}

/// A one-line summary is enough to count findings and not enough to act on
/// one, which is the position a reader inheriting a change is in. The body and
/// the anchor are recorded; only the read surfaces dropped them.
#[test]
fn read_surfaces_carry_the_finding_body_and_anchor() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "legible"]));
    let wt = repo.home.join(".worktrees/repo-legible");
    repo.commit(&wt, "reviewed.rs", "broken\n", "test: add reviewed file");
    stdout(repo.arc(&wt).args(["snapshot", "legible"]));
    stdout(repo.arc(&wt).args([
        "finding",
        "legible",
        "--summary",
        "unchecked index",
        "--body",
        "The slice is indexed before its length is checked.",
        "--path",
        "reviewed.rs",
        "--line",
        "1",
    ]));

    let text = stdout(repo.arc(&wt).args(["findings", "legible"]));
    assert!(text.contains("unchecked index"), "{text}");
    assert!(text.contains("at: reviewed.rs:1 (head)"), "{text}");
    assert!(
        text.contains("| The slice is indexed before its length is checked."),
        "{text}"
    );
    assert!(text.contains("against: ps-01"), "{text}");

    let status = json_stdout(repo.arc(&wt).args(["status", "legible", "--json"]));
    assert_eq!(
        status["findings"][0]["body"],
        "The slice is indexed before its length is checked."
    );
    assert_eq!(status["findings"][0]["anchor"]["path"], "reviewed.rs");
    assert_eq!(status["findings"][0]["patchset_id"], "ps-01");
    assert_eq!(status["findings"][0]["reported_by"], "tester");

    // SARIF exists to carry file, line, and message; the message is the body.
    let sarif = json_stdout(
        repo.arc(&wt)
            .args(["findings", "legible", "--format", "sarif"]),
    );
    let message = sarif["runs"][0]["results"][0]["message"]["text"]
        .as_str()
        .unwrap();
    assert!(message.contains("indexed before its length"), "{message}");
    // The one-line summary stays addressable on its own.
    assert_eq!(
        sarif["runs"][0]["results"][0]["properties"]["summary"],
        "unchecked index"
    );
}

/// An anchor arc recorded may be malformed; the reader shows what is there
/// rather than inventing a range that runs backwards.
#[test]
fn a_backwards_anchor_range_renders_as_its_start() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "backwards"]));
    let wt = repo.home.join(".worktrees/repo-backwards");
    repo.commit(&wt, "reviewed.rs", "broken\n", "test: add reviewed file");
    stdout(repo.arc(&wt).args(["snapshot", "backwards"]));
    stdout(repo.arc(&wt).args([
        "finding",
        "backwards",
        "--summary",
        "reversed range",
        "--path",
        "reviewed.rs",
        "--line",
        "10",
        "--line-end",
        "5",
    ]));

    let text = stdout(repo.arc(&wt).args(["findings", "backwards"]));
    assert!(text.contains("at: reviewed.rs:10 (head)"), "{text}");
    let sarif = json_stdout(
        repo.arc(&wt)
            .args(["findings", "backwards", "--format", "sarif"]),
    );
    let region = &sarif["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"];
    assert_eq!(region["startLine"], 10, "{region}");
    assert_eq!(region["endLine"], 10, "{region}");
}

/// A finding filed inside a review batch is the same object as one filed
/// standalone, and the batch is the path review loops use.
#[test]
fn log_renders_findings_filed_inside_a_review() {
    let repo = Repo::new();
    let (wt, _change, finding_ids, _event) = inline_findings(&repo, "batched");
    let log = stdout(repo.arc(&wt).args(["log", "batched"]));
    for id in &finding_ids {
        assert!(log.contains(id.as_str()), "{log}");
    }
    assert_eq!(
        log.matches("finding-added").count(),
        finding_ids.len(),
        "{log}"
    );
    // Every line keeps the shape a log line has, so nothing that reads this
    // output has to learn a second one.
    for line in log.lines() {
        let fields: Vec<&str> = line.split("  ").filter(|f| !f.is_empty()).collect();
        assert!(fields.len() >= 3, "{line}");
        assert!(fields[0].ends_with('Z'), "{line}");
        assert!(fields[1].contains('@'), "{line}");
    }
}

/// A findings count that mixes rounds saturates and gates nothing, so a
/// finding carried in from an earlier patchset says which one it answers.
#[test]
fn blocking_findings_name_the_patchset_they_predate() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "eras"]));
    let wt = repo.home.join(".worktrees/repo-eras");
    repo.commit(&wt, "one.rs", "first\n", "feat: first");
    stdout(repo.arc(&wt).args(["snapshot", "eras"]));
    stdout(repo.arc(&wt).args([
        "finding",
        "eras",
        "--summary",
        "raised in round one",
        "--blocking",
    ]));
    repo.commit(&wt, "two.rs", "second\n", "feat: second");
    stdout(repo.arc(&wt).args(["snapshot", "eras"]));

    let check = stdout_any_status(repo.arc(&wt).args(["check", "eras"]));
    assert!(
        check.contains("raised in round one (against ps-01)"),
        "{check}"
    );
}

#[test]
fn finding_resolution_survives_rebase_via_patch_id_and_blocks_when_dropped() {
    let repo = Repo::new();
    let begin = stdout(repo.arc(&repo.root).args(["begin", "rebase-finding"]));
    let change_id = opened_change_id(&begin);
    let wt = repo.home.join(".worktrees/repo-rebase-finding");
    repo.commit(&wt, "feature.rs", "broken\n", "feat: initial change");
    stdout(repo.arc(&wt).args(["snapshot", "rebase-finding"]));
    let finding_out = stdout(repo.arc(&wt).args([
        "finding",
        "rebase-finding",
        "--summary",
        "fix edge case",
        "--blocking",
        "--severity",
        "major",
    ]));
    let finding_id = finding_out
        .lines()
        .find_map(|line| line.strip_prefix("finding: "))
        .unwrap()
        .to_string();

    // Fix the finding in a commit and resolve citing that commit.
    repo.commit(&wt, "feature.rs", "fixed\n", "fix: handle edge case");
    let fix_commit = repo.head(&wt);
    stdout(repo.arc(&wt).args([
        "resolve",
        "rebase-finding",
        &finding_id,
        "--status",
        "resolved",
        "--commit",
        &fix_commit,
    ]));
    stdout(repo.arc(&wt).args(["snapshot", "rebase-finding"]));

    // Advance master with an unrelated commit and rebase the branch so the fix
    // commit is carried forward with a new SHA and identical Git patch-id.
    repo.commit(&repo.root, "unrelated.txt", "target\n", "feat: target move");
    stdout(repo.arc(&wt).args(["rebase", "rebase-finding"]));
    let carried_commit = repo.head(&wt);
    assert_ne!(carried_commit, fix_commit);

    // Status and check must recognize the carried fix via patch-id (no open blockers).
    let status = json_stdout(repo.arc(&wt).args(["status", "rebase-finding", "--json"]));
    assert_eq!(
        status["open_blocking_findings"],
        serde_json::json!([]),
        "carried resolution commit must not reopen blocking finding: {status}"
    );
    assert_eq!(status["findings"][0]["status"], "resolved", "{status}");

    // Approval + integration must succeed without re-resolving the finding.
    stdout(repo.arc(&wt).args([
        "--actor",
        "reviewer",
        "review",
        "rebase-finding",
        "--verdict",
        "approved",
    ]));
    repo.arc(&wt)
        .args(["check", "rebase-finding"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &change_id])
        .assert()
        .success();

    // Contrast: if the cited fix commit is dropped (not in head and no matching
    // patch-id in base..head), the blocking finding stays open and blocks integration.
    let begin2 = stdout(repo.arc(&repo.root).args(["begin", "dropped-finding"]));
    let change_id2 = opened_change_id(&begin2);
    let wt2 = repo.home.join(".worktrees/repo-dropped-finding");
    repo.commit(&wt2, "other.rs", "v1\n", "feat: base change");
    stdout(repo.arc(&wt2).args(["snapshot", "dropped-finding"]));
    let finding_out2 = stdout(repo.arc(&wt2).args([
        "finding",
        "dropped-finding",
        "--summary",
        "must keep fix",
        "--blocking",
        "--severity",
        "major",
    ]));
    let finding_id2 = finding_out2
        .lines()
        .find_map(|line| line.strip_prefix("finding: "))
        .unwrap()
        .to_string();
    repo.commit(&wt2, "other.rs", "v2-fixed\n", "fix: resolve issue");
    let dropped_fix = repo.head(&wt2);
    stdout(repo.arc(&wt2).args([
        "resolve",
        "dropped-finding",
        &finding_id2,
        "--status",
        "resolved",
        "--commit",
        &dropped_fix,
    ]));
    // Drop the fix commit by resetting back one commit and re-snapshotting.
    git(&wt2, &["reset", "--hard", "HEAD~1"]);
    stdout(repo.arc(&wt2).args(["snapshot", "dropped-finding"]));
    let status2 = json_stdout(repo.arc(&wt2).args(["status", "dropped-finding", "--json"]));
    assert_eq!(
        status2["open_blocking_findings"],
        serde_json::json!([finding_id2]),
        "dropped fix commit must keep blocking finding open: {status2}"
    );
    assert_eq!(status2["findings"][0]["status"], "open", "{status2}");
    let check2 = repo.arc(&wt2).args(["check", &change_id2]).assert().code(2);
    let out2 = String::from_utf8_lossy(&check2.get_output().stdout);
    assert!(
        out2.contains("not in head"),
        "check should explain missing resolution commit:\n{out2}"
    );
}

#[test]
fn reverted_resolution_reopens_blocking_finding_before_and_after_rebase() {
    let repo = Repo::new();

    // Case 1: direct revert of C1 without rebase.
    let begin1 = stdout(repo.arc(&repo.root).args(["begin", "direct-revert"]));
    let change1 = opened_change_id(&begin1);
    let wt1 = repo.home.join(".worktrees/repo-direct-revert");
    repo.commit(&wt1, "direct.rs", "broken\n", "feat: broken");
    stdout(repo.arc(&wt1).args(["snapshot", "direct-revert"]));
    let fid1 = stdout(repo.arc(&wt1).args([
        "finding",
        "direct-revert",
        "--summary",
        "must stay fixed",
        "--blocking",
        "--severity",
        "major",
    ]))
    .lines()
    .find_map(|line| line.strip_prefix("finding: "))
    .unwrap()
    .to_string();
    repo.commit(&wt1, "direct.rs", "fixed\n", "fix: direct fix");
    let c1 = repo.head(&wt1);
    stdout(repo.arc(&wt1).args([
        "resolve",
        "direct-revert",
        &fid1,
        "--status",
        "resolved",
        "--commit",
        &c1,
    ]));
    git(&wt1, &["revert", "--no-edit", &c1]);
    stdout(repo.arc(&wt1).args(["snapshot", "direct-revert"]));

    let status1 = json_stdout(repo.arc(&wt1).args(["status", &change1, "--json"]));
    assert_eq!(
        status1["open_blocking_findings"],
        serde_json::json!([fid1]),
        "directly reverted fix must reopen blocking finding: {status1}"
    );
    assert_eq!(status1["findings"][0]["status"], "open", "{status1}");
    let check1 = repo.arc(&wt1).args(["check", &change1]).assert().code(2);
    let out1 = String::from_utf8_lossy(&check1.get_output().stdout);
    assert!(
        out1.contains("reverted before head"),
        "check should explain reverted resolution commit:\n{out1}"
    );

    // Case 2: rebase carries C1 -> C2, then C2 is reverted.
    let begin2 = stdout(repo.arc(&repo.root).args(["begin", "rebase-revert"]));
    let change2 = opened_change_id(&begin2);
    let wt2 = repo.home.join(".worktrees/repo-rebase-revert");
    repo.commit(&wt2, "rebased.rs", "broken\n", "feat: broken");
    stdout(repo.arc(&wt2).args(["snapshot", "rebase-revert"]));
    let fid2 = stdout(repo.arc(&wt2).args([
        "finding",
        "rebase-revert",
        "--summary",
        "must survive after rebase",
        "--blocking",
        "--severity",
        "major",
    ]))
    .lines()
    .find_map(|line| line.strip_prefix("finding: "))
    .unwrap()
    .to_string();
    repo.commit(&wt2, "rebased.rs", "fixed\n", "fix: handle bug");
    let orig_fix = repo.head(&wt2);
    stdout(repo.arc(&wt2).args([
        "resolve",
        "rebase-revert",
        &fid2,
        "--status",
        "resolved",
        "--commit",
        &orig_fix,
    ]));
    stdout(repo.arc(&wt2).args(["snapshot", "rebase-revert"]));
    repo.commit(&repo.root, "target.txt", "target\n", "feat: advance target");
    stdout(repo.arc(&wt2).args(["rebase", "rebase-revert"]));
    let carried = repo.head(&wt2);
    assert_ne!(carried, orig_fix);

    // Revert the carried fix commit C2 and snapshot.
    git(&wt2, &["revert", "--no-edit", &carried]);
    stdout(repo.arc(&wt2).args(["snapshot", "rebase-revert"]));
    let status2 = json_stdout(repo.arc(&wt2).args(["status", &change2, "--json"]));
    assert_eq!(
        status2["open_blocking_findings"],
        serde_json::json!([fid2]),
        "reverted rebased fix must reopen blocking finding: {status2}"
    );
    assert_eq!(status2["findings"][0]["status"], "open", "{status2}");
    let check2 = repo.arc(&wt2).args(["check", &change2]).assert().code(2);
    let out2 = String::from_utf8_lossy(&check2.get_output().stdout);
    assert!(
        out2.contains("reverted before head"),
        "check should explain reverted rebased resolution commit:\n{out2}"
    );
}

#[test]
fn rewritten_unsnapshotted_resolution_survives_rebase_and_gc() {
    let repo = Repo::new();
    let begin = stdout(repo.arc(&repo.root).args(["begin", "rewrite-rebase-gc"]));
    let change_id = opened_change_id(&begin);
    let wt = repo.home.join(".worktrees/repo-rewrite-rebase-gc");
    repo.commit(&wt, "gc.rs", "broken\n", "feat: initial");
    stdout(repo.arc(&wt).args(["snapshot", "rewrite-rebase-gc"]));
    let fid = stdout(repo.arc(&wt).args([
        "finding",
        "rewrite-rebase-gc",
        "--summary",
        "fix before rewrite and rebase",
        "--blocking",
        "--severity",
        "major",
    ]))
    .lines()
    .find_map(|line| line.strip_prefix("finding: "))
    .unwrap()
    .to_string();

    // Unsnapshotted fix commit C1, resolved against C1, then rewritten to C2.
    repo.commit(&wt, "gc.rs", "fixed\n", "fix: unsnapshotted fix");
    let c1 = repo.head(&wt);
    stdout(repo.arc(&wt).args([
        "resolve",
        "rewrite-rebase-gc",
        &fid,
        "--status",
        "resolved",
        "--commit",
        &c1,
    ]));
    repo.arc(&wt)
        .args([
            "rewrite",
            "trailers",
            "--from",
            &c1,
            "--no-sign",
            "--append",
            "Planned-by: Planner <planner@example.invalid>",
        ])
        .assert()
        .success();
    let c2 = repo.head(&wt);
    assert_ne!(c2, c1);

    // Advance master and run `arc rebase` (carrying C2 -> C3 without snapping C2 first).
    repo.commit(&repo.root, "target.txt", "move\n", "feat: move master");
    stdout(repo.arc(&wt).args(["rebase", "rewrite-rebase-gc"]));
    let c3 = repo.head(&wt);
    assert_ne!(c3, c2);

    // Prune all unreferenced objects aggressively.
    git(&repo.root, &["reflog", "expire", "--expire=now", "--all"]);
    git(&repo.root, &["gc", "--prune=now"]);

    // C2 must still be retained so resolution verification keeps the finding resolved.
    git(&repo.root, &["cat-file", "-e", &c2]);
    let status = json_stdout(repo.arc(&wt).args(["status", &change_id, "--json"]));
    assert_eq!(
        status["open_blocking_findings"],
        serde_json::json!([]),
        "finding must stay resolved after rewrite + rebase + gc: {status}"
    );
    assert_eq!(status["findings"][0]["status"], "resolved", "{status}");
}

#[test]
fn reverted_fix_with_unrelated_edit_partial_rollback_and_merges() {
    let repo = Repo::new();

    // Case 1: Clean revert + unrelated edit to same file, BEFORE rebase.
    let begin1 = stdout(repo.arc(&repo.root).args(["begin", "revert-plus-edit"]));
    let change1 = opened_change_id(&begin1);
    let wt1 = repo.home.join(".worktrees/repo-revert-plus-edit");
    repo.commit(&wt1, "a.txt", "broken\n", "feat: broken");
    stdout(repo.arc(&wt1).args(["snapshot", "revert-plus-edit"]));
    let fid1 = stdout(repo.arc(&wt1).args([
        "finding",
        "revert-plus-edit",
        "--summary",
        "fix a.txt",
        "--blocking",
        "--severity",
        "major",
    ]))
    .lines()
    .find_map(|line| line.strip_prefix("finding: "))
    .unwrap()
    .to_string();
    repo.commit(&wt1, "a.txt", "fixed\n", "fix: repair a.txt");
    let fix1 = repo.head(&wt1);
    stdout(repo.arc(&wt1).args([
        "resolve",
        "revert-plus-edit",
        &fid1,
        "--status",
        "resolved",
        "--commit",
        &fix1,
    ]));
    git(&wt1, &["revert", "--no-edit", &fix1]);
    repo.commit(
        &wt1,
        "a.txt",
        "broken\nunrelated\n",
        "chore: unrelated line after revert",
    );
    stdout(repo.arc(&wt1).args(["snapshot", "revert-plus-edit"]));
    let status1 = json_stdout(repo.arc(&wt1).args(["status", &change1, "--json"]));
    assert_eq!(
        status1["open_blocking_findings"],
        serde_json::json!([fid1]),
        "revert + unrelated edit before rebase must keep finding open: {status1}"
    );
    assert_eq!(status1["findings"][0]["status"], "open", "{status1}");
    repo.arc(&wt1).args(["check", &change1]).assert().code(2);

    // Case 2: Rebase carries fix -> revert carried fix -> unrelated edit to same file.
    let begin2 = stdout(repo.arc(&repo.root).args(["begin", "rebase-revert-edit"]));
    let change2 = opened_change_id(&begin2);
    let wt2 = repo.home.join(".worktrees/repo-rebase-revert-edit");
    repo.commit(&wt2, "a.txt", "broken\n", "feat: broken");
    stdout(repo.arc(&wt2).args(["snapshot", "rebase-revert-edit"]));
    let fid2 = stdout(repo.arc(&wt2).args([
        "finding",
        "rebase-revert-edit",
        "--summary",
        "fix a.txt after rebase",
        "--blocking",
        "--severity",
        "major",
    ]))
    .lines()
    .find_map(|line| line.strip_prefix("finding: "))
    .unwrap()
    .to_string();
    repo.commit(&wt2, "a.txt", "fixed\n", "fix: repair a.txt");
    let fix2 = repo.head(&wt2);
    stdout(repo.arc(&wt2).args([
        "resolve",
        "rebase-revert-edit",
        &fid2,
        "--status",
        "resolved",
        "--commit",
        &fix2,
    ]));
    stdout(repo.arc(&wt2).args(["snapshot", "rebase-revert-edit"]));
    repo.commit(
        &repo.root,
        "target.txt",
        "advance\n",
        "feat: advance target",
    );
    stdout(repo.arc(&wt2).args(["rebase", "rebase-revert-edit"]));
    let carried2 = repo.head(&wt2);
    assert_ne!(carried2, fix2);
    git(&wt2, &["revert", "--no-edit", &carried2]);
    repo.commit(
        &wt2,
        "a.txt",
        "broken\nunrelated\n",
        "chore: unrelated line after rebased revert",
    );
    stdout(repo.arc(&wt2).args(["snapshot", "rebase-revert-edit"]));
    let status2 = json_stdout(repo.arc(&wt2).args(["status", &change2, "--json"]));
    assert_eq!(
        status2["open_blocking_findings"],
        serde_json::json!([fid2]),
        "revert + unrelated edit after rebase must keep finding open: {status2}"
    );
    assert_eq!(status2["findings"][0]["status"], "open", "{status2}");
    repo.arc(&wt2).args(["check", &change2]).assert().code(2);

    // Case 3: Multi-file fix where one required file is restored to pre-fix state.
    let begin3 = stdout(repo.arc(&repo.root).args(["begin", "partial-rollback"]));
    let change3 = opened_change_id(&begin3);
    let wt3 = repo.home.join(".worktrees/repo-partial-rollback");
    fs::write(wt3.join("one.txt"), "bad-one\n").unwrap();
    fs::write(wt3.join("two.txt"), "bad-two\n").unwrap();
    git(&wt3, &["add", "one.txt", "two.txt"]);
    git(&wt3, &["commit", "-m", "feat: two broken files"]);
    stdout(repo.arc(&wt3).args(["snapshot", "partial-rollback"]));
    let fid3 = stdout(repo.arc(&wt3).args([
        "finding",
        "partial-rollback",
        "--summary",
        "fix both files",
        "--blocking",
        "--severity",
        "major",
    ]))
    .lines()
    .find_map(|line| line.strip_prefix("finding: "))
    .unwrap()
    .to_string();
    fs::write(wt3.join("one.txt"), "good-one\n").unwrap();
    fs::write(wt3.join("two.txt"), "good-two\n").unwrap();
    git(&wt3, &["add", "one.txt", "two.txt"]);
    git(&wt3, &["commit", "-m", "fix: repair both files"]);
    let fix3 = repo.head(&wt3);
    stdout(repo.arc(&wt3).args([
        "resolve",
        "partial-rollback",
        &fid3,
        "--status",
        "resolved",
        "--commit",
        &fix3,
    ]));
    // Restore only one.txt back to its pre-fix state while keeping two.txt fixed.
    repo.commit(&wt3, "one.txt", "bad-one\n", "chore: restore one.txt");
    stdout(repo.arc(&wt3).args(["snapshot", "partial-rollback"]));
    let status3 = json_stdout(repo.arc(&wt3).args(["status", &change3, "--json"]));
    assert_eq!(
        status3["open_blocking_findings"],
        serde_json::json!([fid3]),
        "partial rollback of one file in a multi-file fix must reopen finding: {status3}"
    );
    assert_eq!(status3["findings"][0]["status"], "open", "{status3}");
    repo.arc(&wt3).args(["check", &change3]).assert().code(2);

    // Case 4: Normal follow-up edits on top of a surviving fix (appending and refactoring)
    // and a side-branch revert discarded by `git merge -s ours` must NOT reopen the finding.
    let begin4 = stdout(repo.arc(&repo.root).args(["begin", "surviving-followups"]));
    let change4 = opened_change_id(&begin4);
    let wt4 = repo.home.join(".worktrees/repo-surviving-followups");
    repo.commit(&wt4, "ok.txt", "broken\n", "feat: broken");
    stdout(repo.arc(&wt4).args(["snapshot", "surviving-followups"]));
    let fid4 = stdout(repo.arc(&wt4).args([
        "finding",
        "surviving-followups",
        "--summary",
        "must stay resolved on follow-up edits",
        "--blocking",
        "--severity",
        "major",
    ]))
    .lines()
    .find_map(|line| line.strip_prefix("finding: "))
    .unwrap()
    .to_string();
    repo.commit(&wt4, "ok.txt", "fixed\n", "fix: repair ok.txt");
    let fix4 = repo.head(&wt4);
    stdout(repo.arc(&wt4).args([
        "resolve",
        "surviving-followups",
        &fid4,
        "--status",
        "resolved",
        "--commit",
        &fix4,
    ]));
    // Create a side branch that reverts fix4, then merge it with `-s ours` (discarding revert).
    git(&wt4, &["checkout", "-b", "side-revert-discarded"]);
    git(&wt4, &["revert", "--no-edit", &fix4]);
    git(&wt4, &["checkout", "arc/surviving-followups"]);
    git(
        &wt4,
        &["merge", "-s", "ours", "--no-edit", "side-revert-discarded"],
    );
    git(&wt4, &["branch", "-D", "side-revert-discarded"]);
    // Follow-up 1: append an unrelated line while keeping the fix.
    repo.commit(
        &wt4,
        "ok.txt",
        "fixed\nextra-feature\n",
        "feat: append unrelated line",
    );
    stdout(repo.arc(&wt4).args(["snapshot", "surviving-followups"]));
    let status4a = json_stdout(repo.arc(&wt4).args(["status", &change4, "--json"]));
    assert_eq!(
        status4a["open_blocking_findings"],
        serde_json::json!([]),
        "discarded side-branch revert + unrelated edit must keep finding resolved: {status4a}"
    );
    assert_eq!(status4a["findings"][0]["status"], "resolved", "{status4a}");
    // Follow-up 2: refactor the fixed line to an improved implementation.
    repo.commit(
        &wt4,
        "ok.txt",
        "fixed-v2\nextra-feature\n",
        "refactor: refine fix",
    );
    stdout(repo.arc(&wt4).args(["snapshot", "surviving-followups"]));
    let status4b = json_stdout(repo.arc(&wt4).args(["status", &change4, "--json"]));
    assert_eq!(
        status4b["open_blocking_findings"],
        serde_json::json!([]),
        "normal refactor on top of surviving fix must keep finding resolved: {status4b}"
    );
    assert_eq!(status4b["findings"][0]["status"], "resolved", "{status4b}");
}
