//! Cost view list prices a fourth review found could be exceeded by the
//! consumption they price: a Pi figure whose abandoned branch was served by
//! another model than the active branch, and a Codex figure one model
//! served at several efforts beside a request no model is named for.
//! Recordings are written into the fixture's own home, named as each
//! harness names them, so tapes reads them as it reads a harness store.

use super::common::*;
use serde_json::{json, Value};

const LIST: &str = "list-price-estimate";
const PI: &str = "019a0000-0000-7000-8000-0000000000c1";
const CODEX: &str = "019a0000-0000-7000-8000-0000000000c2";
const UNATTRIBUTED: &str = "some requests could not be attributed to a model";

/// A table whose cheap row halves its rates from 16:30 to 00:30 UTC, and
/// whose expensive row costs twice the cheap one's peak rate all day.
const TIME_OF_DAY: &str = r#"schema = "tapes-price-table/2"
currency = "USD"

[[models]]
id = "cheap-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.25, output = 4.0 }

[[models.time_of_day]]
from = "16:30"
until = "00:30"
per_million = { uncached_input = 0.5, cache_read = 0.05, cache_write_5m = 0.625, output = 2.0 }

[[models]]
id = "expensive-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 2.0, cache_read = 0.2, cache_write_5m = 2.5, output = 8.0 }
"#;

/// One request of 1M uncached input tokens on the cheap model inside and
/// outside its off-peak window, and on the expensive model.
const PEAK: f64 = 1.0;
const OFF_PEAK: f64 = 0.5;
const EXPENSIVE: f64 = 2.0;

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

