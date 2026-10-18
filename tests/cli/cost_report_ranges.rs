//! The cost report over a range of work: executor liveness from run ends,
//! the per-session table and its groups, and the ranges a view selects.
//! Recordings are written into the fixture's home, and liveness comes from a
//! stand-in `agent-status` selected with `ARC_AGENT_STATUS`.

use super::common::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

const USD: &str = "harness-calculated-estimate";

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

/// A pi recording, named as pi names it, whose one assistant request carries
/// a harness-written cost and 39 tokens.
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

/// `arc` naming no session, so its acting events join no figure.
fn anonymous(repo: &Repo) -> AssertCommand {
    let mut command = repo.arc(&repo.root);
    command.env_remove("ARC_HARNESS").env_remove("ARC_SESSION");
    command
}

fn begin(repo: &Repo, slug: &str, extra: &[&str]) -> String {
    let change = opened_change_id(&stdout(anonymous(repo).args(["begin", slug]).args(extra)));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: {slug}"),
    );
    change
}

/// Dispatch a run on `change` with `route`, before the recordings begin.
fn dispatch(repo: &Repo, change: &str, route: &str) -> String {
    let id = event_id(&stdout(anonymous(repo).args([
        "run",
        "dispatch",
        "--route",
        route,
        "--worktree",
        "/work/run",
        "--change",
        change,
    ])));
    let path = repo
        .root
        .join(".git/arc/repository/events")
        .join(format!("{id}.json"));
    let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    event["created_at"] = json!("2026-01-01T09:59:00Z");
    fs::write(path, serde_json::to_vec(&event).unwrap()).unwrap();
    id
}

fn bind(repo: &Repo, dispatch: &str, session: &str) {
    stdout(anonymous(repo).args([
        "run",
        "bind",
        dispatch,
        "--executor-harness",
        "pi",
        "--executor-session",
        session,
    ]));
}

fn end(repo: &Repo, dispatch: &str) {
    stdout(anonymous(repo).args(["run", "end", dispatch, "--outcome", "completed"]));
}

