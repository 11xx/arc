//! A session whose tapes read failed is unmeasured with a reason that says
//! whether reading again may measure it: a store tapes found busy through
//! every bounded attempt failed transiently, and any other failure failed
//! for good. Every recording is written into the fixture's own home, named
//! as its harness names it.

use super::common::*;
use serde_json::{json, Value};

const LEAD: &str = "019a0000-0000-7000-8000-0000000000a1";
/// An OpenCode session id, shaped as OpenCode writes one.
const REVIEWER: &str = "ses_0f8a3ec487ffeFixtureRevwr2";
const TRANSIENT: &str = "usage read failed transiently; a later read may succeed";
const FAILED: &str = "usage read failed";

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

/// A Pi recording with one request, served by `lead-model`.
fn pi_recording(repo: &Repo, id: &str) {
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
                    "usage": {"input": 7, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 7,
                        "cost": {"total": 0.01}},
                    "content": [{"type": "text", "text": "Done."}]}}),
        ],
    );
}

fn lead(repo: &Repo, cwd: &Path) -> AssertCommand {
    let mut command = repo.arc(cwd);
    command
        .env("ARC_HARNESS", "pi")
        .env("ARC_SESSION", LEAD)
        .env_remove("ARC_ACTOR");
    command
}

/// A change the lead opened, snapshotted, approved and integrated, with an
/// approving audit recorded on behalf of the OpenCode reviewer.
fn reviewed_change(repo: &Repo) -> String {
    let change = opened_change_id(&stdout(lead(repo, &repo.root).args(["begin", "reviewed"])));
    let worktree = repo.home.join(".worktrees").join("repo-reviewed");
    repo.commit(&worktree, "reviewed.txt", "x\n", "feat: reviewed");
    stdout(lead(repo, &worktree).args(["snapshot", "reviewed"]));
    lead(repo, &worktree)
        .args(["review", "reviewed", "--verdict", "approved"])
        .assert()
        .success();
    lead(repo, &repo.root)
        .args(["integrate", "reviewed"])
        .assert()
        .success();
    lead(repo, &repo.root)
        .args([
            "audit",
            &change,
            "--verdict",
            "approved",
            "--on-behalf-of",
            &format!("opencode:{REVIEWER}"),
        ])
        .assert()
        .success();
    change
}

/// A stable OpenCode store tapes reads through `sqlite3`: the fixture's
/// reader answers an exact lookup of the reviewer alone, and fails every
/// message read writing `message_error` on stderr. Returns the `PATH` and
/// `XDG_DATA_HOME` that select it.
fn opencode_store(repo: &Repo, message_error: &str) -> (String, PathBuf) {
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
             case \"$*\" in *'FROM message'*) echo '{message_error}' >&2; exit 1;; esac\n\
             printf '%s\\n' 'row'\n\
             case \"$*\" in *\"'{REVIEWER}'\"*) printf '%s\\n' '{{\"id\":\"{REVIEWER}\",\"model\":null}}';; esac\n"
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

/// The OpenCode reviewer's measurement in the cost view of a reviewed
/// change whose store fails every message read with `message_error`.
fn reviewer_measurement(message_error: &str) -> Value {
    let repo = Repo::new();
    pi_recording(&repo, LEAD);
    let change = reviewed_change(&repo);
    let (path, data_home) = opencode_store(&repo, message_error);
    let view = json_stdout(
        repo.arc(&repo.root)
            .env("PATH", path)
            .env("XDG_DATA_HOME", data_home)
            .args(["cost", "show", "--json", &change]),
    );
    view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == format!("opencode:{REVIEWER}"))
        .unwrap_or_else(|| panic!("no reviewer row: {view:#}"))["measurement"]
        .clone()
}

/// Claim: a participant whose store tapes found busy through every bounded
/// attempt is unmeasured with a reason that says a later read may succeed.
/// Rejects naming it `usage read failed`, which a sweep cannot tell from a
/// read that will never measure.
#[test]
fn a_participant_read_that_stayed_busy_failed_transiently() {
    let measurement =
        reviewer_measurement("Parse error in 6th command line argument: database is locked (5)");
    assert_eq!(measurement["status"], "unmeasured", "{measurement:#}");
    assert_eq!(
        measurement["reasons"],
        json!([TRANSIENT]),
        "{measurement:#}"
    );
}

/// Claim: a participant whose read failed for a reason that cannot pass
/// keeps the failed-read reason. Rejects calling every failure transient.
#[test]
fn a_participant_read_that_cannot_pass_failed_for_good() {
    let measurement =
        reviewer_measurement("Parse error in 6th command line argument: no such table: message");
    assert_eq!(measurement["status"], "unmeasured", "{measurement:#}");
    assert_eq!(measurement["reasons"], json!([FAILED]), "{measurement:#}");
}
