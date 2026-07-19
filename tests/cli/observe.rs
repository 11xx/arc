use super::common::*;

#[test]
fn events_replays_parseable_ndjson_and_filters_change_and_type() {
    let repo = Repo::new();
    let first = stdout(repo.arc(&repo.root).args(["begin", "events-a"]));
    let first_id = first
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string();
    stdout(repo.arc(&repo.root).args(["begin", "events-b"]));
    stdout(
        repo.arc(&repo.root)
            .args(["comment", "events-a", "--body", "watch this"]),
    );

    let output = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "events-a",
        "--type",
        "comment-added",
    ]));
    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    let event: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(event["change_id"], first_id);
    assert_eq!(event["event_type"], "comment-added");

    let all = stdout(repo.arc(&repo.root).args(["events"]));
    let ids = all
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect::<Vec<_>>();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
}

#[test]
fn events_since_replays_only_the_suffix() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "events-since"]));
    stdout(
        repo.arc(&repo.root)
            .args(["comment", "events-since", "--body", "first"]),
    );
    let replay = stdout(
        repo.arc(&repo.root)
            .args(["events", "--change", "events-since"]),
    );
    let cursor = replay
        .lines()
        .next()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .unwrap();

    let suffix = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "events-since",
        "--since",
        &cursor,
    ]));
    let ids = suffix
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert!(!ids.is_empty());
    assert!(ids.iter().all(|id| id > &cursor));
}

#[test]
fn events_follow_emits_a_later_snapshot_once() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "events-follow");
    // Create a second patchset after the watcher begins; filtering excludes
    // the first patchset replay so the output must contain exactly one line.
    repo.commit(&worktree, "later.txt", "later\n", "test: later snapshot");
    let mut child = spawn_arc(
        &repo,
        &repo.root,
        &[
            "events",
            "--follow",
            "--change",
            "events-follow",
            "--type",
            "patchset-added",
        ],
    );
    let child_output = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let reader_thread = thread::spawn(move || {
        let mut reader = BufReader::new(child_output);
        let mut first_line = String::new();
        let result = reader.read_line(&mut first_line);
        let _ = sender.send((reader, first_line, result));
    });
    let (mut reader, first_line, first_read) = match receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(result) => result,
        Err(_) => {
            child.kill().unwrap();
            child.wait().unwrap();
            reader_thread.join().unwrap();
            panic!("events --follow did not flush its initial replay");
        }
    };
    reader_thread.join().unwrap();
    assert!(first_read.unwrap() > 0);
    let first: serde_json::Value = serde_json::from_str(first_line.trim()).unwrap();
    assert_eq!(first["patchset_id"], "ps-01");

    // The second snapshot is created only after follow mode has flushed its
    // replay, so a delayed startup cannot make this test pass accidentally.
    stdout(repo.arc(&worktree).args(["snapshot", "events-follow"]));
    thread::sleep(Duration::from_millis(250));
    child.kill().unwrap();
    child.wait().unwrap();
    let mut later = String::new();
    reader.read_to_string(&mut later).unwrap();

    let events = std::iter::once(first_line.as_str())
        .chain(later.lines())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2, "replay plus the later snapshot");
    assert_eq!(events[0]["patchset_id"], "ps-01");
    assert_eq!(events[1]["patchset_id"], "ps-02");
}

#[test]
fn events_follow_exec_observes_each_emitted_event() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "events-exec");
    let observed = repo.root.join("observed-events");
    let command = format!("cat >> {}", observed.display());
    let mut child = spawn_arc(
        &repo,
        &repo.root,
        &[
            "events",
            "--follow",
            "--change",
            "events-exec",
            "--type",
            "patchset-added",
            "--exec",
            &command,
        ],
    );
    for expected in 1..=2 {
        let deadline = Instant::now() + Duration::from_secs(2);
        while fs::read_to_string(&observed)
            .map(|contents| contents.lines().count())
            .unwrap_or(0)
            < expected
        {
            assert!(
                Instant::now() < deadline,
                "hook did not observe event {expected}"
            );
            thread::sleep(Duration::from_millis(25));
        }
        if expected == 1 {
            repo.commit(&worktree, "next.txt", "next\n", "test: next patchset");
            stdout(repo.arc(&worktree).args(["snapshot", "events-exec"]));
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(fs::read_to_string(observed).unwrap().lines().count(), 2);
}

#[test]
fn watch_snapshot_observes_a_later_snapshot() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "watch-snapshot"]));
    let worktree = repo.home.join(".worktrees").join("repo-watch-snapshot");
    assert!(opened.contains("change: watch-snapshot-"));
    repo.commit(
        &worktree,
        "snapshot.txt",
        "snapshot\n",
        "test: add snapshot",
    );

    let mut child = spawn_arc(
        &repo,
        &repo.root,
        &[
            "watch",
            "watch-snapshot",
            "--until",
            "snapshot",
            "--timeout",
            "2",
        ],
    );
    thread::sleep(Duration::from_millis(50));
    stdout(repo.arc(&worktree).args(["snapshot", "watch-snapshot"]));
    assert!(wait_for_exit(&mut child).success());
    assert_eq!(child_stdout(&mut child).trim(), "reached: snapshot");
}

