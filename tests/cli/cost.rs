//! The work cost view over synthetic harness recordings. Every recording here
//! is written by the test into the fixture's own home, so tapes reads it
//! exactly as it would read a harness store.

use super::common::*;
use serde_json::{json, Value};

const USD: &str = "harness-calculated-estimate";

fn event_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .expect("command output should contain an event id")
        .to_string()
}

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

/// A pi recording whose assistant requests each carry a harness-written cost.
/// With `abandoned`, one priced request sits on a branch the session rewound
/// away from, outside the active branch tapes sums.
fn pi_recording(repo: &Repo, id: &str, costs: &[f64], abandoned: bool) -> PathBuf {
    let path = repo
        .home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T10-00-00-000Z_{id}.jsonl"));
    let request = |entry: &str, parent: &str, cost: f64| {
        json!({"type": "message", "id": entry, "parentId": parent, "timestamp": "2026-01-01T10:00:02Z",
            "message": {"role": "assistant", "timestamp": 1767261602000u64, "provider": "fixture", "model": "fixture-model",
                "usage": {"input": 10, "output": 20, "reasoning": 3, "cacheRead": 4, "cacheWrite": 5, "totalTokens": 39,
                    "cost": {"total": cost}},
                "content": [{"type": "text", "text": "Done."}]}})
    };
    let mut lines = vec![
        json!({"type": "session", "version": 3, "id": id, "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project"}),
        json!({"type": "message", "id": "user-1", "parentId": null, "timestamp": "2026-01-01T10:00:01Z",
            "message": {"role": "user", "timestamp": 1767261601000u64, "content": [{"type": "text", "text": "Work."}]}}),
    ];
    if abandoned {
        lines.push(request("abandoned-1", "user-1", 9.0));
    }
    let mut parent = "user-1".to_string();
    for (n, cost) in costs.iter().enumerate() {
        let entry = format!("assistant-{n}");
        lines.push(request(&entry, &parent, *cost));
        parent = entry;
    }
    write_lines(&path, &lines);
    path
}

/// A codex recording whose cumulative counter records input, cached input,
/// and output. Codex records no dollar figure.
fn codex_recording(repo: &Repo, id: &str, input: u64) -> PathBuf {
    let path = repo
        .home
        .join(".codex/sessions/2026/01/01")
        .join(format!("rollout-2026-01-01T10-00-00-{id}.jsonl"));
    write_lines(
        &path,
        &[
            json!({"timestamp": "2026-01-01T10:00:00Z", "type": "session_meta", "payload": {"id": id, "session_id": id, "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project", "model_provider": "fixture"}}),
            json!({"timestamp": "2026-01-01T10:00:01Z", "type": "turn_context", "payload": {"cwd": "/fixture/project", "model": "fixture-model", "effort": "high"}}),
            json!({"timestamp": "2026-01-01T10:00:02Z", "type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Work."}]}}),
            json!({"timestamp": "2026-01-01T10:00:03Z", "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Done."}]}}),
            json!({"timestamp": "2026-01-01T10:00:04Z", "type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": input, "cached_input_tokens": 100, "output_tokens": 50, "reasoning_output_tokens": 10, "total_tokens": input + 50},
                "last_token_usage": {"input_tokens": input, "cached_input_tokens": 100, "output_tokens": 50, "reasoning_output_tokens": 10, "total_tokens": input + 50},
                "model_context_window": 1000}, "rate_limits": null}}),
            json!({"timestamp": "2026-01-01T10:00:05Z", "type": "event_msg", "payload": {"type": "task_complete"}}),
        ],
    );
    path
}

/// A claude recording with one request's usage.
fn claude_recording(repo: &Repo, id: &str, input: u64) -> PathBuf {
    let path = repo
        .home
        .join(".claude/projects/-fixture-project")
        .join(format!("{id}.jsonl"));
    write_lines(
        &path,
        &[
            json!({"type": "user", "sessionId": id, "uuid": "prompt-1", "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project", "message": {"role": "user", "content": "Work."}}),
            json!({"type": "assistant", "sessionId": id, "uuid": "assistant-1", "requestId": "request-1", "timestamp": "2026-01-01T10:00:01Z", "cwd": "/fixture/project",
                "message": {"role": "assistant", "model": "fixture-model", "content": [{"type": "text", "text": "Done."}],
                    "usage": {"input_tokens": input, "output_tokens": 30, "cache_read_input_tokens": 200, "cache_creation_input_tokens": 40}}}),
        ],
    );
    path
}

