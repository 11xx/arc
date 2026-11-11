use super::common::*;
use serde_json::{json, Value};

const EXECUTOR: &str = "codex:executor-one";
const COORDINATE: &str = "codex:writer-model#high";

struct Fixture {
    repo: Repo,
    change: String,
    worktree: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let repo = Repo::new();
        repo.declare_gates_locally("[policy]\nforbid_self_approval = true\n");
        stdout(repo.arc(&repo.root).args(["begin", "derived"]));
        let state = json_stdout(repo.arc(&repo.root).args(["show", "derived", "--json"]));
        let change = state["change_id"].as_str().unwrap().to_string();
        let worktree = repo.home.join(".worktrees/repo-derived");
        Self {
            repo,
            change,
            worktree,
        }
    }

    fn lead(&self) -> AssertCommand {
        let mut command = self.repo.arc(&self.worktree);
        command
            .env("ARC_ACTOR", "Lead")
            .env("ARC_HARNESS", "claude")
            .env("ARC_SESSION", "lead-session")
            .env("ARC_MODEL", "reader-model#high");
        command
    }

    fn event(&self, output: String, time: i64) -> String {
        let id = output
            .lines()
            .find_map(|line| line.strip_prefix("event: "))
            .unwrap();
        let file = self
            .repo
            .root
            .join(".git/arc/repository/events")
            .join(format!("{id}.json"));
        let mut event: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        event["created_at"] =
            json!(chrono::DateTime::from_timestamp(1_577_836_800 + time, 0).unwrap());
        fs::write(file, serde_json::to_vec(&event).unwrap()).unwrap();
        id.to_string()
    }

    fn dispatch(&self, coordinate: Option<&str>, start: i64) -> String {
        let mut command = self.lead();
        command.args([
            "run",
            "dispatch",
            "--route",
            COORDINATE,
            "--worktree",
            self.worktree.to_str().unwrap(),
            "--change",
            &self.change,
        ]);
        if let Some(coordinate) = coordinate {
            command.args(["--on-behalf-of", coordinate]);
        }
        self.event(stdout(&mut command), start)
    }

    fn bind(&self, dispatch: &str, harness: &str, session: &str, time: i64) -> String {
        self.event(
            stdout(self.lead().args([
                "run",
                "bind",
                dispatch,
                "--executor-harness",
                harness,
                "--executor-session",
                session,
            ])),
            time,
        )
    }

    fn end(&self, dispatch: &str, end: i64) {
        self.event(
            stdout(
                self.lead()
                    .args(["run", "end", dispatch, "--outcome", "completed"]),
            ),
            end,
        );
    }

    fn run(&self, coordinate: &str, harness: &str, session: &str, start: i64, end: i64) -> String {
        let dispatch = self.dispatch(Some(coordinate), start);
        self.bind(&dispatch, harness, session, start + 1);
        self.end(&dispatch, end);
        dispatch
    }

    fn commit(&self, name: &str, time: i64, coordinate: Option<&str>) {
        fs::write(self.worktree.join(name), name).unwrap();
        git(&self.worktree, &["add", name]);
        let message = coordinate.map_or_else(
            || format!("feat: {name}"),
            |coordinate| format!("feat: {name}\n\nImplemented-by: {coordinate}"),
        );
        let output = Command::new("git")
            .current_dir(&self.worktree)
            .env(
                "GIT_COMMITTER_DATE",
                format!("@{} +0000", 1_577_836_800 + time),
            )
            .args(["commit", "-m", &message])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn snapshot(&self) -> std::process::Output {
        let output = self
            .lead()
            .args(["snapshot", &self.change])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn state(&self) -> Value {
        json_stdout(self.lead().args(["show", &self.change, "--json"]))
    }

    fn status(&self) -> Value {
        json_stdout(self.lead().args(["status", &self.change, "--json"]))
    }

    fn assert_recorder(&self, output: &std::process::Output, reason: &str) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("attribution: recorder retained:"),
            "{stderr}"
        );
        assert!(stderr.contains(reason), "{stderr}");
        assert!(
            stderr.contains("--contributors and --on-behalf-of, or --solo"),
            "{stderr}"
        );
        assert_eq!(
            stderr
                .lines()
                .filter(|line| line.starts_with("attribution:"))
                .count(),
            1
        );
        let state = self.state();
        let ps = state["patchsets"].as_array().unwrap().last().unwrap();
        assert_eq!(ps["actor"], "Lead", "{state}");
        assert!(ps["on_behalf_of"].is_null(), "{state}");
        assert!(ps.get("contributors").is_none(), "{state}");
        assert!(ps.get("inherited_authors").is_none(), "{state}");
    }
}

