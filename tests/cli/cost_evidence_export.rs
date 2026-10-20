//! `arc cost evidence` writes the evidence `arc cost show` divides its
//! sessions by as a `tapes-attribution-evidence/1` file. Each test builds a
//! synthetic ledger and Pi recordings, named as Pi names them, and moves the
//! ledger events to fixed instants, so every record's expected content is
//! known.

use super::common::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const EXECUTOR: &str = "019a0000-0000-7000-8000-00000000000e";
const OTHER_EXECUTOR: &str = "019a0000-0000-7000-8000-00000000000f";
const CALLER: &str = "019a0000-0000-7000-8000-0000000000c1";
const REVIEWER: &str = "019a0000-0000-7000-8000-0000000000e1";

fn at(clock: &str) -> String {
    format!("2026-01-01T{clock}Z")
}

fn event_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .expect("command output should contain an event id")
        .to_string()
}

/// A Pi recording, named as Pi names one, whose assistant requests each
/// record `input` uncached tokens at their own instant.
fn pi_session(repo: &Repo, id: &str, requests: &[(&str, u64)]) {
    let path = repo
        .home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T07-00-00-000Z_{id}.jsonl"));
    let mut lines = vec![
        json!({"type": "session", "version": 3, "id": id, "timestamp": at("07:00:00"), "cwd": "/fixture/project"}),
        json!({"type": "message", "id": "user-0", "parentId": null, "timestamp": at("07:00:01"),
            "message": {"role": "user", "timestamp": 1767250801000u64, "content": [{"type": "text", "text": "Work."}]}}),
    ];
    let mut parent = "user-0".to_string();
    for (n, (clock, input)) in requests.iter().enumerate() {
        let entry = format!("assistant-{n}");
        lines.push(json!({"type": "message", "id": entry, "parentId": parent, "timestamp": at(clock),
            "message": {"role": "assistant", "timestamp": 1767250802000u64, "provider": "fixture", "model": "fixture-model",
                "usage": {"input": input, "output": 200, "cacheRead": 400, "cacheWrite": 0, "totalTokens": input + 600,
                    "cost": {"total": *input as f64 / 1000.0}},
                "content": [{"type": "text", "text": "Done."}]}}));
        parent = entry;
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

/// Move a ledger event, wherever it is stored, to `instant`.
fn retime(repo: &Repo, id: &str, instant: &str) {
    let name = format!("{id}.json");
    let mut pending = vec![repo.root.join(".git/arc")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|file| file == name.as_str()) {
                let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                event["created_at"] = json!(instant);
                fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
                return;
            }
        }
    }
    panic!("no ledger event {id}");
}

/// `arc` naming no acting session.
fn anonymous(repo: &Repo, cwd: &Path) -> AssertCommand {
    let mut command = repo.arc(cwd);
    command.env_remove("ARC_HARNESS").env_remove("ARC_SESSION");
    command
}

fn as_pi(repo: &Repo, session: &str) -> AssertCommand {
    let mut command = repo.arc(&repo.root);
    command.env("ARC_HARNESS", "pi").env("ARC_SESSION", session);
    command
}

/// Move every event a change holds to `clock`.
fn settle(repo: &Repo, change: &str, clock: &str) {
    for entry in fs::read_dir(event_dir(repo, change)).unwrap() {
        let path = entry.unwrap().path();
        let id = path.file_stem().unwrap().to_str().unwrap().to_string();
        retime(repo, &id, &at(clock));
    }
}

