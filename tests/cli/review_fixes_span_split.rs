//! The span split's totals and footer account for every request once: a
//! selection takes its items' shares and never a session's unattributed
//! part, the footer holds everything of a session outside the total, an
//! undivided shared session withholds the totals it touches, conservation is
//! claimed only where every counter compares, and model shares of one model
//! at several efforts add up. Recordings are Pi sessions named as Pi names
//! them, with per-request timestamps; ledger events are moved to fixed
//! instants so every request's place is known.

use super::common::*;
use serde_json::{json, Value};

const LEAD: &str = "019a0000-0000-7000-8000-00000000010a";
const EXECUTOR: &str = "019a0000-0000-7000-8000-00000000010e";
const ABSENT: &str = "019a0000-0000-7000-8000-00000000010f";

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

fn recording(repo: &Repo, id: &str) -> PathBuf {
    repo.home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T09-00-00-000Z_{id}.jsonl"))
}

/// A Pi recording whose assistant requests each record `input` uncached
/// tokens, 200 output and 400 cache reads at their own instant. A request
/// naming a thinking level is preceded by the `thinking_level_change` entry
/// Pi writes when the effort changes.
fn pi_session(repo: &Repo, id: &str, requests: &[(&str, u64, Option<&str>)]) {
    let mut lines = vec![
        json!({"type": "session", "version": 3, "id": id, "timestamp": at("09:00:00"), "cwd": "/fixture/project"}),
        json!({"type": "message", "id": "user-0", "parentId": null, "timestamp": at("09:00:01"),
            "message": {"role": "user", "timestamp": 1767258001000u64, "content": [{"type": "text", "text": "Work."}]}}),
    ];
    let mut parent = "user-0".to_string();
    for (n, (clock, input, level)) in requests.iter().enumerate() {
        if let Some(level) = level {
            let entry = format!("level-{n}");
            lines.push(
                json!({"type": "thinking_level_change", "id": entry, "parentId": parent,
                "timestamp": at(clock), "thinkingLevel": level}),
            );
            parent = entry;
        }
        let entry = format!("assistant-{n}");
        lines.push(json!({"type": "message", "id": entry, "parentId": parent, "timestamp": at(clock),
            "message": {"role": "assistant", "timestamp": 1767258002000u64, "provider": "fixture", "model": "fixture-model",
                "usage": {"input": input, "output": 200, "cacheRead": 400, "cacheWrite": 0, "totalTokens": input + 600,
                    "cost": {"total": *input as f64 / 1000.0}},
                "content": [{"type": "text", "text": "Done."}]}}));
        parent = entry;
    }
    let path = recording(repo, id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

fn plain(requests: &[(&'static str, u64)]) -> Vec<(&'static str, u64, Option<&'static str>)> {
    requests
        .iter()
        .map(|(clock, input)| (*clock, *input, None))
        .collect()
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

/// Move the events a change holds, in their order, evenly from `first` to
/// `last`.
fn retime_change(repo: &Repo, change: &str, first: &str, last: &str) {
    let mut events: Vec<(String, String)> = fs::read_dir(event_dir(repo, change))
        .unwrap()
        .map(|entry| {
            let event: Value =
                serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
            (
                event["created_at"].as_str().unwrap().to_string(),
                event["event_id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    events.sort();
    let parse = |instant: &str| chrono::DateTime::parse_from_rfc3339(instant).unwrap();
    let (from, to) = (parse(&at(first)), parse(&at(last)));
    let steps = events.len().saturating_sub(1).max(1) as i32;
    for (index, (_, id)) in events.iter().enumerate() {
        let instant = from + (to - from) / steps * index as i32;
        retime(repo, id, &instant.to_rfc3339());
    }
}

fn acting(repo: &Repo, cwd: &Path, session: &str) -> AssertCommand {
    let mut command = repo.arc(cwd);
    command.env("ARC_HARNESS", "pi").env("ARC_SESSION", session);
    command
}

/// One change the lead opens, snapshots, approves and integrates, with its
/// events moved to `first`..`last`.
fn led_change(repo: &Repo, slug: &str, first: &str, last: &str) -> String {
    let change = opened_change_id(&stdout(
        acting(repo, &repo.root, LEAD).args(["begin", slug]),
    ));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        "x\n",
        &format!("feat: {slug}"),
    );
    stdout(acting(repo, &worktree, LEAD).args(["snapshot", slug]));
    acting(repo, &worktree, LEAD)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    acting(repo, &repo.root, LEAD)
        .args(["integrate", slug])
        .assert()
        .success();
    retime_change(repo, &change, first, last);
    change
}

/// A run for `change`, dispatched by `by` (or nobody) at `start`, bound to
/// the executor and ended at `end`.
fn run(repo: &Repo, by: Option<&str>, change: &str, start: &str, end: &str) -> String {
    let mut dispatcher = match by {
        Some(session) => acting(repo, &repo.root, session),
        None => {
            let mut anonymous = repo.arc(&repo.root);
            anonymous
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION");
            anonymous
        }
    };
    let dispatch = event_id(&stdout(dispatcher.args([
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
        "pi",
        "--executor-session",
        EXECUTOR,
    ]));
    let ended = event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "end",
        &dispatch,
        "--outcome",
        "completed",
    ])));
    retime(repo, &ended, &at(end));
    dispatch
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
        .unwrap_or_else(|| panic!("no session {key}: {report}"))
}

fn quantity<'a>(totals: &'a Value, categories: &str, category: &str) -> &'a Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals}"))
        .iter()
        .find(|quantity| {
            quantity.get("partition").is_none()
                && match categories {
                    "kind" => quantity["kind"] == category,
                    _ => quantity["categories"] == categories && quantity["category"] == category,
                }
        })
        .unwrap_or_else(|| panic!("no {categories} {category}: {totals}"))
}

/// A figure's amount, or its known subtotal while a qualification such as
/// unknown liveness keeps it from being complete.
fn amount(totals: &Value, categories: &str, category: &str) -> Option<u64> {
    let quantity = quantity(totals, categories, category);
    quantity["amount"]
        .as_u64()
        .or_else(|| quantity["known_subtotal"].as_u64())
}

fn uncached(totals: &Value) -> Option<u64> {
    amount(totals, "disjoint", "uncached_input")
}

/// A table cell's token count: its amount or known subtotal.
fn cell(figure: &Value) -> Option<u64> {
    let tokens = &figure["tokens"];
    tokens["amount"]
        .as_u64()
        .or_else(|| tokens["known_subtotal"].as_u64())
}

/// Each request adds `input + 600` disjoint tokens.
fn disjoint(inputs: &[u64]) -> u64 {
    inputs.iter().map(|input| input + 600).sum()
}

/// A lead acting on two changes in turn, whose recording holds 10 tokens in
/// the first change's span, 20 in the second's, and 5 before either.
fn two_items(repo: &Repo) -> (String, String) {
    pi_session(
        repo,
        LEAD,
        &plain(&[("09:50:00", 5), ("10:05:00", 10), ("10:25:00", 20)]),
    );
    let first = led_change(repo, "first", "10:00:00", "10:10:00");
    let second = led_change(repo, "second", "10:20:00", "10:30:00");
    (first, second)
}

/// Claim: selecting every item a session acted on takes those items' shares,
/// and the session's consumption no item holds stays in the unattributed
/// footer. Rejects charging the whole recording, unattributed part included,
/// to the selection.
#[test]
fn selecting_a_whole_membership_takes_shares_and_leaves_the_unattributed_part_outside() {
    let repo = Repo::new();
    let (first, second) = two_items(&repo);
    let view = report(&repo, &[&first, &second]);
    assert_eq!(uncached(&view["union"]), Some(30), "{:#}", view["union"]);
    let footer = &view["table"]["unattributed"];
    assert_eq!(cell(footer), Some(disjoint(&[5])), "{footer:#}");
    assert_eq!(
        cell(&view["table"]["total"]),
        Some(disjoint(&[10, 20])),
        "{:#}",
        view["table"]
    );
}

/// Claim: the footer holds everything of a session outside the total: its
/// unattributed part and its shares of items the view does not select, with
/// the reason naming the unselected item. Rejects a footer that leaves an
/// unselected item's share outside both footer figures.
#[test]
fn the_footer_holds_unselected_items_shares_beside_the_unattributed_part() {
    let repo = Repo::new();
    let (first, _) = two_items(&repo);
    let view = report(&repo, &[&first]);
    let table = &view["table"];
    assert_eq!(cell(&table["total"]), Some(disjoint(&[10])), "{table:#}");
    let footer = &table["unattributed"];
    assert_eq!(cell(footer), Some(disjoint(&[5, 20])), "{footer:#}");
    let reasons = &footer["sessions"][0]["reasons"];
    assert!(
        reasons
            .as_array()
            .unwrap()
            .contains(&json!("unselected item")),
        "{footer:#}"
    );
    let text = stdout(repo.arc(&repo.root).args(["cost", "show", &first]));
    assert!(text.contains("unselected item"), "{text}");
}

/// Claim: a session the view cannot divide whose membership reaches past the
/// selection withholds the totals its unknown share would join. Rejects a
/// complete total beside a participant whose share is unknown.
#[test]
fn an_undivided_shared_session_withholds_the_totals_it_touches() {
    let repo = Repo::new();
    pi_session(&repo, EXECUTOR, &plain(&[("10:05:00", 50)]));
    let selected = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "selected"])));
    let other = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "other"])));
    // A lead whose recording is not in the store dispatches on both changes.
    run(&repo, Some(ABSENT), &selected, "08:59:00", "10:10:00");
    let mut lead = acting(&repo, &repo.root, ABSENT);
    stdout(lead.args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/other",
        "--change",
        &other,
    ]));

    let view = report(&repo, &[&selected]);
    assert_eq!(
        view["work"][0]["shared"],
        json!([format!("pi:{ABSENT}")]),
        "{view:#}"
    );
    let union = quantity(&view["union"], "disjoint", "total");
    assert_ne!(union["status"], "complete", "{union:#}");
    assert!(
        union["reasons"]
            .to_string()
            .contains(&format!("pi:{ABSENT}")),
        "the withheld total names the session whose share is unknown: {union:#}"
    );
    assert_ne!(
        view["table"]["total"]["status"], "complete",
        "{:#}",
        view["table"]
    );
}

