//! Cost view inputs and figures a review found mishandled: a retained
//! participant estimate read at another idle gap, and a non-ASCII `--idle`.
//! Recordings are written into the fixture's own home, named as each
//! harness names them, so tapes reads them as it reads a harness store.

use super::common::*;
use serde_json::{json, Value};

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
fn quantity<'a>(totals: &'a Value, matches: impl Fn(&Value) -> bool) -> &'a Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals:#}"))
        .iter()
        .find(|quantity| quantity.get("partition").is_none() && matches(quantity))
        .unwrap_or_else(|| panic!("no such quantity: {totals:#}"))
}

fn uncached(totals: &Value) -> Option<u64> {
    let figure = quantity(totals, |quantity| {
        quantity["categories"] == "disjoint" && quantity["category"] == "uncached_input"
    });
    figure["amount"]
        .as_u64()
        .or_else(|| figure["known_subtotal"].as_u64())
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