/// A change opened and snapshotted naming no session, its events moved to
/// 06:00 before any recording begins.
fn change(repo: &Repo, slug: &str) -> String {
    let change = opened_change_id(&stdout(anonymous(repo, &repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        "x\n",
        &format!("feat: {slug}"),
    );
    stdout(anonymous(repo, &worktree).args(["snapshot", slug]));
    settle(repo, &change, "06:00:00");
    change
}

/// A change opened, snapshotted, approved and integrated naming no session.
fn integrated(repo: &Repo, slug: &str) -> String {
    let change = change(repo, slug);
    anonymous(repo, &repo.root)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    anonymous(repo, &repo.root)
        .args(["integrate", slug])
        .assert()
        .success();
    settle(repo, &change, "06:00:00");
    change
}

/// Dispatch a run for `change` at `start`, recorded by `caller`, bind it to
/// `executor`, and end it at `end` unless it stays open.
fn run(
    repo: &Repo,
    change: &str,
    executor: &str,
    start: &str,
    end: Option<&str>,
    caller: Option<&str>,
    handle: Option<&str>,
) -> String {
    let mut dispatch = match caller {
        Some(caller) => {
            let mut command = repo.arc(&repo.root);
            command
                .env("ARC_HARNESS", "claude")
                .env("ARC_SESSION", caller);
            command
        }
        None => anonymous(repo, &repo.root),
    };
    let dispatched = event_id(&stdout(dispatch.args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        "--change",
        change,
    ])));
    retime(repo, &dispatched, &at(start));
    let mut bind = anonymous(repo, &repo.root);
    bind.args([
        "run",
        "bind",
        &dispatched,
        "--executor-harness",
        "pi",
        "--executor-session",
        executor,
    ]);
    if let Some(handle) = handle {
        bind.args(["--handle", handle]);
    }
    stdout(&mut bind);
    if let Some(end) = end {
        let ended = event_id(&stdout(anonymous(repo, &repo.root).args([
            "run",
            "end",
            &dispatched,
            "--outcome",
            "completed",
        ])));
        retime(repo, &ended, &at(end));
    }
    dispatched
}

/// The export of a selection: its standard output and its summary.
fn export(repo: &Repo, args: &[&str]) -> (String, String) {
    let output = repo
        .arc(&repo.root)
        .args(["cost", "evidence"])
        .args(args)
        .assert()
        .success()
        .get_output()
        .clone();
    (
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn lines(text: &str) -> Vec<Value> {
    text.lines()
        .map(|line| serde_json::from_str(line).expect("every line is a JSON object"))
        .collect()
}

/// The records of one kind that concern `session`, in file order.
fn records<'a>(file: &'a [Value], kind: &str, session: &str) -> Vec<&'a Value> {
    file.iter()
        .skip(1)
        .filter(|record| {
            record["kind"] == kind
                && (record["session"]["id"] == session || record["child"]["id"] == session)
        })
        .collect()
}

fn kinds(file: &[Value]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for record in file.iter().skip(1) {
        *counts
            .entry(record["kind"].as_str().unwrap().to_string())
            .or_default() += 1;
    }
    counts
}

fn summary_line<'a>(summary: &'a str, prefix: &str) -> &'a str {
    summary
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("no `{prefix}` in {summary}"))
}

// Claim: an executor the ledger gives one run exports one binding naming
// its change, with no span, which tapes reads as the whole session. Rejects
// writing the run as a span, which would confine the session to the run's
// window.
#[test]
fn an_executor_with_one_run_exports_a_binding_without_a_span() {
    let repo = Repo::new();
    pi_session(&repo, EXECUTOR, &[("10:05:00", 10), ("10:15:00", 100)]);
    let change = change(&repo, "solo");
    run(
        &repo,
        &change,
        EXECUTOR,
        "10:00:00",
        Some("10:20:00"),
        None,
        None,
    );

    let (out, summary) = export(&repo, &[&change]);
    let file = lines(&out);
    let bindings = records(&file, "binding", EXECUTOR);
    assert_eq!(bindings.len(), 1, "{out}");
    let binding = bindings[0];
    assert!(binding.get("span").is_none(), "{binding}");
    assert_eq!(binding["session"], json!({"harness": "pi", "id": EXECUTOR}));
    assert_eq!(binding["work"]["label"], change.as_str());
    assert_eq!(binding["tier"], "bound");
    assert_eq!(binding["role"], "implementer");
    assert_eq!(kinds(&file), BTreeMap::from([("binding".to_string(), 1)]));
    assert_eq!(summary_line(&summary, "sessions: "), "1", "{summary}");
}

