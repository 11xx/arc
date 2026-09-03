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

/// Every tag reader normalises before matching, so a padded tag that selects a
/// member for one verb must select it for all of them. Timing out proves the
/// member was selected; "no changes match" would prove it was skipped.
#[test]
fn watch_normalises_tags_like_every_other_tag_reader() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "padded", "--tag", "series"]),
    );
    repo.arc(&repo.root)
        .args(["query", "--tag", " series"])
        .assert()
        .success()
        .stdout(predicates::str::contains("padded"));
    repo.arc(&repo.root)
        .args([
            "watch",
            "--tag",
            " series",
            "--any",
            "--until",
            "ready",
            "--timeout",
            "1",
        ])
        .assert()
        .code(2)
        .stdout("timeout: ready\n");
}

/// A lead waiting on a dispatched review wants to resume when a verdict lands,
/// whatever it concluded. `ready` cannot express that — a review asking for
/// changes never satisfies it, so the wait runs to timeout and *still
/// working*, *changes requested*, and *reviewer died* all look identical from
/// outside.
#[test]
fn watch_reviewed_returns_on_any_verdict_including_changes_requested() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "watch-reviewed");

    // Nothing recorded yet: the wait is genuinely waiting.
    repo.arc(&worktree)
        .args([
            "watch",
            "watch-reviewed",
            "--until",
            "reviewed",
            "--timeout",
            "1",
        ])
        .assert()
        .code(2)
        .stdout("timeout: reviewed\n");

    // A verdict that refuses the change still ends the wait: the caller reads
    // the verdict to learn which way it went.
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args([
            "review",
            "watch-reviewed",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
            "--body",
            "not yet",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "watch",
            "watch-reviewed",
            "--until",
            "reviewed",
            "--timeout",
            "4",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("reached: reviewed"));

    // `ready` remains the stricter, different question, and this change does
    // not satisfy it.
    repo.arc(&worktree)
        .args([
            "watch",
            "watch-reviewed",
            "--until",
            "ready",
            "--timeout",
            "1",
        ])
        .assert()
        .code(2);
}

/// A verdict answered by a commit since is not a review of what is there now.
/// Satisfying the wait with it would report a review of code the reviewer
/// never saw.
#[test]
fn watch_reviewed_ignores_a_verdict_on_an_earlier_patchset() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "watch-stale-verdict");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args([
            "review",
            "watch-stale-verdict",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
            "--body",
            "fix this",
        ])
        .assert()
        .success();

    // The change moves on, and the verdict now describes an older tree.
    repo.commit(&worktree, "next.rs", "more\n", "feat: more");
    repo.arc(&worktree)
        .args(["snapshot", "watch-stale-verdict"])
        .assert()
        .success();

    repo.arc(&worktree)
        .args([
            "watch",
            "watch-stale-verdict",
            "--until",
            "reviewed",
            "--timeout",
            "1",
        ])
        .assert()
        .code(2)
        .stdout("timeout: reviewed\n");
}

#[test]
fn watch_approved_returns_on_approval_but_not_changes_requested() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "watch-approved");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args([
            "review",
            "watch-approved",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
            "--body",
            "fix this first",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "watch",
            "watch-approved",
            "--until",
            "approved",
            "--timeout",
            "1",
        ])
        .assert()
        .code(2)
        .stdout("timeout: approved\n");

    let approval = repo
        .arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args(["review", "watch-approved", "--verdict", "approved"])
        .output()
        .unwrap();
    assert!(approval.status.success());
    let approval_event = String::from_utf8_lossy(&approval.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();

    let reached = json_stdout(repo.arc(&worktree).args([
        "watch",
        "watch-approved",
        "--until",
        "approved",
        "--json",
    ]));
    assert_eq!(reached["condition"], "approved", "{reached}");
    assert_eq!(reached["event_id"], approval_event, "{reached}");
}

