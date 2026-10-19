//! Sessions a lead records ceremony for, and the agent a coordinate names.
//! A reviewer whose verdict the lead records with `--on-behalf-of` is a
//! participant of the work it reviewed, divided by none of the lead's
//! instants; and a route or declared model that carries its harness as a
//! prefix names that harness once in the agent cell. Every recording is
//! written into the fixture's own home, named as its harness names it.

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

/// A Pi recording with one request of `input` uncached tokens.
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
                "message": {"role": "assistant", "timestamp": 1767261602000u64, "provider": "fixture", "model": "fixture-model",
                    "usage": {"input": input, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": input,
                        "cost": {"total": 0.01}},
                    "content": [{"type": "text", "text": "Done."}]}}),
        ],
    );
}

/// A Codex rollout whose cumulative counter records `input` tokens.
fn codex_recording(repo: &Repo, id: &str, input: u64) {
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
            json!({"timestamp": "2026-01-01T10:00:01Z", "type": "turn_context", "payload": {"cwd": "/fixture/project", "model": "fixture-model", "effort": "high"}}),
            json!({"timestamp": "2026-01-01T10:00:02Z", "type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Review."}]}}),
            json!({"timestamp": "2026-01-01T10:00:03Z", "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Reviewed."}]}}),
            json!({"timestamp": "2026-01-01T10:00:04Z", "type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": usage, "last_token_usage": usage, "model_context_window": 1000}, "rate_limits": null}}),
            json!({"timestamp": "2026-01-01T10:00:05Z", "type": "event_msg", "payload": {"type": "task_complete"}}),
        ],
    );
}

