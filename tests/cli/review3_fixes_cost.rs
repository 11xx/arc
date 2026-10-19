//! Cost view inputs and figures a review found mishandled: a list price
//! under a table with terms priced by the request, a retained
//! participant estimate read at another idle gap, and a non-ASCII `--idle`.
//! Recordings are written into the fixture's own home, named as each
//! harness names them, so tapes reads them as it reads a harness store.

use super::common::*;
use serde_json::{json, Value};

const LIST: &str = "list-price-estimate";
const CODEX: &str = "019a0000-0000-7000-8000-0000000000c1";
const REVIEWER: &str = "019a0000-0000-7000-8000-0000000000e1";

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

fn work<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["work"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == key)
        .unwrap_or_else(|| panic!("no work item {key}: {report:#}"))
}

/// The combined figure of one quantity kind, or of one disjoint category.
fn quantity(totals: &Value, matches: impl Fn(&Value) -> bool) -> &Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals:#}"))
        .iter()
        .find(|quantity| quantity.get("partition").is_none() && matches(quantity))
        .unwrap_or_else(|| panic!("no such quantity: {totals:#}"))
}

fn list_price(totals: &Value) -> &Value {
    quantity(totals, |quantity| quantity["kind"] == LIST)
}

fn uncached(totals: &Value) -> Option<u64> {
    let figure = quantity(totals, |quantity| {
        quantity["categories"] == "disjoint" && quantity["category"] == "uncached_input"
    });
    figure["amount"]
        .as_u64()
        .or_else(|| figure["known_subtotal"].as_u64())
}

fn close(value: Option<f64>, expected: f64, context: &Value) {
    let value = value.unwrap_or_else(|| panic!("no amount: {context:#}"));
    assert!(
        (value - expected).abs() < 1e-12,
        "{value} != {expected}: {context:#}"
    );
}

// --- A version 2 table's long-context term --------------------------------

/// A table whose one row charges five times the uncached input and output
/// rates on a request whose prompt passes 100K tokens, under API billing.
const SURCHARGED: &str = r#"schema = "tapes-price-table/2"
currency = "USD"

[[models]]
id = "fixture-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 0.10, cache_read = 0.01, cache_write_5m = 0.125, output = 0.50 }

[models.long_context]
above_prompt_tokens = 100000
prompt_tokens = ["uncached_input", "cache_read", "cache_write"]
multipliers = { uncached_input = 5.0, output = 5.0 }
"#;

/// The request below at base rates, and above the threshold.
const BASE: f64 = 200_000.0 * 0.10 / 1e6 + 1_000.0 * 0.50 / 1e6;
const SURCHARGE: f64 = 200_000.0 * 0.50 / 1e6 + 1_000.0 * 2.50 / 1e6;

/// A Codex rollout of requests that each record their uncached input and
/// 1K output at their own instant, under `provider` and with
/// `rate_limits`, as Codex records them: a provider other than `openai`
/// bills through an API key, and a quota window reporting a used percent is
/// a ChatGPT plan's.
fn codex_rollout(
    repo: &Repo,
    provider: &str,
    rate_limits: Value,
    requests: &[(&str, u64)],
) -> PathBuf {
    let path = repo
        .home
        .join(".codex/sessions/2026/01/01")
        .join(format!("rollout-2026-01-01T10-00-00-{CODEX}.jsonl"));
    let usage = |input: u64, output: u64| {
        json!({"input_tokens": input, "cached_input_tokens": 0, "cache_write_input_tokens": 0,
            "output_tokens": output, "reasoning_output_tokens": 0, "total_tokens": input + output})
    };
    let mut lines = vec![
        json!({"timestamp": at("10:00:00"), "type": "session_meta", "payload": {"id": CODEX, "session_id": CODEX,
            "timestamp": at("10:00:00"), "cwd": "/fixture/project", "model_provider": provider}}),
        json!({"timestamp": at("10:00:01"), "type": "turn_context", "payload": {"cwd": "/fixture/project", "model": "fixture-model", "effort": "high"}}),
    ];
    let (mut input, mut output) = (0, 0);
    for (clock, uncached) in requests {
        input += uncached;
        output += 1_000;
        lines.extend([
            json!({"timestamp": at(clock), "type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Work."}]}}),
            json!({"timestamp": at(clock), "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Done."}]}}),
            json!({"timestamp": at(clock), "type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": usage(input, output), "last_token_usage": usage(*uncached, 1_000),
                "model_context_window": 1_000_000}, "rate_limits": rate_limits}}),
        ]);
    }
    lines.push(json!({"timestamp": at("10:30:00"), "type": "event_msg", "payload": {"type": "task_complete"}}));
    write_lines(&path, &lines);
    path
}

