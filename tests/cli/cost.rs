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

/// Dispatch as an orchestrator that names no session: its acting event is
/// unattributed, so it qualifies assignment and joins no figure, and the
/// figures stay those of the bound executors.
fn dispatch(repo: &Repo, subject: &str, value: &str) -> String {
    let mut anonymous = repo.arc(&repo.root);
    anonymous
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION");
    let id = event_id(&stdout(anonymous.args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        subject,
        value,
    ])));
    // Dedicated recordings begin in January; their first dispatch is earlier.
    dispatch_time(repo, &id, "2026-01-01T09:59:00Z");
    id
}

fn dispatch_time(repo: &Repo, id: &str, timestamp: &str) {
    let path = repo
        .root
        .join(".git/arc/repository/events")
        .join(format!("{id}.json"));
    let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    event["created_at"] = json!(timestamp);
    fs::write(path, serde_json::to_vec(&event).unwrap()).unwrap();
}

fn recording_lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Bind a finished run: its recorded end makes the executor's figure final.
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
    end_run(repo, dispatch_id);
}

/// Record the run's end once; a second binding of the same dispatch finds it
/// already ended.
fn end_run(repo: &Repo, dispatch_id: &str) {
    stdout_any_status(repo.arc(&repo.root).args([
        "run",
        "end",
        dispatch_id,
        "--outcome",
        "completed",
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
                && quantity["categories"] != "disjoint"
                && quantity.get("harness").is_none()
                && quantity.get("partition").is_none()
        })
        .unwrap_or_else(|| panic!("no {kind} {category:?} quantity: {totals}"))
}

/// The combined figure of one disjoint token category.
fn disjoint<'a>(totals: &'a Value, category: &str) -> &'a Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals}"))
        .iter()
        .find(|quantity| {
            quantity["categories"] == "disjoint"
                && quantity["category"] == category
                && quantity.get("partition").is_none()
        })
        .unwrap_or_else(|| panic!("no disjoint {category} quantity: {totals}"))
}

fn usd(totals: &Value) -> Option<f64> {
    let figure = quantity(totals, USD, None);
    figure["amount"]
        .as_f64()
        .or_else(|| figure["known_subtotal"].as_f64())
}

fn recorded_bound(quantity: &Value) -> &Value {
    if quantity["bound"].is_object() {
        &quantity["bound"]
    } else {
        &quantity["known_bound"]
    }
}

#[test]
fn recorded_token_counters_preserve_integers_above_the_float_exact_range() {
    let repo = Repo::new();
    let id = "22222222-2222-4222-8222-222222222222";
    let input = 9_007_199_254_740_993_u64;
    codex_recording(&repo, id, input);
    bind(
        &repo,
        &dispatch(&repo, "--change", "large-counter"),
        "codex",
        id,
    );
    let view = report(&repo, "large-counter");
    assert_eq!(
        quantity(&view["union"], "resource-quantity", Some("input"))["amount"],
        input
    );
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
fn review_pre_dispatch_consumption_withholds_assignment_after_retention() {
    let repo = Repo::new();
    let path = pi_recording(&repo, "preexisting", &[0.03], false);
    let run = dispatch(&repo, "--change", "resumed");
    dispatch_time(&repo, &run, "2026-10-01T00:00:00Z");
    bind(&repo, &run, "pi", "preexisting");
    let fresh = report(&repo, "resumed");
    assert_eq!(
        fresh["coverage"]["assignment"]["status"], "incomplete",
        "{fresh}"
    );
    for totals in [&fresh["union"], &work(&fresh, "change:resumed")["direct"]] {
        let figure = quantity(totals, USD, None);
        assert_eq!(figure["status"], "withheld");
        assert!(figure.get("amount").is_none() && figure.get("known_subtotal").is_none());
        assert!(figure["reasons"]
            .to_string()
            .contains("pre-dispatch consumption"));
    }
    assert!(session(&fresh, "pi:preexisting")["measurement"]["source_started_at"].is_string());
    assert_eq!(
        usd(&session(&fresh, "pi:preexisting")["totals"]),
        Some(0.03)
    );
    stdout(repo.arc(&repo.root).args(["cost", "retain", "resumed"]));
    fs::remove_file(path).unwrap();
    let rotated = report(&repo, "resumed");
    assert_eq!(
        rotated["coverage"]["assignment"],
        fresh["coverage"]["assignment"]
    );
    assert_eq!(rotated["union"], fresh["union"]);
}

#[test]
fn review_native_pi_parent_path_resolves_to_its_id_and_is_never_transported() {
    let repo = Repo::new();
    let change = begin_without_checkout(&repo, "native-fork", &[]);
    let parent_id = "11111111-1111-4111-8111-111111111111";
    let fork_id = "22222222-2222-4222-8222-222222222222";
    let parent = pi_recording(&repo, parent_id, &[0.03], false);
    let original = pi_recording(&repo, fork_id, &[0.03, 0.02], false);
    let mut lines = recording_lines(&original);
    lines[0]["timestamp"] = json!("2026-10-01T10:00:00Z");
    let fork = original
        .parent()
        .unwrap()
        .join(format!("2026-10-01T10-00-00-000Z_{fork_id}.jsonl"));
    write_lines(&fork, &lines);
    fs::remove_file(original).unwrap();
    // Pi createBranchedSession and forkFrom write the source recording path.
    name_pi_parent(&fork, parent.to_str().unwrap());
    for id in [parent_id, fork_id] {
        let run = dispatch(&repo, "--change", &change);
        if id == fork_id {
            dispatch_time(&repo, &run, "2026-10-01T09:59:00Z");
        }
        bind(&repo, &run, "pi", id);
    }
    let fresh = report(&repo, &change);
    assert_eq!(
        fresh["coverage"]["identity"]["status"], "incomplete",
        "{fresh}"
    );
    assert!(usd(&fresh["union"]).is_none());
    assert_eq!(fresh["coverage"]["assignment"]["status"], "incomplete");
    assert!(fresh["coverage"]["assignment"]["reasons"]
        .to_string()
        .contains("pre-dispatch consumption"));
    // The reader derives the parent's session id from the recorded path.
    let lineage = &session(&fresh, &format!("pi:{fork_id}"))["measurement"]["lineage"];
    assert_eq!(lineage["parent"], parent_id, "{fresh}");
    assert_eq!(lineage["unknown"], json!([]));
    assert_eq!(
        fresh["union"]["identity"]["groups"],
        json!([[format!("pi:{parent_id}"), format!("pi:{fork_id}")]])
    );
    stdout(repo.arc(&repo.root).args(["cost", "retain", &change]));
    let bundle = repo.home.join("native-export.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &change, "--output", bundle.to_str().unwrap()]),
    );
    let retained: Value = serde_json::from_slice(&fs::read(bundle).unwrap()).unwrap();
    for event in retained["repository_events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event_type"] == "cost-facts-retained")
    {
        assert!(
            !event["facts"]
                .to_string()
                .contains(repo.home.to_str().unwrap()),
            "{event}"
        );
    }
    assert!(!fresh.to_string().contains(parent.to_str().unwrap()));
    fs::remove_file(parent).unwrap();
    fs::remove_file(fork).unwrap();
    let rotated = report(&repo, &change);
    assert_eq!(rotated["union"], fresh["union"]);
}

#[test]
fn review_lineage_error_text_is_replaced_by_fixed_categories() {
    let repo = Repo::new();
    let id = "44444444-4444-4444-8444-444444444444";
    let path = claude_recording(&repo, id, 100);
    let children = path.with_extension("").join("subagents");
    fs::create_dir_all(children.parent().unwrap()).unwrap();
    fs::write(children, "not a directory").unwrap();
    bind(
        &repo,
        &dispatch(&repo, "--change", "private-error"),
        "claude",
        id,
    );
    let fresh = report(&repo, "private-error");
    let lineage = &fresh["sessions"][0]["measurement"]["lineage"];
    assert_eq!(
        lineage["unknown"],
        json!(["lineage read failed"]),
        "{fresh}"
    );
    stdout(
        repo.arc(&repo.root)
            .args(["cost", "retain", "private-error"]),
    );
    let events = stdout(repo.arc(&repo.root).args(["events", "--repository"]));
    for event in events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|e| e["event_type"] == "cost-facts-retained")
    {
        assert!(!event["facts"]
            .to_string()
            .contains(repo.home.to_str().unwrap()));
    }
}

#[test]
fn native_lineage_notes_are_counted_as_unknown_without_retaining_their_text() {
    let repo = Repo::new();
    let parent_id = "33333333-3333-4333-8333-333333333333";
    let parent = claude_recording(&repo, parent_id, 100);
    let child_dir = parent.with_extension("").join("subagents");
    fs::create_dir_all(&child_dir).unwrap();
    fs::write(child_dir.join("agent-aa11bb22.jsonl"), "\n").unwrap();
    fs::write(child_dir.join("agent-aa11bb22.meta.json"), "not json").unwrap();
    claude_recording(&repo, "44444444-4444-4444-8444-444444444444", 200);
    let change = begin_without_checkout(&repo, "lineage-notes", &[]);
    for id in [parent_id, "44444444-4444-4444-8444-444444444444"] {
        bind(&repo, &dispatch(&repo, "--change", &change), "claude", id);
    }
    let view = report(&repo, &change);
    let unknown =
        &session(&view, &format!("claude:{parent_id}"))["measurement"]["lineage"]["unknown"];
    assert!(
        unknown
            .as_array()
            .unwrap()
            .contains(&json!("1 lineage notes")),
        "{unknown}"
    );
    assert_eq!(view["coverage"]["identity"]["status"], "incomplete");
    assert!(!view.to_string().contains(child_dir.to_str().unwrap()));
    stdout(repo.arc(&repo.root).args(["cost", "retain", &change]));
    let bundle = repo.home.join("notes.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &change, "--output", bundle.to_str().unwrap()]),
    );
    assert!(!fs::read_to_string(bundle)
        .unwrap()
        .contains(child_dir.to_str().unwrap()));
}

