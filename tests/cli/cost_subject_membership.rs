//! Which sessions an event names. An event recorded on behalf of a
//! `harness:id` subject names a session when its harness's reader locates
//! the id, whatever shape that harness gives its ids. Every recording is
//! written into the fixture's own home, named as its harness names it.

use super::common::*;
use serde_json::{json, Value};

const LEAD: &str = "019a0000-0000-7000-8000-0000000000a1";
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

fn session<'a>(report: &'a Value, key: &str) -> Option<&'a Value> {
    report["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == key)
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
