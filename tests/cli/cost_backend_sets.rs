//! One cost command reads every session through one set of tapes backends:
//! its store walks, catalogs and lineage indexes serve every worker thread
//! and every change the command reads. Counts come from the file
//! `ARC_TEST_LEDGER_READS` names, which arc writes as it exits.

use super::common::*;
use serde_json::{json, Value};

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

/// Open `count` more snapshotted changes from `first` on, each executed by
/// `sessions` sessions of its own and reviewed on behalf of the first of
/// them, so the command reads subjects and several sessions per change.
/// Returns each change and its patchset head.
fn changes(repo: &Repo, first: usize, count: usize, sessions: usize) -> Vec<(String, String)> {
    (first..first + count)
        .map(|index| {
            let slug = format!("measured-{index}");
            let (change, _, head) = change_with_patchset(repo, &slug);
            for session in 0..sessions {
                let session = format!("exec-{index}-{session}");
                recording(repo, &session);
                executed(repo, &change, &session);
            }
            anonymous(repo)
                .args([
                    "review",
                    &change,
                    "--verdict",
                    "approved",
                    "--on-behalf-of",
                    &format!("pi:exec-{index}-0"),
                ])
                .assert()
                .success();
            (change, head)
        })
        .collect()
}

/// Run `arc` with `args`; return its tapes counts and its output.
fn counted(repo: &Repo, args: &[&str]) -> (Value, String) {
    let counts = repo.home.join("ledger-reads.json");
    let output = stdout(
        repo.arc(&repo.root)
            .env("ARC_TEST_LEDGER_READS", &counts)
            .args(args),
    );
    let counts: Value = serde_json::from_slice(&fs::read(&counts).unwrap()).unwrap();
    (counts["tapes"].clone(), output)
}

fn assert_one_set(repo: &Repo, args: &[&str], what: &str) -> String {
    let (tapes, output) = counted(repo, args);
    assert_eq!(
        tapes["backend_sets"],
        json!(1),
        "`arc {}` builds one tapes backend set ({what}): {tapes}",
        args.join(" ")
    );
    output
}

/// Claim: `cost show --all`, in text and JSON, `cost show` of one change and
/// of one commit, `cost evidence` and `cost retain` each build one tapes
/// backend set, at one change with one session and at four changes with
/// three sessions each. Rejects a set built per read worker, per subject
/// lookup, or per child read.
#[test]
fn each_cost_report_reads_every_session_through_one_backend_set() {
    let repo = Repo::new();
    for (first, count, sessions) in [(0, 1, 1), (1, 3, 3)] {
        let measured = changes(&repo, first, count, sessions);
        let total = first + count;
        let what = format!("{total} changes");
        let json = assert_one_set(&repo, &["cost", "show", "--all", "--json"], &what);
        let view: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(view["target"]["changes"], total, "{view}");
        let text = assert_one_set(&repo, &["cost", "show", "--all"], &what);
        assert!(text.contains("exec-"), "{text}");
        let (change, head) = &measured[0];
        assert_one_set(&repo, &["cost", "show", "--json", change], &what);
        assert_one_set(&repo, &["cost", "show", "--json", "--commit", head], &what);
        assert_one_set(&repo, &["cost", "evidence", change], &what);
        let retained = assert_one_set(&repo, &["cost", "retain", change], &what);
        assert!(retained.contains("retained: pi:exec-"), "{retained}");
    }
}

/// Claim: `cost note --all` and `cost note` of one change build one tapes
/// backend set however many changes they note, at one noted change and at
/// four, and `--all` still reports every commit it refreshes. Rejects a set
/// built per change's cost read.
#[test]
fn a_note_refresh_reads_every_change_through_one_backend_set() {
    let repo = Repo::new();
    for (first, count) in [(0, 1), (1, 3)] {
        let measured = changes(&repo, first, count, 2);
        for (change, _) in &measured {
            stdout(repo.arc(&repo.root).args(["cost", "note", change]));
        }
        let total = first + count;
        let what = format!("{total} noted changes");
        assert_one_set(&repo, &["cost", "note", &measured[0].0], &what);
        let output = assert_one_set(&repo, &["cost", "note", "--all"], &what);
        let reported = output
            .lines()
            .filter(|line| line.starts_with("unchanged: ") || line.starts_with("written: "))
            .count();
        assert_eq!(reported, total, "{output}");
    }
}