#[test]
fn review_legacy_retained_locators_are_sanitized_and_refused_for_transport() {
    let repo = Repo::new();
    let change = begin_without_checkout(&repo, "legacy-private", &[]);
    let recording = pi_recording(&repo, "legacy-private", &[0.03], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", &change),
        "pi",
        "legacy-private",
    );
    stdout(repo.arc(&repo.root).args(["cost", "retain", &change]));
    let input = repo.home.join("unsafe-import.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &change, "--output", input.to_str().unwrap()]),
    );
    let mut exported: Value = serde_json::from_slice(&fs::read(&input).unwrap()).unwrap();
    let imported = exported["repository_events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "cost-facts-retained")
        .unwrap();
    imported["facts"]["lineage"]["unknown"] =
        json!([format!("cannot read {}", recording.display())]);
    fs::write(&input, serde_json::to_vec(&exported).unwrap()).unwrap();
    let refusal = repo
        .arc(&repo.root)
        .args(["import", input.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!refusal.status.success());
    let stderr = String::from_utf8_lossy(&refusal.stderr);
    assert!(stderr.contains("cost facts transport refused"));
    assert!(!stderr.contains(recording.to_str().unwrap()));
    let values = stdout(repo.arc(&repo.root).args(["events", "--repository"]));
    let mut held = values
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|e| e["event_type"] == "cost-facts-retained")
        .unwrap();
    held["facts"]["lineage"]["parent"] = json!(recording);
    held["facts"]["lineage"]["unknown"] = json!([format!("cannot read {}", recording.display())]);
    let path = repo
        .root
        .join(".git/arc/repository/events")
        .join(format!("{}.json", held["event_id"].as_str().unwrap()));
    fs::write(path, serde_json::to_vec(&held).unwrap()).unwrap();
    fs::remove_file(&recording).unwrap();
    let view = report(&repo, &change);
    assert!(
        !view.to_string().contains(recording.to_str().unwrap()),
        "{view}"
    );
    let bundle = repo.home.join("unsafe-export.json");
    let output = repo
        .arc(&repo.root)
        .args(["export", &change, "--output", bundle.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(recording.to_str().unwrap()));
    assert!(!bundle.exists());
}

#[test]
fn review_conflicting_bindings_withhold_assignment_and_keep_each_session() {
    let repo = Repo::new();
    let change = begin_without_checkout(&repo, "conflicted-cost", &[]);
    for id in ["executor-one", "executor-two"] {
        pi_recording(&repo, id, &[0.03], false);
    }
    let run = dispatch(&repo, "--change", &change);
    bind(&repo, &run, "pi", "executor-one");
    let path = repo.home.join("conflict.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &change, "--output", path.to_str().unwrap()]),
    );
    let mut bundle: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let events = bundle["repository_events"].as_array_mut().unwrap();
    let mut second = events
        .iter()
        .find(|e| e["event_type"] == "run-bound")
        .unwrap()
        .clone();
    second["event_id"] = json!("01K0000000000000000000000Z");
    second["executor_session"] = json!("executor-two");
    events.push(second);
    fs::write(&path, serde_json::to_vec(&bundle).unwrap()).unwrap();
    stdout(
        repo.arc(&repo.root)
            .args(["import", path.to_str().unwrap()]),
    );
    let view = report(&repo, &change);
    assert_eq!(
        view["coverage"]["assignment"]["status"], "incomplete",
        "{view}"
    );
    assert_eq!(view["sessions"].as_array().unwrap().len(), 2);
    for totals in [
        &view["union"],
        &work(&view, &format!("change:{change}"))["direct"],
    ] {
        let amount = quantity(totals, USD, None);
        assert_eq!(amount["status"], "withheld");
        assert!(amount.get("amount").is_none() && amount.get("known_subtotal").is_none());
        assert!(amount["reasons"].to_string().contains("binding conflict"));
    }
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
    // Every counted request recorded a price, so the sum is complete.
    assert_eq!(estimate["status"], "complete", "{plan_report}");
    assert_eq!(estimate["amount"].as_f64(), Some(0.10));

    let change_report = report(&repo, &change_a);
    assert_eq!(usd(&work(&change_report, &a_key)["direct"]), Some(0.05));
    assert_eq!(usd(&work(&change_report, &a_key)["inclusive"]), Some(0.07));
    assert_eq!(usd(&change_report["union"]), Some(0.07));

    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--detail", &plan]),
    );
    assert!(text.contains("harness-calculated estimate"), "{text}");
    assert!(text.contains("0.1 USD"), "{text}");
}

/// Both runs stay open across the whole recording, so no request lies in one
/// run alone.
#[test]
fn two_spans_on_one_session_are_divided_per_run_and_by_work_item() {
    let repo = Repo::new();
    pi_recording(&repo, "resumed-session", &[0.04], false);
    let first = dispatch(&repo, "--change", "span-change");
    bind(&repo, &first, "pi", "resumed-session");
    let second = dispatch(&repo, "--change", "span-change");
    bind(&repo, &second, "pi", "resumed-session");

    let report = report(&repo, "span-change");
    let row = session(&report, "pi:resumed-session");
    assert_eq!(row["spans"].as_array().unwrap().len(), 2, "{report}");
    assert_eq!(row["per_run"], "split", "{report}");
    for run in work(&report, "change:span-change")["runs"]
        .as_array()
        .unwrap()
    {
        assert_eq!(run["per_run"], "split", "{report}");
        assert_eq!(usd(&run["totals"]), Some(0.0), "{report}");
    }
    // Both spans are this work's, so the session is counted once for it.
    assert_eq!(
        usd(&work(&report, "change:span-change")["direct"]),
        Some(0.04)
    );

    // Spans on two different work items divide the session between them;
    // the request lies in both, so it is unattributed overlap.
    let other = dispatch(&repo, "--change", "other-change");
    bind(&repo, &other, "pi", "resumed-session");
    let report = super::cost::report(&repo, "span-change");
    let item = work(&report, "change:span-change");
    assert_eq!(item["shared"], json!([]), "{report}");
    assert_eq!(
        item["direct"]["shares"],
        json!(["pi:resumed-session"]),
        "{report}"
    );
    assert_eq!(usd(&item["direct"]), Some(0.0), "{report}");
    let split = &session(&report, "pi:resumed-session")["split"];
    assert_eq!(
        split["unattributed"]["reasons"],
        json!(["overlap"]),
        "{report}"
    );
    assert_eq!(
        usd(&split["unattributed"]["totals"]),
        Some(0.04),
        "{report}"
    );
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
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--detail", "mixed"]),
    );
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
    assert_eq!(estimate["status"], "withheld", "{report}");
    assert!(estimate.get("amount").is_none(), "{report}");
    assert!(estimate.get("known_subtotal").is_none(), "{report}");
    assert!(estimate.get("bound").is_none(), "{report}");
    assert!(estimate.get("known_bound").is_none());
    assert!(estimate["reasons"]
        .to_string()
        .contains("unknown source start"));
    assert_eq!(
        usd(&session(&report, "pi:priced-session")["totals"]),
        Some(0.05)
    );
    assert_eq!(report["union"]["identity"]["status"], "incomplete");
    let codex = session(&report, &format!("codex:{codex_id}"));
    let codex_estimate = quantity(&codex["totals"], USD, None);
    assert_eq!(codex_estimate["status"], "absent", "{report}");
    assert!(codex_estimate.get("amount").is_none(), "{report}");
    let missing = session(&report, "pi:no-recording-session");
    assert_eq!(missing["measurement"]["source"], "unavailable", "{report}");
    assert_eq!(missing["measurement"]["status"], "unmeasured", "{report}");
    assert!(usd(&missing["totals"]).is_none(), "{report}");
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--detail", "absent"]),
    );
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
fn a_rewound_pi_recording_counts_consumption_on_every_retained_branch() {
    let repo = Repo::new();
    let path = pi_recording(&repo, "rewound-session", &[0.05], true);
    bind(
        &repo,
        &dispatch(&repo, "--change", "rewound"),
        "pi",
        "rewound-session",
    );

    let report = report(&repo, "rewound");
    let measurement = &session(&report, "pi:rewound-session")["measurement"];
    assert_eq!(measurement["selection"], "retained-branches", "{report}");
    assert_eq!(measurement["basis"], "summed-requests");
    let events = &measurement["events"];
    assert_eq!(
        (
            &events["counted"],
            &events["active_branch"],
            &events["other_branches"]
        ),
        (&json!(2), &json!(1), &json!(1)),
        "{report}"
    );
    assert_eq!(events["identity"]["recording_entry"], 2, "{report}");
    assert_eq!(measurement["status"], "complete", "{report}");
    let estimate = quantity(&work(&report, "change:rewound")["direct"], USD, None);
    assert_eq!(estimate["status"], "complete", "{report}");
    assert_eq!(estimate["amount"].as_f64(), Some(9.05), "{report}");
    let input = quantity(&report["union"], "resource-quantity", Some("input"));
    assert_eq!(
        (&input["status"], &input["amount"]),
        (&json!("complete"), &json!(20))
    );
    stdout(repo.arc(&repo.root).args(["cost", "retain", "rewound"]));
    fs::remove_file(path).unwrap();
    let rotated = super::cost::report(&repo, "rewound");
    assert_eq!(rotated["union"], report["union"]);
    assert_eq!(
        session(&rotated, "pi:rewound-session")["measurement"]["events"],
        *events
    );
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
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--detail", "rotating"]),
    );
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
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--detail", &change]),
    );
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

fn name_pi_parent(path: &Path, parent: &str) {
    let mut lines: Vec<Value> = fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    lines[0]["parentSession"] = json!(parent);
    write_lines(path, &lines);
}

#[test]
fn a_bound_fork_withholds_exact_unions_and_retains_identity_after_rotation() {
    let repo = Repo::new();
    let plan = file_artifact(&repo, "plan", "fork-plan");
    let change = begin_without_checkout(&repo, "fork-work", &["--from-journal", &plan]);
    let parent = pi_recording(&repo, "parent", &[0.04], false);
    let fork = pi_recording(&repo, "fork", &[0.04, 0.02], false);
    name_pi_parent(&fork, "parent");
    bind(&repo, &dispatch(&repo, "--journal", &plan), "pi", "parent");
    bind(&repo, &dispatch(&repo, "--change", &change), "pi", "fork");
    let fresh = report(&repo, &change);
    let inclusive = &work(&fresh, &format!("change:{change}"))["inclusive"];
    for total in [&fresh["union"], inclusive] {
        assert_eq!(total["identity"]["status"], "incomplete", "{fresh}");
        assert_eq!(
            total["identity"]["groups"],
            json!([["pi:fork", "pi:parent"]])
        );
        let estimate = quantity(total, USD, None);
        assert_eq!(estimate["status"], "withheld");
        assert!(estimate.get("amount").is_none());
        assert!(estimate.get("known_subtotal").is_none());
        assert_eq!(recorded_bound(estimate)["at_least"], 0.06);
        assert_eq!(recorded_bound(estimate)["at_most"], 0.10);
    }
    assert_eq!(fresh["coverage"]["identity"]["status"], "incomplete");
    assert_eq!(
        work(&fresh, &format!("change:{change}"))["direct"]["identity"]["status"],
        "complete"
    );
    assert_eq!(usd(&session(&fresh, "pi:parent")["totals"]), Some(0.04));
    assert_eq!(usd(&session(&fresh, "pi:fork")["totals"]), Some(0.06));
    stdout(repo.arc(&repo.root).args(["cost", "retain", &change]));
    fs::remove_file(parent).unwrap();
    fs::remove_file(fork).unwrap();
    let rotated = report(&repo, &change);
    assert_eq!(rotated["union"], fresh["union"]);
    assert_eq!(
        rotated["coverage"]["identity"],
        fresh["coverage"]["identity"]
    );
    assert_eq!(
        session(&rotated, "pi:fork")["measurement"]["lineage"]["parent"],
        "parent"
    );
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--detail", &change]),
    );
    assert!(
        text.contains("at least 0.06 USD, at most 0.1 USD"),
        "{text}"
    );
    assert!(text.contains("identity: incomplete"), "{text}");
}

#[test]
fn unrelated_bound_sessions_have_exact_complete_identity() {
    let repo = Repo::new();
    for (id, amount) in [("first", 0.02), ("second", 0.03)] {
        pi_recording(&repo, id, &[amount], false);
        bind(&repo, &dispatch(&repo, "--change", "unrelated"), "pi", id);
    }
    let report = report(&repo, "unrelated");
    assert_eq!(usd(&report["union"]), Some(0.05));
    assert_eq!(report["union"]["identity"]["status"], "complete");
    assert_eq!(report["union"]["identity"]["groups"], json!([]));
    assert_eq!(report["coverage"]["identity"]["status"], "complete");
    assert_eq!(
        work(&report, "change:unrelated")["direct"]["identity"]["status"],
        "complete"
    );
}

#[test]
fn a_failed_lineage_read_is_unknown_and_survives_retention() {
    let repo = Repo::new();
    let id = "44444444-4444-4444-8444-444444444444";
    let recording = claude_recording(&repo, id, 100);
    let child_dir = recording.with_extension("").join("subagents");
    fs::create_dir_all(child_dir.parent().unwrap()).unwrap();
    fs::write(&child_dir, "not a directory").unwrap();
    let other = claude_recording(&repo, "55555555-5555-4555-8555-555555555555", 200);
    for session in [id, "55555555-5555-4555-8555-555555555555"] {
        bind(
            &repo,
            &dispatch(&repo, "--change", "failed-lineage"),
            "claude",
            session,
        );
    }
    let fresh = report(&repo, "failed-lineage");
    let measured = &session(&fresh, &format!("claude:{id}"))["measurement"];
    assert_eq!(measured["status"], "incomplete", "{fresh}");
    assert_eq!(
        measured["lineage"]["unknown"],
        json!(["lineage read failed"])
    );
    assert_eq!(fresh["coverage"]["identity"]["status"], "incomplete");
    let input = quantity(&fresh["union"], "resource-quantity", Some("input"));
    assert_eq!(input["status"], "withheld");
    assert!(input.get("amount").is_none());
    assert_eq!(recorded_bound(input)["at_least"], 200);
    assert_eq!(recorded_bound(input)["at_most"], 300);
    stdout(
        repo.arc(&repo.root)
            .args(["cost", "retain", "failed-lineage"]),
    );
    fs::remove_file(recording).unwrap();
    fs::remove_file(other).unwrap();
    let rotated = report(&repo, "failed-lineage");
    assert_eq!(rotated["union"], fresh["union"]);
    assert_eq!(
        rotated["coverage"]["identity"],
        fresh["coverage"]["identity"]
    );
}

#[test]
fn transitive_relatives_form_one_overlap_group_beside_disjoint_consumption() {
    let repo = Repo::new();
    for (id, parent, amount) in [
        ("first", None, 0.02),
        ("second", Some("first"), 0.03),
        ("third", Some("second"), 0.04),
        ("disjoint", None, 0.01),
    ] {
        let path = pi_recording(&repo, id, &[amount], false);
        if let Some(parent) = parent {
            name_pi_parent(&path, parent);
        }
        bind(&repo, &dispatch(&repo, "--change", "transitive"), "pi", id);
    }
    let report = report(&repo, "transitive");
    for total in [
        &report["union"],
        &work(&report, "change:transitive")["direct"],
    ] {
        assert_eq!(
            total["identity"]["groups"],
            json!([["pi:first", "pi:second", "pi:third"]])
        );
        let estimate = quantity(total, USD, None);
        assert_eq!(recorded_bound(estimate)["at_least"], 0.05);
        assert_eq!(recorded_bound(estimate)["at_most"], 0.10);
        assert!(estimate.get("amount").is_none());
    }
}