// Claim: an executor with two runs exports one binding per run, each with
// the span from its dispatch to its recorded end, whichever work the view
// selects: the session is divided by every run it served. Rejects exporting
// the session whole, which would hand both changes the entire session.
#[test]
fn an_executor_with_two_runs_exports_a_spanned_binding_per_run() {
    let repo = Repo::new();
    pi_session(
        &repo,
        EXECUTOR,
        &[("10:05:00", 1), ("10:35:00", 10), ("11:05:00", 100)],
    );
    let (first, second) = (change(&repo, "first"), change(&repo, "second"));
    run(
        &repo,
        &first,
        EXECUTOR,
        "10:00:00",
        Some("10:20:00"),
        None,
        None,
    );
    run(
        &repo,
        &second,
        EXECUTOR,
        "10:30:00",
        Some("11:00:00"),
        None,
        None,
    );

    // Selecting one change still writes both runs.
    for selection in [&[first.as_str()][..], &["--all"][..]] {
        let (out, _) = export(&repo, selection);
        let file = lines(&out);
        let bindings = records(&file, "binding", EXECUTOR);
        assert_eq!(bindings.len(), 2, "{out}");
        let spans: Vec<(&str, &str, &str)> = bindings
            .iter()
            .map(|binding| {
                (
                    binding["work"]["label"].as_str().unwrap(),
                    binding["span"]["from"].as_str().unwrap(),
                    binding["span"]["to"].as_str().unwrap(),
                )
            })
            .collect();
        let mut expected = vec![
            (
                first.as_str(),
                "2026-01-01T10:00:00Z",
                "2026-01-01T10:20:00Z",
            ),
            (
                second.as_str(),
                "2026-01-01T10:30:00Z",
                "2026-01-01T11:00:00Z",
            ),
        ];
        expected.sort_by_key(|(_, from, _)| *from);
        assert_eq!(spans, expected, "{out}");
        assert!(bindings
            .iter()
            .all(|binding| binding["role"] == "implementer" && binding["tier"] == "bound"));
    }
}

// Claim: a run that has no recorded end is exported with its span ending at
// the file's `written_at`, and the summary counts it. Rejects an open
// span, which the format has no way to state, and a span ending at its
// dispatch.
#[test]
fn an_open_run_ends_at_the_files_written_at() {
    let repo = Repo::new();
    pi_session(&repo, EXECUTOR, &[("10:05:00", 1), ("10:35:00", 10)]);
    let (first, second) = (change(&repo, "closed"), change(&repo, "open"));
    run(
        &repo,
        &first,
        EXECUTOR,
        "10:00:00",
        Some("10:20:00"),
        None,
        None,
    );
    run(&repo, &second, EXECUTOR, "10:30:00", None, None, None);

    let (out, summary) = export(&repo, &["--all"]);
    let file = lines(&out);
    let written_at = file[0]["written_at"].as_str().unwrap();
    let open = records(&file, "binding", EXECUTOR)
        .into_iter()
        .find(|binding| binding["work"]["label"] == second.as_str())
        .unwrap_or_else(|| panic!("no binding for the open run: {out}"));
    assert_eq!(open["span"]["from"], "2026-01-01T10:30:00Z", "{out}");
    assert_eq!(open["span"]["to"], written_at, "{out}");
    assert_eq!(
        summary_line(&summary, "runs still open: "),
        "1",
        "{summary}"
    );
}

/// A reviewer session that starts reviewing `change` at `start` and records
/// its verdict at `end`.
fn review(repo: &Repo, session: &str, change: &str, start: &str, end: &str) {
    let opened = event_id(&stdout(
        as_pi(repo, session).args(["review", change, "--start"]),
    ));
    retime(repo, &opened, &at(start));
    let verdict = event_id(&stdout(as_pi(repo, session).args([
        "review",
        change,
        "--verdict",
        "comment-only",
    ])));
    retime(repo, &verdict, &at(end));
}

// Claim: a participant acting on two changes exports one event per event it
// recorded, each with the role its kind gives it, and no binding. Rejects
// exporting it whole, which would hand both changes its entire session.
#[test]
fn a_participant_on_two_changes_exports_its_events() {
    let repo = Repo::new();
    pi_session(
        &repo,
        REVIEWER,
        &[("10:05:00", 1), ("10:35:00", 10), ("10:45:00", 100)],
    );
    let (first, second) = (change(&repo, "alpha"), change(&repo, "beta"));
    review(&repo, REVIEWER, &first, "10:00:00", "10:20:00");
    review(&repo, REVIEWER, &second, "10:30:00", "10:50:00");

    let (out, _) = export(&repo, &["--all"]);
    let file = lines(&out);
    assert!(records(&file, "binding", REVIEWER).is_empty(), "{out}");
    let events = records(&file, "event", REVIEWER);
    let seen: Vec<(&str, &str, &str)> = events
        .iter()
        .map(|event| {
            (
                event["work"]["label"].as_str().unwrap(),
                event["at"].as_str().unwrap(),
                event["role"].as_str().unwrap(),
            )
        })
        .collect();
    let mut expected = vec![
        (first.as_str(), "2026-01-01T10:00:00Z", "reviewer"),
        (first.as_str(), "2026-01-01T10:20:00Z", "reviewer"),
        (second.as_str(), "2026-01-01T10:30:00Z", "reviewer"),
        (second.as_str(), "2026-01-01T10:50:00Z", "reviewer"),
    ];
    expected.sort_by_key(|(_, at, _)| *at);
    assert_eq!(seen, expected, "{out}");
    assert!(events.iter().all(|event| event["tier"] == "bound"));
}

