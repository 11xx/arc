//! A cost figure reads complete only when every session that acted on the
//! work, or is bound to it, was measured to the end of its session. Liveness
//! comes from a stand-in `agent-status` the test writes into the fixture's
//! home and selects with `ARC_AGENT_STATUS`.

use super::common::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

const USD: &str = "harness-calculated-estimate";
const EXECUTOR: &str = "exec-session";

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

/// A pi recording, named as pi names it, whose one assistant request carries
/// a harness-written cost.
fn pi_recording(repo: &Repo, id: &str, cost: f64) -> PathBuf {
    let path = repo
        .home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T10-00-00-000Z_{id}.jsonl"));
    write_lines(
        &path,
        &[
            json!({"type": "session", "version": 3, "id": id, "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project"}),
            json!({"type": "message", "id": "user-1", "parentId": null, "timestamp": "2026-01-01T10:00:01Z",
                "message": {"role": "user", "timestamp": 1767261601000u64, "content": [{"type": "text", "text": "Work."}]}}),
            json!({"type": "message", "id": "assistant-1", "parentId": "user-1", "timestamp": "2026-01-01T10:00:02Z",
                "message": {"role": "assistant", "timestamp": 1767261602000u64, "provider": "fixture", "model": "fixture-model",
                    "usage": {"input": 10, "output": 20, "reasoning": 3, "cacheRead": 4, "cacheWrite": 5, "totalTokens": 39,
                        "cost": {"total": cost}},
                    "content": [{"type": "text", "text": "Done."}]}}),
        ],
    );
    path
}

fn event_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .expect("command output should contain an event id")
        .to_string()
}

/// `arc` acting as a pi session.
fn acting(repo: &Repo, cwd: &Path, session: &str) -> AssertCommand {
    let mut command = repo.arc(cwd);
    command.env("ARC_HARNESS", "pi").env("ARC_SESSION", session);
    command
}

/// A change with a dispatch bound to the pi executor session, opened before
/// the executor's recording begins. The dispatcher names no session, so it
/// joins no figure.
fn executed_change(repo: &Repo, slug: &str) -> String {
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: {slug}"),
    );
    let mut anonymous = repo.arc(&repo.root);
    anonymous
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION");
    let dispatch = event_id(&stdout(anonymous.args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        "--change",
        &change,
    ])));
    let path = repo
        .root
        .join(".git/arc/repository/events")
        .join(format!("{dispatch}.json"));
    let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    event["created_at"] = json!("2026-01-01T09:59:00Z");
    fs::write(path, serde_json::to_vec(&event).unwrap()).unwrap();
    stdout(repo.arc(&repo.root).args([
        "run",
        "bind",
        &dispatch,
        "--executor-harness",
        "pi",
        "--executor-session",
        EXECUTOR,
    ]));
    change
}

/// A stand-in `agent-status`, named `name`, whose bare invocation runs `body`.
fn agent_status(repo: &Repo, name: &str, body: &str) -> PathBuf {
    let path = repo.home.join("bin").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A stand-in, named `name`, that answers one owned snapshot listing
/// `threads`.
fn snapshot(repo: &Repo, name: &str, threads: Value) -> PathBuf {
    let answer = json!({"threads": threads, "owner": {"pid": 1, "start_ticks": 1, "started_at": 1, "build": "fixture"}});
    let file = repo.home.join("bin").join(format!("{name}.json"));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, answer.to_string()).unwrap();
    agent_status(repo, name, &format!("cat '{}'", file.display()))
}

fn report(repo: &Repo, target: &str, status: &Path) -> Value {
    let report = json_stdout(
        repo.arc(&repo.root)
            .env("ARC_AGENT_STATUS", status)
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

fn usd(totals: &Value) -> &Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals}"))
        .iter()
        .find(|quantity| quantity["kind"] == USD && quantity.get("partition").is_none())
        .unwrap_or_else(|| panic!("no USD quantity: {totals}"))
}

fn session<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == key)
        .unwrap_or_else(|| panic!("no session {key}: {report}"))
}

fn outside<'a>(report: &'a Value, key: &str) -> Option<&'a Value> {
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

fn reasons(value: &Value) -> Vec<String> {
    value["reasons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|reason| reason.as_str().map(str::to_string))
        .collect()
}

/// A figure that leaves out a known contribution reads neither complete nor
/// as an amount, and names the session it lacks.
fn assert_qualified(figure: &Value, missing: &str, view: &Value) {
    assert_ne!(figure["status"], "complete", "{figure}\n{view}");
    assert!(figure.get("amount").is_none(), "{figure}\n{view}");
    assert!(
        reasons(figure)
            .iter()
            .any(|reason| reason.contains(missing)),
        "{figure}\n{view}"
    );
}

/// Claim: an unreadable participant beside a complete $0.05 executor keeps
/// the union, direct and USD figures from reading complete, and measurement
/// and assignment coverage agree with the outside listing. Rejects the view
/// that drops the participant and reports a complete, smaller total.
#[test]
fn an_unreadable_participant_beside_a_priced_executor_never_reads_complete() {
    let repo = Repo::new();
    pi_recording(&repo, EXECUTOR, 0.05);
    let change = executed_change(&repo, "repro");
    let worktree = repo.home.join(".worktrees/repo-repro");
    stdout(acting(&repo, &worktree, "broken").args(["snapshot", "repro"]));
    let status = snapshot(&repo, "empty", json!([]));

    let view = report(&repo, &change, &status);
    let item = work(&view, &format!("change:{change}"));
    assert_qualified(usd(&view["union"]), "pi:broken", &view);
    assert_qualified(usd(&item["direct"]), "pi:broken", &view);
    let coverage = &view["coverage"];
    assert_eq!(coverage["measurement"]["status"], "incomplete", "{view}");
    assert!(
        coverage["measurement"]["sessions"]["pi:broken"].is_array(),
        "{view}"
    );
    assert_eq!(coverage["assignment"]["status"], "incomplete", "{view}");
    assert!(
        reasons(&coverage["assignment"])
            .iter()
            .any(|reason| reason.contains("pi:broken")),
        "{view}"
    );
    let broken = outside(&view, "pi:broken").unwrap_or_else(|| panic!("outside: {view}"));
    assert!(broken["reason"].is_string(), "{view}");
    assert_eq!(
        session(&view, "pi:broken")["measurement"]["status"],
        "unmeasured",
        "{view}"
    );
}

/// Claim: a bound executor whose read fails with nothing retained keeps the
/// figures it belongs to from reading complete. Rejects a total that counts
/// only the readable executor.
#[test]
fn an_unreadable_bound_executor_beside_a_priced_one_never_reads_complete() {
    let repo = Repo::new();
    pi_recording(&repo, EXECUTOR, 0.05);
    let change = executed_change(&repo, "unread");
    let mut anonymous = repo.arc(&repo.root);
    anonymous
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION");
    let second = event_id(&stdout(anonymous.args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        "--change",
        &change,
    ])));
    stdout(repo.arc(&repo.root).args([
        "run",
        "bind",
        &second,
        "--executor-harness",
        "pi",
        "--executor-session",
        "missing-exec",
    ]));
    let status = snapshot(&repo, "empty", json!([]));

    let view = report(&repo, &change, &status);
    let item = work(&view, &format!("change:{change}"));
    assert_qualified(usd(&view["union"]), "pi:missing-exec", &view);
    assert_qualified(usd(&item["direct"]), "pi:missing-exec", &view);
    assert_eq!(
        view["coverage"]["measurement"]["status"], "incomplete",
        "{view}"
    );
}
