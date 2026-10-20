//! A figure priced share by share, or whole by its one model, is priced as
//! tapes prices the same recording: each share by its recorded cache-write
//! TTLs, and each share or whole figure with its own completeness. Recordings are
//! written into the fixture's own home, named as each harness names them,
//! so tapes reads them as it reads a harness store.

use super::common::*;
use serde_json::{json, Value};
use std::collections::BTreeSet;

const LIST: &str = "list-price-estimate";
const CLAUDE: &str = "019a0000-0000-7000-8000-0000000000e1";
const CODEX: &str = "019a0000-0000-7000-8000-0000000000e2";
const UNATTRIBUTED: &str = "some requests could not be attributed to a model";
const MISSING_COUNTER: &str = "some counted events did not record this category";
const TTL_UNRECORDED: &str = "some cache writes recorded no TTL";

/// A table pricing two models alike, a 1-hour cache write at twice the
/// 5-minute rate.
const TTL_TABLE: &str = r#"schema = "tapes-price-table/1"
currency = "USD"

[[models]]
id = "fixture-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.0, cache_write_1h = 2.0, output = 4.0 }

[[models]]
id = "fixture-other"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.0, cache_write_1h = 2.0, output = 4.0 }
"#;

/// A table that prices the cheap model at one rate all day.
const FLAT: &str = r#"schema = "tapes-price-table/1"
currency = "USD"

[[models]]
id = "cheap-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.25, output = 4.0 }
"#;

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

/// A change with one run bound to the `harness` session, dispatched before
/// its recording begins and ended after it.
fn executed_change(repo: &Repo, harness: &str, session: &str) -> String {
    run_change(repo, harness, harness, session, "09:00:00", "23:59:00")
}

/// A change `slug` with one run bound to the `harness` session, dispatched
/// at `from` and ended at `to`.
fn run_change(
    repo: &Repo,
    slug: &str,
    harness: &str,
    session: &str,
    from: &str,
    to: &str,
) -> String {
    let change = opened_change_id(&stdout(anonymous(repo).args([
        "begin",
        slug,
        "--no-worktree",
        "--target",
        "master",
    ])));
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
    retime(repo, &dispatch, &at(from));
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
    retime(repo, &ended, &at(to));
    change
}

fn prices(repo: &Repo, table: &str) -> String {
    let path = repo.home.join("prices.toml");
    fs::write(&path, table).unwrap();
    path.to_string_lossy().to_string()
}

/// The `harness` session's row and the union in the cost view of `change`
/// under `table`.
fn view(repo: &Repo, change: &str, table: &str, harness: &str, session: &str) -> (Value, Value) {
    let prices = prices(repo, table);
    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--json", "--prices", &prices, change]),
    );
    let row = view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == format!("{harness}:{session}"))
        .unwrap_or_else(|| panic!("no session: {view:#}"))
        .clone();
    (row, view["union"].clone())
}

fn list_price(totals: &Value) -> Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals:#}"))
        .iter()
        .find(|quantity| quantity.get("partition").is_none() && quantity["kind"] == LIST)
        .unwrap_or_else(|| panic!("no list price: {totals:#}"))
        .clone()
}

/// The usage answer `tapes usage --full --prices` gives `session` under
/// `table`, through the same tapes reader and pricing arc pins.
fn tapes_figure(
    backend: Box<dyn agent_tapes_core::backend::Backend>,
    session: &str,
    table: &str,
) -> Value {
    let backends = vec![backend];
    let mut usage = agent_tapes_core::usage_full_with_backends(
        &backends,
        agent_tapes_core::Selection::Id(session),
    )
    .unwrap();
    let table = agent_tapes_core::pricing::PriceTable::parse(table).unwrap();
    agent_tapes_core::pricing::price_view(&mut usage, &table);
    serde_json::to_value(&usage).unwrap()
}