#[test]
fn a_codex_fork_header_withholds_token_unions() {
    let repo = Repo::new();
    let parent = "66666666-6666-4666-8666-666666666666";
    let fork = "77777777-7777-4777-8777-777777777777";
    codex_recording(&repo, parent, 500);
    let path = codex_recording(&repo, fork, 700);
    let mut lines: Vec<Value> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    lines[0]["payload"]["forked_from_id"] = json!(parent);
    write_lines(&path, &lines);
    for id in [parent, fork] {
        bind(
            &repo,
            &dispatch(&repo, "--change", "codex-fork"),
            "codex",
            id,
        );
    }
    let report = report(&repo, "codex-fork");
    assert_eq!(report["union"]["identity"]["status"], "incomplete");
    let input = quantity(&report["union"], "resource-quantity", Some("input"));
    assert!(input.get("amount").is_none());
    assert_eq!(recorded_bound(input)["at_least"], 700);
    assert_eq!(recorded_bound(input)["at_most"], 1200);
    assert_eq!(
        session(&report, &format!("codex:{fork}"))["measurement"]["lineage"]["forked_from"],
        parent
    );
}

#[test]
fn unknown_lineage_does_not_spoil_figures_that_exclude_that_session() {
    let repo = Repo::new();
    let plan = file_artifact(&repo, "plan", "unknown-plan");
    let change = begin_without_checkout(&repo, "known-work", &["--from-journal", &plan]);
    for id in ["first", "second"] {
        pi_recording(&repo, id, &[0.02], false);
        bind(&repo, &dispatch(&repo, "--change", &change), "pi", id);
    }
    bind(&repo, &dispatch(&repo, "--journal", &plan), "pi", "missing");
    let report = report(&repo, &change);
    let item = work(&report, &format!("change:{change}"));
    assert_eq!(item["direct"]["identity"]["status"], "complete");
    assert_eq!(usd(&item["direct"]), Some(0.04));
    assert_eq!(item["inclusive"]["identity"]["status"], "incomplete");
    assert!(usd(&item["inclusive"]).is_none());
    assert_eq!(report["coverage"]["identity"]["status"], "incomplete");
}

#[test]
fn siblings_naming_an_unbound_parent_may_overlap() {
    let repo = Repo::new();
    for id in ["first", "second"] {
        let path = pi_recording(&repo, id, &[0.02], false);
        name_pi_parent(&path, "unbound-parent");
        bind(&repo, &dispatch(&repo, "--change", "siblings"), "pi", id);
    }
    let report = report(&repo, "siblings");
    assert_eq!(
        report["union"]["identity"]["groups"],
        json!([["pi:first", "pi:second"]])
    );
    let estimate = quantity(&report["union"], USD, None);
    assert_eq!(recorded_bound(estimate)["at_least"], 0.02);
    assert_eq!(recorded_bound(estimate)["at_most"], 0.04);
}

fn bind_fork(repo: &Repo, dispatch_id: &str, harness: &str, session: &str, origin: &str) {
    stdout(repo.arc(&repo.root).args([
        "run",
        "bind",
        dispatch_id,
        "--executor-harness",
        harness,
        "--executor-session",
        session,
        "--forked-from",
        origin,
    ]));
    end_run(repo, dispatch_id);
}

#[test]
fn a_declared_claude_fork_withholds_union_after_retention_and_rotation() {
    let repo = Repo::new();
    let origin_id = "88888888-8888-4888-8888-888888888888";
    let fork_id = "99999999-9999-4999-8999-999999999999";
    let origin = claude_recording(&repo, origin_id, 100);
    let fork = claude_recording(&repo, fork_id, 200);
    bind(
        &repo,
        &dispatch(&repo, "--change", "claude-fork"),
        "claude",
        origin_id,
    );
    bind_fork(
        &repo,
        &dispatch(&repo, "--change", "claude-fork"),
        "claude",
        fork_id,
        origin_id,
    );
    let fresh = report(&repo, "claude-fork");
    assert_eq!(fresh["union"]["identity"]["status"], "incomplete");
    assert_eq!(
        work(&fresh, "change:claude-fork")["direct"]["identity"]["status"],
        "incomplete"
    );
    let input = quantity(&fresh["union"], "resource-quantity", Some("input"));
    assert_eq!(input["status"], "withheld");
    assert!(input.get("amount").is_none());
    assert_eq!(recorded_bound(input)["at_least"], 200);
    assert_eq!(recorded_bound(input)["at_most"], 300);
    let fork_row = session(&fresh, &format!("claude:{fork_id}"));
    assert!(fork_row["measurement"]["lineage"]["forked_from"].is_null());
    assert_eq!(fork_row["spans"][0]["forked_from"], origin_id);
    assert_eq!(
        quantity(&fork_row["totals"], "resource-quantity", Some("input"))["amount"],
        200
    );
    stdout(repo.arc(&repo.root).args(["cost", "retain", "claude-fork"]));
    fs::remove_file(origin).unwrap();
    fs::remove_file(fork).unwrap();
    let rotated = report(&repo, "claude-fork");
    assert_eq!(rotated["union"], fresh["union"]);
    assert_eq!(
        rotated["coverage"]["identity"],
        fresh["coverage"]["identity"]
    );
    assert_eq!(
        session(&rotated, &format!("claude:{fork_id}"))["measurement"]["source"],
        "retained"
    );
}

#[test]
fn a_declared_claude_fork_has_complete_identity_when_its_origin_is_outside_the_figure() {
    let repo = Repo::new();
    let origin_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let fork_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let unrelated_id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    claude_recording(&repo, origin_id, 100);
    let fork = claude_recording(&repo, fork_id, 200);
    let unrelated = claude_recording(&repo, unrelated_id, 50);
    bind(
        &repo,
        &dispatch(&repo, "--change", "other-work"),
        "claude",
        origin_id,
    );
    bind_fork(
        &repo,
        &dispatch(&repo, "--change", "fork-only"),
        "claude",
        fork_id,
        origin_id,
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", "fork-only"),
        "claude",
        unrelated_id,
    );
    let fresh = report(&repo, "fork-only");
    assert_eq!(fresh["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(fresh["union"]["identity"]["status"], "complete");
    assert_eq!(fresh["coverage"]["identity"]["status"], "complete");
    assert_eq!(
        quantity(&fresh["union"], "resource-quantity", Some("input"))["amount"],
        250
    );
    stdout(repo.arc(&repo.root).args(["cost", "retain", "fork-only"]));
    fs::remove_file(fork).unwrap();
    fs::remove_file(unrelated).unwrap();
    let rotated = report(&repo, "fork-only");
    assert_eq!(rotated["union"], fresh["union"]);
}

#[test]
fn review_journal_aliases_select_the_same_runs_and_changes_through_transitions() {
    let repo = Repo::new();
    let plan = file_artifact(&repo, "plan", "aliases");
    let dir = stdout(repo.arc(&repo.root).args(["journal", "dir"]));
    let qualified = format!("{}::{plan}", dir.trim());
    let equivalent = format!("{}/.::{plan}", dir.trim());
    for (reference, id, amount) in [
        (&plan, "bare", 0.01),
        (&qualified, "qualified", 0.02),
        (&equivalent, "equivalent", 0.03),
    ] {
        pi_recording(&repo, id, &[amount], false);
        bind(&repo, &dispatch(&repo, "--journal", reference), "pi", id);
    }
    for (slug, reference, amount) in [
        ("from-bare", &plan, 0.04),
        ("from-qualified", &equivalent, 0.05),
    ] {
        let change = begin_without_checkout(&repo, slug, &["--from-journal", reference]);
        pi_recording(&repo, slug, &[amount], false);
        bind(&repo, &dispatch(&repo, "--change", &change), "pi", slug);
    }
    for reference in [&plan, &qualified, &equivalent] {
        let view = report(&repo, reference);
        assert_eq!(view["sessions"].as_array().unwrap().len(), 5, "{view}");
        assert_eq!(usd(&view["union"]), Some(0.15), "{view}");
    }
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "transition", &plan, "--to", "discussion"]),
    );
    let successor = json_stdout(repo.arc(&repo.root).args([
        "journal",
        "latest",
        "aliases",
        "--kind",
        "discussion",
        "--json",
    ]))["file"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        report(&repo, &successor)["sessions"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}

#[cfg(unix)]
#[test]
fn unreadable_journal_membership_qualifies_a_partial_cost_view() {
    let repo = Repo::new();
    let plan = file_artifact(&repo, "plan", "missing-alias");
    let directory = stdout(repo.arc(&repo.root).args(["journal", "dir"]));
    let alias = repo.home.join("journal-alias");
    std::os::unix::fs::symlink(directory.trim(), &alias).unwrap();
    let qualified = format!("{}::{plan}", alias.display());
    pi_recording(&repo, "missing-alias", &[0.03], false);
    bind(
        &repo,
        &dispatch(&repo, "--journal", &qualified),
        "pi",
        "missing-alias",
    );
    fs::remove_file(&alias).unwrap();
    let view = report(&repo, &plan);
    assert_eq!(
        usd(&session(&view, "pi:missing-alias")["totals"]),
        Some(0.03)
    );
    assert_eq!(view["coverage"]["selection"]["status"], "incomplete");
    assert!(usd(&view["union"]).is_none());
    assert!(!view["coverage"]["selection"]
        .to_string()
        .contains(alias.to_str().unwrap()));
    stdout(repo.arc(&repo.root).args(["cost", "retain", &plan]));
}

#[cfg(unix)]
#[test]
fn unrelated_unavailable_journal_reference_keeps_healthy_costs_and_retention() {
    let repo = Repo::new();
    let healthy = file_artifact(&repo, "plan", "healthy-query");
    let unrelated = file_artifact(&repo, "plan", "unrelated-query");
    let directory = stdout(repo.arc(&repo.root).args(["journal", "dir"]));
    let alias = repo.home.join("retired-journal-alias");
    std::os::unix::fs::symlink(directory.trim(), &alias).unwrap();
    let qualified = format!("{}::{unrelated}", alias.display());
    pi_recording(&repo, "healthy-query", &[0.03], false);
    bind(
        &repo,
        &dispatch(&repo, "--journal", &healthy),
        "pi",
        "healthy-query",
    );
    let before = report(&repo, &healthy);
    dispatch(&repo, "--journal", &qualified);
    begin_without_checkout(&repo, "unrelated-query", &["--from-journal", &qualified]);
    fs::remove_file(alias).unwrap();
    let after = report(&repo, &healthy);
    assert_eq!(after["sessions"], before["sessions"]);
    assert_eq!(after["union"], before["union"]);
    stdout(repo.arc(&repo.root).args(["cost", "retain", &healthy]));
}

#[test]
fn bad_ledger_records_and_imports_preserve_healthy_session_figures() {
    for fault in ["repository", "import", "change", "wrong-scope", "pending"] {
        let repo = Repo::new();
        let healthy = begin_without_checkout(&repo, "healthy-ledger", &[]);
        pi_recording(&repo, "healthy-ledger", &[0.03], false);
        bind(
            &repo,
            &dispatch(&repo, "--change", &healthy),
            "pi",
            "healthy-ledger",
        );
        let before = report(&repo, &healthy);
        let path = match fault {
            "repository" | "wrong-scope" | "pending" => {
                repo.root.join(".git/arc/repository/events/bad-peer.json")
            }
            "import" => repo.root.join(".git/arc/imports/bad-peer.json"),
            _ => {
                let peer = begin_without_checkout(&repo, "bad-peer", &[]);
                repo.root
                    .join(".git/arc/changes")
                    .join(peer)
                    .join("events/bad-peer.json")
            }
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if fault == "pending" {
            fs::create_dir(&path).unwrap();
        } else if fault == "wrong-scope" {
            fs::write(
                &path,
                json!({"event_id":"bad-peer", "change_id":"foreign"}).to_string(),
            )
            .unwrap();
        } else {
            fs::write(&path, "not json").unwrap();
        }
        let after = report(&repo, &healthy);
        assert_eq!(after["sessions"], before["sessions"], "{fault}");
        if fault != "change" {
            assert_eq!(after["coverage"]["ledger"]["status"], "incomplete");
            assert!(usd(&after["union"]).is_none(), "{fault}: {after}");
        } else {
            assert_eq!(after["union"], before["union"]);
        }
        let retained = stdout(repo.arc(&repo.root).args(["cost", "retain", &healthy]));
        assert!(
            retained.contains("retained: pi:healthy-ledger"),
            "{retained}"
        );
    }
}

#[test]
fn unreadable_change_opening_preserves_sessions_with_uncertain_journal_membership() {
    let repo = Repo::new();
    let plan = file_artifact(&repo, "plan", "partial-membership");
    let change = begin_without_checkout(&repo, "partial-peer", &["--from-journal", &plan]);
    pi_recording(&repo, "healthy-plan", &[0.03], false);
    pi_recording(&repo, "readable-peer", &[0.02], false);
    bind(
        &repo,
        &dispatch(&repo, "--journal", &plan),
        "pi",
        "healthy-plan",
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", &change),
        "pi",
        "readable-peer",
    );
    let events = repo
        .root
        .join(".git/arc/changes")
        .join(&change)
        .join("events");
    for entry in fs::read_dir(events).unwrap() {
        let path = entry.unwrap().path();
        let event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] == "change-opened" {
            fs::write(path, "corrupt opening").unwrap();
        }
    }
    let journal = report(&repo, &plan);
    for (id, amount) in [("pi:healthy-plan", 0.03), ("pi:readable-peer", 0.02)] {
        assert_eq!(usd(&session(&journal, id)["totals"]), Some(amount));
    }
    assert_eq!(journal["coverage"]["selection"]["status"], "incomplete");
    assert!(usd(&journal["union"]).is_none());
    let direct = report(&repo, &change);
    assert_eq!(
        usd(&work(&direct, &format!("change:{change}"))["direct"]),
        Some(0.02)
    );
    assert!(usd(&direct["union"]).is_none());
    let retained = stdout(repo.arc(&repo.root).args(["cost", "retain", &plan]));
    assert!(retained.contains("retained: pi:healthy-plan"));
    assert!(retained.contains("retained: pi:readable-peer"));
}

#[test]
fn an_unrelated_nonportable_binding_preserves_healthy_costs() {
    let repo = Repo::new();
    pi_recording(&repo, "healthy-binding", &[0.03], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "healthy-binding"),
        "pi",
        "healthy-binding",
    );
    let peer = dispatch(&repo, "--change", "unrelated-binding");
    bind(&repo, &peer, "pi", "peer-session");
    let before = report(&repo, "healthy-binding");
    let locator = repo.home.join("private/peer-session.jsonl");
    for entry in fs::read_dir(repo.root.join(".git/arc/repository/events")).unwrap() {
        let path = entry.unwrap().path();
        let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] == "run-bound" && event["dispatch_event_id"] == peer {
            event["executor_session"] = json!(locator);
            fs::write(path, event.to_string()).unwrap();
        }
    }
    let after = report(&repo, "healthy-binding");
    assert_eq!(after["sessions"], before["sessions"]);
    assert_eq!(after["union"], before["union"]);
    assert!(!after.to_string().contains(locator.to_str().unwrap()));
    let retained = stdout(
        repo.arc(&repo.root)
            .args(["cost", "retain", "healthy-binding"]),
    );
    assert!(retained.contains("retained: pi:healthy-binding"));
}