/// A lead's plain snapshot credits the bound executor and its coordinate;
/// rejects recorder authorship, self-approval by that executor, and lost gate credit for the lead.
#[test]
fn completed_run_attributes_executor_and_preserves_independence() {
    let f = Fixture::new();
    let dispatch = f.run(COORDINATE, "codex", "executor-one", 0, 20);
    f.commit("work", 10, Some(COORDINATE));
    let output = f.snapshot();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(&dispatch) && text.contains(EXECUTOR) && text.contains(COORDINATE),
        "{text}"
    );
    let state = f.state();
    let ps = &state["patchsets"][0];
    assert_eq!(ps["contributors"], json!([EXECUTOR]), "{state}");
    assert_eq!(ps["on_behalf_of"], COORDINATE, "{state}");
    assert_eq!(
        ps["inherited_authors"][0]["session"], "executor-one",
        "{state}"
    );
    assert_eq!(
        ps["inherited_authors"][0]["model"], "writer-model#high",
        "{state}"
    );
    stdout_contains(&f, "Implemented-by: codex:writer-model#high");
    stdout(
        f.lead()
            .args(["review", &f.change, "--verdict", "approved"]),
    );
    assert_eq!(f.status()["has_valid_approval"], true);
    stdout(
        f.repo
            .arc(&f.worktree)
            .env("ARC_ACTOR", EXECUTOR)
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", "executor-one")
            .args(["review", &f.change, "--verdict", "approved"]),
    );
    let status = f.status();
    assert_eq!(status["has_valid_approval"], false, "{status}");
    assert!(
        status["approval_rejection_reason"]
            .as_str()
            .unwrap()
            .contains(EXECUTOR),
        "{status}"
    );
}

fn stdout_contains(f: &Fixture, expected: &str) {
    let text = stdout(f.lead().args(["trailers", &f.change]));
    assert!(text.contains(expected), "{text}");
}

/// Several disjoint implementation runs retain every bound session and model;
/// rejects crediting only the final executor or borrowing the lead's coordinate.
#[test]
fn several_executors_are_all_authors() {
    let f = Fixture::new();
    let first = f.run(COORDINATE, "codex", "executor-one", 0, 20);
    f.commit("one", 10, Some(COORDINATE));
    let second = f.run("claude:other-writer#high", "claude", "executor-two", 30, 50);
    f.commit("two", 40, Some("claude:other-writer#high"));
    let output = f.snapshot();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains(&first) && text.contains(&second), "{text}");
    let state = f.state();
    assert_eq!(
        state["patchsets"][0]["contributors"],
        json!([EXECUTOR, "claude:executor-two"]),
        "{state}"
    );
    assert_eq!(
        state["patchsets"][0]["inherited_authors"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "{state}"
    );
    stdout_contains(&f, "Implemented-by: claude:other-writer#high");
}

/// Uncovered recorder work and the recorder's own bound run both include it;
/// rejects allowing that recorder to approve a mixed patchset as independent.
#[test]
fn invoker_work_is_included_inside_and_outside_its_own_run() {
    for own_run in [false, true] {
        let f = Fixture::new();
        f.run(COORDINATE, "codex", "executor-one", 0, 20);
        f.commit("executor", 10, Some(COORDINATE));
        if own_run {
            f.run("claude:reader-model#high", "claude", "lead-session", 30, 50);
        }
        f.commit("lead", 40, None);
        f.snapshot();
        assert_eq!(
            f.state()["patchsets"][0]["contributors"],
            json!([EXECUTOR, "Lead"])
        );
        stdout(
            f.lead()
                .args(["review", &f.change, "--verdict", "approved"]),
        );
        assert_eq!(f.status()["has_valid_approval"], false);
    }
}

