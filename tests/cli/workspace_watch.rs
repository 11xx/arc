use crate::common::{journal_artifact, json_stdout_any_status, opened_change_id, stdout, Repo};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc;
use std::time::Duration;

/// How long a test waits for a line the watch owes. Generous: the debug
/// binary reads slowly on a loaded machine.
const OWED: Duration = Duration::from_secs(60);
/// How long a test waits to conclude the watch printed nothing.
const QUIET: Duration = Duration::from_secs(3);

/// A report without the times it was observed at, which differ between any
/// two reads.
fn bare(mut report: Value) -> Value {
    report.as_object_mut().unwrap().remove("observation");
    report
}

/// What `arc workspace report --json` prints now, bar its observation.
fn fresh_report(repo: &Repo, previous: Option<&std::path::Path>) -> Value {
    let mut report = repo.arc(&repo.root);
    report.args(["workspace", "report", "--json"]);
    if let Some(previous) = previous {
        report.arg("--previous").arg(previous);
    }
    bare(json_stdout_any_status(&mut report))
}

fn request(stdin: &mut std::process::ChildStdin, request: Value) {
    writeln!(stdin, "{request}").unwrap();
    stdin.flush().unwrap();
}

/// Claim: `arc workspace watch` prints the report as one line, then again,
/// equal to a fresh `arc workspace report --json` bar its observation, after
/// each arc write that changes it, whether a journal write or one change's
/// event, including in a project it holds read from the write before; a
/// request to compare with an earlier report is answered with that
/// comparison; a refresh that changes nothing prints nothing; a malformed
/// request is refused on stderr without ending the watch; and the watch
/// exits 0 once its input closes.
/// Rejects a watch that misses the feed, one that keeps a written change's
/// earlier state or a written journal's earlier projection, one that prints
/// an unchanged report again, and one that outlives its consumer.
#[test]
fn watch_follows_writes_and_requests_with_the_report() {
    let repo = Repo::new();
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "watched",
        "--no-worktree",
    ])));

    let mut watch = repo.spawn_arc_piped(&repo.root, &["workspace", "watch"]);
    let mut stdin = watch.stdin.take().unwrap();
    let (lines, reports) = mpsc::channel::<String>();
    let output = BufReader::new(watch.stdout.take().unwrap());
    let reader = std::thread::spawn(move || {
        for line in output.lines() {
            if lines.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let next_report = |what: &str| -> Value {
        let line = reports
            .recv_timeout(OWED)
            .unwrap_or_else(|_| panic!("the watch printed no report {what}"));
        serde_json::from_str(&line).expect("each report is one JSON line")
    };

    let first = next_report("at start");
    assert_eq!(first["schema"], "arc-workspace-report/3");
    assert_eq!(bare(first.clone()), fresh_report(&repo, None));

    // The first write leaves the project held read for the writes after it.
    let (_, noted_file) = journal_artifact(&repo, "noted", "todo", "# Noted\n");
    let noted = next_report("after a journal write");
    assert_eq!(bare(noted.clone()), fresh_report(&repo, None));
    assert_ne!(bare(noted.clone()), bare(first), "the note changed nothing");

    repo.arc(&repo.root)
        .args(["close", &change, "--abandoned"])
        .assert()
        .success();
    let closed = next_report("after a change event");
    assert_eq!(bare(closed.clone()), fresh_report(&repo, None));
    assert_ne!(bare(closed.clone()), bare(noted), "closing changed nothing");

    // What a journal event alone records, as consuming an artifact does,
    // shows only through a projection read again.
    repo.arc(&repo.root)
        .args(["journal", "consume", &noted_file, "--outcome", "done"])
        .assert()
        .success();
    let consumed = next_report("after a journal write to a held journal");
    assert_eq!(bare(consumed.clone()), fresh_report(&repo, None));
    assert_ne!(
        bare(consumed.clone()),
        bare(closed),
        "consuming changed nothing"
    );
    let closed = consumed;

    let baseline = repo.home.join("baseline.json");
    std::fs::write(&baseline, serde_json::to_string(&closed).unwrap()).unwrap();
    request(
        &mut stdin,
        serde_json::json!({"schema": "arc-workspace-watch-request/1", "refresh": true}),
    );
    request(&mut stdin, serde_json::json!({"refresh": true}));
    request(
        &mut stdin,
        serde_json::json!({"schema": "arc-workspace-watch-request/1", "previous": baseline}),
    );
    // A refresh that printed the unchanged report would be read here first.
    let compared = next_report("after a previous request");
    assert_eq!(compared["previous"], closed["observation"]["finished_at"]);
    assert_eq!(bare(compared), fresh_report(&repo, Some(&baseline)));
    assert!(
        reports.recv_timeout(QUIET).is_err(),
        "the watch printed a report nothing changed"
    );

    drop(stdin);
    let deadline = std::time::Instant::now() + OWED;
    let status = loop {
        if let Some(status) = watch.try_wait().unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the watch outlived its input"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    watch
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("request refused") && stderr.contains("arc-workspace-watch-request/1"),
        "{stderr}"
    );
    reader.join().unwrap();
}

/// A test owns its watch even when an assertion fails.
struct RunningWatch(std::process::Child);

impl Drop for RunningWatch {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Claim: --library emits a report for non-actionable filings and amendments,
/// including repeated writes to a held journal, identical to a fresh library
/// report apart from observation. Pinning and unpinning a consumed artifact
/// each emit its current mark, including within one recorded second.
/// A refresh with no change stays silent.
/// Rejects comparisons that omit the library, cached libraries that are not
/// invalidated on notices, missed pin changes, stale marks,
/// stale event projections and unconditional emissions.
#[test]
fn watch_library_emits_non_actionable_writes_and_held_updates() {
    let repo = Repo::new();
    journal_artifact(&repo, "reference", "note", "# Reference\n");
    let mut watch =
        RunningWatch(repo.spawn_arc_piped(&repo.root, &["workspace", "watch", "--library"]));
    let mut stdin = watch.0.stdin.take().unwrap();
    let (lines, reports) = mpsc::channel::<String>();
    let output = BufReader::new(watch.0.stdout.take().unwrap());
    let reader = std::thread::spawn(move || {
        for line in output.lines() {
            if lines.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let fresh = || {
        bare(json_stdout_any_status(repo.arc(&repo.root).args([
            "workspace",
            "report",
            "--library",
            "--json",
        ])))
    };
    let next = |what: &str| -> Value {
        let line = reports
            .recv_timeout(OWED)
            .unwrap_or_else(|_| panic!("the library watch printed no report {what}"));
        bare(serde_json::from_str(&line).expect("one JSON report per line"))
    };
    assert_eq!(next("at start"), fresh());
    let (_, conclusion) = journal_artifact(&repo, "finished", "conclusion", "# Finished\n");
    let filed = next("after a non-actionable conclusion");
    assert_eq!(filed, fresh());
    assert!(filed["sections"]["work"].as_array().unwrap().is_empty());
    repo.arc(&repo.root)
        .args([
            "journal",
            "correct",
            &conclusion,
            "--target",
            "artifact",
            "--field",
            "title",
            "--value",
            "Corrected conclusion",
        ])
        .assert()
        .success();
    let corrected = next("after a title correction in a held journal");
    assert_eq!(corrected, fresh());
    assert_ne!(corrected, filed);
    let row = corrected["projects"][0]["library"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["file"] == conclusion)
        .unwrap();
    assert_eq!(row["title"], "Corrected conclusion");
    repo.arc(&repo.root)
        .args(["journal", "consume", &conclusion, "--outcome", "done"])
        .assert()
        .success();
    let consumed = next("after consumption in a held journal");
    assert_eq!(consumed, fresh());
    assert_eq!(
        consumed["projects"][0]["library"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["file"] == conclusion)
            .unwrap()["status"],
        "consumed"
    );
    let mut previous = consumed;
    for (verb, mark) in [("pin", Some(true)), ("unpin", None)] {
        repo.arc(&repo.root)
            .args(["journal", verb, &conclusion])
            .assert()
            .success();
        let changed = next(&format!("after {verb} in a held journal"));
        assert_eq!(changed, fresh());
        let row = changed["projects"][0]["library"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["file"] == conclusion)
            .unwrap();
        assert_eq!(row.get("pinned").and_then(Value::as_bool), mark);
        assert_ne!(changed, previous);
        previous = changed;
    }
    request(
        &mut stdin,
        serde_json::json!({"schema": "arc-workspace-watch-request/1", "refresh": true}),
    );
    assert!(
        reports.recv_timeout(QUIET).is_err(),
        "unchanged library emitted again"
    );
    drop(stdin);
    assert!(watch.0.wait().unwrap().success());
    reader.join().unwrap();
}

/// Claim: a watch given `--progress` prints the report again when an item of
/// a series changes and when a linked change's ledger does, each time equal
/// to a fresh report. Rejects a page left showing a stage the ledger has
/// moved past, and an item edit that reaches no reader until another write.
#[test]
fn watch_progress_emits_item_edits_and_linked_ledger_writes() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["progress", "start", "watched", "--title", "Watched"])
        .args(["--item", "Linked", "--item", "By hand"])
        .assert()
        .success();
    let begun =
        stdout(
            repo.arc(&repo.root)
                .args(["begin", "watched-work", "--item", "watched:i1"]),
        );
    let change = opened_change_id(&begun);
    let mut watch =
        RunningWatch(repo.spawn_arc_piped(&repo.root, &["workspace", "watch", "--progress"]));
    let stdin = watch.0.stdin.take().unwrap();
    let (lines, reports) = mpsc::channel::<String>();
    let output = BufReader::new(watch.0.stdout.take().unwrap());
    let reader = std::thread::spawn(move || {
        for line in output.lines() {
            if lines.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let fresh = || {
        bare(json_stdout_any_status(repo.arc(&repo.root).args([
            "workspace",
            "report",
            "--progress",
            "--json",
        ])))
    };
    let next = |what: &str| -> Value {
        let line = reports
            .recv_timeout(OWED)
            .unwrap_or_else(|_| panic!("the progress watch printed no report {what}"));
        bare(serde_json::from_str(&line).expect("one JSON report per line"))
    };
    let items = |report: &Value| report["projects"][0]["progress"][0]["items"].clone();
    let started = next("at start");
    assert_eq!(started, fresh());
    assert_eq!(items(&started)[0]["change"], change.as_str());
    assert_eq!(items(&started)[0]["stage"], "opened");
    assert_eq!(items(&started)[1]["stage"], "open");

    repo.arc(&repo.root)
        .args(["progress", "done", "watched", "i2"])
        .assert()
        .success();
    let edited = next("after an item was marked done");
    assert_eq!(edited, fresh());
    assert_eq!(items(&edited)[1]["stage"], "done");

    let worktree = repo.home.join(".worktrees").join("repo-watched-work");
    repo.commit(&worktree, "watched.txt", "one\n", "feat: watched");
    stdout(repo.arc(&worktree).args(["snapshot", "watched-work"]));
    let snapshotted = next("after the linked change recorded a patchset");
    assert_eq!(snapshotted, fresh());
    assert_eq!(items(&snapshotted)[0]["stage"], "snapshotted");

    drop(stdin);
    drop(watch);
    reader.join().unwrap();
}
