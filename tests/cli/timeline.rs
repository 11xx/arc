use crate::common::*;

/// Find the id of the most recent event of a given type in a change's ledger.
fn last_event_id(repo: &Repo, change_id: &str, event_type: &str) -> String {
    let mut paths = fs::read_dir(event_dir(repo, change_id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .rev()
        .find_map(|path| {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            (value["event_type"] == event_type)
                .then(|| value["event_id"].as_str().unwrap().to_string())
        })
        .expect("event type should exist")
}

#[test]
fn log_lists_events_in_chronological_order() {
    let repo = Repo::new();
    let (_id, wt, _head) = change_with_patchset(&repo, "feat-x");
    repo.arc(&wt)
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();

    let out = stdout(repo.arc(&repo.root).args(["log", "feat-x"]));
    let opened = out.find("change-opened").expect("change-opened line");
    let patchset = out.find("patchset-added").expect("patchset-added line");
    let verdict = out.find("verdict-recorded").expect("verdict-recorded line");
    assert!(
        opened < patchset && patchset < verdict,
        "log not in chronological order:\n{out}"
    );

    let reversed = stdout(repo.arc(&repo.root).args(["log", "feat-x", "--reverse"]));
    let r_opened = reversed.find("change-opened").unwrap();
    let r_verdict = reversed.find("verdict-recorded").unwrap();
    assert!(
        r_verdict < r_opened,
        "--reverse should print newest first:\n{reversed}"
    );
}

#[test]
fn show_at_replays_state_as_of_an_event() {
    let repo = Repo::new();
    let (change_id, wt, _head) = change_with_patchset(&repo, "feat-x");
    repo.arc(&wt)
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();
    let approval = last_event_id(&repo, &change_id, "verdict-recorded");

    // A second snapshot advances the head, invalidating the ps-01 approval.
    repo.commit(&wt, "feat-x.txt", "more\n", "feat: more feat-x");
    stdout(repo.arc(&wt).args(["snapshot", "feat-x"]));

    let live = json_stdout(repo.arc(&repo.root).args(["status", "feat-x"]));
    assert_eq!(live["latest_patchset"]["id"], "ps-02");
    assert_eq!(live["verdict"]["valid_for_current_head"], false);

    let as_of = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "feat-x", "--at", &approval]),
    );
    assert_eq!(as_of["state"], "open");
    assert_eq!(as_of["latest_patchset"]["id"], "ps-01");
    assert_eq!(as_of["verdict"]["valid_for_current_head"], true);
}

#[test]
fn show_at_rejects_unknown_event() {
    let repo = Repo::new();
    change_with_patchset(&repo, "feat-x");
    repo.arc(&repo.root)
        .args(["show", "feat-x", "--at", "not-an-event"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown event"));
}

#[test]
fn check_explain_lists_every_blocker_with_first_exit_code() {
    let repo = Repo::new();
    let (_id, wt, _head) = change_with_patchset(&repo, "feat-x");
    // Two blockers at once: no approval and an active hold.
    repo.arc(&wt)
        .args(["hold", "feat-x", "--reason", "waiting on review"])
        .assert()
        .success();

    let assert = repo
        .arc(&repo.root)
        .args(["check", "feat-x", "--explain"])
        .assert()
        .code(3);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(out.contains("valid approval at head"), "{out}");
    assert!(out.contains("no active hold"), "{out}");
    assert!(out.contains("Exit code: 3 (no-valid-approval)"), "{out}");
}

#[test]
fn integrate_dry_run_reports_without_mutating() {
    let repo = Repo::new();
    let (change_id, wt, _head) = change_with_patchset(&repo, "feat-x");
    repo.arc(&wt)
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();

    let events_before = event_count(&repo, &change_id);
    let target_before = repo.head(&repo.root);

    let assert = repo
        .arc(&repo.root)
        .args(["integrate", "feat-x", "--dry-run"])
        .assert()
        .code(0);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(out.contains("would integrate"), "{out}");
    assert!(out.contains("merge result: clean"), "{out}");

    assert_eq!(
        event_count(&repo, &change_id),
        events_before,
        "dry-run must not append events"
    );
    assert_eq!(
        repo.head(&repo.root),
        target_before,
        "dry-run must not move the target branch"
    );

    // The real integration still succeeds and does append the closure event.
    repo.arc(&repo.root)
        .args(["integrate", "feat-x"])
        .assert()
        .success();
    assert!(event_count(&repo, &change_id) > events_before);
}