/// One request of 200K uncached input at 10:00:04.
fn one_request(repo: &Repo, provider: &str, rate_limits: Value) -> PathBuf {
    codex_rollout(repo, provider, rate_limits, &[("10:00:04", 200_000)])
}

/// Dispatch a run for `change` at `start`, bind it to the Codex session
/// and end it at `end`.
fn executed(repo: &Repo, change: &str, start: &str, end: &str) {
    let dispatch = event_id(&stdout(anonymous(repo, &repo.root).args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        "--change",
        change,
    ])));
    retime(repo, &dispatch, &at(start));
    stdout(repo.arc(&repo.root).args([
        "run",
        "bind",
        &dispatch,
        "--executor-harness",
        "codex",
        "--executor-session",
        CODEX,
    ]));
    let ended = event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "end",
        &dispatch,
        "--outcome",
        "completed",
    ])));
    retime(repo, &ended, &at(end));
}

fn surcharged_table(repo: &Repo) -> String {
    let path = repo.home.join("prices.toml");
    fs::write(&path, SURCHARGED).unwrap();
    path.to_string_lossy().to_string()
}

/// Claim: a fresh read prices a version 2 table's long-context term request
/// by request under the billing the recording establishes, so an ended
/// API-billed executor whose 200K-token prompt passes the threshold is
/// complete at the surcharged rates, and a subscription session at base
/// rates. Rejects pricing every token at the row's flat rates and calling
/// the underestimate complete.
#[test]
fn a_fresh_read_prices_a_long_context_term_by_the_request_and_its_billing() {
    let repo = Repo::new();
    one_request(&repo, "fixture", Value::Null);
    executed(&repo, "surcharged", "09:59:00", "10:31:00");
    let prices = surcharged_table(&repo);

    let view = report(&repo, &["surcharged", "--prices", &prices]);
    let row = session(&view, &format!("codex:{CODEX}"));
    let list = list_price(&row["totals"]);
    assert_eq!(list["status"], "complete", "{row:#}");
    close(list["amount"].as_f64(), SURCHARGE, row);
    assert_eq!(list["schema"], "tapes-price-table/2", "{list:#}");
    let union = list_price(&view["union"]);
    assert_eq!(union["status"], "complete", "{view:#}");
    close(union["amount"].as_f64(), SURCHARGE, &view);

    // A ChatGPT plan's quota bills by subscription, which the term does not
    // reach.
    let repo = Repo::new();
    let quota = json!({"limit_id": "codex", "plan_type": "plus",
        "primary": {"used_percent": 10.0, "window_minutes": 300, "resets_at": 1_791_000_000}, "secondary": null});
    one_request(&repo, "openai", quota);
    executed(&repo, "subscribed", "09:59:00", "10:31:00");
    let prices = surcharged_table(&repo);
    let view = report(&repo, &["subscribed", "--prices", &prices]);
    let list = list_price(&view["union"]);
    assert_eq!(list["status"], "complete", "{view:#}");
    close(list["amount"].as_f64(), BASE, &view);
}

