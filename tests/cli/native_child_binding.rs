//! A native child, a Claude subagent or a Codex `thread_spawn` thread, bound
//! to the work it served. Each test writes recordings shaped as the harness
//! writes them: a Claude parent at `<project>/<parent>.jsonl` with its
//! subagents at `<project>/<parent>/subagents/agent-<id>.jsonl` beside an
//! `agent-<id>.meta.json`, and Codex rollouts named
//! `rollout-<timestamp>-<id>.jsonl`.

use super::common::*;
use serde_json::{json, Value};

const PARENT: &str = "019a0000-0000-7000-8000-0000000000a1";
const SCOUT: &str = "a1000000000000001";
const REVIEWER: &str = "a1000000000000002";
const IDLE: &str = "a1000000000000003";
const MIRROR: &str = "a1000000000000004";
const GONE: &str = "a1000000000000005";
const HANDLE: &str = "lane-20260101T000000Z-ab12";

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

fn project(repo: &Repo) -> PathBuf {
    repo.home.join(".claude/projects/-fixture-project")
}

/// One request's usage, as a Claude assistant record carries it.
fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Value {
    json!({"input_tokens": input, "output_tokens": output,
        "cache_read_input_tokens": cache_read, "cache_creation_input_tokens": cache_write})
}

/// Disjoint total of one request under Claude's convention, where input
/// excludes the cache.
fn total(input: u64, output: u64, cache_read: u64, cache_write: u64) -> u64 {
    input + output + cache_read + cache_write
}

const OWN: (u64, u64, u64, u64) = (10, 30, 200, 40);
const SCOUTED: (u64, u64, u64, u64) = (3, 20, 100, 10);
const REVIEWED: (u64, u64, u64, u64) = (5, 40, 300, 20);
const IDLED: (u64, u64, u64, u64) = (7, 11, 50, 5);

fn of((input, output, cache_read, cache_write): (u64, u64, u64, u64)) -> u64 {
    total(input, output, cache_read, cache_write)
}

/// The lead's recording: its own request, the Agent call and launch result
/// that name a subagent whose transcript is not in the store, and the
/// cost-state Claude writes, whose totals cover the whole family. Like
/// Claude's, the cost-state carries no timestamp.
fn parent_recording(repo: &Repo) {
    let (input, output, cache_read, cache_write) = OWN;
    write_lines(
        &project(repo).join(format!("{PARENT}.jsonl")),
        &[
            json!({"type": "user", "sessionId": PARENT, "uuid": "user-1", "parentUuid": null, "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project", "entrypoint": "cli", "message": {"role": "user", "content": "Lead the work."}}),
            json!({"type": "assistant", "sessionId": PARENT, "uuid": "assistant-1", "parentUuid": "user-1", "requestId": "req_parent_1", "timestamp": "2026-01-01T10:00:01Z", "cwd": "/fixture/project",
                "message": {"id": "msg_parent_1", "role": "assistant", "model": "fixture-lead", "content": [{"type": "tool_use", "id": "toolu_gone", "name": "Agent", "input": {"description": "Gone", "subagent_type": "Explore", "prompt": "Look."}}],
                    "usage": usage(input, output, cache_read, cache_write)}}),
            json!({"type": "user", "sessionId": PARENT, "uuid": "user-2", "parentUuid": "assistant-1", "timestamp": "2026-01-01T10:00:02Z", "cwd": "/fixture/project",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_gone", "content": "launched"}]},
                "toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": GONE, "description": "Gone"}}),
            json!({"type": "cost-state", "sessionId": PARENT, "totalCostUSD": 0.5, "totalAPIDuration": 9000, "totalDuration": 10000,
                "modelUsage": {"fixture-lead": {"inputTokens": 40, "outputTokens": 120, "thinkingTokens": 0, "cacheReadInputTokens": 900, "cacheCreationInputTokens": 90, "webSearchRequests": 0, "costUSD": 0.5}},
                "hasUnknownModelCost": false}),
        ],
    );
}

