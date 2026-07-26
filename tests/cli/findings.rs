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
