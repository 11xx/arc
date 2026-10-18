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

/// `arc` acting as a pi session.
fn acting(repo: &Repo, cwd: &Path, session: &str) -> AssertCommand {
    let mut command = repo.arc(cwd);
    command.env("ARC_HARNESS", "pi").env("ARC_SESSION", session);
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

/// A codex recording whose cumulative counter records input, cached input
/// and output. Codex records no dollar figure.
fn codex_recording(repo: &Repo, id: &str, input: u64) {
    let path = repo
        .home
        .join(".codex/sessions/2026/01/01")
        .join(format!("rollout-2026-01-01T10-00-00-{id}.jsonl"));
    write_lines(
        &path,
        &[
            json!({"timestamp": "2026-01-01T10:00:00Z", "type": "session_meta", "payload": {"id": id, "session_id": id, "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project", "model_provider": "fixture"}}),
            json!({"timestamp": "2026-01-01T10:00:01Z", "type": "turn_context", "payload": {"cwd": "/fixture/project", "model": "fixture-model", "effort": "high"}}),
            json!({"timestamp": "2026-01-01T10:00:02Z", "type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Work."}]}}),
            json!({"timestamp": "2026-01-01T10:00:03Z", "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Done."}]}}),
            json!({"timestamp": "2026-01-01T10:00:04Z", "type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": input, "cached_input_tokens": 100, "output_tokens": 50, "reasoning_output_tokens": 10, "total_tokens": input + 50},
                "last_token_usage": {"input_tokens": input, "cached_input_tokens": 100, "output_tokens": 50, "reasoning_output_tokens": 10, "total_tokens": input + 50},
                "model_context_window": 1000}, "rate_limits": null}}),
            json!({"timestamp": "2026-01-01T10:00:05Z", "type": "event_msg", "payload": {"type": "task_complete"}}),
        ],
    );
}

/// A finished run on `change` with `route`, bound to `session`.
fn finished(repo: &Repo, change: &str, route: &str, harness: &str, session: &str) {
    let run = dispatch(repo, change, route);
    stdout(anonymous(repo).args([
        "run",
        "bind",
        &run,
        "--executor-harness",
        harness,
        "--executor-session",
        session,
    ]));
    end(repo, &run);
}

/// The pi session `session` snapshots and approves `slug`.
fn reviewed_by(repo: &Repo, slug: &str, session: &str) {
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    stdout(acting(repo, &worktree, session).args(["snapshot", slug]));
    acting(repo, &worktree, session)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
}

/// The table's view of one column of a totals object, as the table cell
/// carries it.
fn column_cell(totals: &Value, usd: bool) -> Value {
    let quantity = totals["quantities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|quantity| {
            quantity.get("partition").is_none()
                && if usd {
                    quantity["kind"] == USD
                } else {
                    quantity["categories"] == "disjoint" && quantity["category"] == "total"
                }
        })
        .unwrap_or_else(|| panic!("no column: {totals}"));
    let mut cell = json!({"status": quantity["status"]});
    for field in ["amount", "known_subtotal", "bound", "known_bound"] {
        if !quantity[field].is_null() {
            cell[field] = quantity[field].clone();
        }
    }
    cell
}

fn text_line<'a>(text: &'a str, label: &str) -> &'a str {
    text.lines()
        .find(|line| line.starts_with(label))
        .unwrap_or_else(|| panic!("no {label} line: {text}"))
}