fn ambiguous_run_evidence(case: &str) {
    let f = Fixture::new();
    let dispatch = f.dispatch((case != "coordinate").then_some(COORDINATE), 0);
    let binding = if case != "unbound" {
        Some(f.bind(&dispatch, "codex", "executor-one", 1))
    } else {
        None
    };
    if case != "open" {
        f.end(&dispatch, 20);
    }
    if case == "worktree" {
        let file = f
            .repo
            .root
            .join(".git/arc/repository/events")
            .join(format!("{dispatch}.json"));
        let mut event: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        event["worktree"] = json!(f.repo.root);
        fs::write(file, serde_json::to_vec(&event).unwrap()).unwrap();
    }
    if case == "overlap" {
        f.run("claude:other-writer#high", "claude", "executor-two", 0, 20);
    }
    if case == "binding-conflict" || case == "reviewer" || case == "reviewer-conflict" {
        let file = f
            .repo
            .root
            .join(".git/arc/repository/events")
            .join(format!("{}.json", binding.unwrap()));
        let mut event: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        if case == "reviewer" || case == "reviewer-conflict" {
            event["role"] = json!("reviewer");
            fs::write(&file, serde_json::to_vec(&event).unwrap()).unwrap();
        }
        if case == "binding-conflict" || case == "reviewer-conflict" {
            event["event_id"] = json!("01ZZZZZZZZZZZZZZZZZZZZZZZZ");
            event["executor_session"] = json!("conflicting-session");
            event["role"] = json!("implementer");
            fs::write(
                file.with_file_name("01ZZZZZZZZZZZZZZZZZZZZZZZZ.json"),
                serde_json::to_vec(&event).unwrap(),
            )
            .unwrap();
        }
    }
    if case == "reviewer-conflict" {
        f.run(COORDINATE, "codex", "executor-two", 0, 20);
    }
    f.commit(
        "work",
        if case == "outside" { 30 } else { 10 },
        if case == "trailers" {
            None
        } else {
            Some(COORDINATE)
        },
    );
    let output = f.snapshot();
    f.assert_recorder(
        &output,
        match case {
            "open" => "has not ended",
            "unbound" => "has no bind",
            "outside" | "reviewer" => "outside a matching completed interval",
            "worktree" => "different or unavailable worktree",
            "trailers" => "Implemented-by corroboration",
            "coordinate" => "no on-behalf-of coordinate",
            "overlap" => "different executors",
            "binding-conflict" | "reviewer-conflict" => "conflicting bindings",
            _ => unreachable!(),
        },
    );
}

/// Recorder attribution and an explicit remedy survive an unfinished run;
/// rejects guessing executor authorship from incomplete or conflicting evidence.
#[test]
fn open_run_keeps_recorder() {
    ambiguous_run_evidence("open");
}

/// Recorder attribution and an explicit remedy survive a dispatch without a bind;
/// rejects guessing executor authorship from incomplete or conflicting evidence.
#[test]
fn unbound_run_keeps_recorder() {
    ambiguous_run_evidence("unbound");
}

/// Recorder attribution and an explicit remedy survive an uncovered executor commit;
/// rejects guessing executor authorship from incomplete or conflicting evidence.
#[test]
fn uncovered_commit_keeps_recorder() {
    ambiguous_run_evidence("outside");
}

/// Recorder attribution and an explicit remedy survive a mismatching worktree;
/// rejects guessing executor authorship from incomplete or conflicting evidence.
#[test]
fn different_worktree_keeps_recorder() {
    ambiguous_run_evidence("worktree");
}

/// Recorder attribution and an explicit remedy survive missing trailer corroboration;
/// rejects guessing executor authorship from incomplete or conflicting evidence.
#[test]
fn missing_trailers_keeps_recorder() {
    ambiguous_run_evidence("trailers");
}

/// Recorder attribution and an explicit remedy survive an absent represented coordinate;
/// rejects guessing executor authorship from incomplete or conflicting evidence.
#[test]
fn missing_subject_keeps_recorder() {
    ambiguous_run_evidence("coordinate");
}

/// Recorder attribution and an explicit remedy survive conflicting covering executors;
/// rejects guessing executor authorship from incomplete or conflicting evidence.
#[test]
fn overlapping_executors_keeps_recorder() {
    ambiguous_run_evidence("overlap");
}

/// Recorder attribution and an explicit remedy survive conflicting session bindings;
/// rejects guessing executor authorship from incomplete or conflicting evidence.
#[test]
fn conflicting_bindings_keeps_recorder() {
    ambiguous_run_evidence("binding-conflict");
}

/// A conflicting implementation binding prevents ignoring a dispatch as reviewer work;
/// rejects using the first binding's reviewer role to bypass ambiguous authorship.
#[test]
fn conflicting_reviewer_binding_keeps_recorder() {
    ambiguous_run_evidence("reviewer-conflict");
}

/// Recorder attribution and an explicit remedy survive a reviewer binding;
/// rejects guessing executor authorship from incomplete or conflicting evidence.
#[test]
fn reviewer_run_keeps_recorder() {
    ambiguous_run_evidence("reviewer");
}