// Claim: a participant with one piece of work exports one binding with no
// span, carrying its role when its events give it one. Rejects exporting
// its events, which would let a stretch of its session go unplaced.
#[test]
fn a_participant_on_one_change_exports_a_whole_binding() {
    let repo = Repo::new();
    pi_session(&repo, REVIEWER, &[("10:05:00", 1), ("10:15:00", 10)]);
    let only = change(&repo, "only");
    review(&repo, REVIEWER, &only, "10:00:00", "10:20:00");

    let (out, _) = export(&repo, &["--all"]);
    let file = lines(&out);
    assert!(records(&file, "event", REVIEWER).is_empty(), "{out}");
    let bindings = records(&file, "binding", REVIEWER);
    assert_eq!(bindings.len(), 1, "{out}");
    assert!(bindings[0].get("span").is_none(), "{out}");
    assert_eq!(bindings[0]["work"]["label"], only.as_str());
    assert_eq!(bindings[0]["role"], "reviewer");
}

// Claim: an integrated change's work carries the commits `cost note` finds
// for it, as full object names, and a change with no commits carries its
// label alone. Rejects a commit list on a change that holds none, and
// abbreviated names.
#[test]
fn work_carries_the_commits_of_its_change_and_a_change_without_commits_its_label() {
    let repo = Repo::new();
    pi_session(&repo, EXECUTOR, &[("10:05:00", 1), ("10:35:00", 10)]);
    pi_session(&repo, OTHER_EXECUTOR, &[("10:05:00", 1)]);
    let landed = integrated(&repo, "landed");
    let bare = begin_without_checkout(&repo, "bare", &[]);
    settle(&repo, &bare, "06:00:00");
    run(
        &repo,
        &landed,
        EXECUTOR,
        "10:00:00",
        Some("10:20:00"),
        None,
        None,
    );
    run(
        &repo,
        &bare,
        OTHER_EXECUTOR,
        "10:00:00",
        Some("10:20:00"),
        None,
        None,
    );

    let (out, _) = export(&repo, &["--all"]);
    let file = lines(&out);
    let work_of =
        |session: &str| -> Value { records(&file, "binding", session)[0]["work"].clone() };
    let landed_work = work_of(EXECUTOR);
    let commits: Vec<&str> = landed_work["commits"]
        .as_array()
        .unwrap_or_else(|| panic!("no commits: {landed_work}"))
        .iter()
        .map(|commit| commit.as_str().unwrap())
        .collect();
    assert!(!commits.is_empty());
    assert!(
        commits
            .iter()
            .all(|commit| commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit())),
        "{commits:?}"
    );
    let integration = git_out(&repo.root, &["rev-parse", "HEAD"]);
    assert!(commits.contains(&integration.trim()), "{commits:?}");
    for commit in &commits {
        git(&repo.root, &["cat-file", "-e", commit]);
    }
    assert_eq!(work_of(OTHER_EXECUTOR), json!({"label": bare}), "{out}");
}

// Claim: the repository is the `origin` remote URL without any credential
// it carries, and is left out when there is no origin or it names a place on
// this machine. Rejects writing a local path or a token.
#[test]
fn the_repository_is_the_origin_url_without_credentials_and_never_a_local_path() {
    let repo = Repo::new();
    pi_session(&repo, EXECUTOR, &[("10:05:00", 1)]);
    let change = change(&repo, "origin");
    run(
        &repo,
        &change,
        EXECUTOR,
        "10:00:00",
        Some("10:20:00"),
        None,
        None,
    );
    let repository = |out: &str| lines(out)[1]["work"].get("repository").cloned();

    let (out, _) = export(&repo, &["--all"]);
    assert_eq!(repository(&out), None, "{out}");

    git(
        &repo.root,
        &[
            "remote",
            "add",
            "origin",
            "https://user:secret-token@example.com/owner/project.git",
        ],
    );
    let (out, _) = export(&repo, &["--all"]);
    assert_eq!(
        repository(&out),
        Some(json!("https://example.com/owner/project.git")),
        "{out}"
    );
    assert!(
        !out.contains("secret-token") && !out.contains("user:"),
        "{out}"
    );

    git(
        &repo.root,
        &[
            "remote",
            "set-url",
            "origin",
            "git@example.com:owner/project.git",
        ],
    );
    let (out, _) = export(&repo, &["--all"]);
    assert_eq!(
        repository(&out),
        Some(json!("git@example.com:owner/project.git")),
        "{out}"
    );

    for local in [repo.home.to_str().unwrap(), "file:///srv/project.git"] {
        git(&repo.root, &["remote", "set-url", "origin", local]);
        let (out, _) = export(&repo, &["--all"]);
        assert_eq!(repository(&out), None, "{local}: {out}");
        assert!(
            !out.contains(&repo.home.to_string_lossy().to_string()),
            "{out}"
        );
    }
}