/// A subagent's transcript and meta record under the parent's directory.
/// `requests` are `(timestamp, request id, usage)`; a mirror row's records
/// carry zero usage and no request id.
fn subagent(
    repo: &Repo,
    agent: &str,
    agent_type: &str,
    brief: &str,
    requests: &[(&str, Option<&str>, Value)],
) {
    let directory = project(repo).join(PARENT).join("subagents");
    let mut lines = vec![
        json!({"type": "user", "sessionId": PARENT, "agentId": agent, "isSidechain": true, "uuid": format!("{agent}-user"), "parentUuid": null,
        "timestamp": requests.first().map_or("2026-01-01T10:00:03Z", |(at, _, _)| at), "cwd": "/fixture/project", "message": {"role": "user", "content": brief}}),
    ];
    let mut previous = format!("{agent}-user");
    for (n, (at, request, usage)) in requests.iter().enumerate() {
        let uuid = format!("{agent}-assistant-{n}");
        let mut record = json!({"type": "assistant", "sessionId": PARENT, "agentId": agent, "isSidechain": true, "uuid": uuid, "parentUuid": previous,
            "timestamp": at, "cwd": "/fixture/project",
            "message": {"id": format!("msg_{agent}_{n}"), "role": "assistant", "model": "fixture-child", "content": [{"type": "text", "text": "Done."}], "usage": usage}});
        if let Some(request) = request {
            record["requestId"] = json!(request);
        }
        lines.push(record);
        previous = uuid;
    }
    write_lines(&directory.join(format!("agent-{agent}.jsonl")), &lines);
    fs::write(
        directory.join(format!("agent-{agent}.meta.json")),
        json!({"agentType": agent_type, "description": brief, "spawnDepth": 1}).to_string(),
    )
    .unwrap();
}

fn one(
    agent: &str,
    at: &str,
    (input, output, cache_read, cache_write): (u64, u64, u64, u64),
) -> Vec<(String, Option<String>, Value)> {
    vec![(
        at.to_string(),
        Some(format!("req_{agent}")),
        usage(input, output, cache_read, cache_write),
    )]
}

fn child(
    repo: &Repo,
    agent: &str,
    agent_type: &str,
    requests: Vec<(String, Option<String>, Value)>,
) {
    let held: Vec<(&str, Option<&str>, Value)> = requests
        .iter()
        .map(|(at, request, usage)| (at.as_str(), request.as_deref(), usage.clone()))
        .collect();
    subagent(repo, agent, agent_type, "Work on the slice.", &held);
}