/// Authority is necessary and not sufficient. A self-approval the repository
/// forbids is the sole tip and is still not something `check` will integrate
/// on, so a wait that returned on it would report ready for a change that
/// cannot merge.
#[test]
fn watch_approved_does_not_return_on_an_approval_policy_refuses() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    let (_, worktree, _) = change_with_patchset(&repo, "watch-selfapproved");
    // The default actor is the one that recorded the patchset, so this is the
    // author approving its own work.
    repo.arc(&worktree)
        .args([
            "review",
            "watch-selfapproved",
            "--verdict",
            "approved",
            "--body",
            "my own work",
        ])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args(["check", "watch-selfapproved"])
        .assert()
        .code(3);
    let out = stdout(repo.arc(&repo.root).args([
        "watch",
        "watch-selfapproved",
        "--until",
        "approved",
        "--timeout",
        "1",
    ]));
    assert!(out.contains("timeout"), "{out}");

    // Somebody else approving the same patchset satisfies both.
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args([
            "review",
            "watch-selfapproved",
            "--verdict",
            "approved",
            "--body",
            "independent",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["check", "watch-selfapproved"])
        .assert()
        .code(0);
    let out = stdout(repo.arc(&repo.root).args([
        "watch",
        "watch-selfapproved",
        "--until",
        "approved",
        "--timeout",
        "1",
    ]));
    assert!(out.contains("reached: approved"), "{out}");
}

#[test]
fn watch_approved_ignores_a_corroborating_approval() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "watch-corroborated");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args([
            "review",
            "watch-corroborated",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
            "--body",
            "fix this first",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args([
            "review",
            "watch-corroborated",
            "--verdict",
            "approved",
            "--relation",
            "corroborates",
        ])
        .assert()
        .success();

    repo.arc(&worktree)
        .args([
            "watch",
            "watch-corroborated",
            "--until",
            "approved",
            "--timeout",
            "1",
        ])
        .assert()
        .code(2)
        .stdout("timeout: approved\n");
    repo.arc(&worktree)
        .args(["check", &change_id, "--json"])
        .assert()
        .code(3)
        .stdout(predicates::str::contains("no-valid-approval"));
}

#[test]
fn watch_approved_reports_provisional_reason_in_human_and_json() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "watch-provisional");
    let reason = "reviewer is an unmeasured\nmodel";
    let approval = repo
        .arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args([
            "review",
            "watch-provisional",
            "--verdict",
            "approved",
            "--provisional",
            reason,
        ])
        .output()
        .unwrap();
    assert!(approval.status.success());
    let approval_event = String::from_utf8_lossy(&approval.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();

    let human =
        stdout(
            repo.arc(&worktree)
                .args(["watch", "watch-provisional", "--until", "approved"]),
        );
    assert_eq!(
        human,
        "reached: approved (provisional: reviewer is an unmeasured model)\n"
    );

    let reached = json_stdout(repo.arc(&worktree).args([
        "watch",
        "watch-provisional",
        "--until",
        "approved",
        "--json",
    ]));
    assert_eq!(reached["event_id"], approval_event, "{reached}");
    assert_eq!(reached["provisional"], reason, "{reached}");
}

#[test]
fn watch_approved_does_not_reach_on_a_contested_verdict_graph() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "watch-contested");
    let first = stdout(repo.arc(&worktree).args([
        "review",
        "watch-contested",
        "--verdict",
        "changes-requested",
        "--cause",
        "executor",
        "--body",
        "fix this first",
    ]));
    let first_event = first
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();
    repo.arc(&worktree)
        .args(["review", "watch-contested", "--verdict", "approved"])
        .assert()
        .success();
    let third =
        stdout(
            repo.arc(&worktree)
                .args(["review", "watch-contested", "--verdict", "approved"]),
        );
    let third_event = third
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();

    let third_path = event_dir(&repo, &change_id).join(format!("{third_event}.json"));
    let mut event: serde_json::Value =
        serde_json::from_slice(&fs::read(&third_path).unwrap()).unwrap();
    event["relation"] = serde_json::json!({
        "kind": "supersedes",
        "observed": [first_event],
    });
    fs::write(third_path, json_file_bytes(&event)).unwrap();

    repo.arc(&worktree)
        .args(["check", &change_id])
        .assert()
        .code(3)
        .stdout(predicates::str::contains("none is authoritative"));
    repo.arc(&worktree)
        .args([
            "watch",
            "watch-contested",
            "--until",
            "approved",
            "--timeout",
            "1",
        ])
        .assert()
        .code(2)
        .stdout("timeout: approved\n");
}

