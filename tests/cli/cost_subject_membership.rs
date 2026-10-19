//! Which work a session is part of, whether it acted itself or was recorded
//! on behalf of. One rule gives a session its membership and the changes
//! `--footprint` selects for it; a subject names a session when its
//! harness's reader locates the id. Every recording is written into the
//! fixture's own home, named as its harness names it.

use super::common::*;
use serde_json::{json, Value};

const LEAD: &str = "019a0000-0000-7000-8000-0000000000a1";
const REVIEWER: &str = "019a0000-0000-7000-8000-0000000000c1";
/// An OpenCode session id, shaped as OpenCode writes one.
const OPENCODE_REVIEWER: &str = "ses_0f8a3ec487ffeFixtureRevwr1";

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

/// A Pi recording with one request of `input` uncached tokens, served by
/// `lead-model`.
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
                "message": {"role": "assistant", "timestamp": 1767261602000u64, "provider": "fixture", "model": "lead-model",
                    "usage": {"input": input, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": input,
                        "cost": {"total": 0.01}},
                    "content": [{"type": "text", "text": "Done."}]}}),
        ],
    );
}

/// A Codex rollout with one turn per model in `models`, each recording
/// `input` more cumulative tokens.
fn codex_recording(repo: &Repo, id: &str, models: &[&str], input: u64) {
    let path = repo
        .home
        .join(".codex/sessions/2026/01/01")
        .join(format!("rollout-2026-01-01T10-00-00-{id}.jsonl"));
    let mut lines = vec![
        json!({"timestamp": "2026-01-01T10:00:00Z", "type": "session_meta", "payload": {"id": id, "session_id": id, "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project", "model_provider": "fixture"}}),
    ];
    for (turn, model) in models.iter().enumerate() {
        let total = input * (turn as u64 + 1);
        let usage = |amount: u64| {
            json!({"input_tokens": amount, "cached_input_tokens": 0, "output_tokens": 50,
                "reasoning_output_tokens": 10, "total_tokens": amount + 50})
        };
        let at = |second: usize| format!("2026-01-01T10:0{turn}:0{second}Z");
        lines.extend([
            json!({"timestamp": at(1), "type": "turn_context", "payload": {"cwd": "/fixture/project", "model": model, "effort": "high"}}),
            json!({"timestamp": at(2), "type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Review."}]}}),
            json!({"timestamp": at(3), "type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Reviewed."}]}}),
            json!({"timestamp": at(4), "type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": usage(total), "last_token_usage": usage(input), "model_context_window": 1000}, "rate_limits": null}}),
            json!({"timestamp": at(5), "type": "event_msg", "payload": {"type": "task_complete"}}),
        ]);
    }
    write_lines(&path, &lines);
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