#[test]
fn watch_exec_fires_exactly_once_on_snapshot() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "watch-exec"]));
    let worktree = repo.home.join(".worktrees").join("repo-watch-exec");
    repo.commit(&worktree, "snapshot.txt", "snapshot\n", "test: snapshot");
    let observed = repo.root.join("watch-hook");
    let command = format!("cat >> {}", observed.display());
    let mut child = spawn_arc(
        &repo,
        &repo.root,
        &[
            "watch",
            "watch-exec",
            "--until",
            "snapshot",
            "--timeout",
            "2",
            "--exec",
            &command,
        ],
    );
    thread::sleep(Duration::from_millis(50));
    stdout(repo.arc(&worktree).args(["snapshot", "watch-exec"]));
    assert!(wait_for_exit(&mut child).success());
    let diagnostic: serde_json::Value =
        serde_json::from_str(fs::read_to_string(observed).unwrap().trim()).unwrap();
    assert_eq!(diagnostic["condition"], "snapshot");
    assert_eq!(diagnostic["event_type"], "watch-reached");
}

#[test]
fn watch_multiple_conditions_reports_the_winner() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "watch-many"]));
    repo.arc(&repo.root)
        .args(["claim", "watch-many", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "watch",
            "watch-many",
            "--until",
            "ready,stalled",
            "--timeout",
            "4",
        ])
        .assert()
        .success()
        .stdout("reached: stalled\n");
}

#[test]
fn watch_ready_times_out_when_check_is_not_green() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "watch-ready"]));
    repo.arc(&repo.root)
        .args(["watch", "watch-ready", "--until", "ready", "--timeout", "1"])
        .assert()
        .code(2)
        .stdout("timeout: ready\n");
}

#[test]
fn watch_ready_observes_a_git_only_head_transition() {
    let repo = Repo::new();
    let (_, worktree, head) = change_with_patchset(&repo, "watch-ready-live");
    repo.arc(&worktree)
        .args(["review", "watch-ready-live", "--verdict", "approved"])
        .assert()
        .success();

    let branch = "refs/heads/arc/watch-ready-live";
    let base = git_out(&repo.root, &["rev-parse", "master"]);
    git(&repo.root, &["update-ref", branch, &base]);
    let mut child = spawn_arc(
        &repo,
        &repo.root,
        &[
            "watch",
            "watch-ready-live",
            "--until",
            "ready",
            "--timeout",
            "2",
        ],
    );
    thread::sleep(Duration::from_millis(50));
    // Ref restoration changes readiness without appending a ledger event.
    git(&repo.root, &["update-ref", branch, &head]);

    assert!(wait_for_exit(&mut child).success());
    assert_eq!(child_stdout(&mut child).trim(), "reached: ready");
}

#[test]
fn watch_integrated_returns_after_real_integration() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "watch-integrated");
    repo.arc(&worktree)
        .args(["review", "watch-integrated", "--verdict", "approved"])
        .assert()
        .success();
    let mut child = spawn_arc(
        &repo,
        &repo.root,
        &[
            "watch",
            "watch-integrated",
            "--until",
            "integrated",
            "--timeout",
            "2",
        ],
    );
    thread::sleep(Duration::from_millis(50));
    repo.arc(&repo.root)
        .args(["integrate", "watch-integrated"])
        .assert()
        .success();
    assert!(wait_for_exit(&mut child).success());
    assert_eq!(child_stdout(&mut child).trim(), "reached: integrated");
}

#[test]
fn watch_closed_accepts_abandoned_and_superseded_but_integrated_does_not() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "watch-abandoned"]));
    stdout(repo.arc(&repo.root).args(["begin", "watch-replacement"]));
    stdout(repo.arc(&repo.root).args(["begin", "watch-superseded"]));
    repo.arc(&repo.root)
        .args(["close", "watch-abandoned", "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "close",
            "watch-superseded",
            "--superseded",
            "watch-replacement",
        ])
        .assert()
        .success();

    for change in ["watch-abandoned", "watch-superseded"] {
        repo.arc(&repo.root)
            .args(["watch", change, "--until", "closed", "--timeout", "1"])
            .assert()
            .success()
            .stdout("reached: closed\n");
    }
    repo.arc(&repo.root)
        .args([
            "watch",
            "watch-abandoned",
            "--until",
            "integrated",
            "--timeout",
            "1",
        ])
        .assert()
        .code(2)
        .stdout("timeout: integrated\n");
}
