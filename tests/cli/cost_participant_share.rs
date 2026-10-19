//! A participant's share of the work it acted on: a review start opens its
//! span, and what its spans leave unattributed is estimated by its working
//! stretches, labelled apart from the measured shares. Each test writes a Pi
//! recording, named as Pi names it, whose requests each carry their own
//! instant, and moves the ledger events to fixed instants, so every
//! request's place is known.

use super::common::*;
use serde_json::{json, Value};

const REVIEWER: &str = "019a0000-0000-7000-8000-0000000000e1";

fn at(clock: &str) -> String {
    format!("2026-01-01T{clock}Z")
}

/// A Pi recording whose assistant requests each record `input` uncached
/// tokens at their own instant.
fn pi_session(home: &Path, id: &str, requests: &[(&str, u64)]) {
    let path = home
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
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
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

fn as_pi(repo: &Repo, session: &str) -> AssertCommand {
    let mut command = repo.arc(&repo.root);
    command.env("ARC_HARNESS", "pi").env("ARC_SESSION", session);
    command
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

/// The reviewer session starts reviewing `change` at `clock`.
fn start(repo: &Repo, session: &str, change: &str, clock: &str) {
    let id = event_id(&stdout(
        as_pi(repo, session).args(["review", change, "--start"]),
    ));
    retime(repo, &id, &at(clock));
}

/// The reviewer session records a verdict on `change` at `clock`.
fn verdict(repo: &Repo, session: &str, change: &str, clock: &str) {
    let out = stdout(as_pi(repo, session).args(["review", change, "--verdict", "comment-only"]));
    retime(repo, &event_id(&out), &at(clock));
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

/// A figure's uncached input tokens: its amount, or its known subtotal.
fn uncached(totals: &Value) -> Option<u64> {
    let quantity = totals["quantities"].as_array()?.iter().find(|quantity| {
        quantity.get("partition").is_none()
            && quantity["categories"] == "disjoint"
            && quantity["category"] == "uncached_input"
    })?;
    quantity["amount"]
        .as_u64()
        .or_else(|| quantity["known_subtotal"].as_u64())
}

fn share_of<'a>(shares: &'a Value, change: &str) -> Option<&'a Value> {
    shares
        .as_array()?
        .iter()
        .find(|share| share["work"] == format!("change:{change}"))
        .map(|share| &share["totals"])
}

fn work<'a>(report: &'a Value, change: &str) -> &'a Value {
    report["work"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == format!("change:{change}"))
        .unwrap_or_else(|| panic!("no work item {change}: {report:#}"))
}

/// Claim: a reviewer that starts each review and records its verdict gets a
/// measured share of each change: its requests between the start and the
/// verdict. Rejects a span one instant wide, which leaves the reading
/// unattributed.
#[test]
fn a_reviewer_with_a_start_and_a_verdict_per_item_gets_measured_shares() {
    let repo = Repo::new();
    pi_session(
        &repo.home,
        REVIEWER,
        &[
            ("10:05:00", 10),
            ("10:15:00", 100),
            ("10:35:00", 1000),
            ("10:45:00", 7),
        ],
    );
    let (a, b) = (change(&repo, "first"), change(&repo, "second"));
    start(&repo, REVIEWER, &a, "10:00:00");
    verdict(&repo, REVIEWER, &a, "10:20:00");
    start(&repo, REVIEWER, &b, "10:30:00");
    verdict(&repo, REVIEWER, &b, "10:50:00");

    let view = report(&repo, &[&a, &b]);
    let row = session(&view, &format!("pi:{REVIEWER}"));
    let shares = &row["split"]["shares"];
    assert_eq!(
        uncached(share_of(shares, &a).unwrap()),
        Some(110),
        "{row:#}"
    );
    assert_eq!(
        uncached(share_of(shares, &b).unwrap()),
        Some(1007),
        "{row:#}"
    );
    assert_eq!(uncached(&work(&view, &a)["direct"]), Some(110), "{view:#}");
}