/// The reasons arc names for the reasons a tapes estimate carries.
fn arc_reasons(estimate: &Value) -> BTreeSet<String> {
    estimate["measurement_completeness"]
        .as_object()
        .into_iter()
        .flatten()
        .flat_map(|(_, reasons)| reasons.as_array().unwrap())
        .map(|reason| {
            match reason.as_str().unwrap() {
                "incomplete-attribution" => UNATTRIBUTED,
                "missing-counter" => MISSING_COUNTER,
                "ttl-unrecorded" => TTL_UNRECORDED,
                "undetermined" => "the recorded counters do not determine this category",
                other => panic!("no arc reason for {other}"),
            }
            .to_string()
        })
        .collect()
}

/// Assert that arc's list price of the `key` session is tapes' estimate:
/// its amount, or its least and most, and the reasons it names.
fn same_price(list: &Value, estimate: &Value, key: &str) {
    let scope = format!("{key}: ");
    let reasons: BTreeSet<String> = list["reasons"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|reason| {
            let reason = reason.as_str().unwrap();
            reason.strip_prefix(&scope).unwrap_or(reason).to_string()
        })
        .collect();
    assert_eq!(reasons, arc_reasons(estimate), "{list:#}\n{estimate:#}");
    let close = |value: &Value, expected: &Value| {
        let (value, expected) = (value.as_f64(), expected.as_f64());
        assert!(
            matches!((value, expected), (Some(value), Some(expected)) if (value - expected).abs() < 1e-9),
            "{value:?} != {expected:?}: {list:#}\n{estimate:#}"
        );
    };
    match estimate["amount"].as_f64() {
        Some(_) if reasons.is_empty() => {
            assert_eq!(list["status"], "complete", "{list:#}");
            close(&list["amount"], &estimate["amount"]);
        }
        Some(_) => close(&list["known_subtotal"], &estimate["amount"]),
        None => {
            close(&list["known_subtotal"], &estimate["at_least"]);
            close(&list["bound"]["at_most"], &estimate["at_most"]);
        }
    }
}

fn claude_backend(repo: &Repo) -> Box<dyn agent_tapes_core::backend::Backend> {
    Box::new(agent_tapes_core::backend::claude::ClaudeBackend::new(
        repo.home.join(".claude/projects"),
    ))
}

/// The records of a Claude recording of one request per `(clock, model,
/// usage)`, each a prompt and its assistant answer.
fn claude_lines(requests: &[(&str, &str, Value)]) -> Vec<Value> {
    let mut lines = Vec::new();
    let mut parent: Option<String> = None;
    for (n, (clock, model, usage)) in requests.iter().enumerate() {
        let prompt = format!("prompt-{n}");
        lines.push(
            json!({"type": "user", "sessionId": CLAUDE, "uuid": prompt, "parentUuid": parent,
            "timestamp": at(clock), "cwd": "/fixture/project",
            "message": {"role": "user", "content": "Work."}}),
        );
        let reply = format!("assistant-{n}");
        lines.push(
            json!({"type": "assistant", "sessionId": CLAUDE, "uuid": reply, "parentUuid": prompt,
            "requestId": format!("request-{n}"), "timestamp": at(clock), "cwd": "/fixture/project",
            "message": {"id": format!("message-{n}"), "role": "assistant", "model": model,
                "content": [{"type": "text", "text": "Done."}], "usage": usage}}),
        );
        parent = Some(reply);
    }
    lines
}

/// Write `lines` as the recording in the fixture project's store.
fn claude_recording(repo: &Repo, lines: &[Value]) {
    let path = repo
        .home
        .join(".claude/projects/-fixture-project")
        .join(format!("{CLAUDE}.jsonl"));
    write_lines(&path, lines);
}

/// Usage of a request that wrote `writes` cache tokens kept for an hour.
fn hour_writes(writes: u64) -> Value {
    json!({"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": writes,
        "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": writes}})
}

