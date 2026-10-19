//! Cost view figures a final review found mishandled or uncovered: a Pi
//! figure priced by its own requests under a version 2 table's time-of-day
//! term, the most a bounded list price can cost beside the least, and
//! lineage evidence that lies where a bounded read does not reach.
//! Recordings are written into the fixture's own home, named as each
//! harness names them, so tapes reads them as it reads a harness store.

use super::common::*;
use serde_json::{json, Value};

const LIST: &str = "list-price-estimate";
const PI: &str = "019a0000-0000-7000-8000-0000000000a1";

fn at(clock: &str) -> String {
    format!("2026-01-01T{clock}Z")
}

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

fn anonymous(repo: &Repo) -> AssertCommand {
    let mut command = repo.arc(&repo.root);
    command.env_remove("ARC_HARNESS").env_remove("ARC_SESSION");
    command
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

/// A change with one run bound to each `(harness, session)` executor,
/// dispatched before the recordings begin and ended after them.
fn executed_change(repo: &Repo, slug: &str, executors: &[(&str, &str)]) -> String {
    let change = opened_change_id(&stdout(anonymous(repo).args([
        "begin",
        slug,
        "--no-worktree",
        "--target",
        "master",
    ])));
    for (harness, session) in executors {
        let dispatch = event_id(&stdout(anonymous(repo).args([
            "run",
            "dispatch",
            "--route",
            "fixture-route",
            "--worktree",
            "/work/run",
            "--change",
            &change,
        ])));
        retime(repo, &dispatch, &at("09:00:00"));
        stdout(anonymous(repo).args([
            "run",
            "bind",
            &dispatch,
            "--executor-harness",
            harness,
            "--executor-session",
            session,
        ]));
        let ended = event_id(&stdout(anonymous(repo).args([
            "run",
            "end",
            &dispatch,
            "--outcome",
            "completed",
        ])));
        retime(repo, &ended, &at("23:59:00"));
    }
    change
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

fn session<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == key)
        .unwrap_or_else(|| panic!("no session {key}: {report:#}"))
}

fn list_price(totals: &Value) -> &Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals:#}"))
        .iter()
        .find(|quantity| quantity.get("partition").is_none() && quantity["kind"] == LIST)
        .unwrap_or_else(|| panic!("no list price: {totals:#}"))
}

fn close(value: &Value, expected: f64, context: &Value) {
    let value = value
        .as_f64()
        .unwrap_or_else(|| panic!("no amount: {context:#}"));
    assert!(
        (value - expected).abs() < 1e-9,
        "{value} != {expected}: {context:#}"
    );
}

// --- Pi requests under a version 2 table's time-of-day term ---------------

/// A table whose one row halves its rates from 16:30 to 00:30 UTC.
const TIME_OF_DAY: &str = r#"schema = "tapes-price-table/2"
currency = "USD"

[[models]]
id = "fixture-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.25, output = 4.0 }

[[models.time_of_day]]
from = "16:30"
until = "00:30"
per_million = { uncached_input = 0.5, cache_read = 0.05, cache_write_5m = 0.625, output = 2.0 }
"#;

/// One request of 1M uncached input tokens inside and outside the window.
const PEAK: f64 = 1.0;
const OFF_PEAK: f64 = 0.5;

fn time_of_day_table(repo: &Repo) -> String {
    let path = repo.home.join("prices.toml");
    fs::write(&path, TIME_OF_DAY).unwrap();
    path.to_string_lossy().to_string()
}

/// A Pi recording opened at 09:30 whose turns each hold a user prompt
/// under `parent` and the request answering it, of 1M uncached input
/// tokens, recorded at `clock`, or with no time of its own when `clock` is
/// absent. Entries name their turn's index: `user-<n>`, `assistant-<n>`.
fn pi_session(repo: &Repo, id: &str, turns: &[(&str, Option<&str>)]) {
    let path = repo
        .home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T09-30-00-000Z_{id}.jsonl"));
    let mut lines = vec![
        json!({"type": "session", "version": 3, "id": id, "timestamp": at("09:30:00"), "cwd": "/fixture/project"}),
        json!({"type": "model_change", "id": "model-1", "parentId": null, "timestamp": at("09:30:00"),
            "provider": "fixture", "modelId": "fixture-model"}),
    ];
    for (n, (parent, clock)) in turns.iter().enumerate() {
        let user = format!("user-{n}");
        lines.push(json!({"type": "message", "id": user, "parentId": parent, "timestamp": at(clock.unwrap_or("09:31:00")),
            "message": {"role": "user", "timestamp": 1767259860000u64, "content": [{"type": "text", "text": "Go on."}]}}));
        let mut assistant = json!({"type": "message", "id": format!("assistant-{n}"), "parentId": user,
            "message": {"role": "assistant", "timestamp": 1767259860000u64, "provider": "fixture", "model": "fixture-model",
                "stopReason": "stop",
                "usage": {"input": 1_000_000, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 1_000_000,
                    "cost": {"total": 1.0}},
                "content": [{"type": "text", "text": "Done."}]}});
        if let Some(clock) = clock {
            assistant["timestamp"] = json!(at(clock));
        }
        lines.push(assistant);
    }
    write_lines(&path, &lines);
}