/// The lead with three native children with consumption, one of them never
/// bound, and a delegated-agents mirror row.
fn family(repo: &Repo) {
    parent_recording(repo);
    child(
        repo,
        SCOUT,
        "Explore",
        one(SCOUT, "2026-01-01T10:00:03Z", SCOUTED),
    );
    child(
        repo,
        REVIEWER,
        "general-purpose",
        one(REVIEWER, "2026-01-01T10:00:05Z", REVIEWED),
    );
    child(
        repo,
        IDLE,
        "Explore",
        one(IDLE, "2026-01-01T10:00:07Z", IDLED),
    );
    subagent(
        repo,
        MIRROR,
        "delegated-agents:run",
        &format!("Mirror delegated run lane · fixture#low · lane\n\n{{\"handle\":\"{HANDLE}\",\"title\":\"lane\"}}"),
        &[("2026-01-01T10:00:08Z", None, usage(0, 0, 0, 0))],
    );
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

fn begin(repo: &Repo, slug: &str) -> String {
    opened_change_id(&stdout(lead(repo).args([
        "begin",
        slug,
        "--no-worktree",
        "--target",
        "master",
    ])))
}

/// The lead dispatches a run for `change` at `at`, before it spawns the
/// child, as a lead records one by hand.
fn dispatch(repo: &Repo, change: &str, at: &str) -> String {
    let id = event_id(&stdout(lead(repo).args([
        "run",
        "dispatch",
        "--route",
        "claude-code subagent: fixture-child#high",
        "--worktree",
        "/work/run",
        "--change",
        change,
    ])));
    retime(repo, &id, at);
    id
}

fn end(repo: &Repo, dispatch: &str, at: &str) {
    let id = event_id(&stdout(lead(repo).args([
        "run",
        "end",
        dispatch,
        "--outcome",
        "completed",
    ])));
    retime(repo, &id, at);
}

fn bind(
    repo: &Repo,
    dispatch: &str,
    harness: &str,
    child: &str,
    parent: &str,
    extra: &[&str],
) -> AssertCommand {
    let mut command = lead(repo);
    command.args([
        "run",
        "bind",
        dispatch,
        "--executor-harness",
        harness,
        "--executor-session",
        child,
        "--parent-session",
        parent,
    ]);
    command.args(extra);
    command
}

fn report(repo: &Repo, targets: &[&str]) -> Value {
    let report = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--json"])
            .args(targets),
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

fn has_session(report: &Value, key: &str) -> bool {
    report["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["key"] == key)
}

/// A figure's disjoint token total: its amount, or its known subtotal when a
/// qualification keeps it from being complete.
fn tokens(totals: &Value) -> Option<u64> {
    let quantity = totals["quantities"].as_array()?.iter().find(|quantity| {
        quantity["category"] == "total" && quantity["kind"] == "resource-quantity"
    })?;
    quantity["amount"]
        .as_u64()
        .or_else(|| quantity["known_subtotal"].as_u64())
}

/// A figure's disjoint output tokens, which a Codex token count records
/// beside a cache-write category it leaves absent.
fn output(totals: &Value) -> Option<u64> {
    let quantity = totals["quantities"].as_array()?.iter().find(|quantity| {
        quantity["category"] == "output" && quantity["categories"] == "disjoint"
    })?;
    quantity["amount"]
        .as_u64()
        .or_else(|| quantity["known_subtotal"].as_u64())
}

fn child_key(agent: &str) -> String {
    format!("claude:{PARENT}::{agent}")
}

/// Claim: two native children of one parent, bound to two different changes,
/// are executor rows measured from their own recordings, each in its own
/// change, with the role its binding names; the parent's figure is its own
/// requests alone; the unbound child is listed under the parent outside every
/// total; and the parent's own requests and each child's add up to the
/// family's identified union. Rejects counting the parent's recorded total,
/// which already holds its children, beside the child rows.
#[test]
fn two_bound_native_children_have_separate_figures_and_conserve_the_family() {
    let repo = Repo::new();
    family(&repo);
    let scouting = begin(&repo, "scouting");
    let reviewing = begin(&repo, "reviewing");
    let first = dispatch(&repo, &scouting, "2026-01-01T09:59:00Z");
    let second = dispatch(&repo, &reviewing, "2026-01-01T09:59:30Z");
    let bound = stdout(&mut bind(&repo, &first, "claude", SCOUT, PARENT, &[]));
    assert!(
        bound.contains(&format!(
            "run bound: claude child {SCOUT} of session {PARENT}"
        )),
        "{bound}"
    );
    stdout(&mut bind(
        &repo,
        &second,
        "claude",
        REVIEWER,
        PARENT,
        &["--executor-role", "reviewer"],
    ));
    end(&repo, &first, "2026-01-01T10:00:04Z");
    end(&repo, &second, "2026-01-01T10:00:06Z");

    let scout = report(&repo, &[&scouting]);
    let row = session(&scout, &child_key(SCOUT));
    assert_eq!(row["kind"], "executor");
    assert_eq!(row["roles"], json!(["implementer"]));
    assert_eq!(row["parent"], format!("claude:{PARENT}"));
    assert_eq!(
        row["agent"], "claude:fixture-child",
        "the child's own model evidence names its agent"
    );
    assert_eq!(tokens(&row["totals"]), Some(of(SCOUTED)));
    assert!(
        !has_session(&scout, &child_key(REVIEWER)),
        "a child bound to another change is not this one's"
    );

    let both = report(&repo, &[&scouting, &reviewing]);
    let reviewer = session(&both, &child_key(REVIEWER));
    assert_eq!(reviewer["roles"], json!(["reviewer"]));
    assert_eq!(tokens(&reviewer["totals"]), Some(of(REVIEWED)));
    let parent = session(&both, &format!("claude:{PARENT}"));
    assert_eq!(parent["measurement"]["basis"], "summed-requests");
    assert_eq!(
        tokens(&parent["totals"]),
        Some(of(OWN)),
        "the parent's figure is its own requests"
    );
    assert_eq!(
        tokens(&both["union"]),
        Some(of(OWN) + of(SCOUTED) + of(REVIEWED)),
        "the union counts the parent's own requests and each bound child once: {:#}",
        both["union"]
    );

    let family = &parent["family"];
    assert_eq!(
        family["bound"],
        json!([child_key(SCOUT), child_key(REVIEWER)])
    );
    let listed: Vec<&str> = family["children"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|child| child["agent"].as_str())
        .collect();
    assert!(
        !listed.contains(&MIRROR),
        "a mirror row is never child consumption: {family:#}"
    );
    assert_eq!(family["unassigned"]["children"], 2, "{family:#}");
    assert_eq!(tokens(&family["unassigned"]["totals"]), Some(of(IDLED)));
    let reconciliation = &family["reconciliation"];
    assert_eq!(
        reconciliation["status"], "conserved",
        "{:#}",
        parent["measurement"]["residual"]
    );
    let output = reconciliation["residual"]
        .as_array()
        .unwrap()
        .iter()
        .find(|counter| counter["counter"] == "output")
        .unwrap();
    assert_eq!(
        output["recorded"], 120,
        "the recorded total stays visible as the parent's"
    );
    assert_eq!(
        output["identified"],
        OWN.1 + SCOUTED.1 + REVIEWED.1 + IDLED.1
    );

    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", &scouting, &reviewing]),
    );
    let short = &PARENT[..8];
    assert!(
        text.contains(&format!(
            "children {short}: 2 bound ({short}::{SCOUT}, {short}::{REVIEWER}); 2 unassigned"
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("family {short}: requests conserved;")),
        "{text}"
    );
}

/// Claim: a delegated-agents mirror row refuses binding and names the
/// agent-delegate run it mirrors, by the dispatch that binds that run's
/// handle. Rejects measuring the mirror's zero usage as a child's figure.
#[test]
fn a_mirror_row_refuses_binding_and_names_the_delegated_run() {
    let repo = Repo::new();
    family(&repo);
    let change = begin(&repo, "mirrored");
    let delegated = dispatch(&repo, &change, "2026-01-01T09:59:00Z");
    stdout(lead(&repo).args([
        "run",
        "bind",
        &delegated,
        "--executor-harness",
        "pi",
        "--executor-session",
        "019a0000-0000-7000-8000-0000000000d1",
        "--handle",
        HANDLE,
    ]));
    let native = dispatch(&repo, &change, "2026-01-01T09:59:10Z");
    let refused = bind(&repo, &native, "claude", MIRROR, PARENT, &[])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr).to_string();
    assert!(stderr.contains("delegated-agents mirror row"), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "agent-delegate run {HANDLE}, which dispatch {delegated} binds"
        )),
        "{stderr}"
    );
}

