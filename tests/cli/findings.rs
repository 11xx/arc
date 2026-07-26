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
            "change-opened" => ("event-1", None),
            "reply-added" => ("event-2", Some("event-3")),
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
