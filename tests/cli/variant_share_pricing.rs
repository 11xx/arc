//! A figure one model served at several efforts, priced under a version 2
//! table's time-of-day term: each effort's share is priced by the requests
//! made at that effort, so the shares and their requests cover the same
//! tokens. Recordings are written into the fixture's own home, named as Pi
//! names them, so tapes reads them as it reads a harness store.

use super::common::*;
use serde_json::{json, Value};

const LIST: &str = "list-price-estimate";
const PI: &str = "019a0000-0000-7000-8000-0000000000b1";
const REQUEST_TERMS: &str =
    "a price term set by the request may apply to requests the figure does not hold one by one";

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

[[models]]
id = "other-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 2.0, cache_read = 0.2, cache_write_5m = 2.5, output = 8.0 }
"#;

/// One request of 1M uncached input tokens inside and outside the window,
/// and on the model the table prices at a flat rate.
const PEAK: f64 = 1.0;
const OFF_PEAK: f64 = 0.5;
const OTHER: f64 = 2.0;

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

/// A change with one run bound to the Pi session, dispatched before its
/// recording begins and ended after it.
fn executed_change(repo: &Repo) -> String {
    let change = opened_change(repo, "efforts");
    run(repo, &change, "09:00:00", "23:59:00");
    change
}

fn opened_change(repo: &Repo, slug: &str) -> String {
    opened_change_id(&stdout(anonymous(repo).args([
        "begin",
        slug,
        "--no-worktree",
        "--target",
        "master",
    ])))
}

/// A run of `change` bound to the Pi session, dispatched at `from` and
/// ended at `to`.
fn run(repo: &Repo, change: &str, from: &str, to: &str) {
    let dispatch = event_id(&stdout(anonymous(repo).args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        "--change",
        change,
    ])));
    retime(repo, &dispatch, &at(from));
    stdout(anonymous(repo).args([
        "run",
        "bind",
        &dispatch,
        "--executor-harness",
        "pi",
        "--executor-session",
        PI,
    ]));
    let ended = event_id(&stdout(anonymous(repo).args([
        "run",
        "end",
        &dispatch,
        "--outcome",
        "completed",
    ])));
    retime(repo, &ended, &at(to));
}

fn sessions_root(repo: &Repo) -> PathBuf {
    repo.home.join(".pi/agent/sessions")
}

fn prices(repo: &Repo) -> String {
    let path = repo.home.join("prices.toml");
    fs::write(&path, TIME_OF_DAY).unwrap();
    path.to_string_lossy().to_string()
}

/// A Pi recording opened at 09:30 whose `turns` each select a model, where
/// it differs from the last turn's, and a thinking level, then hold a user
/// prompt and the request answering it,
/// of 1M uncached input tokens, recorded at the turn's clock, or with no
/// time of its own when the clock is absent. One branch holds every turn.
fn pi_session(repo: &Repo, turns: &[(&str, &str, Option<&str>)]) {
    let path = sessions_root(repo)
        .join("--fixture-project--")
        .join(format!("2026-01-01T09-30-00-000Z_{PI}.jsonl"));
    let mut lines = vec![
        json!({"type": "session", "version": 3, "id": PI, "timestamp": at("09:30:00"), "cwd": "/fixture/project"}),
    ];
    let mut parent = Value::Null;
    let mut serving = None;
    for (n, (model, level, clock)) in turns.iter().enumerate() {
        if serving != Some(model) {
            let selected = format!("model-{n}");
            lines.push(
                json!({"type": "model_change", "id": selected, "parentId": parent,
                "timestamp": at("09:30:00"), "provider": "fixture", "modelId": model}),
            );
            parent = json!(selected);
            serving = Some(model);
        }
        let thinking = format!("level-{n}");
        lines.push(
            json!({"type": "thinking_level_change", "id": thinking, "parentId": parent,
            "timestamp": at("09:30:00"), "thinkingLevel": level}),
        );
        let user = format!("user-{n}");
        lines.push(json!({"type": "message", "id": user, "parentId": thinking, "timestamp": at(clock.unwrap_or("09:31:00")),
            "message": {"role": "user", "timestamp": 1767259860000u64, "content": [{"type": "text", "text": "Go on."}]}}));
        parent = json!(format!("assistant-{n}"));
        let mut assistant = json!({"type": "message", "id": parent, "parentId": user,
            "message": {"role": "assistant", "timestamp": 1767259860000u64, "provider": "fixture", "model": model,
                "stopReason": "stop",
                "usage": {"input": 1_000_000, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 1_000_000,
                    "cost": {"total": 1.0}},
                "content": [{"type": "text", "text": "Done."}]}});
        if let Some(clock) = clock {
            assistant["timestamp"] = json!(at(clock));
        }
        lines.push(assistant);
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

/// The figure `tapes usage --full --prices` gives the recording, through
/// the same tapes reader and pricing arc pins.
fn tapes_figure(repo: &Repo) -> Value {
    let backends: Vec<Box<dyn agent_tapes_core::backend::Backend>> = vec![Box::new(
        agent_tapes_core::backend::pi::PiBackend::new(sessions_root(repo)),
    )];
    let mut usage =
        agent_tapes_core::usage_full_with_backends(&backends, agent_tapes_core::Selection::Id(PI))
            .unwrap();
    let table = agent_tapes_core::pricing::PriceTable::parse(TIME_OF_DAY).unwrap();
    agent_tapes_core::pricing::price_view(&mut usage, &table);
    serde_json::to_value(&usage).unwrap()
}

/// The Pi session's row in the cost view of `changes`.
fn session_row(repo: &Repo, changes: &[&str]) -> Value {
    let prices = prices(repo);
    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--json", "--prices", &prices])
            .args(changes),
    );
    view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == format!("pi:{PI}"))
        .unwrap_or_else(|| panic!("no session: {view:#}"))
        .clone()
}