/// Claim: a coordinate the reader cannot resolve to exactly one recording
/// refuses with a fixed reason, as does a child whose recording is missing
/// though its parent's records name it; and a bound child whose recording
/// later disappears is an unmeasured row that leaves totals without an
/// amount, never zero. Rejects binding by a guess or counting a missing child
/// as nothing.
#[test]
fn unresolvable_and_missing_children_stay_explicit() {
    let repo = Repo::new();
    family(&repo);
    let change = begin(&repo, "explicit");
    let run = dispatch(&repo, &change, "2026-01-01T09:59:00Z");
    for (child, parent, reason) in [
        (
            "a1000000000000099",
            PARENT,
            "the parent session records no child with this agent id",
        ),
        (
            SCOUT,
            "019a0000-0000-7000-8000-0000000000ff",
            "the parent session does not resolve to exactly one recording",
        ),
        (GONE, PARENT, "the child's recording is not in the store"),
        (SCOUT, "unknown", "a placeholder is not an identity"),
    ] {
        let refused = bind(&repo, &run, "claude", child, parent, &[])
            .assert()
            .failure();
        let stderr = String::from_utf8_lossy(&refused.get_output().stderr).to_string();
        assert!(stderr.contains(reason), "{child} {parent}: {stderr}");
        assert!(
            !stderr.contains(child) || child == SCOUT,
            "refusals never echo the coordinate: {stderr}"
        );
    }

    stdout(&mut bind(&repo, &run, "claude", SCOUT, PARENT, &[]));
    end(&repo, &run, "2026-01-01T10:00:04Z");
    let listed = report(&repo, &[&change]);
    let gone = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|row| {
            row["family"]["children"]
                .as_array()?
                .iter()
                .find(|child| child["agent"] == GONE)
                .cloned()
        })
        .unwrap_or_else(|| panic!("the missing child is listed: {listed:#}"));
    assert_eq!(gone["recording"], "missing");
    assert_eq!(gone["reason"], "child recording missing");

    fs::remove_file(
        project(&repo)
            .join(PARENT)
            .join("subagents")
            .join(format!("agent-{SCOUT}.jsonl")),
    )
    .unwrap();
    let after = report(&repo, &[&change]);
    let row = session(&after, &child_key(SCOUT));
    assert_eq!(row["measurement"]["status"], "unmeasured");
    assert_eq!(
        row["measurement"]["reasons"],
        json!(["child recording missing"])
    );
    assert_eq!(
        tokens(&after["union"]),
        None,
        "a missing child is never a zero: {:#}",
        after["union"]
    );
}