fn retained_event(repo: &Repo) -> PathBuf {
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
    path.clone()
}

/// Claim: a counter one side does not establish is not compared, and
/// conservation is not claimed: it reads `unestablished` and names the
/// counter. Rejects reporting `conserved` while a nonempty segment's counter
/// is unknown.
#[test]
fn conservation_with_an_unknown_counter_is_unestablished() {
    let repo = Repo::new();
    pi_session(
        &repo,
        EXECUTOR,
        &plain(&[("10:05:00", 7), ("10:25:00", 100)]),
    );
    run(&repo, None, "held-one", "08:59:00", "10:10:00");
    run(&repo, None, "held-two", "10:20:00", "10:30:00");
    stdout(repo.arc(&repo.root).args(["cost", "retain", "held-one"]));
    let path = retained_event(&repo);
    let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let held = event["facts"]["spans"]["segments"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|segment| segment["requests"] == 1)
        .unwrap();
    held["facts"]["counters"]
        .as_object_mut()
        .unwrap()
        .remove("input");
    fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
    fs::remove_file(recording(&repo, EXECUTOR)).unwrap();

    let view = report(&repo, &["held-one"]);
    let row = session(&view, &format!("pi:{EXECUTOR}"));
    assert_eq!(row["measurement"]["source"], "retained", "{row:#}");
    let conservation = &row["split"]["conservation"];
    assert_eq!(conservation["status"], "unestablished", "{conservation:#}");
    assert_eq!(
        conservation["uncompared"],
        json!(["input"]),
        "{conservation:#}"
    );
}