fn close(value: &Value, expected: f64, context: &Value) {
    let value = value
        .as_f64()
        .unwrap_or_else(|| panic!("no amount: {context:#}"));
    assert!(
        (value - expected).abs() < 1e-9,
        "{value} != {expected}: {context:#}"
    );
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

// --- Consumption on an abandoned branch another model served --------------

/// A Pi recording opened at 09:30 on the cheap model whose 10:00 request was
/// followed by a switch to the expensive model and a 12:00 request on it,
/// then rewound to the 10:00 answer, switched back and followed by a 17:00
/// request on the cheap model, the active branch's leaf. Each request holds
/// 1M uncached input tokens.
fn pi_abandoned_on_another_model(repo: &Repo) {
    let path = repo
        .home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T09-30-00-000Z_{PI}.jsonl"));
    let selected = |id: &str, parent: &str, model: &str| {
        json!({"type": "model_change", "id": id, "parentId": parent, "timestamp": at("09:30:00"),
            "provider": "fixture", "modelId": model})
    };
    let turn = |n: usize, parent: &str, model: &str, clock: &str| {
        let user = format!("user-{n}");
        [
            json!({"type": "message", "id": user, "parentId": parent, "timestamp": at(clock),
                "message": {"role": "user", "timestamp": 1767259860000u64, "content": [{"type": "text", "text": "Go on."}]}}),
            json!({"type": "message", "id": format!("assistant-{n}"), "parentId": user, "timestamp": at(clock),
                "message": {"role": "assistant", "timestamp": 1767259860000u64, "provider": "fixture", "model": model,
                    "stopReason": "stop",
                    "usage": {"input": 1_000_000, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 1_000_000,
                        "cost": {"total": 1.0}},
                    "content": [{"type": "text", "text": "Done."}]}}),
        ]
    };
    let mut lines = vec![
        json!({"type": "session", "version": 3, "id": PI, "timestamp": at("09:30:00"), "cwd": "/fixture/project"}),
        json!({"type": "model_change", "id": "model-0", "parentId": null, "timestamp": at("09:30:00"),
            "provider": "fixture", "modelId": "cheap-model"}),
    ];
    lines.extend(turn(0, "model-0", "cheap-model", "10:00:00"));
    lines.push(selected("model-1", "assistant-0", "expensive-model"));
    lines.extend(turn(1, "model-1", "expensive-model", "12:00:00"));
    lines.push(selected("model-2", "assistant-0", "cheap-model"));
    lines.extend(turn(2, "model-2", "cheap-model", "17:00:00"));
    write_lines(&path, &lines);
}

/// Claim: a Pi figure counting every retained branch prices each request
/// by the model that request names, so an abandoned 12:00 request on the
/// expensive model costs the expensive rate beside the active branch's
/// peak and off-peak requests on the cheap one: complete at 3.5, the
/// amount tapes gives the consumption. Rejects pricing all of the
/// consumption by the active branch's one model, which reads a known
/// subtotal of 2.0 bounded at 2.5, below what the session cost.
#[test]
fn an_abandoned_branch_is_priced_by_the_model_it_names() {
    let repo = Repo::new();
    pi_abandoned_on_another_model(&repo);
    let tapes = tapes_figure(
        Box::new(agent_tapes_core::backend::pi::PiBackend::new(
            repo.home.join(".pi/agent/sessions"),
        )),
        PI,
        TIME_OF_DAY,
    );
    let consumption = &tapes["consumption"];
    assert_eq!(consumption["events"]["other_branches"], 1, "{tapes:#}");
    let expected = PEAK + EXPENSIVE + OFF_PEAK;
    close(
        &consumption["list_price_estimate"]["amount"],
        expected,
        &tapes,
    );

    let change = executed_change(&repo, "pi", PI);
    let (row, union) = view(&repo, &change, TIME_OF_DAY, "pi", PI);
    assert_eq!(row["measurement"]["events"]["other_branches"], 1, "{row:#}");
    for list in [list_price(&row["totals"]), list_price(&union)] {
        assert_eq!(list["status"], "complete", "{list:#}");
        close(&list["amount"], expected, &list);
    }
}

/// Claim: a divided Pi session's figure added up from its segments prices
/// a segment one model served whole beside a segment priced share by share:
/// with one change's run holding the 10:00 request and the abandoned 12:00
/// one and another's the 17:00 request, the union of both changes is
/// complete at 3.5 and each change's share at its own requests' rates.
/// Rejects dropping the segment one model served from the sum's shares,
/// which leaves the union a known subtotal of 2.5 with the
/// unattributed-request reason.
#[test]
fn segments_priced_whole_and_share_by_share_add_up() {
    let repo = Repo::new();
    pi_abandoned_on_another_model(&repo);
    let morning = run_change(&repo, "morning", "pi", PI, "09:00:00", "13:00:00");
    let evening = run_change(&repo, "evening", "pi", PI, "16:00:00", "23:59:00");
    let prices = prices(&repo, TIME_OF_DAY);
    let view = json_stdout(repo.arc(&repo.root).args([
        "cost", "show", "--json", "--prices", &prices, &morning, &evening,
    ]));
    let union = list_price(&view["union"]);
    assert_eq!(union["status"], "complete", "{union:#}");
    close(&union["amount"], PEAK + EXPENSIVE + OFF_PEAK, &union);
    for (change, expected) in [(&morning, PEAK + EXPENSIVE), (&evening, OFF_PEAK)] {
        let work = view["work"]
            .as_array()
            .unwrap()
            .iter()
            .find(|work| work["key"] == format!("change:{change}"))
            .unwrap_or_else(|| panic!("no work {change}: {view:#}"));
        let list = list_price(&work["direct"]);
        assert_eq!(list["status"], "complete", "{list:#}");
        close(&list["amount"], expected, &list);
    }
}

/// Claim: retained facts that count an abandoned branch's events but keep
/// no model share for them, as facts an earlier calculation retained keep
/// them, are never priced by the one model the active branch names: with
/// the recording gone, the list price has no amount and no bound, and names
/// the unattributed-request reason. Rejects pricing the three requests at
/// the cheap model's rates, which bounds the session at 1.5 to 3.0 although
/// it cost 3.5.
#[test]
fn retained_branch_consumption_without_shares_is_not_priced_by_the_active_model() {
    let repo = Repo::new();
    pi_abandoned_on_another_model(&repo);
    let change = executed_change(&repo, "pi", PI);
    stdout(repo.arc(&repo.root).args(["cost", "retain", &change]));
    let events: Vec<PathBuf> = fs::read_dir(repo.root.join(".git/arc/repository/events"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            fs::read_to_string(path)
                .unwrap()
                .contains("cost-facts-retained")
        })
        .collect();
    let [path] = &events[..] else {
        panic!("one retention: {events:?}");
    };
    let mut event: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let facts = event["facts"].as_object_mut().unwrap();
    assert_eq!(facts["served_model"], "cheap-model", "{facts:?}");
    assert_eq!(facts["events"]["other_branches"], 1, "{facts:?}");
    assert!(facts.remove("by_model").is_some(), "{facts:?}");
    facts.insert("calculation".into(), json!("usage-normalization 7"));
    fs::write(path, serde_json::to_vec(&event).unwrap()).unwrap();
    fs::remove_dir_all(repo.home.join(".pi/agent/sessions")).unwrap();

    let (row, union) = view(&repo, &change, TIME_OF_DAY, "pi", PI);
    assert_ne!(row["measurement"]["source"], "fresh", "{row:#}");
    for list in [list_price(&row["totals"]), list_price(&union)] {
        assert_eq!(list["status"], "absent", "{list:#}");
        assert!(list.get("known_subtotal").is_none(), "{list:#}");
        assert!(list.get("bound").is_none(), "{list:#}");
        assert!(
            list["reasons"].to_string().contains(UNATTRIBUTED),
            "{list:#}"
        );
    }
}

// --- One model at several efforts beside an unattributed request ----------

/// A table that prices the cheap model at one rate all day.
const FLAT: &str = r#"schema = "tapes-price-table/1"
currency = "USD"

[[models]]
id = "cheap-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.25, output = 4.0 }
"#;

/// A Codex rollout of 1M-token requests at 10:00, one under each of
/// `efforts`: a request under no effort comes before any turn context names
/// a model, and the others are under the cheap model at that effort.
fn codex_rollout(repo: &Repo, efforts: &[Option<&str>]) {
    let path = repo
        .home
        .join(".codex/sessions/2026/01/01")
        .join(format!("rollout-2026-01-01T10-00-00-{CODEX}.jsonl"));
    let usage = |input: u64| {
        json!({"input_tokens": input, "cached_input_tokens": 0, "cache_write_input_tokens": 0, "output_tokens": 0,
            "reasoning_output_tokens": 0, "total_tokens": input})
    };
    let mut lines = vec![
        json!({"timestamp": at("10:00:00"), "type": "session_meta", "payload": {"id": CODEX, "session_id": CODEX,
            "timestamp": at("10:00:00"), "cwd": "/fixture/project", "model_provider": "fixture"}}),
    ];
    for (n, effort) in efforts.iter().enumerate() {
        let clock = format!("10:00:0{}", n * 2 + 1);
        if let Some(effort) = effort {
            lines.push(
                json!({"timestamp": at(&clock), "type": "turn_context", "payload": {
                "cwd": "/fixture/project", "model": "cheap-model", "effort": effort}}),
            );
        }
        let total = (n as u64 + 1) * 1_000_000;
        lines.extend([
            json!({"timestamp": at(&clock), "type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Work."}]}}),
            json!({"timestamp": at(&clock), "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Done."}]}}),
            json!({"timestamp": at(&clock), "type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": usage(total), "last_token_usage": usage(1_000_000),
                "model_context_window": 2_000_000}, "rate_limits": null}}),
        ]);
    }
    lines.push(json!({"timestamp": at("10:30:00"), "type": "event_msg", "payload": {"type": "task_complete"}}));
    write_lines(&path, &lines);
}