/// A stand-in, named `name`, that answers one owned snapshot listing
/// `threads`.
fn snapshot(repo: &Repo, name: &str, threads: Value) -> PathBuf {
    let answer = json!({"threads": threads, "owner": {"pid": 1}});
    let file = repo.home.join("bin").join(format!("{name}.json"));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, answer.to_string()).unwrap();
    let path = repo.home.join("bin").join(name);
    fs::write(&path, format!("#!/bin/sh\ncat '{}'\n", file.display())).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn report(repo: &Repo, args: &[&str]) -> Value {
    let report = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--json"])
            .args(args),
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

fn reasons(value: &Value) -> Vec<String> {
    value["reasons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|reason| reason.as_str().map(str::to_string))
        .collect()
}

/// Claim: an executor whose bound dispatch has no recorded run end is still
/// running, so its figure is incomplete as of its frontier and names the
/// open dispatch. Rejects pricing a running executor as complete.
#[test]
fn a_running_executor_is_live_and_incomplete() {
    let repo = Repo::new();
    pi_recording(&repo, "running-exec", 0.05);
    let change = begin(&repo, "running", &[]);
    let run = dispatch(&repo, &change, "fixture-route");
    bind(&repo, &run, "running-exec");

    let view = report(&repo, &[&change]);
    let figure = usd(&work(&view, &format!("change:{change}"))["direct"]);
    assert_eq!(figure["status"], "incomplete", "{view}");
    assert_eq!(figure["known_subtotal"].as_f64(), Some(0.05), "{view}");
    assert!(
        reasons(figure)
            .iter()
            .any(|reason| reason.contains(&run) && reason.contains("no recorded run end")),
        "{view}"
    );
    let row = session(&view, "pi:running-exec");
    assert_eq!(
        row["measurement"]["liveness"]["runs"],
        json!({"ended": 0, "dispatches": 1}),
        "{view}"
    );
    assert_eq!(row["measurement"]["liveness"]["final"], false, "{view}");
}

/// Claim: an executor whose every bound dispatch ended keeps its figure,
/// with agent-status unavailable; one agent-status still lists is live even
/// after its run ended. Rejects both a finished run read as running and a
/// listing ignored.
#[test]
fn an_executor_whose_dispatches_all_ended_keeps_its_figure_unless_listed() {
    let repo = Repo::new();
    pi_recording(&repo, "done-exec", 0.05);
    let change = begin(&repo, "done", &[]);
    for _ in 0..2 {
        let run = dispatch(&repo, &change, "fixture-route");
        bind(&repo, &run, "done-exec");
        end(&repo, &run);
    }

    let view = report(&repo, &[&change]);
    let figure = usd(&work(&view, &format!("change:{change}"))["direct"]);
    assert_eq!(figure["status"], "complete", "{view}");
    assert_eq!(figure["amount"].as_f64(), Some(0.05), "{view}");
    assert_eq!(
        session(&view, "pi:done-exec")["measurement"]["liveness"]["runs"],
        json!({"ended": 2, "dispatches": 2}),
        "{view}"
    );

    let listed = snapshot(
        &repo,
        "listed",
        json!([{"id": "done-exec", "harness": "pi", "state": "idle"}]),
    );
    let view = json_stdout(
        repo.arc(&repo.root)
            .env("ARC_AGENT_STATUS", &listed)
            .args(["cost", "show", "--json", &change]),
    );
    let figure = usd(&work(&view, &format!("change:{change}"))["direct"]);
    assert_eq!(figure["status"], "incomplete", "{view}");
    assert!(
        reasons(figure)
            .iter()
            .any(|reason| reason.contains("executor session is live")),
        "{view}"
    );
}

/// Claim: facts retained before an executor's run ended stay incomplete once
/// the source is gone, and facts retained after it ended are final. Rejects
/// a retained frontier read as final when the run went on past it.
#[test]
fn retained_executor_facts_are_final_only_after_the_run_ended() {
    let repo = Repo::new();
    let path = pi_recording(&repo, "kept-exec", 0.05);
    let change = begin(&repo, "kept", &[]);
    let run = dispatch(&repo, &change, "fixture-route");
    bind(&repo, &run, "kept-exec");
    stdout(repo.arc(&repo.root).args(["cost", "retain", &change]));
    end(&repo, &run);
    let source = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();

    let view = report(&repo, &[&change]);
    assert_eq!(
        session(&view, "pi:kept-exec")["measurement"]["source"],
        "retained",
        "{view}"
    );
    let figure = usd(&work(&view, &format!("change:{change}"))["direct"]);
    assert_eq!(figure["status"], "incomplete", "{view}");
    assert!(
        reasons(figure)
            .iter()
            .any(|reason| reason.contains("retained before its last run ended")),
        "{view}"
    );

    // The run went on past the first retention, then ended.
    let mut grown = source;
    grown.extend_from_slice(
        format!(
            "{}\n",
            json!({"type": "message", "id": "assistant-2", "parentId": "assistant-1", "timestamp": "2026-01-01T10:00:03Z",
                "message": {"role": "assistant", "timestamp": 1767261603000u64, "provider": "fixture", "model": "fixture-model",
                    "usage": {"input": 1, "output": 2, "reasoning": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 3,
                        "cost": {"total": 0.01}},
                    "content": [{"type": "text", "text": "More."}]}})
        )
        .as_bytes(),
    );
    fs::write(&path, grown).unwrap();
    let retained = stdout(repo.arc(&repo.root).args(["cost", "retain", &change]));
    assert!(retained.contains("retained: pi:kept-exec"), "{retained}");
    fs::remove_file(&path).unwrap();
    let view = report(&repo, &[&change]);
    let figure = usd(&work(&view, &format!("change:{change}"))["direct"]);
    assert_eq!(figure["status"], "complete", "{view}");
    assert_eq!(figure["amount"].as_f64(), Some(0.06), "{view}");
}