#[test]
fn watch_tag_all_flattens_multiline_provisional_reasons() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "watch-tag-provisional", "--tag", "series"]),
    );
    let worktree = repo.home.join(".worktrees/repo-watch-tag-provisional");
    repo.commit(
        &worktree,
        "watch-tag-provisional.txt",
        "watch-tag-provisional\n",
        "test: add tagged provisional change",
    );
    repo.arc(&worktree)
        .args(["snapshot", "watch-tag-provisional"])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args([
            "review",
            "watch-tag-provisional",
            "--verdict",
            "approved",
            "--provisional",
            "first line\nsecond line",
        ])
        .assert()
        .success();

    let human = stdout(
        repo.arc(&repo.root)
            .args(["watch", "--tag", "series", "--all", "--until", "approved"]),
    );
    assert_eq!(
        human,
        "reached: approved (1 changes; provisional: first line second line)\n"
    );
}

#[test]
fn watch_approved_ignores_approval_on_a_superseded_patchset() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "watch-stale-approval");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args(["review", "watch-stale-approval", "--verdict", "approved"])
        .assert()
        .success();

    repo.commit(&worktree, "later.rs", "more\n", "feat: more");
    repo.arc(&worktree)
        .args(["snapshot", "watch-stale-approval"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "watch",
            "watch-stale-approval",
            "--until",
            "approved",
            "--timeout",
            "1",
        ])
        .assert()
        .code(2)
        .stdout("timeout: approved\n");
}

#[test]
fn watch_gates_green_waits_for_every_required_gate() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.alpha]\ncommand = \"true\"\n[gates.beta]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare watch gates"]);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "watch-gates", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["verify", "watch-gates", "--gate", "alpha"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "watch",
            "watch-gates",
            "--until",
            "gates-green",
            "--timeout",
            "1",
        ])
        .assert()
        .code(2)
        .stdout("timeout: gates-green\n");

    repo.arc(&repo.root)
        .args(["verify", "watch-gates", "--gate", "beta"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["watch", "watch-gates", "--until", "gates-green"])
        .assert()
        .success()
        .stdout("reached: gates-green\n");
}

#[test]
fn watch_blocked_names_the_latest_blocked_on_event() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "watch-blocked", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["claim", "watch-blocked"])
        .assert()
        .success();
    let first = stdout(repo.arc(&repo.root).args([
        "stage",
        "watch-blocked",
        "blocked-on",
        "--note",
        "waiting for input",
        "--blocker",
        "external",
    ]));
    assert!(first.contains("event: "), "{first}");
    let latest = stdout(repo.arc(&repo.root).args([
        "stage",
        "watch-blocked",
        "blocked-on",
        "--note",
        "still waiting",
        "--blocker",
        "external",
    ]));
    let latest_event = latest
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();

    let reached = json_stdout(repo.arc(&repo.root).args([
        "watch",
        "watch-blocked",
        "--until",
        "blocked",
        "--json",
    ]));
    assert_eq!(reached["condition"], "blocked", "{reached}");
    assert_eq!(reached["event_id"], latest_event, "{reached}");
}

