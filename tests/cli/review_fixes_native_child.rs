//! A Claude parent and its native children: a request the parent's and a
//! child's recordings both hold never counts twice in a total holding both,
//! and an unmeasured child keeps the unassigned children's figure a known
//! subtotal. Recordings are written as Claude writes them: the parent at
//! `<project>/<parent>.jsonl`, each subagent at
//! `<project>/<parent>/subagents/agent-<id>.jsonl` beside its
//! `agent-<id>.meta.json`. Liveness comes from a stand-in `agent-status`.

use super::common::*;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

const PARENT: &str = "019a0000-0000-7000-8000-0000000000b1";
const CHILD: &str = "b1000000000000001";
const GONE: &str = "b1000000000000002";

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

fn project(repo: &Repo) -> PathBuf {
    repo.home.join(".claude/projects/-fixture-project")
}

fn usage((input, output, cache_read, cache_write): (u64, u64, u64, u64)) -> Value {
    json!({"input_tokens": input, "output_tokens": output,
        "cache_read_input_tokens": cache_read, "cache_creation_input_tokens": cache_write})
}

/// One assistant request as Claude records it, in the parent's transcript
/// or, with `agent`, in a subagent's sidechain transcript.
fn request(
    agent: Option<&str>,
    uuid: &str,
    parent_uuid: &str,
    request: &str,
    message: &str,
    at: &str,
    tokens: (u64, u64, u64, u64),
) -> Value {
    let mut record = json!({"type": "assistant", "sessionId": PARENT, "uuid": uuid, "parentUuid": parent_uuid,
        "requestId": request, "timestamp": at, "cwd": "/fixture/project",
        "message": {"id": message, "role": "assistant", "model": "fixture-model",
            "content": [{"type": "text", "text": "Done."}], "usage": usage(tokens)}});
    if let Some(agent) = agent {
        record["agentId"] = json!(agent);
        record["isSidechain"] = json!(true);
    }
    record
}

/// The parent's transcript: a prompt, `requests`, the launch of each child
/// in `launched`, and a cost-state whose totals are `cost_state` input and
/// output tokens.
fn parent_recording(repo: &Repo, requests: &[Value], launched: &[&str], cost_state: (u64, u64)) {
    let mut lines = vec![
        json!({"type": "user", "sessionId": PARENT, "uuid": "user-1", "parentUuid": null, "timestamp": "2026-01-01T10:00:00Z",
            "cwd": "/fixture/project", "entrypoint": "cli", "message": {"role": "user", "content": "Lead the work."}}),
    ];
    lines.extend(requests.iter().cloned());
    for (n, agent) in launched.iter().enumerate() {
        lines.push(json!({"type": "user", "sessionId": PARENT, "uuid": format!("launch-{n}"), "parentUuid": "assistant-1",
            "timestamp": "2026-01-01T10:00:02Z", "cwd": "/fixture/project",
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": format!("toolu_{n}"), "content": "launched"}]},
            "toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": agent, "description": "Work"}}));
    }
    let (input, output) = cost_state;
    lines.push(json!({"type": "cost-state", "sessionId": PARENT, "totalCostUSD": 0.0, "totalAPIDuration": 1000, "totalDuration": 2000,
        "modelUsage": {"fixture-model": {"inputTokens": input, "outputTokens": output, "thinkingTokens": 0,
            "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0, "webSearchRequests": 0, "costUSD": 0.0}},
        "hasUnknownModelCost": false}));
    write_lines(&project(repo).join(format!("{PARENT}.jsonl")), &lines);
}

/// A subagent's sidechain transcript holding `requests`, and its meta record.
fn subagent(repo: &Repo, agent: &str, requests: &[Value]) {
    let directory = project(repo).join(PARENT).join("subagents");
    let mut lines = vec![
        json!({"type": "user", "sessionId": PARENT, "agentId": agent, "isSidechain": true, "uuid": format!("{agent}-user"),
            "parentUuid": null, "timestamp": "2026-01-01T10:00:01Z", "cwd": "/fixture/project",
            "message": {"role": "user", "content": "Work on the slice."}}),
    ];
    lines.extend(requests.iter().cloned());
    write_lines(&directory.join(format!("agent-{agent}.jsonl")), &lines);
    fs::write(
        directory.join(format!("agent-{agent}.meta.json")),
        json!({"agentType": "general-purpose", "description": "Work on the slice.", "spawnDepth": 1})
            .to_string(),
    )
    .unwrap();
}

fn event_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .expect("command output should contain an event id")
        .to_string()
}