/// A single branch: one request at peak, one off-peak.
fn single_branch(repo: &Repo, id: &str) {
    pi_session(
        repo,
        id,
        &[
            ("model-1", Some("10:00:00")),
            ("assistant-0", Some("17:00:00")),
        ],
    );
}

/// A 10:00 request whose follow-up at `abandoned` was abandoned for one at
/// 17:00, the active branch's leaf.
fn abandoned_branch(repo: &Repo, id: &str, abandoned: Option<&str>) {
    pi_session(
        repo,
        id,
        &[
            ("model-1", Some("10:00:00")),
            ("assistant-0", abandoned),
            ("assistant-0", Some("17:00:00")),
        ],
    );
}

/// Claim: a Pi figure is priced under a time-of-day term by each of its
/// requests' own times, so a session of one peak and one off-peak request
/// is complete at their sum, and one whose abandoned branch holds a third
/// request at peak counts that request at its own rate too. Rejects
/// leaving every Pi figure a known subtotal at the least it can cost,
/// `a price term set by the request may apply to requests the figure does
/// not hold one by one`, as though no request were kept.
#[test]
fn a_pi_figure_is_priced_by_its_own_requests() {
    for (branches, expected) in [(false, PEAK + OFF_PEAK), (true, 2.0 * PEAK + OFF_PEAK)] {
        let repo = Repo::new();
        if branches {
            abandoned_branch(&repo, PI, Some("12:00:00"));
        } else {
            single_branch(&repo, PI);
        }
        let change = executed_change(&repo, "pi", &[("pi", PI)]);
        let prices = time_of_day_table(&repo);
        let view = report(&repo, &[&change, "--prices", &prices]);
        let row = session(&view, &format!("pi:{PI}"));
        assert_eq!(
            row["measurement"]["events"]["other_branches"],
            u64::from(branches),
            "{row:#}"
        );
        let list = list_price(&row["totals"]);
        assert_eq!(list["status"], "complete", "{list:#}");
        close(&list["amount"], expected, list);
        let union = list_price(&view["union"]);
        assert_eq!(union["status"], "complete", "{union:#}");
        close(&union["amount"], expected, union);
    }
}

// --- Lineage evidence outside a bounded read's windows ---------------------

const CODEX: &str = "019a0000-0000-7000-8000-0000000000b1";
const CLAUDE: &str = "019a0000-0000-7000-8000-0000000000d1";
const AGENT: &str = "d1000000000000001";

/// Bytes before the evidence, past tapes' 1 MiB head probe.
const BEFORE: usize = 2 * 1024 * 1024;
/// Bytes after the evidence, past tapes' 4 MiB bounded tail.
const AFTER: usize = 5 * 1024 * 1024;

/// A Codex rollout whose only lineage evidence, a `spawn_agent` call and
/// its output naming the spawned agent, sits between `BEFORE` and `AFTER`
/// bytes of prompts. The spawned agent's own rollout is not in the store.
fn codex_spawner(repo: &Repo) {
    let path = repo
        .home
        .join(".codex/sessions/2026/01/01")
        .join(format!("rollout-2026-01-01T10-00-00-{CODEX}.jsonl"));
    let prompt = |clock: &str, size: usize| {
        json!({"timestamp": at(clock), "type": "response_item", "payload": {"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": "x".repeat(size)}]}})
    };
    let usage = json!({"input_tokens": 100, "cached_input_tokens": 0, "output_tokens": 20,
        "reasoning_output_tokens": 0, "total_tokens": 120});
    write_lines(
        &path,
        &[
            json!({"timestamp": at("10:00:00"), "type": "session_meta", "payload": {"id": CODEX, "session_id": CODEX,
                "timestamp": at("10:00:00"), "cwd": "/fixture/project", "model_provider": "openai"}}),
            json!({"timestamp": at("10:00:01"), "type": "turn_context", "payload": {"cwd": "/fixture/project", "model": "fixture-model", "effort": "high"}}),
            prompt("10:00:02", BEFORE),
            json!({"timestamp": at("10:00:03"), "type": "response_item", "payload": {"type": "function_call", "name": "spawn_agent",
                "call_id": "call_spawn", "arguments": json!({"task_name": "worker", "message": "Work.", "model": "fixture-model"}).to_string()}}),
            json!({"timestamp": at("10:00:04"), "type": "response_item", "payload": {"type": "function_call_output",
                "call_id": "call_spawn", "output": json!({"task_name": "/root/worker"}).to_string()}}),
            prompt("10:00:05", AFTER),
            json!({"timestamp": at("10:00:06"), "type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": usage, "last_token_usage": usage, "model_context_window": 1_000_000}}}),
            json!({"timestamp": at("10:00:07"), "type": "event_msg", "payload": {"type": "task_complete"}}),
        ],
    );
}

