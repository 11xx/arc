//! `arc trailers`, `arc roles` and integration advice never emit a
//! placeholder or a session id as a contribution coordinate. What arc cannot
//! attribute is named in a note instead, with the value and the remedy.

use crate::common::*;

const RECORDER: &str = "019f0000-0000-7000-8000-00000000000a";
const EXECUTOR: &str = "019f0000-0000-7000-8000-00000000000b";

fn open(repo: &Repo, slug: &str) -> (String, PathBuf) {
    let out = stdout(repo.arc(&repo.root).args(["begin", slug]));
    (
        opened_change_id(&out),
        repo.home.join(".worktrees").join(format!("repo-{slug}")),
    )
}

fn commit_file(worktree: &Path, file: &str, message: &str) {
    fs::write(worktree.join(file), file).unwrap();
    git(worktree, &["add", "--", file]);
    git(worktree, &["commit", "-m", message]);
}

/// The invocation of a Codex session whose actor arc derives from the
/// harness session, because nobody declared one.
fn as_derived_codex(cmd: &mut AssertCommand, session: &str) {
    cmd.env_remove("ARC_ACTOR")
        .env("ARC_HARNESS", "codex")
        .env("ARC_SESSION", session);
}

fn trailers(repo: &Repo, change: &str) -> (Vec<String>, Vec<String>) {
    let result = json_stdout(repo.arc(&repo.root).args(["trailers", change, "--json"]));
    let strings = |key: &str| -> Vec<String> {
        result[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect()
    };
    (strings("trailers"), strings("notes"))
}

fn note_with<'a>(notes: &'a [String], words: &[&str]) -> &'a str {
    notes
        .iter()
        .find(|note| words.iter().all(|word| note.contains(word)))
        .unwrap_or_else(|| panic!("no note carries {words:?}: {notes:#?}"))
}

/// Replace a value in every recorded event of a change that carries it, the
/// way a ledger written before fresh coordinates were required reads.
fn seed_history(repo: &Repo, change: &str, from: &str, to: &str) {
    let mut seeded = false;
    for entry in fs::read_dir(event_dir(repo, change)).unwrap() {
        let path = entry.unwrap().path();
        let text = fs::read_to_string(&path).unwrap();
        if text.contains(from) {
            fs::write(&path, text.replace(from, to)).unwrap();
            seeded = true;
        }
    }
    assert!(seeded, "no event carries {from:?}");
}

/// A declared recorder whose recorded model is a placeholder, crediting an
/// executor named only by its harness session.
#[test]
fn a_session_id_contributor_projects_no_implementer() {
    let repo = Repo::new();
    let (change, wt) = open(&repo, "crash-recovery");
    commit_file(&wt, "one", "feat: one");
    stdout(
        repo.arc(&wt)
            .args(["snapshot", &change, "--contributors"])
            .arg(format!("codex:{EXECUTOR}"))
            .env("ARC_ACTOR", format!("codex:{RECORDER}"))
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", RECORDER)
            .args(["--model", "seed-model"]),
    );
    seed_history(&repo, &change, "seed-model", "unknown");

    let (trailers, notes) = trailers(&repo, &change);
    assert!(
        !trailers
            .iter()
            .any(|line| line.starts_with("Implemented-by")),
        "{trailers:?}"
    );
    assert!(!trailers.iter().any(|line| line.contains(EXECUTOR)));
    let note = note_with(
        &notes,
        &["ps-01", &format!("codex:{EXECUTOR}"), "session id"],
    );
    assert!(!note.contains("codex:unknown"), "{note}");
}

/// The recorder credited as its own contributor, in session form, with a
/// placeholder recorded where its model belongs: the subject is no
/// coordinate, and neither is the recorder's.
#[test]
fn a_solo_recorder_with_a_placeholder_model_projects_no_implementer() {
    let repo = Repo::new();
    let (change, wt) = open(&repo, "solo-placeholder");
    commit_file(&wt, "one", "feat: one");
    stdout(
        repo.arc(&wt)
            .args(["snapshot", &change, "--solo"])
            .env("ARC_ACTOR", format!("codex:{RECORDER}"))
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", RECORDER)
            .args(["--model", "seed-model"]),
    );
    seed_history(&repo, &change, "seed-model", "unknown");

    let (trailers, notes) = trailers(&repo, &change);
    assert!(
        !trailers
            .iter()
            .any(|line| line.starts_with("Implemented-by")),
        "{trailers:?}"
    );
    note_with(&notes, &["ps-01", RECORDER, "session id", "placeholder"]);
    let text = stdout(repo.arc(&repo.root).args(["trailers", &change]));
    assert!(!text.contains("unknown"), "{text}");
    assert!(!text.contains(RECORDER), "{text}");
}