/// Claim: retained facts hold no request one by one, so a term priced by
/// the request leaves their list price a known subtotal at the least it can
/// be, naming tapes' reason, never a complete amount. Rejects the flat
/// estimate presented as complete once the recording is gone.
#[test]
fn retained_facts_leave_a_term_priced_by_the_request_a_known_subtotal() {
    let repo = Repo::new();
    let path = one_request(&repo, "fixture", Value::Null);
    executed(&repo, "surcharged", "09:59:00", "10:31:00");
    let prices = surcharged_table(&repo);
    let output = stdout(repo.arc(&repo.root).args(["cost", "retain", "surcharged"]));
    assert!(
        output.contains(&format!("retained: codex:{CODEX}")),
        "{output}"
    );
    fs::remove_file(path).unwrap();

    let view = report(&repo, &["surcharged", "--prices", &prices]);
    let row = session(&view, &format!("codex:{CODEX}"));
    assert_eq!(row["measurement"]["source"], "retained", "{row:#}");
    let list = list_price(&row["totals"]);
    assert_ne!(list["status"], "complete", "{row:#}");
    assert!(list.get("amount").is_none(), "{row:#}");
    close(list["known_subtotal"].as_f64(), BASE, row);
    assert!(
        list["reasons"].to_string().contains(
            "a price term set by the request may apply to requests the figure does not hold one by one"
        ),
        "{list:#}"
    );
    let union = list_price(&view["union"]);
    assert_ne!(union["status"], "complete", "{view:#}");
}

fn share<'a>(row: &'a Value, change: &str) -> &'a Value {
    &row["split"]["shares"]
        .as_array()
        .unwrap_or_else(|| panic!("no shares: {row:#}"))
        .iter()
        .find(|share| share["work"] == format!("change:{change}"))
        .unwrap_or_else(|| panic!("no share of {change}: {row:#}"))["totals"]
}

/// Claim: a divided executor's share of each item is priced over the
/// requests its segments hold, so the run whose request passes the
/// threshold carries the surcharge and the other run base rates, each
/// complete. Rejects pricing a share's tokens flat, and pricing a share as
/// though the session's largest request were its own.
#[test]
fn a_divided_session_prices_each_share_by_its_own_requests() {
    let repo = Repo::new();
    codex_rollout(
        &repo,
        "fixture",
        Value::Null,
        &[("10:05:00", 200_000), ("10:20:00", 50_000)],
    );
    executed(&repo, "large", "09:59:00", "10:10:00");
    executed(&repo, "small", "10:15:00", "10:31:00");
    let prices = surcharged_table(&repo);

    let view = report(&repo, &["large", "small", "--prices", &prices]);
    let row = session(&view, &format!("codex:{CODEX}"));
    let small = 50_000.0 * 0.10 / 1e6 + 1_000.0 * 0.50 / 1e6;
    for (change, expected) in [("large", SURCHARGE), ("small", small)] {
        let list = list_price(share(row, change));
        assert_eq!(list["status"], "complete", "{change}: {list:#}");
        close(list["amount"].as_f64(), expected, row);
    }
    let union = list_price(&view["union"]);
    assert_eq!(union["status"], "complete", "{view:#}");
    close(union["amount"].as_f64(), SURCHARGE + small, &view);
}

/// A table whose two rows charge five times above a 100K prompt, for any
/// billing, so a recording that names none is priced by its requests.
const ANY_BILLING: &str = r#"schema = "tapes-price-table/2"
currency = "USD"

[[models]]
id = "fixture-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 0.10, cache_read = 0.01, cache_write_5m = 0.125, output = 0.50 }

[models.long_context]
above_prompt_tokens = 100000
prompt_tokens = ["uncached_input", "cache_read", "cache_write"]
billing = ["api", "subscription"]
multipliers = { uncached_input = 5.0, output = 5.0 }

[[models]]
id = "fixture-other"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 0.10, cache_read = 0.01, cache_write_5m = 0.125, output = 0.50 }

[models.long_context]
above_prompt_tokens = 100000
prompt_tokens = ["uncached_input", "cache_read", "cache_write"]
billing = ["api", "subscription"]
multipliers = { uncached_input = 5.0, output = 5.0 }
"#;