/// Claim: a Codex figure whose split leaves a request no model is named for
/// is priced only by its model shares, as tapes prices it, and never
/// complete: one model at `high` and `max` effort beside an unattributed
/// request, or at `high` alone beside it, is a known subtotal with the
/// unattributed-request reason at the amount tapes gives under a flat
/// table, and has no amount where tapes gives none under a time-of-day
/// table. Rejects promoting the one model's shares to the whole figure,
/// which prices the unattributed request as that model's and calls the
/// figure complete at 3.0, or 2.0 with one effort.
#[test]
fn an_unattributed_request_beside_one_models_shares_is_never_priced_as_that_model() {
    for efforts in [
        &[None, Some("high"), Some("max")][..],
        &[None, Some("high")][..],
    ] {
        let repo = Repo::new();
        codex_rollout(&repo, efforts);
        let change = executed_change(&repo, "codex", CODEX);
        for table in [FLAT, TIME_OF_DAY] {
            let tapes = tapes_figure(
                Box::new(agent_tapes_core::backend::codex::CodexBackend::new(
                    repo.home.join(".codex/sessions"),
                )),
                CODEX,
                table,
            );
            assert_eq!(
                tapes["session"]["model_observation"]["attribution_uncertain"], true,
                "{tapes:#}"
            );
            let figure = &tapes["list_price_estimate"];
            let (row, union) = view(&repo, &change, table, "codex", CODEX);
            for list in [list_price(&row["totals"]), list_price(&union)] {
                assert_ne!(list["status"], "complete", "{list:#}");
                assert!(
                    list["reasons"].to_string().contains(UNATTRIBUTED),
                    "{list:#}"
                );
                if figure.is_null() {
                    assert_eq!(list["status"], "absent", "{list:#}");
                } else {
                    assert!(
                        figure["measurement_completeness"]["uncached_input"]
                            .to_string()
                            .contains("incomplete-attribution"),
                        "{tapes:#}"
                    );
                    close(
                        &list["known_subtotal"],
                        figure["amount"].as_f64().unwrap(),
                        &list,
                    );
                }
            }
        }
    }
}
