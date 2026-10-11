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
        "--change",
        "agent-change",
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
        "--change",
        "run-change",
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
    let change_id = begin_without_checkout(&repo, "not-a-run", &[]);
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

/// A dispatch names exactly one subject. Neither refusal is generic: a caller
/// who named none and a caller who named two made different mistakes.
#[test]
fn dispatch_takes_exactly_one_subject() {
    let repo = Repo::new();
    let none = repo
        .arc(&repo.root)
        .args(["run", "dispatch", "--route", "r", "--worktree", "w"])
        .assert()
        .failure();
    let none_stderr = String::from_utf8_lossy(&none.get_output().stderr);
    assert!(none_stderr.contains("--change"), "{none_stderr}");
    assert!(none_stderr.contains("--fork"), "{none_stderr}");
    assert!(none_stderr.contains("--range"), "{none_stderr}");

    let two = repo
        .arc(&repo.root)
        .args([
            "run",
            "dispatch",
            "--route",
            "r",
            "--worktree",
            "w",
            "--change",
            "c",
            "--fork",
            "f",
        ])
        .assert()
        .failure();
    let two_stderr = String::from_utf8_lossy(&two.get_output().stderr);
    assert!(two_stderr.contains("one subject"), "{two_stderr}");
    assert!(two_stderr.contains("--change and --fork"), "{two_stderr}");

    let malformed = repo
        .arc(&repo.root)
        .args([
            "run",
            "dispatch",
            "--route",
            "r",
            "--worktree",
            "w",
            "--range",
            "justonerev",
        ])
        .assert()
        .failure();
    let malformed_stderr = String::from_utf8_lossy(&malformed.get_output().stderr);
    assert!(
        malformed_stderr.contains("<base>..<head>"),
        "{malformed_stderr}"
    );

    assert!(
        stdout(repo.arc(&repo.root).args(["run", "list"])).contains("no runs"),
        "a refused dispatch records nothing"
    );
}

#[test]
fn a_fork_and_a_range_are_dispatch_subjects_like_a_change() {
    let repo = Repo::new();
    let fork = stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "r",
        "--worktree",
        "w",
        "--fork",
        "spool-spike",
    ]));
    assert!(fork.contains("round: 1 of fork spool-spike"), "{fork}");
    let range = stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "r",
        "--worktree",
        "w",
        "--range",
        "abc123..def456",
    ]));
    assert!(
        range.contains("round: 1 of range abc123..def456"),
        "{range}"
    );

    let listed = stdout(repo.arc(&repo.root).args(["run", "list"]));
    assert!(listed.contains("fork spool-spike  1 round(s)"), "{listed}");
    assert!(
        listed.contains("range abc123..def456  1 round(s)"),
        "{listed}"
    );

    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    let subjects: Vec<&serde_json::Value> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["subject"])
        .collect();
    assert!(
        subjects
            .iter()
            .any(|s| s["kind"] == "fork" && s["slug"] == "spool-spike"),
        "{rows}"
    );
    assert!(
        subjects
            .iter()
            .any(|s| s["kind"] == "range" && s["base"] == "abc123" && s["head"] == "def456"),
        "{rows}"
    );
    assert!(
        rows.as_array().unwrap().iter().all(|r| r["round"] == 1),
        "{rows}"
    );
}