/// Claim: a reviewer that records only a verdict per item gets estimated
/// shares from its working stretch: requests up to a verdict serve its
/// change. They are labelled `estimated` with the `working-stretch` basis
/// and the idle gap, the measured figure stays beside them, and measured,
/// estimated and unattributed conserve the whole. Rejects leaving the
/// reviewer's reading unattributed, and mixing estimates into the measured
/// figure.
#[test]
fn a_reviewer_with_only_verdicts_gets_labelled_estimated_shares_that_conserve() {
    let repo = Repo::new();
    pi_session(
        &repo.home,
        REVIEWER,
        &[
            ("10:05:00", 10),
            ("10:15:00", 100),
            ("10:35:00", 1000),
            ("10:45:00", 7),
        ],
    );
    let (a, b) = (change(&repo, "first"), change(&repo, "second"));
    verdict(&repo, REVIEWER, &a, "10:20:00");
    verdict(&repo, REVIEWER, &b, "10:50:00");

    let view = report(&repo, &[&a, &b]);
    let row = session(&view, &format!("pi:{REVIEWER}"));
    let estimate = &row["split"]["estimate"];
    assert_eq!(estimate["status"], "estimated", "{row:#}");
    assert_eq!(estimate["basis"], "working-stretch", "{estimate:#}");
    assert_eq!(estimate["idle"], "45m", "{estimate:#}");
    assert_eq!(
        estimate["conservation"]["status"], "conserved",
        "{estimate:#}"
    );
    assert_eq!(
        uncached(share_of(&estimate["shares"], &a).unwrap()),
        Some(110)
    );
    assert_eq!(
        uncached(share_of(&estimate["shares"], &b).unwrap()),
        Some(1007)
    );
    assert_eq!(uncached(&estimate["unattributed"]), Some(0), "{estimate:#}");

    // The measured figure and the figure with estimates, both, and labelled.
    let item = work(&view, &a);
    assert_eq!(uncached(&item["direct"]), Some(0), "{item:#}");
    let estimated = &item["estimated"]["direct"];
    assert_eq!(uncached(estimated), Some(110), "{item:#}");
    assert_eq!(estimated["estimate"]["basis"], "working-stretch");
    assert!(estimated["quantities"]
        .as_array()
        .unwrap()
        .iter()
        .all(|quantity| quantity["estimated"] == true));
    assert_eq!(
        uncached(&view["union"]["estimated"]),
        Some(1117),
        "{view:#}"
    );
    let text = stdout(repo.arc(&repo.root).args(["cost", "show", &a, &b]));
    assert!(
        text.contains("total with estimates (working stretches, idle 45m)"),
        "{text}"
    );
}

/// Claim: a working stretch holding no event of the participant stays
/// unattributed, while the stretch holding its verdicts is estimated; a
/// shorter idle gap cuts the stretches finer. Rejects giving an idle-gapped
/// stretch to the nearest verdict.
#[test]
fn a_stretch_with_no_participant_event_stays_unattributed() {
    let repo = Repo::new();
    pi_session(
        &repo.home,
        REVIEWER,
        &[
            ("08:00:00", 5),
            ("08:10:00", 50),
            ("10:05:00", 10),
            ("10:15:00", 100),
            ("10:25:00", 3),
            ("10:50:00", 1000),
            ("10:55:00", 7),
        ],
    );
    let (a, b) = (change(&repo, "first"), change(&repo, "second"));
    verdict(&repo, REVIEWER, &a, "10:20:00");
    verdict(&repo, REVIEWER, &b, "11:00:00");

    let view = report(&repo, &[&a, &b]);
    let estimate = &session(&view, &format!("pi:{REVIEWER}"))["split"]["estimate"];
    assert_eq!(
        uncached(&estimate["unattributed"]),
        Some(55),
        "{estimate:#}"
    );
    assert_eq!(
        uncached(share_of(&estimate["shares"], &a).unwrap()),
        Some(110)
    );
    assert_eq!(
        uncached(share_of(&estimate["shares"], &b).unwrap()),
        Some(1010)
    );
    assert_eq!(
        estimate["conservation"]["status"], "conserved",
        "{estimate:#}"
    );

    // At a 15m idle gap, the 25 minutes after 10:25 end the first verdict's
    // stretch, so the request after that verdict stays with its change.
    let view = report(&repo, &[&a, &b, "--idle", "15m"]);
    let estimate = &session(&view, &format!("pi:{REVIEWER}"))["split"]["estimate"];
    assert_eq!(estimate["idle"], "15m", "{estimate:#}");
    assert_eq!(
        uncached(share_of(&estimate["shares"], &a).unwrap()),
        Some(113)
    );
    assert_eq!(
        uncached(share_of(&estimate["shares"], &b).unwrap()),
        Some(1007)
    );
    assert_eq!(
        uncached(&estimate["unattributed"]),
        Some(55),
        "{estimate:#}"
    );
}