/// Claim: a figure two models split is priced share by share by each
/// share's recorded cache-write TTLs, so two requests that each wrote 1M
/// tokens kept for an hour are complete at the 1-hour rate, 4.0, as tapes
/// prices them. Rejects a share whose writes lost their TTLs, priced
/// between the 5-minute and the 1-hour rate as a known subtotal of 2.0
/// bounded at 4.0 with the no-TTL reason.
#[test]
fn a_model_share_is_priced_by_its_recorded_cache_write_ttls() {
    let repo = Repo::new();
    claude_recording(
        &repo,
        &claude_lines(&[
            ("10:05:00", "fixture-model", hour_writes(1_000_000)),
            ("10:10:00", "fixture-other", hour_writes(1_000_000)),
        ]),
    );
    let tapes = tapes_figure(claude_backend(&repo), CLAUDE, TTL_TABLE);
    let estimate = &tapes["list_price_estimate"];
    assert_eq!(estimate["amount"], 4.0, "{tapes:#}");
    assert_eq!(
        tapes["by_model"].as_array().map(Vec::len),
        Some(2),
        "{tapes:#}"
    );
    let change = executed_change(&repo, "claude", CLAUDE);
    let (row, union) = view(&repo, &change, TTL_TABLE, "claude", CLAUDE);
    for list in [list_price(&row["totals"]), list_price(&union)] {
        same_price(&list, estimate, &format!("claude:{CLAUDE}"));
    }
}

/// Claim: a divided session's figure added up from its segments keeps each
/// segment's shares' recorded cache-write TTLs: two runs, each holding a
/// request of each model that wrote 1M tokens kept for an hour, add up to
/// tapes' complete 8.0 for the session. Rejects segment shares whose writes
/// lost their TTLs, a known subtotal of 4.0 bounded at 8.0.
#[test]
fn segment_shares_added_up_keep_their_cache_write_ttls() {
    let repo = Repo::new();
    claude_recording(
        &repo,
        &claude_lines(&[
            ("10:05:00", "fixture-model", hour_writes(1_000_000)),
            ("10:06:00", "fixture-other", hour_writes(1_000_000)),
            ("10:15:00", "fixture-model", hour_writes(1_000_000)),
            ("10:16:00", "fixture-other", hour_writes(1_000_000)),
        ]),
    );
    let tapes = tapes_figure(claude_backend(&repo), CLAUDE, TTL_TABLE);
    let estimate = &tapes["list_price_estimate"];
    assert_eq!(estimate["amount"], 8.0, "{tapes:#}");
    let morning = run_change(&repo, "morning", "claude", CLAUDE, "10:00:00", "10:10:00");
    let later = run_change(&repo, "later", "claude", CLAUDE, "10:10:00", "10:30:00");
    let prices = prices(&repo, TTL_TABLE);
    let view = json_stdout(repo.arc(&repo.root).args([
        "cost", "show", "--json", "--prices", &prices, &morning, &later,
    ]));
    same_price(
        &list_price(&view["union"]),
        estimate,
        &format!("claude:{CLAUDE}"),
    );
}