/// The round is the ordinal of a dispatch within its subject, so a second
/// dispatch against the same fork is round 2 while an unrelated subject
/// restarts at 1.
#[test]
fn rounds_are_numbered_within_their_subject() {
    let repo = Repo::new();
    let dispatch = |subject: &str, value: &str| {
        event_id(&stdout(repo.arc(&repo.root).args([
            "run",
            "dispatch",
            "--route",
            "r",
            "--worktree",
            "w",
            subject,
            value,
        ])))
    };
    let first = dispatch("--fork", "loop");
    stdout(
        repo.arc(&repo.root)
            .args(["run", "end", &first, "--outcome", "completed"]),
    );
    let second = dispatch("--fork", "loop");
    dispatch("--fork", "other");

    let listed = stdout(repo.arc(&repo.root).args(["run", "list"]));
    assert!(listed.contains("fork loop  2 round(s)"), "{listed}");
    assert!(listed.contains("fork other  1 round(s)"), "{listed}");

    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    let round_of = |id: &str| {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["dispatch_event_id"] == id)
            .unwrap()["round"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(round_of(&first), 1, "{rows}");
    assert_eq!(round_of(&second), 2, "{rows}");
}

#[test]
fn an_ending_records_what_the_round_reviewed_raised_and_deferred() {
    let repo = Repo::new();
    let dispatch_id = event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "r",
        "--worktree",
        "w",
        "--fork",
        "loop",
    ])));
    let raised = repo.root.join("raised.json");
    std::fs::write(
        &raised,
        r#"[{"summary": "the reducer drops the field", "severity": "major"},
            {"summary": "the help text lies"}]"#,
    )
    .unwrap();
    let deferred = repo.root.join("deferred.json");
    std::fs::write(
        &deferred,
        r#"[{"summary": "the listing is O(n^2)", "why": "n is under ten in every real ledger"}]"#,
    )
    .unwrap();

    let ended = stdout(repo.arc(&repo.root).args([
        "run",
        "end",
        &dispatch_id,
        "--outcome",
        "completed",
        "--reviewed-head",
        "0123456789abcdef0123456789abcdef01234567",
        "--raised-json",
        raised.to_str().unwrap(),
        "--deferred-json",
        deferred.to_str().unwrap(),
    ]));
    // An ID is minted for a deferral that named none, and printed, because it
    // is the handle a later round collects by.
    let deferral_id = ended
        .lines()
        .find_map(|line| line.strip_prefix("deferred: "))
        .and_then(|rest| rest.split_whitespace().next())
        .expect("the ending prints each deferral's id")
        .to_string();
    assert!(deferral_id.starts_with("def-"), "{ended}");

    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    let row = &rows[0];
    assert_eq!(
        row["reviewed_head"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(row["raised"].as_array().unwrap().len(), 2, "{rows}");
    assert_eq!(row["raised"][0]["severity"], "major", "{rows}");
    assert!(row["raised"][1]["severity"].is_null(), "{rows}");
    assert_eq!(row["deferred"][0]["id"], deferral_id, "{rows}");
    assert_eq!(
        row["deferred"][0]["why"], "n is under ten in every real ledger",
        "{rows}"
    );

    let listed = stdout(repo.arc(&repo.root).args(["run", "list"]));
    assert!(listed.contains("reviewed=0123456"), "{listed}");
    assert!(listed.contains("raised=2"), "{listed}");
    assert!(listed.contains("open-deferrals=1"), "{listed}");
    assert!(
        listed.contains(&format!("open {deferral_id}: the listing is O(n^2)")),
        "{listed}"
    );
}

/// A deferral is open until a later round on the same subject collects it,
/// which is the whole point of recording one: the next round inherits a list
/// instead of a memory.
#[test]
fn a_later_round_collects_a_deferral_and_closes_it() {
    let repo = Repo::new();
    let dispatch = || {
        event_id(&stdout(repo.arc(&repo.root).args([
            "run",
            "dispatch",
            "--route",
            "r",
            "--worktree",
            "w",
            "--fork",
            "loop",
        ])))
    };
    let first = dispatch();
    let deferred = repo.root.join("deferred.json");
    std::fs::write(
        &deferred,
        r#"[{"id": "def-known", "summary": "the O(n^2) listing", "why": "not now"}]"#,
    )
    .unwrap();
    stdout(repo.arc(&repo.root).args([
        "run",
        "end",
        &first,
        "--outcome",
        "completed",
        "--deferred-json",
        deferred.to_str().unwrap(),
    ]));
    assert!(
        stdout(repo.arc(&repo.root).args(["run", "list"])).contains("open def-known"),
        "a deferral nobody collected is still open"
    );

    let second = dispatch();
    stdout(repo.arc(&repo.root).args([
        "run",
        "end",
        &second,
        "--outcome",
        "completed",
        "--collects",
        "def-known",
    ]));

    let listed = stdout(repo.arc(&repo.root).args(["run", "list"]));
    assert!(!listed.contains("open def-known"), "{listed}");
    assert!(listed.contains("collects=def-known"), "{listed}");
    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    let first_row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["dispatch_event_id"] == first)
        .unwrap();
    assert_eq!(first_row["deferred"].as_array().unwrap().len(), 1, "{rows}");
    assert!(first_row["open_deferrals"].is_null(), "{rows}");
}

