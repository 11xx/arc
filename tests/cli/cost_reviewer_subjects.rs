//! Every reviewer a ledger records is found. A subject recorded as
//! `harness:session:model#effort (label)` is the session it names, with the
//! model it declares; a bound session's own verdicts give it the reviewer
//! role beside the role its binding names.
//! Every recording is written into the fixture's own home, named as its
//! harness names it.

use super::common::*;
use serde_json::{json, Value};

const LEAD: &str = "019a0000-0000-7000-8000-0000000000a1";
const REVIEWER: &str = "019a0000-0000-7000-8000-0000000000c1";
const EXECUTOR: &str = "019a0000-0000-4000-8000-0000000000e1";

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

/// A Pi recording with one request of `input` uncached tokens, served by
/// `served-model`.
fn pi_recording(repo: &Repo, id: &str, input: u64) {
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
                "message": {"role": "assistant", "timestamp": 1767261602000u64, "provider": "fixture", "model": "served-model",
                    "usage": {"input": input, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": input,
                        "cost": {"total": 0.01}},
                    "content": [{"type": "text", "text": "Done."}]}}),
        ],
    );
}

/// A Codex rollout with one turn served by `model`, recording `input`
/// uncached tokens.
fn codex_recording(repo: &Repo, id: &str, model: &str, input: u64) {
    let path = repo
        .home
        .join(".codex/sessions/2026/01/01")
        .join(format!("rollout-2026-01-01T10-00-00-{id}.jsonl"));
    let usage = json!({"input_tokens": input, "cached_input_tokens": 0, "output_tokens": 50,
        "reasoning_output_tokens": 10, "total_tokens": input + 50});
    write_lines(
        &path,
        &[
            json!({"timestamp": "2026-01-01T10:00:00Z", "type": "session_meta", "payload": {"id": id, "session_id": id, "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project", "model_provider": "fixture"}}),
            json!({"timestamp": "2026-01-01T10:00:01Z", "type": "turn_context", "payload": {"cwd": "/fixture/project", "model": model, "effort": "high"}}),
            json!({"timestamp": "2026-01-01T10:00:02Z", "type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Work."}]}}),
            json!({"timestamp": "2026-01-01T10:00:03Z", "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Done."}]}}),
            json!({"timestamp": "2026-01-01T10:00:04Z", "type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": usage, "last_token_usage": usage, "model_context_window": 1000}, "rate_limits": null}}),
            json!({"timestamp": "2026-01-01T10:00:05Z", "type": "event_msg", "payload": {"type": "task_complete"}}),
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

fn acting(repo: &Repo, cwd: &Path, harness: &str, session: &str) -> AssertCommand {
    let mut command = repo.arc(cwd);
    command
        .env("ARC_HARNESS", harness)
        .env("ARC_SESSION", session)
        .env_remove("ARC_ACTOR");
    command
}

fn lead(repo: &Repo, cwd: &Path) -> AssertCommand {
    acting(repo, cwd, "pi", LEAD)
}

/// A change the lead opened and snapshotted, with `message` as its one
/// commit's message. Returns the change and its worktree.
fn snapshotted(repo: &Repo, slug: &str, message: &str) -> (String, PathBuf) {
    let change = opened_change_id(&stdout(lead(repo, &repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&worktree, &format!("{slug}.txt"), "x\n", message);
    stdout(lead(repo, &worktree).args(["snapshot", slug]));
    (change, worktree)
}

/// The lead approves and integrates `slug`.
fn integrate(repo: &Repo, worktree: &Path, slug: &str) {
    lead(repo, worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    lead(repo, &repo.root)
        .args(["integrate", slug])
        .assert()
        .success();
}

/// The lead records the approving audit `subject` performed on `change`.
fn audit_for(repo: &Repo, change: &str, subject: &str) {
    lead(repo, &repo.root)
        .args([
            "audit",
            change,
            "--verdict",
            "approved",
            "--on-behalf-of",
            subject,
        ])
        .assert()
        .success();
}

/// The lead dispatches `change` through `route` and binds the run to the
/// Codex `EXECUTOR` as its implementer. Returns the dispatch.
fn bind_implementer(repo: &Repo, change: &str, route: &str) -> String {
    let dispatch = event_id(&stdout(lead(repo, &repo.root).args([
        "run",
        "dispatch",
        "--route",
        route,
        "--worktree",
        "/work/run",
        "--change",
        change,
    ])));
    stdout(repo.arc(&repo.root).args([
        "run",
        "bind",
        &dispatch,
        "--executor-harness",
        "codex",
        "--executor-session",
        EXECUTOR,
    ]));
    dispatch
}

fn end_run(repo: &Repo, dispatch: &str) {
    stdout_any_status(repo.arc(&repo.root).args([
        "run",
        "end",
        dispatch,
        "--outcome",
        "completed",
    ]));
}

fn report(repo: &Repo, change: &str) -> Value {
    let report = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--json", change]),
    );
    assert_eq!(report["schema"], "arc-cost/1", "{report}");
    report
}

fn session<'a>(report: &'a Value, key: &str) -> Option<&'a Value> {
    report["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == key)
}

fn commit_view(repo: &Repo, worktree: &Path) -> Value {
    let commit = git_out(worktree, &["rev-parse", "HEAD"]);
    json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--commit", &commit, "--json"]),
    )
}

fn commit_role<'a>(view: &'a Value, trailer: &str) -> &'a Value {
    view["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["trailer"] == trailer)
        .unwrap_or_else(|| panic!("no {trailer} row: {view:#}"))
}