#[test]
fn watch_brief_recorded_names_the_brief_event() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "watch-brief", "--no-worktree"]),
    );
    let brief = repo
        .arc(&repo.root)
        .args(["brief", "watch-brief", "--body-file", "-"])
        .write_stdin("record the contract\n")
        .output()
        .unwrap();
    assert!(brief.status.success());
    let brief_event = String::from_utf8_lossy(&brief.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();

    let reached = json_stdout(repo.arc(&repo.root).args([
        "watch",
        "watch-brief",
        "--until",
        "brief-recorded",
        "--json",
    ]));
    assert_eq!(reached["condition"], "brief-recorded", "{reached}");
    assert_eq!(reached["event_id"], brief_event, "{reached}");
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

#[test]
fn watch_tag_any_names_the_first_member_to_reach_a_condition() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "quiet-one", "--tag", "series"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "stalls-fast", "--tag", "series"]),
    );
    // Only the second member gets a claim that can outlive its stage budget, so
    // a passing `--any` must have selected it rather than reported the set.
    repo.arc(&repo.root)
        .args(["claim", "stalls-fast", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    let out = stdout(repo.arc(&repo.root).args([
        "watch",
        "--tag",
        "series",
        "--until",
        "stalled",
        "--any",
        "--timeout",
        "6",
    ]));
    assert!(
        out.starts_with("reached: stalled (stalls-fast-"),
        "expected the stalled member to be named, got {out:?}"
    );
}

#[test]
fn watch_tag_all_waits_for_every_member() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "first", "--tag", "both"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "second", "--tag", "both"]),
    );
    repo.arc(&repo.root)
        .args(["claim", "first", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    // One stalled member must not satisfy `--all`; the watch times out at 2.
    repo.arc(&repo.root)
        .args([
            "watch",
            "--tag",
            "both",
            "--until",
            "stalled",
            "--all",
            "--timeout",
            "3",
        ])
        .assert()
        .code(2)
        .stdout("timeout: stalled\n");
    repo.arc(&repo.root)
        .args(["claim", "second", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "watch",
            "--tag",
            "both",
            "--until",
            "stalled",
            "--all",
            "--timeout",
            "6",
        ])
        .assert()
        .success()
        .stdout("reached: stalled (2 changes)\n");
}

#[test]
fn watch_scope_and_quorum_misuse_is_refused() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "scoped", "--tag", "series"]),
    );
    for (args, expected) in [
        (
            vec!["watch", "--until", "stalled"],
            "requires <CHANGE> or --tag",
        ),
        (
            vec!["watch", "scoped", "--tag", "series", "--until", "stalled"],
            "select different scopes",
        ),
        (
            vec!["watch", "--tag", "series", "--until", "stalled"],
            "--tag requires --any or --all",
        ),
        (
            vec!["watch", "scoped", "--any", "--until", "stalled"],
            "apply to --tag, not a single change",
        ),
        (
            vec!["watch", "--tag", "absent", "--any", "--until", "stalled"],
            "no changes match tags absent",
        ),
    ] {
        repo.arc(&repo.root)
            .args(&args)
            .assert()
            .failure()
            .stderr(predicates::str::contains(expected));
    }
}

/// A watch is something a script waits on, so its outcome has to be readable
/// without parsing prose — including which event satisfied it, when one did.
#[test]
fn watch_json_names_the_change_the_condition_and_the_event() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "watched"]));
    let wt = repo.home.join(".worktrees/repo-watched");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    let snapshot = stdout(repo.arc(&wt).args(["snapshot", "watched"]));
    let event_id = snapshot
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();

    let out = stdout(
        repo.arc(&wt)
            .args(["watch", "watched", "--until", "snapshot", "--json"]),
    );
    let value: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(value["event_type"], "watch-reached");
    assert_eq!(value["condition"], "snapshot");
    assert_eq!(value["event_id"], event_id, "{value}");
    assert!(value["change_id"].as_str().unwrap().starts_with("watched-"));

    // A timeout is an outcome a script branches on, so it is JSON too.
    let out = repo
        .arc(&wt)
        .args([
            "watch",
            "watched",
            "--until",
            "integrated",
            "--timeout",
            "1",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let value: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(value["event_type"], "watch-timeout");
}

/// A condition derived from elapsed time or from policy is satisfied by no
/// event, and a field that otherwise holds an event ID should not sometimes
/// hold a placeholder.
#[test]
fn watch_json_omits_the_event_id_for_a_derived_condition() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "derived"]));
    let wt = repo.home.join(".worktrees/repo-derived");
    repo.commit(&wt, "work.rs", "done\n", "feat: work");
    stdout(repo.arc(&wt).args(["snapshot", "derived"]));
    repo.arc(&wt)
        .args(["review", "derived", "--verdict", "approved"])
        .assert()
        .success();

    let out = stdout(repo.arc(&wt).args([
        "watch",
        "derived",
        "--until",
        "ready",
        "--timeout",
        "10",
        "--json",
    ]));
    let value: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(value["condition"], "ready", "{value}");
    assert!(value.get("event_id").is_none(), "{value}");
}

