//! A cost report is written one entry at a time: `cost show` renders each
//! work-item and session entry as it writes it, so the entries it holds at
//! once stay one however many changes and sessions a ledger holds. Counts
//! come from the file `ARC_TEST_LEDGER_READS` names, which arc writes as it
//! exits.

use super::common::*;
use serde_json::{json, Value};

const CHANGES: usize = 5;

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

/// A pi recording whose one assistant request carries a harness-written
/// cost.
fn recording(repo: &Repo, id: &str) {
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
                    "usage": {"input": 10, "output": 20, "reasoning": 3, "cacheRead": 4, "cacheWrite": 5, "totalTokens": 39,
                        "cost": {"total": 0.01}},
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

/// `arc` naming no session, so its acting events join no figure.
fn anonymous(repo: &Repo) -> AssertCommand {
    let mut command = repo.arc(&repo.root);
    command.env_remove("ARC_HARNESS").env_remove("ARC_SESSION");
    command
}

/// Dispatch a run on `change`, bind it to the pi session `session` and end
/// it.
fn executed(repo: &Repo, change: &str, session: &str) {
    let run = event_id(&stdout(anonymous(repo).args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        "--change",
        change,
    ])));
    stdout(anonymous(repo).args([
        "run",
        "bind",
        &run,
        "--executor-harness",
        "pi",
        "--executor-session",
        session,
    ]));
    stdout(anonymous(repo).args(["run", "end", &run, "--outcome", "completed"]));
}

/// `CHANGES` changes, each executed by its own session, and one more
/// session executing the first two, so one session's figure is shared.
fn ledger(repo: &Repo) -> Vec<String> {
    recording(repo, "shared-exec");
    let mut changes = Vec::new();
    for index in 0..CHANGES {
        let slug = format!("entry-{index}");
        let change = opened_change_id(&stdout(anonymous(repo).args(["begin", &slug])));
        let session = format!("exec-{index}");
        recording(repo, &session);
        executed(repo, &change, &session);
        if index < 2 {
            executed(repo, &change, "shared-exec");
        }
        changes.push(change);
    }
    changes
}

/// Run `cost show` with `args`; return its counts and output.
fn counted(repo: &Repo, args: &[&str]) -> (Value, String) {
    let counts = repo.home.join("ledger-reads.json");
    let output = stdout(
        repo.arc(&repo.root)
            .env("ARC_TEST_LEDGER_READS", &counts)
            .args(["cost", "show"])
            .args(args),
    );
    let counts: Value = serde_json::from_slice(&fs::read(&counts).unwrap()).unwrap();
    (counts, output)
}

fn held(counts: &Value) -> &Value {
    &counts["cost"]["entries_held_most"]
}

/// Claim: `cost show --all --json` and `--detail` hold at most one
/// work-item or session entry rendered and unwritten at once, the table
/// renders none, and the JSON written entry by entry is the pretty-printed
/// whole document byte for byte. Rejects a report assembled whole, every
/// entry held, before any of it is written.
#[test]
fn a_whole_ledger_report_is_written_one_entry_at_a_time() {
    let repo = Repo::new();
    let changes = ledger(&repo);

    let (counts, output) = counted(&repo, &["--all", "--json"]);
    assert_eq!(held(&counts), &json!(1), "--json: {counts}");
    let view: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        view["work"].as_array().map(Vec::len),
        Some(CHANGES),
        "{view}"
    );
    assert_eq!(
        view["sessions"].as_array().map(Vec::len),
        Some(CHANGES + 1),
        "{view}"
    );
    assert_eq!(
        format!("{}\n", serde_json::to_string_pretty(&view).unwrap()),
        output,
        "the streamed document is the whole document pretty-printed"
    );

    let (counts, detail) = counted(&repo, &["--all", "--detail"]);
    assert_eq!(held(&counts), &json!(1), "--detail: {counts}");
    for change in &changes {
        assert!(detail.contains(&format!("change:{change}")), "{detail}");
    }
    assert!(detail.contains("pi:shared-exec"), "{detail}");

    let (counts, table) = counted(&repo, &["--all"]);
    assert_eq!(held(&counts), &json!(0), "table: {counts}");
    assert!(table.contains("shared-e"), "{table}");
}