#[test]
fn corrupt_retained_facts_do_not_spoil_healthy_fresh_figures() {
    let repo = Repo::new();
    pi_recording(&repo, "fresh-healthy", &[0.03], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "fresh-healthy"),
        "pi",
        "fresh-healthy",
    );
    stdout(
        repo.arc(&repo.root)
            .args(["cost", "retain", "fresh-healthy"]),
    );
    let before = report(&repo, "fresh-healthy");
    for entry in fs::read_dir(repo.root.join(".git/arc/repository/events")).unwrap() {
        let path = entry.unwrap().path();
        let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] == "cost-facts-retained" {
            event["facts"]["counters"] = json!("corrupt counters");
            fs::write(path, event.to_string()).unwrap();
        }
    }
    let after = report(&repo, "fresh-healthy");
    // Only the echo of the corrupted record's liveness may differ.
    let figures = |view: &Value| {
        let mut sessions = view["sessions"].clone();
        for row in sessions.as_array_mut().unwrap() {
            row["measurement"]["liveness"]["retained"] = Value::Null;
        }
        sessions
    };
    assert_eq!(figures(&after), figures(&before));
    assert_eq!(after["union"], before["union"]);
    assert_eq!(after["coverage"]["ledger"]["status"], "incomplete");
    let retained = stdout(
        repo.arc(&repo.root)
            .args(["cost", "retain", "fresh-healthy"]),
    );
    assert!(retained.contains("retained: pi:fresh-healthy"));
}

#[cfg(unix)]
#[test]
fn unavailable_source_journal_preserves_independent_change_costs() {
    let repo = Repo::new();
    let plan = file_artifact(&repo, "plan", "unavailable-source");
    let directory = stdout(repo.arc(&repo.root).args(["journal", "dir"]));
    let alias = repo.home.join("source-alias");
    std::os::unix::fs::symlink(directory.trim(), &alias).unwrap();
    let qualified = format!("{}::{plan}", alias.display());
    let change = begin_without_checkout(&repo, "healthy-direct", &["--from-journal", &qualified]);
    pi_recording(&repo, "healthy-direct", &[0.03], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", &change),
        "pi",
        "healthy-direct",
    );
    fs::remove_file(alias).unwrap();
    let view = report(&repo, &change);
    let item = work(&view, &format!("change:{change}"));
    assert_eq!(usd(&item["direct"]), Some(0.03));
    assert!(usd(&item["inclusive"]).is_none());
    assert_eq!(view["coverage"]["selection"]["status"], "incomplete");
    assert_eq!(usd(&view["sessions"][0]["totals"]), Some(0.03));
    stdout(repo.arc(&repo.root).args(["cost", "retain", &change]));
}

#[test]
fn an_unreadable_target_without_any_readable_dispatch_refuses() {
    let repo = Repo::new();
    let output = repo
        .arc(&repo.root)
        .args(["cost", "show", "missing-plan.md", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn review_mixed_claude_accounting_bases_withhold_combined_quantities() {
    let repo = Repo::new();
    claude_recording(&repo, "requests", 10);
    let path = claude_recording(&repo, "cumulative", 100);
    let mut lines = recording_lines(&path);
    lines.push(json!({"type":"cost-state", "sessionId":"cumulative", "totalCostUSD":0.03,
        "modelUsage":{"fixture-model":{"inputTokens":100,"outputTokens":30,"cacheReadInputTokens":200,"cacheCreationInputTokens":40}}}));
    write_lines(&path, &lines);
    for id in ["requests", "cumulative"] {
        bind(
            &repo,
            &dispatch(&repo, "--change", "mixed-basis"),
            "claude",
            id,
        );
    }
    let view = report(&repo, "mixed-basis");
    for totals in [&view["union"], &work(&view, "change:mixed-basis")["direct"]] {
        let input = quantity(totals, "resource-quantity", Some("input"));
        assert_eq!(input["status"], "withheld", "{view}");
        assert!(input.get("amount").is_none() && input.get("known_subtotal").is_none());
        let bases: Vec<&str> = totals["quantities"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|q| q["category"] == "input" && q.get("harness").is_some())
            .filter_map(|q| q["accounting_basis"].as_str())
            .collect();
        assert_eq!(bases, ["recorded-total", "summed-requests"]);
    }
}

#[test]
fn review_partial_event_counters_are_known_subtotals_per_category() {
    let repo = Repo::new();
    let path = pi_recording(&repo, "partial-counters", &[0.01, 0.02], false);
    let mut lines = recording_lines(&path);
    lines[3]["message"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("input");
    write_lines(&path, &lines);
    bind(
        &repo,
        &dispatch(&repo, "--change", "partial-counters"),
        "pi",
        "partial-counters",
    );
    let view = report(&repo, "partial-counters");
    let input = quantity(
        &view["sessions"][0]["totals"],
        "resource-quantity",
        Some("input"),
    );
    assert_eq!(input["status"], "incomplete", "{view}");
    assert!(input.get("amount").is_none());
    assert_eq!(input["known_subtotal"], 10);
    assert!(input["reasons"]
        .to_string()
        .contains("some counted events did not record this category"));
    let output = quantity(
        &view["sessions"][0]["totals"],
        "resource-quantity",
        Some("output"),
    );
    assert_eq!(
        (&output["status"], &output["amount"]),
        (&json!("complete"), &json!(40)),
        "{view}"
    );
    // The disjoint categories inherit the gaps of the counters they restate.
    let totals = &view["sessions"][0]["totals"];
    assert_eq!(disjoint(totals, "uncached_input")["status"], "incomplete");
    assert_eq!(disjoint(totals, "total")["status"], "incomplete");
    assert_eq!(disjoint(totals, "output")["amount"], 40);
}

#[test]
fn review_zero_price_beside_nonzero_usage_is_unmeasured() {
    let repo = Repo::new();
    pi_recording(&repo, "zero-price", &[0.0], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "zero-price"),
        "pi",
        "zero-price",
    );
    let view = report(&repo, "zero-price");
    for total in [&view["union"], &view["sessions"][0]["totals"]] {
        let estimate = quantity(total, USD, None);
        assert_ne!(estimate["status"], "complete", "{view}");
        assert!(estimate.get("amount").is_none() && estimate.get("known_subtotal").is_none());
    }
}

#[test]
fn missing_categories_qualify_only_the_affected_recorded_total() {
    let repo = Repo::new();
    let first = "11111111-1111-4111-8111-111111111111";
    let second = "22222222-2222-4222-8222-222222222222";
    codex_recording(&repo, first, 700);
    let path = codex_recording(&repo, second, 300);
    let mut lines = recording_lines(&path);
    for field in ["total_token_usage", "last_token_usage"] {
        lines[4]["payload"]["info"][field]
            .as_object_mut()
            .unwrap()
            .remove("input_tokens");
    }
    write_lines(&path, &lines);
    for id in [first, second] {
        bind(
            &repo,
            &dispatch(&repo, "--change", "category-gaps"),
            "codex",
            id,
        );
    }
    let view = report(&repo, "category-gaps");
    let input = quantity(&view["union"], "resource-quantity", Some("input"));
    assert_ne!(input["status"], "complete");
    assert!(input.get("amount").is_none());
    assert_eq!(input["known_subtotal"], 700);
    let output = quantity(&view["union"], "resource-quantity", Some("output"));
    assert_eq!(output["status"], "complete");
    assert_eq!(output["amount"], 100);
}

#[test]
fn mixed_zero_and_positive_request_prices_remain_qualified_after_retention() {
    let repo = Repo::new();
    let path = pi_recording(&repo, "mixed-price", &[0.0, 0.02], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "mixed-price"),
        "pi",
        "mixed-price",
    );
    let fresh = report(&repo, "mixed-price");
    let estimate = quantity(&fresh["union"], USD, None);
    assert_eq!(estimate["status"], "incomplete", "{fresh}");
    assert!(estimate.get("amount").is_none());
    assert_eq!(estimate["known_subtotal"], 0.02);
    assert!(estimate["reasons"]
        .to_string()
        .contains("recorded zero price accompanies nonzero or unknown usage"));
    let coverage = &session(&fresh, "pi:mixed-price")["measurement"]["events"]["coverage"]["cost"];
    assert_eq!(
        coverage,
        &json!({"measured": 1, "missing": 0, "unpriced": 1})
    );
    stdout(repo.arc(&repo.root).args(["cost", "retain", "mixed-price"]));
    fs::remove_file(path).unwrap();
    assert_eq!(report(&repo, "mixed-price")["union"], fresh["union"]);
}

#[test]
fn unknown_source_opening_withholds_assignment_without_losing_measurement() {
    let repo = Repo::new();
    let path = pi_recording(&repo, "unknown-opening", &[0.03], false);
    let mut lines = recording_lines(&path);
    for line in &mut lines {
        line.as_object_mut().unwrap().remove("timestamp");
        if let Some(message) = line["message"].as_object_mut() {
            message.remove("timestamp");
        }
    }
    write_lines(&path, &lines);
    bind(
        &repo,
        &dispatch(&repo, "--change", "unknown-opening"),
        "pi",
        "unknown-opening",
    );
    let view = report(&repo, "unknown-opening");
    assert_eq!(view["coverage"]["assignment"]["status"], "incomplete");
    assert!(view["coverage"]["assignment"]["reasons"]
        .to_string()
        .contains("unknown source start"));
    assert!(usd(&view["union"]).is_none());
    assert_eq!(usd(&view["sessions"][0]["totals"]), Some(0.03));
}

/// A claude recording whose cumulative cost-state records `input` against
/// one request that recorded 100.
fn claude_with_total(repo: &Repo, id: &str, input: u64, usd: f64) -> PathBuf {
    let path = claude_recording(repo, id, 100);
    let mut lines = recording_lines(&path);
    lines.push(json!({"type": "cost-state", "timestamp": "2026-01-01T10:00:02Z", "sessionId": id, "totalCostUSD": usd,
        "modelUsage": {"fixture-model": {"inputTokens": input, "outputTokens": 30, "cacheReadInputTokens": 200,
            "cacheCreationInputTokens": 40, "costUSD": usd}}}));
    write_lines(&path, &lines);
    path
}

fn residual_counter<'a>(residual: &'a Value, counter: &str) -> &'a Value {
    residual["counters"]
        .as_array()
        .unwrap_or_else(|| panic!("no residual counters: {residual}"))
        .iter()
        .find(|row| row["counter"] == counter)
        .unwrap_or_else(|| panic!("no {counter} residual: {residual}"))
}