/// Claim: a child resumed under a second dispatch for another change is bound
/// twice and divided by those runs' spans from the ledger: each change takes
/// the requests its run alone covers, a request between the runs belongs to
/// neither, and nothing counts twice. Rejects attributing by timestamp
/// proximity or counting a resumed child whole in both changes.
#[test]
fn a_resumed_child_bound_to_two_runs_is_divided_by_their_spans() {
    let repo = Repo::new();
    parent_recording(&repo);
    let resumed = "a1000000000000006";
    child(
        &repo,
        resumed,
        "general-purpose",
        vec![
            (
                "2026-01-01T10:10:00Z".to_string(),
                Some("req_first".to_string()),
                usage(1, 10, 100, 0),
            ),
            (
                "2026-01-01T10:20:00Z".to_string(),
                Some("req_between".to_string()),
                usage(2, 20, 200, 0),
            ),
            (
                "2026-01-01T10:30:00Z".to_string(),
                Some("req_resumed".to_string()),
                usage(3, 30, 300, 0),
            ),
        ],
    );
    let first = begin(&repo, "first-pass");
    let second = begin(&repo, "second-pass");
    let run_one = dispatch(&repo, &first, "2026-01-01T10:09:00Z");
    stdout(&mut bind(&repo, &run_one, "claude", resumed, PARENT, &[]));
    end(&repo, &run_one, "2026-01-01T10:15:00Z");
    let run_two = dispatch(&repo, &second, "2026-01-01T10:29:00Z");
    stdout(&mut bind(&repo, &run_two, "claude", resumed, PARENT, &[]));
    end(&repo, &run_two, "2026-01-01T10:35:00Z");

    let both = report(&repo, &[&first, &second]);
    let row = session(&both, &child_key(resumed));
    assert_eq!(row["split"]["status"], "divided", "{:#}", row["split"]);
    let share = |work: &str| {
        row["split"]["shares"]
            .as_array()
            .unwrap()
            .iter()
            .find(|share| share["work"] == format!("change:{work}"))
            .and_then(|share| tokens(&share["totals"]))
    };
    assert_eq!(share(&first), Some(111));
    assert_eq!(share(&second), Some(333));
    assert_eq!(tokens(&row["split"]["unattributed"]["totals"]), Some(222));
    assert_eq!(
        row["split"]["unattributed"]["reasons"],
        json!(["between items"])
    );
}

fn codex_rollout(repo: &Repo, id: &str, parent: Option<&str>, output: u64) {
    let input = 500;
    let mut meta = json!({"id": id, "session_id": id, "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project", "model_provider": "fixture"});
    if let Some(parent) = parent {
        meta["parent_thread_id"] = json!(parent);
        meta["source"] = json!({"subagent": {"thread_spawn": {"parent_thread_id": parent, "depth": 1, "agent_path": "/root/worker", "agent_nickname": "Worker", "agent_role": null}}});
        meta["thread_source"] = json!("subagent");
    }
    write_lines(
        &repo
            .home
            .join(".codex/sessions/2026/01/01")
            .join(format!("rollout-2026-01-01T10-00-00-{id}.jsonl")),
        &[
            json!({"timestamp": "2026-01-01T10:00:00Z", "type": "session_meta", "payload": meta}),
            json!({"timestamp": "2026-01-01T10:00:01Z", "type": "turn_context", "payload": {"cwd": "/fixture/project", "model": "fixture-model", "effort": "high"}}),
            json!({"timestamp": "2026-01-01T10:00:02Z", "type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Work."}]}}),
            json!({"timestamp": "2026-01-01T10:00:03Z", "type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": input, "cached_input_tokens": 100, "output_tokens": output, "reasoning_output_tokens": 10, "total_tokens": input + output},
                "last_token_usage": {"input_tokens": input, "cached_input_tokens": 100, "output_tokens": output, "reasoning_output_tokens": 10, "total_tokens": input + output},
                "model_context_window": 1000}, "rate_limits": null}}),
            json!({"timestamp": "2026-01-01T10:00:04Z", "type": "event_msg", "payload": {"type": "task_complete"}}),
        ],
    );
}