/// Claim: the table's total is the JSON union figure, which for one change
/// begun from no artifact is its direct figure, with the same status, on a
/// selection holding an executor, a participant divided between two changes
/// and an unmeasured one; the divided participant joins with its share and
/// its unattributed part stays outside. Rejects a table total summed apart
/// from the figures the JSON qualifies.
#[test]
fn the_table_total_is_the_json_union_and_direct_figure() {
    let repo = Repo::new();
    pi_recording(&repo, "exec-session", 0.05);
    pi_recording(&repo, "lead-session", 0.2);
    let change = begin(&repo, "tabled", &[]);
    let other = begin(&repo, "elsewhere", &[]);
    finished(&repo, &change, "model-a#high", "pi", "exec-session");
    // The first snapshot of a head records it; the lead then approves it.
    let worktree = repo.home.join(".worktrees/repo-tabled");
    stdout(acting(&repo, &worktree, "broken").args(["snapshot", "tabled"]));
    reviewed_by(&repo, "tabled", "lead-session");
    reviewed_by(&repo, "elsewhere", "lead-session");
    assert!(!other.is_empty());

    let view = report(&repo, &[&change]);
    let table = &view["table"];
    let direct = &work(&view, &format!("change:{change}"))["direct"];
    for usd in [false, true] {
        let column = if usd { "usd" } else { "tokens" };
        assert_eq!(
            table["total"][column],
            column_cell(&view["union"], usd),
            "{view}"
        );
        assert_eq!(table["total"][column], column_cell(direct, usd), "{view}");
    }
    assert_ne!(table["total"]["status"], "complete", "{view}");
    let rows = table["rows"].as_array().unwrap();
    let status = |key: &str| {
        rows.iter()
            .find(|row| row["key"] == key)
            .unwrap_or_else(|| panic!("no row {key}: {view}"))["status"]
            .clone()
    };
    assert_eq!(status("pi:exec-session"), "complete", "{view}");
    assert_eq!(status("pi:lead-session"), "split ×2", "{view}");
    assert_eq!(status("pi:broken"), "unmeasured", "{view}");
    assert_eq!(
        table["unattributed"]["sessions"][0]["key"], "pi:lead-session",
        "{view}"
    );

    let text = stdout(repo.arc(&repo.root).args(["cost", "show", &change]));
    let total = text_line(&text, "total");
    assert!(
        total.ends_with(table["total"]["status"].as_str().unwrap()),
        "{text}"
    );
    assert!(
        text_line(&text, "pi:model-a#high").contains("$0.0500"),
        "{text}"
    );
    assert!(
        !text_line(&text, "unattributed shared (1 session)").is_empty(),
        "{text}"
    );
    assert!(text.contains("arc cost show --detail"), "{text}");
    for line in text.lines() {
        assert!(line.chars().count() <= 120, "{line}");
    }
    let detail = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--detail", &change]),
    );
    assert!(detail.contains("harness-calculated estimate"), "{detail}");
}

/// The change ids a view selects.
fn changes(view: &Value) -> Vec<String> {
    let mut ids: Vec<String> = view["work"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["kind"] == "change")
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

fn sorted(ids: &[&String]) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    ids.sort();
    ids
}