#[test]
fn a_claude_total_reports_its_residual_per_counter_and_retains_it() {
    let repo = Repo::new();
    let id = "55555555-5555-4555-8555-555555555555";
    let path = claude_with_total(&repo, id, 100, 0.03);
    bind(
        &repo,
        &dispatch(&repo, "--change", "residual"),
        "claude",
        id,
    );
    let fresh = report(&repo, "residual");
    let residual = &session(&fresh, &format!("claude:{id}"))["measurement"]["residual"];
    assert_eq!(
        residual["rules"], "tapes-claude-reconciliation/1",
        "{fresh}"
    );
    let input = residual_counter(residual, "input");
    assert_eq!(
        (
            &input["recorded"],
            &input["identified"],
            &input["difference"]
        ),
        (&json!(100), &json!(100), &json!(0)),
        "{fresh}"
    );
    for row in residual["counters"].as_array().unwrap() {
        assert!(
            ["reconciled", "conflict", "discrepancy", "withheld"]
                .contains(&row["status"].as_str().unwrap()),
            "{row}"
        );
    }
    assert!(
        fresh["coverage"]["measurement"]["residual"]["sessions"][format!("claude:{id}")]
            .is_object(),
        "{fresh}"
    );
    assert!(!fresh.to_string().contains(repo.home.to_str().unwrap()));
    stdout(repo.arc(&repo.root).args(["cost", "retain", "residual"]));
    fs::remove_file(path).unwrap();
    let rotated = super::cost::report(&repo, "residual");
    assert_eq!(
        session(&rotated, &format!("claude:{id}"))["measurement"]["residual"],
        *residual
    );
    assert_eq!(
        rotated["coverage"]["measurement"]["residual"],
        fresh["coverage"]["measurement"]["residual"]
    );
}

#[test]
fn a_claude_total_below_its_requests_is_a_conflict_never_clamped() {
    let repo = Repo::new();
    let id = "66666666-6666-4666-8666-666666666666";
    claude_with_total(&repo, id, 60, 0.03);
    bind(
        &repo,
        &dispatch(&repo, "--change", "conflicted-total"),
        "claude",
        id,
    );
    let view = report(&repo, "conflicted-total");
    let row = session(&view, &format!("claude:{id}"));
    let input = residual_counter(&row["measurement"]["residual"], "input");
    assert_eq!(input["status"], "conflict", "{view}");
    assert_eq!(input["difference"], -40, "{view}");
    let figure = quantity(&row["totals"], "resource-quantity", Some("input"));
    assert_ne!(figure["status"], "complete", "{view}");
    assert!(figure["reasons"]
        .to_string()
        .contains("recorded total is below its identified requests"));
    // The disjoint category restating the understated counter inherits it.
    for category in ["uncached_input", "total"] {
        let restated = disjoint(&row["totals"], category);
        assert_ne!(restated["status"], "complete", "{view}");
        assert!(restated["reasons"]
            .to_string()
            .contains("recorded total is below its identified requests"));
    }
    assert_eq!(disjoint(&row["totals"], "output")["status"], "complete");
    assert_eq!(
        view["coverage"]["measurement"]["residual"]["status"],
        "incomplete"
    );
}

#[test]
fn a_claude_request_sum_reports_its_residual_withheld() {
    let repo = Repo::new();
    let id = "77777777-7777-4777-8777-777777777777";
    claude_recording(&repo, id, 100);
    bind(
        &repo,
        &dispatch(&repo, "--change", "no-total"),
        "claude",
        id,
    );
    let view = report(&repo, "no-total");
    let residual = &session(&view, &format!("claude:{id}"))["measurement"]["residual"];
    let input = residual_counter(residual, "input");
    assert_eq!(input["status"], "withheld", "{view}");
    assert!(input.get("difference").is_none(), "{view}");
    assert!(!input["reasons"].as_array().unwrap().is_empty());
}

#[test]
fn an_unenumerable_claude_family_is_never_reconciled_and_leaves_the_measurement_whole() {
    let repo = Repo::new();
    let id = "88888888-8888-4888-8888-888888888888";
    let path = claude_with_total(&repo, id, 100, 0.03);
    let children = path.with_extension("").join("subagents");
    fs::create_dir_all(children.parent().unwrap()).unwrap();
    fs::write(&children, "not a directory").unwrap();
    bind(
        &repo,
        &dispatch(&repo, "--change", "unreadable-family"),
        "claude",
        id,
    );
    let view = report(&repo, "unreadable-family");
    let row = session(&view, &format!("claude:{id}"));
    let residual = &row["measurement"]["residual"];
    assert_eq!(residual["scope_established"], false, "{view}");
    let input = residual_counter(residual, "input");
    assert_ne!(input["status"], "reconciled", "{view}");
    assert_eq!(input["recorded"], 100, "{view}");
    assert_eq!(
        view["coverage"]["measurement"]["residual"]["status"],
        "incomplete"
    );
    assert_eq!(usd(&row["totals"]), Some(0.03), "{view}");
    let input = quantity(&row["totals"], "resource-quantity", Some("input"));
    assert_eq!(input["amount"], 100, "{view}");
    assert!(!view.to_string().contains(repo.home.to_str().unwrap()));
    assert!(!view.to_string().contains("not a directory"));
}

#[test]
fn a_written_zero_cost_beside_usage_stays_unpriced() {
    let repo = Repo::new();
    let id = "99999999-9999-4999-8999-999999999999";
    claude_with_total(&repo, id, 100, 0.0);
    bind(
        &repo,
        &dispatch(&repo, "--change", "zero-total"),
        "claude",
        id,
    );
    let view = report(&repo, "zero-total");
    let row = session(&view, &format!("claude:{id}"));
    let estimate = quantity(&row["totals"], USD, None);
    assert_eq!(estimate["status"], "absent", "{view}");
    assert!(estimate.get("amount").is_none() && estimate.get("known_subtotal").is_none());
    assert!(estimate["reasons"]
        .to_string()
        .contains("recorded zero price accompanies nonzero or unknown usage"));
    assert_eq!(
        row["measurement"]["cost_coverage"]["complete"], false,
        "{view}"
    );
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--detail", "zero-total"]),
    );
    assert!(!text.contains(" 0 USD"), "{text}");
}

#[test]
fn disjoint_categories_total_tokens_across_harnesses_with_one_basis() {
    let repo = Repo::new();
    let claude_id = "11111111-1111-4111-8111-111111111111";
    claude_recording(&repo, claude_id, 300);
    pi_recording(&repo, "pi-session", &[0.01], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "two-harnesses"),
        "claude",
        claude_id,
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", "two-harnesses"),
        "pi",
        "pi-session",
    );
    let view = report(&repo, "two-harnesses");
    let union = &view["union"];
    assert_eq!(
        quantity(union, "resource-quantity", Some("input"))["status"],
        "refused"
    );
    let output = quantity(union, "resource-quantity", Some("output"));
    assert_eq!(output["status"], "withheld", "{view}");
    assert!(output["reasons"].to_string().contains("disjoint"), "{view}");
    for (category, expected) in [
        ("uncached_input", 310),
        ("cache_read", 204),
        ("cache_write", 45),
        ("cache_write_ttl_unrecorded", 45),
        ("output", 50),
        ("total", 609),
    ] {
        let figure = disjoint(union, category);
        assert_eq!(figure["accounting_basis"], "summed-requests", "{figure}");
        assert_eq!(
            figure["amount"]
                .as_u64()
                .or(figure["known_subtotal"].as_u64()),
            Some(expected),
            "{category}: {view}"
        );
    }
    // Claude records no reasoning count, so the sum is only pi's.
    let reasoning = disjoint(union, "reasoning");
    assert_eq!(reasoning["status"], "absent", "{view}");
    assert_eq!(reasoning["known_subtotal"], 3);
    assert_eq!(
        disjoint(union, "total")["conventions"],
        json!({"claude": {"input": "excludes-cache", "output": "includes-reasoning"},
            "pi": {"input": "excludes-cache", "output": "includes-reasoning"}})
    );
}

#[test]
fn disjoint_categories_across_accounting_bases_are_partitioned() {
    let repo = Repo::new();
    let claude_id = "11111111-1111-4111-8111-111111111111";
    let codex_id = "22222222-2222-4222-8222-222222222222";
    claude_recording(&repo, claude_id, 300);
    codex_recording(&repo, codex_id, 700);
    bind(
        &repo,
        &dispatch(&repo, "--change", "two-bases"),
        "claude",
        claude_id,
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", "two-bases"),
        "codex",
        codex_id,
    );
    let view = report(&repo, "two-bases");
    let cache_read = disjoint(&view["union"], "cache_read");
    assert_eq!(cache_read["status"], "withheld", "{view}");
    assert!(cache_read.get("amount").is_none() && cache_read.get("known_subtotal").is_none());
    let partitions: Vec<(&str, Option<u64>)> = view["union"]["quantities"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|q| {
            q["categories"] == "disjoint" && q["category"] == "cache_read" && q["partition"] == true
        })
        .map(|q| {
            (
                q["accounting_basis"].as_str().unwrap(),
                q["amount"].as_u64().or(q["known_subtotal"].as_u64()),
            )
        })
        .collect();
    assert_eq!(
        partitions,
        [
            ("recorded-total", Some(100)),
            ("summed-requests", Some(200))
        ],
        "{view}"
    );
}

#[test]
fn tampered_retained_accounting_fields_degrade_field_by_field() {
    let repo = Repo::new();
    let claude_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let claude_path = claude_with_total(&repo, claude_id, 100, 0.03);
    let pi_path = pi_recording(&repo, "tampered-pi", &[0.03], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "tampered"),
        "claude",
        claude_id,
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", "tampered"),
        "pi",
        "tampered-pi",
    );
    stdout(repo.arc(&repo.root).args(["cost", "retain", "tampered"]));
    let leak = "/home/user/recording.jsonl";
    for entry in fs::read_dir(repo.root.join(".git/arc/repository/events")).unwrap() {
        let path = entry.unwrap().path();
        let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] != "cost-facts-retained" {
            continue;
        }
        let claude = event["executor_harness"] == "claude";
        let facts = &mut event["facts"];
        if claude {
            facts["residual"]["counters"][0]["reasons"] = json!([format!("cannot read {leak}")]);
        } else {
            facts["selection"] = json!(leak);
            facts["disjoint"]["convention"]["input"] = json!(leak);
            facts["events"]["identity"]["account_namespace"] = json!(leak);
        }
        fs::write(path, event.to_string()).unwrap();
    }
    fs::remove_file(claude_path).unwrap();
    fs::remove_file(pi_path).unwrap();
    let view = report(&repo, "tampered");
    assert!(!view.to_string().contains(leak), "{view}");
    let claude = session(&view, &format!("claude:{claude_id}"));
    assert_eq!(
        claude["measurement"]["residual"],
        json!({"unavailable": "retained reconciliation gap", "scope_established": false}),
        "{view}"
    );
    assert_eq!(usd(&claude["totals"]), Some(0.03), "{view}");
    let pi = session(&view, "pi:tampered-pi");
    assert_eq!(usd(&pi["totals"]), Some(0.03), "{view}");
    assert!(pi["measurement"]["selection"].is_null(), "{view}");
    assert!(pi["measurement"]["events"].is_null(), "{view}");
    assert!(pi["measurement"]["disjoint"].is_null(), "{view}");
    assert!(
        pi["measurement"]["reasons"]
            .to_string()
            .contains("retained measurement gap"),
        "{view}"
    );
    // The withheld disjoint categories are absent, never zero.
    assert_eq!(
        disjoint(&pi["totals"], "total")["status"],
        "absent",
        "{view}"
    );
}

