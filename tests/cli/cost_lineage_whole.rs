//! A cost view reads each session's lineage from its whole recording, so a
//! recording larger than tapes' bounded read window keeps its identity, and
//! `--footprint` names a bound native child. Pi recordings are named as Pi
//! names them, `<timestamp>_<uuid>.jsonl`, and a forked one names its origin
//! by path in its header's `parentSession`; a Claude subagent sits at
//! `<project>/<parent>/subagents/agent-<id>.jsonl`.

use super::common::*;
use serde_json::{json, Value};

const LARGE: &str = "019a0000-0000-7000-8000-0000000000c1";
const SMALL: &str = "019a0000-0000-7000-8000-0000000000c2";
const ORIGIN: &str = "019a0000-0000-7000-8000-0000000000c3";
/// Past tapes' default 4 MiB bounded read window.
const PADDING: usize = 5 * 1024 * 1024;

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

fn pi_path(repo: &Repo, id: &str) -> PathBuf {
    repo.home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T10-00-00-000Z_{id}.jsonl"))
}

/// A Pi recording with one priced request of `input` tokens, a user prompt
/// of `padding` bytes before it, and, when `origin` is given, the header of
/// a session forked from that recording.
fn pi_recording(repo: &Repo, id: &str, input: u64, padding: usize, origin: Option<&str>) {
    let mut header = json!({"type": "session", "version": 3, "id": id, "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project"});
    if let Some(origin) = origin {
        header["parentSession"] = json!(pi_path(repo, origin).display().to_string());
    }
    let prompt = "x".repeat(padding.max(5));
    write_lines(
        &pi_path(repo, id),
        &[
            header,
            json!({"type": "message", "id": "user-1", "parentId": null, "timestamp": "2026-01-01T10:00:01Z",
                "message": {"role": "user", "timestamp": 1767261601000u64, "content": [{"type": "text", "text": prompt}]}}),
            json!({"type": "message", "id": "assistant-1", "parentId": "user-1", "timestamp": "2026-01-01T10:00:02Z",
                "message": {"role": "assistant", "timestamp": 1767261602000u64, "provider": "fixture", "model": "fixture-model",
                    "usage": {"input": input, "output": 20, "cacheRead": 0, "cacheWrite": 0, "totalTokens": input + 20,
                        "cost": {"total": input as f64 / 1000.0}},
                    "content": [{"type": "text", "text": "Done."}]}}),
        ],
    );
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

/// A change with one ended run bound to each Pi session in `executors`.
fn executed_change(repo: &Repo, slug: &str, executors: &[&str]) -> String {
    let change = opened_change_id(&stdout(anonymous(repo).args([
        "begin",
        slug,
        "--no-worktree",
        "--target",
        "master",
    ])));
    for executor in executors {
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
        retime(repo, &dispatch, "2026-01-01T09:59:00Z");
        stdout(anonymous(repo).args([
            "run",
            "bind",
            &dispatch,
            "--executor-harness",
            "pi",
            "--executor-session",
            executor,
        ]));
        let ended = event_id(&stdout(anonymous(repo).args([
            "run",
            "end",
            &dispatch,
            "--outcome",
            "completed",
        ])));
        retime(repo, &ended, "2026-01-01T10:10:00Z");
    }
    change
}

fn report(repo: &Repo, args: &[&str]) -> Value {
    json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--json"])
            .args(args),
    )
}

fn native_input(totals: &Value) -> &Value {
    totals["quantities"]
        .as_array()
        .unwrap_or_else(|| panic!("no quantities: {totals}"))
        .iter()
        .find(|quantity| {
            quantity.get("partition").is_none()
                && quantity["categories"] == "native"
                && quantity["category"] == "input"
        })
        .unwrap_or_else(|| panic!("no native input: {totals}"))
}

/// Claim: a recording past the bounded read window with no fork or copy
/// evidence leaves identity established, so a union holding it and another
/// session is exact. Rejects reading identity as unknown, and withholding
/// the union behind overlap bounds, because the read was bounded.
#[test]
fn a_large_recording_without_fork_evidence_keeps_the_union_exact() {
    let repo = Repo::new();
    pi_recording(&repo, LARGE, 100, PADDING, None);
    pi_recording(&repo, SMALL, 10, 0, None);
    let change = executed_change(&repo, "large", &[LARGE, SMALL]);
    let view = report(&repo, &[&change]);
    let identity = &view["union"]["identity"];
    assert_eq!(identity["status"], "complete", "{identity:#}");
    let input = native_input(&view["union"]);
    assert_eq!(input["status"], "complete", "{input:#}");
    assert_eq!(input["amount"], 110, "{input:#}");
}