// Claim: a dispatch that records the session which made it and the executor
// it bound writes one lineage record per distinct pair, at the earliest
// dispatch, with the run's handle and the work as subject; a resumed run
// repeats no pair. Rejects one record per dispatch, and a record for a
// dispatch that names no caller.
#[test]
fn lineage_is_one_record_per_distinct_pair() {
    let repo = Repo::new();
    pi_session(
        &repo,
        EXECUTOR,
        &[("10:05:00", 1), ("10:35:00", 10), ("11:05:00", 100)],
    );
    pi_session(&repo, OTHER_EXECUTOR, &[("12:05:00", 1)]);
    let (first, second) = (change(&repo, "left"), change(&repo, "right"));
    run(
        &repo,
        &first,
        EXECUTOR,
        "10:00:00",
        Some("10:20:00"),
        Some(CALLER),
        Some("handle-one"),
    );
    run(
        &repo,
        &second,
        EXECUTOR,
        "10:30:00",
        Some("11:00:00"),
        Some(CALLER),
        Some("handle-two"),
    );
    run(
        &repo,
        &first,
        OTHER_EXECUTOR,
        "12:00:00",
        Some("12:20:00"),
        Some(CALLER),
        None,
    );

    let (out, summary) = export(&repo, &["--all"]);
    let file = lines(&out);
    let launches: Vec<&Value> = file
        .iter()
        .skip(1)
        .filter(|record| record["kind"] == "lineage")
        .collect();
    assert_eq!(launches.len(), 2, "{out}");
    let by_child = |child: &str| {
        launches
            .iter()
            .find(|record| record["child"]["id"] == child)
            .copied()
            .unwrap_or_else(|| panic!("no lineage for {child}: {out}"))
    };
    let launch = by_child(EXECUTOR);
    assert_eq!(launch["parent"], json!({"harness": "claude", "id": CALLER}));
    assert_eq!(launch["child"], json!({"harness": "pi", "id": EXECUTOR}));
    assert_eq!(launch["tier"], "bound");
    assert_eq!(launch["at"], "2026-01-01T10:00:00Z");
    assert_eq!(launch["label"], "handle-one");
    assert_eq!(launch["subject"], first.as_str());
    let other = by_child(OTHER_EXECUTOR);
    assert!(other.get("label").is_none(), "{other}");
    assert_eq!(other["at"], "2026-01-01T12:00:00Z");
    // The caller has no recording: it is named without a figure and writes
    // no binding.
    assert!(records(&file, "binding", CALLER).is_empty(), "{out}");
    assert_eq!(summary_line(&summary, "sessions without a figure: "), "1");
    assert_eq!(
        summary_line(&summary, "records: "),
        "3 binding, 0 event, 2 lineage",
        "{summary}"
    );
}

// Claim: a session the view gives no figure writes no record and is counted
// on standard error. Rejects a binding for a session nothing measured.
#[test]
fn a_session_without_a_figure_writes_nothing_and_is_counted() {
    let repo = Repo::new();
    pi_session(&repo, EXECUTOR, &[("10:05:00", 1)]);
    let change = change(&repo, "missing");
    run(
        &repo,
        &change,
        EXECUTOR,
        "10:00:00",
        Some("10:20:00"),
        None,
        None,
    );
    run(
        &repo,
        &change,
        OTHER_EXECUTOR,
        "10:00:00",
        Some("10:20:00"),
        None,
        None,
    );

    let (out, summary) = export(&repo, &["--all"]);
    let file = lines(&out);
    assert!(
        records(&file, "binding", OTHER_EXECUTOR).is_empty(),
        "{out}"
    );
    assert_eq!(records(&file, "binding", EXECUTOR).len(), 1, "{out}");
    assert_eq!(summary_line(&summary, "sessions without a figure: "), "1");
    assert_eq!(summary_line(&summary, "sessions: "), "1");
}

fn rfc3339(text: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(text).is_ok()
}