/// Every figure's reasons, flattened, for a session's totals.
fn all_reasons(totals: &Value) -> String {
    totals["quantities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|figure| figure["reasons"].to_string())
        .collect()
}

#[test]
fn a_claude_request_sum_is_complete_where_tapes_states_every_counter() {
    let repo = Repo::new();
    let id = "88888888-8888-4888-8888-888888888888";
    claude_recording(&repo, id, 300);
    bind(
        &repo,
        &dispatch(&repo, "--change", "whole-sum"),
        "claude",
        id,
    );
    let view = report(&repo, "whole-sum");
    let row = session(&view, &format!("claude:{id}"));
    for (category, expected) in [
        ("input", 300),
        ("output", 30),
        ("cache_read", 200),
        ("cache_write", 40),
    ] {
        let figure = quantity(&row["totals"], "resource-quantity", Some(category));
        assert_eq!(figure["status"], "complete", "{category}: {view}");
        assert_eq!(figure["amount"], expected, "{category}: {view}");
    }
    for (category, expected) in [
        ("uncached_input", 300),
        ("cache_read", 200),
        ("cache_write", 40),
        ("output", 30),
        ("total", 570),
    ] {
        let figure = disjoint(&row["totals"], category);
        assert_eq!(figure["status"], "complete", "{category}: {view}");
        assert_eq!(figure["amount"], expected, "{category}: {view}");
    }
    // Claude records no reasoning count: absent, and the only gap.
    let reasoning = quantity(&row["totals"], "resource-quantity", Some("reasoning"));
    assert_eq!(reasoning["status"], "absent", "{view}");
    assert!(
        !all_reasons(&row["totals"]).contains("coverage is unknown"),
        "{view}"
    );
}

/// A claude recording with `extra` records appended after its one request.
fn claude_with_records(repo: &Repo, id: &str, extra: &[Value]) {
    let path = claude_recording(repo, id, 300);
    let mut lines = recording_lines(&path);
    lines.extend_from_slice(extra);
    write_lines(&path, &lines);
}

#[test]
fn attachment_records_without_usage_leave_a_claude_measurement_complete() {
    let repo = Repo::new();
    let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    claude_with_records(
        &repo,
        id,
        &[
            json!({"type": "attachment", "sessionId": id, "uuid": "attachment-1", "timestamp": "2026-01-01T10:00:02Z",
                "attachment": {"type": "date", "date": "2026-01-01"}}),
            json!({"type": "attachment", "sessionId": id, "uuid": "attachment-2", "timestamp": "2026-01-01T10:00:03Z",
                "attachment": {"type": "total_tokens_reminder", "message": "Tokens are counted."}}),
        ],
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", "attachments-only"),
        "claude",
        id,
    );
    let view = report(&repo, "attachments-only");
    let row = session(&view, &format!("claude:{id}"));
    for category in ["input", "output", "cache_read", "cache_write"] {
        let figure = quantity(&row["totals"], "resource-quantity", Some(category));
        assert_eq!(figure["status"], "complete", "{category}: {view}");
    }
    assert!(
        !all_reasons(&row["totals"]).contains("records carrying usage were not read"),
        "{view}"
    );
}

#[test]
fn a_usage_bearing_unrecognized_record_qualifies_every_category_and_the_dollar_figure() {
    let repo = Repo::new();
    let id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    claude_with_records(
        &repo,
        id,
        &[
            json!({"type": "mystery", "sessionId": id, "uuid": "mystery-1", "timestamp": "2026-01-01T10:00:02Z",
                "message": {"usage": {"input_tokens": 7, "output_tokens": 3}}}),
        ],
    );
    bind(
        &repo,
        &dispatch(&repo, "--change", "unread-usage"),
        "claude",
        id,
    );
    let view = report(&repo, "unread-usage");
    let row = session(&view, &format!("claude:{id}"));
    let reason = "some records carrying usage were not read";
    for category in ["input", "output", "cache_read", "cache_write"] {
        let figure = quantity(&row["totals"], "resource-quantity", Some(category));
        assert_ne!(figure["status"], "complete", "{category}: {view}");
        assert!(figure["reasons"].to_string().contains(reason), "{view}");
    }
    for category in [
        "uncached_input",
        "cache_read",
        "cache_write",
        "output",
        "total",
    ] {
        let figure = disjoint(&row["totals"], category);
        assert_eq!(figure["status"], "incomplete", "{category}: {view}");
        assert!(figure["reasons"].to_string().contains(reason), "{view}");
    }
    let dollars = quantity(&row["totals"], USD, None);
    assert_ne!(dollars["status"], "complete", "{view}");
    assert!(dollars["reasons"].to_string().contains(reason), "{view}");
}

#[test]
fn a_claude_recorded_total_is_complete_where_its_models_each_record_a_counter() {
    let repo = Repo::new();
    let id = "99999999-9999-4999-8999-999999999999";
    claude_with_total(&repo, id, 100, 0.03);
    bind(
        &repo,
        &dispatch(&repo, "--change", "whole-total"),
        "claude",
        id,
    );
    let view = report(&repo, "whole-total");
    let row = session(&view, &format!("claude:{id}"));
    assert_eq!(row["measurement"]["basis"], "recorded-total", "{view}");
    for category in ["input", "output", "cache_read", "cache_write"] {
        let figure = quantity(&row["totals"], "resource-quantity", Some(category));
        assert_eq!(figure["status"], "complete", "{category}: {view}");
    }
    assert_eq!(
        disjoint(&row["totals"], "total")["status"],
        "complete",
        "{view}"
    );
    assert!(
        !all_reasons(&row["totals"]).contains("coverage is unknown"),
        "{view}"
    );
}

#[test]
fn a_stopped_request_and_an_unreadable_record_qualify_every_category_by_type() {
    let repo = Repo::new();
    let path = pi_recording(&repo, "stopped-unreadable", &[0.01, 0.02], false);
    let mut lines = recording_lines(&path);
    lines[3]["message"]["stopReason"] = json!("error");
    let mut body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    body.push_str("{ this record is not json\n");
    fs::write(&path, body).unwrap();
    bind(
        &repo,
        &dispatch(&repo, "--change", "stopped-unreadable"),
        "pi",
        "stopped-unreadable",
    );
    let fresh = report(&repo, "stopped-unreadable");
    let row = session(&fresh, "pi:stopped-unreadable");
    for category in ["input", "output", "cache_read", "cache_write"] {
        let figure = quantity(&row["totals"], "resource-quantity", Some(category));
        assert_eq!(figure["status"], "incomplete", "{category}: {fresh}");
        let reasons = figure["reasons"].to_string();
        assert!(
            reasons.contains("a request stopped before its usage was complete"),
            "{reasons}"
        );
        assert!(
            reasons.contains("some source records could not be read"),
            "{reasons}"
        );
    }
    for category in [
        "uncached_input",
        "cache_read",
        "cache_write",
        "output",
        "total",
    ] {
        let figure = disjoint(&row["totals"], category);
        assert_eq!(figure["status"], "incomplete", "{category}: {fresh}");
        assert!(figure["known_subtotal"].is_u64(), "{category}: {fresh}");
    }
    // No count of prose notes stands in for a typed reason.
    let reasons = row["measurement"]["reasons"].to_string();
    assert!(
        !reasons.contains("consumption notes") && !reasons.contains("usage notes"),
        "{reasons}"
    );
    stdout(
        repo.arc(&repo.root)
            .args(["cost", "retain", "stopped-unreadable"]),
    );
    fs::remove_file(path).unwrap();
    let retained = report(&repo, "stopped-unreadable");
    assert_eq!(retained["union"], fresh["union"]);
    assert_eq!(
        session(&retained, "pi:stopped-unreadable")["measurement"]["disjoint"]["gaps"],
        row["measurement"]["disjoint"]["gaps"]
    );
}

#[test]
fn a_pi_disjoint_figure_carries_tapes_category_completeness() {
    let repo = Repo::new();
    let path = pi_recording(&repo, "category-completeness", &[0.01, 0.02], false);
    let mut lines = recording_lines(&path);
    lines[3]["message"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("cacheRead");
    write_lines(&path, &lines);
    bind(
        &repo,
        &dispatch(&repo, "--change", "category-completeness"),
        "pi",
        "category-completeness",
    );
    let view = report(&repo, "category-completeness");
    let row = session(&view, "pi:category-completeness");
    let gaps = &row["measurement"]["disjoint"]["gaps"];
    assert_eq!(
        gaps["cache_read"],
        json!(["some counted events did not record this category"]),
        "{view}"
    );
    assert!(
        gaps.get("output").is_none() && gaps.get("uncached_input").is_none(),
        "{view}"
    );
    assert_eq!(
        disjoint(&row["totals"], "output")["status"],
        "complete",
        "{view}"
    );
    assert_eq!(
        disjoint(&row["totals"], "cache_read")["status"],
        "incomplete",
        "{view}"
    );
    assert_eq!(
        disjoint(&row["totals"], "total")["status"],
        "incomplete",
        "{view}"
    );
}

#[test]
fn tampered_retained_disjoint_gaps_degrade_category_by_category() {
    let repo = Repo::new();
    let path = pi_recording(&repo, "tampered-gaps", &[0.01], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "tampered-gaps"),
        "pi",
        "tampered-gaps",
    );
    stdout(
        repo.arc(&repo.root)
            .args(["cost", "retain", "tampered-gaps"]),
    );
    let leak = "/home/user/recording.jsonl";
    for entry in fs::read_dir(repo.root.join(".git/arc/repository/events")).unwrap() {
        let path = entry.unwrap().path();
        let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] != "cost-facts-retained" {
            continue;
        }
        event["facts"]["disjoint"]["gaps"] = json!({"output": [format!("cannot read {leak}")]});
        fs::write(path, event.to_string()).unwrap();
    }
    fs::remove_file(path).unwrap();
    let view = report(&repo, "tampered-gaps");
    assert!(!view.to_string().contains(leak), "{view}");
    let row = session(&view, "pi:tampered-gaps");
    // The category whose reason was unreadable is qualified, never complete;
    // the others keep their retained completeness.
    let output = disjoint(&row["totals"], "output");
    assert_eq!(output["status"], "incomplete", "{view}");
    assert_eq!(output["known_subtotal"], 20, "{view}");
    assert!(output["reasons"]
        .to_string()
        .contains("quantity coverage was not retained"));
    assert_eq!(
        disjoint(&row["totals"], "cache_read")["status"],
        "complete",
        "{view}"
    );
    assert_eq!(usd(&row["totals"]), Some(0.01), "{view}");
}

#[test]
fn retained_disjoint_facts_without_category_gaps_are_not_complete() {
    let repo = Repo::new();
    let path = pi_recording(&repo, "no-gaps", &[0.01], false);
    bind(
        &repo,
        &dispatch(&repo, "--change", "no-gaps"),
        "pi",
        "no-gaps",
    );
    stdout(repo.arc(&repo.root).args(["cost", "retain", "no-gaps"]));
    for entry in fs::read_dir(repo.root.join(".git/arc/repository/events")).unwrap() {
        let path = entry.unwrap().path();
        let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] != "cost-facts-retained" {
            continue;
        }
        event["facts"]["disjoint"]
            .as_object_mut()
            .unwrap()
            .remove("gaps");
        fs::write(path, event.to_string()).unwrap();
    }
    fs::remove_file(path).unwrap();
    let view = report(&repo, "no-gaps");
    let row = session(&view, "pi:no-gaps");
    for category in ["uncached_input", "cache_read", "output", "total"] {
        let figure = disjoint(&row["totals"], category);
        assert_ne!(figure["status"], "complete", "{category}: {view}");
        assert!(figure.get("amount").is_none(), "{category}: {view}");
    }
}

const LEAD: &str = "lead-session";

/// `arc` acting as a pi session, the way a lead that reads, approves and
/// integrates writes its events.
fn acting(repo: &Repo, cwd: &Path, session: &str) -> AssertCommand {
    let mut command = repo.arc(cwd);
    command.env("ARC_HARNESS", "pi").env("ARC_SESSION", session);
    command
}

/// Open, snapshot, approve and integrate one change as `session`, and hand
/// back its id.
fn integrated_by(repo: &Repo, slug: &str, session: &str, extra: &[&str]) -> String {
    let mut begin = acting(repo, &repo.root, session);
    begin.args(["begin", slug]).args(extra);
    let change = opened_change_id(&stdout(&mut begin));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: {slug}"),
    );
    stdout(acting(repo, &worktree, session).args(["snapshot", slug]));
    acting(repo, &worktree, session)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    acting(repo, &repo.root, session)
        .args(["integrate", slug])
        .assert()
        .success();
    change
}

fn participant<'a>(item: &'a Value, key: &str) -> &'a Value {
    item["participants"]
        .as_array()
        .unwrap_or_else(|| panic!("no participants: {item}"))
        .iter()
        .find(|row| row["key"] == key)
        .unwrap_or_else(|| panic!("no participant {key}: {item}"))
}

