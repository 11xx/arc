use super::common::*;

fn event_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .expect("command output should contain an event id")
        .to_string()
}

fn first_change_event_id(repo: &Repo, change_id: &str) -> String {
    let output = stdout(repo.arc(&repo.root).args(["events", "--change", change_id]));
    serde_json::from_str::<serde_json::Value>(output.lines().next().unwrap()).unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn dispatch_stays_open_until_a_terminal_outcome_is_recorded() {
    let repo = Repo::new();
    let dispatched = stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "codex:gpt-5.6-luna#max",
        "--worktree",
        "/tmp/agent-worktree",
    ]));
    let dispatch_id = event_id(&dispatched);

    let open = stdout(repo.arc(&repo.root).args(["run", "list"]));
    assert!(open.contains("open"), "{open}");
    assert!(open.contains("route=codex:gpt-5.6-luna#max"), "{open}");
    assert!(open.contains("worktree=/tmp/agent-worktree"), "{open}");

    let ended =
        stdout(
            repo.arc(&repo.root)
                .args(["run", "end", &dispatch_id, "--outcome", "completed"]),
        );
    let ending_id = event_id(&ended);
    let listed = stdout(repo.arc(&repo.root).args(["run", "list"]));
    assert!(listed.contains("completed"), "{listed}");
    // The dispatch id is the handle `run end` takes, so the row carries it.
    // The ending is a record pointer with no command to hand it to, and it
    // stays in the structured rendering.
    assert!(
        listed.contains(&format!("dispatch={dispatch_id}")),
        "{listed}"
    );
    assert!(!listed.contains(&ending_id), "{listed}");
    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    assert_eq!(rows[0]["ending_event_id"], ending_id, "{rows}");
}

#[test]
fn ending_a_run_twice_names_the_existing_ending() {
    let repo = Repo::new();
    let dispatch_id = event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "local",
        "--worktree",
        "/tmp/run",
    ])));
    let ending_id = event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "end",
        &dispatch_id,
        "--outcome",
        "completed",
    ])));

    let assertion = repo
        .arc(&repo.root)
        .args(["run", "end", &dispatch_id, "--outcome", "stopped"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr);
    assert!(stderr.contains("already ended"), "{stderr}");
    assert!(stderr.contains("completed"), "{stderr}");
    assert!(stderr.contains(&ending_id), "{stderr}");
}

#[test]
fn ending_unknown_or_non_dispatch_ids_has_a_distinct_refusal() {
    let repo = Repo::new();
    let change_id = begin_no_worktree(&repo, "not-a-run", &[]);
    let non_dispatch_id = first_change_event_id(&repo, &change_id);

    let non_dispatch = repo
        .arc(&repo.root)
        .args(["run", "end", &non_dispatch_id, "--outcome", "unknown"])
        .assert()
        .failure();
    let non_dispatch_stderr = String::from_utf8_lossy(&non_dispatch.get_output().stderr);
    assert!(
        non_dispatch_stderr.contains("not a repository-scoped run-dispatched event"),
        "{non_dispatch_stderr}"
    );

    let missing_id = "01NOTHINGHAS THIS ID";
    let missing = repo
        .arc(&repo.root)
        .args(["run", "end", missing_id, "--outcome", "unknown"])
        .assert()
        .failure();
    let missing_stderr = String::from_utf8_lossy(&missing.get_output().stderr);
    assert!(
        missing_stderr.contains("no event has id"),
        "{missing_stderr}"
    );
    assert_ne!(non_dispatch_stderr, missing_stderr);
}

#[test]
fn json_lists_optional_dispatch_fields_and_unknown_is_terminal() {
    let repo = Repo::new();
    let dispatch_id = event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "codex:gpt-5.6-luna#max",
        "--worktree",
        "/tmp/json-worktree",
        "--change",
        "change-123",
        "--brief-event",
        "brief-456",
        "--note",
        "started from the brief",
    ])));

    let open = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    assert_eq!(open.as_array().unwrap().len(), 1);
    let row = &open[0];
    assert_eq!(row["dispatch_event_id"], dispatch_id);
    assert_eq!(row["route"], "codex:gpt-5.6-luna#max");
    assert_eq!(row["worktree"], "/tmp/json-worktree");
    assert_eq!(row["change"], "change-123");
    assert_eq!(row["brief_event_id"], "brief-456");
    assert_eq!(row["note"], "started from the brief");
    assert!(row["ending_event_id"].is_null());
    assert!(row["outcome"].is_null());

    stdout(
        repo.arc(&repo.root)
            .args(["run", "end", &dispatch_id, "--outcome", "unknown"]),
    );
    let ended = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    assert_eq!(ended[0]["outcome"], "unknown");
    assert!(ended[0]["ending_event_id"].is_string());
    assert!(ended[0]["ended_at"].is_string());
}