fn session_ref(value: &Value) -> bool {
    ["claude", "codex", "pi", "opencode"].contains(&value["harness"].as_str().unwrap_or_default())
        && !value["id"].as_str().unwrap_or_default().is_empty()
}

// Claim: the file is a conformant `tapes-attribution-evidence/1` file: the
// header comes first, every record carries the fields its kind requires, and
// two exports of an unchanged ledger differ only in `written_at`. Rejects a
// missing header, a record lacking a required field, and an export that
// orders or times its records differently on a second run.
#[test]
fn the_export_conforms_to_the_format_and_is_deterministic() {
    let repo = Repo::new();
    pi_session(
        &repo,
        EXECUTOR,
        &[("10:05:00", 1), ("10:35:00", 10), ("11:05:00", 100)],
    );
    pi_session(&repo, REVIEWER, &[("10:05:00", 1), ("10:35:00", 10)]);
    let (first, second, landed) = (
        change(&repo, "one"),
        change(&repo, "two"),
        integrated(&repo, "landed"),
    );
    run(
        &repo,
        &first,
        EXECUTOR,
        "10:00:00",
        Some("10:20:00"),
        Some(CALLER),
        Some("handle"),
    );
    run(
        &repo,
        &landed,
        EXECUTOR,
        "10:30:00",
        Some("11:00:00"),
        Some(CALLER),
        None,
    );
    review(&repo, REVIEWER, &first, "10:00:00", "10:20:00");
    review(&repo, REVIEWER, &second, "10:30:00", "10:50:00");

    let (out, _) = export(&repo, &["--all"]);
    let file = lines(&out);
    let header = &file[0];
    assert_eq!(header["schema"], "tapes-attribution-evidence/1");
    assert_eq!(
        header["producer"],
        format!("arc/{}", env!("CARGO_PKG_VERSION"))
    );
    assert!(rfc3339(header["written_at"].as_str().unwrap()), "{header}");
    assert!(file
        .iter()
        .skip(1)
        .all(|record| record.get("schema").is_none()));
    assert_eq!(
        kinds(&file),
        BTreeMap::from([
            ("binding".to_string(), 2),
            ("event".to_string(), 4),
            ("lineage".to_string(), 1),
        ]),
        "{out}"
    );

    let mut commits: BTreeMap<String, Value> = BTreeMap::new();
    for record in file.iter().skip(1) {
        assert_eq!(record["tier"], "bound", "{record}");
        match record["kind"].as_str().unwrap() {
            "lineage" => {
                assert!(session_ref(&record["parent"]) && session_ref(&record["child"]));
                assert_ne!(record["parent"], record["child"]);
                assert!(rfc3339(record["at"].as_str().unwrap()));
            }
            kind => {
                assert!(session_ref(&record["session"]), "{record}");
                let work = &record["work"];
                let label = work["label"].as_str().unwrap();
                assert!(
                    !label.is_empty() && work.get("commits").is_some(),
                    "{record}"
                );
                // Records naming one label name the same commits.
                let held = commits
                    .entry(label.to_string())
                    .or_insert_with(|| work["commits"].clone());
                assert_eq!(held, &work["commits"], "{record}");
                if kind == "event" {
                    assert!(rfc3339(record["at"].as_str().unwrap()), "{record}");
                } else if let Some(span) = record.get("span") {
                    let (from, to) = (span["from"].as_str().unwrap(), span["to"].as_str().unwrap());
                    assert!(rfc3339(from) && rfc3339(to) && from <= to, "{record}");
                }
            }
        }
    }

    let (again, _) = export(&repo, &["--all"]);
    let strip = |text: &str| {
        let mut file = lines(text);
        file[0].as_object_mut().unwrap().remove("written_at");
        file
    };
    assert_eq!(strip(&out), strip(&again));
    assert_eq!(
        out.lines().skip(1).collect::<Vec<_>>(),
        again.lines().skip(1).collect::<Vec<_>>()
    );
}

/// Every file under `root`, by path, with its bytes' digest.
fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(
                    path.clone(),
                    Sha256::digest(fs::read(&path).unwrap()).to_vec(),
                );
            }
        }
    }
    files
}

