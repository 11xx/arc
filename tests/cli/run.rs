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