/// Claim: several targets, a tag, a session footprint, a time window and
/// `--all` each select exactly their changes, and a session shared by two
/// selected changes counts once in the union. Rejects a selector that adds
/// or drops work, and a union that counts a session per change.
#[test]
fn every_range_selects_exactly_its_changes_and_counts_a_session_once() {
    let repo = Repo::new();
    pi_recording(&repo, "lead-session", 0.2);
    pi_recording(&repo, "foot-session", 0.1);
    pi_recording(&repo, "exec-session", 0.05);
    let plan = Path::new(
        stdout(
            repo.arc(&repo.root)
                .args(["journal", "plan", "ranged-plan", "--body-file", "-"])
                .write_stdin("# Plan\n\nBody.\n"),
        )
        .trim(),
    )
    .file_name()
    .unwrap()
    .to_str()
    .unwrap()
    .to_string();
    let alpha = begin(&repo, "alpha", &["--tag", "x"]);
    let beta = begin(&repo, "beta", &["--tag", "x"]);
    let gamma = begin(&repo, "gamma", &[]);
    let delta = begin(&repo, "delta", &["--from-journal", &plan]);
    reviewed_by(&repo, "alpha", "lead-session");
    reviewed_by(&repo, "beta", "lead-session");
    reviewed_by(&repo, "alpha", "foot-session");
    reviewed_by(&repo, "gamma", "foot-session");
    finished(&repo, &delta, "model-a#high", "pi", "exec-session");
    // Gamma's events are older than every other change's.
    for entry in fs::read_dir(event_dir(&repo, &gamma)).unwrap() {
        let path = entry.unwrap().path();
        let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        event["created_at"] = json!("2025-06-01T00:00:00Z");
        fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
    }

    let view = report(&repo, &[&alpha, &plan]);
    assert_eq!(changes(&view), sorted(&[&alpha, &delta]), "{view}");
    assert!(
        view["work"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "journal"),
        "{view}"
    );
    assert_eq!(view["target"]["kind"], "range", "{view}");

    let tagged = report(&repo, &["--tag", "x"]);
    assert_eq!(changes(&tagged), sorted(&[&alpha, &beta]), "{tagged}");
    let lead: Vec<&Value> = tagged["union"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|key| *key == "pi:lead-session")
        .collect();
    assert_eq!(lead.len(), 1, "the lead counts once: {tagged}");
    // The footprint session also acted on an unselected change, so the
    // union takes its share of the selected ones.
    assert_eq!(tagged["union"]["shared"], json!([]), "{tagged}");
    assert!(
        tagged["union"]["shares"]
            .as_array()
            .unwrap()
            .contains(&json!("pi:foot-session")),
        "{tagged}"
    );

    let footprint = report(&repo, &["--footprint", "pi:foot-session"]);
    assert_eq!(
        changes(&footprint),
        sorted(&[&alpha, &gamma]),
        "{footprint}"
    );
    assert_eq!(
        footprint["footprint"]["session"], "pi:foot-session",
        "{footprint}"
    );
    assert_eq!(
        footprint["footprint"]["row"]["usd"]["known_subtotal"].as_f64(),
        Some(0.1),
        "{footprint}"
    );
    let executed = report(&repo, &["--footprint", "pi:exec-session"]);
    assert_eq!(changes(&executed), sorted(&[&delta]), "{executed}");

    let old = report(&repo, &["--until", "2025-12-31T00:00:00Z"]);
    assert_eq!(changes(&old), sorted(&[&gamma]), "{old}");
    let recent = report(&repo, &["--since", "2026-01-02T00:00:00Z"]);
    assert_eq!(
        changes(&recent),
        sorted(&[&alpha, &beta, &delta]),
        "{recent}"
    );

    let all = report(&repo, &["--all"]);
    assert_eq!(
        changes(&all),
        sorted(&[&alpha, &beta, &gamma, &delta]),
        "{all}"
    );
    let union: Vec<&Value> = all["union"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .collect();
    let mut unique = union.clone();
    unique.dedup();
    assert_eq!(union.len(), unique.len(), "{all}");

    let none = repo
        .arc(&repo.root)
        .args(["cost", "show", "--tag", "absent-tag"])
        .output()
        .unwrap();
    assert!(!none.status.success());
    assert!(String::from_utf8_lossy(&none.stderr).contains("selects no work"));
}

fn group<'a>(view: &'a Value, grouping: &str, value: &str) -> &'a Value {
    view["table"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["by"][grouping] == value)
        .unwrap_or_else(|| panic!("no {grouping} {value} group: {view}"))
}

fn amount(cell: &Value) -> f64 {
    cell["amount"]
        .as_f64()
        .unwrap_or_else(|| panic!("no amount: {cell}"))
}