/// Claim: a Codex `thread_spawn` child binds by its own thread id with the
/// parent its recording names, and is measured as its own executor row
/// without joining its parent in an overlap group; a parent its recording
/// does not name refuses. Rejects binding a thread to a parent it never had,
/// and withholding the union over a spawn that copies no history.
#[test]
fn a_codex_thread_spawn_child_binds_to_the_parent_its_recording_names() {
    let repo = Repo::new();
    let lead_thread = "019a0000-0000-7000-8000-0000000000c1";
    let spawned = "019a0000-0000-7000-8000-0000000000c2";
    let stranger = "019a0000-0000-7000-8000-0000000000c3";
    codex_rollout(&repo, lead_thread, None, 40);
    codex_rollout(&repo, spawned, Some(lead_thread), 90);
    codex_rollout(&repo, stranger, None, 10);
    let change = begin(&repo, "spawned");
    // The Codex lead dispatches, so it is the orchestrator the view selects.
    let mut codex_lead = repo.arc(&repo.root);
    codex_lead
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", lead_thread);
    let run = event_id(&stdout(codex_lead.args([
        "run",
        "dispatch",
        "--route",
        "codex thread_spawn: fixture-model#high",
        "--worktree",
        "/work/run",
        "--change",
        &change,
    ])));
    retime(&repo, &run, "2026-01-01T09:59:00Z");
    let refused = bind(&repo, &run, "codex", spawned, stranger, &[])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr).to_string();
    assert!(
        stderr.contains("the child's recording names another parent"),
        "{stderr}"
    );
    stdout(&mut bind(&repo, &run, "codex", spawned, lead_thread, &[]));
    end(&repo, &run, "2026-01-01T10:00:05Z");

    let view = report(&repo, &[&change]);
    let row = session(&view, &format!("codex:{spawned}"));
    assert_eq!(row["parent"], format!("codex:{lead_thread}"));
    assert_eq!(output(&row["totals"]), Some(90), "{:#}", row["measurement"]);
    let parent = session(&view, &format!("codex:{lead_thread}"));
    assert_eq!(
        parent["family"]["bound"],
        json!([format!("codex:{spawned}")])
    );
    assert_eq!(
        output(&parent["totals"]),
        Some(40),
        "the parent's figure is its own recording"
    );
    assert_eq!(
        view["union"]["identity"]["groups"],
        json!([]),
        "{:#}",
        view["union"]["identity"]
    );
}

/// Claim: on a commit's cost view, a session bound as reviewer answers the
/// commit's `Reviewed-by` trailer and not its `Implemented-by`, and one bound
/// as implementer the reverse. Rejects reading every bound executor as the
/// implementer whatever role its binding names.
#[test]
fn a_reviewer_binding_answers_reviewed_by_and_not_implemented_by() {
    let repo = Repo::new();
    family(&repo);
    let change = opened_change_id(&stdout(lead(&repo).args(["begin", "reviewed"])));
    let worktree = repo.home.join(".worktrees").join("repo-reviewed");
    repo.commit(
        &worktree,
        "reviewed.txt",
        "x\n",
        "feat: reviewed\n\nImplemented-by: claude:fixture-child#high\nReviewed-by: claude:fixture-child#high",
    );
    let sha = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    stdout(
        lead(&repo)
            .current_dir(&worktree)
            .args(["snapshot", "reviewed"]),
    );
    let mut dispatch = lead(&repo);
    let run = event_id(&stdout(dispatch.args([
        "run",
        "dispatch",
        "--route",
        "fixture-child#high via claude subagent",
        "--worktree",
        "/work/run",
        "--change",
        &change,
    ])));
    retime(&repo, &run, "2026-01-01T09:59:00Z");
    stdout(&mut bind(
        &repo,
        &run,
        "claude",
        REVIEWER,
        PARENT,
        &["--executor-role", "reviewer"],
    ));
    end(&repo, &run, "2026-01-01T10:00:06Z");

    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--json", "--commit", &sha]),
    );
    let role = |trailer: &str| {
        view["roles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|role| role["trailer"] == trailer)
            .unwrap_or_else(|| panic!("no {trailer}: {view:#}"))
            .clone()
    };
    let keys = |role: &Value| -> Vec<String> {
        role["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|session| session["key"].as_str().map(str::to_string))
            .collect()
    };
    assert_eq!(keys(&role("Reviewed-by")), vec![child_key(REVIEWER)]);
    assert_eq!(role("Implemented-by")["status"], "no-session-recorded");
}