/// Collection is scoped to the subject, and a deferral that was never open on
/// it cannot be reported discharged.
#[test]
fn collecting_refuses_an_id_that_is_not_open_on_this_subject() {
    let repo = Repo::new();
    let deferred = repo.root.join("deferred.json");
    std::fs::write(
        &deferred,
        r#"[{"id": "def-elsewhere", "summary": "a finding", "why": "not now"}]"#,
    )
    .unwrap();
    let elsewhere = event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "r",
        "--worktree",
        "w",
        "--fork",
        "elsewhere",
    ])));
    stdout(repo.arc(&repo.root).args([
        "run",
        "end",
        &elsewhere,
        "--outcome",
        "completed",
        "--deferred-json",
        deferred.to_str().unwrap(),
    ]));

    let here = event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "r",
        "--worktree",
        "w",
        "--fork",
        "here",
    ])));
    let refused = repo
        .arc(&repo.root)
        .args([
            "run",
            "end",
            &here,
            "--outcome",
            "completed",
            "--collects",
            "def-elsewhere",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr);
    assert!(
        stderr.contains("no open deferral def-elsewhere"),
        "{stderr}"
    );
    assert!(stderr.contains("fork here"), "{stderr}");
    // The refusal left the dispatch open rather than half-ending it.
    assert!(
        stdout(repo.arc(&repo.root).args(["run", "list"])).contains("round 1  open"),
        "the refused ending recorded nothing"
    );
}

/// A deferral without a reason cannot be told from a finding that was missed,
/// so the reason is required rather than encouraged.
#[test]
fn a_deferral_without_a_reason_is_refused() {
    let repo = Repo::new();
    let dispatch_id = event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "r",
        "--worktree",
        "w",
        "--fork",
        "loop",
    ])));
    let deferred = repo.root.join("deferred.json");
    std::fs::write(&deferred, r#"[{"summary": "a finding"}]"#).unwrap();
    let refused = repo
        .arc(&repo.root)
        .args([
            "run",
            "end",
            &dispatch_id,
            "--outcome",
            "completed",
            "--deferred-json",
            deferred.to_str().unwrap(),
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr);
    assert!(stderr.contains("needs a why"), "{stderr}");
    assert!(
        stdout(repo.arc(&repo.root).args(["run", "list"])).contains("round 1  open"),
        "a refused ending records nothing"
    );
}

fn dispatch_against(repo: &Repo, subject: &str, value: &str) -> String {
    event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "codex:model-a#high",
        "--worktree",
        "/work/run",
        subject,
        value,
    ])))
}

fn file_todo(repo: &Repo, topic: &str) -> String {
    let path = stdout(
        repo.arc(&repo.root)
            .args(["journal", "todo", topic, "--body-file", "-"])
            .write_stdin("# Work item\n\nBody.\n"),
    );
    Path::new(path.trim())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

fn row_for<'a>(rows: &'a serde_json::Value, dispatch_id: &str) -> &'a serde_json::Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row["dispatch_event_id"] == dispatch_id)
        .unwrap_or_else(|| panic!("no row for {dispatch_id}: {rows}"))
}

fn bind(repo: &Repo, dispatch_id: &str, extra: &[&str]) -> AssertCommand {
    let mut command = repo.arc(&repo.root);
    command.args(["run", "bind", dispatch_id]).args(extra);
    command
}

#[test]
fn a_dispatch_against_a_change_is_listed_unbound() {
    let repo = Repo::new();
    let dispatch_id = dispatch_against(&repo, "--change", "bound-change");
    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    let row = row_for(&rows, &dispatch_id);
    assert_eq!(row["subject"]["kind"], "change", "{rows}");
    assert_eq!(row["subject"]["id"], "bound-change", "{rows}");
    assert!(row["binding"].is_null(), "{rows}");
    assert!(row.get("journal").is_none(), "{rows}");
    let listed = stdout(repo.arc(&repo.root).args(["run", "list"]));
    assert!(listed.contains("unbound"), "{listed}");
}

