//! A session that served several work items is divided among them by span:
//! an executor's runs, and a participant's events on each item. Each test
//! writes Pi recordings with per-request timestamps and moves the ledger
//! events to fixed instants, so every request's place is known.

use super::common::*;
use serde_json::{json, Value};

const LEAD: &str = "019a0000-0000-7000-8000-00000000000a";
const EXECUTOR: &str = "019a0000-0000-7000-8000-00000000000e";
const LIST: &str = "list-price-estimate";
const COUNTERS: [&str; 5] = ["input", "output", "reasoning", "cache_read", "cache_write"];
const DISJOINT: [&str; 5] = [
    "uncached_input",
    "cache_read",
    "cache_write",
    "output",
    "total",
];

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

/// A Pi recording, named as Pi names one, whose assistant requests each
/// record `input` uncached tokens at their own instant.
fn pi_session(repo: &Repo, id: &str, requests: &[(&str, u64)]) {
    let path = repo
        .home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T09-00-00-000Z_{id}.jsonl"));
    let mut lines = vec![
        json!({"type": "session", "version": 3, "id": id, "timestamp": at("09:00:00"), "cwd": "/fixture/project"}),
        json!({"type": "message", "id": "user-0", "parentId": null, "timestamp": at("09:00:01"),
            "message": {"role": "user", "timestamp": 1767258001000u64, "content": [{"type": "text", "text": "Work."}]}}),
    ];
    let mut parent = "user-0".to_string();
    for (n, (clock, input)) in requests.iter().enumerate() {
        let entry = format!("assistant-{n}");
        lines.push(json!({"type": "message", "id": entry, "parentId": parent, "timestamp": at(clock),
            "message": {"role": "assistant", "timestamp": 1767258002000u64, "provider": "fixture", "model": "fixture-model",
                "usage": {"input": input, "output": 200, "cacheRead": 400, "cacheWrite": 0, "totalTokens": input + 600,
                    "cost": {"total": *input as f64 / 1000.0}},
                "content": [{"type": "text", "text": "Done."}]}}));
        parent = entry;
    }
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

/// Move the events a change holds, in their order, to the instants given:
/// the first event to the first, the last to the last, and the rest evenly
/// between.
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

/// Open, snapshot, approve and integrate one change as the lead, with its
/// events moved to `first`..`last`.
fn led_change(repo: &Repo, slug: &str, first: &str, last: &str) -> String {
    let mut begin = acting(repo, &repo.root, LEAD);
    begin.args(["begin", slug]);
    let change = opened_change_id(&stdout(&mut begin));
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

/// Dispatch a run for `change` at `start`, bind it to the executor, and end
/// it at `end`.
fn run(repo: &Repo, change: &str, start: &str, end: &str) -> String {
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

fn work<'a>(report: &'a Value, change: &str) -> &'a Value {
    report["work"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == format!("change:{change}"))
        .unwrap_or_else(|| panic!("no work item {change}: {report}"))
}

/// One combined quantity of a figure: a native counter, a disjoint
/// category, or a dollar kind.
fn figure<'a>(totals: &'a Value, categories: &str, category: &str) -> &'a Value {
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

/// A figure's amount, or its known subtotal while a qualification, such as
/// unknown liveness, keeps it from being complete.
fn amount(totals: &Value, categories: &str, category: &str) -> Option<u64> {
    let quantity = figure(totals, categories, category);
    quantity["amount"]
        .as_u64()
        .or_else(|| quantity["known_subtotal"].as_u64())
}

fn uncached(totals: &Value) -> Option<u64> {
    amount(totals, "disjoint", "uncached_input")
}

fn share<'a>(row: &'a Value, change: &str) -> &'a Value {
    &row["split"]["shares"]
        .as_array()
        .unwrap_or_else(|| panic!("no shares: {row}"))
        .iter()
        .find(|share| share["work"] == format!("change:{change}"))
        .unwrap_or_else(|| panic!("no share of {change}: {row}"))["totals"]
}