/// A Claude recording whose only lineage evidence, an `Agent` call and its
/// result naming the agent it launched, sits between `BEFORE` and `AFTER`
/// bytes of prompts. The agent's own transcript is not in the store.
fn claude_spawner(repo: &Repo) {
    let path = repo
        .home
        .join(".claude/projects/-fixture-project")
        .join(format!("{CLAUDE}.jsonl"));
    let record = |kind: &str, uuid: &str, parent: Option<&str>, clock: &str, message: Value| {
        json!({"type": kind, "sessionId": CLAUDE, "uuid": uuid, "parentUuid": parent, "timestamp": at(clock),
            "cwd": "/fixture/project", "entrypoint": "cli", "isSidechain": false, "message": message})
    };
    let usage = json!({"input_tokens": 3, "output_tokens": 20, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0});
    let mut launched = record(
        "user",
        "launch",
        Some("call"),
        "10:00:04",
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_spawn", "content": "launched"}]}),
    );
    launched["toolUseResult"] = json!({"isAsync": true, "status": "async_launched", "agentId": AGENT, "description": "Work"});
    let mut call = record(
        "assistant",
        "call",
        Some("before"),
        "10:00:03",
        json!({"id": "msg_call", "role": "assistant", "model": "fixture-model", "usage": usage,
            "content": [{"type": "tool_use", "id": "toolu_spawn", "name": "Agent",
                "input": {"description": "Work", "prompt": "Work.", "subagent_type": "general-purpose"}}]}),
    );
    call["requestId"] = json!("req_call");
    let mut answer = record(
        "assistant",
        "answer",
        Some("after"),
        "10:00:06",
        json!({"id": "msg_answer", "role": "assistant", "model": "fixture-model", "usage": usage,
            "content": [{"type": "text", "text": "Done."}]}),
    );
    answer["requestId"] = json!("req_answer");
    write_lines(
        &path,
        &[
            record(
                "user",
                "before",
                None,
                "10:00:02",
                json!({"role": "user", "content": "x".repeat(BEFORE)}),
            ),
            call,
            launched,
            record(
                "user",
                "after",
                Some("launch"),
                "10:00:05",
                json!({"role": "user", "content": "x".repeat(AFTER)}),
            ),
            answer,
        ],
    );
}

/// Claim: a Codex recording larger than the bounded read window whose only
/// spawned-agent evidence lies past the read's head and before its tail
/// still names that unresolved child, so identity is not established and
/// the union says why, never `lineage read bounded`. Rejects a lineage
/// reader that keeps the bounded windows and drops their warning, which
/// would miss the child and call identity complete.
#[test]
fn a_codex_child_marker_mid_recording_keeps_identity_unestablished() {
    let repo = Repo::new();
    codex_spawner(&repo);
    let change = executed_change(&repo, "codex", &[("codex", CODEX)]);
    let view = report(&repo, &[&change]);
    let identity = &view["union"]["identity"];
    assert_eq!(identity["status"], "incomplete", "{identity:#}");
    assert_eq!(
        identity["unknown"][format!("codex:{CODEX}")],
        json!(["lineage relative unresolved"]),
        "{identity:#}"
    );
}

/// Claim: a Claude recording larger than the bounded read window whose
/// only `Agent` launch lies past the read's head and before its tail still
/// lists that native child under its family, its recording missing, and
/// the lineage is not reported bounded. Rejects a lineage reader that
/// keeps the bounded windows and drops their warning, which would list no
/// child.
#[test]
fn a_claude_child_marker_mid_recording_is_listed_in_its_family() {
    let repo = Repo::new();
    claude_spawner(&repo);
    let change = executed_change(&repo, "claude", &[("claude", CLAUDE)]);
    let view = report(&repo, &[&change]);
    let row = session(&view, &format!("claude:{CLAUDE}"));
    let children = &row["family"]["children"];
    assert_eq!(children.as_array().map(Vec::len), Some(1), "{row:#}");
    assert_eq!(children[0]["agent"], AGENT, "{row:#}");
    assert_eq!(children[0]["recording"], "missing", "{row:#}");
    assert!(
        !row["measurement"]["lineage"]
            .to_string()
            .contains("lineage read bounded"),
        "{row:#}"
    );
}