/// Claim: a subject recorded as `pi:<session>:<model>#<effort> (<label>)`
/// is the Pi session it names: a participant measured from its recording,
/// attributed to the model the subject declares, and the session a
/// `Reviewed-by` trailer naming that model finds. Rejects splitting the
/// subject at its first colon only, which reads the model and label as part
/// of the id, locates nothing, and leaves the reviewer and its consumption
/// out of the view and the trailer at `no session recorded`.
#[test]
fn a_subject_with_a_model_suffix_is_the_session_it_names() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    pi_recording(&repo, REVIEWER, 900);
    let (change, worktree) = snapshotted(
        &repo,
        "suffixed",
        "feat: suffixed\n\nReviewed-by: pi:declared-model#medium",
    );
    integrate(&repo, &worktree, "suffixed");
    audit_for(
        &repo,
        &change,
        &format!("pi:{REVIEWER}:declared-model#medium (correctness reviewer)"),
    );

    let view = report(&repo, &change);
    let key = format!("pi:{REVIEWER}");
    let row = session(&view, &key).unwrap_or_else(|| panic!("no reviewer row: {view:#}"));
    assert_eq!(row["kind"], "participant", "{row:#}");
    assert_eq!(row["roles"], json!(["reviewer"]), "{row:#}");
    assert_eq!(row["measurement"]["source"], "fresh", "{row:#}");
    assert_eq!(row["agent"], "pi:declared-model#medium", "{row:#}");
    assert_eq!(row["agent_source"], "events", "{row:#}");

    let commit = commit_view(&repo, &worktree);
    let reviewed = commit_role(&commit, "Reviewed-by");
    assert_eq!(reviewed["status"], "recorded", "{reviewed:#}");
    let sessions = reviewed["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{reviewed:#}");
    assert_eq!(sessions[0]["key"], key, "{reviewed:#}");
    assert_eq!(sessions[0]["model_source"], "events", "{reviewed:#}");
}

/// Claim: an executor a dispatch bound as implementer that recorded a
/// verdict of its own holds the reviewer role beside its implementer role,
/// in the table and for a `Reviewed-by` trailer naming the model its verdict
/// declared. Rejects taking an executor's roles from its binding alone,
/// which lists it as `implementer` only and reads that trailer as `no
/// session recorded`.
#[test]
fn a_bound_implementer_s_own_verdict_answers_a_reviewed_by_trailer() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    codex_recording(&repo, EXECUTOR, "impl-model", 5000);
    let (change, worktree) = snapshotted(
        &repo,
        "self-reviewed",
        "feat: self-reviewed\n\nReviewed-by: codex:review-model#xhigh",
    );
    let dispatch = bind_implementer(&repo, &change, "codex:impl-model#high");
    acting(&repo, &worktree, "codex", EXECUTOR)
        .args([
            "review",
            "self-reviewed",
            "--verdict",
            "approved",
            "--model",
            "review-model#xhigh",
        ])
        .assert()
        .success();
    end_run(&repo, &dispatch);
    integrate(&repo, &worktree, "self-reviewed");

    let view = report(&repo, &change);
    let key = format!("codex:{EXECUTOR}");
    let row = session(&view, &key).unwrap_or_else(|| panic!("no executor row: {view:#}"));
    assert_eq!(row["kind"], "executor", "{row:#}");
    assert_eq!(row["roles"], json!(["implementer", "reviewer"]), "{row:#}");
    let text = stdout(repo.arc(&repo.root).args(["cost", "show", &change]));
    assert!(
        text.lines()
            .any(|line| line.contains("impl,review") && line.contains(&EXECUTOR[..8])),
        "{text}"
    );

    let commit = commit_view(&repo, &worktree);
    let reviewed = commit_role(&commit, "Reviewed-by");
    assert_eq!(reviewed["status"], "recorded", "{reviewed:#}");
    let sessions = reviewed["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{reviewed:#}");
    assert_eq!(sessions[0]["key"], key, "{reviewed:#}");
    assert_eq!(sessions[0]["kind"], "executor", "{reviewed:#}");
    assert_eq!(sessions[0]["model_source"], "events", "{reviewed:#}");
}

/// Claim: a bound implementer's own snapshot is the work its binding gave
/// it and makes it no reviewer, so a `Reviewed-by` trailer naming its model
/// finds nobody. Rejects reading a snapshot as a reviewer's for a bound
/// session, which credits the implementer with reviewing its own patchset.
#[test]
fn a_bound_implementer_s_own_snapshot_makes_it_no_reviewer() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    codex_recording(&repo, EXECUTOR, "impl-model", 5000);
    let change = opened_change_id(&stdout(
        lead(&repo, &repo.root).args(["begin", "own-snapshot"]),
    ));
    let worktree = repo.home.join(".worktrees/repo-own-snapshot");
    repo.commit(
        &worktree,
        "own-snapshot.txt",
        "x\n",
        "feat: own snapshot\n\nReviewed-by: codex:impl-model#high",
    );
    let dispatch = bind_implementer(&repo, &change, "codex:impl-model#high");
    acting(&repo, &worktree, "codex", EXECUTOR)
        .args(["snapshot", "own-snapshot", "--model", "impl-model#high"])
        .assert()
        .success();
    end_run(&repo, &dispatch);
    integrate(&repo, &worktree, "own-snapshot");

    let view = report(&repo, &change);
    let row = session(&view, &format!("codex:{EXECUTOR}")).unwrap();
    assert_eq!(row["roles"], json!(["implementer"]), "{row:#}");
    let commit = commit_view(&repo, &worktree);
    let reviewed = commit_role(&commit, "Reviewed-by");
    assert_eq!(reviewed["status"], "no-session-recorded", "{reviewed:#}");
    assert!(reviewed.get("unestablished").is_none(), "{reviewed:#}");
}