/// A Codex rollout of one request per `(effort, usage)` at 10:00, each
/// under a turn context naming the cheap model at that effort, or, with no
/// effort, before any turn context names a model.
fn codex_rollout(repo: &Repo, requests: &[(Option<&str>, Value)]) {
    let path = repo
        .home
        .join(".codex/sessions/2026/01/01")
        .join(format!("rollout-2026-01-01T10-00-00-{CODEX}.jsonl"));
    let mut lines = vec![
        json!({"timestamp": at("10:00:00"), "type": "session_meta", "payload": {"id": CODEX, "session_id": CODEX,
            "timestamp": at("10:00:00"), "cwd": "/fixture/project", "model_provider": "fixture"}}),
    ];
    let mut total = json!({});
    for (n, (effort, usage)) in requests.iter().enumerate() {
        let clock = format!("10:00:0{}", n * 2 + 1);
        if let Some(effort) = effort {
            lines.push(
                json!({"timestamp": at(&clock), "type": "turn_context", "payload": {
                "cwd": "/fixture/project", "model": "cheap-model", "effort": effort}}),
            );
        }
        for (name, amount) in usage.as_object().unwrap() {
            total[name] = json!(total[name].as_u64().unwrap_or(0) + amount.as_u64().unwrap());
        }
        lines.extend([
            json!({"timestamp": at(&clock), "type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Work."}]}}),
            json!({"timestamp": at(&clock), "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Done."}]}}),
            json!({"timestamp": at(&clock), "type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": total, "last_token_usage": usage,
                "model_context_window": 2_000_000}, "rate_limits": null}}),
        ]);
    }
    lines.push(json!({"timestamp": at("10:30:00"), "type": "event_msg", "payload": {"type": "task_complete"}}));
    write_lines(&path, &lines);
}

/// Codex usage of `input` tokens, `cached` of them read from the cache.
fn codex_usage(input: u64, cached: u64) -> Value {
    json!({"input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0,
        "output_tokens": 0, "reasoning_output_tokens": 0, "total_tokens": input})
}

fn codex_figure(repo: &Repo) -> Value {
    tapes_figure(
        Box::new(agent_tapes_core::backend::codex::CodexBackend::new(
            repo.home.join(".codex/sessions"),
        )),
        CODEX,
        FLAT,
    )
}

/// Claim: a share priced share by share keeps its own completeness, so a
/// Codex share whose uncached input tapes leaves undetermined under
/// uncertain attribution names only the reasons tapes names for it, the
/// unattributed-request reason, at tapes' known subtotal. Rejects a share
/// restated without its completeness, whose category with no amount is
/// named as one some counted events did not record.
#[test]
fn a_model_share_keeps_its_own_completeness() {
    let repo = Repo::new();
    codex_rollout(
        &repo,
        &[
            (None, codex_usage(1_000_000, 0)),
            (Some("high"), codex_usage(1_000_000, 0)),
            (Some("max"), codex_usage(1_000_000, 0)),
        ],
    );
    let tapes = codex_figure(&repo);
    assert_eq!(
        tapes["session"]["model_observation"]["attribution_uncertain"], true,
        "{tapes:#}"
    );
    let estimate = &tapes["list_price_estimate"];
    assert!(estimate["amount"].is_number(), "{tapes:#}");
    let change = executed_change(&repo, "codex", CODEX);
    let (row, union) = view(&repo, &change, FLAT, "codex", CODEX);
    for list in [list_price(&row["totals"]), list_price(&union)] {
        same_price(&list, estimate, &format!("codex:{CODEX}"));
    }
}

/// Claim: a figure one model served whole names, for a category it holds
/// no amount for, only the reasons tapes names for it: a Codex request
/// that read more tokens from the cache than it counted as input leaves
/// uncached input undetermined, and the list price names that reason alone
/// at tapes' known subtotal. Rejects pricing the figure without its own
/// completeness, which also names the category one some counted events
/// did not record.
#[test]
fn a_figure_priced_whole_keeps_its_own_completeness() {
    let repo = Repo::new();
    codex_rollout(&repo, &[(Some("high"), codex_usage(1_000_000, 2_000_000))]);
    let tapes = codex_figure(&repo);
    let estimate = &tapes["list_price_estimate"];
    assert!(
        estimate["measurement_completeness"]["uncached_input"]
            .to_string()
            .contains("undetermined"),
        "{tapes:#}"
    );
    let change = executed_change(&repo, "codex", CODEX);
    let (row, union) = view(&repo, &change, FLAT, "codex", CODEX);
    for list in [list_price(&row["totals"]), list_price(&union)] {
        same_price(&list, estimate, &format!("codex:{CODEX}"));
    }
}