fn dispatch(repo: &Repo, subject: &str, value: &str) -> String {
    event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        subject,
        value,
    ])))
}

fn bind(repo: &Repo, dispatch_id: &str, harness: &str, session: &str) {
    stdout(repo.arc(&repo.root).args([
        "run",
        "bind",
        dispatch_id,
        "--executor-harness",
        harness,
        "--executor-session",
        session,
    ]));
}

fn file_artifact(repo: &Repo, kind: &str, topic: &str) -> String {
    let path = stdout(
        repo.arc(&repo.root)
            .args(["journal", kind, topic, "--body-file", "-"])
            .write_stdin("# Work item\n\nBody.\n"),
    );
    Path::new(path.trim())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

fn report(repo: &Repo, target: &str) -> Value {
    let report = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", target, "--json"]),
    );
    assert_eq!(report["schema"], "arc-cost/1", "{report}");
    report
}

fn work<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["work"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == key)
        .unwrap_or_else(|| panic!("no work item {key}: {report}"))
}

fn quantity<'a>(totals: &'a Value, kind: &str, category: Option<&str>) -> &'a Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals}"))
        .iter()
        .find(|quantity| {
            quantity["kind"] == kind
                && category.is_none_or(|category| quantity["category"] == category)
                && quantity.get("harness").is_none()
        })
        .unwrap_or_else(|| panic!("no {kind} {category:?} quantity: {totals}"))
}

fn usd(totals: &Value) -> Option<f64> {
    quantity(totals, USD, None)["amount"].as_f64()
}

fn session<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["key"] == key)
        .unwrap_or_else(|| panic!("no session {key}: {report}"))
}

#[test]
fn a_shared_plan_counts_once_in_the_union_and_in_each_inclusive_view() {
    let repo = Repo::new();
    let plan = file_artifact(&repo, "plan", "shared-plan");
    let change_a = begin_without_checkout(&repo, "change-a", &["--from-journal", &plan]);
    let change_b = begin_without_checkout(&repo, "change-b", &["--from-journal", &plan]);
    pi_recording(&repo, "plan-session", &[0.02], false);
    pi_recording(&repo, "a-session", &[0.03, 0.02], false);
    pi_recording(&repo, "b-session", &[0.03], false);
    bind(
        &repo,
        &dispatch(&repo, "--journal", &plan),
        "pi",
        "plan-session",
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", &change_a),
        "pi",
        "a-session",
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", &change_b),
        "pi",
        "b-session",
    );

    let plan_report = report(&repo, &plan);
    let plan_key = format!("journal:{plan}");
    let (a_key, b_key) = (format!("change:{change_a}"), format!("change:{change_b}"));
    assert_eq!(usd(&work(&plan_report, &plan_key)["direct"]), Some(0.02));
    assert_eq!(usd(&work(&plan_report, &a_key)["direct"]), Some(0.05));
    assert_eq!(usd(&work(&plan_report, &b_key)["direct"]), Some(0.03));
    assert_eq!(usd(&work(&plan_report, &a_key)["inclusive"]), Some(0.07));
    assert_eq!(usd(&work(&plan_report, &b_key)["inclusive"]), Some(0.05));
    assert_eq!(usd(&plan_report["union"]), Some(0.10), "{plan_report}");
    let estimate = quantity(&plan_report["union"], USD, None);
    assert_eq!(estimate["unit"], "USD");
    assert_eq!(estimate["pricing_basis"], "unknown");
    assert_eq!(estimate["status"], "complete");

    let change_report = report(&repo, &change_a);
    assert_eq!(usd(&work(&change_report, &a_key)["direct"]), Some(0.05));
    assert_eq!(usd(&work(&change_report, &a_key)["inclusive"]), Some(0.07));
    assert_eq!(usd(&change_report["union"]), Some(0.07));

    let text = stdout(repo.arc(&repo.root).args(["cost", "show", &plan]));
    assert!(text.contains("harness-calculated estimate"), "{text}");
    assert!(text.contains("0.1 USD"), "{text}");
}

