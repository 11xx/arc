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

    let check = stdout(repo.arc(&wt).args(["check", "eras"]));
    assert!(
        check.contains("raised in round one (against ps-01)"),
        "{check}"
    );
}
