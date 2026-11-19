use crate::common::{
    git, journal_artifact, json_stdout_any_status, opened_change_id, stdout, Repo,
};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
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

/// Claim: a watch follows a registered project's first ledger write and later
/// events across Git layouts and configured store paths. Unrelated ledger
/// writes and comparison requests leave retained project reads alone.
/// Rejects a watch that needs a binding notice or refresh to see a first ledger,
/// loses later events, or rereads projects on unrelated notices.
#[test]
fn watch_discovers_a_registered_projects_first_ledger() {
    for layout in [
        "default",
        "linked",
        "separate",
        "data-root",
        "exact",
        "journal-first",
    ] {
        let repo = Repo::new();
        let mut cwd = repo.root.clone();
        let mut environment: Vec<(&str, &Path)> = Vec::new();
        let store_parent = repo.home.join("stores");
        let store_alias = repo.home.join("store-alias");
        let store_path = store_alias.join("missing/ledgers");
        match layout {
            "journal-first" => {
                cwd = repo.home.join("journal-first");
                std::fs::create_dir(&cwd).unwrap();
                stdout(repo.arc(&cwd).args(["journal", "bind"]));
            }
            "linked" => {
                cwd = repo.home.join("linked");
                git(
                    &repo.root,
                    &["worktree", "add", "--detach", cwd.to_str().unwrap()],
                );
            }
            "separate" => git(
                &repo.root,
                &[
                    "init",
                    "--separate-git-dir",
                    repo.home.join("git-admin").to_str().unwrap(),
                ],
            ),
            "data-root" | "exact" => {
                std::fs::create_dir(&store_parent).unwrap();
                std::os::unix::fs::symlink(&store_parent, &store_alias).unwrap();
                let setting = if layout == "exact" {
                    "ARC_DATA_DIR"
                } else {
                    "ARC_DATA_ROOT"
                };
                environment.push((setting, &store_path));
            }
            _ => {}
        }
        let arc = || {
            let mut command = repo.arc(&cwd);
            command.envs(environment.iter().copied());
            command
        };
        let fresh = || {
            bare(json_stdout_any_status(arc().args([
                "workspace",
                "report",
                "--json",
            ])))
        };
        arc()
            .args(["journal", "log", "registered", "Journal only"])
            .assert()
            .success();

        let mut watch = RunningWatch(repo.spawn_arc_piped_with_env(
            &cwd,
            &["workspace", "watch"],
            &environment,
        ));
        let (lines, reports) = mpsc::channel();
        let output = BufReader::new(watch.0.stdout.take().unwrap());
        let reader = std::thread::spawn(move || {
            for line in output.lines() {
                if lines.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let next = |what: &str| -> Value {
            let line = reports
                .recv_timeout(OWED)
                .unwrap_or_else(|_| panic!("{layout}: the watch printed no report {what}"));
            serde_json::from_str(&line).unwrap()
        };
        let initial = next("at start");
        assert_eq!(initial["projects"].as_array().unwrap().len(), 1, "{layout}");
        assert_eq!(
            initial["sections"]["in_flight"],
            serde_json::json!([]),
            "{layout}"
        );
        assert_eq!(bare(initial.clone()), fresh());

        if layout == "journal-first" {
            git(&cwd, &["init", "-b", "master"]);
            git(
                &cwd,
                &[
                    "-c",
                    "user.name=Tester",
                    "-c",
                    "user.email=tester@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "First",
                ],
            );
        }
        let change = opened_change_id(&stdout(arc().args([
            "begin",
            "first-ledger",
            "--no-worktree",
            "--target",
            "master",
        ])));
        let opened = fresh();
        assert_eq!(
            opened["sections"]["in_flight"][0]["change_id"], change,
            "{layout}"
        );
        assert_eq!(
            bare(next("after the first ledger write")),
            opened,
            "{layout}"
        );

        arc()
            .args(["close", &change, "--abandoned"])
            .assert()
            .success();
        let closed = bare(next("after closing the change"));
        assert_eq!(
            closed["sections"]["in_flight"],
            serde_json::json!([]),
            "{layout}"
        );
        assert_eq!(closed, fresh());

        if layout == "default" {
            let unrelated = Repo::new();
            let other_change = opened_change_id(&stdout(unrelated.arc(&unrelated.root).args([
                "begin",
                "unregistered-here",
                "--no-worktree",
            ])));
            let journal_dir = Path::new(initial["projects"][0]["journal_dir"].as_str().unwrap());
            let silent_file = "20261101T000000Z-unannounced-todo.md";
            std::fs::write(journal_dir.join(silent_file), "# Unannounced\n").unwrap();
            repo.arc(&unrelated.root)
                .args(["close", &other_change, "--abandoned"])
                .assert()
                .success();
            assert!(
                reports.recv_timeout(QUIET).is_err(),
                "unrelated ledger invalidated the registered project"
            );

            let baseline = repo.home.join("initial.json");
            std::fs::write(&baseline, initial.to_string()).unwrap();
            let stdin = watch.0.stdin.as_mut().unwrap();
            request(
                stdin,
                serde_json::json!({"schema": "arc-workspace-watch-request/1", "previous": baseline}),
            );
            let compared = next("after a comparison request");
            assert_eq!(compared["previous"], initial["observation"]["finished_at"]);
            assert_eq!(compared["sections"]["work"], serde_json::json!([]));
            request(
                stdin,
                serde_json::json!({"schema": "arc-workspace-watch-request/1", "refresh": true}),
            );
            assert_eq!(
                next("after a refresh")["sections"]["work"][0]["file"],
                silent_file
            );
        }

        drop(watch.0.stdin.take());
        assert!(watch.0.wait().unwrap().success());
        reader.join().unwrap();
    }
}

/// Claim: a report and its watch attach each library to its own journal,
/// including a library-only update to one of two projects with the same label.
/// Rejects either library being shared with or overwritten by its namesake.
#[test]
fn report_and_watch_keep_same_label_libraries_separate() {
    let first = Repo::new();
    let second = Repo::new();
    let (first_dir, first_file) = journal_artifact(&first, "first", "conclusion", "# First\n");
    let second_body = first.home.join("second.md");
    std::fs::write(&second_body, "# Second\n").unwrap();
    let second_path = stdout(first.arc(&second.root).args([
        "journal",
        "note",
        "second",
        "--kind",
        "conclusion",
        "--body-file",
        second_body.to_str().unwrap(),
    ]));
    let second_path = std::path::Path::new(second_path.trim());
    let second_file = second_path.file_name().unwrap().to_str().unwrap();
    let fresh = || {
        bare(json_stdout_any_status(first.arc(&first.root).args([
            "workspace",
            "report",
            "--library",
            "--json",
        ])))
    };
    let check_libraries = |report: &Value, second_pinned: bool| {
        let projects = report["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 2);
        for (dir, file, pinned) in [
            (first_dir.as_path(), first_file.as_str(), false),
            (second_path.parent().unwrap(), second_file, second_pinned),
        ] {
            let project = projects
                .iter()
                .find(|project| project["journal_dir"] == dir.to_str().unwrap())
                .unwrap();
            assert_eq!(project["project"], "repo");
            let library = project["library"].as_array().unwrap();
            assert_eq!(library.len(), 1);
            assert_eq!(library[0]["file"], file, "library for {}", dir.display());
            assert_eq!(library[0]["path"], dir.join(file).to_str().unwrap());
            assert_eq!(
                library[0].get("pinned").and_then(Value::as_bool),
                pinned.then_some(true)
            );
        }
    };
    let initial = fresh();
    check_libraries(&initial, false);

    let mut watch =
        RunningWatch(first.spawn_arc_piped(&first.root, &["workspace", "watch", "--library"]));
    let (lines, reports) = mpsc::channel();
    let output = BufReader::new(watch.0.stdout.take().unwrap());
    let reader = std::thread::spawn(move || {
        for line in output.lines() {
            if lines.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let next = || bare(serde_json::from_str(&reports.recv_timeout(OWED).unwrap()).unwrap());
    assert_eq!(next(), initial);

    first
        .arc(&second.root)
        .args(["journal", "pin", second_file])
        .assert()
        .success();
    let pinned = next();
    check_libraries(&pinned, true);
    assert_eq!(pinned, fresh());
    drop(watch.0.stdin.take());
    assert!(watch.0.wait().unwrap().success());
    reader.join().unwrap();
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

/// Claim: a watch given `--progress` re-reads a project whose series item is
/// linked into another project's ledger when that ledger is written, so the
/// item's stage in the next report equals a fresh report's. Rejects a watch
/// that re-reads only the project whose ledger was written and keeps the
/// linking project's earlier stage.
#[test]
fn watch_progress_follows_an_item_linked_into_another_project() {
    let repo = Repo::new();
    let other = repo.root.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    for args in [
        &["init", "-b", "master"][..],
        &["config", "user.name", "Tester"],
        &["config", "user.email", "tester@example.invalid"],
        &["config", "commit.gpgsign", "false"],
    ] {
        git(&other, args);
    }
    repo.commit(&other, "README.md", "other\n", "init");
    let other = std::fs::canonicalize(other).unwrap();
    git(&other, &["branch", "arc/remote", "HEAD"]);
    let change = opened_change_id(&stdout(repo.arc(&other).args([
        "begin",
        "remote",
        "--adopt",
        "arc/remote",
    ])));
    repo.arc(&repo.root)
        .args(["progress", "start", "linked", "--title", "Linked"])
        .args(["--item", "Remote"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["progress", "link", "linked", "i1"])
        .arg(format!("other::{change}"))
        .assert()
        .success();

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
    let next = |what: &str| -> Value {
        let line = reports
            .recv_timeout(OWED)
            .unwrap_or_else(|_| panic!("the progress watch printed no report {what}"));
        bare(serde_json::from_str(&line).expect("one JSON report per line"))
    };
    let stage = |report: &Value| -> Value {
        report["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|project| &project["progress"][0]["items"][0])
            .find(|item| item.get("project_slug").is_some())
            .map(|item| item["stage"].clone())
            .unwrap_or_else(|| panic!("no linked item in {report}"))
    };
    assert_eq!(stage(&next("at start")), "opened");

    git(&other, &["checkout", "arc/remote"]);
    repo.commit(&other, "remote.txt", "one\n", "feat: remote");
    stdout(repo.arc(&other).args(["snapshot", "remote"]));
    let snapshotted = next("after the other project's change recorded a patchset");
    assert_eq!(stage(&snapshotted), "snapshotted");

    drop(stdin);
    drop(watch);
    reader.join().unwrap();
}