#[test]
fn two_spans_on_one_session_share_it_without_a_per_run_figure() {
    let repo = Repo::new();
    pi_recording(&repo, "resumed-session", &[0.04], false);
    let first = dispatch(&repo, "--change", "span-change");
    bind(&repo, &first, "pi", "resumed-session");
    let second = dispatch(&repo, "--change", "span-change");
    bind(&repo, &second, "pi", "resumed-session");

    let report = report(&repo, "span-change");
    let row = session(&report, "pi:resumed-session");
    assert_eq!(row["spans"].as_array().unwrap().len(), 2, "{report}");
    assert_eq!(row["per_run"], "shared-among-spans", "{report}");
    for run in work(&report, "change:span-change")["runs"]
        .as_array()
        .unwrap()
    {
        assert_eq!(run["per_run"], "shared-among-spans", "{report}");
        assert!(run.get("amount").is_none(), "{report}");
    }
    // Both spans are this work's, so the session is counted once for it.
    assert_eq!(
        usd(&work(&report, "change:span-change")["direct"]),
        Some(0.04)
    );

    // Spans on two different work items make the session shared between them.
    let other = dispatch(&repo, "--change", "other-change");
    bind(&repo, &other, "pi", "resumed-session");
    let report = super::cost::report(&repo, "span-change");
    let item = work(&report, "change:span-change");
    assert_eq!(item["shared"], json!(["pi:resumed-session"]), "{report}");
    assert_eq!(quantity(&item["direct"], USD, None)["status"], "absent");
    assert!(usd(&item["direct"]).is_none(), "{report}");
    let membership = &session(&report, "pi:resumed-session")["membership"];
    assert_eq!(
        membership,
        &json!(["change:other-change", "change:span-change"]),
        "{report}"
    );
}

#[test]
fn rereading_and_retaining_again_add_no_consumption() {
    let repo = Repo::new();
    pi_recording(&repo, "retained-session", &[0.05], false);
    let dispatch_id = dispatch(&repo, "--change", "reread-change");
    bind(&repo, &dispatch_id, "pi", "retained-session");
    bind(&repo, &dispatch_id, "pi", "retained-session");

    let first = report(&repo, "reread-change");
    let second = report(&repo, "reread-change");
    assert_eq!(first, second);
    assert_eq!(usd(&first["union"]), Some(0.05));

    let retained = stdout(
        repo.arc(&repo.root)
            .args(["cost", "retain", "reread-change"]),
    );
    assert!(
        retained.contains("retained: pi:retained-session"),
        "{retained}"
    );
    let again = stdout(
        repo.arc(&repo.root)
            .args(["cost", "retain", "reread-change"]),
    );
    assert!(
        again.contains("already retained: pi:retained-session"),
        "{again}"
    );
    assert_eq!(event_id(&again), event_id(&retained));
    let events = stdout(repo.arc(&repo.root).args(["events", "--repository"]));
    assert_eq!(events.matches("\"cost-facts-retained\"").count(), 1);

    let after = report(&repo, "reread-change");
    assert_eq!(usd(&after["union"]), Some(0.05), "{after}");
    let measurement = &session(&after, "pi:retained-session")["measurement"];
    assert_eq!(measurement["source"], "fresh", "{after}");
    assert!(measurement.get("conflict").is_none(), "{after}");
}

#[test]
fn a_claude_and_codex_set_refuses_a_raw_input_sum() {
    let repo = Repo::new();
    let claude_id = "11111111-1111-4111-8111-111111111111";
    let codex_id = "22222222-2222-4222-8222-222222222222";
    claude_recording(&repo, claude_id, 300);
    codex_recording(&repo, codex_id, 700);
    bind(
        &repo,
        &dispatch(&repo, "--change", "mixed"),
        "claude",
        claude_id,
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", "mixed"),
        "codex",
        codex_id,
    );

    let report = report(&repo, "mixed");
    let input = quantity(&report["union"], "resource-quantity", Some("input"));
    assert_eq!(input["status"], "refused", "{report}");
    assert!(input.get("amount").is_none(), "{report}");
    assert!(
        input["reasons"][0].as_str().unwrap().contains("input"),
        "{report}"
    );
    // Each session's own counter stays readable.
    let claude_input = quantity(
        &session(&report, &format!("claude:{claude_id}"))["totals"],
        "resource-quantity",
        Some("input"),
    );
    assert_eq!(claude_input["amount"], 300, "{report}");
    let codex_input = quantity(
        &session(&report, &format!("codex:{codex_id}"))["totals"],
        "resource-quantity",
        Some("input"),
    );
    assert_eq!(codex_input["amount"], 700, "{report}");
    let text = stdout(repo.arc(&repo.root).args(["cost", "show", "mixed"]));
    assert!(text.contains("refused"), "{text}");
}

