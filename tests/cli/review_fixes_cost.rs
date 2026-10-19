//! Cost views answer from what was read: a figure retained while its session
//! ran stays a known subtotal once it ends, `--by change` subtotals each
//! selected change, a recorded model keeps its provider's namespace and case,
//! `cost retain` reads each repository record once, and `query --commit`
//! names the changes it could not read. Pi recordings are written into the
//! fixture's home, named as Pi names them, and liveness comes from a
//! stand-in `agent-status` selected with `ARC_AGENT_STATUS`.

use super::common::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

const USD: &str = "harness-calculated-estimate";
const LEAD: &str = "019a0000-0000-7000-8000-00000000000a";
const EXECUTOR: &str = "019a0000-0000-7000-8000-00000000000e";

fn at(clock: &str) -> String {
    format!("2026-01-01T{clock}Z")
}

/// A Pi recording whose assistant requests, served by `model`, each record
/// `input` uncached tokens and a harness-written cost at their own instant.
fn pi_session(repo: &Repo, id: &str, model: &str, requests: &[(&str, u64, f64)]) -> PathBuf {
    let path = repo
        .home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T09-00-00-000Z_{id}.jsonl"));
    let mut lines = vec![
        json!({"type": "session", "version": 3, "id": id, "timestamp": at("09:00:00"), "cwd": "/fixture/project"}),
        json!({"type": "model_change", "id": "model-0", "parentId": null, "timestamp": at("09:00:00"),
            "provider": "fixture", "modelId": model}),
        json!({"type": "message", "id": "user-0", "parentId": "model-0", "timestamp": at("09:00:01"),
            "message": {"role": "user", "timestamp": 1767258001000u64, "content": [{"type": "text", "text": "Work."}]}}),
    ];
    let mut parent = "user-0".to_string();
    for (n, (clock, input, cost)) in requests.iter().enumerate() {
        let entry = format!("assistant-{n}");
        lines.push(json!({"type": "message", "id": entry, "parentId": parent, "timestamp": at(clock),
            "message": {"role": "assistant", "timestamp": 1767258002000u64, "provider": "fixture", "model": model,
                "usage": {"input": input, "output": 200, "cacheRead": 400, "cacheWrite": 0, "totalTokens": input + 600,
                    "cost": {"total": cost}},
                "content": [{"type": "text", "text": "Done."}]}}));
        parent = entry;
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(&path, body).unwrap();
    path
}

fn event_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .expect("command output should contain an event id")
        .to_string()
}

/// Move a ledger event, wherever it is stored, to `instant`.
fn retime(repo: &Repo, id: &str, instant: &str) {
    let name = format!("{id}.json");
    let mut pending = vec![repo.root.join(".git/arc")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|file| file == name.as_str()) {
                let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                event["created_at"] = json!(instant);
                fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
                return;
            }
        }
    }
    panic!("no ledger event {id}");
}

fn anonymous(repo: &Repo, cwd: &Path) -> AssertCommand {
    let mut command = repo.arc(cwd);
    command.env_remove("ARC_HARNESS").env_remove("ARC_SESSION");
    command
}

fn acting(repo: &Repo, cwd: &Path, session: &str) -> AssertCommand {
    let mut command = repo.arc(cwd);
    command.env("ARC_HARNESS", "pi").env("ARC_SESSION", session);
    command
}

/// Open `slug` naming no session, commit once in its worktree and snapshot
/// it. Returns the change id and the commit.
fn snapshotted(repo: &Repo, slug: &str) -> (String, String) {
    let change = opened_change_id(&stdout(anonymous(repo, &repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        "x\n",
        &format!("feat: {slug}"),
    );
    let commit = repo.head(&worktree);
    stdout(anonymous(repo, &worktree).args(["snapshot", slug]));
    (change, commit)
}

/// Dispatch a run for `change` at `start` on `route`, bind it to the
/// executor, and end it at `end`.
fn run_on(repo: &Repo, route: &str, change: &str, start: &str, end: &str) {
    let dispatch = event_id(&stdout(anonymous(repo, &repo.root).args([
        "run",
        "dispatch",
        "--route",
        route,
        "--worktree",
        "/work/run",
        "--change",
        change,
    ])));
    retime(repo, &dispatch, &at(start));
    stdout(anonymous(repo, &repo.root).args([
        "run",
        "bind",
        &dispatch,
        "--executor-harness",
        "pi",
        "--executor-session",
        EXECUTOR,
    ]));
    let ended = event_id(&stdout(anonymous(repo, &repo.root).args([
        "run",
        "end",
        &dispatch,
        "--outcome",
        "completed",
    ])));
    retime(repo, &ended, &at(end));
}

fn run(repo: &Repo, change: &str, start: &str, end: &str) {
    run_on(repo, "fixture-route", change, start, end);
}

/// A stand-in `agent-status`, named `name`, answering one owned snapshot
/// that lists `threads`.
fn snapshot(repo: &Repo, name: &str, threads: Value) -> PathBuf {
    let answer = json!({"threads": threads, "owner": {"pid": 1, "start_ticks": 1, "started_at": 1, "build": "fixture"}});
    let file = repo.home.join("bin").join(format!("{name}.json"));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, answer.to_string()).unwrap();
    let path = repo.home.join("bin").join(name);
    fs::write(&path, format!("#!/bin/sh\ncat '{}'\n", file.display())).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn usd(totals: &Value) -> &Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals}"))
        .iter()
        .find(|quantity| quantity["kind"] == USD && quantity.get("partition").is_none())
        .unwrap_or_else(|| panic!("no USD quantity: {totals}"))
}