fn outside_session<'a>(report: &'a Value, key: &str) -> Option<&'a Value> {
    report["coverage"]["assignment"]["outside"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            format!(
                "{}:{}",
                row["harness"].as_str().unwrap(),
                row["session"].as_str().unwrap()
            ) == key
        })
}

/// The lead's one recorded request precedes its events on either change, so
/// its share of each is empty and the request is unattributed.
#[test]
fn a_lead_acting_on_two_changes_is_divided_by_span_beside_its_whole_figure() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    let first = integrated_by(&repo, "first", LEAD, &[]);
    let second = integrated_by(&repo, "second", LEAD, &[]);
    // The lead also dispatched work on the first change.
    let dispatched = event_id(&stdout(acting(&repo, &repo.root, LEAD).args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        "--change",
        &first,
    ])));
    assert!(!dispatched.is_empty());

    for (change, other) in [(&first, &second), (&second, &first)] {
        let view = report(&repo, change);
        let key = format!("change:{change}");
        let item = work(&view, &key);
        assert_eq!(item["shared"], json!([]), "{view}");
        assert_eq!(
            item["direct"]["shares"],
            json!(["pi:lead-session"]),
            "{view}"
        );
        let lead = participant(item, "pi:lead-session");
        assert_eq!(lead["spans"], 2, "{view}");
        assert_eq!(lead["counted"], false, "{view}");
        assert_eq!(lead["share"], true, "{view}");
        let roles = if change == &first {
            json!(["integrator", "orchestrator", "reviewer"])
        } else {
            json!(["integrator", "reviewer"])
        };
        assert_eq!(lead["roles"], roles, "{view}");
        // The whole figure is shown, and the direct total takes only the
        // empty share; the first change's unbound dispatch withholds it.
        assert_eq!(usd(&lead["totals"]), Some(0.2), "{view}");
        assert_ne!(usd(&item["direct"]), Some(0.2), "{view}");
        let row = session(&view, "pi:lead-session");
        let share = row["split"]["shares"]
            .as_array()
            .unwrap()
            .iter()
            .find(|share| share["work"] == key)
            .unwrap();
        assert_eq!(usd(&share["totals"]), Some(0.0), "{view}");
        assert_eq!(
            row["split"]["unattributed"]["reasons"],
            json!(["before first item"]),
            "{view}"
        );
        assert_eq!(row["kind"], "participant", "{view}");
        assert_eq!(row["participant"]["spans"], 2, "{view}");
        let membership = [format!("change:{change}"), format!("change:{other}")];
        let mut membership = membership.to_vec();
        membership.sort();
        assert_eq!(row["membership"], json!(membership), "{view}");
        assert!(
            outside_session(&view, "pi:lead-session").is_none(),
            "{view}"
        );
        // A session may act in other ledgers: the view says so.
        assert!(
            view["coverage"]["assignment"]["participants"]["statement"]
                .as_str()
                .unwrap()
                .contains("other repositories' ledgers"),
            "{view}"
        );
    }
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--detail", &first]),
    );
    assert!(text.contains("pi:lead-session"), "{text}");
    assert!(
        text.contains("integrator, orchestrator, reviewer"),
        "{text}"
    );
}

#[test]
fn a_union_over_both_changes_counts_the_lead_once() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    let plan = file_artifact(&repo, "plan", "led-plan");
    let first = integrated_by(&repo, "first", LEAD, &["--from-journal", &plan]);
    let second = integrated_by(&repo, "second", LEAD, &["--from-journal", &plan]);

    let view = report(&repo, &plan);
    // The lead's one request precedes both of its spans: the union takes
    // the shares, which hold none of it, and the footer holds it.
    assert_eq!(usd(&view["union"]), Some(0.0), "{view}");
    assert_eq!(
        view["union"]["sessions"],
        json!(["pi:lead-session"]),
        "{view}"
    );
    assert_eq!(view["union"]["shared"], json!([]), "{view}");
    let lead = participant(&view["union"], "pi:lead-session");
    assert_eq!(lead["counted"], false, "{view}");
    assert_eq!(lead["share"], true, "{view}");
    let footer = &view["table"]["unattributed"]["usd"];
    assert_eq!(
        footer["amount"]
            .as_f64()
            .or(footer["known_subtotal"].as_f64()),
        Some(0.2),
        "{view}"
    );
    // Each change takes its share, which holds none of the one request.
    for change in [&first, &second] {
        let item = work(&view, &format!("change:{change}"));
        assert_eq!(item["shared"], json!([]), "{view}");
        assert_eq!(
            item["direct"]["shares"],
            json!(["pi:lead-session"]),
            "{view}"
        );
        assert_eq!(usd(&item["direct"]), Some(0.0), "{view}");
    }
}

#[test]
fn retention_keeps_a_participant_and_prices_it_after_its_source_is_deleted() {
    let repo = Repo::new();
    let path = pi_recording(&repo, LEAD, &[0.2], false);
    let first = integrated_by(&repo, "first", LEAD, &[]);
    integrated_by(&repo, "second", LEAD, &[]);

    let fresh = report(&repo, &first);
    let output = stdout(repo.arc(&repo.root).args(["cost", "retain", &first]));
    assert!(output.contains("retained: pi:lead-session"), "{output}");
    fs::remove_file(path).unwrap();

    let rotated = report(&repo, &first);
    let row = session(&rotated, "pi:lead-session");
    assert_eq!(row["measurement"]["source"], "retained", "{rotated}");
    let key = format!("change:{first}");
    let item = work(&rotated, &key);
    assert_eq!(
        usd(&participant(item, "pi:lead-session")["totals"]),
        Some(0.2),
        "{rotated}"
    );
    assert_eq!(rotated["union"], fresh["union"], "{rotated}");
    // Retaining again records nothing new.
    let again = stdout(repo.arc(&repo.root).args(["cost", "retain", &first]));
    assert!(again.contains("pi:lead-session"), "{again}");
}

#[test]
fn a_participant_the_store_cannot_resolve_stays_outside_with_its_event_count() {
    let repo = Repo::new();
    let change = integrated_by(&repo, "ghosted", "ghost-session", &[]);
    let held: usize = fs::read_dir(event_dir(&repo, &change))
        .unwrap()
        .filter(|entry| {
            let event: Value =
                serde_json::from_slice(&fs::read(entry.as_ref().unwrap().path()).unwrap()).unwrap();
            event["session"] == "ghost-session"
        })
        .count();
    assert!(
        held >= 3,
        "begin, snapshot, review and integrate wrote events"
    );

    let view = report(&repo, &change);
    let ghost = outside_session(&view, "pi:ghost-session")
        .unwrap_or_else(|| panic!("the unresolved session is outside: {view}"));
    assert_eq!(ghost["events"], held, "{view}");
    assert!(ghost.get("amount").is_none(), "{view}");
    let row = session(&view, "pi:ghost-session");
    assert_eq!(row["kind"], "participant", "{view}");
    assert_eq!(row["measurement"]["status"], "unmeasured", "{view}");
    let statement = view["coverage"]["assignment"]["outside"]["statement"]
        .as_str()
        .unwrap();
    assert!(statement.contains("never zero"), "{statement}");
}

#[test]
fn an_executor_session_that_also_wrote_events_stays_an_executor_row() {
    let repo = Repo::new();
    pi_recording(&repo, "exec-session", &[0.05], false);
    let change = integrated_by(&repo, "executed", "exec-session", &[]);
    bind(
        &repo,
        &dispatch(&repo, "--change", &change),
        "pi",
        "exec-session",
    );

    let view = report(&repo, &change);
    let rows: Vec<_> = view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["key"] == "pi:exec-session")
        .collect();
    assert_eq!(rows.len(), 1, "{view}");
    assert_eq!(rows[0]["kind"], "executor", "{view}");
    assert!(rows[0].get("participant").is_none(), "{view}");
    let item = work(&view, &format!("change:{change}"));
    assert_eq!(usd(&item["direct"]), Some(0.05), "{view}");
    assert!(
        item["participants"]
            .as_array()
            .is_none_or(|rows| rows.iter().all(|row| row["key"] != "pi:exec-session")),
        "{view}"
    );
    assert!(
        outside_session(&view, "pi:exec-session").is_none(),
        "{view}"
    );
}

#[test]
fn a_participant_that_acted_on_one_change_is_direct_for_it() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    let change = integrated_by(&repo, "solo", LEAD, &[]);
    let view = report(&repo, &change);
    let item = work(&view, &format!("change:{change}"));
    assert_eq!(item["shared"], json!([]), "{view}");
    assert_eq!(
        participant(item, "pi:lead-session")["counted"],
        true,
        "{view}"
    );
    assert_eq!(usd(&item["direct"]), Some(0.2), "{view}");
}

#[test]
fn a_participant_is_measured_as_of_its_read_frontier() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    let change = integrated_by(&repo, "running", LEAD, &[]);
    let view = report(&repo, &change);
    let row = session(&view, "pi:lead-session");
    assert_eq!(row["measurement"]["status"], "incomplete", "{view}");
    assert!(
        row["measurement"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason.as_str().unwrap().contains("read frontier")),
        "{view}"
    );
    assert!(
        row["measurement"]["frontier"].as_u64().unwrap() > 0,
        "{view}"
    );
    let item = work(&view, &format!("change:{change}"));
    let figure = quantity(&item["direct"], USD, None);
    assert_eq!(figure["status"], "incomplete", "{view}");
    assert!(figure.get("amount").is_none(), "{view}");
    assert_eq!(figure["known_subtotal"].as_f64(), Some(0.2), "{view}");
}

#[test]
fn one_unreadable_participant_leaves_the_others_figures_intact() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    let change = opened_change_id(&stdout(
        acting(&repo, &repo.root, LEAD).args(["begin", "mixed"]),
    ));
    let worktree = repo.home.join(".worktrees/repo-mixed");
    repo.commit(&worktree, "mixed.txt", "mixed\n", "feat: mixed");
    stdout(acting(&repo, &worktree, "broken").args(["snapshot", "mixed"]));
    acting(&repo, &worktree, LEAD)
        .args(["review", "mixed", "--verdict", "approved"])
        .assert()
        .success();

    let view = report(&repo, &change);
    let item = work(&view, &format!("change:{change}"));
    assert_eq!(
        usd(&participant(item, "pi:lead-session")["totals"]),
        Some(0.2),
        "{view}"
    );
    let broken = outside_session(&view, "pi:broken")
        .unwrap_or_else(|| panic!("the unreadable participant is outside: {view}"));
    assert!(broken["events"].as_u64().unwrap() >= 1, "{view}");
    let unmeasured = participant(item, "pi:broken");
    assert_eq!(
        quantity(&unmeasured["totals"], USD, None)["status"],
        "absent",
        "{view}"
    );
}

#[test]
fn participant_facts_carry_no_locator() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    let change = integrated_by(&repo, "private", LEAD, &[]);
    stdout(repo.arc(&repo.root).args(["cost", "retain", &change]));
    let home = repo.home.to_string_lossy().to_string();
    let mut retained = 0;
    for entry in fs::read_dir(repo.root.join(".git/arc/repository/events")).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        if text.contains("cost-facts-retained") {
            retained += 1;
            assert!(!text.contains(&home), "{text}");
            assert!(!text.contains(".jsonl"), "{text}");
        }
    }
    assert_eq!(retained, 1);
    let view = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", &change, "--json"]),
    );
    assert!(!view.contains(&home), "{view}");
}

#[test]
fn acting_events_without_a_session_are_counted_never_zero() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    let change = opened_change_id(&stdout(
        acting(&repo, &repo.root, LEAD).args(["begin", "anonymous"]),
    ));
    let worktree = repo.home.join(".worktrees/repo-anonymous");
    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(acting(&repo, &worktree, LEAD).args(["snapshot", "anonymous"]));
    let before = report(&repo, &change);
    let outside = &before["coverage"]["assignment"]["outside"];
    assert_eq!(outside["unattributed_events"], 0, "{before}");

    repo.commit(&worktree, "more.txt", "more\n", "feat: more");
    repo.arc(&worktree)
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION")
        .args(["snapshot", "anonymous"])
        .assert()
        .success();
    let view = report(&repo, &change);
    let outside = &view["coverage"]["assignment"]["outside"];
    assert_eq!(outside["unattributed_events"], 1, "{view}");
    assert_eq!(
        view["coverage"]["assignment"]["status"], "incomplete",
        "{view}"
    );
    // The session that did name itself is still measured, whole.
    let item = work(&view, &format!("change:{change}"));
    assert_eq!(
        usd(&participant(item, "pi:lead-session")["totals"]),
        Some(0.2),
        "{view}"
    );
}