// Claim: the export writes no ledger event and changes nothing under the
// ledger or the sandbox. Rejects an export that records its read.
#[test]
fn the_export_leaves_the_ledger_byte_identical() {
    let repo = Repo::new();
    pi_session(&repo, EXECUTOR, &[("10:05:00", 1), ("10:35:00", 10)]);
    pi_session(&repo, REVIEWER, &[("10:05:00", 1), ("10:35:00", 10)]);
    let (first, second) = (change(&repo, "ledger-a"), change(&repo, "ledger-b"));
    run(
        &repo,
        &first,
        EXECUTOR,
        "10:00:00",
        Some("10:20:00"),
        Some(CALLER),
        None,
    );
    run(
        &repo,
        &second,
        EXECUTOR,
        "10:30:00",
        None,
        Some(CALLER),
        None,
    );
    review(&repo, REVIEWER, &first, "10:00:00", "10:20:00");
    review(&repo, REVIEWER, &second, "10:30:00", "10:50:00");

    let ledger = repo.root.join(".git/arc");
    let before = (tree(&ledger), tree(&repo.home.join(".local")));
    let (out, _) = export(&repo, &["--all"]);
    assert!(out.lines().count() > 1);
    let after = (tree(&ledger), tree(&repo.home.join(".local")));
    assert!(
        before == after,
        "the export changed the ledger or the sandbox"
    );
}

// Claim: a journal artifact selects the dispatches made for it, and its work
// is the artifact's filename with no commits. Rejects naming the work by a
// local path or leaving the run out.
#[test]
fn a_journal_artifact_is_named_by_its_file() {
    let repo = Repo::new();
    pi_session(&repo, EXECUTOR, &[("10:05:00", 1)]);
    let (_, file) = journal_artifact(&repo, "plan-topic", "plan", "A plan.\n");
    let mut dispatch = anonymous(&repo, &repo.root);
    let dispatched = event_id(&stdout(dispatch.args([
        "run",
        "dispatch",
        "--route",
        "fixture-route",
        "--worktree",
        "/work/run",
        "--journal",
        &file,
    ])));
    retime(&repo, &dispatched, &at("10:00:00"));
    stdout(anonymous(&repo, &repo.root).args([
        "run",
        "bind",
        &dispatched,
        "--executor-harness",
        "pi",
        "--executor-session",
        EXECUTOR,
    ]));

    let (out, _) = export(&repo, &[&file]);
    let evidence = lines(&out);
    let bindings = records(&evidence, "binding", EXECUTOR);
    assert_eq!(bindings.len(), 1, "{out}");
    assert_eq!(bindings[0]["work"], json!({"label": file}), "{out}");
}

// Claim: the command needs a selection, as `cost show` does. Rejects an
// export of the whole ledger from a bare invocation.
#[test]
fn a_bare_export_is_refused() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["cost", "evidence"])
        .assert()
        .failure();
}

/// The branch a change was begun on, found by its slug.
fn branch_of(repo: &Repo, slug: &str) -> String {
    git_out(
        &repo.root,
        &["branch", "--list", "--format=%(refname:short)"],
    )
    .lines()
    .find(|branch| branch.contains(slug))
    .unwrap_or_else(|| panic!("no branch for {slug}"))
    .to_string()
}