/// Claim: `--by model` and `--by role` subtotals sum to the total within
/// one accounting partition, and a group spanning two accounting bases is
/// withheld beside its partitions rather than added. Rejects subtotals that
/// miss or double a session, and a sum across incompatible domains.
#[test]
fn group_subtotals_sum_to_the_total_within_one_partition_and_withhold_across() {
    let repo = Repo::new();
    pi_recording(&repo, "first-exec", 0.05);
    pi_recording(&repo, "second-exec", 0.03);
    pi_recording(&repo, "lead-session", 0.2);
    let change = begin(&repo, "grouped", &[]);
    finished(&repo, &change, "model-a#high", "pi", "first-exec");
    finished(&repo, &change, "model-b#low", "pi", "second-exec");
    reviewed_by(&repo, "grouped", "lead-session");
    let ended = snapshot(&repo, "ended", json!([]));
    let grouped = |by: &str| {
        json_stdout(
            repo.arc(&repo.root)
                .env("ARC_AGENT_STATUS", &ended)
                .args(["cost", "show", "--json", "--by", by, &change]),
        )
    };

    let view = grouped("model");
    let total = &view["table"]["total"];
    assert_eq!(total["status"], "complete", "{view}");
    let groups = view["table"]["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 3, "{view}");
    for column in ["tokens", "usd"] {
        let sum: f64 = groups.iter().map(|group| amount(&group[column])).sum();
        assert!(
            (sum - amount(&total[column])).abs() < 1e-9,
            "{column}: {view}"
        );
    }
    assert_eq!(
        amount(&group(&view, "model", "model-a")["usd"]),
        0.05,
        "{view}"
    );

    let view = grouped("role");
    let total = &view["table"]["total"];
    let implementer = group(&view, "role", "implementer");
    let reviewer = group(&view, "role", "reviewer");
    for column in ["tokens", "usd"] {
        let sum = amount(&implementer[column]) + amount(&reviewer[column]);
        assert!(
            (sum - amount(&total[column])).abs() < 1e-9,
            "{column}: {view}"
        );
    }
    let text = stdout(
        repo.arc(&repo.root)
            .env("ARC_AGENT_STATUS", &ended)
            .args(["cost", "show", "--by", "role", &change]),
    );
    assert!(
        text_line(&text, "subtotal role implementer").contains("$0.0800"),
        "{text}"
    );

    // A codex executor brings a second accounting basis.
    codex_recording(&repo, "third-exec", 500);
    finished(
        &repo,
        &change,
        "gpt-x#high via codex",
        "codex",
        "third-exec",
    );
    let view = grouped("role");
    let implementer = group(&view, "role", "implementer");
    assert_eq!(implementer["tokens"]["status"], "withheld", "{view}");
    assert!(
        implementer["partitions"].as_array().unwrap().len() >= 2,
        "{view}"
    );
    assert_eq!(
        view["table"]["total"]["tokens"]["status"], "withheld",
        "{view}"
    );
    // One harness per group: no group mixes accounting bases.
    let view = grouped("harness");
    assert_eq!(
        group(&view, "harness", "pi")["tokens"]["status"],
        "complete",
        "{view}"
    );
    assert_ne!(
        group(&view, "harness", "codex")["tokens"]["status"],
        "withheld",
        "{view}"
    );
    let codex = view["table"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == "codex:third-exec")
        .unwrap();
    assert_eq!(codex["agent"], "codex:gpt-x#high", "{view}");
}

/// Claim: two sessions whose ids share their first eight characters, as
/// time-ordered ids do, get distinct short ids. Rejects a table where two
/// rows read as one session.
#[test]
fn short_session_ids_stay_distinct_when_ids_share_a_prefix() {
    let repo = Repo::new();
    pi_recording(&repo, "01a12b46-aaaa", 0.01);
    pi_recording(&repo, "01a12b46-bbbb", 0.02);
    let change = begin(&repo, "prefixed", &[]);
    finished(&repo, &change, "model-a#high", "pi", "01a12b46-aaaa");
    finished(&repo, &change, "model-a#high", "pi", "01a12b46-bbbb");
    let view = report(&repo, &[&change]);
    let mut short: Vec<&str> = view["table"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["session"].as_str().unwrap())
        .collect();
    short.sort();
    assert_eq!(short, ["01a12b46-a", "01a12b46-b"], "{view}");
}