fn quantity(totals: &Value) -> Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals:#}"))
        .iter()
        .find(|quantity| quantity.get("partition").is_none() && quantity["kind"] == LIST)
        .unwrap_or_else(|| panic!("no list price: {totals:#}"))
        .clone()
}

/// The Pi session's list price, the session bound to one change's run.
fn list_price(repo: &Repo) -> Value {
    let change = executed_change(repo);
    quantity(&session_row(repo, &[&change])["totals"])
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

/// Each share of tapes' split, as `(model, variant)`.
fn split(tapes: &Value) -> Vec<(Value, Value)> {
    tapes["by_model"]
        .as_array()
        .unwrap_or_else(|| panic!("no split: {tapes:#}"))
        .iter()
        .map(|share| (share["model"].clone(), share["variant"].clone()))
        .collect()
}

/// Claim: a figure one model served at two efforts, every request timed,
/// is priced over each effort's own requests, alone or beside another
/// model, so its list price is complete at the figure tapes gives the
/// recording: a `high` request at peak and two `max` requests off-peak,
/// 2.0, and with a request on a flat-rate model too, 4.0. Rejects matching
/// each effort's share against all of the model's requests, which then
/// cover more than the share and leave a known subtotal of 1.5 up to 3.0,
/// or 3.5 up to 5.0, with the request-terms reason.
#[test]
fn a_model_at_two_efforts_is_priced_by_each_efforts_requests() {
    let efforts = [
        ("fixture-model", "high", Some("10:00:00")),
        ("fixture-model", "max", Some("17:00:00")),
        ("fixture-model", "max", Some("18:00:00")),
    ];
    let beside = [("other-model", "max", Some("19:00:00"))];
    let alone = efforts.to_vec();
    let mixed = [&efforts[..2], &beside[..], &efforts[2..]].concat();
    for (turns, expected, models) in [
        (alone, PEAK + 2.0 * OFF_PEAK, &["fixture-model"][..]),
        (
            mixed,
            PEAK + 2.0 * OFF_PEAK + OTHER,
            &["fixture-model", "other-model"][..],
        ),
    ] {
        let repo = Repo::new();
        pi_session(&repo, &turns);
        let tapes = tapes_figure(&repo);
        let mut shares = vec![
            (json!("fixture-model"), json!("high")),
            (json!("fixture-model"), json!("max")),
        ];
        if models.len() > 1 {
            shares.push((json!("other-model"), json!("max")));
        }
        assert_eq!(split(&tapes), shares, "{tapes:#}");
        let figure = &tapes["list_price_estimate"]["amount"];
        close(figure, expected, &tapes);

        let list = list_price(&repo);
        assert_eq!(list["status"], "complete", "{list:#}");
        close(&list["amount"], figure.as_f64().unwrap(), &list);
    }
}

/// Claim: an effort's share whose request has no time of its own is priced
/// between every rate it could pay, with the request-terms reason, while
/// the other effort's timed request is priced at its own rate: a known
/// subtotal of 1.5 up to 2.0. Rejects pricing an untimed request as though
/// its time were settled, and rejects leaving the timed effort in doubt
/// too, which reads 1.0 up to 2.0.
#[test]
fn an_effort_share_without_request_times_stays_a_range() {
    let repo = Repo::new();
    pi_session(
        &repo,
        &[
            ("fixture-model", "high", Some("10:00:00")),
            ("fixture-model", "max", None),
        ],
    );
    let tapes = tapes_figure(&repo);
    assert_eq!(
        split(&tapes),
        [
            (json!("fixture-model"), json!("high")),
            (json!("fixture-model"), json!("max"))
        ],
        "{tapes:#}"
    );
    assert!(
        tapes["list_price_estimate"]["amount"].is_null(),
        "{tapes:#}"
    );

    let list = list_price(&repo);
    assert_eq!(list["status"], "incomplete", "{list:#}");
    close(&list["known_subtotal"], PEAK + OFF_PEAK, &list);
    assert_eq!(list["bound"]["scope"], "whole figure", "{list:#}");
    close(&list["bound"]["at_least"], PEAK + OFF_PEAK, &list);
    close(&list["bound"]["at_most"], 2.0 * PEAK, &list);
    assert!(
        list["reasons"].to_string().contains(REQUEST_TERMS),
        "{list:#}"
    );
}