fn uncached(totals: &Value) -> Option<u64> {
    let quantity = totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals}"))
        .iter()
        .find(|quantity| {
            quantity.get("partition").is_none()
                && quantity["categories"] == "disjoint"
                && quantity["category"] == "uncached_input"
        })
        .unwrap_or_else(|| panic!("no uncached input: {totals}"));
    quantity["amount"]
        .as_u64()
        .or_else(|| quantity["known_subtotal"].as_u64())
}

/// Claim: participant facts retained while the session was live stay a
/// known subtotal after the source rotates, even when a fresh agent-status
/// snapshot says the session has ended, because the session went on past
/// their frontier. Rejects pricing the retained $0.20 as the complete figure
/// of a session that spent $0.50.
#[test]
fn facts_retained_while_live_stay_a_subtotal_once_the_session_ends() {
    let repo = Repo::new();
    let path = pi_session(&repo, LEAD, "fixture-model", &[("10:05:00", 10, 0.2)]);
    let change = opened_change_id(&stdout(
        acting(&repo, &repo.root, LEAD).args(["begin", "retained"]),
    ));
    let worktree = repo.home.join(".worktrees/repo-retained");
    repo.commit(&worktree, "retained.txt", "x\n", "feat: retained");
    stdout(acting(&repo, &worktree, LEAD).args(["snapshot", "retained"]));
    let live = snapshot(
        &repo,
        "live",
        json!([{"id": LEAD, "harness": "pi", "state": "working"}]),
    );
    let ended = snapshot(&repo, "ended", json!([]));
    stdout(
        repo.arc(&repo.root)
            .env("ARC_AGENT_STATUS", &live)
            .args(["cost", "retain", &change]),
    );
    // The session goes on, then ends, and its recording rotates away.
    pi_session(
        &repo,
        LEAD,
        "fixture-model",
        &[("10:05:00", 10, 0.2), ("10:15:00", 30, 0.3)],
    );
    fs::remove_file(&path).unwrap();

    let view = json_stdout(
        repo.arc(&repo.root)
            .env("ARC_AGENT_STATUS", &ended)
            .args(["cost", "show", "--json", &change]),
    );
    let row = view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == format!("pi:{LEAD}"))
        .unwrap();
    assert_eq!(row["measurement"]["source"], "retained", "{view}");
    assert_eq!(row["measurement"]["liveness"]["answer"], "ended", "{view}");
    let direct = usd(&view["work"][0]["direct"]);
    assert_ne!(direct["status"], "complete", "{view}");
    assert!(direct.get("amount").is_none(), "{view}");
    assert_eq!(direct["known_subtotal"].as_f64(), Some(0.2), "{view}");
}

/// Claim: `--by change` over three of the five changes a warm executor
/// served gives one subtotal per selected change, each holding that
/// change's share. Rejects a single group keyed by the session's whole
/// membership, unselected changes included.
#[test]
fn by_change_subtotals_each_selected_change() {
    let repo = Repo::new();
    pi_session(
        &repo,
        EXECUTOR,
        "fixture-model",
        &[
            ("10:05:00", 10, 0.01),
            ("10:25:00", 100, 0.1),
            ("10:45:00", 1000, 1.0),
            ("11:05:00", 3, 0.003),
            ("11:25:00", 7, 0.007),
        ],
    );
    let slugs = ["warm-a", "warm-b", "warm-c", "warm-d", "warm-e"];
    let windows = [
        ("08:59:00", "10:10:00"),
        ("10:20:00", "10:30:00"),
        ("10:40:00", "10:50:00"),
        ("11:00:00", "11:10:00"),
        ("11:20:00", "11:30:00"),
    ];
    let changes: Vec<String> = slugs
        .iter()
        .zip(windows)
        .map(|(slug, (start, end))| {
            let (change, _) = snapshotted(&repo, slug);
            run(&repo, &change, start, end);
            change
        })
        .collect();
    let view = json_stdout(repo.arc(&repo.root).args([
        "cost",
        "show",
        "--json",
        "--by",
        "change",
        &changes[0],
        &changes[1],
        &changes[2],
    ]));
    let groups = view["table"]["groups"].as_array().unwrap();
    let labels: Vec<&str> = groups
        .iter()
        .map(|group| group["by"]["change"].as_str().unwrap())
        .collect();
    let expected: Vec<String> = changes[..3]
        .iter()
        .map(|change| format!("change:{change}"))
        .collect();
    assert_eq!(labels, expected, "{view}");
    for (group, tokens) in groups.iter().zip([10, 100, 1000]) {
        assert_eq!(
            group["sessions"],
            json!([format!("pi:{EXECUTOR}")]),
            "{view}"
        );
        assert!(
            (group["usd"]["amount"].as_f64().unwrap() - tokens as f64 / 1000.0).abs() < 1e-9,
            "{group}\n{view}"
        );
    }
    assert_eq!(uncached(&view["union"]), Some(1110), "{view}");
}

