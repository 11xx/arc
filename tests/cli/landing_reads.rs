//! `arc check`, which every landing runs, reads the ledger a fixed number of
//! times however many changes it holds. Counts come from the `ARC_TRACE` record and
//! from the file `ARC_TEST_LEDGER_READS` names.

use super::common::*;
use serde_json::Value;

/// One traced run of `arc args` from `cwd`: its trace record, its read counts
/// and its exit status.
fn traced(repo: &Repo, cwd: &Path, args: &[&str]) -> (Value, Value, Option<i32>) {
    let counts = repo.home.join("landing-reads.json");
    let _ = fs::remove_file(&counts);
    let output = repo
        .arc(cwd)
        .env("ARC_TRACE", "1")
        .env("ARC_TEST_LEDGER_READS", &counts)
        .args(args)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let record = stderr
        .lines()
        .find(|line| line.contains("\"arc-perf/1\""))
        .map(|line| serde_json::from_str(line).unwrap())
        .unwrap_or_else(|| panic!("arc {args:?} writes a trace record: {stderr}"));
    let counts = serde_json::from_slice(&fs::read(&counts).unwrap()).unwrap();
    (record, counts, output.status.code())
}

/// A ledger holding one imported change and one repository event, so the
/// reads cover an import batch and the repository events.
fn ledger_with_import() -> Repo {
    let source = Repo::new();
    let (imported, _, _) = change_with_patchset(&source, "received");
    let bundle = source.home.join("received.json");
    source
        .arc(&source.root)
        .args(["export", &imported, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let repo = Repo::new();
    deliver_git_objects(&source, &repo);
    repo.arc(&repo.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let (dispatched, _, _) = change_with_patchset(&repo, "dispatched");
    stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        "--change",
        &dispatched,
    ]));
    repo
}

/// Open `count` more changes beside whatever the ledger holds.
fn add_changes(repo: &Repo, first: usize, count: usize) {
    for index in first..first + count {
        begin_without_checkout(repo, &format!("more-{index}"), &[]);
    }
}

/// Open, approve and integrate a change.
fn land(repo: &Repo, slug: &str) {
    let (_, worktree, _) = change_with_patchset(repo, slug);
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", slug])
        .assert()
        .success();
}

/// The changes the ledger holds: a directory each, and each change an import
/// batch carries.
fn changes_held(repo: &Repo) -> u64 {
    let ledger = repo.root.join(".git/arc");
    let mut changes: std::collections::BTreeSet<String> = fs::read_dir(ledger.join("changes"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    for entry in fs::read_dir(ledger.join("imports")).unwrap() {
        let batch: Value =
            serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
        changes.insert(batch["change_id"].as_str().unwrap().to_string());
    }
    changes.len() as u64
}

/// The read counts that must not grow with the ledger, named for a failure.
fn fixed_reads(record: &Value, counts: &Value) -> Vec<(&'static str, Value)> {
    vec![
        (
            "repository materializations",
            record["counts"]["repository_materializations"].clone(),
        ),
        (
            "scope derivations",
            record["counts"]["scope_derivations"].clone(),
        ),
        (
            "import enumerations",
            record["ledger"]["import"]["enumerations"].clone(),
        ),
        (
            "most reads of one change file",
            counts["most_reads_per_file"]["change"].clone(),
        ),
        (
            "most reads of one repository file",
            counts["most_reads_per_file"]["repository"].clone(),
        ),
        (
            "most reads of one import batch",
            counts["most_reads_per_file"]["import"].clone(),
        ),
    ]
}

/// Assert one read of everything `arc args` needs: one repository
/// materialization, one scope, each ledger file read and each import batch
/// decoded once, and no change reduced twice.
fn assert_read_once(repo: &Repo, args: &[&str]) {
    let (record, counts, _) = traced(repo, &repo.root, args);
    for (name, value) in fixed_reads(&record, &counts) {
        assert_eq!(value, 1, "arc {args:?}: {name}: {record} {counts}");
    }
    assert_eq!(
        record["ledger"]["import"]["decoded"], record["ledger"]["import"]["opened"],
        "arc {args:?}: {record}"
    );
    let reduced = record["counts"]["changes_reduced"].as_u64().unwrap();
    assert!(
        reduced <= changes_held(repo),
        "arc {args:?} reduced {reduced} changes of {}: {record}",
        changes_held(repo)
    );
}

/// Claim: `arc check` of an open or an integrated change reads the ledger
/// once: one repository materialization and one scope, every ledger file read
/// once and no change reduced twice, however many changes the ledger holds.
/// Rejects a check whose report, debt and review-queue reads each read and
/// reduce the whole ledger again.
#[test]
fn check_reads_the_ledger_once_however_many_changes() {
    let repo = ledger_with_import();
    let (open, _, _) = change_with_patchset(&repo, "open");
    land(&repo, "landed");
    add_changes(&repo, 0, 2);
    for change in [open.as_str(), "landed"] {
        assert_read_once(&repo, &["check", change]);
        assert_read_once(&repo, &["check", change, "--json"]);
    }
    add_changes(&repo, 2, 3);
    for change in [open.as_str(), "landed"] {
        assert_read_once(&repo, &["check", change]);
    }
}
