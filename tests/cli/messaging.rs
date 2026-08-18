use super::common::*;

fn bucket_has(inbox: &serde_json::Value, bucket: &str, change_id: &str) -> bool {
    inbox[bucket]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["change_id"] == change_id)
}

#[test]
fn message_appends_well_formed_event_and_rejects_bad_input() {
    let repo = Repo::new();
    let change_id = begin_change(&repo, "msg-basic", None);
    let baseline = event_count(&repo, &change_id);

    repo.arc(&repo.root)
        .args([
            "message",
            "msg-basic",
            "--type",
            "status",
            "--summary",
            "pipeline green",
            "--detail",
            "all gates passed",
            "--severity",
            "warning",
            "--json",
            "{\"run\":42}",
        ])
        .assert()
        .success();
    assert_eq!(event_count(&repo, &change_id), baseline + 1);

    // Every rejection is a usage error that appends nothing.
    let before = event_count(&repo, &change_id);
    repo.arc(&repo.root)
        .args([
            "message",
            "msg-basic",
            "--type",
            "note",
            "--summary",
            "x",
            "--json",
            "[1,2]",
        ])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("--json must be a JSON object"));
    repo.arc(&repo.root)
        .args(["message", "msg-basic", "--type", "note", "--summary", "   "])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(
            "summary must be a non-empty single line",
        ));
    repo.arc(&repo.root)
        .args([
            "message",
            "msg-basic",
            "--type",
            "verdict",
            "--summary",
            "x",
        ])
        .assert()
        .code(2);
    repo.arc(&repo.root)
        .args([
            "message",
            "msg-basic",
            "--type",
            "note",
            "--summary",
            "x",
            "--severity",
            "fatal",
        ])
        .assert()
        .code(2);
    assert_eq!(event_count(&repo, &change_id), before);
}

