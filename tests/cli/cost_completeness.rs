//! A cost figure reads complete only when every session that acted on the
//! work, or is bound to it, was measured to the end of its session. Liveness
//! comes from a stand-in `agent-status` the test writes into the fixture's
//! home and selects with `ARC_AGENT_STATUS`.

use super::common::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

const USD: &str = "harness-calculated-estimate";
const LEAD: &str = "lead-session";
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

/// A change the pi session `lead` opened, snapshotted and approved.
fn led_change(repo: &Repo, slug: &str, lead: &str) -> String {
    let change = opened_change_id(&stdout(
        acting(repo, &repo.root, lead).args(["begin", slug]),
    ));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: {slug}"),
    );
    stdout(acting(repo, &worktree, lead).args(["snapshot", slug]));
    acting(repo, &worktree, lead)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
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

/// Claim: a participant absent from an owned snapshot, for a harness
/// agent-status reports on, has ended and prices complete. Rejects a figure
/// that stays incomplete because a recording cannot show its own end.
#[test]
fn an_ended_participant_prices_complete() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 0.2);
    let change = led_change(&repo, "ended", LEAD);
    let status = snapshot(
        &repo,
        "other",
        json!([{"id": "another-session", "harness": "pi", "state": "working"}]),
    );

    let view = report(&repo, &change, &status);
    let figure = usd(&work(&view, &format!("change:{change}"))["direct"]);
    assert_eq!(figure["status"], "complete", "{view}");
    assert_eq!(figure["amount"].as_f64(), Some(0.2), "{view}");
    let row = session(&view, "pi:lead-session");
    assert_eq!(row["measurement"]["liveness"]["answer"], "ended", "{view}");
    assert_eq!(row["measurement"]["status"], "complete", "{view}");
    let text = stdout(
        repo.arc(&repo.root)
            .env("ARC_AGENT_STATUS", &status)
            .args(["cost", "show", "--detail", &change]),
    );
    assert!(text.contains("liveness: ended"), "{text}");
}

/// Claim: a participant agent-status lists, in any state, is live and stays
/// incomplete as of its frontier. Rejects reading a listed session as ended.
#[test]
fn a_live_participant_stays_incomplete() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 0.2);
    let change = led_change(&repo, "live", LEAD);
    for state in ["working", "completed", "idle", "attention"] {
        let status = snapshot(
            &repo,
            state,
            json!([{"id": LEAD, "harness": "pi", "state": state}]),
        );
        let view = report(&repo, &change, &status);
        let row = session(&view, "pi:lead-session");
        assert_eq!(
            row["measurement"]["liveness"]["answer"], "live",
            "{state}: {view}"
        );
        assert_eq!(
            row["measurement"]["status"], "incomplete",
            "{state}: {view}"
        );
        let figure = usd(&work(&view, &format!("change:{change}"))["direct"]);
        assert_eq!(figure["status"], "incomplete", "{state}: {view}");
        assert_eq!(figure["known_subtotal"].as_f64(), Some(0.2), "{view}");
        assert!(
            reasons(figure).iter().any(|reason| reason.contains("live")),
            "{view}"
        );
    }
}

/// Claim: no agent-status, a failing one, a slow one, unreadable output, or
/// a snapshot no registry owner answered leave a participant incomplete
/// with a reason, never complete. Rejects treating an empty or missing
/// answer as ended, and an agent-status that never answers blocks no read.
#[test]
fn unavailable_liveness_leaves_a_participant_incomplete_with_a_reason() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 0.2);
    let change = led_change(&repo, "unknown", LEAD);
    let cases = [
        ("absent", repo.home.join("bin/no-such-agent-status")),
        ("failing", agent_status(&repo, "failing", "exit 3")),
        (
            "unparseable",
            agent_status(&repo, "unparseable", "echo not-json"),
        ),
        (
            "ownerless",
            agent_status(&repo, "ownerless", "echo '{\"threads\":[]}'"),
        ),
        ("slow", agent_status(&repo, "slow", "sleep 30")),
    ];
    for (case, status) in cases {
        let started = Instant::now();
        let view = report(&repo, &change, &status);
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{case} took {:?}",
            started.elapsed()
        );
        let row = session(&view, "pi:lead-session");
        assert_eq!(
            row["measurement"]["liveness"]["answer"], "unknown",
            "{case}: {view}"
        );
        assert!(
            row["measurement"]["liveness"]["reason"].is_string(),
            "{case}: {view}"
        );
        let figure = usd(&work(&view, &format!("change:{change}"))["direct"]);
        assert_eq!(figure["status"], "incomplete", "{case}: {view}");
        assert!(
            reasons(figure)
                .iter()
                .any(|reason| reason.contains("liveness is unknown")),
            "{case}: {view}"
        );
    }
}

/// The liveness each retention of the lead recorded, oldest first.
fn retained_liveness(repo: &Repo) -> Vec<Value> {
    stdout(repo.arc(&repo.root).args(["events", "--repository"]))
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| {
            event["event_type"] == "cost-facts-retained" && event["executor_session"] == LEAD
        })
        .map(|event| event["facts"]["liveness"].clone())
        .collect()
}

/// Claim: retention records the liveness answer it was taken under, and a
/// figure retained while live stays incomplete after its source is gone,
/// while one retained after the session ended prices complete. Rejects
/// retained facts that lose whether their session was still running.
#[test]
fn retained_facts_keep_the_liveness_they_were_taken_under() {
    let repo = Repo::new();
    let path = pi_recording(&repo, LEAD, 0.2);
    let change = led_change(&repo, "retained", LEAD);
    let live = snapshot(
        &repo,
        "live",
        json!([{"id": LEAD, "harness": "pi", "state": "idle"}]),
    );
    let ended = snapshot(&repo, "ended", json!([]));
    let unavailable = repo.home.join("bin/no-such-agent-status");
    let retain = |status: &Path| {
        stdout(
            repo.arc(&repo.root)
                .env("ARC_AGENT_STATUS", status)
                .args(["cost", "retain", &change]),
        )
    };
    let direct = |view: &Value| usd(&work(view, &format!("change:{change}"))["direct"]).clone();

    retain(&live);
    assert_eq!(retained_liveness(&repo), vec![json!("live")]);
    let source = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    // Retained while live: the session may have gone on past that frontier.
    let view = report(&repo, &change, &ended);
    let row = session(&view, "pi:lead-session");
    assert_eq!(row["measurement"]["source"], "retained", "{view}");
    assert_eq!(row["measurement"]["liveness"]["retained"], "live", "{view}");
    assert_eq!(row["measurement"]["status"], "incomplete", "{view}");
    assert_eq!(direct(&view)["status"], "incomplete", "{view}");

    // Retained after the session ended: its frontier is final.
    fs::write(&path, source).unwrap();
    retain(&ended);
    assert_eq!(
        retained_liveness(&repo),
        vec![json!("live"), json!("ended")]
    );
    fs::remove_file(&path).unwrap();
    let view = report(&repo, &change, &unavailable);
    let row = session(&view, "pi:lead-session");
    assert_eq!(
        row["measurement"]["liveness"]["retained"], "ended",
        "{view}"
    );
    assert_eq!(row["measurement"]["status"], "complete", "{view}");
    assert_eq!(direct(&view)["amount"].as_f64(), Some(0.2), "{view}");

    // A fresh answer that lists the session again outranks the retained end.
    let view = report(&repo, &change, &live);
    assert_eq!(
        session(&view, "pi:lead-session")["measurement"]["status"],
        "incomplete",
        "{view}"
    );
}
