//! The commands every landing runs, `check` and `integrate`, and the journal
//! queue a lead reads beside them, read the ledger a fixed number of times
//! however many changes it holds. Counts come from the `ARC_TRACE` record and
//! from the file `ARC_TEST_LEDGER_READS` names.

use super::common::*;
use serde_json::Value;

/// One traced run of `arc args` from `cwd`: its trace record, its read counts
/// and its exit status.
pub(crate) fn traced(repo: &Repo, cwd: &Path, args: &[&str]) -> (Value, Value, Option<i32>) {
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
/// batch carries, if the ledger holds any.
pub(crate) fn changes_held(repo: &Repo) -> u64 {
    let ledger = repo.root.join(".git/arc");
    let mut changes: std::collections::BTreeSet<String> = fs::read_dir(ledger.join("changes"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let imports = fs::read_dir(ledger.join("imports"));
    for entry in imports.into_iter().flatten() {
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

/// Claim: `arc journal open` reads the ledger once however many changes it
/// holds. Rejects a queue that materializes the repository events, the
/// import batches or the scope once per inventory pass.
#[test]
fn journal_open_reads_the_ledger_once_however_many_changes() {
    let repo = ledger_with_import();
    change_with_patchset(&repo, "open");
    repo.arc(&repo.root)
        .args(["journal", "todo", "follow-up"])
        .write_stdin("Follow up.\n")
        .args(["--body-file", "-"])
        .assert()
        .success();
    add_changes(&repo, 0, 2);
    assert_read_once(&repo, &["journal", "open"]);
    add_changes(&repo, 2, 3);
    assert_read_once(&repo, &["journal", "open"]);
    assert_read_once(&repo, &["journal", "open", "--json"]);
}

/// The reads one traced integration of a fresh approved change makes in a
/// ledger holding `extra` more changes.
fn integration_reads(extra: usize) -> (Value, Value, u64) {
    let repo = ledger_with_import();
    add_changes(&repo, 0, extra);
    let (_, worktree, _) = change_with_patchset(&repo, "landing");
    repo.arc(&worktree)
        .args(["review", "landing", "--verdict", "approved"])
        .assert()
        .success();
    let (record, counts, code) = traced(&repo, &repo.root, &["integrate", "landing"]);
    assert_eq!(code, Some(0), "{record}");
    (record, counts, changes_held(&repo))
}

/// Claim: `arc integrate` reads the repository events once per read of the
/// ledger it makes, however many changes the ledger holds: the change once
/// before the locks, the confirming re-read under them, the readiness report
/// and the confirmation. Rejects a change read that materializes the
/// repository events once for its rewrites and again for its projection, and
/// a landing that reads the change a second time before the locks.
#[test]
fn integrate_reads_the_ledger_once_per_read_however_many_changes() {
    let (few, few_counts, few_changes) = integration_reads(1);
    let (many, many_counts, many_changes) = integration_reads(5);
    for (record, counts) in [(&few, &few_counts), (&many, &many_counts)] {
        assert_eq!(
            record["counts"]["repository_materializations"], 4,
            "{record} {counts}"
        );
        assert_eq!(record["counts"]["status_reports"], 2, "{record}");
    }
    for ((name, few_value), (_, many_value)) in fixed_reads(&few, &few_counts)
        .into_iter()
        .zip(fixed_reads(&many, &many_counts))
    {
        assert_eq!(
            few_value, many_value,
            "{name} grew from {few_value} to {many_value} with more changes"
        );
    }
    // Each change is reduced by the readiness report and by the confirmation.
    let reduced = |record: &Value| record["counts"]["changes_reduced"].as_u64().unwrap();
    assert_eq!(
        reduced(&many) - reduced(&few),
        2 * (many_changes - few_changes),
        "{few} {many}"
    );
}

/// The trace record of a queue that lands one tagged, approved change in a
/// ledger holding `extra` more changes.
fn queued_integration_reads(extra: usize) -> Value {
    let repo = ledger_with_import();
    add_changes(&repo, 0, extra);
    let (landing, worktree, _) = change_with_patchset(&repo, "queued");
    repo.arc(&worktree)
        .args(["review", "queued", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["metadata", &landing, "--tag", "#queued"])
        .assert()
        .success();
    let (record, _, code) = traced(&repo, &repo.root, &["integrate", "--tag", "#queued"]);
    assert_eq!(code, Some(0), "{record}");
    record
}

/// Claim: a queued landing reads the change once between its readiness turn
/// and the locks, however many changes the ledger holds: the queue's
/// selection, the turn's state, its readiness report and its last state
/// before the merge, then the re-read under the locks, the readiness report
/// and the confirmation. Rejects a merge step that reads the change again
/// before taking the locks.
#[test]
fn a_queued_landing_reads_its_change_once_before_the_locks() {
    for extra in [1, 4] {
        let record = queued_integration_reads(extra);
        assert_eq!(
            record["counts"]["repository_materializations"], QUEUED_MATERIALIZATIONS,
            "{record}"
        );
    }
}

/// The repository materializations a one-member queue makes.
const QUEUED_MATERIALIZATIONS: u64 = 9;

/// Open a change on `base` blocked by `blocked_by`, commit to it, snapshot
/// and approve it.
fn approved_on(repo: &Repo, slug: &str, base: &str, blocked_by: &[String]) {
    let mut args = vec!["begin", slug, "--base", base];
    for blocker in blocked_by {
        args.extend(["--blocked-by", blocker]);
    }
    stdout(repo.arc(&repo.root).args(&args));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: {slug}"),
    );
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
}

/// The trace record of integrating a change blocked by `count` integrated
/// prerequisites, each stacked on the one before.
fn integration_with_prerequisites(count: usize) -> Value {
    let repo = ledger_with_import();
    let mut base = "master".to_string();
    let mut prerequisites = Vec::new();
    for index in 0..count {
        let slug = format!("prerequisite-{index}");
        approved_on(&repo, &slug, &base, &[]);
        stdout(repo.arc(&repo.root).args(["integrate", &slug]));
        base = format!("arc/{slug}");
        prerequisites.push(slug);
    }
    approved_on(&repo, "dependent", &base, &prerequisites);
    let (record, _, code) = traced(&repo, &repo.root, &["integrate", "dependent"]);
    assert_eq!(code, Some(0), "{record}");
    record
}

/// Claim: the authorization basis reads every prerequisite through one read
/// of the ledger, so integrating a change blocked by more prerequisites
/// materializes the repository events, lists the import batches and derives
/// the scope no more often. Rejects a basis that reads each prerequisite
/// from a fresh store read of its own.
#[test]
fn integrate_reads_prerequisites_through_one_read() {
    let one = integration_with_prerequisites(1);
    let three = integration_with_prerequisites(3);
    for (name, path) in [
        (
            "repository materializations",
            "/counts/repository_materializations",
        ),
        ("scope derivations", "/counts/scope_derivations"),
        ("import enumerations", "/ledger/import/enumerations"),
    ] {
        assert_eq!(
            one.pointer(path),
            three.pointer(path),
            "{name} grew with prerequisites: {one} {three}"
        );
    }
}