/// A change begun from the target and snapshotted after its worktree took
/// `below`'s branch, with `own` commits of its own on top: its patchset
/// range holds everything `below` holds.
fn stacked(repo: &Repo, slug: &str, below: &str, own: bool) -> String {
    let change = opened_change_id(&stdout(anonymous(repo, &repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    git(&worktree, &["merge", "--ff-only", &branch_of(repo, below)]);
    if own {
        repo.commit(
            &worktree,
            &format!("{slug}.txt"),
            "x\n",
            &format!("feat: {slug}"),
        );
    }
    stdout(anonymous(repo, &worktree).args(["snapshot", slug]));
    settle(repo, &change, "06:00:00");
    change
}

/// Approve and integrate the change `slug` names.
fn land(repo: &Repo, slug: &str, change: &str) {
    anonymous(repo, &repo.root)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    anonymous(repo, &repo.root)
        .args(["integrate", slug])
        .assert()
        .success();
    settle(repo, change, "06:00:00");
}

fn head_of(repo: &Repo, slug: &str) -> String {
    git_out(&repo.root, &["rev-parse", &branch_of(repo, slug)])
        .trim()
        .to_string()
}

/// The commits a change's work carries in the export, by session.
fn commits_of(file: &[Value], session: &str) -> Vec<String> {
    let mut commits: Vec<String> = records(file, "binding", session)[0]["work"]["commits"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|commit| commit.as_str().unwrap().to_string())
        .collect();
    commits.sort();
    commits
}

/// A lower and an upper change, each with an executor run so both are
/// exported.
fn run_both(repo: &Repo, lower: &str, upper: &str) {
    pi_session(repo, EXECUTOR, &[("10:05:00", 1)]);
    pi_session(repo, OTHER_EXECUTOR, &[("10:05:00", 1)]);
    for (change, executor) in [(lower, EXECUTOR), (upper, OTHER_EXECUTOR)] {
        run(
            repo,
            change,
            executor,
            "10:00:00",
            Some("10:20:00"),
            None,
            None,
        );
    }
}

// Claim: a change built on an integrated change exports its own commits and
// its integration commit, and the lower change its own and its integration
// commit. Rejects the upper change carrying the lower change's commits,
// which tapes would then share the lower change's consumption over.
#[test]
fn a_stacked_integrated_change_exports_only_its_own_commits() {
    let repo = Repo::new();
    let lower = change(&repo, "base-layer");
    let upper = stacked(&repo, "top-layer", "base-layer", true);
    let (lower_head, upper_head) = (head_of(&repo, "base-layer"), head_of(&repo, "top-layer"));
    land(&repo, "base-layer", &lower);
    land(&repo, "top-layer", &upper);
    run_both(&repo, &lower, &upper);

    let (out, summary) = export(&repo, &["--all"]);
    let file = lines(&out);
    let integration = git_out(&repo.root, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let mut expected_upper = vec![upper_head.clone(), integration.clone()];
    expected_upper.sort();
    expected_upper.dedup();
    assert_eq!(commits_of(&file, OTHER_EXECUTOR), expected_upper, "{out}");
    assert!(
        !commits_of(&file, OTHER_EXECUTOR).contains(&lower_head),
        "{out}"
    );
    let lower_commits = commits_of(&file, EXECUTOR);
    assert!(lower_commits.contains(&lower_head), "{out}");
    assert!(!lower_commits.contains(&upper_head), "{out}");
    assert_eq!(
        summary_line(&summary, "commits under several labels: "),
        "0",
        "{summary}"
    );
}

// Claim: a change built on an open change excludes that change's commits.
// Rejects the upper change carrying them.
#[test]
fn a_change_stacked_on_an_open_change_excludes_its_commits() {
    let repo = Repo::new();
    let lower = change(&repo, "open-base");
    let upper = stacked(&repo, "open-top", "open-base", true);
    let (lower_head, upper_head) = (head_of(&repo, "open-base"), head_of(&repo, "open-top"));
    run_both(&repo, &lower, &upper);

    let (out, _) = export(&repo, &["--all"]);
    let file = lines(&out);
    assert_eq!(commits_of(&file, OTHER_EXECUTOR), vec![upper_head], "{out}");
    assert_eq!(commits_of(&file, EXECUTOR), vec![lower_head], "{out}");
}

// Claim: a change built on an abandoned change keeps that change's commits,
// since they ship only with it. Rejects excluding commits of a change that
// stands nowhere.
#[test]
fn a_change_stacked_on_an_abandoned_change_keeps_its_commits() {
    let repo = Repo::new();
    let lower = change(&repo, "gone-base");
    let upper = stacked(&repo, "gone-top", "gone-base", true);
    let (lower_head, upper_head) = (head_of(&repo, "gone-base"), head_of(&repo, "gone-top"));
    anonymous(&repo, &repo.root)
        .args(["close", "gone-base", "--abandoned"])
        .assert()
        .success();
    settle(&repo, &lower, "06:00:00");
    run_both(&repo, &lower, &upper);

    let (out, _) = export(&repo, &["--all"]);
    let file = lines(&out);
    let mut expected = vec![lower_head, upper_head];
    expected.sort();
    assert_eq!(commits_of(&file, OTHER_EXECUTOR), expected, "{out}");
}

// Claim: two changes with the same patchset head keep their commits, and the
// summary counts the commits under both labels. Rejects either change
// excluding the other's commits.
#[test]
fn changes_with_the_same_head_keep_their_commits_and_are_counted() {
    let repo = Repo::new();
    let first = change(&repo, "twin-one");
    let second = stacked(&repo, "twin-two", "twin-one", false);
    let head = head_of(&repo, "twin-one");
    run_both(&repo, &first, &second);

    let (out, summary) = export(&repo, &["--all"]);
    let file = lines(&out);
    assert_eq!(commits_of(&file, EXECUTOR), vec![head.clone()], "{out}");
    assert_eq!(commits_of(&file, OTHER_EXECUTOR), vec![head], "{out}");
    assert_eq!(
        summary_line(&summary, "commits under several labels: "),
        "1",
        "{summary}"
    );
}