/// The item shares and the unattributed part add up to the session's whole
/// figure in the segments' basis, per counter and per disjoint category.
fn assert_conserves(row: &Value) {
    let split = &row["split"];
    assert_eq!(split["status"], "divided", "{row}");
    assert!(
        amount(&split["whole"], "disjoint", "total").is_some(),
        "{row}"
    );
    let parts: Vec<&Value> = split["shares"]
        .as_array()
        .unwrap()
        .iter()
        .map(|share| &share["totals"])
        .chain(
            split["unattributed"]["totals"]
                .as_object()
                .map(|_| &split["unattributed"]["totals"]),
        )
        .collect();
    for (categories, names) in [("native", &COUNTERS[..]), ("disjoint", &DISJOINT[..])] {
        for name in names {
            let whole = amount(&split["whole"], categories, name);
            let sum: Option<u64> = parts
                .iter()
                .map(|part| amount(part, categories, name))
                .sum();
            // A counter the recording never records is absent in every part.
            assert_eq!(sum, whole, "{categories} {name} conserves: {row}");
        }
    }
}

// Claim: a lead acting on two changes one after the other is divided
// exactly: each change's total holds the requests inside its span, and what
// lies before, between and after the spans is unattributed with that
// reason. Rejects the whole figure in neither total, and a split by
// proximity that hands the between request to a neighbour.
#[test]
fn a_lead_across_two_sequential_changes_splits_exactly() {
    let repo = Repo::new();
    pi_session(
        &repo,
        LEAD,
        &[
            ("09:50:00", 1),
            ("10:05:00", 10),
            ("10:15:00", 100),
            ("10:25:00", 1000),
            ("10:40:00", 10000),
        ],
    );
    let first = led_change(&repo, "first", "10:00:00", "10:10:00");
    let second = led_change(&repo, "second", "10:20:00", "10:30:00");

    let view = report(&repo, &[&first, &second]);
    let key = format!("pi:{LEAD}");
    let row = session(&view, &key);
    assert_conserves(row);
    assert_eq!(uncached(share(row, &first)), Some(10), "{row}");
    assert_eq!(uncached(share(row, &second)), Some(1000), "{row}");
    let reasons = &row["split"]["unattributed"]["reasons"];
    assert_eq!(
        reasons,
        &json!(["after last item", "before first item", "between items"]),
        "{row}"
    );
    assert_eq!(
        uncached(&row["split"]["unattributed"]["totals"]),
        Some(10101),
        "{row}"
    );
    for (change, expected) in [(&first, 10), (&second, 1000)] {
        let direct = &work(&view, change)["direct"];
        assert_eq!(uncached(direct), Some(expected), "{change}: {view}");
        assert_eq!(direct["shares"], json!([key]), "{view}");
        assert_eq!(work(&view, change)["shared"], json!([]), "{view}");
    }
    // The union covers the lead's whole membership, so it counts it whole.
    assert_eq!(uncached(&view["union"]), Some(11111), "{view}");

    // A view of one change takes its share; the rest is unattributed.
    let alone = report(&repo, &[&first]);
    assert_eq!(uncached(&alone["union"]), Some(10), "{alone}");
    let footer = &alone["table"]["unattributed"];
    assert_eq!(footer["sessions"][0]["key"], key, "{alone}");
    // Three requests of `input + 600` disjoint tokens each.
    let tokens = &footer["tokens"];
    let shown = tokens["amount"]
        .as_u64()
        .or(tokens["known_subtotal"].as_u64());
    assert_eq!(shown, Some(10101 + 3 * 600), "{alone}");
    let text = stdout(repo.arc(&repo.root).args(["cost", "show", &first]));
    assert!(text.contains("split ×2"), "{text}");
    assert!(text.contains("unattributed shared (1 session)"), "{text}");
    assert!(
        text.contains("after last item, before first item, between items"),
        "{text}"
    );
}