#[test]
fn a_missing_binding_withholds_the_work_total() {
    let repo = Repo::new();
    pi_recording(&repo, "bound-session", &[0.05], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "half-bound"),
        "pi",
        "bound-session",
    );
    let unbound = dispatch(&repo, "--change", "half-bound");

    let report = report(&repo, "half-bound");
    let direct = &work(&report, "change:half-bound")["direct"];
    let estimate = quantity(direct, USD, None);
    assert_eq!(estimate["status"], "withheld", "{report}");
    assert!(estimate.get("amount").is_none(), "{report}");
    assert!(usd(&report["union"]).is_none(), "{report}");
    assert_eq!(report["coverage"]["assignment"]["status"], "incomplete");
    assert_eq!(
        report["coverage"]["assignment"]["unbound"],
        json!([unbound]),
        "{report}"
    );
    // The session's own figure is not a work total and stays available.
    assert_eq!(
        usd(&session(&report, "pi:bound-session")["totals"]),
        Some(0.05)
    );
}

#[test]
fn an_absent_amount_stays_absent_never_zero() {
    let repo = Repo::new();
    let codex_id = "33333333-3333-4333-8333-333333333333";
    codex_recording(&repo, codex_id, 500);
    pi_recording(&repo, "priced-session", &[0.05], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "absent"),
        "codex",
        codex_id,
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", "absent"),
        "pi",
        "priced-session",
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", "absent"),
        "pi",
        "no-recording-session",
    );

    let report = report(&repo, "absent");
    let estimate = quantity(&report["union"], USD, None);
    assert_eq!(estimate["status"], "absent", "{report}");
    assert!(estimate.get("amount").is_none(), "{report}");
    assert_eq!(estimate["known_subtotal"].as_f64(), Some(0.05), "{report}");
    let codex = session(&report, &format!("codex:{codex_id}"));
    let codex_estimate = quantity(&codex["totals"], USD, None);
    assert_eq!(codex_estimate["status"], "absent", "{report}");
    assert!(codex_estimate.get("amount").is_none(), "{report}");
    let missing = session(&report, "pi:no-recording-session");
    assert_eq!(missing["measurement"]["source"], "unavailable", "{report}");
    assert_eq!(missing["measurement"]["status"], "unmeasured", "{report}");
    assert!(usd(&missing["totals"]).is_none(), "{report}");
    let text = stdout(repo.arc(&repo.root).args(["cost", "show", "absent"]));
    assert!(!text.contains(" 0 USD"), "{text}");
}

#[test]
fn transition_and_begin_from_journal_add_no_consumption() {
    let repo = Repo::new();
    let todo = file_artifact(&repo, "todo", "moving-subject");
    pi_recording(&repo, "subject-session", &[0.02], false);
    bind(
        &repo,
        &dispatch(&repo, "--journal", &todo),
        "pi",
        "subject-session",
    );
    assert_eq!(usd(&report(&repo, &todo)["union"]), Some(0.02));

    repo.arc(&repo.root)
        .args(["journal", "transition", &todo, "--to", "discussion"])
        .assert()
        .success();
    let discussion = json_stdout(repo.arc(&repo.root).args([
        "journal",
        "latest",
        "moving-subject",
        "--kind",
        "discussion",
        "--json",
    ]))["file"]
        .as_str()
        .unwrap()
        .to_string();
    let after_transition = report(&repo, &discussion);
    assert_eq!(usd(&after_transition["union"]), Some(0.02));
    assert_eq!(
        after_transition["target"]["lineage"],
        json!([todo, discussion])
    );

    let change = begin_without_checkout(&repo, "from-discussion", &["--from-journal", &discussion]);
    let after_begin = report(&repo, &discussion);
    assert_eq!(usd(&after_begin["union"]), Some(0.02), "{after_begin}");
    let begun = report(&repo, &change);
    let change_key = format!("change:{change}");
    assert!(
        usd(&work(&begun, &change_key)["direct"]).is_none(),
        "{begun}"
    );
    assert_eq!(usd(&work(&begun, &change_key)["inclusive"]), Some(0.02));
}