const TABLE: &str = r#"schema = "tapes-price-table/1"
currency = "USD"

[[models]]
id = "fixture-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 2.0, cache_read = 0.5, cache_write_5m = 3.0, cache_write_1h = 4.0, output = 10.0 }
"#;

/// Claim: one model served at two efforts inside one segment is priced on
/// both efforts' consumption. Rejects a share that keeps only the first
/// effort's model share and leaves the other out of its list price.
#[test]
fn one_model_at_two_efforts_in_a_segment_prices_both() {
    let repo = Repo::new();
    pi_session(
        &repo,
        EXECUTOR,
        &[
            ("10:05:00", 100, Some("high")),
            ("10:06:00", 200, Some("low")),
            ("10:25:00", 1000, Some("high")),
        ],
    );
    run(&repo, None, "mixed-one", "08:59:00", "10:10:00");
    run(&repo, None, "mixed-two", "10:20:00", "10:30:00");
    let prices = repo.home.join("prices.toml");
    fs::write(&prices, TABLE).unwrap();
    let prices = prices.to_string_lossy().to_string();

    let view = report(&repo, &["mixed-one", "--prices", &prices]);
    let row = session(&view, &format!("pi:{EXECUTOR}"));
    let share = &row["split"]["shares"]
        .as_array()
        .unwrap()
        .iter()
        .find(|share| share["work"].as_str().unwrap().contains("mixed-one"))
        .unwrap_or_else(|| panic!("no share: {row:#}"))["totals"];
    assert_eq!(uncached(share), Some(300), "{share:#}");
    let list = quantity(share, "kind", "list-price-estimate");
    let priced = list["amount"]
        .as_f64()
        .or_else(|| list["known_subtotal"].as_f64())
        .unwrap_or_else(|| panic!("no list price: {list:#}"));
    // Two requests: uncached input at $2/M, 400 cache reads at $0.5/M and
    // 200 output at $10/M each.
    let expected = 300.0 * 2.0 / 1e6 + 2.0 * (400.0 * 0.5 / 1e6 + 200.0 * 10.0 / 1e6);
    assert!(
        (priced - expected).abs() < 1e-12,
        "{priced} != {expected}: {list:#}"
    );
}