#[test]
fn a_dispatch_against_a_journal_artifact_names_it_as_the_subject() {
    let repo = Repo::new();
    let file = file_todo(&repo, "journal-subject");
    let dispatched = stdout(repo.arc(&repo.root).args([
        "run",
        "dispatch",
        "--route",
        "r",
        "--worktree",
        "w",
        "--journal",
        &file,
    ]));
    assert!(
        dispatched.contains(&format!("round: 1 of journal {file}")),
        "{dispatched}"
    );
    let dispatch_id = event_id(&dispatched);
    let second = dispatch_against(&repo, "--journal", &file);

    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    let row = row_for(&rows, &dispatch_id);
    assert_eq!(row["subject"]["kind"], "journal", "{rows}");
    assert_eq!(row["subject"]["file"], file.as_str(), "{rows}");
    assert_eq!(row["journal"]["file"], file.as_str(), "{rows}");
    assert!(
        row["journal"]["digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "{rows}"
    );
    assert!(row["change"].is_null(), "{rows}");
    assert_eq!(row_for(&rows, &second)["round"], 2, "{rows}");
    let listed = stdout(repo.arc(&repo.root).args(["run", "list"]));
    assert!(
        listed.contains(&format!("journal {file}  2 round(s)")),
        "{listed}"
    );

    // Still one subject per dispatch, and only an artifact resolves.
    let two = repo
        .arc(&repo.root)
        .args([
            "run",
            "dispatch",
            "--route",
            "r",
            "--worktree",
            "w",
            "--change",
            "c",
            "--journal",
            &file,
        ])
        .assert()
        .failure();
    let two_stderr = String::from_utf8_lossy(&two.get_output().stderr);
    assert!(
        two_stderr.contains("--change and --journal"),
        "{two_stderr}"
    );
    for missing in [
        "20260101T000000Z-absent-todo.md",
        "notes/20260101T000000Z-absent-todo.md",
    ] {
        repo.arc(&repo.root)
            .args([
                "run",
                "dispatch",
                "--route",
                "r",
                "--worktree",
                "w",
                "--journal",
                missing,
            ])
            .assert()
            .failure();
    }
    let none = repo
        .arc(&repo.root)
        .args(["run", "dispatch", "--route", "r", "--worktree", "w"])
        .assert()
        .failure();
    let none_stderr = String::from_utf8_lossy(&none.get_output().stderr);
    assert!(none_stderr.contains("--journal"), "{none_stderr}");
    let after = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    assert_eq!(after.as_array().unwrap().len(), 2, "{after}");
}

#[test]
fn a_binding_written_after_launch_and_after_the_ending_is_listed() {
    let repo = Repo::new();
    let dispatch_id = dispatch_against(&repo, "--change", "late-binding");
    let ending = event_id(&stdout(repo.arc(&repo.root).args([
        "run",
        "end",
        &dispatch_id,
        "--outcome",
        "completed",
    ])));
    let bound = stdout(&mut bind(
        &repo,
        &dispatch_id,
        &[
            "--executor-harness",
            "codex",
            "--executor-session",
            "exec-session-1",
            "--handle",
            "delegate-handle-1",
        ],
    ));
    let binding_id = event_id(&bound);
    assert_ne!(binding_id, ending);

    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    let row = row_for(&rows, &dispatch_id);
    assert_eq!(row["binding"]["event_id"], binding_id.as_str(), "{rows}");
    assert_eq!(row["binding"]["executor_harness"], "codex", "{rows}");
    assert_eq!(
        row["binding"]["executor_session"], "exec-session-1",
        "{rows}"
    );
    assert_eq!(row["binding"]["handle"], "delegate-handle-1", "{rows}");
    assert!(row["binding"]["bound_at"].is_string(), "{rows}");
    assert_eq!(row["outcome"], "completed", "{rows}");
    let listed = stdout(repo.arc(&repo.root).args(["run", "list"]));
    assert!(
        listed.contains("executor=codex session=exec-session-1 handle=delegate-handle-1"),
        "{listed}"
    );

    // A binding to anything but a dispatch refuses, as an ending does.
    let refused = bind(
        &repo,
        &ending,
        &["--executor-harness", "codex", "--executor-session", "s"],
    )
    .assert()
    .failure();
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr);
    assert!(stderr.contains("not a run-dispatched event"), "{stderr}");
    bind(
        &repo,
        "01NOTHINGHASTHISID",
        &["--executor-harness", "codex", "--executor-session", "s"],
    )
    .assert()
    .failure();
}