/// Who ran `snapshot` is not proof of who wrote the diff: a derived recorder
/// with no declaration is not credited, and the note names what it recorded
/// and how to declare the author.
#[test]
fn a_derived_recorder_is_not_credited_and_the_note_names_the_remedy() {
    let repo = Repo::new();
    let (change, wt) = open(&repo, "falsification");
    commit_file(&wt, "one", "feat: one");
    let mut snapshot = repo.arc(&wt);
    snapshot.args(["snapshot", &change, "--model", "gpt-6.1-sol#xhigh"]);
    as_derived_codex(&mut snapshot, RECORDER);
    stdout(&mut snapshot);

    let (trailers, notes) = trailers(&repo, &change);
    assert!(
        !trailers
            .iter()
            .any(|line| line.starts_with("Implemented-by")),
        "{trailers:?}"
    );
    let note = note_with(
        &notes,
        &[
            "ps-01",
            "codex:gpt-6.1-sol#xhigh",
            "--solo",
            "--contributors",
            "--amend ps-01",
        ],
    );
    assert!(note.contains("declared"), "{note}");
}

#[test]
fn solo_under_a_derived_actor_projects_the_recorder_coordinate() {
    let repo = Repo::new();
    let (change, wt) = open(&repo, "derived-solo");
    commit_file(&wt, "one", "feat: one");
    let mut snapshot = repo.arc(&wt);
    snapshot.args([
        "snapshot",
        &change,
        "--solo",
        "--model",
        "gpt-6.1-sol#xhigh",
    ]);
    as_derived_codex(&mut snapshot, RECORDER);
    stdout(&mut snapshot);

    let (trailers, notes) = trailers(&repo, &change);
    assert_eq!(
        trailers,
        vec!["Implemented-by: codex:gpt-6.1-sol#xhigh".to_string()],
        "{notes:#?}"
    );
}

/// An executor named by its session is credited with nothing, but when the
/// change's own records by that actor name exactly one model, the note
/// suggests it for the operator to confirm.
#[test]
fn a_session_contributor_note_suggests_the_model_its_own_records_name() {
    let repo = Repo::new();
    let (change, wt) = open(&repo, "suggested");
    let brief = repo.home.join("brief.md");
    fs::write(&brief, "Implement the bounded task.").unwrap();
    let mut write_brief = repo.arc(&wt);
    write_brief.args([
        "brief",
        &change,
        "--body-file",
        brief.to_str().unwrap(),
        "--model",
        "gpt-6.1-sol#medium",
    ]);
    as_derived_codex(&mut write_brief, EXECUTOR);
    stdout(&mut write_brief);
    commit_file(&wt, "one", "feat: one");
    stdout(
        repo.arc(&wt)
            .args(["snapshot", &change, "--contributors"])
            .arg(format!("codex:{EXECUTOR}"))
            .args(["--model", "lead-model#high"]),
    );

    let (trailers, notes) = trailers(&repo, &change);
    assert!(
        !trailers
            .iter()
            .any(|line| line.starts_with("Implemented-by")),
        "{trailers:?}"
    );
    note_with(
        &notes,
        &[
            "ps-01",
            &format!("codex:{EXECUTOR}"),
            "codex:gpt-6.1-sol#medium",
        ],
    );
}

#[test]
fn a_historical_placeholder_role_assertion_is_a_note_with_its_remedy() {
    let repo = Repo::new();
    let (change, _) = open(&repo, "historical-roles");
    stdout(repo.arc(&repo.root).args([
        "roles",
        &change,
        "--planned-by",
        "codex:seed-model",
        "--orchestrated-by",
        "lead:coordinator#high",
    ]));
    seed_history(&repo, &change, "codex:seed-model", "codex:unknown");

    let (trailers, notes) = trailers(&repo, &change);
    assert_eq!(
        trailers,
        vec!["Orchestrated-by: lead:coordinator#high".to_string()],
        "{notes:#?}"
    );
    note_with(
        &notes,
        &[
            "Planned-by",
            "codex:unknown",
            "placeholder",
            &format!("arc roles {change} --withdraw 'Planned-by=codex:unknown'"),
        ],
    );
    let text = stdout(repo.arc(&repo.root).args(["trailers", &change]));
    assert!(!text.contains("codex:unknown"), "{text}");
}

#[test]
fn bare_role_flags_under_a_derived_actor_assert_the_invoking_coordinate() {
    let repo = Repo::new();
    let (change, wt) = open(&repo, "bare-roles");
    codex_recording(&repo, RECORDER, "gpt-6.1-sol");
    let mut roles = repo.arc(&repo.root);
    roles
        .args(["roles", &change, "--planned-by", "--orchestrated-by"])
        .env("CODEX_HOME", repo.home.join("codex-state"));
    as_derived_codex(&mut roles, RECORDER);
    stdout(&mut roles);

    let brief = repo.home.join("brief.md");
    fs::write(&brief, "Implement the bounded task.").unwrap();
    let mut write_brief = repo.arc(&wt);
    write_brief
        .args(["brief", &change, "--planned-by", "--body-file"])
        .arg(&brief)
        .args(["--model", "gpt-6.1-sol#xhigh"]);
    as_derived_codex(&mut write_brief, EXECUTOR);
    stdout(&mut write_brief);

    let (trailers, notes) = trailers(&repo, &change);
    assert_eq!(
        trailers,
        vec![
            "Planned-by: codex:gpt-6.1-sol#high".to_string(),
            "Planned-by: codex:gpt-6.1-sol#xhigh".to_string(),
            "Orchestrated-by: codex:gpt-6.1-sol#high".to_string(),
        ],
        "{notes:#?}"
    );
    for line in &trailers {
        assert!(
            !line.contains(RECORDER) && !line.contains(EXECUTOR),
            "{line}"
        );
    }
}