/// A Claude recording with one request of `input` uncached tokens.
fn claude_recording(repo: &Repo, id: &str, input: u64) {
    let path = repo
        .home
        .join(".claude/projects/-fixture-project")
        .join(format!("{id}.jsonl"));
    write_lines(
        &path,
        &[
            json!({"type": "user", "sessionId": id, "uuid": "prompt-1", "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project", "message": {"role": "user", "content": "Work."}}),
            json!({"type": "assistant", "sessionId": id, "uuid": "assistant-1", "requestId": "request-1", "timestamp": "2026-01-01T10:00:01Z", "cwd": "/fixture/project",
                "message": {"role": "assistant", "model": "claude-opus-5-5", "content": [{"type": "text", "text": "Done."}],
                    "usage": {"input_tokens": input, "output_tokens": 30, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}}}),
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

/// A change the lead opened, snapshotted, approved and integrated, with
/// `message` as its one commit's message. Returns the change and its worktree.
fn integrated(repo: &Repo, slug: &str, message: &str) -> (String, PathBuf) {
    let change = opened_change_id(&stdout(lead(repo, &repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&worktree, &format!("{slug}.txt"), "x\n", message);
    stdout(lead(repo, &worktree).args(["snapshot", slug]));
    lead(repo, &worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    lead(repo, &repo.root)
        .args(["integrate", slug])
        .assert()
        .success();
    (change, worktree)
}

/// The lead records the audit `subject` performed on `change`.
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

fn report(repo: &Repo, args: &[&str]) -> Value {
    let report = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--json"])
            .args(args),
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

fn table_row<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["table"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == key)
        .unwrap_or_else(|| panic!("no table row {key}: {report:#}"))
}

fn work<'a>(report: &'a Value, change: &str) -> &'a Value {
    report["work"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == format!("change:{change}"))
        .unwrap_or_else(|| panic!("no work item {change}: {report:#}"))
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

fn mentions(value: &Value, needle: &str) -> bool {
    value.to_string().contains(needle)
}

/// Claim: a codex session whose audits of two changes the lead recorded on
/// its behalf is a reviewer participant of each change, shown in each
/// change's view with its share there withheld rather than dropped, and
/// counted once, whole, in their union; the lead keeps its own roles.
/// Rejects a view that measures only the acting session, which lists no
/// reviewer row and leaves its consumption out of every figure.
#[test]
fn a_reviewer_recorded_on_behalf_of_is_a_participant_of_each_change_it_reviewed() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    codex_recording(&repo, REVIEWER, 5000);
    let (first, _) = integrated(&repo, "first", "feat: first");
    let (second, _) = integrated(&repo, "second", "feat: second");
    let subject = format!("codex:{REVIEWER}");
    audit_for(&repo, &first, &subject);
    audit_for(&repo, &second, &subject);

    for change in [&first, &second] {
        let view = report(&repo, &[change]);
        let row = session(&view, &subject).unwrap_or_else(|| panic!("no reviewer row: {view:#}"));
        assert_eq!(row["roles"], json!(["reviewer"]), "{row:#}");
        let table = table_row(&view, &subject);
        assert_eq!(table["status"], "shared ×2", "{table:#}");
        assert_eq!(table["counted"], false, "{table:#}");
        let totals = &work(&view, change)["direct"];
        assert!(
            mentions(totals, &format!("session {subject} also acted on other work and is not divided (recorded on its behalf)")),
            "{totals:#}"
        );
        let lead = session(&view, &format!("pi:{LEAD}")).unwrap();
        assert_eq!(lead["roles"], json!(["integrator", "reviewer"]), "{lead:#}");
    }

    let union = report(&repo, &[&first, &second]);
    let table = table_row(&union, &subject);
    assert_eq!(table["counted"], true, "{table:#}");
    assert_eq!(table["spans"], 2, "{table:#}");
    let reviewers = union["table"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["key"] == subject)
        .count();
    assert_eq!(reviewers, 1, "{union:#}");
    let total = &union["union"];
    assert!(mentions(&total["sessions"], &subject), "{total:#}");
    assert!(!mentions(&total["shares"], &subject), "{total:#}");
    assert!(!mentions(total, "is not divided"), "{total:#}");
}

/// Claim: events recorded on a reviewer's behalf carry the lead's instants,
/// so they open no span of the reviewer's: one that also starts a review of
/// its own on one change is not divided by the lead's later recording of the
/// other. Rejects dividing the reviewer at the recording time, which hands
/// the second change a share measured from the lead's clock.
#[test]
fn events_recorded_on_behalf_of_a_session_open_no_span_of_its_own() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    codex_recording(&repo, REVIEWER, 5000);
    let (first, _) = integrated(&repo, "first", "feat: first");
    let (second, _) = integrated(&repo, "second", "feat: second");
    acting(&repo, &repo.root, "codex", REVIEWER)
        .args(["review", &first, "--start"])
        .assert()
        .success();
    audit_for(&repo, &second, &format!("codex:{REVIEWER}"));

    let view = report(&repo, &[&first, &second]);
    let subject = format!("codex:{REVIEWER}");
    let row = session(&view, &subject).unwrap_or_else(|| panic!("no reviewer row: {view:#}"));
    assert!(row["split"]["shares"].is_null(), "{row:#}");
    assert_eq!(table_row(&view, &subject)["counted"], true, "{view:#}");
    let alone = report(&repo, &[&second]);
    assert!(
        mentions(&work(&alone, &second)["direct"], "recorded on its behalf"),
        "{alone:#}"
    );
}

/// Claim: a reviewer recorded on behalf of for one change is counted whole
/// in that change's view, and `--footprint` on it selects the change.
/// Rejects a footprint that reads only the acting session, which selects
/// nothing and refuses.
#[test]
fn a_reviewer_of_one_change_counts_whole_there_and_its_footprint_selects_it() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    codex_recording(&repo, REVIEWER, 5000);
    let (change, _) = integrated(&repo, "only", "feat: only");
    let subject = format!("codex:{REVIEWER}");
    audit_for(&repo, &change, &subject);

    let view = report(&repo, &[&change]);
    assert_eq!(table_row(&view, &subject)["counted"], true, "{view:#}");
    let footprint = report(&repo, &["--footprint", &subject]);
    assert_eq!(
        footprint["footprint"]["session"], subject,
        "{:#}",
        footprint["footprint"]
    );
    assert!(work(&footprint, &change).is_object());
}

/// Claim: a subject that names a model or a party, not a session id, adds
/// no participant. Rejects reading every `harness:id` subject as a session,
/// which lists `claude:claude-opus-5-5` as an unmeasured row and withholds
/// the change's figure.
#[test]
fn a_subject_naming_a_model_or_a_party_adds_no_participant() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    let (change, _) = integrated(&repo, "named", "feat: named");
    audit_for(&repo, &change, "claude:claude-opus-5-5");
    audit_for(&repo, &change, "sol-reviewer");

    let view = report(&repo, &[&change]);
    let keys: Vec<&Value> = view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| &row["key"])
        .collect();
    assert_eq!(keys, vec![&json!(format!("pi:{LEAD}"))], "{view:#}");
    assert_eq!(
        uncached(&work(&view, &change)["direct"]),
        Some(7),
        "{view:#}"
    );
}