/// The lead audits `change` with one finding of its own, and records the
/// finding's disposition on behalf of `subject`: the subject's only event
/// there is of a kind that names no role.
fn disposition_for(repo: &Repo, change: &str, subject: &str) {
    let findings = repo.home.join(format!("{change}-findings.json"));
    fs::write(
        &findings,
        json_file_bytes(&json!([{"blocking": false, "severity": "minor", "summary": "edge case"}])),
    )
    .unwrap();
    lead(repo, &repo.root)
        .args([
            "audit",
            change,
            "--verdict",
            "comment-only",
            "--findings-json",
        ])
        .arg(&findings)
        .assert()
        .success();
    let listed = json_stdout(
        repo.arc(&repo.root)
            .args(["findings", change, "--audit", "--format", "json"]),
    );
    let finding = listed["findings"][0]["id"].as_str().unwrap().to_string();
    lead(repo, &repo.root)
        .args([
            "resolve",
            change,
            &finding,
            "--status",
            "resolved",
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

fn work_keys(report: &Value) -> Vec<String> {
    report["work"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["key"].as_str().map(str::to_string))
        .collect()
}

/// Claim: a disposition the lead recorded on a reviewer's behalf makes that
/// change part of the reviewer's work in its footprint as in its
/// membership, so the footprint selects both changes and counts the
/// reviewer whole. Rejects a footprint that selects only changes holding a
/// role-bearing event, which leaves the reviewer `shared ×2` there and its
/// footprint figure withheld.
#[test]
fn a_disposition_recorded_on_behalf_of_a_reviewer_is_part_of_its_footprint() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    codex_recording(&repo, REVIEWER, &["reviewer-model"], 5000);
    let (audited, _) = integrated(&repo, "audited", "feat: audited");
    let (disposed, _) = integrated(&repo, "disposed", "feat: disposed");
    let subject = format!("codex:{REVIEWER}");
    audit_for(&repo, &audited, &subject);
    disposition_for(&repo, &disposed, &subject);

    let footprint = report(&repo, &["--footprint", &subject]);
    let keys = work_keys(&footprint);
    for change in [&audited, &disposed] {
        assert!(keys.contains(&format!("change:{change}")), "{keys:?}");
    }
    let row = &footprint["footprint"]["row"];
    assert_eq!(row["counted"], true, "{row:#}");
    assert!(
        !row["status"].as_str().unwrap().starts_with("shared"),
        "{row:#}"
    );
    let membership = &session(&footprint, &subject).unwrap()["membership"];
    assert_eq!(membership.as_array().unwrap().len(), 2, "{membership:#}");
}

/// Claim: a session's own event of a kind that names no role, a comment,
/// is part of its work in its footprint as in its membership, so the
/// footprint holds every item it is divided among. Rejects a footprint that
/// drops the commented change, which leaves the session's share there
/// outside the footprint's figure.
#[test]
fn a_session_s_own_event_of_any_kind_is_part_of_its_footprint() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    codex_recording(&repo, REVIEWER, &["reviewer-model"], 5000);
    let (audited, _) = integrated(&repo, "audited", "feat: audited");
    let (commented, _) = integrated(&repo, "commented", "feat: commented");
    acting(&repo, &repo.root, "codex", REVIEWER)
        .args(["audit", &audited, "--verdict", "approved"])
        .assert()
        .success();
    acting(&repo, &repo.root, "codex", REVIEWER)
        .args(["comment", &commented, "--body", "read it"])
        .assert()
        .success();

    let subject = format!("codex:{REVIEWER}");
    let footprint = report(&repo, &["--footprint", &subject]);
    let keys = work_keys(&footprint);
    for change in [&audited, &commented] {
        assert!(keys.contains(&format!("change:{change}")), "{keys:?}");
    }
    let row = &footprint["footprint"]["row"];
    assert_eq!(row["status"], "split ×2", "{row:#}");
    assert_eq!(row["share"], true, "{row:#}");
}

/// A stable OpenCode store tapes reads through `sqlite3`: the fixture's
/// reader answers an exact lookup of `id` alone, and refuses every message
/// read. Returns the `PATH` and `XDG_DATA_HOME` that select it.
fn opencode_store(repo: &Repo, id: &str) -> (String, PathBuf) {
    let data_home = repo.home.join("data");
    let store = data_home.join("opencode/opencode.db");
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    fs::write(&store, b"SQLite format 3\0").unwrap();
    let bin = repo.home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let sqlite = bin.join("sqlite3");
    fs::write(
        &sqlite,
        format!(
            "#!/bin/sh\n\
             case \"$*\" in *'FROM message'*) echo 'message table unreadable' >&2; exit 1;; esac\n\
             printf '%s\\n' 'row'\n\
             case \"$*\" in *\"'{id}'\"*) printf '%s\\n' '{{\"id\":\"{id}\",\"model\":null}}';; esac\n"
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&sqlite, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (path, data_home)
}

/// Claim: an OpenCode reviewer recorded on behalf of, `opencode:ses_…`, is
/// a participant: its harness's reader locates the id. A subject naming an
/// OpenCode model adds none. Rejects reading only UUID-shaped ids as
/// sessions, which drops the OpenCode reviewer from the change view.
#[test]
fn an_opencode_subject_its_reader_locates_is_a_participant() {
    let repo = Repo::new();
    pi_recording(&repo, LEAD, 7);
    let (change, _) = integrated(&repo, "opencode", "feat: opencode");
    let subject = format!("opencode:{OPENCODE_REVIEWER}");
    audit_for(&repo, &change, &subject);
    audit_for(&repo, &change, "opencode:gpt-5.6-luna#max");
    let (path, data_home) = opencode_store(&repo, OPENCODE_REVIEWER);

    let view = json_stdout(
        repo.arc(&repo.root)
            .env("PATH", path)
            .env("XDG_DATA_HOME", data_home)
            .args(["cost", "show", "--json", &change]),
    );
    let row = session(&view, &subject).unwrap_or_else(|| panic!("no reviewer row: {view:#}"));
    assert_eq!(row["kind"], "participant", "{row:#}");
    assert_eq!(row["roles"], json!(["reviewer"]), "{row:#}");
    let mut keys: Vec<&str> = view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["key"].as_str())
        .collect();
    keys.sort_unstable();
    let lead_key = format!("pi:{LEAD}");
    assert_eq!(keys, vec![subject.as_str(), lead_key.as_str()], "{view:#}");
}