#[test]
fn rebinding_identical_values_records_nothing_and_different_values_refuse() {
    let repo = Repo::new();
    let dispatch_id = dispatch_against(&repo, "--fork", "rebind");
    let values = [
        "--executor-harness",
        "claude",
        "--executor-session",
        "exec-session-2",
    ];
    let first = event_id(&stdout(&mut bind(&repo, &dispatch_id, &values)));
    let repeated = stdout(&mut bind(&repo, &dispatch_id, &values));
    assert_eq!(event_id(&repeated), first, "{repeated}");
    assert!(repeated.contains("already bound"), "{repeated}");

    for different in [
        &[
            "--executor-harness",
            "claude",
            "--executor-session",
            "exec-session-3",
        ][..],
        &[
            "--executor-harness",
            "codex",
            "--executor-session",
            "exec-session-2",
        ][..],
        &[
            "--executor-harness",
            "claude",
            "--executor-session",
            "exec-session-2",
            "--handle",
            "added-later",
        ][..],
        &[
            "--executor-harness",
            "claude",
            "--executor-session",
            "exec-session-2",
            "--forked-from",
            "origin-added-later",
        ][..],
    ] {
        let refused = bind(&repo, &dispatch_id, different).assert().failure();
        let stderr = String::from_utf8_lossy(&refused.get_output().stderr);
        assert!(stderr.contains("already bound"), "{stderr}");
        assert!(stderr.contains(&first), "{stderr}");
    }
    let events = stdout(repo.arc(&repo.root).args(["events", "--repository"]));
    assert_eq!(
        events.matches("\"run-bound\"").count(),
        1,
        "exactly one binding is recorded: {events}"
    );
}