/// Claim: a figure the recording splits between two models prices each
/// model's share over that model's own requests, so the one whose request
/// passes the threshold pays the surcharge and the other base rates, and
/// the sum is complete. Rejects pricing each model's share flat.
#[test]
fn each_model_share_is_priced_by_its_own_requests() {
    let repo = Repo::new();
    let id = "019a0000-0000-7000-8000-0000000000d1";
    let path = repo
        .home
        .join(".claude/projects/-fixture-project")
        .join(format!("{id}.jsonl"));
    let mut lines = Vec::new();
    let mut parent: Option<String> = None;
    for (n, (clock, model, input)) in [
        ("10:05:00", "fixture-model", 200_000u64),
        ("10:10:00", "fixture-other", 50_000),
    ]
    .into_iter()
    .enumerate()
    {
        let prompt = format!("prompt-{n}");
        lines.push(json!({"type": "user", "sessionId": id, "uuid": prompt, "parentUuid": parent, "timestamp": at(clock),
            "cwd": "/fixture/project", "message": {"role": "user", "content": "Work."}}));
        let reply = format!("assistant-{n}");
        lines.push(json!({"type": "assistant", "sessionId": id, "uuid": reply, "parentUuid": prompt,
            "requestId": format!("request-{n}"), "timestamp": at(clock), "cwd": "/fixture/project",
            "message": {"id": format!("message-{n}"), "role": "assistant", "model": model,
                "content": [{"type": "text", "text": "Done."}],
                "usage": {"input_tokens": input, "output_tokens": 1_000, "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0}}}}));
        parent = Some(reply);
    }
    write_lines(&path, &lines);
    let dispatch = event_id(&stdout(anonymous(&repo, &repo.root).args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        "--change",
        "mixed",
    ])));
    retime(&repo, &dispatch, &at("09:59:00"));
    stdout(repo.arc(&repo.root).args([
        "run",
        "bind",
        &dispatch,
        "--executor-harness",
        "claude",
        "--executor-session",
        id,
    ]));
    stdout(
        repo.arc(&repo.root)
            .args(["run", "end", &dispatch, "--outcome", "completed"]),
    );
    let prices = repo.home.join("prices.toml");
    fs::write(&prices, ANY_BILLING).unwrap();

    let view = report(&repo, &["mixed", "--prices", &prices.to_string_lossy()]);
    let row = session(&view, &format!("claude:{id}"));
    let list = list_price(&row["totals"]);
    let small = 50_000.0 * 0.10 / 1e6 + 1_000.0 * 0.50 / 1e6;
    assert_eq!(list["status"], "complete", "{row:#}");
    close(list["amount"].as_f64(), SURCHARGE + small, row);
}

// --- A retained estimate read at another idle gap ---------------------------

/// A Pi recording whose assistant requests each record `input` uncached
/// tokens at their own instant.
fn pi_session(repo: &Repo, id: &str, requests: &[(&str, u64)]) -> PathBuf {
    let path = repo
        .home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T07-00-00-000Z_{id}.jsonl"));
    let mut lines = vec![
        json!({"type": "session", "version": 3, "id": id, "timestamp": at("07:00:00"), "cwd": "/fixture/project"}),
    ];
    let mut parent: Option<String> = None;
    for (n, (clock, input)) in requests.iter().enumerate() {
        let user = format!("user-{n}");
        lines.push(json!({"type": "message", "id": user, "parentId": parent, "timestamp": at(clock),
            "message": {"role": "user", "timestamp": 1767250800000u64, "content": [{"type": "text", "text": "Go on."}]}}));
        let entry = format!("assistant-{n}");
        lines.push(json!({"type": "message", "id": entry, "parentId": user, "timestamp": at(clock),
            "message": {"role": "assistant", "timestamp": 1767250800000u64, "provider": "fixture", "model": "fixture-model",
                "usage": {"input": input, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": input,
                    "cost": {"total": *input as f64 / 1000.0}},
                "content": [{"type": "text", "text": "Done."}]}}));
        parent = Some(entry);
    }
    write_lines(&path, &lines);
    path
}

/// A change with one snapshotted patchset, opened naming no session, with
/// its events moved before the recording begins.
fn change(repo: &Repo, slug: &str) -> String {
    let change = opened_change_id(&stdout(anonymous(repo, &repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        "x\n",
        &format!("feat: {slug}"),
    );
    stdout(anonymous(repo, &worktree).args(["snapshot", slug]));
    for entry in fs::read_dir(event_dir(repo, &change)).unwrap() {
        let path = entry.unwrap().path();
        let id = path.file_stem().unwrap().to_str().unwrap().to_string();
        retime(repo, &id, &at("06:00:00"));
    }
    change
}

/// The reviewer session records a verdict on `change` at `clock`.
fn verdict(repo: &Repo, change: &str, clock: &str) {
    let mut command = repo.arc(&repo.root);
    command
        .env("ARC_HARNESS", "pi")
        .env("ARC_SESSION", REVIEWER);
    let out = stdout(command.args(["review", change, "--verdict", "comment-only"]));
    retime(repo, &event_id(&out), &at(clock));
}

/// Each item's figure with estimates, in uncached input tokens.
fn estimated(view: &Value, a: &str, b: &str) -> (Option<u64>, Option<u64>) {
    let of = |change: &str| {
        let item = work(view, &format!("change:{change}"));
        item.get("estimated")
            .and_then(|estimated| uncached(&estimated["direct"]))
    };
    (of(a), of(b))
}

/// Claim: an estimate answers only the idle gap it was read at. Retained
/// facts estimated at 45m, shown at 15m once the recording is gone, carry
/// no estimate and say why, rather than the 45m split labelled 15m; shown
/// at 45m they keep it. Rejects reusing a retained estimate under another
/// `--idle` and relabelling it.
#[test]
fn a_retained_estimate_answers_only_its_own_idle_gap() {
    let repo = Repo::new();
    let path = pi_session(
        &repo,
        REVIEWER,
        &[("10:00:00", 10), ("10:10:00", 3), ("10:40:00", 20)],
    );
    let (a, b) = (change(&repo, "first"), change(&repo, "second"));
    verdict(&repo, &a, "10:05:00");
    verdict(&repo, &b, "10:45:00");

    // Fresh: one stretch at 45m, two at 15m.
    let view = report(&repo, &[&a, &b]);
    assert_eq!(estimated(&view, &a, &b), (Some(10), Some(23)), "{view:#}");
    let view = report(&repo, &[&a, &b, "--idle", "15m"]);
    assert_eq!(estimated(&view, &a, &b), (Some(13), Some(20)), "{view:#}");

    let output = stdout(repo.arc(&repo.root).args(["cost", "retain", &a]));
    assert!(
        output.contains(&format!("retained: pi:{REVIEWER}")),
        "{output}"
    );
    fs::remove_file(path).unwrap();

    let view = report(&repo, &[&a, &b, "--idle", "15m"]);
    let row = session(&view, &format!("pi:{REVIEWER}"));
    assert_eq!(row["measurement"]["source"], "retained", "{row:#}");
    assert_eq!(estimated(&view, &a, &b), (None, None), "{view:#}");
    let estimate = &row["split"]["estimate"];
    assert_eq!(estimate["idle"], "15m", "{estimate:#}");
    assert_eq!(estimate["status"], "withheld", "{estimate:#}");
    assert_eq!(
        estimate["reason"], "retained facts were estimated at another idle gap",
        "{estimate:#}"
    );
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", &a, &b, "--idle", "15m"]),
    );
    assert!(!text.contains("total with estimates"), "{text}");

    let view = report(&repo, &[&a, &b, "--idle", "45m"]);
    assert_eq!(estimated(&view, &a, &b), (Some(10), Some(23)), "{view:#}");
    let estimate = &session(&view, &format!("pi:{REVIEWER}"))["split"]["estimate"];
    assert_eq!(estimate["idle"], "45m", "{estimate:#}");
}

// --- A non-ASCII idle gap ---------------------------------------------------

/// Claim: `--idle` refuses any value that is not a whole number with an
/// `s`, `m` or `h` unit as a usage error naming it, whatever its characters.
/// Rejects splitting a multi-byte character, which panics.
#[test]
fn a_non_ascii_idle_gap_is_a_usage_error() {
    let repo = Repo::new();
    for value in ["é", "5é", "é5m", "", "0m", "5"] {
        let output = repo
            .arc(&repo.root)
            .args(["cost", "show", "anything", "--idle", value])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{value:?}: {stderr}");
        assert!(
            stderr.contains("is not a duration such as 90s, 45m or 2h"),
            "{value:?}: {stderr}"
        );
        assert!(!stderr.contains("panicked"), "{value:?}: {stderr}");
    }
}