#[test]
fn messages_filter_across_open_and_closed_changes() {
    let repo = Repo::new();
    let open_id = begin_change(&repo, "msg-open", None);
    let closed_id = begin_change(&repo, "msg-closed", None);

    repo.arc(&repo.root)
        .args([
            "message",
            "msg-open",
            "--type",
            "status",
            "--summary",
            "open status info",
            "--severity",
            "info",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "message",
            "msg-open",
            "--type",
            "discovery",
            "--summary",
            "open discovery error",
            "--severity",
            "error",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "message",
            "msg-closed",
            "--type",
            "note",
            "--summary",
            "closed note warning",
            "--severity",
            "warning",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["close", "msg-closed", "--abandoned"])
        .assert()
        .success();

    // No filter: all three, including the message on the closed change.
    let all = json_stdout(repo.arc(&repo.root).args(["messages", "--json"]));
    let all = all.as_array().unwrap();
    assert_eq!(all.len(), 3);
    assert!(all
        .iter()
        .any(|m| m["change_id"] == closed_id && m["summary"] == "closed note warning"));

    // --change scopes to one change.
    let scoped = json_stdout(
        repo.arc(&repo.root)
            .args(["messages", "--change", &open_id, "--json"]),
    );
    assert_eq!(scoped.as_array().unwrap().len(), 2);

    // --type and --severity filter independently.
    let discovery =
        json_stdout(
            repo.arc(&repo.root)
                .args(["messages", "--type", "discovery", "--json"]),
        );
    assert_eq!(discovery.as_array().unwrap().len(), 1);
    let errors =
        json_stdout(
            repo.arc(&repo.root)
                .args(["messages", "--severity", "error", "--json"]),
        );
    assert_eq!(errors.as_array().unwrap().len(), 1);

    // --since: a far-past floor keeps all; a far-future floor drops all.
    let since_past = json_stdout(repo.arc(&repo.root).args([
        "messages",
        "--since",
        "2000-01-01T00:00:00Z",
        "--json",
    ]));
    assert_eq!(since_past.as_array().unwrap().len(), 3);
    let since_future = json_stdout(repo.arc(&repo.root).args([
        "messages",
        "--since",
        "2999-01-01T00:00:00Z",
        "--json",
    ]));
    assert_eq!(since_future.as_array().unwrap().len(), 0);
}

#[test]
fn messages_never_affect_check() {
    let repo = Repo::new();
    change_with_patchset(&repo, "msg-check");
    repo.arc(&repo.root)
        .args([
            "message",
            "msg-check",
            "--type",
            "discovery",
            "--summary",
            "scary error announcement",
            "--severity",
            "error",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["review", "msg-check", "--verdict", "approved"])
        .assert()
        .success();
    // An error-severity message is an announcement, not policy: check is green.
    repo.arc(&repo.root)
        .args(["check", "msg-check"])
        .assert()
        .success();
}

#[test]
fn status_lists_messages_and_assignment_without_changing_schema() {
    let repo = Repo::new();
    begin_change(&repo, "msg-status", None);

    repo.arc(&repo.root)
        .args([
            "message",
            "msg-status",
            "--type",
            "status",
            "--summary",
            "work started",
            "--severity",
            "info",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "message",
            "msg-status",
            "--type",
            "discovery",
            "--summary",
            "edge case found",
            "--detail",
            "consumer needs context",
            "--severity",
            "warning",
            "--json",
            "{\"issue\":42}",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["metadata", "msg-status", "--assign", "codex"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", "msg-status"]));
    assert_eq!(status["schema"], "arc-status/8");
    assert_eq!(status["assigned_to"], "codex");
    let messages = status["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["message_type"], "status");
    assert_eq!(messages[0]["severity"], "info");
    assert_eq!(messages[0]["summary"], "work started");
    assert_eq!(messages[0]["detail"], serde_json::Value::Null);
    assert_eq!(messages[0]["metadata"], serde_json::Value::Null);
    assert_eq!(messages[1]["message_type"], "discovery");
    assert_eq!(messages[1]["severity"], "warning");
    assert_eq!(messages[1]["summary"], "edge case found");
    assert_eq!(messages[1]["detail"], "consumer needs context");
    assert_eq!(messages[1]["metadata"]["issue"], 42);
    for message in messages {
        assert!(message["event_id"].is_string());
        assert_eq!(message["actor"], "tester");
        assert_eq!(message["harness"], "test");
        assert_eq!(message["session"], "session-a");
        assert!(message["created_at"].is_string());
    }

    repo.arc(&repo.root)
        .args(["metadata", "msg-status", "--assign", ""])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "msg-status"]));
    assert_eq!(status["assigned_to"], serde_json::Value::Null);
}

#[test]
fn inbox_buckets_classify_open_changes() {
    let repo = Repo::new();

    let (review_id, ..) = change_with_patchset(&repo, "inbox-review");

    let (cr_id, ..) = change_with_patchset(&repo, "inbox-cr");
    repo.arc(&repo.root)
        .args([
            "review",
            "inbox-cr",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();

    let (ready_id, ..) = change_with_patchset(&repo, "inbox-ready");
    repo.arc(&repo.root)
        .args(["review", "inbox-ready", "--verdict", "approved"])
        .assert()
        .success();

    let blocker_id = begin_change(&repo, "inbox-blocker", None);
    let blocked_id = begin_change(&repo, "inbox-blocked", Some(&blocker_id));

    let (held_id, ..) = change_with_patchset(&repo, "inbox-held");
    repo.arc(&repo.root)
        .args(["hold", "inbox-held", "--reason", "pause"])
        .assert()
        .success();

    let stalled_id = begin_change(&repo, "inbox-stalled", None);
    repo.arc(&repo.root)
        .args(["claim", "inbox-stalled"])
        .assert()
        .success();
    age_event(&repo, &stalled_id, "claim-set", 120);

    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    assert_eq!(inbox["schema"], "arc-inbox/3");
    assert!(bucket_has(&inbox, "needs-review", &review_id));
    assert!(bucket_has(&inbox, "changes-requested", &cr_id));
    assert!(bucket_has(&inbox, "ready-to-integrate", &ready_id));
    assert!(bucket_has(&inbox, "blocked", &blocked_id));
    assert!(bucket_has(&inbox, "held", &held_id));
    assert!(bucket_has(&inbox, "stalled", &stalled_id));
}

#[test]
fn inbox_claim_rows_show_active_details_and_keep_stale_claims_exclusive() {
    let repo = Repo::new();
    let active_id = begin_change(&repo, "inbox-active", None);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "worker")
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", "run-7")
        .args(["claim", "inbox-active", "--stage-budget", "implementing=1m"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "worker")
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", "run-7")
        .args(["stage", "inbox-active", "implementing"])
        .assert()
        .success();

    let stalled_id = begin_change(&repo, "inbox-stale", None);
    repo.arc(&repo.root)
        .args(["claim", "inbox-stale", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    age_event(&repo, &stalled_id, "claim-set", 2);

    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    let active = inbox["in-progress"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["change_id"] == active_id)
        .unwrap();
    assert_eq!(active["next_actor"], "codex");
    assert_eq!(active["owner"]["actor"], "worker");
    assert_eq!(active["owner"]["harness"], "codex");
    assert_eq!(active["owner"]["session"], "run-7");
    assert_eq!(active["stage"], "implementing");
    assert!(active["age_seconds"].is_u64());
    assert!(!bucket_has(&inbox, "stalled", &active_id));

    let stalled = inbox["stalled"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["change_id"] == stalled_id)
        .unwrap();
    assert_eq!(stalled["next_actor"], "test");
    assert_eq!(stalled["owner"]["harness"], "test");
    assert_eq!(stalled["stage"], "launch");
    assert!(stalled["age_seconds"].is_u64());
    assert!(!bucket_has(&inbox, "in-progress", &stalled_id));

    let text = stdout(repo.arc(&repo.root).args(["inbox"]));
    let active_line = text
        .lines()
        .find(|line| {
            line.contains(&active_id)
                && line.contains("[owner: worker/codex/run-7; stage: implementing; age:")
        })
        .unwrap();
    assert!(active_line.contains("→ codex"));
    assert!(active_line.ends_with("s]"));
}

#[test]
fn inbox_omits_released_and_expired_claims() {
    let repo = Repo::new();
    let released_id = begin_change(&repo, "inbox-released", None);
    repo.arc(&repo.root)
        .args(["claim", "inbox-released"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["release-claim", "inbox-released"])
        .assert()
        .success();

    let expired_id = begin_change(&repo, "inbox-expired", None);
    repo.arc(&repo.root)
        .args(["claim", "inbox-expired", "--ttl", "1s"])
        .assert()
        .success();
    age_event(&repo, &expired_id, "claim-set", 2);

    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    for bucket in ["in-progress", "stalled"] {
        assert!(!bucket_has(&inbox, bucket, &released_id));
        assert!(!bucket_has(&inbox, bucket, &expired_id));
    }
}

#[test]
fn assignment_set_override_clear_and_filter() {
    let repo = Repo::new();
    let assigned_id = begin_change(&repo, "assign-a", None);
    begin_change(&repo, "assign-b", None);

    // Set, then override; latest wins.
    repo.arc(&repo.root)
        .args(["metadata", "assign-a", "--assign", "codex"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["metadata", "assign-a", "--assign", "claude"])
        .assert()
        .success();
    let show = json_stdout(repo.arc(&repo.root).args(["show", "assign-a", "--json"]));
    assert_eq!(show["assigned_to"], "claude");

    // --assigned-to filters to the one assigned change.
    let filtered =
        json_stdout(
            repo.arc(&repo.root)
                .args(["inbox", "--assigned-to", "claude", "--json"]),
        );
    assert_eq!(filtered["assigned_to"], "claude");
    assert!(bucket_has(&filtered, "needs-review", &assigned_id));
    let unfiltered_review = filtered["needs-review"].as_array().unwrap();
    assert_eq!(unfiltered_review.len(), 1);

    // Clear with an empty value; the change drops out of the filtered inbox.
    repo.arc(&repo.root)
        .args(["metadata", "assign-a", "--assign", ""])
        .assert()
        .success();
    let show = json_stdout(repo.arc(&repo.root).args(["show", "assign-a", "--json"]));
    assert_eq!(show["assigned_to"], serde_json::Value::Null);
    let filtered =
        json_stdout(
            repo.arc(&repo.root)
                .args(["inbox", "--assigned-to", "claude", "--json"]),
        );
    assert_eq!(filtered["needs-review"].as_array().unwrap().len(), 0);
}

#[test]
fn implementer_role_may_announce_and_assign() {
    let repo = Repo::new();
    let change_id = begin_change(&repo, "impl-announce", None);
    let before = event_count(&repo, &change_id);

    repo.arc(&repo.root)
        .env("ARC_ROLE", "implementer")
        .args([
            "message",
            "impl-announce",
            "--type",
            "status",
            "--summary",
            "implementer announcement",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ROLE", "implementer")
        .args(["metadata", "impl-announce", "--assign", "codex"])
        .assert()
        .success();
    assert_eq!(event_count(&repo, &change_id), before + 2);

    // Read-only surfaces are permitted for the implementer too.
    repo.arc(&repo.root)
        .env("ARC_ROLE", "implementer")
        .args(["messages", "--json"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ROLE", "implementer")
        .args(["inbox", "--json"])
        .assert()
        .success();
}

#[test]
fn inbox_names_the_journal_backlog_even_when_the_ledger_is_empty() {
    let repo = Repo::new();
    let body = repo.home.join("body.md");
    fs::write(&body, "unstarted work\n").unwrap();
    repo.arc(&repo.root)
        .args([
            "journal",
            "note",
            "waiting",
            "--kind",
            "plan",
            "--body-file",
            body.to_str().unwrap(),
        ])
        .assert()
        .success();

    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    assert_eq!(inbox["journal"]["open"], 1);
    assert_eq!(inbox["journal"]["later"], 0);
    assert_eq!(inbox["journal"]["feature-requests"], 0);
    assert_eq!(inbox["journal"]["preview"][0]["kind"], "plan");

    // The text rendering is the surface a session actually reads: an empty
    // ledger must still point at the queue that is not empty.
    let text = stdout(repo.arc(&repo.root).args(["inbox"]));
    assert!(text.contains("## journal backlog"), "{text}");
    assert!(
        text.contains("1 open, 0 later, 0 feature-request"),
        "{text}"
    );
    assert!(text.contains("arc journal open"), "{text}");
}

#[test]
fn catchup_reports_ledger_and_journal_together() {
    let repo = Repo::new();
    let body = repo.home.join("body.md");
    fs::write(&body, "parked\n").unwrap();
    repo.arc(&repo.root)
        .args([
            "journal",
            "note",
            "parked",
            "--kind",
            "later",
            "--body-file",
            body.to_str().unwrap(),
        ])
        .assert()
        .success();
    let change_id = begin_change(&repo, "catchup-change", None);

    let catchup = json_stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
    assert_eq!(catchup["schema"], "arc-catchup/1");
    assert!(bucket_has(&catchup["ledger"], "needs-review", &change_id));
    assert_eq!(catchup["journal"]["later"][0]["kind"], "later");

    let text = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(text.contains(&change_id), "{text}");
    assert!(text.contains("later (1):"), "{text}");
}