// Claim: requests inside the spans of two interleaved changes are given to
// neither: they are unattributed with the reason `overlap`, and the shares
// still conserve. `--allocate even` divides them evenly between the two and
// labels every allocated figure, keeping the conserving one beside it.
// Rejects counting an overlapped request in both changes, and an allocation
// that replaces the conserving figure or is not labelled.
#[test]
fn interleaved_changes_leave_the_overlap_unattributed_unless_allocated() {
    let repo = Repo::new();
    pi_session(
        &repo,
        LEAD,
        &[("10:05:00", 10), ("10:15:00", 1001), ("10:25:00", 100000)],
    );
    let first = led_change(&repo, "first", "10:00:00", "10:20:00");
    let second = led_change(&repo, "second", "10:10:00", "10:30:00");

    let view = report(&repo, &[&first]);
    let key = format!("pi:{LEAD}");
    let row = session(&view, &key);
    assert_conserves(row);
    assert_eq!(uncached(share(row, &first)), Some(10), "{row}");
    assert_eq!(uncached(share(row, &second)), Some(100000), "{row}");
    assert_eq!(
        row["split"]["unattributed"]["reasons"],
        json!(["overlap"]),
        "{row}"
    );
    assert_eq!(
        uncached(&row["split"]["unattributed"]["totals"]),
        Some(1001),
        "{row}"
    );
    assert_eq!(uncached(&work(&view, &first)["direct"]), Some(10), "{view}");
    assert!(view["work"][0].get("allocated").is_none(), "{view}");

    let allocated = report(&repo, &[&first, "--allocate", "even"]);
    let item = work(&allocated, &first);
    assert_eq!(uncached(&item["direct"]), Some(10), "{allocated}");
    let even = &item["allocated"]["direct"];
    assert_eq!(even["allocation"], "even", "{allocated}");
    // 1001 tokens in two shares: the first item in order takes the odd one.
    assert_eq!(uncached(even), Some(10 + 501), "{allocated}");
    assert_eq!(
        figure(even, "disjoint", "uncached_input")["allocated"],
        true
    );
    let other = report(&repo, &[&second, "--allocate", "even"]);
    assert_eq!(
        uncached(&work(&other, &second)["allocated"]["direct"]),
        Some(100000 + 500),
        "{other}"
    );
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", &first, "--allocate", "even"]),
    );
    assert!(text.contains("total allocated even"), "{text}");
    assert!(text.contains("overlap"), "{text}");
}