/// Claim: a session whose recording names a namespaced model,
/// `z-ai/glm-5.3-flash`, is labelled and grouped under that model, and a
/// note can still be written for its change. Rejects dropping the model to
/// a bare `pi` agent in an `unestablished` model group.
#[test]
fn a_namespaced_recorded_model_establishes_the_agent_and_its_group() {
    let repo = Repo::new();
    pi_session(
        &repo,
        EXECUTOR,
        "z-ai/glm-5.3-flash",
        &[("10:05:00", 10, 0.01)],
    );
    let (change, _) = snapshotted(&repo, "namespaced");
    // A route in prose names no model, so only the recording does.
    run_on(&repo, "the fixture route", &change, "08:59:00", "10:10:00");
    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--json", "--by", "model", &change]),
    );
    let row = view["table"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == format!("pi:{EXECUTOR}"))
        .unwrap();
    assert_eq!(row["agent"], "pi:z-ai/glm-5.3-flash", "{view}");
    let groups = view["table"]["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "{view}");
    assert_eq!(groups[0]["by"]["model"], "z-ai/glm-5.3-flash", "{view}");
    stdout(repo.arc(&repo.root).args(["cost", "note", &change]));
}

/// Claim: `cost retain` reads each repository event file from disk once,
/// the records published while it read the sources included. Rejects a
/// second full read of the repository store after taking its lock.
#[test]
fn retain_reads_each_repository_record_once() {
    let repo = Repo::new();
    pi_session(&repo, EXECUTOR, "fixture-model", &[("10:05:00", 10, 0.01)]);
    for slug in ["kept-a", "kept-b"] {
        let (change, _) = snapshotted(&repo, slug);
        run(&repo, &change, "08:59:00", "10:10:00");
    }
    let counts = repo.home.join("ledger-reads.json");
    let output = stdout(
        repo.arc(&repo.root)
            .env("ARC_TEST_LEDGER_READS", &counts)
            .args(["cost", "retain", "kept-a"]),
    );
    assert!(
        output.contains(&format!("retained: pi:{EXECUTOR}")),
        "{output}"
    );
    let counts: Value = serde_json::from_slice(&fs::read(&counts).unwrap()).unwrap();
    assert_eq!(
        counts["most_reads_per_file"]["repository"],
        json!(1),
        "{counts}"
    );
    let again = stdout(repo.arc(&repo.root).args(["cost", "retain", "kept-a"]));
    assert!(again.contains("already retained"), "{again}");
}

/// Make a change's state unreadable by corrupting its opening event.
fn corrupt(repo: &Repo, change: &str) {
    for entry in fs::read_dir(event_dir(repo, change)).unwrap() {
        let path = entry.unwrap().path();
        let event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] == "change-opened" {
            fs::write(path, "corrupt opening").unwrap();
        }
    }
}

/// Claim: `query --commit` beside an unreadable change answers from the
/// readable changes and names the unreadable one: a match is qualified as
/// possibly not unique, and no match reads as the gap. Rejects a query that
/// fails outright, and one that prints nothing as if no change recorded the
/// commit.
#[test]
fn query_commit_names_the_changes_it_could_not_read() {
    let repo = Repo::new();
    let (hidden, hidden_commit) = snapshotted(&repo, "hidden");
    let (visible, visible_commit) = snapshotted(&repo, "visible");
    corrupt(&repo, &hidden);

    let matched = repo
        .arc(&repo.root)
        .args(["query", "--commit", &visible_commit])
        .assert()
        .success();
    let output = matched.get_output();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.lines().next().unwrap().starts_with(&visible), "{text}");
    assert!(text.contains("may not be unique"), "{text}");
    assert!(text.contains(&hidden), "{text}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("unreadable change {hidden}")),
        "{stderr}"
    );

    let gap = stdout(
        repo.arc(&repo.root)
            .args(["query", "--commit", &hidden_commit]),
    );
    assert!(
        gap.contains("no readable change records this commit"),
        "{gap}"
    );
    assert!(gap.contains(&hidden), "{gap}");
}