#[test]
fn imported_binding_tuples_expose_conflicts_and_equal_observations_are_harmless() {
    for field in ["executor_session", "executor_harness", "handle", "equal"] {
        let repo = Repo::new();
        let change = begin_without_checkout(&repo, "binding-import", &[]);
        let dispatch = dispatch_against(&repo, "--change", &change);
        stdout(&mut bind(
            &repo,
            &dispatch,
            &[
                "--executor-harness",
                "codex",
                "--executor-session",
                "executor-one",
                "--handle",
                "handle-one",
            ],
        ));
        let path = repo.home.join("bundle.json");
        stdout(
            repo.arc(&repo.root)
                .args(["export", &change, "--output", path.to_str().unwrap()]),
        );
        let mut bundle: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let events = bundle["repository_events"].as_array_mut().unwrap();
        let mut imported = events
            .iter()
            .find(|e| e["event_type"] == "run-bound")
            .unwrap()
            .clone();
        imported["event_id"] = serde_json::json!("01K0000000000000000000000Z");
        if field != "equal" {
            imported[field] = serde_json::json!("different");
        }
        events.push(imported);
        fs::write(&path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        stdout(
            repo.arc(&repo.root)
                .args(["import", path.to_str().unwrap()]),
        );
        let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
        let binding = &row_for(&rows, &dispatch)["binding"];
        let text = stdout(repo.arc(&repo.root).args(["run", "list"]));
        if field == "equal" {
            assert!(binding["conflicts"].is_null(), "{rows}");
            assert!(!text.contains("binding conflict"), "{text}");
        } else {
            assert_eq!(
                binding["conflicts"].as_array().map(Vec::len),
                Some(1),
                "{rows}"
            );
            assert!(text.contains("binding conflict"), "{text}");
            assert!(text.contains("different"), "{text}");
            bind(
                &repo,
                &dispatch,
                &[
                    "--executor-harness",
                    "codex",
                    "--executor-session",
                    "executor-one",
                    "--handle",
                    "handle-one",
                ],
            )
            .assert()
            .failure();
        }
        let events = stdout(repo.arc(&repo.root).args(["events", "--repository"]));
        assert_eq!(events.matches("\"run-bound\"").count(), 2, "{events}");
    }
}

#[test]
fn binding_private_locators_are_refused_without_echoing_them() {
    let repo = Repo::new();
    let dispatch = dispatch_against(&repo, "--change", "private-binding");
    for flag in ["--executor-harness", "--executor-session", "--handle"] {
        for locator in [
            "/fixture/private/session.jsonl",
            "~/private/session.jsonl",
            "private/session.jsonl",
        ] {
            let mut args = vec![
                "--executor-harness",
                "codex",
                "--executor-session",
                "session-one",
            ];
            if flag == "--handle" {
                args.extend([flag, locator]);
            } else {
                let index = args.iter().position(|value| *value == flag).unwrap();
                args[index + 1] = locator;
            }
            let output = bind(&repo, &dispatch, &args).output().unwrap();
            assert!(!output.status.success(), "{flag} accepted a locator");
            assert!(!String::from_utf8_lossy(&output.stderr).contains(locator));
        }
    }
    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    assert!(row_for(&rows, &dispatch)["binding"].is_null());
}

/// A follow-up or resume is its own dispatch with its own binding, so two
/// dispatches bound to one executor session are two spans of one scope.
#[test]
fn a_follow_up_opens_a_second_span_on_the_same_session() {
    let repo = Repo::new();
    let first = dispatch_against(&repo, "--change", "follow-up");
    let values = [
        "--executor-harness",
        "codex",
        "--executor-session",
        "shared-session",
        "--handle",
        "h-1",
    ];
    stdout(&mut bind(&repo, &first, &values));
    stdout(
        repo.arc(&repo.root)
            .args(["run", "end", &first, "--outcome", "completed"]),
    );
    let second = dispatch_against(&repo, "--change", "follow-up");
    stdout(&mut bind(&repo, &second, &values));

    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    let (first_row, second_row) = (row_for(&rows, &first), row_for(&rows, &second));
    assert_eq!(first_row["round"], 1, "{rows}");
    assert_eq!(second_row["round"], 2, "{rows}");
    for row in [first_row, second_row] {
        assert_eq!(
            row["binding"]["executor_session"], "shared-session",
            "{rows}"
        );
    }
    assert_ne!(
        first_row["binding"]["event_id"], second_row["binding"]["event_id"],
        "{rows}"
    );
}

#[test]
fn a_placeholder_or_malformed_binding_refuses_and_records_nothing() {
    let repo = Repo::new();
    let dispatch_id = dispatch_against(&repo, "--change", "placeholder-binding");
    let cases: [(&[&str], &str); 9] = [
        (
            &["--executor-harness", "unknown", "--executor-session", "s"],
            "placeholder",
        ),
        (
            &["--executor-harness", "codex", "--executor-session", "None"],
            "placeholder",
        ),
        (
            &[
                "--executor-harness",
                "codex",
                "--executor-session",
                "<synthetic>",
            ],
            "placeholder",
        ),
        (
            &[
                "--executor-harness",
                "codex",
                "--executor-session",
                "s",
                "--handle",
                "?",
            ],
            "placeholder",
        ),
        (
            &["--executor-harness", "", "--executor-session", "s"],
            "empty",
        ),
        (
            &[
                "--executor-harness",
                "codex",
                "--executor-session",
                "two words",
            ],
            "whitespace",
        ),
        (
            &[
                "--executor-harness",
                "codex:model",
                "--executor-session",
                "s",
            ],
            "':'",
        ),
        (
            &[
                "--executor-harness",
                "019f0000-0000-7000-8000-00000000000a",
                "--executor-session",
                "s",
            ],
            "session id",
        ),
        (
            &[
                "--executor-harness",
                "codex",
                "--executor-session",
                "s",
                "--handle",
                " ",
            ],
            "empty",
        ),
    ];
    for (args, reason) in cases {
        let refused = bind(&repo, &dispatch_id, args).assert().failure();
        let stderr = String::from_utf8_lossy(&refused.get_output().stderr);
        assert!(stderr.contains("run bind refused"), "{args:?}: {stderr}");
        assert!(stderr.contains(reason), "{args:?}: {stderr}");
    }
    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    assert!(row_for(&rows, &dispatch_id)["binding"].is_null(), "{rows}");
}

#[test]
fn a_journal_subject_stays_resolvable_through_supersedes_after_transition() {
    let repo = Repo::new();
    let file = file_todo(&repo, "transitioning-subject");
    let dispatch_id = dispatch_against(&repo, "--journal", &file);
    repo.arc(&repo.root)
        .args(["journal", "transition", &file, "--to", "discussion"])
        .assert()
        .success();
    let successor = json_stdout(repo.arc(&repo.root).args([
        "journal",
        "latest",
        "transitioning-subject",
        "--kind",
        "discussion",
        "--json",
    ]))["file"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(successor, file);

    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    let row = row_for(&rows, &dispatch_id);
    assert_eq!(row["subject"]["file"], file.as_str(), "{rows}");
    assert_eq!(
        row["journal_lineage"],
        serde_json::json!([file, successor]),
        "{rows}"
    );
    let listed = stdout(repo.arc(&repo.root).args(["run", "list"]));
    assert!(
        listed.contains(&format!("journal {file} (now {successor})")),
        "{listed}"
    );
}

#[test]
fn fork_origin_is_listed_and_participates_in_binding_retry_rules() {
    let repo = Repo::new();
    let dispatch_id = dispatch_against(&repo, "--change", "fork-binding");
    let values = [
        "--executor-harness",
        "claude",
        "--executor-session",
        "fork-session",
        "--forked-from",
        "origin-session",
    ];
    let first = event_id(&stdout(&mut bind(&repo, &dispatch_id, &values)));
    let repeated = stdout(&mut bind(&repo, &dispatch_id, &values));
    assert_eq!(event_id(&repeated), first);
    assert!(repeated.contains("already bound"));
    for changed in [Some("different-origin"), None] {
        let mut values = vec![
            "--executor-harness",
            "claude",
            "--executor-session",
            "fork-session",
        ];
        if let Some(origin) = changed {
            values.extend(["--forked-from", origin]);
        }
        let refused = bind(&repo, &dispatch_id, &values).assert().failure();
        let stderr = String::from_utf8_lossy(&refused.get_output().stderr);
        assert!(stderr.contains("already bound"), "{stderr}");
        assert!(stderr.contains("forked-from origin-session"), "{stderr}");
    }
    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    assert_eq!(
        row_for(&rows, &dispatch_id)["binding"]["forked_from"],
        "origin-session"
    );
    let listed = stdout(repo.arc(&repo.root).args(["run", "list"]));
    assert!(listed.contains("forked-from=origin-session"), "{listed}");
    let events = stdout(repo.arc(&repo.root).args(["events", "--repository"]));
    assert_eq!(events.matches("\"run-bound\"").count(), 1);
}

#[test]
fn fork_origin_is_validated_like_an_executor_session() {
    let repo = Repo::new();
    let dispatch_id = dispatch_against(&repo, "--change", "invalid-origin");
    for (origin, reason) in [
        ("", "empty"),
        ("None", "placeholder"),
        ("two words", "whitespace"),
    ] {
        let refused = bind(
            &repo,
            &dispatch_id,
            &[
                "--executor-harness",
                "claude",
                "--executor-session",
                "fork-session",
                "--forked-from",
                origin,
            ],
        )
        .assert()
        .failure();
        let stderr = String::from_utf8_lossy(&refused.get_output().stderr);
        assert!(stderr.contains("--forked-from"), "{stderr}");
        assert!(stderr.contains(reason), "{stderr}");
    }
    let rows = json_stdout(repo.arc(&repo.root).args(["run", "list", "--json"]));
    assert!(row_for(&rows, &dispatch_id)["binding"].is_null());
}