// Claim: one warm executor bound to three changes through three sequential
// runs is divided per run, and each change's total holds exactly its run.
// Rejects the whole session shared outside every total, and per-run figures
// that are absent for a session with several runs.
#[test]
fn a_warm_executor_across_three_sequential_runs_splits_per_run() {
    let repo = Repo::new();
    pi_session(
        &repo,
        EXECUTOR,
        &[
            ("10:05:00", 10),
            ("10:25:00", 100),
            ("10:45:00", 1000),
            ("11:30:00", 7),
        ],
    );
    let changes = ["warm-one", "warm-two", "warm-three"];
    // The first run opens the recording, so nothing is consumed before it.
    let dispatches = [
        run(&repo, changes[0], "08:59:00", "10:10:00"),
        run(&repo, changes[1], "10:20:00", "10:30:00"),
        run(&repo, changes[2], "10:40:00", "10:50:00"),
    ];
    let view = report(&repo, &changes);
    let key = format!("pi:{EXECUTOR}");
    let row = session(&view, &key);
    assert_conserves(row);
    assert_eq!(row["per_run"], "split", "{row}");
    for ((change, dispatch), expected) in changes.iter().zip(&dispatches).zip([10, 100, 1000]) {
        let alone = report(&repo, &[change]);
        let item = work(&alone, change);
        assert_eq!(
            uncached(&item["direct"]),
            Some(expected),
            "{change}: {alone}"
        );
        assert_eq!(
            uncached(&alone["union"]),
            Some(expected),
            "{change}: {alone}"
        );
        let run = item["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["dispatch_event_id"] == *dispatch)
            .unwrap();
        assert_eq!(run["per_run"], "split", "{alone}");
        assert_eq!(uncached(&run["totals"]), Some(expected), "{alone}");
    }
    // Consumption after the last run ends is the executor's, not any run's.
    assert_eq!(
        row["split"]["unattributed"]["reasons"],
        json!(["after last item"]),
        "{row}"
    );
    assert_eq!(uncached(&view["union"]), Some(1117), "{view}");
}

// Claim: a split read is retained with its segments, so a session divided
// before its source rotated is still divided after. Rejects retention that
// keeps only the whole figure.
#[test]
fn a_retained_split_survives_source_rotation() {
    let repo = Repo::new();
    pi_session(&repo, EXECUTOR, &[("10:05:00", 10), ("10:25:00", 100)]);
    run(&repo, "kept-one", "08:59:00", "10:10:00");
    run(&repo, "kept-two", "10:20:00", "10:30:00");
    stdout(repo.arc(&repo.root).args(["cost", "retain", "kept-one"]));
    let recording = repo
        .home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T09-00-00-000Z_{EXECUTOR}.jsonl"));
    fs::remove_file(recording).unwrap();
    let view = report(&repo, &["kept-one"]);
    let row = session(&view, &format!("pi:{EXECUTOR}"));
    assert_eq!(row["measurement"]["source"], "retained", "{row}");
    assert_conserves(row);
    assert_eq!(
        uncached(&work(&view, "kept-one")["direct"]),
        Some(10),
        "{view}"
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

// Claim: with a price table every row and total carries a list-price
// estimate equal to hand arithmetic over its disjoint categories, leading
// the money columns beside the harness figure. Without one the money column
// is the harness figure alone and the footer says how to install a table.
// Rejects a list price that ignores a category or adds across rows wrongly,
// and a table that shows a list column with nothing to price it.
#[test]
fn list_prices_follow_hand_arithmetic_and_absent_tables_leave_the_harness_figure() {
    let repo = Repo::new();
    pi_session(&repo, EXECUTOR, &[("10:05:00", 1000), ("10:25:00", 3000)]);
    run(&repo, "priced-one", "08:59:00", "10:10:00");
    run(&repo, "priced-two", "10:20:00", "10:30:00");
    let prices = repo.home.join("prices.toml");
    fs::write(&prices, TABLE).unwrap();
    let prices = prices.to_string_lossy().to_string();

    // Each request: input at $2/M, 400 cache reads at $0.5/M, 200 output at
    // $10/M.
    let request = |input: f64| input * 2.0 / 1e6 + 400.0 * 0.5 / 1e6 + 200.0 * 10.0 / 1e6;
    let view = report(&repo, &["priced-one", "priced-two", "--prices", &prices]);
    assert_eq!(view["prices"]["status"], "loaded", "{view}");
    let close = |value: &Value, expected: f64| {
        let value = value["amount"]
            .as_f64()
            .or_else(|| value["known_subtotal"].as_f64())
            .unwrap_or_else(|| panic!("no amount: {value}"));
        assert!((value - expected).abs() < 1e-12, "{value} != {expected}");
    };
    close(
        figure(&view["union"], "kind", LIST),
        request(1000.0) + request(3000.0),
    );
    close(
        figure(&work(&view, "priced-one")["direct"], "kind", LIST),
        request(1000.0),
    );
    let row = session(&view, &format!("pi:{EXECUTOR}"));
    close(
        figure(&row["totals"], "kind", LIST),
        request(1000.0) + request(3000.0),
    );
    close(
        figure(share(row, "priced-two"), "kind", LIST),
        request(3000.0),
    );
    let text = stdout(repo.arc(&repo.root).args([
        "cost",
        "show",
        "priced-one",
        "priced-two",
        "--prices",
        &prices,
    ]));
    let header = text.lines().nth(1).unwrap();
    assert!(
        header.contains("list") && header.contains("harness"),
        "{text}"
    );
    assert!(header.find("list") < header.find("harness"), "{text}");
    assert!(
        text.contains(&format!("${:.4}", request(1000.0) + request(3000.0))),
        "{text}"
    );

    let bare = report(&repo, &["priced-one", "priced-two"]);
    assert_eq!(bare["prices"]["status"], "absent", "{bare}");
    let list = figure(&bare["union"], "kind", LIST);
    assert!(list.get("amount").is_none(), "{bare}");
    assert!(
        list["reasons"]
            .to_string()
            .contains("no price table is loaded"),
        "{bare}"
    );
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "priced-one", "priced-two"]),
    );
    let header = text.lines().nth(1).unwrap();
    assert!(
        header.contains("money") && !header.contains("list"),
        "{text}"
    );
    assert!(text.contains("tapes-price-table/1"), "{text}");
    assert!(text.contains("tapes/prices.toml"), "{text}");
}
