//! A commit lookup over every change's state names the changes it could not
//! read. A change whose state does not reduce might be the one recording the
//! commit, so a lookup that skipped one never answers a plain no-match, and a
//! match found beside one is qualified as possibly not unique.

use super::common::*;
use serde_json::Value;

/// Open `slug` in its own worktree, commit once there and snapshot it, so its
/// patchset records the commit. Returns the change id and the commit.
fn snapshotted(repo: &Repo, slug: &str) -> (String, String) {
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        "x\n",
        &format!("feat: {slug}"),
    );
    let commit = repo.head(&worktree);
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    (change, commit)
}

/// Make a change's state unreadable by corrupting its opening event, as the
/// cost tests do.
fn corrupt(repo: &Repo, change: &str) {
    for entry in fs::read_dir(event_dir(repo, change)).unwrap() {
        let path = entry.unwrap().path();
        let event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] == "change-opened" {
            fs::write(path, "corrupt opening").unwrap();
        }
    }
}

/// Claim: when the only change recording a commit cannot be read, the commit
/// view answers with the gap and names the unreadable change in text and
/// JSON, and the note command does too. Rejects the plain `no change records
/// this commit`, which reads as an authoritative no-match.
#[test]
fn an_unreadable_change_turns_a_no_match_into_the_gap() {
    let repo = Repo::new();
    let (unreadable, commit) = snapshotted(&repo, "hidden");
    snapshotted(&repo, "visible");
    corrupt(&repo, &unreadable);

    let assert = repo
        .arc(&repo.root)
        .args(["cost", "show", "--commit", &commit, "--json"])
        .assert()
        .success();
    let output = assert.get_output();
    let view: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(view["change"].is_null(), "{view:#}");
    assert_ne!(view["answer"], "no change records this commit", "{view:#}");
    assert_eq!(view["lookup"]["status"], "unresolved", "{view:#}");
    assert_eq!(
        view["lookup"]["unreadable"],
        serde_json::json!([unreadable]),
        "{view:#}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("unreadable change {unreadable}")),
        "{stderr}"
    );

    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--commit", &commit]),
    );
    assert!(!text.contains("no change records this commit"), "{text}");
    assert!(text.contains(&unreadable), "{text}");

    let refused = repo
        .arc(&repo.root)
        .args(["cost", "note", &commit])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr).to_string();
    assert!(
        !stderr.contains("error: no change records this commit"),
        "{stderr}"
    );
    assert!(stderr.contains(&unreadable), "{stderr}");
}

/// Claim: a match found while another change is unreadable still answers
/// with the matching change, qualified as possibly not unique and naming the
/// unreadable change. Rejects an unqualified match that hides the gap.
#[test]
fn a_match_beside_an_unreadable_change_is_qualified() {
    let repo = Repo::new();
    let (unreadable, _) = snapshotted(&repo, "hidden");
    let (visible, commit) = snapshotted(&repo, "visible");
    corrupt(&repo, &unreadable);

    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--commit", &commit, "--json"]),
    );
    assert_eq!(view["change"]["id"], visible, "{view:#}");
    assert_eq!(view["lookup"]["status"], "may-not-be-unique", "{view:#}");
    assert_eq!(
        view["lookup"]["unreadable"],
        serde_json::json!([unreadable]),
        "{view:#}"
    );
    let text = stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--commit", &commit]),
    );
    assert!(text.contains("may not be unique"), "{text}");

    // A note written from that lookup carries the same qualification.
    stdout(repo.arc(&repo.root).args(["cost", "note", &commit]));
    let note = String::from_utf8(
        std::process::Command::new("git")
            .args(["notes", "--ref=arc-cost", "show", &commit])
            .current_dir(&repo.root)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(note.contains("may not be unique"), "{note}");
    assert!(note.contains(&unreadable), "{note}");
}

/// Claim: with every change readable a commit view carries no lookup
/// qualification, so its shape and text are unchanged. Rejects a gap reported
/// where there is none.
#[test]
fn a_complete_lookup_carries_no_qualification() {
    let repo = Repo::new();
    let (_, commit) = snapshotted(&repo, "visible");
    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["cost", "show", "--commit", &commit, "--json"]),
    );
    assert!(view.get("lookup").is_none(), "{view:#}");
}

/// Claim: `query --commit` keeps its own recorded-revision lookup, which
/// refuses when a change cannot be read rather than answering for the
/// readable ones, so it never gives a no-match beside a gap. Rejects a
/// query that skips the unreadable change and prints nothing as if no change
/// recorded the commit.
#[test]
fn query_commit_refuses_beside_an_unreadable_change() {
    let repo = Repo::new();
    let (unreadable, commit) = snapshotted(&repo, "hidden");
    snapshotted(&repo, "visible");
    corrupt(&repo, &unreadable);
    let refused = repo
        .arc(&repo.root)
        .args(["query", "--commit", &commit])
        .assert()
        .failure();
    let output = refused.get_output();
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(&unreadable), "{stderr}");
}