#[test]
fn an_unreadable_ledger_record_keeps_a_participant_from_counting_as_the_works_alone() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    let change = integrated_by(&repo, "solo-led", LEAD, &[]);
    let peer = begin_without_checkout(&repo, "bad-peer", &[]);
    let record = repo
        .root
        .join(".git/arc/changes")
        .join(peer)
        .join("events/bad-peer.json");
    fs::write(record, "not json").unwrap();

    let view = report(&repo, &change);
    let item = work(&view, &format!("change:{change}"));
    // The unreadable record might be the lead's event on another change.
    assert_eq!(
        participant(item, "pi:lead-session")["counted"],
        false,
        "{view}"
    );
    assert_eq!(item["shared"], json!(["pi:lead-session"]), "{view}");
    assert!(usd(&item["direct"]).is_none(), "{view}");
    assert_eq!(
        usd(&participant(item, "pi:lead-session")["totals"]),
        Some(0.2),
        "{view}"
    );
    assert_eq!(
        view["coverage"]["assignment"]["status"], "incomplete",
        "{view}"
    );
}

const LEAD_MODEL: &str = "claude-opus-5-5#xhigh";
const EXECUTOR: &str = "exec-run";

/// `arc` acting as the lead session with its declared model.
fn lead(repo: &Repo, cwd: &Path) -> AssertCommand {
    let mut command = acting(repo, cwd, LEAD);
    command.env("ARC_MODEL", LEAD_MODEL);
    command
}

/// A two-commit change the lead opened, dispatched to an executor, approved
/// and integrated. Its commits carry role trailers naming the executor's
/// route model, the lead's model, and a reviewer no session recorded.
struct Shipped {
    change: String,
    first: String,
    second: String,
}

fn shipped_change(repo: &Repo, slug: &str) -> Shipped {
    let begin = stdout(lead(repo, &repo.root).args(["begin", slug]));
    let change = opened_change_id(&begin);
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        "one.txt",
        "one\n",
        &format!(
            "feat: first\n\nPlanned-by: pi:{LEAD_MODEL}\nImplemented-by: pi:claude-sonnet-5-5#high\nReviewed-by: pi:{LEAD_MODEL}\nOrchestrated-by: pi:{LEAD_MODEL}"
        ),
    );
    repo.commit(
        &worktree,
        "two.txt",
        "two\n",
        "feat: second\n\nImplemented-by: pi:claude-sonnet-5-5#high\nReviewed-by: pi:gpt-9-unrecorded#high",
    );
    let dispatched = event_id(&stdout(lead(repo, &repo.root).args([
        "run",
        "dispatch",
        "--route",
        "claude-sonnet-5-5#high",
        "--worktree",
        "/work/run",
        "--change",
        &change,
    ])));
    dispatch_time(repo, &dispatched, "2026-01-01T09:59:00Z");
    bind(repo, &dispatched, "pi", EXECUTOR);
    stdout(lead(repo, &worktree).args(["snapshot", slug]));
    lead(repo, &worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    lead(repo, &repo.root)
        .args(["integrate", slug])
        .assert()
        .success();
    Shipped {
        change,
        first: git_out(&worktree, &["rev-parse", "HEAD~1"]),
        second: git_out(&worktree, &["rev-parse", "HEAD"]),
    }
}

fn commit_view(repo: &Repo, commit: &str) -> Value {
    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--commit", commit, "--json"]),
    );
    assert_eq!(view["schema"], "arc-cost-commit/1", "{view}");
    view
}

fn trailer_row<'a>(view: &'a Value, trailer: &str) -> &'a Value {
    view["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["trailer"] == trailer)
        .unwrap_or_else(|| panic!("no {trailer} row: {view}"))
}

fn note_of(repo: &Repo, commit: &str) -> String {
    git_out(&repo.root, &["notes", "--ref=arc-cost", "show", commit])
}

#[test]
fn a_commit_of_a_two_commit_change_shows_the_changes_figures_and_its_executor_run() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    pi_recording(&repo, EXECUTOR, &[0.05], false);
    let shipped = shipped_change(&repo, "two");

    let view = commit_view(&repo, &shipped.first);
    assert_eq!(view["commit"], shipped.first, "{view}");
    assert_eq!(view["change"]["id"], shipped.change, "{view}");
    assert_eq!(view["position"], json!({"index": 1, "of": 2}), "{view}");
    assert_eq!(view["commits"].as_array().unwrap().len(), 2, "{view}");
    assert_eq!(view["commits"][1]["sha"], shipped.second, "{view}");
    // The embedded document is the change's own view, unchanged.
    assert_eq!(view["cost"], report(&repo, &shipped.change), "{view}");
    let implemented = trailer_row(&view, "Implemented-by");
    assert_eq!(implemented["status"], "recorded", "{view}");
    assert_eq!(implemented["sessions"][0]["key"], "pi:exec-run", "{view}");
    assert_eq!(implemented["sessions"][0]["kind"], "executor", "{view}");
    assert_eq!(usd(&implemented["sessions"][0]["totals"]), Some(0.05));

    let second = commit_view(&repo, &shipped.second);
    assert_eq!(second["position"], json!({"index": 2, "of": 2}), "{second}");

    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--commit", &shipped.first]),
    );
    assert!(text.contains("commit 1 of 2"), "{text}");
    assert!(
        text.contains("shared by the change's 2 commits; no figure is per commit"),
        "{text}"
    );
    assert!(text.contains("Implemented-by"), "{text}");
    assert!(text.contains("pi:exec-run"), "{text}");
}

#[test]
fn a_commit_outside_every_change_answers_that_none_records_it() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    pi_recording(&repo, EXECUTOR, &[0.05], false);
    shipped_change(&repo, "two");
    let init = git_out(&repo.root, &["rev-list", "--max-parents=0", "HEAD"]);

    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--commit", &init]),
    );
    assert!(text.contains("no change records this commit"), "{text}");
    assert!(!text.contains("harness-calculated"), "{text}");
    let view = commit_view(&repo, &init);
    assert_eq!(view["change"], Value::Null, "{view}");
    assert!(view.get("cost").is_none(), "{view}");
}

#[test]
fn a_reviewed_by_trailer_maps_to_the_participant_that_approved_and_an_unrecorded_one_says_so() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    pi_recording(&repo, EXECUTOR, &[0.05], false);
    let shipped = shipped_change(&repo, "two");

    let first = commit_view(&repo, &shipped.first);
    let reviewed = trailer_row(&first, "Reviewed-by");
    assert_eq!(reviewed["status"], "recorded", "{first}");
    assert_eq!(reviewed["sessions"][0]["key"], "pi:lead-session", "{first}");
    assert_eq!(reviewed["sessions"][0]["kind"], "participant", "{first}");
    // The whole figure, shared exactly as the change view shares it.
    assert_eq!(
        usd(&reviewed["sessions"][0]["totals"]),
        Some(0.2),
        "{first}"
    );
    for trailer in ["Planned-by", "Orchestrated-by"] {
        let row = trailer_row(&first, trailer);
        assert_eq!(row["sessions"][0]["key"], "pi:lead-session", "{first}");
    }

    let second = commit_view(&repo, &shipped.second);
    let unrecorded = trailer_row(&second, "Reviewed-by");
    assert_eq!(unrecorded["status"], "no-session-recorded", "{second}");
    assert_eq!(unrecorded["sessions"], json!([]), "{second}");
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--commit", &shipped.second]),
    );
    assert!(text.contains("no session recorded"), "{text}");
}

#[test]
fn a_note_is_the_commit_view_as_text_and_reads_incomplete_until_a_refresh() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    let recording = pi_recording(&repo, EXECUTOR, &[0.05], false);
    let mut body = fs::read_to_string(&recording).unwrap();
    body.push_str("{ this record is not json\n");
    fs::write(&recording, body).unwrap();
    let shipped = shipped_change(&repo, "two");

    stdout(repo.arc(&repo.root).args(["cost", "note", &shipped.change]));
    for commit in [&shipped.first, &shipped.second] {
        let note = note_of(&repo, commit);
        let opening = note.lines().next().unwrap();
        assert!(opening.starts_with("arc cost as of 20"), "{note}");
        assert!(opening.contains(" reader "), "{note}");
        assert!(note.contains("pi:exec-run  fresh incomplete"), "{note}");
        assert!(
            note.contains("shared by the change's 2 commits; no figure is per commit"),
            "{note}"
        );
        assert!(note.contains("harness-calculated estimate"), "{note}");
        assert!(note.contains("accounting basis"), "{note}");
    }

    // The run finishes: its recording is whole.
    pi_recording(&repo, EXECUTOR, &[0.05], false);
    let before = git_out(
        &repo.root,
        &["notes", "--ref=arc-cost", "list", &shipped.first],
    );
    let output = stdout(repo.arc(&repo.root).args(["cost", "note", "--all"]));
    assert!(output.contains(&shipped.first[..12]), "{output}");
    for commit in [&shipped.first, &shipped.second] {
        let note = note_of(&repo, commit);
        assert!(note.contains("pi:exec-run  fresh complete"), "{note}");
        assert!(!note.contains("pi:exec-run  fresh incomplete"), "{note}");
    }
    let after = git_out(
        &repo.root,
        &["notes", "--ref=arc-cost", "list", &shipped.first],
    );
    assert_ne!(before, after);

    // A refresh with nothing to change leaves every note as it is.
    let settled = stdout(repo.arc(&repo.root).args(["cost", "note", "--all"]));
    assert!(settled.contains("unchanged"), "{settled}");
    let again = git_out(
        &repo.root,
        &["notes", "--ref=arc-cost", "list", &shipped.first],
    );
    assert_eq!(after, again);
}

#[test]
fn a_note_for_a_commit_resolves_to_its_change_and_never_reaches_a_push_refspec() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    pi_recording(&repo, EXECUTOR, &[0.05], false);
    let remote = repo.home.join("remote.git");
    git(&repo.home, &["init", "--bare", remote.to_str().unwrap()]);
    git(
        &repo.root,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(
        &repo.root,
        &[
            "config",
            "--add",
            "remote.origin.push",
            "refs/heads/*:refs/heads/*",
        ],
    );
    let shipped = shipped_change(&repo, "two");

    stdout(repo.arc(&repo.root).args(["cost", "note", &shipped.second]));
    assert!(note_of(&repo, &shipped.first).starts_with("arc cost as of"));
    assert!(note_of(&repo, &shipped.second).starts_with("arc cost as of"));
    let config = fs::read_to_string(repo.root.join(".git/config")).unwrap();
    assert!(!config.contains("refs/notes/arc-cost"), "{config}");
    let pushes = git_out(
        &repo.root,
        &["config", "--get-regexp", r"^remote\..*\.push$"],
    );
    assert!(!pushes.contains("notes"), "{pushes}");
    assert!(!remote.join("refs/notes").exists());
}

#[test]
fn a_note_carries_no_path_link_or_raw_error_text_even_from_a_tampered_retained_fact() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, &[0.2], false);
    let recording = pi_recording(&repo, EXECUTOR, &[0.05], false);
    let shipped = shipped_change(&repo, "two");
    stdout(
        repo.arc(&repo.root)
            .args(["cost", "retain", &shipped.change]),
    );
    let values = stdout(repo.arc(&repo.root).args(["events", "--repository"]));
    for mut held in values
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|e| e["event_type"] == "cost-facts-retained")
    {
        let private = recording.display().to_string();
        held["facts"]["lineage"]["parent"] = json!(private);
        held["facts"]["lineage"]["unknown"] = json!([format!("cannot read {private}")]);
        held["facts"]["digest"] = json!(private);
        held["facts"]["incomplete"] = json!([format!("cannot read {private}")]);
        let path = repo
            .root
            .join(".git/arc/repository/events")
            .join(format!("{}.json", held["event_id"].as_str().unwrap()));
        fs::write(path, serde_json::to_vec(&held).unwrap()).unwrap();
    }
    fs::remove_file(&recording).unwrap();
    fs::remove_file(
        repo.home
            .join(".pi/agent/sessions/--fixture-project--")
            .join(format!("2026-01-01T10-00-00-000Z_{LEAD}.jsonl")),
    )
    .unwrap();

    stdout(repo.arc(&repo.root).args(["cost", "note", &shipped.change]));
    let home = repo.home.display().to_string();
    for commit in [&shipped.first, &shipped.second] {
        let note = note_of(&repo, commit);
        assert!(note.contains("pi:exec-run  retained"), "{note}");
        assert!(!note.contains(&home), "{note}");
        assert!(!note.contains("cannot read"), "{note}");
        for line in note.lines() {
            assert!(
                !line.contains(['/', '\\', '~']) && !line.chars().any(char::is_control),
                "{line}"
            );
        }
        assert!(!note.contains("claude.ai"), "{note}");
    }
}