/// Explicit contributors, solo and represented subjects override available run evidence;
/// rejects adding derived sessions or coordinates to a caller's declaration.
#[test]
fn explicit_flags_win() {
    for flags in [
        vec!["--contributors", "Declared"],
        vec!["--solo"],
        vec!["--on-behalf-of", "codex:declared-model#high"],
    ] {
        let f = Fixture::new();
        f.run(COORDINATE, "codex", "executor-one", 0, 20);
        f.commit("work", 10, Some(COORDINATE));
        let output = f
            .lead()
            .args(["snapshot", &f.change])
            .args(&flags)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("via completed bound dispatches"));
        let state = f.state();
        let ps = &state["patchsets"][0];
        assert!(ps.get("inherited_authors").is_none(), "{state}");
        match flags[0] {
            "--contributors" => assert_eq!(ps["contributors"], json!(["Declared"])),
            "--solo" => assert_eq!(ps["contributors"], json!(["Lead"])),
            _ => assert_eq!(ps["on_behalf_of"], "codex:declared-model#high"),
        }
    }
}

/// A change without a dispatch preserves ordinary recorder attribution and output;
/// rejects inferred authorship based only on commit trailers.
#[test]
fn no_dispatch_preserves_defaults() {
    let f = Fixture::new();
    f.commit("work", 10, Some(COORDINATE));
    let output = f.snapshot();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("attribution:"));
    assert!(f.state()["patchsets"][0]["on_behalf_of"].is_null());
    assert!(f.state()["patchsets"][0].get("contributors").is_none());
}

/// Each snapshot considers only commits added since its predecessor and is idempotent;
/// rejects retaining a previous executor for new recorder work or reattributing unchanged heads.
#[test]
fn delta_and_unchanged_head_keep_their_boundaries() {
    let f = Fixture::new();
    f.run(COORDINATE, "codex", "executor-one", 0, 20);
    f.commit("executor", 10, Some(COORDINATE));
    f.snapshot();
    let repeated = f.snapshot();
    assert!(String::from_utf8_lossy(&repeated.stdout).contains("(unchanged)"));
    assert_eq!(f.state()["patchsets"].as_array().unwrap().len(), 1);
    f.commit("lead", 30, None);
    let output = f.snapshot();
    f.assert_recorder(&output, "no commit is covered");
}

/// A replaced previous head cannot establish executor authorship for its successor;
/// rejects attributing rewritten commits from preserved trailers and timestamps.
#[test]
fn rewritten_previous_patchset_keeps_recorder() {
    let f = Fixture::new();
    f.run(COORDINATE, "codex", "executor-one", 0, 20);
    f.commit("executor", 10, Some(COORDINATE));
    f.snapshot();
    git(&f.worktree, &["reset", "--hard", "HEAD^"]);
    f.commit("rewritten", 10, Some(COORDINATE));
    let output = f.snapshot();
    f.assert_recorder(&output, "previous patchset or base was rewritten");
}

/// Review's composed snapshot uses the same executor attribution as a plain snapshot;
/// rejects a recorder self-approval caused by bypassing derivation in review.
#[test]
fn review_snapshot_uses_derived_authorship() {
    let f = Fixture::new();
    f.run(COORDINATE, "codex", "executor-one", 0, 20);
    f.commit("work", 10, Some(COORDINATE));
    stdout(
        f.lead()
            .args(["review", &f.change, "--snapshot", "--verdict", "approved"]),
    );
    assert_eq!(f.state()["patchsets"][0]["contributors"], json!([EXECUTOR]));
    assert_eq!(f.status()["has_valid_approval"], true);
}

/// A binding recorded after its end still credits only the next patchset's new executor;
/// rejects using bind time as the production boundary or carrying the previous executor.
#[test]
fn late_binding_and_next_executor_use_the_commit_delta() {
    let f = Fixture::new();
    f.run(COORDINATE, "codex", "executor-one", 0, 20);
    f.commit("one", 10, Some(COORDINATE));
    f.snapshot();
    let coordinate = "claude:second-writer#high";
    let dispatch = f.dispatch(Some(coordinate), 30);
    f.end(&dispatch, 50);
    f.bind(&dispatch, "claude", "executor-two", 60);
    f.commit("two", 40, Some(coordinate));
    f.snapshot();
    let state = f.state();
    assert_eq!(
        state["patchsets"][1]["contributors"],
        json!(["claude:executor-two"])
    );
    assert_eq!(
        state["patchsets"][1]["inherited_authors"][0]["model"],
        "second-writer#high"
    );
}

