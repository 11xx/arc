//! One command reads the ledger once: a commit's cost, a whole-ledger cost
//! view and catchup each read every ledger file from disk at most once, and
//! locating a commit starts no Git process per change. Counts come from the
//! file `ARC_TEST_LEDGER_READS` names, which arc writes as it exits.

use super::common::*;
use serde_json::{json, Value};

const CHANGES: usize = 12;

fn event_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .expect("command output should contain an event id")
        .to_string()
}

fn head(worktree: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(worktree)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A ledger of `CHANGES` snapshotted changes, each with a dispatch, so the
/// repository store holds events too. The last change's patchset holds two
/// commits; its first commit is returned, which only a range holds.
fn ledger(repo: &Repo) -> (String, String) {
    let mut ranged = (String::new(), String::new());
    for index in 0..CHANGES {
        let slug = format!("work-{index}");
        let change = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", &slug])));
        let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
        repo.commit(
            &worktree,
            "one.txt",
            &format!("{slug}\n"),
            &format!("feat: {slug} one"),
        );
        let first = head(&worktree);
        if index + 1 == CHANGES {
            repo.commit(&worktree, "two.txt", "two\n", &format!("feat: {slug} two"));
        }
        stdout(repo.arc(&worktree).args(["snapshot", &slug]));
        let dispatch = event_id(&stdout(repo.arc(&repo.root).args([
            "run",
            "dispatch",
            "--route",
            "fixture-route",
            "--worktree",
            "/work/run",
            "--change",
            &change,
        ])));
        assert!(!dispatch.is_empty());
        ranged = (change, first);
    }
    ranged
}

/// Run `arc` with `args` and return its read counts.
fn counted(repo: &Repo, args: &[&str]) -> (Value, String) {
    let counts = repo.home.join("ledger-reads.json");
    let output = stdout(
        repo.arc(&repo.root)
            .env("ARC_TEST_LEDGER_READS", &counts)
            .args(args),
    );
    let counts: Value = serde_json::from_slice(&fs::read(&counts).unwrap()).unwrap();
    (counts, output)
}

fn assert_read_once(counts: &Value, what: &str) {
    for kind in ["repository", "change"] {
        assert_eq!(
            counts["most_reads_per_file"][kind],
            json!(1),
            "{what} reads each {kind} event file once: {counts}"
        );
    }
}

/// Claim: `cost show --commit` for a commit only a patchset range holds
/// reads each ledger file once and starts fewer Git processes than there are
/// changes, and still names the change and the commit's place in it.
/// Rejects a lookup that replays the ledger, or walks a range in its own
/// process, per change.
#[test]
fn a_commit_lookup_reads_the_ledger_once_and_walks_no_range_per_change() {
    let repo = Repo::new();
    let (change, first) = ledger(&repo);
    let (counts, output) = counted(&repo, &["cost", "show", "--json", "--commit", &first]);
    assert_read_once(&counts, "cost show --commit");
    let processes = counts["git_processes"].as_u64().unwrap();
    assert!(
        (processes as usize) < CHANGES,
        "{processes} Git processes for {CHANGES} changes: {counts}"
    );
    let view: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(view["change"]["id"], change, "{view}");
    assert_eq!(view["position"], json!({"index": 1, "of": 2}), "{view}");
}

/// Claim: a whole-ledger cost view and catchup each read every ledger file
/// once. Rejects a projection replayed per change or per section.
#[test]
fn a_whole_ledger_cost_view_and_catchup_read_the_ledger_once() {
    let repo = Repo::new();
    ledger(&repo);
    let (counts, output) = counted(&repo, &["cost", "show", "--all", "--json"]);
    assert_read_once(&counts, "cost show --all");
    let view: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(view["target"]["changes"], CHANGES, "{view}");
    let (counts, _) = counted(&repo, &["catchup"]);
    assert_read_once(&counts, "catchup");
}

/// Claim: a commit no change records still answers that no change records
/// it, reading the ledger once. Rejects the one-pass lookup answering for a
/// commit outside every range.
#[test]
fn a_commit_no_change_records_still_answers_so() {
    let repo = Repo::new();
    ledger(&repo);
    repo.commit(
        &repo.root,
        "outside.txt",
        "outside\n",
        "chore: outside any change",
    );
    let outside = head(&repo.root);
    let (counts, output) = counted(&repo, &["cost", "show", "--commit", &outside]);
    assert!(output.contains("no change records this commit"), "{output}");
    assert_read_once(&counts, "cost show --commit");
}