/// A tagged program is the unit an orchestrator waits on, and following each
/// member separately loses the interleaving that makes the stream worth
/// reading.
#[test]
fn events_can_follow_a_tagged_program() {
    let repo = Repo::new();
    begin_no_worktree(&repo, "member", &["--tag", "program"]);
    begin_no_worktree(&repo, "outsider", &[]);

    let tagged = stdout(repo.arc(&repo.root).args(["events", "--tag", "program"]));
    assert!(tagged.contains("member"), "{tagged}");
    assert!(!tagged.contains("outsider"), "{tagged}");

    // Membership is what it is when the stream is read, not when it started.
    repo.arc(&repo.root)
        .args(["metadata", "outsider", "--tag", "program"])
        .assert()
        .success();
    let tagged = stdout(repo.arc(&repo.root).args(["events", "--tag", "program"]));
    assert!(tagged.contains("outsider"), "{tagged}");

    // A change and a tag are different scopes.
    repo.arc(&repo.root)
        .args(["events", "--change", "member", "--tag", "program"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("different scopes"));
}

/// Integrate a change owing a review, recorded as opened from `harness`.
fn debt_opened_from(repo: &Repo, slug: &str, harness: &str) -> String {
    let opened = stdout(
        repo.arc(&repo.root)
            .env("ARC_HARNESS", harness)
            .args(["begin", slug]),
    );
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&worktree, &format!("{slug}.txt"), "work\n", "feat: work");
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&repo.root)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", slug, "--debt", "no reviewer reachable"])
        .assert()
        .success();
    opened_change_id(&opened)
}

/// Every session exports its identity, so a read consulting it would answer a
/// different question in each harness — and `--debt`, which enumerates
/// obligations, would hide the ones another harness opened. Filters come only
/// from flags the caller typed.
#[test]
fn query_filters_on_typed_flags_not_the_exported_identity() {
    let repo = Repo::new();
    let mine = debt_opened_from(&repo, "opened-here", "test");
    let theirs = debt_opened_from(&repo, "opened-elsewhere", "codex");

    // The fixture exports ARC_HARNESS=test, matching exactly one of the two.
    let listed = json_stdout(repo.arc(&repo.root).args(["query", "--json"]));
    let ids = |value: &serde_json::Value| {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|change| change["change_id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&listed).len(), 2, "{listed}");
    assert!(ids(&listed).contains(&theirs), "{listed}");

    let filtered =
        json_stdout(
            repo.arc(&repo.root)
                .args(["query", "--harness", "codex", "--json"]),
        );
    assert_eq!(ids(&filtered), vec![theirs.clone()], "{filtered}");

    // A typed flag outranks the exported identity in both directions.
    let filtered = json_stdout(repo.arc(&repo.root).env("ARC_HARNESS", "codex").args([
        "query",
        "--harness",
        "test",
        "--json",
    ]));
    assert_eq!(ids(&filtered), vec![mine.clone()], "{filtered}");

    let debts = json_stdout(repo.arc(&repo.root).args(["query", "--debt", "--json"]));
    assert_eq!(ids(&debts).len(), 2, "{debts}");
    assert!(ids(&debts).contains(&theirs), "{debts}");

    // The identity still reaches the ledger: it is recorded, not consulted.
    let opened_harnesses = json_stdout(repo.arc(&repo.root).args(["show", &theirs, "--json"]));
    assert_eq!(opened_harnesses["opened_harness"], "codex");
}