/// Claim: `--allocate following` gives what stays unattributed between two
/// items, here a stretch with no event of the reviewer, to the next item,
/// labelled `allocated` with the method beside the conserving and the
/// estimated figures. Rejects silently adding it to the measured or the
/// estimated figure.
#[test]
fn allocate_following_gives_what_remains_between_items_to_the_next() {
    let repo = Repo::new();
    pi_session(
        &repo.home,
        REVIEWER,
        &[("10:05:00", 10), ("11:30:00", 20), ("12:50:00", 300)],
    );
    let (a, b) = (change(&repo, "first"), change(&repo, "second"));
    verdict(&repo, REVIEWER, &a, "10:20:00");
    verdict(&repo, REVIEWER, &b, "13:00:00");

    let view = report(&repo, &[&a, &b, "--allocate", "following"]);
    let second = work(&view, &b);
    assert_eq!(uncached(&second["direct"]), Some(0), "{second:#}");
    assert_eq!(
        uncached(&second["estimated"]["direct"]),
        Some(300),
        "{second:#}"
    );
    let allocated = &second["allocated"]["direct"];
    assert_eq!(allocated["allocation"], "following", "{second:#}");
    assert_eq!(uncached(allocated), Some(320), "{second:#}");
    assert!(allocated["quantities"]
        .as_array()
        .unwrap()
        .iter()
        .all(|quantity| quantity["allocated"] == true));
    assert_eq!(uncached(&work(&view, &a)["allocated"]["direct"]), Some(10));
    let text =
        stdout(
            repo.arc(&repo.root)
                .args(["cost", "show", &a, &b, "--allocate", "following"]),
        );
    assert!(text.contains("total allocated following"), "{text}");
}

const EXECUTOR: &str = "019a0000-0000-7000-8000-0000000000e2";
/// Three sequential runs, each with its requests, and clean gaps between.
/// A run's dispatch and end, and the requests it holds.
type Run = (&'static str, &'static str, &'static [(&'static str, u64)]);

const RUNS: [Run; 3] = [
    (
        "09:00:00",
        "09:30:00",
        &[("09:05:00", 11), ("09:25:00", 13)],
    ),
    (
        "09:40:00",
        "10:10:00",
        &[("09:45:00", 170), ("10:05:00", 190)],
    ),
    (
        "10:20:00",
        "10:50:00",
        &[("10:25:00", 2300), ("10:45:00", 2900)],
    ),
];

fn executor_requests() -> Vec<(&'static str, u64)> {
    RUNS.iter()
        .flat_map(|(_, _, requests)| requests.iter().copied())
        .collect()
}

/// Claim: an executor session with exact run spans, read as if it were a
/// participant with one event per item at each run's end, gets estimated
/// shares equal to its exact run shares for sequential work with clean
/// gaps. Rejects an estimator that drifts from the truth it can know.
#[test]
fn the_estimate_equals_exact_run_shares_for_sequential_work() {
    // The truth: the session bound to three runs, one per change.
    let exact = Repo::new();
    pi_session(&exact.home, EXECUTOR, &executor_requests());
    let mut truth = Vec::new();
    let mut exact_changes = Vec::new();
    for (index, (start, end, _)) in RUNS.iter().enumerate() {
        let change = change(&exact, &format!("run-{index}"));
        let dispatch = event_id(&stdout(anonymous(&exact, &exact.root).args([
            "run",
            "dispatch",
            "--route",
            "fixture-route",
            "--worktree",
            "/work/run",
            "--change",
            &change,
        ])));
        retime(&exact, &dispatch, &at(start));
        stdout(anonymous(&exact, &exact.root).args([
            "run",
            "bind",
            &dispatch,
            "--executor-harness",
            "pi",
            "--executor-session",
            EXECUTOR,
        ]));
        let ended = event_id(&stdout(anonymous(&exact, &exact.root).args([
            "run",
            "end",
            &dispatch,
            "--outcome",
            "completed",
        ])));
        retime(&exact, &ended, &at(end));
        exact_changes.push(change);
    }
    let targets: Vec<&str> = exact_changes.iter().map(String::as_str).collect();
    let view = report(&exact, &targets);
    for change in &exact_changes {
        truth.push(uncached(&work(&view, change)["direct"]).unwrap());
    }
    assert_eq!(truth, [24, 360, 5200], "{view:#}");

    // The same recording as a participant's: one verdict per change at its
    // run's end, and no run spans.
    let read = Repo::new();
    pi_session(&read.home, EXECUTOR, &executor_requests());
    let mut changes = Vec::new();
    for (index, (_, end, _)) in RUNS.iter().enumerate() {
        let change = change(&read, &format!("run-{index}"));
        verdict(&read, EXECUTOR, &change, end);
        changes.push(change);
    }
    let targets: Vec<&str> = changes.iter().map(String::as_str).collect();
    let view = report(&read, &targets);
    let estimated: Vec<u64> = changes
        .iter()
        .map(|change| uncached(&work(&view, change)["estimated"]["direct"]).unwrap())
        .collect();
    assert_eq!(estimated, truth, "{view:#}");
}