#[test]
fn a_bare_role_flag_keeps_a_declared_person_in_their_own_form() {
    let repo = Repo::new();
    let (change, _) = open(&repo, "bare-person");
    stdout(
        repo.arc(&repo.root)
            .args(["roles", &change, "--planned-by"])
            .env("ARC_ACTOR", "Person <p@example.invalid>")
            .args(["--model", "unused#high"]),
    );
    let (trailers, _) = trailers(&repo, &change);
    assert_eq!(
        trailers,
        vec!["Planned-by: Person <p@example.invalid>".to_string()]
    );
}

#[test]
fn a_bare_role_flag_refuses_what_it_cannot_resolve_and_records_nothing() {
    let repo = Repo::new();
    let (change, _) = open(&repo, "bare-unresolved");
    let before = event_count(&repo, &change);

    let mut unresolved = repo.arc(&repo.root);
    unresolved.args(["roles", &change, "--planned-by"]);
    as_derived_codex(&mut unresolved, RECORDER);
    unresolved
        .env("CODEX_HOME", repo.home.join("no-codex-state"))
        .assert()
        .failure()
        .stderr(predicates::str::contains("unresolved"))
        .stderr(predicates::str::contains("omit"));

    repo.arc(&repo.root)
        .args(["roles", &change, "--orchestrated-by"])
        .env_remove("ARC_ACTOR")
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION")
        .args(["--model", "gpt-6.1-sol#xhigh"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("declared"));

    assert_eq!(event_count(&repo, &change), before);
}

fn gate_and_approve(repo: &Repo, worktree: &Path, change: &str) {
    stdout(repo.arc(worktree).args(["snapshot", change, "--solo"]));
    repo.arc(worktree)
        .args(["verify", change, "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(worktree)
        .args(["review", change, "--verdict", "approved"])
        .env("ARC_ACTOR", "reviewer")
        .assert()
        .success();
}

fn advisory<'a>(report: &'a serde_json::Value, code: &str) -> Option<&'a str> {
    report["advisories"]
        .as_array()?
        .iter()
        .find(|advisory| advisory["code"] == code)
        .map(|advisory| advisory["detail"].as_str().unwrap())
}

#[test]
fn placeholder_role_trailers_are_advised_and_never_block() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");
    let (change, wt) = open(&repo, "placeholder-advice");
    commit_file(&wt, "clean", "feat: clean");
    commit_file(
        &wt,
        "tainted",
        "feat: tainted\n\nOrchestrated-by: codex:unknown\nImplemented-by: codex:gpt-6.1-sol#xhigh\n",
    );
    let tainted = repo.head(&wt);
    gate_and_approve(&repo, &wt, &change);

    let check = repo
        .arc(&wt)
        .args(["check", &change, "--json"])
        .output()
        .unwrap();
    assert_eq!(check.status.code(), Some(0), "{check:?}");
    let report: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(report["ready"], true, "{report}");
    let detail = advisory(&report, "placeholder-contribution-trailers")
        .unwrap_or_else(|| panic!("no placeholder advisory: {report}"));
    assert!(detail.contains(&tainted), "{detail}");
    assert!(
        detail.contains("Orchestrated-by: codex:unknown"),
        "{detail}"
    );
    assert!(!detail.contains("Implemented-by"), "{detail}");

    repo.arc(&wt)
        .args(["integrate", &change, "--dry-run"])
        .assert()
        .success()
        .stderr(predicates::str::contains(
            "advice: commit role trailers carry values that are no coordinate",
        ))
        .stderr(predicates::str::contains("Orchestrated-by: codex:unknown"));
}

#[test]
fn missing_trailer_advice_never_suggests_a_placeholder_line() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");
    let (change, wt) = open(&repo, "missing-advice");
    stdout(repo.arc(&wt).args([
        "roles",
        &change,
        "--planned-by",
        "codex:seed-model",
        "--planned-by",
        "lead:planner#high",
    ]));
    seed_history(&repo, &change, "codex:seed-model", "codex:unknown");
    commit_file(&wt, "one", "feat: one");
    gate_and_approve(&repo, &wt, &change);

    let report = json_stdout_any_status(repo.arc(&wt).args(["check", &change, "--json"]));
    let detail = advisory(&report, "missing-contribution-trailers")
        .unwrap_or_else(|| panic!("no missing-trailer advisory: {report}"));
    assert!(detail.contains("Planned-by: lead:planner#high"), "{detail}");
    assert!(!detail.contains("codex:unknown"), "{detail}");
}
