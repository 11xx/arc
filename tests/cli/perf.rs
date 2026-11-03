//! `ARC_TRACE=1` adds one `arc-perf/1` record to stderr at every exit and
//! changes nothing else a command does: its stdout, its exit status and every
//! file it could write stay what they are without the trace.

use super::common::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Every `arc-perf` record on a run's stderr.
fn records(stderr: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| line.contains("\"arc-perf/"))
        .map(|line| serde_json::from_str(line).expect("a trace line is JSON"))
        .collect()
}

/// Run `arc` in `cwd` with `ARC_TRACE` set to `trace` when one is given.
fn run(repo: &Repo, cwd: &Path, args: &[&str], trace: Option<&str>) -> std::process::Output {
    let mut command = repo.arc(cwd);
    command.env_remove("ARC_TRACE").args(args);
    if let Some(trace) = trace {
        command.env("ARC_TRACE", trace);
    }
    command.output().unwrap()
}

/// The one record a traced run wrote, beside the run's output.
fn traced(repo: &Repo, args: &[&str]) -> (std::process::Output, Value) {
    let output = run(repo, &repo.root, args, Some("1"));
    let mut found = records(&output.stderr);
    assert_eq!(
        found.len(),
        1,
        "arc {args:?} writes one trace record: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (output, found.remove(0))
}

/// Every file under `dir`, with its bytes.
fn files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut held = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                held.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    held
}

/// What a run could have written: the fixture's home, which holds the
/// journal and configuration, and its ledger.
fn written(repo: &Repo) -> (BTreeMap<PathBuf, Vec<u8>>, BTreeMap<PathBuf, Vec<u8>>) {
    (files(&repo.home), files(&repo.root.join(".git/arc")))
}

/// Claim: a traced command prints the same stdout and exits with the same
/// status as an untraced one, writes no file, and adds exactly one
/// `arc-perf/1` line to stderr; `ARC_TRACE` set to anything but `1` traces
/// nothing. Rejects a record on stdout or in a file, a trace that alters
/// the exit status, and a trace on by any non-empty value.
#[test]
fn a_trace_changes_nothing_but_stderr() {
    let repo = Repo::new();
    let (change, _, _) = change_with_patchset(&repo, "traced");
    for args in [
        vec!["list"],
        vec!["show", change.as_str(), "--json"],
        vec!["check", change.as_str()],
    ] {
        let plain = run(&repo, &repo.root, &args, None);
        let before = written(&repo);
        let (traced_run, record) = traced(&repo, &args);
        assert_eq!(record["schema"], "arc-perf/1", "{record}");
        assert!(written(&repo) == before, "arc {args:?} wrote a file");
        assert_eq!(traced_run.stdout, plain.stdout, "arc {args:?} stdout");
        assert_eq!(traced_run.status.code(), plain.status.code(), "{args:?}");
        assert!(records(&plain.stderr).is_empty(), "{args:?} untraced");
        let off = run(&repo, &repo.root, &args, Some("0"));
        assert!(records(&off.stderr).is_empty(), "{args:?} ARC_TRACE=0");
    }
}

/// Claim: the record is written whichever way arc exits, naming the outcome
/// and the status: success, a status the command chose, an error, refused
/// arguments and printed help. Rejects an exit path that leaves without the
/// record or names the wrong outcome.
#[test]
fn a_trace_is_written_on_every_exit_path() {
    let repo = Repo::new();
    let (change, _, _) = change_with_patchset(&repo, "exits");
    let blocked = run(&repo, &repo.root, &["check", &change], None)
        .status
        .code()
        .unwrap();
    assert_ne!(blocked, 0, "an unreviewed change's check is blocked");
    for (args, command, outcome, code) in [
        (vec!["list"], json!("list"), "exited", 0),
        (
            vec!["check", change.as_str()],
            json!("check"),
            "exited",
            blocked,
        ),
        (vec!["show", "no-such-change"], json!("show"), "error", 1),
        (vec!["list", "--no-such-flag"], Value::Null, "usage", 2),
        (vec!["--help"], Value::Null, "exited", 0),
    ] {
        let (output, record) = traced(&repo, &args);
        assert_eq!(output.status.code(), Some(code), "{args:?}");
        assert_eq!(record["outcome"], outcome, "{args:?}: {record}");
        assert_eq!(record["exit_code"], code, "{args:?}: {record}");
        assert_eq!(record["command"], command, "{args:?}: {record}");
    }
}

/// Claim: an aggregate read's record counts its ledger work, each event file
/// and the import batch opened once, the repository events materialized
/// once and every change reduced, and names no path or argument value.
/// Rejects counters that double count, miss a namespace, or leak the
/// fixture's paths or the selected change.
#[test]
fn a_trace_counts_ledger_work_and_names_no_path_or_argument() {
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
    let (local, _, _) = change_with_patchset(&repo, "local");
    stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        "--change",
        &local,
    ]));
    let ledger = repo.root.join(".git/arc");
    let count = |dir: PathBuf| fs::read_dir(dir).map_or(0, |entries| entries.count());
    let change_files = count(ledger.join("changes").join(&local).join("events"))
        + count(ledger.join("changes").join(&imported).join("events"));
    let repository_files = count(ledger.join("repository/events"));
    assert!(repository_files > 0, "the dispatch is a repository event");
    let imports = count(ledger.join("imports"));
    assert_eq!(imports, 1, "the import publishes one batch");

    let (_, record) = traced(&repo, &["list"]);
    assert_eq!(record["command"], "list", "{record}");
    assert_eq!(record["counts"]["changes_reduced"], 2, "{record}");
    assert_eq!(
        record["counts"]["repository_materializations"], 1,
        "{record}"
    );
    let ledger_counts = &record["ledger"];
    assert_eq!(ledger_counts["change"]["opened"], change_files, "{record}");
    assert_eq!(
        ledger_counts["repository"]["opened"], repository_files,
        "{record}"
    );
    assert_eq!(ledger_counts["import"]["opened"], imports, "{record}");
    for namespace in ["change", "repository", "import"] {
        let opened = &ledger_counts[namespace]["opened"];
        assert_eq!(&ledger_counts[namespace]["opens"], opened, "{record}");
        assert!(ledger_counts[namespace]["bytes"].as_u64().unwrap() > 0);
    }
    // The store and the privilege scope each decode the batch they share.
    for namespace in ["change", "repository"] {
        let opened = &ledger_counts[namespace]["opened"];
        assert_eq!(&ledger_counts[namespace]["decoded"], opened, "{record}");
    }
    assert!(
        ledger_counts["import"]["decoded"].as_u64() >= ledger_counts["import"]["opened"].as_u64(),
        "{record}"
    );
    assert!(record["phases"]["read"]["calls"].as_u64().unwrap() > 0);

    let (_, record) = traced(&repo, &["check", &local]);
    let text = record.to_string();
    for leak in [
        repo.root.to_str().unwrap(),
        repo.home.to_str().unwrap(),
        local.as_str(),
    ] {
        assert!(!text.contains(leak), "the record names {leak}: {text}");
    }
    let children = record["children"]["by_kind"].as_object().unwrap();
    assert!(
        children.keys().any(|kind| kind.starts_with("git ")),
        "{record}"
    );
    let runs: u64 = children
        .values()
        .map(|kind| kind["runs"].as_u64().unwrap())
        .sum();
    assert_eq!(record["children"]["runs"], runs, "{record}");
}