#[test]
fn a_pi_recording_with_abandoned_entries_is_incomplete_measurement() {
    let repo = Repo::new();
    pi_recording(&repo, "rewound-session", &[0.05], true);
    bind(
        &repo,
        &dispatch(&repo, "--change", "rewound"),
        "pi",
        "rewound-session",
    );

    let report = report(&repo, "rewound");
    let measurement = &session(&report, "pi:rewound-session")["measurement"];
    assert_eq!(measurement["status"], "incomplete", "{report}");
    assert!(
        measurement["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason.as_str().unwrap().contains("abandoned")),
        "{report}"
    );
    assert_eq!(report["coverage"]["measurement"]["status"], "incomplete");
    let estimate = quantity(&work(&report, "change:rewound")["direct"], USD, None);
    assert_eq!(estimate["status"], "incomplete", "{report}");
    assert!(estimate.get("amount").is_none(), "{report}");
    assert_eq!(estimate["known_subtotal"].as_f64(), Some(0.05), "{report}");
}

#[test]
fn retained_facts_reproduce_the_report_after_the_source_rotates() {
    let repo = Repo::new();
    let path = pi_recording(&repo, "rotating-session", &[0.05], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "rotating"),
        "pi",
        "rotating-session",
    );
    let fresh = report(&repo, "rotating");
    stdout(repo.arc(&repo.root).args(["cost", "retain", "rotating"]));

    // Same length, different bytes: a fresh read at the retained frontier
    // that disagrees with the retained facts is a conflict, shown.
    let body = fs::read_to_string(&path).unwrap();
    fs::write(&path, body.replace("0.05", "0.06")).unwrap();
    let disagreeing = report(&repo, "rotating");
    let measurement = &session(&disagreeing, "pi:rotating-session")["measurement"];
    assert_eq!(measurement["source"], "fresh", "{disagreeing}");
    assert!(measurement["conflict"].is_object(), "{disagreeing}");

    fs::remove_file(&path).unwrap();
    let rotated = report(&repo, "rotating");
    let measurement = &session(&rotated, "pi:rotating-session")["measurement"];
    assert_eq!(measurement["source"], "retained", "{rotated}");
    assert_eq!(
        measurement["reader"],
        fresh["sessions"][0]["measurement"]["reader"]
    );
    assert_eq!(usd(&rotated["union"]), usd(&fresh["union"]));
    assert_eq!(rotated["union"], fresh["union"]);
    let text = stdout(repo.arc(&repo.root).args(["cost", "show", "rotating"]));
    assert!(text.contains("retained"), "{text}");
}

#[test]
fn a_never_dispatched_lead_session_is_outside_assignment_coverage() {
    let repo = Repo::new();
    let change = begin_without_checkout(&repo, "led-change", &[]);
    pi_recording(&repo, "executor-session", &[0.05], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", &change),
        "pi",
        "executor-session",
    );

    let report = report(&repo, &change);
    let outside = &report["coverage"]["assignment"]["outside"];
    let lead = outside["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["harness"] == "test" && session["session"] == "session-a")
        .unwrap_or_else(|| panic!("the lead session is listed: {report}"));
    assert!(lead.get("amount").is_none(), "{report}");
    assert!(outside["statement"]
        .as_str()
        .unwrap()
        .contains("outside assignment coverage"));
    let text = stdout(repo.arc(&repo.root).args(["cost", "show", &change]));
    assert!(text.contains("outside assignment coverage"), "{text}");
    assert!(text.contains("test:session-a"), "{text}");
}

#[test]
fn the_report_records_nothing() {
    let repo = Repo::new();
    pi_recording(&repo, "quiet-session", &[0.01], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "quiet"),
        "pi",
        "quiet-session",
    );
    let before = stdout(repo.arc(&repo.root).args(["events", "--repository"]));
    report(&repo, "quiet");
    let after = stdout(repo.arc(&repo.root).args(["events", "--repository"]));
    assert_eq!(before, after);
}