/// Claim: a recording past the bounded read window that names the session
/// it was forked from forms an overlap group with that origin alone, so an
/// unrelated session stays outside it. Rejects one group of every session,
/// as unknown lineage from a bounded read gives, and losing the fork's
/// group.
#[test]
fn a_large_fork_groups_with_its_origin_alone() {
    let repo = Repo::new();
    pi_recording(&repo, ORIGIN, 10, 0, None);
    pi_recording(&repo, LARGE, 100, PADDING, Some(ORIGIN));
    pi_recording(&repo, SMALL, 1, 0, None);
    let change = executed_change(&repo, "fork", &[ORIGIN, LARGE, SMALL]);
    let view = report(&repo, &[&change]);
    let identity = &view["union"]["identity"];
    assert_eq!(
        identity["groups"],
        json!([[format!("pi:{LARGE}"), format!("pi:{ORIGIN}")]]),
        "{identity:#}"
    );
    let input = native_input(&view["union"]);
    assert_ne!(input["status"], "complete", "{input:#}");
    assert_eq!(input["bound"]["at_least"], 101, "{input:#}");
    assert_eq!(input["bound"]["at_most"], 111, "{input:#}");
}

const PARENT: &str = "019a0000-0000-7000-8000-0000000000d1";
const CHILD: &str = "d1000000000000001";
const UNBOUND: &str = "d1000000000000002";

/// A Claude parent and one subagent, written as Claude writes them.
fn claude_family(repo: &Repo) {
    let project = repo.home.join(".claude/projects/-fixture-project");
    let usage = json!({"input_tokens": 3, "output_tokens": 20, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0});
    write_lines(
        &project.join(format!("{PARENT}.jsonl")),
        &[
            json!({"type": "user", "sessionId": PARENT, "uuid": "user-1", "parentUuid": null, "timestamp": "2026-01-01T10:00:00Z",
                "cwd": "/fixture/project", "entrypoint": "cli", "message": {"role": "user", "content": "Lead."}}),
            json!({"type": "user", "sessionId": PARENT, "uuid": "launch-1", "parentUuid": "user-1", "timestamp": "2026-01-01T10:00:02Z",
                "cwd": "/fixture/project",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "launched"}]},
                "toolUseResult": {"isAsync": true, "status": "async_launched", "agentId": CHILD, "description": "Work"}}),
        ],
    );
    let directory = project.join(PARENT).join("subagents");
    write_lines(
        &directory.join(format!("agent-{CHILD}.jsonl")),
        &[
            json!({"type": "user", "sessionId": PARENT, "agentId": CHILD, "isSidechain": true, "uuid": "child-user", "parentUuid": null,
                "timestamp": "2026-01-01T10:00:03Z", "cwd": "/fixture/project", "message": {"role": "user", "content": "Work."}}),
            json!({"type": "assistant", "sessionId": PARENT, "agentId": CHILD, "isSidechain": true, "uuid": "child-assistant", "parentUuid": "child-user",
                "requestId": "req_child", "timestamp": "2026-01-01T10:00:04Z", "cwd": "/fixture/project",
                "message": {"id": "msg_child", "role": "assistant", "model": "fixture-child", "content": [{"type": "text", "text": "Done."}], "usage": usage}}),
        ],
    );
    fs::write(
        directory.join(format!("agent-{CHILD}.meta.json")),
        json!({"agentType": "general-purpose", "description": "Work", "spawnDepth": 1}).to_string(),
    )
    .unwrap();
}

/// Claim: `--footprint claude:<parent>::<agent>` for a native child a
/// binding records selects the change that binding names and shows the
/// child's row, and a child key no binding records is refused naming it.
/// Rejects refusing the native child key as malformed.
#[test]
fn footprint_of_a_bound_native_child_selects_its_work() {
    let repo = Repo::new();
    claude_family(&repo);
    let change = opened_change_id(&stdout(anonymous(&repo).args([
        "begin",
        "served",
        "--no-worktree",
        "--target",
        "master",
    ])));
    opened_change_id(&stdout(anonymous(&repo).args([
        "begin",
        "elsewhere",
        "--no-worktree",
        "--target",
        "master",
    ])));
    let dispatch = event_id(&stdout(anonymous(&repo).args([
        "run",
        "dispatch",
        "--route",
        "claude-code subagent: fixture-child#high",
        "--worktree",
        "/work/run",
        "--change",
        &change,
    ])));
    retime(&repo, &dispatch, "2026-01-01T09:59:00Z");
    stdout(anonymous(&repo).args([
        "run",
        "bind",
        &dispatch,
        "--executor-harness",
        "claude",
        "--executor-session",
        CHILD,
        "--parent-session",
        PARENT,
    ]));

    let key = format!("claude:{PARENT}::{CHILD}");
    let view = report(&repo, &["--footprint", &key]);
    let work: Vec<&str> = view["work"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert_eq!(work, [change.as_str()], "{view:#}");
    assert_eq!(view["footprint"]["session"], key, "{view:#}");
    assert_eq!(view["footprint"]["row"]["key"], key, "{view:#}");

    let refused = repo
        .arc(&repo.root)
        .args([
            "cost",
            "show",
            "--footprint",
            &format!("claude:{PARENT}::{UNBOUND}"),
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr);
    assert!(
        stderr.contains(&format!("{PARENT}::{UNBOUND}")) && stderr.contains("no binding records"),
        "{stderr}"
    );
}
