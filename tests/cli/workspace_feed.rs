use crate::common::{opened_change_id, stdout, stdout_any_status, Repo};
use serde_json::Value;
use std::fs;
use std::path::Path;

/// Every notice the fixture's workspace feed holds, oldest first.
fn notices(repo: &Repo) -> Vec<Value> {
    let feed = repo.home.join(".local/ai/arc/feed/events.jsonl");
    fs::read_to_string(feed)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("each notice is one JSON line"))
        .collect()
}

/// Claim: a committed ledger write appends an `arc-workspace-feed/1` notice
/// naming the file and, for a change event, the change; a committed journal
/// write appends one naming the journal directory, marked `binding` when it
/// registered the journal and unmarked otherwise; a read appends none.
/// Rejects a write that leaves the feed silent, a notice that cannot be traced
/// to its ledger, a registration a follower cannot tell from other journal
/// writes, and a read that writes the feed.
#[test]
fn writes_append_feed_notices_and_reads_do_not() {
    let repo = Repo::new();
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "feed-probe",
        "--no-worktree",
    ])));

    let ledger = fs::canonicalize(repo.root.join(".git/arc")).unwrap();
    let opened = notices(&repo);
    let change_notice = opened
        .iter()
        .find(|notice| notice["source"] == "ledger" && notice["change"] == change.as_str())
        .unwrap_or_else(|| panic!("no ledger notice names {change}: {opened:?}"));
    assert_eq!(change_notice["schema"], "arc-workspace-feed/1");
    let path = Path::new(change_notice["path"].as_str().unwrap());
    assert!(
        path.starts_with(&ledger),
        "{} is not under {}",
        path.display(),
        ledger.display()
    );
    assert!(change_notice["at"].as_str().is_some(), "{change_notice}");
    // Beginning the first change registers the project's journal.
    assert!(
        opened
            .iter()
            .any(|notice| notice["source"] == "journal" && notice["binding"] == true),
        "no binding notice after the registering begin: {opened:?}"
    );

    stdout_any_status(repo.arc(&repo.root).args(["workspace", "report", "--json"]));
    stdout(repo.arc(&repo.root).args(["status", &change]));
    assert_eq!(notices(&repo), opened, "a read appended to the feed");

    repo.arc(&repo.root)
        .args(["journal", "note", "feed-probe", "--body-file", "-"])
        .write_stdin("A note the feed should mention.\n")
        .assert()
        .success();
    let noted = notices(&repo);
    let journal_notice = noted[opened.len()..]
        .iter()
        .find(|notice| notice["source"] == "journal")
        .unwrap_or_else(|| panic!("no journal notice after a note: {noted:?}"));
    assert!(journal_notice.get("binding").is_none(), "{journal_notice}");
    let dir = Path::new(journal_notice["dir"].as_str().unwrap());
    assert!(
        dir.join("events.jsonl").is_file(),
        "{} is not a journal directory",
        dir.display()
    );
}