fn lead(repo: &Repo) -> AssertCommand {
    let mut command = repo.arc(&repo.root);
    command
        .env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", PARENT);
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

/// A change the parent opened, with every event it holds at 10:00:00.
fn led_change(repo: &Repo, slug: &str) -> String {
    let change = opened_change_id(&stdout(lead(repo).args([
        "begin",
        slug,
        "--no-worktree",
        "--target",
        "master",
    ])));
    for entry in fs::read_dir(event_dir(repo, &change)).unwrap() {
        let path = entry.unwrap().path();
        let id = path.file_stem().unwrap().to_str().unwrap().to_string();
        retime(repo, &id, "2026-01-01T10:00:00Z");
    }
    change
}

/// The parent dispatches a run for `change`, binds `agent` to it as its
/// native child, and ends the run around the child's work.
fn bound_run(repo: &Repo, change: &str, agent: &str) {
    let dispatch = event_id(&stdout(lead(repo).args([
        "run",
        "dispatch",
        "--route",
        "claude-code subagent: fixture-model#high",
        "--worktree",
        "/work/run",
        "--change",
        change,
    ])));
    retime(repo, &dispatch, "2026-01-01T09:59:00Z");
    stdout(lead(repo).args([
        "run",
        "bind",
        &dispatch,
        "--executor-harness",
        "claude",
        "--executor-session",
        agent,
        "--parent-session",
        PARENT,
    ]));
    let ended = event_id(&stdout(lead(repo).args([
        "run",
        "end",
        &dispatch,
        "--outcome",
        "completed",
    ])));
    retime(repo, &ended, "2026-01-01T10:00:10Z");
}

/// A stand-in `agent-status` whose owned snapshot lists no thread, so every
/// Claude session it is asked about has ended.
fn ended(repo: &Repo) -> PathBuf {
    let answer = json!({"threads": [], "owner": {"pid": 1, "start_ticks": 1, "started_at": 1, "build": "fixture"}});
    let file = repo.home.join("bin/ended.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, answer.to_string()).unwrap();
    let path = repo.home.join("bin/ended");
    fs::write(&path, format!("#!/bin/sh\ncat '{}'\n", file.display())).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn report(repo: &Repo, change: &str) -> Value {
    json_stdout(
        repo.arc(&repo.root)
            .env("ARC_AGENT_STATUS", ended(repo))
            .args(["cost", "show", "--json", change]),
    )
}

/// One combined quantity of a figure by its category name and kind.
fn quantity<'a>(totals: &'a Value, categories: &str, category: &str) -> &'a Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals}"))
        .iter()
        .find(|quantity| {
            quantity.get("partition").is_none()
                && quantity["categories"] == categories
                && quantity["category"] == category
        })
        .unwrap_or_else(|| panic!("no {categories} {category}: {totals}"))
}

/// Claim: an ended parent counted in a change's union beside its bound
/// native child, whose recordings both hold one 10-token request, never
/// gives that union 20 as a complete figure: it is withheld with identity
/// incomplete, bounded at least 10 and at most 20, while the family
/// reconciliation names the request both recordings hold. Rejects summing
/// the two recordings as disjoint.
#[test]
fn a_request_parent_and_bound_child_both_hold_never_counts_twice() {
    let repo = Repo::new();
    let shared = (10, 0, 0, 0);
    parent_recording(
        &repo,
        &[request(
            None,
            "assistant-1",
            "user-1",
            "req_shared",
            "msg_shared",
            "2026-01-01T10:00:03Z",
            shared,
        )],
        &[CHILD],
        (10, 0),
    );
    subagent(
        &repo,
        CHILD,
        &[request(
            Some(CHILD),
            "child-assistant-0",
            &format!("{CHILD}-user"),
            "req_shared",
            "msg_shared",
            "2026-01-01T10:00:03Z",
            shared,
        )],
    );
    let change = led_change(&repo, "shared");
    bound_run(&repo, &change, CHILD);

    let view = report(&repo, &change);
    let parent = view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == format!("claude:{PARENT}"))
        .unwrap_or_else(|| panic!("no parent row: {view:#}"));
    assert_eq!(
        parent["family"]["reconciliation"]["status"], "not-conserved",
        "{:#}",
        parent["family"]
    );
    let input = quantity(&view["union"], "native", "input");
    assert_ne!(input["status"], "complete", "{input:#}");
    assert!(input.get("amount").is_none(), "{input:#}");
    let bound = &input["bound"];
    assert_eq!(bound["at_least"], 10, "{input:#}");
    assert_eq!(bound["at_most"], 20, "{input:#}");
    assert_ne!(view["union"]["identity"]["status"], "complete", "{view:#}");
}

/// Claim: a parent with one unbound child measured at 73 tokens and another
/// whose recording is missing gives its unassigned children no amount: the
/// 73 is a known subtotal and the missing child is named. Rejects a
/// complete 73 that leaves the unmeasured child out silently.
#[test]
fn an_unmeasured_unassigned_child_keeps_the_unassigned_figure_a_subtotal() {
    let repo = Repo::new();
    let measured = (7, 11, 50, 5);
    parent_recording(
        &repo,
        &[request(
            None,
            "assistant-1",
            "user-1",
            "req_parent",
            "msg_parent",
            "2026-01-01T10:00:01Z",
            (10, 30, 0, 0),
        )],
        &[CHILD, GONE],
        (17, 41),
    );
    subagent(
        &repo,
        CHILD,
        &[request(
            Some(CHILD),
            "child-assistant-0",
            &format!("{CHILD}-user"),
            "req_child",
            "msg_child",
            "2026-01-01T10:00:05Z",
            measured,
        )],
    );
    let change = led_change(&repo, "unassigned");
    // A dispatch makes the parent an orchestrator of the change; no binding
    // names either child, so both are unassigned.
    let dispatch = event_id(&stdout(lead(&repo).args([
        "run",
        "dispatch",
        "--route",
        "claude-code subagent: fixture-model#high",
        "--worktree",
        "/work/run",
        "--change",
        &change,
    ])));
    retime(&repo, &dispatch, "2026-01-01T09:59:00Z");

    let view = report(&repo, &change);
    let parent = view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == format!("claude:{PARENT}"))
        .unwrap_or_else(|| panic!("no parent row: {view:#}"));
    let unassigned = &parent["family"]["unassigned"];
    assert_eq!(unassigned["children"], 2, "{unassigned:#}");
    assert_eq!(unassigned["unmeasured"], 1, "{unassigned:#}");
    let total = quantity(&unassigned["totals"], "disjoint", "total");
    assert_ne!(total["status"], "complete", "{total:#}");
    assert!(total.get("amount").is_none(), "{total:#}");
    assert_eq!(total["known_subtotal"], 73, "{total:#}");
    assert!(
        total["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason.as_str().unwrap().contains(GONE)),
        "{total:#}"
    );
}