/// A change the lead dispatched through `route` to a Claude executor, bound
/// and ended, with `message` as its commit.
fn dispatched(repo: &Repo, slug: &str, route: &str, message: &str) -> (String, PathBuf) {
    let (change, worktree) = integrated(repo, slug, message);
    let dispatch = event_id(&stdout(lead(repo, &repo.root).args([
        "run",
        "dispatch",
        "--route",
        route,
        "--worktree",
        "/work/run",
        "--change",
        &change,
    ])));
    stdout(repo.arc(&repo.root).args([
        "run",
        "bind",
        &dispatch,
        "--executor-harness",
        "claude",
        "--executor-session",
        EXECUTOR,
    ]));
    stdout_any_status(repo.arc(&repo.root).args([
        "run",
        "end",
        &dispatch,
        "--outcome",
        "completed",
    ]));
    (change, worktree)
}

/// Claim: `run dispatch` records a route that names its harness as given,
/// and the agent cell names that harness once. Rejects joining the row's
/// harness to the route unexamined, `claude:claude:claude-opus-5-5#medium`.
#[test]
fn a_dispatch_route_naming_its_harness_names_it_once_in_the_agent_cell() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    claude_recording(&repo, EXECUTOR, 300);
    let route = "claude:claude-opus-5-5#medium";
    let (change, _) = dispatched(&repo, "routed", route, "feat: routed");

    let events = stdout(repo.arc(&repo.root).args(["events", "--repository"]));
    assert!(
        events.contains(&format!("\"route\":\"{route}\"")),
        "{events}"
    );
    let view = report(&repo, &[&change]);
    let row = table_row(&view, &format!("claude:{EXECUTOR}"));
    assert_eq!(row["agent"], route, "{row:#}");
    let text = stdout(repo.arc(&repo.root).args(["cost", "show", &change]));
    assert!(!text.contains("claude:claude:"), "{text}");
}

/// Claim: a commit's `Implemented-by` trailer finds the executor whose
/// route names the trailer's coordinate with its harness. Rejects comparing
/// the route's `claude:claude-opus-5-5` with the trailer's model
/// `claude-opus-5-5`, which answers `no session recorded`.
#[test]
fn an_implementer_trailer_finds_the_run_whose_route_names_its_harness() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    claude_recording(&repo, EXECUTOR, 300);
    let (_, worktree) = dispatched(
        &repo,
        "trailer",
        "claude:claude-opus-5-5#medium",
        "feat: trailer\n\nImplemented-by: claude:claude-opus-5-5#medium",
    );
    let commit = git_out(&worktree, &["rev-parse", "HEAD"]);
    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--commit", &commit, "--json"]),
    );
    let implemented = view["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["trailer"] == "Implemented-by")
        .unwrap_or_else(|| panic!("no Implemented-by row: {view:#}"));
    assert_eq!(
        implemented["sessions"][0]["key"],
        format!("claude:{EXECUTOR}"),
        "{implemented:#}"
    );
}

/// Claim: a participant whose own events already declared a model carrying
/// its harness, `claude:claude-opus-5-5#high`, is shown with that harness
/// once. Rejects `claude:claude:claude-opus-5-5#high` for a record already
/// in the ledger.
#[test]
fn a_declared_model_naming_its_harness_names_it_once_in_the_agent_cell() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    claude_recording(&repo, EXECUTOR, 300);
    let change = opened_change_id(&stdout(lead(&repo, &repo.root).args(["begin", "declared"])));
    let worktree = repo.home.join(".worktrees/repo-declared");
    repo.commit(&worktree, "declared.txt", "x\n", "feat: declared");
    stdout(lead(&repo, &worktree).args(["snapshot", "declared"]));
    acting(&repo, &repo.root, "claude", EXECUTOR)
        .env("ARC_MODEL", "claude:claude-opus-5-5#high")
        .args(["review", &change, "--verdict", "comment-only"])
        .assert()
        .success();
    let events = stdout(repo.arc(&repo.root).args(["events", "--change", &change]));
    assert!(
        events.contains("\"model\":\"claude:claude-opus-5-5#high\""),
        "{events}"
    );

    let view = report(&repo, &[&change]);
    let row = table_row(&view, &format!("claude:{EXECUTOR}"));
    assert_eq!(row["agent"], "claude:claude-opus-5-5#high", "{row:#}");
}