/// Fractional dispatch times cannot credit a commit timestamp before dispatch;
/// rejects rounding event times down to manufacture interval coverage.
#[test]
fn fractional_dispatch_boundary_keeps_recorder() {
    let f = Fixture::new();
    let dispatch = f.run(COORDINATE, "codex", "executor-one", 10, 20);
    let file = f
        .repo
        .root
        .join(".git/arc/repository/events")
        .join(format!("{dispatch}.json"));
    let mut event: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    event["created_at"] = json!("2020-01-01T00:00:10.500Z");
    fs::write(file, serde_json::to_vec(&event).unwrap()).unwrap();
    f.commit("work", 10, Some(COORDINATE));
    f.assert_recorder(&f.snapshot(), "outside a matching completed interval");
}

/// Recorded rewrites bar attribution even when their commits retain matching times and trailers;
/// rejects treating a rewrite's successor as the dispatched executor's original commit.
#[test]
fn recorded_rewrite_keeps_recorder() {
    let f = Fixture::new();
    f.run(COORDINATE, "codex", "executor-one", 0, 20);
    f.commit("work", 10, Some(COORDINATE));
    let old = f.repo.head(&f.worktree);
    let output = Command::new("git")
        .current_dir(&f.worktree)
        .env("GIT_COMMITTER_DATE", "@1577836810 +0000")
        .args([
            "commit",
            "--amend",
            "-m",
            &format!("feat: rewritten\n\nImplemented-by: {COORDINATE}"),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let map = f.repo.home.join("map.txt");
    fs::write(&map, format!("{old} {}\n", f.repo.head(&f.worktree))).unwrap();
    stdout(f.lead().args([
        "history",
        "rewrite",
        "--map",
        map.to_str().unwrap(),
        "--reason",
        "record fixture rewrite",
    ]));
    f.assert_recorder(&f.snapshot(), "recorded rewrite touches");
}

/// A done invocation records executor authorship through the shared snapshot path;
/// rejects crediting its recorder merely because snapshot was composed with gate execution.
#[test]
fn done_uses_derived_authorship() {
    let f = Fixture::new();
    f.run(COORDINATE, "codex", "executor-one", 0, 20);
    f.commit("work", 10, Some(COORDINATE));
    let output = f.lead().args(["done", &f.change]).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(f.state()["patchsets"][0]["contributors"], json!([EXECUTOR]));
}

/// A session that changes author coordinates across runs keeps recorder attribution;
/// rejects dropping one of that session's authoring models from independence checks.
#[test]
fn changing_executor_coordinate_keeps_recorder() {
    let f = Fixture::new();
    f.run(COORDINATE, "codex", "executor-one", 0, 20);
    f.commit("one", 10, Some(COORDINATE));
    let changed = "codex:different-writer#high";
    f.run(changed, "codex", "executor-one", 30, 50);
    f.commit("two", 40, Some(changed));
    f.assert_recorder(&f.snapshot(), "different author coordinates");
}

/// One ambiguous commit retains recorder attribution for the entire snapshot;
/// rejects keeping partial executor attribution after another overlapping run has not ended.
#[test]
fn ambiguous_tail_discards_partial_derivation() {
    let f = Fixture::new();
    f.run(COORDINATE, "codex", "executor-one", 0, 20);
    f.commit("one", 10, Some(COORDINATE));
    let dispatch = f.dispatch(Some(COORDINATE), 30);
    f.bind(&dispatch, "codex", "executor-one", 31);
    f.commit("two", 40, Some(COORDINATE));
    f.assert_recorder(&f.snapshot(), "has not ended");
}

/// Mechanical replay preserves a human's portable identity without omission notes;
/// rejects treating a retained person as an agent with an unestablished model.
#[test]
fn mechanical_replay_preserves_human_contribution_trailers() {
    let f = Fixture::new();
    let person = "Person <person@example.test>";
    f.commit("work", 10, None);
    stdout(
        f.lead()
            .args(["snapshot", &f.change, "--contributors", person]),
    );
    f.repo
        .commit(&f.repo.root, "target", "target", "feat: target");
    stdout(f.lead().args(["rebase", &f.change]));
    stdout_contains(&f, "Implemented-by: Person <person@example.test>");
    let state = f.state();
    assert!(
        state["patchsets"][1]["inherited_authors"].is_array(),
        "{state}"
    );
    let trailers = json_stdout(f.lead().args(["trailers", &f.change, "--json"]));
    assert!(
        !trailers["notes"].as_array().unwrap().iter().any(|note| {
            note.as_str()
                .unwrap()
                .contains("retained author has no established")
        }),
        "{trailers}"
    );
}
