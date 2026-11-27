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

fn check_optional_role_flag(command: &str, flag: &str) {
    let repo = Repo::new();
    let (id, _) = open(&repo, "role-order");
    let (_, closed_worktree) = open(&repo, "closed-role-order");
    repo.arc(&closed_worktree)
        .args(["close", "closed-role-order", "--abandoned"])
        .assert()
        .success();
    for reference in [&id[..], "role-order", "closed-role-order"] {
        let mut cmd = repo.arc(&repo.root);
        cmd.args([command, flag, reference]);
        if command == "begin" {
            cmd.arg("another-change");
        } else if command == "brief" {
            cmd.args(["--body-file", "-"]).write_stdin("# Contract\n");
        }
        cmd.assert()
            .failure()
            .stderr(predicates::str::contains("put the change first"))
            .stderr(predicates::str::contains(format!("{flag}=<value>")));
    }
    let mut cmd = repo.arc(&repo.root);
    let subject = if command == "begin" {
        format!("attached-{}", flag.trim_start_matches('-'))
    } else {
        "role-order".to_string()
    };
    cmd.args([
        command,
        &format!("{flag}=codex:fixture-model#high"),
        &subject,
    ]);
    if command == "brief" {
        cmd.args(["--body-file", "-"]).write_stdin("# Contract\n");
    }
    cmd.assert().success();
}

macro_rules! role_flag_test {
    ($name:ident, $command:literal, $flag:literal) => {
        #[test]
        fn $name() {
            check_optional_role_flag($command, $flag);
        }
    };
}

role_flag_test!(optional_role_flags_roles_planner, "roles", "--planned-by");
role_flag_test!(
    optional_role_flags_roles_orchestrator,
    "roles",
    "--orchestrated-by"
);
role_flag_test!(optional_role_flags_begin_planner, "begin", "--planned-by");
role_flag_test!(
    optional_role_flags_begin_orchestrator,
    "begin",
    "--orchestrated-by"
);
role_flag_test!(optional_role_flags_brief_planner, "brief", "--planned-by");

/// Pre-integration qualifying approvals project `Reviewed-by` in canonical
/// order alongside `Planned-by`, `Implemented-by`, and `Orchestrated-by`,
/// while unresolved/placeholder/session-only reviewers become notes.
#[test]
fn pre_integration_approvals_project_reviewed_by_and_sync_across_commits() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\nbinds = \"commit\"\n");
    let (change, wt) = open(&repo, "reviewed-sync");
    stdout(repo.arc(&wt).args([
        "roles",
        &change,
        "--planned-by",
        "lead:planner#high",
        "--orchestrated-by",
        "lead:coordinator#high",
    ]));
    commit_file(&wt, "one.txt", "feat: one");
    commit_file(&wt, "two.txt", "feat: two");
    stdout(
        repo.arc(&wt)
            .args([
                "snapshot",
                &change,
                "--solo",
                "--model",
                "worker-model#medium",
            ])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&wt)
        .args(["verify", &change, "--all"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--model",
            "reviewer-model#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();
    // Corroborating approval from a human reviewer also projects Reviewed-by.
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--relation",
            "corroborates",
        ])
        .env("ARC_ACTOR", "Ada Example <ada@example.invalid>")
        .assert()
        .success();

    let (projected, notes) = trailers(&repo, &change);
    assert_eq!(
        projected,
        vec![
            "Planned-by: lead:planner#high".to_string(),
            "Implemented-by: codex:worker-model#medium".to_string(),
            "Reviewed-by: claude:reviewer-model#high".to_string(),
            "Reviewed-by: Ada Example <ada@example.invalid>".to_string(),
            "Orchestrated-by: lead:coordinator#high".to_string(),
        ],
        "{notes:#?}"
    );
    assert!(notes.is_empty(), "{notes:#?}");

    // Advisory covers all projected roles and recommends `--sync` without
    // claiming that appending role trailers invalidates approvals or gates.
    let report = json_stdout(repo.arc(&wt).args(["check", &change, "--json"]));
    assert_eq!(report["ready"], true, "{report}");
    let detail = advisory(&report, "missing-contribution-trailers")
        .unwrap_or_else(|| panic!("no missing advisory: {report}"));
    assert!(detail.contains("Planned-by: lead:planner#high"), "{detail}");
    assert!(
        detail.contains("Implemented-by: codex:worker-model#medium"),
        "{detail}"
    );
    assert!(
        detail.contains("Reviewed-by: claude:reviewer-model#high"),
        "{detail}"
    );
    assert!(
        detail.contains("Reviewed-by: Ada Example <ada@example.invalid>"),
        "{detail}"
    );
    assert!(
        detail.contains("Orchestrated-by: lead:coordinator#high"),
        "{detail}"
    );
    assert!(detail.contains("arc rewrite trailers --sync"), "{detail}");
    assert!(
        detail.contains("preserves approvals, debt waivers, and gates"),
        "{detail}"
    );

    // Dry-run previews the sync without moving HEAD.
    let before = repo.head(&wt);
    let dry =
        stdout(
            repo.arc(&wt)
                .args(["rewrite", "trailers", "--sync", "--dry-run", "--no-sign"]),
        );
    assert!(dry.contains("2 commits would be rewritten"), "{dry}");
    assert_eq!(repo.head(&wt), before);

    // Real `--sync` without `--from` defaults to target..head, stamps both
    // commits, preserves approval and commit-bound gate, and clears advisory.
    stdout(
        repo.arc(&wt)
            .args(["rewrite", "trailers", "--sync", "--no-sign"]),
    );
    assert_ne!(repo.head(&wt), before);
    for rev in ["HEAD~1", "HEAD"] {
        let message = git_out(&wt, &["show", "-s", "--format=%B", rev]);
        for line in &projected {
            assert!(message.contains(line), "{rev} missing {line}:\n{message}");
        }
    }
    let after_check = json_stdout(repo.arc(&wt).args(["check", &change, "--json"]));
    assert_eq!(after_check["ready"], true, "{after_check}");
    assert_eq!(
        advisory(&after_check, "missing-contribution-trailers"),
        None,
        "{after_check}"
    );
    // Syncing again is a no-op.
    let synced_head = repo.head(&wt);
    let noop = stdout(
        repo.arc(&wt)
            .args(["rewrite", "trailers", "--sync", "--no-sign"]),
    );
    assert!(noop.contains("changed none of them"), "{noop}");
    assert_eq!(repo.head(&wt), synced_head);

    // `--sync` refuses combination with `--drop` or `--edits`, and refuses
    // branches with no open change.
    repo.arc(&wt)
        .args(["rewrite", "trailers", "--sync", "--drop", "Planned-by"])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["rewrite", "trailers", "--sync", "--no-sign"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no open change tracks"));
}

/// Non-approvals, rejected self-approvals, findings-only non-approvals,
/// debt waivers, and post-integration audits never project `Reviewed-by`.
#[test]
fn non_qualifying_verdicts_waivers_and_audits_never_project_reviewed_by() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n[danger]\npaths = [\"*.txt\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: forbid self approval"]);

    let (change, wt) = open(&repo, "no-reviewed-by");
    commit_file(&wt, "work.txt", "feat: work");
    stdout(
        repo.arc(&wt)
            .args([
                "snapshot",
                &change,
                "--solo",
                "--model",
                "worker-model#high",
            ])
            .env("ARC_HARNESS", "codex"),
    );

    // comment-only never projects Reviewed-by.
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "comment-only",
            "--model",
            "reviewer-model#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();
    let (lines, _) = trailers(&repo, &change);
    assert!(
        !lines.iter().any(|l| l.starts_with("Reviewed-by:")),
        "{lines:?}"
    );

    // changes-requested never projects Reviewed-by.
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
            "--model",
            "reviewer-model#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();
    let (lines, _) = trailers(&repo, &change);
    assert!(
        !lines.iter().any(|l| l.starts_with("Reviewed-by:")),
        "{lines:?}"
    );

    // Rejected self-approval never projects Reviewed-by, even after a debt waiver.
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--model",
            "worker-model#xhigh",
        ])
        .env("ARC_ACTOR", "self-reviewer")
        .env("ARC_HARNESS", "codex")
        .assert()
        .success();
    repo.arc(&wt)
        .args(["debt", &change, "--reason", "waiving self-approval"])
        .assert()
        .success();
    let (lines, _) = trailers(&repo, &change);
    assert!(
        !lines.iter().any(|l| l.starts_with("Reviewed-by:")),
        "{lines:?}"
    );

    // Integrate under debt, then discharge with post-integration audit:
    // audit stays ledger-only and never projects Reviewed-by.
    repo.arc(&repo.root)
        .args(["integrate", &change, "--no-cleanup"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "audit",
            &change,
            "--verdict",
            "approved",
            "--model",
            "auditor-model#high",
        ])
        .env("ARC_ACTOR", "auditor")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();
    let (lines, notes) = trailers(&repo, &change);
    assert!(
        !lines.iter().any(|l| l.starts_with("Reviewed-by:")),
        "{lines:?}"
    );
    note_with(&notes, &["post-integration audit", "never backfilled"]);
}

/// Findings-only approvals under `review_clearance = "assessed"` never project
/// `Reviewed-by`, even when waived by debt; clearing approvals do.
#[test]
fn findings_only_verdicts_never_project_reviewed_by() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nreview_clearance = \"assessed\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: assessed clearance"]);

    let (change, wt) = open(&repo, "assessed-reviewed-by");
    commit_file(&wt, "work.txt", "feat: work");
    stdout(
        repo.arc(&wt)
            .args([
                "snapshot",
                &change,
                "--solo",
                "--model",
                "worker-model#high",
            ])
            .env("ARC_HARNESS", "codex"),
    );

    // Unassessed agent approval is findings-only under assessed clearance.
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--model",
            "reviewer-model#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();
    repo.arc(&wt)
        .args(["debt", &change, "--reason", "waiving findings-only"])
        .assert()
        .success();
    let (lines, _) = trailers(&repo, &change);
    assert!(
        !lines.iter().any(|l| l.starts_with("Reviewed-by:")),
        "findings-only approval must not project Reviewed-by: {lines:?}"
    );
}

/// Session-only reviewers are omitted from `Reviewed-by` with an explanatory note.
#[test]
fn session_only_reviewer_is_omitted_with_note() {
    let repo = Repo::new();
    let (change, wt) = open(&repo, "session-reviewer");
    commit_file(&wt, "work.txt", "feat: work");
    stdout(
        repo.arc(&wt)
            .args([
                "snapshot",
                &change,
                "--solo",
                "--model",
                "worker-model#high",
            ])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--on-behalf-of",
            &format!("codex:{EXECUTOR}"),
        ])
        .assert()
        .success();
    let (lines, notes) = trailers(&repo, &change);
    assert!(
        !lines.iter().any(|l| l.starts_with("Reviewed-by:")),
        "{lines:?}"
    );
    note_with(&notes, &["Reviewed-by omitted", EXECUTOR, "session id"]);
}

/// `arc instructions git --check` reports single-participant misuse as an
/// advisory issue (exit 0) while preserving exit 1 for malformed/placeholder lines.
#[test]
fn instructions_git_check_reports_single_participant_advisory() {
    let repo = Repo::new();
    let output = repo
        .arc(&repo.root)
        .args(["instructions", "git", "--check", "-"])
        .write_stdin(
            "feat: solo\n\nPlanned-by: claude:opus#high\nImplemented-by: claude:opus#high\nOrchestrated-by: claude:opus#high\n",
        )
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        out.contains("single-participant: Planned-by: claude:opus#high"),
        "{out}"
    );
    assert!(
        out.contains("single-participant: Orchestrated-by: claude:opus#high"),
        "{out}"
    );

    // Multi-participant block passes with no issues.
    let clean = repo
        .arc(&repo.root)
        .args(["instructions", "git", "--check", "-"])
        .write_stdin(
            "feat: multi\n\nPlanned-by: claude:opus#high\nImplemented-by: codex:gpt-6.1-sol#high\nOrchestrated-by: claude:opus#high\n",
        )
        .output()
        .unwrap();
    assert_eq!(clean.status.code(), Some(0));
    assert!(
        clean.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&clean.stdout)
    );

    // Malformed/placeholder lines still exit 1 alongside single-participant advice.
    repo.arc(&repo.root)
        .args(["instructions", "git", "--check", "-"])
        .write_stdin(
            "feat: mixed\n\nPlanned-by: claude:opus#high\nImplemented-by: claude:opus#high\nReviewed-by: codex:unknown\n",
        )
        .assert()
        .code(1)
        .stdout(predicates::str::contains("single-participant: Planned-by"))
        .stdout(predicates::str::contains("placeholder: Reviewed-by"));
}

/// An open change whose branch was deleted has no live head to match against
/// `approved_head`, so `arc trailers` must omit `Reviewed-by` and record a
/// note rather than manufacturing a match from `patchset.head`.
#[test]
fn an_open_change_with_a_deleted_branch_omits_reviewed_by() {
    let repo = Repo::new();
    let (change, wt) = open(&repo, "deleted-branch-review");
    commit_file(&wt, "work.txt", "feat: approved work");
    stdout(
        repo.arc(&wt)
            .args([
                "snapshot",
                &change,
                "--solo",
                "--model",
                "worker-model#medium",
            ])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--model",
            "reviewer-model#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    let (before_lines, _) = trailers(&repo, &change);
    assert!(
        before_lines
            .iter()
            .any(|line| line == "Reviewed-by: claude:reviewer-model#high"),
        "{before_lines:?}"
    );

    git(
        &repo.root,
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );
    git(&repo.root, &["branch", "-D", "arc/deleted-branch-review"]);

    let (after_lines, notes) = trailers(&repo, &change);
    assert!(
        !after_lines
            .iter()
            .any(|line| line.starts_with("Reviewed-by:")),
        "deleted branch must not project Reviewed-by: {after_lines:?}"
    );
    note_with(&notes, &["Reviewed-by omitted", "unreadable"]);
}

/// `arc rewrite trailers --sync` and `missing-contribution-trailers` apply the
/// single-participant rule against each commit's resulting trailer block so
/// solo `Planned-by`/`Orchestrated-by` assertions are never stamped or advised.
#[test]
fn sync_and_missing_advisory_omit_single_participant_planning_and_orchestration() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    // Solo change: planner, orchestrator, and sole implementer are the same
    // participant. Neither `--sync` nor `missing-contribution-trailers` may
    // stamp or request `Planned-by`/`Orchestrated-by`.
    let (solo, solo_wt) = open(&repo, "solo-sync");
    stdout(repo.arc(&solo_wt).args([
        "roles",
        &solo,
        "--planned-by",
        "codex:solo-model#high",
        "--orchestrated-by",
        "codex:solo-model#high",
    ]));
    commit_file(&solo_wt, "solo.txt", "feat: solo work");
    stdout(
        repo.arc(&solo_wt)
            .args(["snapshot", &solo, "--solo", "--model", "solo-model#high"])
            .env("ARC_HARNESS", "codex"),
    );

    let before_report = json_stdout_any_status(repo.arc(&solo_wt).args(["check", &solo, "--json"]));
    let detail = advisory(&before_report, "missing-contribution-trailers")
        .unwrap_or_else(|| panic!("expected missing Implemented-by advisory: {before_report}"));
    assert!(
        detail.contains("Implemented-by: codex:solo-model#high"),
        "{detail}"
    );
    assert!(
        !detail.contains("Planned-by: codex:solo-model#high"),
        "solo Planned-by must not be advised: {detail}"
    );
    assert!(
        !detail.contains("Orchestrated-by: codex:solo-model#high"),
        "solo Orchestrated-by must not be advised: {detail}"
    );

    stdout(
        repo.arc(&solo_wt)
            .args(["rewrite", "trailers", "--sync", "--no-sign"]),
    );
    let solo_msg = git_out(&solo_wt, &["show", "-s", "--format=%B", "HEAD"]);
    assert!(
        solo_msg.contains("Implemented-by: codex:solo-model#high"),
        "{solo_msg}"
    );
    assert!(
        !solo_msg.contains("Planned-by:"),
        "solo --sync must not stamp Planned-by:\n{solo_msg}"
    );
    assert!(
        !solo_msg.contains("Orchestrated-by:"),
        "solo --sync must not stamp Orchestrated-by:\n{solo_msg}"
    );
    let check_solo = repo
        .arc(&solo_wt)
        .args(["instructions", "git", "--check", "-"])
        .write_stdin(solo_msg)
        .output()
        .unwrap();
    assert_eq!(check_solo.status.code(), Some(0));
    assert!(
        check_solo.stdout.is_empty(),
        "synced solo commit must produce zero convention issues: {}",
        String::from_utf8_lossy(&check_solo.stdout)
    );
    let after_report = json_stdout_any_status(repo.arc(&solo_wt).args(["check", &solo, "--json"]));
    assert_eq!(
        advisory(&after_report, "missing-contribution-trailers"),
        None,
        "{after_report}"
    );
    let synced_head = repo.head(&solo_wt);
    let noop = stdout(
        repo.arc(&solo_wt)
            .args(["rewrite", "trailers", "--sync", "--no-sign"]),
    );
    assert!(noop.contains("changed none of them"), "{noop}");
    assert_eq!(repo.head(&solo_wt), synced_head);

    // Mixed-commit branch: commit 1 is solely implemented by the lead, while
    // commit 2 is implemented by another participant. `--sync` and the
    // advisory apply `Planned-by`/`Orchestrated-by` only to commit 2.
    let (mixed, mixed_wt) = open(&repo, "mixed-sync");
    stdout(repo.arc(&mixed_wt).args([
        "roles",
        &mixed,
        "--planned-by",
        "claude:lead#high",
        "--orchestrated-by",
        "claude:lead#high",
    ]));
    commit_file(
        &mixed_wt,
        "lead.txt",
        "feat: lead prep\n\nImplemented-by: claude:lead#high",
    );
    let lead_commit = repo.head(&mixed_wt);
    commit_file(
        &mixed_wt,
        "worker.txt",
        "feat: worker change\n\nImplemented-by: codex:worker#medium",
    );
    let worker_commit = repo.head(&mixed_wt);
    stdout(repo.arc(&mixed_wt).args(["snapshot", &mixed]));

    let mixed_before =
        json_stdout_any_status(repo.arc(&mixed_wt).args(["check", &mixed, "--json"]));
    let mixed_detail = advisory(&mixed_before, "missing-contribution-trailers")
        .unwrap_or_else(|| panic!("expected missing advisory on worker commit: {mixed_before}"));
    assert!(mixed_detail.contains(&worker_commit), "{mixed_detail}");
    assert!(
        !mixed_detail.contains(&lead_commit),
        "lead's own solo commit must not be asked for Planned-by/Orchestrated-by: {mixed_detail}"
    );

    stdout(
        repo.arc(&mixed_wt)
            .args(["rewrite", "trailers", "--sync", "--no-sign"]),
    );
    let first_msg = git_out(&mixed_wt, &["show", "-s", "--format=%B", "HEAD~1"]);
    let second_msg = git_out(&mixed_wt, &["show", "-s", "--format=%B", "HEAD"]);
    assert!(
        !first_msg.contains("Planned-by:") && !first_msg.contains("Orchestrated-by:"),
        "lead's solo commit must not gain Planned-by/Orchestrated-by:\n{first_msg}"
    );
    assert!(
        second_msg.contains("Planned-by: claude:lead#high")
            && second_msg.contains("Orchestrated-by: claude:lead#high"),
        "worker commit must gain Planned-by and Orchestrated-by:\n{second_msg}"
    );
    for msg in [first_msg, second_msg] {
        let checked = repo
            .arc(&mixed_wt)
            .args(["instructions", "git", "--check", "-"])
            .write_stdin(msg)
            .output()
            .unwrap();
        assert_eq!(checked.status.code(), Some(0));
        assert!(
            checked.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&checked.stdout)
        );
    }
    let mixed_after = json_stdout_any_status(repo.arc(&mixed_wt).args(["check", &mixed, "--json"]));
    assert_eq!(
        advisory(&mixed_after, "missing-contribution-trailers"),
        None,
        "{mixed_after}"
    );
}

/// On a mixed two-commit range where commit 1 is multi-participant and commit 2
/// is solo, `missing-contribution-trailers` must omit the bulk `--from ... --append`
/// alternative (which would stamp single-participant lines onto commit 2) and
/// recommend `arc rewrite trailers --sync`, leaving zero convention issues.
#[test]
fn missing_advisory_omits_unsafe_bulk_append_on_mixed_range() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    let (change, wt) = open(&repo, "mixed-bulk-safety");
    stdout(repo.arc(&wt).args([
        "roles",
        &change,
        "--planned-by",
        "claude:lead#high",
        "--orchestrated-by",
        "claude:lead#high",
    ]));
    commit_file(
        &wt,
        "first.txt",
        "feat: multi-participant first\n\nImplemented-by: claude:lead#high\nImplemented-by: codex:worker#medium",
    );
    let first_commit = repo.head(&wt);
    commit_file(
        &wt,
        "second.txt",
        "feat: solo second\n\nImplemented-by: claude:lead#high",
    );
    let second_commit = repo.head(&wt);
    stdout(repo.arc(&wt).args(["snapshot", &change]));

    let before = json_stdout_any_status(repo.arc(&wt).args(["check", &change, "--json"]));
    let detail = advisory(&before, "missing-contribution-trailers")
        .unwrap_or_else(|| panic!("expected missing advisory on first commit: {before}"));
    assert!(detail.contains(&first_commit), "{detail}");
    assert!(
        !detail.contains(&second_commit),
        "solo second commit must not be reported missing Planned-by/Orchestrated-by: {detail}"
    );
    assert!(detail.contains("arc rewrite trailers --sync"), "{detail}");
    assert!(
        !detail.contains("--from") && !detail.contains("--append"),
        "advisory must omit unsafe bulk --from/--append when a later commit in the range is solo: {detail}"
    );

    stdout(
        repo.arc(&wt)
            .args(["rewrite", "trailers", "--sync", "--no-sign"]),
    );
    let first_msg = git_out(&wt, &["show", "-s", "--format=%B", "HEAD~1"]);
    let second_msg = git_out(&wt, &["show", "-s", "--format=%B", "HEAD"]);
    assert!(
        first_msg.contains("Planned-by: claude:lead#high")
            && first_msg.contains("Orchestrated-by: claude:lead#high"),
        "first commit must gain Planned-by and Orchestrated-by:\n{first_msg}"
    );
    assert!(
        !second_msg.contains("Planned-by:") && !second_msg.contains("Orchestrated-by:"),
        "solo second commit must not gain Planned-by or Orchestrated-by:\n{second_msg}"
    );
    for msg in [first_msg, second_msg] {
        let checked = repo
            .arc(&wt)
            .args(["instructions", "git", "--check", "-"])
            .write_stdin(msg)
            .output()
            .unwrap();
        assert_eq!(checked.status.code(), Some(0));
        assert!(
            checked.stdout.is_empty(),
            "synced commit must have zero convention issues: {}",
            String::from_utf8_lossy(&checked.stdout)
        );
    }
    let after = json_stdout_any_status(repo.arc(&wt).args(["check", &change, "--json"]));
    assert_eq!(
        advisory(&after, "missing-contribution-trailers"),
        None,
        "{after}"
    );
}

/// On a merge DAG where an older solo sibling is merged with a newer
/// multi-participant sibling, `commits_from(head, newer_sibling)` also
/// rewrites the older solo sibling. The advisory must validate the exact
/// `commits_from` set, omit the unsafe bulk `--from/--append` command, and
/// leave every commit (including the solo sibling) clean after `--sync`.
#[test]
fn missing_advisory_omits_unsafe_bulk_append_on_merged_solo_sibling() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    let (change, wt) = open(&repo, "merged-sibling-safety");
    stdout(repo.arc(&wt).args([
        "roles",
        &change,
        "--planned-by",
        "claude:lead#high",
        "--orchestrated-by",
        "claude:lead#high",
    ]));
    let base = repo.head(&wt);

    // Older sibling: solo lead commit (Planned-by/Orchestrated-by inapplicable).
    git(&wt, &["checkout", "-b", "older-sibling", &base]);
    commit_file(
        &wt,
        "older.txt",
        "feat: older solo sibling\n\nImplemented-by: claude:lead#high",
    );
    let older_sibling = repo.head(&wt);

    // Newer sibling on the change branch: multi-participant commit.
    git(&wt, &["checkout", "arc/merged-sibling-safety"]);
    commit_file(
        &wt,
        "newer.txt",
        "feat: newer multi-participant sibling\n\nImplemented-by: claude:lead#high\nImplemented-by: codex:worker#medium",
    );
    let newer_sibling = repo.head(&wt);

    // Merge the older solo sibling into the change branch with both
    // participants on the merge commit, then delete the temporary branch ref.
    git(
        &wt,
        &[
            "merge",
            "--no-ff",
            "older-sibling",
            "-m",
            "merge: combine siblings\n\nImplemented-by: claude:lead#high\nImplemented-by: codex:worker#medium",
        ],
    );
    git(&wt, &["branch", "-D", "older-sibling"]);
    stdout(repo.arc(&wt).args(["snapshot", &change]));

    let before = json_stdout_any_status(repo.arc(&wt).args(["check", &change, "--json"]));
    let detail = advisory(&before, "missing-contribution-trailers")
        .unwrap_or_else(|| panic!("expected missing advisory on merged siblings: {before}"));
    assert!(detail.contains(&newer_sibling), "{detail}");
    assert!(
        !detail.contains(&older_sibling),
        "older solo sibling must not be reported missing Planned-by/Orchestrated-by: {detail}"
    );
    assert!(detail.contains("arc rewrite trailers --sync"), "{detail}");
    assert!(
        !detail.contains("--from") && !detail.contains("--append"),
        "advisory must omit bulk --from/--append when commits_from includes a solo merged sibling: {detail}"
    );

    stdout(
        repo.arc(&wt)
            .args(["rewrite", "trailers", "--sync", "--no-sign"]),
    );
    let commits = git_out(
        &wt,
        &["rev-list", "--topo-order", "--reverse", "master..HEAD"],
    );
    let commits: Vec<&str> = commits.lines().collect();
    assert_eq!(commits.len(), 3, "{commits:?}");
    let mut saw_solo_untouched = false;
    let mut saw_multi_stamped = 0usize;
    for rev in &commits {
        let msg = git_out(&wt, &["show", "-s", "--format=%B", rev]);
        if msg.contains("older solo sibling") {
            assert!(
                !msg.contains("Planned-by:") && !msg.contains("Orchestrated-by:"),
                "older solo sibling must not gain Planned-by or Orchestrated-by:\n{msg}"
            );
            saw_solo_untouched = true;
        } else {
            assert!(
                msg.contains("Planned-by: claude:lead#high")
                    && msg.contains("Orchestrated-by: claude:lead#high"),
                "multi-participant commit must gain Planned-by and Orchestrated-by:\n{msg}"
            );
            saw_multi_stamped += 1;
        }
        let checked = repo
            .arc(&wt)
            .args(["instructions", "git", "--check", "-"])
            .write_stdin(msg)
            .output()
            .unwrap();
        assert_eq!(checked.status.code(), Some(0));
        assert!(
            checked.stdout.is_empty(),
            "synced commit {rev} must have zero convention issues: {}",
            String::from_utf8_lossy(&checked.stdout)
        );
    }
    assert!(saw_solo_untouched);
    assert_eq!(saw_multi_stamped, 2);

    let after = json_stdout_any_status(repo.arc(&wt).args(["check", &change, "--json"]));
    assert_eq!(
        advisory(&after, "missing-contribution-trailers"),
        None,
        "{after}"
    );
}

/// Appending `Implemented-by` for the sole existing `Planned-by` participant
/// would introduce a `single-participant` violation into a clean commit
/// message. Neither `missing-contribution-trailers` nor `arc rewrite trailers
/// --sync` may recommend or apply that unsafe append; when a second distinct
/// implementer is also present/projected, both apply cleanly.
#[test]
fn sync_and_advisory_never_introduce_single_participant_violation_on_existing_planner() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    // Case 1: single commit whose only trailer is `Planned-by: codex:worker#medium`,
    // snapshotted with `--contributors codex:worker#medium`.
    let (solo, solo_wt) = open(&repo, "existing-planner-solo");
    commit_file(
        &solo_wt,
        "work.txt",
        "feat: planned work\n\nPlanned-by: codex:worker#medium",
    );
    let initial_head = repo.head(&solo_wt);
    let initial_msg = git_out(&solo_wt, &["show", "-s", "--format=%B", "HEAD"]);
    let initial_check = repo
        .arc(&solo_wt)
        .args(["instructions", "git", "--check", "-"])
        .write_stdin(initial_msg.clone())
        .output()
        .unwrap();
    assert_eq!(initial_check.status.code(), Some(0));
    assert!(initial_check.stdout.is_empty());

    stdout(
        repo.arc(&solo_wt)
            .args(["snapshot", &solo, "--contributors", "codex:worker#medium"]),
    );

    let check = json_stdout_any_status(repo.arc(&solo_wt).args(["check", &solo, "--json"]));
    assert_eq!(
        advisory(&check, "missing-contribution-trailers"),
        None,
        "must not recommend appending Implemented-by that invalidates existing Planned-by: {check}"
    );

    let sync_out = stdout(
        repo.arc(&solo_wt)
            .args(["rewrite", "trailers", "--sync", "--no-sign"]),
    );
    assert!(sync_out.contains("changed none of them"), "{sync_out}");
    assert_eq!(repo.head(&solo_wt), initial_head);
    let after_msg = git_out(&solo_wt, &["show", "-s", "--format=%B", "HEAD"]);
    assert_eq!(after_msg, initial_msg);
    let after_check = repo
        .arc(&solo_wt)
        .args(["instructions", "git", "--check", "-"])
        .write_stdin(after_msg)
        .output()
        .unwrap();
    assert_eq!(after_check.status.code(), Some(0));
    assert!(after_check.stdout.is_empty());

    // Case 2: mixed two-commit range where commit 1 has no trailers and commit 2
    // has only `Planned-by: codex:worker#medium` (`--contributors codex:worker#medium`).
    // Advisory must report missing only on commit 1, omit the unsafe bulk `--from/--append`,
    // and `--sync` must stamp commit 1 while leaving commit 2 clean.
    let (mixed, mixed_wt) = open(&repo, "existing-planner-mixed");
    commit_file(&mixed_wt, "one.txt", "feat: bare first");
    let first_commit = repo.head(&mixed_wt);
    commit_file(
        &mixed_wt,
        "two.txt",
        "feat: planned second\n\nPlanned-by: codex:worker#medium",
    );
    let second_commit = repo.head(&mixed_wt);
    stdout(
        repo.arc(&mixed_wt)
            .args(["snapshot", &mixed, "--contributors", "codex:worker#medium"]),
    );

    let mixed_check = json_stdout_any_status(repo.arc(&mixed_wt).args(["check", &mixed, "--json"]));
    let detail = advisory(&mixed_check, "missing-contribution-trailers")
        .unwrap_or_else(|| panic!("expected missing advisory on bare first commit: {mixed_check}"));
    assert!(detail.contains(&first_commit), "{detail}");
    assert!(!detail.contains(&second_commit), "{detail}");
    assert!(detail.contains("arc rewrite trailers --sync"), "{detail}");
    assert!(
        !detail.contains("--from") && !detail.contains("--append"),
        "bulk --from/--append would violate single-participant on second commit: {detail}"
    );
    stdout(
        repo.arc(&mixed_wt)
            .args(["rewrite", "trailers", "--sync", "--no-sign"]),
    );
    let first_msg = git_out(&mixed_wt, &["show", "-s", "--format=%B", "HEAD~1"]);
    let second_msg = git_out(&mixed_wt, &["show", "-s", "--format=%B", "HEAD"]);
    assert!(
        first_msg.contains("Implemented-by: codex:worker#medium"),
        "{first_msg}"
    );
    assert!(
        second_msg.contains("Planned-by: codex:worker#medium")
            && !second_msg.contains("Implemented-by: codex:worker#medium"),
        "second commit must keep existing Planned-by without unsafe Implemented-by:\n{second_msg}"
    );
    for msg in [first_msg, second_msg] {
        let checked = repo
            .arc(&mixed_wt)
            .args(["instructions", "git", "--check", "-"])
            .write_stdin(msg)
            .output()
            .unwrap();
        assert_eq!(checked.status.code(), Some(0));
        assert!(checked.stdout.is_empty());
    }

    // Case 3: commit with only `Planned-by: codex:worker#medium` snapshotted with
    // TWO contributors (`codex:worker#medium` and `claude:other#high`): combined
    // result has two distinct implementers, so both apply cleanly.
    let (pair, pair_wt) = open(&repo, "existing-planner-multi");
    commit_file(
        &pair_wt,
        "pair.txt",
        "feat: paired work\n\nPlanned-by: codex:worker#medium",
    );
    stdout(repo.arc(&pair_wt).args([
        "snapshot",
        &pair,
        "--contributors",
        "codex:worker#medium",
        "--contributors",
        "claude:other#high",
    ]));
    let pair_before = json_stdout_any_status(repo.arc(&pair_wt).args(["check", &pair, "--json"]));
    let pair_detail =
        advisory(&pair_before, "missing-contribution-trailers").unwrap_or_else(|| {
            panic!("expected missing advisory for paired contributors: {pair_before}")
        });
    assert!(
        pair_detail.contains("Implemented-by: codex:worker#medium")
            && pair_detail.contains("Implemented-by: claude:other#high"),
        "{pair_detail}"
    );
    stdout(
        repo.arc(&pair_wt)
            .args(["rewrite", "trailers", "--sync", "--no-sign"]),
    );
    let pair_msg = git_out(&pair_wt, &["show", "-s", "--format=%B", "HEAD"]);
    assert!(
        pair_msg.contains("Planned-by: codex:worker#medium")
            && pair_msg.contains("Implemented-by: codex:worker#medium")
            && pair_msg.contains("Implemented-by: claude:other#high"),
        "{pair_msg}"
    );
    let pair_checked = repo
        .arc(&pair_wt)
        .args(["instructions", "git", "--check", "-"])
        .write_stdin(pair_msg)
        .output()
        .unwrap();
    assert_eq!(pair_checked.status.code(), Some(0));
    assert!(pair_checked.stdout.is_empty());
}

fn setup_antigravity_store(repo: &Repo, sessions: &[(&str, &str)]) -> (PathBuf, String) {
    use std::os::unix::fs::PermissionsExt;
    let t3_home = repo.home.join(".t3");
    let profile = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let acp = t3_home
        .join("userdata/providers/antigravity")
        .join(profile)
        .join("antigravity-acp");
    let conversations = acp.join("conversations");
    fs::create_dir_all(&conversations).unwrap();
    for (session, model) in sessions {
        fs::write(
            conversations.join(format!("{session}.db")),
            b"SQLite format 3\0",
        )
        .unwrap();
        let logs = acp
            .join("brain")
            .join(session)
            .join(".system_generated/logs");
        fs::create_dir_all(&logs).unwrap();
        let user = serde_json::json!({
            "step_index": 0,
            "source": "USER_EXPLICIT",
            "type": "USER_INPUT",
            "status": "DONE",
            "created_at": "2026-11-26T10:00:00Z",
            "content": "Implement the change."
        });
        let planner = serde_json::json!({
            "step_index": 1,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "2026-11-26T10:00:01Z",
            "model": model,
            "input_tokens": 120,
            "cache_read_tokens": 40,
            "output_tokens": 30,
            "content": "Done."
        });
        fs::write(
            logs.join("transcript_full.jsonl"),
            format!("{user}\n{planner}\n"),
        )
        .unwrap();
    }
    let bin = repo.home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let sqlite = bin.join("sqlite3");
    fs::write(
        &sqlite,
        concat!(
            "#!/bin/sh\n",
            "case \"$*\" in\n",
            "  *'trajectory_metadata_blob'*) printf '[]\\n' ;;\n",
            "  *'trajectory_meta'*)\n",
            "    for arg in \"$@\"; do\n",
            "      case \"$arg\" in\n",
            "        *.db)\n",
            "          id=\"$(basename \"$arg\" .db)\"\n",
            "          printf '[{\"cascade_id\":\"%s\"}]\\n' \"$id\"\n",
            "          exit 0\n",
            "          ;;\n",
            "      esac\n",
            "    done\n",
            "    printf '[]\\n'\n",
            "    ;;\n",
            "  *) printf '[]\\n' ;;\n",
            "esac\n",
        ),
    )
    .unwrap();
    fs::set_permissions(&sqlite, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (t3_home, path)
}

#[test]
fn antigravity_sessions_record_project_and_account_normalized_effort_coordinates() {
    let repo = Repo::new();
    let impl_session = "33333333-2222-4333-8444-000000000001";
    let rev_session = "33333333-2222-4333-8444-000000000002";
    let (t3_home, path) = setup_antigravity_store(
        &repo,
        &[
            (impl_session, "gemini-4-neon-xhigh"),
            (rev_session, "claude-opus-5-5-high"),
        ],
    );

    let as_agy = |cmd: &mut AssertCommand, session: &str| {
        cmd.env_remove("ARC_ACTOR")
            .env("ARC_HARNESS", "antigravity")
            .env("ARC_SESSION", session)
            .env("T3CODE_HOME", &t3_home)
            .env("PATH", &path);
    };

    let mut begin_cmd = repo.arc(&repo.root);
    begin_cmd.args(["begin", "agy-normalized"]);
    as_agy(&mut begin_cmd, impl_session);
    let change = opened_change_id(&stdout(&mut begin_cmd));
    let wt = repo.home.join(".worktrees/repo-agy-normalized");
    commit_file(&wt, "work.txt", "feat: antigravity work");

    let mut snap_cmd = repo.arc(&wt);
    snap_cmd.args([
        "snapshot",
        &change,
        "--solo",
        "--on-behalf-of",
        "antigravity:gemini-4-neon#xhigh",
    ]);
    as_agy(&mut snap_cmd, impl_session);
    stdout(&mut snap_cmd);

    let mut review_cmd = repo.arc(&wt);
    review_cmd.args(["review", &change, "--verdict", "approved"]);
    as_agy(&mut review_cmd, rev_session);
    stdout(&mut review_cmd);

    let mut roles_cmd = repo.arc(&wt);
    roles_cmd.args(["roles", &change, "--orchestrated-by"]);
    as_agy(&mut roles_cmd, rev_session);
    stdout(&mut roles_cmd);

    let events: Vec<serde_json::Value> =
        stdout(repo.arc(&repo.root).args(["events", "--change", &change]))
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(events[0]["model"], "gemini-4-neon#xhigh");
    assert!(
        events
            .iter()
            .any(|event| event["event_type"] == "verdict-recorded"
                && event["model"] == "claude-opus-5-5#high"),
        "{events:#?}"
    );

    let (projected, notes) = trailers(&repo, &change);
    assert_eq!(
        projected,
        vec![
            "Implemented-by: antigravity:gemini-4-neon#xhigh".to_string(),
            "Reviewed-by: antigravity:claude-opus-5-5#high".to_string(),
            "Orchestrated-by: antigravity:claude-opus-5-5#high".to_string(),
        ],
        "{notes:#?}"
    );

    let stats = json_stdout(repo.arc(&repo.root).args(["stats", "--by-model", "--json"]));
    let models = stats["models"].as_array().unwrap();
    assert!(
        models.iter().any(
            |row| row["identity"] == "antigravity:gemini-4-neon#xhigh" && row["patchsets"] == 1
        ),
        "{stats:#?}"
    );

    let mut cost_cmd = repo.arc(&repo.root);
    cost_cmd
        .args(["cost", "show", &change, "--json"])
        .env("T3CODE_HOME", &t3_home)
        .env("PATH", &path);
    let cost = json_stdout(&mut cost_cmd);
    let sessions = cost["sessions"].as_array().unwrap();
    assert!(
        sessions.iter().any(|row| {
            row["key"] == format!("antigravity:{impl_session}")
                && row["agent"] == "antigravity:gemini-4-neon#xhigh"
                && row["agent_conflict"].is_null()
        }),
        "{cost:#?}"
    );
    assert!(
        sessions.iter().any(|row| {
            row["key"] == format!("antigravity:{rev_session}")
                && row["agent"] == "antigravity:claude-opus-5-5#high"
                && row["agent_conflict"].is_null()
        }),
        "{cost:#?}"
    );
}

#[test]
fn historical_hyphenated_antigravity_coordinates_remain_readable_and_valid() {
    let repo = Repo::new();
    let (change, wt) = open(&repo, "agy-historical");
    commit_file(
        &wt,
        "legacy.txt",
        "feat: historical antigravity commit\n\nImplemented-by: antigravity:gemini-4-neon-high\n",
    );
    stdout(
        repo.arc(&wt)
            .args(["snapshot", &change, "--solo", "--model", "seed-model"])
            .env("ARC_ACTOR", format!("antigravity:{RECORDER}"))
            .env("ARC_HARNESS", "antigravity")
            .env("ARC_SESSION", RECORDER),
    );
    seed_history(&repo, &change, "seed-model", "gemini-4-neon-high");

    let (projected, notes) = trailers(&repo, &change);
    assert_eq!(
        projected,
        vec!["Implemented-by: antigravity:gemini-4-neon-high".to_string()],
        "{notes:#?}"
    );
    let checked = repo
        .arc(&wt)
        .args(["instructions", "git", "--check", "-"])
        .write_stdin(git_out(&wt, &["show", "-s", "--format=%B", "HEAD"]))
        .output()
        .unwrap();
    assert_eq!(checked.status.code(), Some(0));
    assert!(checked.stdout.is_empty());

    let stats = json_stdout(repo.arc(&repo.root).args(["stats", "--by-model", "--json"]));
    assert!(
        stats["models"].as_array().unwrap().iter().any(|row| {
            row["identity"] == "antigravity:gemini-4-neon-high"
                && row["identity_kind"] == "coordinate"
        }),
        "{stats:#?}"
    );
}

#[test]
fn antigravity_cost_show_prices_effort_specific_base_and_mixed_effort_recordings() {
    let repo = Repo::new();
    let single_session = "44444444-2222-4333-8444-000000000001";
    let mixed_session = "44444444-2222-4333-8444-000000000002";
    let (t3_home, path) = setup_antigravity_store(
        &repo,
        &[
            (single_session, "gemini-4-neon-xhigh"),
            (mixed_session, "gemini-4-neon-xhigh"),
        ],
    );
    // Overwrite `mixed_session` with two distinct effort steps: `high` and `xhigh`.
    let profile = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let mixed_log = t3_home
        .join("userdata/providers/antigravity")
        .join(profile)
        .join("antigravity-acp/brain")
        .join(mixed_session)
        .join(".system_generated/logs/transcript_full.jsonl");
    fs::write(
        &mixed_log,
        format!(
            "{}\n{}\n{}\n",
            serde_json::json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-11-26T10:00:00Z",
                "content": "Work across efforts."
            }),
            serde_json::json!({
                "step_index": 1,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-26T10:00:01Z",
                "model": "gemini-4-neon-high",
                "input_tokens": 1_000_000,
                "cache_read_tokens": 0,
                "output_tokens": 0,
                "content": "High step."
            }),
            serde_json::json!({
                "step_index": 2,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-26T10:00:02Z",
                "model": "gemini-4-neon-xhigh",
                "input_tokens": 1_000_000,
                "cache_read_tokens": 0,
                "output_tokens": 0,
                "content": "Xhigh step."
            }),
        ),
    )
    .unwrap();

    let open_as_agy = |slug: &str, session: &str| {
        let mut cmd = repo.arc(&repo.root);
        cmd.args(["begin", slug, "--no-worktree", "--target", "master"])
            .env_remove("ARC_ACTOR")
            .env("ARC_HARNESS", "antigravity")
            .env("ARC_SESSION", session)
            .env("T3CODE_HOME", &t3_home)
            .env("PATH", &path);
        let change = opened_change_id(&stdout(&mut cmd));
        let dispatch_out = stdout(
            repo.arc(&repo.root)
                .args([
                    "run",
                    "dispatch",
                    "--route",
                    "fixture-route",
                    "--worktree",
                    "/work/run",
                    "--change",
                    &change,
                ])
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION"),
        );
        let dispatch = dispatch_out
            .lines()
            .find_map(|line| line.strip_prefix("event: "))
            .unwrap()
            .to_string();
        stdout(
            repo.arc(&repo.root)
                .args([
                    "run",
                    "bind",
                    &dispatch,
                    "--executor-harness",
                    "antigravity",
                    "--executor-session",
                    session,
                ])
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION"),
        );
        stdout(
            repo.arc(&repo.root)
                .args(["run", "end", &dispatch, "--outcome", "completed"])
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION"),
        );
        change
    };
    let single_change = open_as_agy("agy-price-single", single_session);
    let mixed_change = open_as_agy("agy-price-mixed", mixed_session);

    let list_price_of = |change: &str, session: &str, table: &str, name: &str| {
        let prices = repo.home.join(name);
        fs::write(&prices, table).unwrap();
        let mut cmd = repo.arc(&repo.root);
        cmd.args([
            "cost",
            "show",
            change,
            "--json",
            "--prices",
            prices.to_str().unwrap(),
        ])
        .env("T3CODE_HOME", &t3_home)
        .env("PATH", &path);
        let view = json_stdout(&mut cmd);
        let row = view["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["key"] == format!("antigravity:{session}"))
            .unwrap_or_else(|| panic!("missing session {session}: {view:#}"));
        row["totals"]["quantities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q.get("partition").is_none() && q["kind"] == "list-price-estimate")
            .unwrap_or_else(|| panic!("missing list price: {row:#}"))
            .clone()
    };

    // Single `gemini-4-neon-xhigh` recording (120 uncached input, 40 cache read, 30 output):
    // at 1.0 / 0.1 / 2.0 per million -> 0.000120 + 0.000004 + 0.000060 = 0.000184 USD.
    let hyphen_only = r#"schema = "tapes-price-table/1"
currency = "USD"

[[models]]
id = "gemini-4-neon-xhigh"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.25, output = 2.0 }
"#;
    let base_only = r#"schema = "tapes-price-table/1"
currency = "USD"

[[models]]
id = "gemini-4-neon"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 0.5, cache_read = 0.05, cache_write_5m = 0.625, output = 1.0 }
"#;
    let both_rows = r#"schema = "tapes-price-table/1"
currency = "USD"

[[models]]
id = "gemini-4-neon"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 0.5, cache_read = 0.05, cache_write_5m = 0.625, output = 1.0 }

[[models]]
id = "gemini-4-neon-xhigh"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 2.0, cache_read = 0.2, cache_write_5m = 2.5, output = 4.0 }
"#;

    let amount_of = |q: &serde_json::Value| {
        q["amount"]
            .as_f64()
            .or_else(|| q["known_subtotal"].as_f64())
            .unwrap_or_else(|| panic!("expected priced figure, got {q:#}"))
    };

    let q_hyphen = list_price_of(&single_change, single_session, hyphen_only, "hyphen.toml");
    assert!(
        (amount_of(&q_hyphen) - 0.000184).abs() < 1e-12,
        "{q_hyphen:#}"
    );

    let q_base = list_price_of(&single_change, single_session, base_only, "base.toml");
    assert!((amount_of(&q_base) - 0.000092).abs() < 1e-12, "{q_base:#}");

    // Effort-specific row takes precedence over base row.
    let q_both = list_price_of(&single_change, single_session, both_rows, "both.toml");
    assert!((amount_of(&q_both) - 0.000368).abs() < 1e-12, "{q_both:#}");

    // Mixed-effort session (1M `high` + 1M `xhigh`): `high` falls back to base ($0.50),
    // `xhigh` uses effort-specific row ($2.00) -> total $2.50.
    let q_mixed = list_price_of(&mixed_change, mixed_session, both_rows, "mixed.toml");
    assert!((amount_of(&q_mixed) - 2.5).abs() < 1e-9, "{q_mixed:#}");
}

#[test]
fn cost_show_preserves_timed_requests_for_antigravity_and_pi_and_legacy_uppercase_rows() {
    let repo = Repo::new();
    let agy_timed = "55555555-2222-4333-8444-000000000001";
    let agy_upper = "55555555-2222-4333-8444-000000000002";
    let pi_timed = "019a0000-0000-7000-8000-0000000000c9";
    let (t3_home, path) = setup_antigravity_store(
        &repo,
        &[
            (agy_timed, "gemini-4-neon-xhigh"),
            (agy_upper, "gemini-4-neon-XHIGH"),
        ],
    );
    let profile = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let acp = t3_home
        .join("userdata/providers/antigravity")
        .join(profile)
        .join("antigravity-acp/brain");
    fs::write(
        acp.join(agy_timed)
            .join(".system_generated/logs/transcript_full.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            serde_json::json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-01-01T09:59:00Z",
                "content": "Timed work."
            }),
            serde_json::json!({
                "step_index": 1,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-01-01T10:00:00Z",
                "model": "gemini-4-neon-high",
                "input_tokens": 1_000_000,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "output_tokens": 0,
                "content": "Peak high."
            }),
            serde_json::json!({
                "step_index": 2,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-01-01T17:00:00Z",
                "model": "gemini-4-neon-xhigh",
                "input_tokens": 1_000_000,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "output_tokens": 0,
                "content": "Off-peak xhigh."
            }),
        ),
    )
    .unwrap();
    fs::write(
        acp.join(agy_upper)
            .join(".system_generated/logs/transcript_full.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-01-01T10:00:00Z",
                "content": "Uppercase work."
            }),
            serde_json::json!({
                "step_index": 1,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-01-01T10:01:00Z",
                "model": "gemini-4-neon-XHIGH",
                "input_tokens": 1_000_000,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "output_tokens": 0,
                "content": "Done."
            }),
        ),
    )
    .unwrap();

    // Pi session with 1M `high` at 10:00 UTC and 1M `max` at 17:00 UTC.
    let pi_path = repo
        .home
        .join(".pi/agent/sessions/--fixture-project--")
        .join(format!("2026-01-01T09-30-00-000Z_{pi_timed}.jsonl"));
    fs::create_dir_all(pi_path.parent().unwrap()).unwrap();
    let pi_lines = [
        serde_json::json!({"type": "session", "version": 3, "id": pi_timed, "timestamp": "2026-01-01T09:30:00Z", "cwd": "/fixture/project"}),
        serde_json::json!({"type": "model_change", "id": "m0", "parentId": null, "timestamp": "2026-01-01T09:30:00Z", "provider": "fixture", "modelId": "fixture-model"}),
        serde_json::json!({"type": "thinking_level_change", "id": "l0", "parentId": "m0", "timestamp": "2026-01-01T09:30:00Z", "thinkingLevel": "high"}),
        serde_json::json!({"type": "message", "id": "u0", "parentId": "l0", "timestamp": "2026-01-01T10:00:00Z", "message": {"role": "user", "timestamp": 1767261600000u64, "content": [{"type": "text", "text": "Peak"}]}}),
        serde_json::json!({"type": "message", "id": "a0", "parentId": "u0", "timestamp": "2026-01-01T10:00:00Z", "message": {"role": "assistant", "timestamp": 1767261600000u64, "provider": "fixture", "model": "fixture-model", "stopReason": "stop", "usage": {"input": 1_000_000, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 1_000_000, "cost": {"total": 1.0}}, "content": [{"type": "text", "text": "Done."}]}}),
        serde_json::json!({"type": "thinking_level_change", "id": "l1", "parentId": "a0", "timestamp": "2026-01-01T16:59:00Z", "thinkingLevel": "max"}),
        serde_json::json!({"type": "message", "id": "u1", "parentId": "l1", "timestamp": "2026-01-01T17:00:00Z", "message": {"role": "user", "timestamp": 1767286800000u64, "content": [{"type": "text", "text": "Off-peak"}]}}),
        serde_json::json!({"type": "message", "id": "a1", "parentId": "u1", "timestamp": "2026-01-01T17:00:00Z", "message": {"role": "assistant", "timestamp": 1767286800000u64, "provider": "fixture", "model": "fixture-model", "stopReason": "stop", "usage": {"input": 1_000_000, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 1_000_000, "cost": {"total": 1.0}}, "content": [{"type": "text", "text": "Done."}]}}),
    ];
    fs::write(
        &pi_path,
        pi_lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>(),
    )
    .unwrap();

    let bind_change = |slug: &str, harness: &str, session: &str| {
        let change = opened_change_id(&stdout(
            repo.arc(&repo.root)
                .args(["begin", slug, "--no-worktree", "--target", "master"])
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION"),
        ));
        let dispatch_out = stdout(
            repo.arc(&repo.root)
                .args([
                    "run",
                    "dispatch",
                    "--route",
                    "fixture-route",
                    "--worktree",
                    "/work/run",
                    "--change",
                    &change,
                ])
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION"),
        );
        let dispatch = dispatch_out
            .lines()
            .find_map(|line| line.strip_prefix("event: "))
            .unwrap()
            .to_string();
        stdout(
            repo.arc(&repo.root)
                .args([
                    "run",
                    "bind",
                    &dispatch,
                    "--executor-harness",
                    harness,
                    "--executor-session",
                    session,
                ])
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION"),
        );
        stdout(
            repo.arc(&repo.root)
                .args(["run", "end", &dispatch, "--outcome", "completed"])
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION"),
        );
        change
    };

    let c_agy_timed = bind_change("agy-timed", "antigravity", agy_timed);
    let c_agy_upper = bind_change("agy-upper", "antigravity", agy_upper);
    let c_pi_timed = bind_change("pi-timed", "pi", pi_timed);

    let session_list_price = |change: &str, key: &str, table: &str, file: &str| {
        let prices = repo.home.join(file);
        fs::write(&prices, table).unwrap();
        let mut cmd = repo.arc(&repo.root);
        cmd.args([
            "cost",
            "show",
            change,
            "--json",
            "--prices",
            prices.to_str().unwrap(),
        ])
        .env("T3CODE_HOME", &t3_home)
        .env("PATH", &path);
        let view = json_stdout(&mut cmd);
        let row = view["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["key"] == key)
            .unwrap_or_else(|| panic!("missing session {key}: {view:#}"))
            .clone();
        let list = row["totals"]["quantities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q.get("partition").is_none() && q["kind"] == "list-price-estimate")
            .unwrap_or_else(|| panic!("missing list price: {row:#}"))
            .clone();
        (row, list)
    };

    let timed_base_table = r#"schema = "tapes-price-table/2"
currency = "USD"

[[models]]
id = "gemini-4-neon"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.25, output = 4.0 }

[[models.time_of_day]]
from = "16:30"
until = "00:30"
per_million = { uncached_input = 0.5, cache_read = 0.05, cache_write_5m = 0.625, output = 2.0 }
"#;
    let (_, q_agy_base) = session_list_price(
        &c_agy_timed,
        &format!("antigravity:{agy_timed}"),
        timed_base_table,
        "timed-base.toml",
    );
    assert_eq!(q_agy_base["status"], "complete", "{q_agy_base:#}");
    assert!(
        (q_agy_base["amount"].as_f64().unwrap() - 1.5).abs() < 1e-9,
        "{q_agy_base:#}"
    );

    let timed_hash_table = r#"schema = "tapes-price-table/2"
currency = "USD"

[[models]]
id = "gemini-4-neon"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.25, output = 4.0 }

[[models]]
id = "gemini-4-neon#xhigh"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 4.0, cache_read = 0.4, cache_write_5m = 5.0, output = 16.0 }

[[models.time_of_day]]
from = "16:30"
until = "00:30"
per_million = { uncached_input = 2.0, cache_read = 0.2, cache_write_5m = 2.5, output = 8.0 }
"#;
    let (_, q_agy_hash) = session_list_price(
        &c_agy_timed,
        &format!("antigravity:{agy_timed}"),
        timed_hash_table,
        "timed-hash.toml",
    );
    assert_eq!(q_agy_hash["status"], "complete", "{q_agy_hash:#}");
    assert!(
        (q_agy_hash["amount"].as_f64().unwrap() - 3.0).abs() < 1e-9,
        "{q_agy_hash:#}"
    );

    let timed_pi_table = r#"schema = "tapes-price-table/2"
currency = "USD"

[[models]]
id = "fixture-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.25, output = 4.0 }

[[models]]
id = "fixture-model-max"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 4.0, cache_read = 0.4, cache_write_5m = 5.0, output = 16.0 }

[[models.time_of_day]]
from = "16:30"
until = "00:30"
per_million = { uncached_input = 2.0, cache_read = 0.2, cache_write_5m = 2.5, output = 8.0 }
"#;
    let (_, q_pi) = session_list_price(
        &c_pi_timed,
        &format!("pi:{pi_timed}"),
        timed_pi_table,
        "timed-pi.toml",
    );
    assert_eq!(q_pi["status"], "complete", "{q_pi:#}");
    assert!(
        (q_pi["amount"].as_f64().unwrap() - 3.0).abs() < 1e-9,
        "{q_pi:#}"
    );

    let uppercase_table = r#"schema = "tapes-price-table/1"
currency = "USD"

[[models]]
id = "gemini-4-neon-XHIGH"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 2.0, cache_read = 0.2, cache_write_5m = 2.5, output = 8.0 }
"#;
    let (_, q_upper) = session_list_price(
        &c_agy_upper,
        &format!("antigravity:{agy_upper}"),
        uppercase_table,
        "upper.toml",
    );
    assert_eq!(q_upper["status"], "complete", "{q_upper:#}");
    assert!(
        (q_upper["amount"].as_f64().unwrap() - 2.0).abs() < 1e-9,
        "{q_upper:#}"
    );
    let upper_env = stdout(
        repo.arc(&repo.root)
            .arg("env")
            .env("ANTIGRAVITY_CONVERSATION_ID", agy_upper)
            .env("T3CODE_HOME", &t3_home)
            .env("PATH", &path),
    );
    assert!(
        upper_env.contains("# model: gemini-4-neon#xhigh\n"),
        "{upper_env}"
    );
}

#[test]
fn cost_show_prices_case_distinct_antigravity_rows_in_whole_and_split_sessions() {
    let repo = Repo::new();
    let single_mixed = "66666666-2222-4333-8444-000000000001";
    let split_mixed = "66666666-2222-4333-8444-000000000002";
    let (t3_home, path) = setup_antigravity_store(
        &repo,
        &[
            (single_mixed, "gemini-4-neon-xhigh"),
            (split_mixed, "gemini-4-neon-XhIgH"),
        ],
    );
    let profile = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let acp = t3_home
        .join("userdata/providers/antigravity")
        .join(profile)
        .join("antigravity-acp/brain");

    // 1. Single recording with 1M `gemini-4-neon-XHIGH` ($2/M) + 1M `gemini-4-neon-xhigh` ($4/M) = $6.
    fs::write(
        acp.join(single_mixed)
            .join(".system_generated/logs/transcript_full.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            serde_json::json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-01-01T10:00:00Z",
                "content": "Mixed case whole."
            }),
            serde_json::json!({
                "step_index": 1,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-01-01T10:01:00Z",
                "model": "gemini-4-neon-XHIGH",
                "input_tokens": 1_000_000,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "output_tokens": 0,
                "content": "Upper."
            }),
            serde_json::json!({
                "step_index": 2,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-01-01T10:02:00Z",
                "model": "gemini-4-neon-xhigh",
                "input_tokens": 1_000_000,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "output_tokens": 0,
                "content": "Lower."
            }),
        ),
    )
    .unwrap();

    // 2. Two-change split recording: 1M `gemini-4-neon-XHIGH` ($2/M) at 10:05 and
    // 1M `gemini-4-neon-XhIgH` ($3/M) at 11:05 -> shares $2 + $3, session total $5.
    fs::write(
        acp.join(split_mixed)
            .join(".system_generated/logs/transcript_full.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            serde_json::json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-01-01T10:00:00Z",
                "content": "Mixed case split."
            }),
            serde_json::json!({
                "step_index": 1,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-01-01T10:05:00Z",
                "model": "gemini-4-neon-XHIGH",
                "input_tokens": 1_000_000,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "output_tokens": 0,
                "content": "First."
            }),
            serde_json::json!({
                "step_index": 2,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-01-01T11:05:00Z",
                "model": "gemini-4-neon-XhIgH",
                "input_tokens": 1_000_000,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "output_tokens": 0,
                "content": "Second."
            }),
        ),
    )
    .unwrap();

    let retime = |id: &str, instant: &str| {
        let name = format!("{id}.json");
        let mut pending = vec![repo.root.join(".git/arc")];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.file_name().is_some_and(|file| file == name.as_str()) {
                    let mut event: serde_json::Value =
                        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    event["created_at"] = serde_json::json!(instant);
                    fs::write(&path, serde_json::to_vec(&event).unwrap()).unwrap();
                    return;
                }
            }
        }
        panic!("no ledger event {id}");
    };
    let bind_window = |slug: &str, session: &str, start: &str, end: &str| {
        let change = opened_change_id(&stdout(
            repo.arc(&repo.root)
                .args(["begin", slug, "--no-worktree", "--target", "master"])
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION"),
        ));
        let dispatch = stdout(
            repo.arc(&repo.root)
                .args([
                    "run",
                    "dispatch",
                    "--route",
                    "fixture-route",
                    "--worktree",
                    "/work/run",
                    "--change",
                    &change,
                ])
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION"),
        )
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();
        retime(&dispatch, start);
        stdout(
            repo.arc(&repo.root)
                .args([
                    "run",
                    "bind",
                    &dispatch,
                    "--executor-harness",
                    "antigravity",
                    "--executor-session",
                    session,
                ])
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION"),
        );
        let ended = stdout(
            repo.arc(&repo.root)
                .args(["run", "end", &dispatch, "--outcome", "completed"])
                .env_remove("ARC_HARNESS")
                .env_remove("ARC_SESSION"),
        )
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();
        retime(&ended, end);
        change
    };

    let c_whole = bind_window(
        "agy-case-whole",
        single_mixed,
        "2026-01-01T09:55:00Z",
        "2026-01-01T10:10:00Z",
    );
    let c_split_one = bind_window(
        "agy-case-one",
        split_mixed,
        "2026-01-01T10:00:00Z",
        "2026-01-01T10:30:00Z",
    );
    let c_split_two = bind_window(
        "agy-case-two",
        split_mixed,
        "2026-01-01T11:00:00Z",
        "2026-01-01T11:30:00Z",
    );

    let prices = repo.home.join("case-distinct.toml");
    fs::write(
        &prices,
        r#"schema = "tapes-price-table/1"
currency = "USD"

[[models]]
id = "gemini-4-neon"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.25, output = 4.0 }

[[models]]
id = "gemini-4-neon-XHIGH"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 2.0, cache_read = 0.2, cache_write_5m = 2.5, output = 8.0 }

[[models]]
id = "gemini-4-neon-XhIgH"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 3.0, cache_read = 0.3, cache_write_5m = 3.75, output = 12.0 }

[[models]]
id = "gemini-4-neon-xhigh"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 4.0, cache_read = 0.4, cache_write_5m = 5.0, output = 16.0 }
"#,
    )
    .unwrap();

    let list_qty = |totals: &serde_json::Value| {
        totals["quantities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q.get("partition").is_none() && q["kind"] == "list-price-estimate")
            .unwrap_or_else(|| panic!("missing list price: {totals:#}"))
            .clone()
    };

    // Whole single recording: $2 + $4 = $6 (not $8).
    let whole_view = json_stdout(
        repo.arc(&repo.root)
            .args([
                "cost",
                "show",
                &c_whole,
                "--json",
                "--prices",
                prices.to_str().unwrap(),
            ])
            .env("T3CODE_HOME", &t3_home)
            .env("PATH", &path),
    );
    let whole_row = whole_view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == format!("antigravity:{single_mixed}"))
        .unwrap();
    let q_whole = list_qty(&whole_row["totals"]);
    assert_eq!(q_whole["status"], "complete", "{q_whole:#}");
    assert!(
        (q_whole["amount"].as_f64().unwrap() - 6.0).abs() < 1e-9,
        "{q_whole:#}"
    );

    // Two-change split: shares are $2 and $3, and session total is $5 (not $8).
    let split_view = json_stdout(
        repo.arc(&repo.root)
            .args([
                "cost",
                "show",
                &c_split_one,
                &c_split_two,
                "--json",
                "--prices",
                prices.to_str().unwrap(),
            ])
            .env("T3CODE_HOME", &t3_home)
            .env("PATH", &path),
    );
    let split_row = split_view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["key"] == format!("antigravity:{split_mixed}"))
        .unwrap();
    let q_split_total = list_qty(&split_row["totals"]);
    assert_eq!(q_split_total["status"], "complete", "{q_split_total:#}");
    assert!(
        (q_split_total["amount"].as_f64().unwrap() - 5.0).abs() < 1e-9,
        "{q_split_total:#}"
    );
    let shares = split_row["split"]["shares"].as_array().unwrap();
    let mut share_amounts: Vec<f64> = shares
        .iter()
        .map(|share| {
            let q = list_qty(&share["totals"]);
            assert_eq!(q["status"], "complete", "{q:#}");
            q["amount"].as_f64().unwrap()
        })
        .collect();
    share_amounts.sort_by(|a, b| a.total_cmp(b));
    assert_eq!(share_amounts.len(), 2, "{shares:#?}");
    assert!((share_amounts[0] - 2.0).abs() < 1e-9, "{share_amounts:?}");
    assert!((share_amounts[1] - 3.0).abs() < 1e-9, "{share_amounts:?}");
}

fn check_clean_trailers(repo: &Repo, cwd: &Path, message: &str) {
    let checked = repo
        .arc(cwd)
        .args(["instructions", "git", "--check", "-"])
        .write_stdin(message.to_string())
        .output()
        .unwrap();
    assert_eq!(
        checked.status.code(),
        Some(0),
        "instructions git --check failed: {}",
        String::from_utf8_lossy(&checked.stdout)
    );
    assert!(
        checked.stdout.is_empty(),
        "expected zero trailer issues:\n{}\nfor message:\n{message}",
        String::from_utf8_lossy(&checked.stdout)
    );
}

fn history_rewrite_count(repo: &Repo) -> usize {
    stdout(
        repo.arc(&repo.root)
            .args(["events", "--type", "history-rewritten"]),
    )
    .lines()
    .filter(|line| !line.trim().is_empty())
    .count()
}

/// Default-on auto-stamping on `arc integrate` (fast-forward):
/// - `--dry-run` (text and JSON) previews trailers without mutating Git or ledger
/// - execution stamps missing projected trailers via `role_additions`, carrying
///   approvals and commit-bound gates cleanly without re-verifying or re-reviewing
/// - all landed commits pass `arc instructions git --check` with zero issues.
#[test]
fn integrate_ff_auto_stamps_trailers_and_preserves_approvals_and_commit_gates() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\nbinds = \"commit\"\n");
    let (change, wt) = open(&repo, "auto-ff");
    stdout(repo.arc(&wt).args([
        "roles",
        &change,
        "--planned-by",
        "lead:planner#high",
        "--orchestrated-by",
        "lead:coordinator#high",
    ]));
    commit_file(&wt, "one.txt", "feat: one");
    let first = repo.head(&wt);
    commit_file(&wt, "two.txt", "feat: two");
    let second = repo.head(&wt);
    let expected_tree = git_out(&wt, &["rev-parse", "HEAD^{tree}"]);
    stdout(
        repo.arc(&wt)
            .args([
                "snapshot",
                &change,
                "--solo",
                "--model",
                "worker-model#medium",
            ])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&wt)
        .args(["verify", &change, "--all"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--model",
            "reviewer-model#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    // `arc check` advisory notes default auto-stamping while keeping `--sync` for PRs.
    let check = json_stdout(repo.arc(&wt).args(["check", &change, "--json"]));
    let detail = advisory(&check, "missing-contribution-trailers")
        .unwrap_or_else(|| panic!("expected missing advisory before integrate: {check}"));
    assert!(
        detail.contains("auto-stamp them by default"),
        "advisory should explain default auto-stamping: {detail}"
    );
    assert!(detail.contains("arc rewrite trailers --sync"), "{detail}");

    let master_before = repo.head(&repo.root);
    let events_before = event_count(&repo, &change);

    // Text `--dry-run` previews auto-stamping and mutates nothing.
    let dry_text = repo
        .arc(&repo.root)
        .args(["integrate", &change, "--dry-run"])
        .output()
        .unwrap();
    assert!(dry_text.status.success(), "{dry_text:?}");
    let dry_stdout = String::from_utf8_lossy(&dry_text.stdout);
    let dry_stderr = String::from_utf8_lossy(&dry_text.stderr);
    assert!(
        dry_stdout.contains("auto-trailers: would stamp 2 commits"),
        "{dry_stdout}"
    );
    assert!(
        dry_stdout.contains("Planned-by: lead:planner#high")
            && dry_stdout.contains("Implemented-by: codex:worker-model#medium")
            && dry_stdout.contains("Reviewed-by: claude:reviewer-model#high")
            && dry_stdout.contains("Orchestrated-by: lead:coordinator#high"),
        "{dry_stdout}"
    );
    assert!(
        !dry_stderr.contains("projected contribution trailers missing"),
        "dry-run with auto-stamping active must not emit missing-trailer advice: {dry_stderr}"
    );
    assert_eq!(repo.head(&repo.root), master_before);
    assert_eq!(repo.head(&wt), second);
    assert_eq!(event_count(&repo, &change), events_before);
    assert_eq!(history_rewrite_count(&repo), 0);

    // JSON `--dry-run --json` includes `auto_trailers` without mutating anything.
    let plan =
        json_stdout(
            repo.arc(&repo.root)
                .args(["integrate", &change, "--dry-run", "--json"]),
        );
    assert_eq!(plan["schema"], "arc-integration-plan/1", "{plan}");
    assert_eq!(plan["auto_trailers"]["target"], "fast-forward", "{plan}");
    assert_eq!(
        plan["auto_trailers"]["trailers"],
        serde_json::json!([
            "Planned-by: lead:planner#high",
            "Implemented-by: codex:worker-model#medium",
            "Reviewed-by: claude:reviewer-model#high",
            "Orchestrated-by: lead:coordinator#high",
        ])
    );
    assert_eq!(
        plan["auto_trailers"]["commits"],
        serde_json::json!([first, second])
    );
    assert_eq!(repo.head(&repo.root), master_before);
    assert_eq!(repo.head(&wt), second);
    assert_eq!(history_rewrite_count(&repo), 0);

    // Real integration auto-stamps both commits, records one `role_additions`
    // rewrite, preserves approval + commit-bound gate, and fast-forwards master.
    let integrated = stdout(
        repo.arc(&repo.root)
            .args(["integrate", &change, "--no-cleanup"]),
    );
    assert!(
        integrated.contains("auto-stamped trailers on arc/auto-ff"),
        "{integrated}"
    );
    let master_after = repo.head(&repo.root);
    assert_ne!(master_after, second, "commits should be stamped");
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "master^{tree}"]),
        expected_tree
    );
    assert_eq!(history_rewrite_count(&repo), 1);

    for rev in ["master~1", "master"] {
        let msg = git_out(&repo.root, &["show", "-s", "--format=%B", rev]);
        for expected in [
            "Planned-by: lead:planner#high",
            "Implemented-by: codex:worker-model#medium",
            "Reviewed-by: claude:reviewer-model#high",
            "Orchestrated-by: lead:coordinator#high",
        ] {
            assert!(msg.contains(expected), "{rev} missing {expected}:\n{msg}");
        }
        check_clean_trailers(&repo, &repo.root, &msg);
    }

    let state = json_stdout(repo.arc(&repo.root).args(["show", &change, "--json"]));
    assert_eq!(state["closure"]["outcome"], "integrated", "{state}");
    assert_eq!(
        state["closure"]["integrated_commit"],
        master_after.as_str(),
        "{state}"
    );
    assert_eq!(state["verdicts"].as_array().unwrap().len(), 1);
}

/// When commits already carry all projected trailers (or when the only missing
/// projected trailer would introduce a single-participant violation), FF
/// integration is a zero-cost no-op with no commits rewritten.
#[test]
fn integrate_ff_is_noop_when_nothing_is_missing_or_applicable() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    // Case 1: trailers already present.
    let (present, present_wt) = open(&repo, "already-stamped");
    commit_file(
        &present_wt,
        "a.txt",
        "feat: already stamped\n\nImplemented-by: codex:worker#medium\nReviewed-by: claude:rev#high\n",
    );
    let original_head = repo.head(&present_wt);
    stdout(
        repo.arc(&present_wt)
            .args(["snapshot", &present, "--solo", "--model", "worker#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&present_wt)
        .args(["verify", &present, "--all"])
        .assert()
        .success();
    repo.arc(&present_wt)
        .args([
            "review",
            &present,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    let plan =
        json_stdout(
            repo.arc(&repo.root)
                .args(["integrate", &present, "--dry-run", "--json"]),
        );
    assert!(
        plan.get("auto_trailers").is_none(),
        "no-op dry-run must omit auto_trailers: {plan}"
    );
    stdout(
        repo.arc(&repo.root)
            .args(["integrate", &present, "--no-cleanup"]),
    );
    assert_eq!(repo.head(&repo.root), original_head);
    assert_eq!(history_rewrite_count(&repo), 0);

    // Case 2: existing `Planned-by` matches sole projected `Implemented-by`.
    let (guarded, guarded_wt) = open(&repo, "single-participant-noop");
    commit_file(
        &guarded_wt,
        "b.txt",
        "feat: planned\n\nPlanned-by: codex:worker#medium\n",
    );
    let guarded_head = repo.head(&guarded_wt);
    stdout(repo.arc(&guarded_wt).args([
        "snapshot",
        &guarded,
        "--contributors",
        "codex:worker#medium",
    ]));
    repo.arc(&guarded_wt)
        .args(["verify", &guarded, "--all"])
        .assert()
        .success();
    repo.arc(&guarded_wt)
        .args(["review", &guarded, "--verdict", "approved"])
        .env("ARC_ACTOR", "reviewer")
        .assert()
        .success();

    stdout(
        repo.arc(&repo.root)
            .args(["integrate", &guarded, "--no-cleanup"]),
    );
    assert_eq!(repo.head(&repo.root), guarded_head);
    assert_eq!(history_rewrite_count(&repo), 0);
    let msg = git_out(&repo.root, &["show", "-s", "--format=%B", "master"]);
    check_clean_trailers(&repo, &repo.root, &msg);
}

/// Escape hatches: `--no-trailers` and `[trailers] auto = false` disable
/// auto-stamping cleanly; `arc doctor` validates `[trailers]` and `arc status`
/// reports `trailers.auto=false` in policy sources.
#[test]
fn integrate_respects_no_trailers_and_policy_trailers_auto_false() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    // `--no-trailers` skips auto-stamping on FF integration.
    let (flag_skip, flag_wt) = open(&repo, "flag-skip");
    commit_file(&flag_wt, "flag.txt", "feat: flag skip");
    let flag_head = repo.head(&flag_wt);
    stdout(
        repo.arc(&flag_wt)
            .args(["snapshot", &flag_skip, "--solo", "--model", "worker#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&flag_wt)
        .args(["verify", &flag_skip, "--all"])
        .assert()
        .success();
    repo.arc(&flag_wt)
        .args([
            "review",
            &flag_skip,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &flag_skip, "--no-trailers", "--no-cleanup"])
        .assert()
        .success()
        .stderr(predicates::str::contains("arc rewrite trailers --sync"));
    assert_eq!(repo.head(&repo.root), flag_head);
    assert_eq!(history_rewrite_count(&repo), 0);

    // `[trailers] auto = false` in `.arc/policy.toml`.
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[trailers]\nauto = false\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "chore: disable auto trailers"],
    );

    repo.arc(&repo.root).args(["doctor"]).assert().success();

    let (policy_skip, policy_wt) = open(&repo, "policy-skip");
    commit_file(&policy_wt, "policy.txt", "feat: policy skip");
    let policy_head = repo.head(&policy_wt);
    stdout(
        repo.arc(&policy_wt)
            .args([
                "snapshot",
                &policy_skip,
                "--solo",
                "--model",
                "worker#medium",
            ])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&policy_wt)
        .args(["verify", &policy_skip, "--all"])
        .assert()
        .success();
    repo.arc(&policy_wt)
        .args([
            "review",
            &policy_skip,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&policy_wt)
            .args(["status", &policy_skip, "--json"]),
    );
    assert_eq!(
        status["policy_sources"]["trailers.auto=false"],
        serde_json::json!([".arc/policy.toml"]),
        "{status}"
    );
    let check = json_stdout(repo.arc(&policy_wt).args(["check", &policy_skip, "--json"]));
    let detail = advisory(&check, "missing-contribution-trailers")
        .unwrap_or_else(|| panic!("expected missing advisory: {check}"));
    assert!(
        !detail.contains("auto-stamp them by default"),
        "advisory must not claim default auto-stamping when auto=false: {detail}"
    );
    assert!(detail.contains("before integration run"), "{detail}");

    repo.arc(&repo.root)
        .args(["integrate", &policy_skip, "--no-cleanup"])
        .assert()
        .success();
    assert_eq!(repo.head(&repo.root), policy_head);
    assert_eq!(history_rewrite_count(&repo), 0);

    // Malformed `[trailers]` fails `arc doctor` cleanly with `policy-unreadable`.
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[trailers]\nauto = \"yes\"\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["doctor"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("policy-unreadable"));
}

/// Shared-commit safety: FF `arc integrate` refuses to rewrite commits shared
/// by a stacked open change (both in `--dry-run` and real execution), naming
/// the stacked branch/change and recommending `arc land` or `--no-trailers`.
#[test]
fn integrate_ff_refuses_rewriting_commits_shared_with_stacked_change() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    let (lower, lower_wt) = open(&repo, "stack-lower");
    commit_file(&lower_wt, "lower.txt", "feat: lower");
    let lower_head = repo.head(&lower_wt);
    stdout(
        repo.arc(&lower_wt)
            .args(["snapshot", &lower, "--solo", "--model", "worker#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&lower_wt)
        .args(["verify", &lower, "--all"])
        .assert()
        .success();
    repo.arc(&lower_wt)
        .args([
            "review",
            &lower,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    // Stack `upper` on top of `lower_head`.
    let out = stdout(
        repo.arc(&repo.root)
            .args(["begin", "stack-upper", "--base", &lower_head]),
    );
    let upper = opened_change_id(&out);
    let upper_wt = repo.home.join(".worktrees/repo-stack-upper");
    commit_file(&upper_wt, "upper.txt", "feat: upper");

    let master_before = repo.head(&repo.root);

    // Dry-run (text + JSON) and execution all refuse with actionable guidance.
    for args in [
        vec!["integrate", &lower, "--dry-run"],
        vec!["integrate", &lower, "--dry-run", "--json"],
        vec!["integrate", &lower, "--no-cleanup"],
    ] {
        repo.arc(&repo.root)
            .args(args)
            .assert()
            .failure()
            .stderr(predicates::str::contains("refs/heads/arc/stack-upper"))
            .stderr(predicates::str::contains(&upper))
            .stderr(predicates::str::contains("arc land"))
            .stderr(predicates::str::contains("--no-trailers"));
    }
    assert_eq!(repo.head(&repo.root), master_before);
    assert_eq!(repo.head(&lower_wt), lower_head);
    assert_eq!(history_rewrite_count(&repo), 0);

    // Bypassing with `--no-trailers` fast-forwards `lower` cleanly without orphaning `upper`.
    repo.arc(&repo.root)
        .args(["integrate", &lower, "--no-trailers", "--no-cleanup"])
        .assert()
        .success();
    assert_eq!(repo.head(&repo.root), lower_head);
    assert_eq!(history_rewrite_count(&repo), 0);
}

/// Auto-stamping respects Git's `commit.gpgsign`: signing failure leaves
/// target and branch untouched, while valid SSH signing signs stamped commits.
#[test]
fn integrate_ff_auto_stamping_respects_commit_gpgsign_and_is_atomic_on_failure() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    let (change, wt) = open(&repo, "signed-ff");
    commit_file(&wt, "signed.txt", "feat: signed work");
    let orig_head = repo.head(&wt);
    stdout(
        repo.arc(&wt)
            .args(["snapshot", &change, "--solo", "--model", "worker#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&wt)
        .args(["verify", &change, "--all"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    let master_before = repo.head(&repo.root);

    // Broken signing setup: `arc integrate` fails cleanly with target and branch untouched.
    git(&repo.root, &["config", "commit.gpgsign", "true"]);
    git(&repo.root, &["config", "gpg.format", "openpgp"]);
    git(&repo.root, &["config", "gpg.program", "/bin/false"]);

    repo.arc(&repo.root)
        .args(["integrate", &change, "--no-cleanup"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("git commit-tree failed"));
    assert_eq!(repo.head(&repo.root), master_before);
    assert_eq!(repo.head(&wt), orig_head);
    assert_eq!(history_rewrite_count(&repo), 0);

    // Working SSH signing key: `arc integrate` stamps and signs the commit.
    let key = repo.home.join("auto-stamp-key");
    assert!(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status()
        .unwrap()
        .success());
    git(&repo.root, &["config", "gpg.format", "ssh"]);
    git(
        &repo.root,
        &["config", "user.signingkey", key.to_str().unwrap()],
    );

    repo.arc(&repo.root)
        .args(["integrate", &change, "--no-cleanup"])
        .assert()
        .success();
    let master_after = repo.head(&repo.root);
    assert_ne!(master_after, orig_head);
    let raw = git_out(&repo.root, &["cat-file", "commit", "master"]);
    assert!(
        raw.contains("gpgsig -----BEGIN SSH SIGNATURE-----"),
        "{raw}"
    );
    assert!(
        raw.contains("Implemented-by: codex:worker#medium")
            && raw.contains("Reviewed-by: claude:rev#high"),
        "{raw}"
    );
}

/// Merge-commit integration (`--shape no-ff`) never rewrites branch commits;
/// it appends applicable missing projected trailers directly onto the merge
/// commit message (respecting single-participant safety and `--no-trailers`).
#[test]
fn integrate_merge_commit_appends_trailers_without_rewriting_branch_commits() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    let (change, wt) = open(&repo, "merge-trailers");
    stdout(repo.arc(&wt).args([
        "roles",
        &change,
        "--planned-by",
        "lead:planner#high",
        "--orchestrated-by",
        "lead:coordinator#high",
    ]));
    commit_file(&wt, "m.txt", "feat: branch work");
    let branch_head = repo.head(&wt);
    stdout(
        repo.arc(&wt)
            .args(["snapshot", &change, "--solo", "--model", "worker#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&wt)
        .args(["verify", &change, "--all"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    // Dry-run previews merge-commit auto-stamping.
    let plan = json_stdout(repo.arc(&repo.root).args([
        "integrate",
        &change,
        "--shape",
        "no-ff",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(plan["auto_trailers"]["target"], "merge-commit", "{plan}");
    assert!(plan["auto_trailers"].get("commits").is_none(), "{plan}");
    assert_eq!(
        plan["auto_trailers"]["trailers"],
        serde_json::json!([
            "Planned-by: lead:planner#high",
            "Implemented-by: codex:worker#medium",
            "Reviewed-by: claude:rev#high",
            "Orchestrated-by: lead:coordinator#high",
        ])
    );

    let dry_text =
        stdout(
            repo.arc(&repo.root)
                .args(["integrate", &change, "--shape", "no-ff", "--dry-run"]),
        );
    assert!(
        dry_text.contains("auto-trailers: would append to merge commit message:"),
        "{dry_text}"
    );

    // Real merge-commit integration leaves branch commit untouched and appends
    // trailers onto the merge commit itself.
    repo.arc(&repo.root)
        .args(["integrate", &change, "--shape", "no-ff", "--no-cleanup"])
        .assert()
        .success();
    assert_eq!(history_rewrite_count(&repo), 0);
    assert_eq!(repo.head(&wt), branch_head);
    let second_parent = git_out(&repo.root, &["rev-parse", "master^2"]);
    assert_eq!(
        second_parent, branch_head,
        "merge commit must reference untouched branch head"
    );
    let branch_msg = git_out(&repo.root, &["show", "-s", "--format=%B", &branch_head]);
    assert!(
        !branch_msg.contains("Reviewed-by:"),
        "branch commit must not be rewritten on merge: {branch_msg}"
    );
    let merge_msg = git_out(&repo.root, &["show", "-s", "--format=%B", "master"]);
    for expected in [
        "Planned-by: lead:planner#high",
        "Implemented-by: codex:worker#medium",
        "Reviewed-by: claude:rev#high",
        "Orchestrated-by: lead:coordinator#high",
    ] {
        assert!(
            merge_msg.contains(expected),
            "merge commit missing {expected}:\n{merge_msg}"
        );
    }
    check_clean_trailers(&repo, &repo.root, &merge_msg);

    // Single-participant guard on custom merge message + `--no-trailers` opt-out.
    let (guarded, guarded_wt) = open(&repo, "merge-single-participant");
    commit_file(&guarded_wt, "g.txt", "feat: guarded");
    stdout(repo.arc(&guarded_wt).args([
        "snapshot",
        &guarded,
        "--contributors",
        "codex:worker#medium",
    ]));
    repo.arc(&guarded_wt)
        .args(["verify", &guarded, "--all"])
        .assert()
        .success();
    repo.arc(&guarded_wt)
        .args(["review", &guarded, "--verdict", "approved"])
        .env("ARC_ACTOR", "reviewer")
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "integrate",
            &guarded,
            "--shape",
            "no-ff",
            "--no-cleanup",
            "--message",
            "merge: custom\n\nPlanned-by: codex:worker#medium",
        ])
        .assert()
        .success();
    let guarded_merge_msg = git_out(&repo.root, &["show", "-s", "--format=%B", "master"]);
    assert!(
        guarded_merge_msg.contains("Planned-by: codex:worker#medium")
            && !guarded_merge_msg.contains("Implemented-by: codex:worker#medium"),
        "merge commit must not introduce single-participant violation:\n{guarded_merge_msg}"
    );
    check_clean_trailers(&repo, &repo.root, &guarded_merge_msg);

    // `--shape no-ff --no-trailers` appends nothing.
    let (plain, plain_wt) = open(&repo, "merge-no-trailers");
    commit_file(&plain_wt, "p.txt", "feat: plain");
    stdout(
        repo.arc(&plain_wt)
            .args(["snapshot", &plain, "--solo", "--model", "worker#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&plain_wt)
        .args(["verify", &plain, "--all"])
        .assert()
        .success();
    repo.arc(&plain_wt)
        .args([
            "review",
            &plain,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "integrate",
            &plain,
            "--shape",
            "no-ff",
            "--no-trailers",
            "--no-cleanup",
        ])
        .assert()
        .success();
    let plain_msg = git_out(&repo.root, &["show", "-s", "--format=%B", "master"]);
    assert!(
        !plain_msg.contains("Implemented-by:") && !plain_msg.contains("Reviewed-by:"),
        "merge --no-trailers must not append trailers:\n{plain_msg}"
    );
}

/// Stacked `arc land` stamps each layer's own projected contribution trailers
/// bottom-to-top onto its commits, previews per-member trailers in `--dry-run`,
/// and respects `--no-trailers` and `[trailers] auto = false`.
#[test]
fn land_stamps_stacked_layers_bottom_to_top_and_respects_no_trailers() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"test -f README.md\"\n");

    // Layer 1: lower
    let (lower, lower_wt) = open(&repo, "land-lower");
    stdout(
        repo.arc(&lower_wt)
            .args(["roles", &lower, "--planned-by", "lead:planner-a#high"]),
    );
    commit_file(&lower_wt, "lower.txt", "feat: lower layer");
    let lower_head = repo.head(&lower_wt);
    stdout(
        repo.arc(&lower_wt)
            .args(["snapshot", &lower, "--solo", "--model", "worker-a#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&lower_wt)
        .args([
            "review",
            &lower,
            "--verdict",
            "approved",
            "--model",
            "reviewer-a#high",
        ])
        .env("ARC_ACTOR", "reviewer-a")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    // Layer 2: upper stacked on lower
    let upper_out = stdout(repo.arc(&repo.root).args([
        "begin",
        "land-upper",
        "--base",
        &lower_head,
        "--blocked-by",
        &lower,
    ]));
    let upper = opened_change_id(&upper_out);
    let upper_wt = repo.home.join(".worktrees/repo-land-upper");
    stdout(
        repo.arc(&upper_wt)
            .args(["roles", &upper, "--orchestrated-by", "lead:coord-b#high"]),
    );
    commit_file(&upper_wt, "upper.txt", "feat: upper layer");
    let upper_head = repo.head(&upper_wt);
    let expected_tree = git_out(&upper_wt, &["rev-parse", "HEAD^{tree}"]);
    stdout(
        repo.arc(&upper_wt)
            .args(["snapshot", &upper, "--solo", "--model", "worker-b#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&upper_wt)
        .args([
            "review",
            &upper,
            "--verdict",
            "approved",
            "--model",
            "reviewer-b#high",
        ])
        .env("ARC_ACTOR", "reviewer-b")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    let master_before = repo.head(&repo.root);

    // Dry-run (text + JSON) previews per-member trailers without mutating anything.
    let dry_text = stdout(
        repo.arc(&repo.root)
            .args(["land", &lower, &upper, "--dry-run"]),
    );
    assert!(
        dry_text.contains("auto-stamping contribution trailers"),
        "{dry_text}"
    );
    assert!(
        dry_text.contains("Implemented-by: codex:worker-a#medium")
            && dry_text.contains("Implemented-by: codex:worker-b#medium"),
        "{dry_text}"
    );
    let plan =
        json_stdout(
            repo.arc(&repo.root)
                .args(["land", &lower, &upper, "--dry-run", "--json"]),
        );
    assert_eq!(plan["schema"], "arc-landing-plan/1", "{plan}");
    assert_eq!(plan["shape"], "stack", "{plan}");
    assert!(plan["target_after"].is_null(), "{plan}");
    assert_eq!(plan["tree"], expected_tree.as_str(), "{plan}");
    assert_eq!(
        plan["members"][0]["auto_trailers"],
        serde_json::json!([
            "Planned-by: lead:planner-a#high",
            "Implemented-by: codex:worker-a#medium",
            "Reviewed-by: claude:reviewer-a#high",
        ])
    );
    assert_eq!(
        plan["members"][1]["auto_trailers"],
        serde_json::json!([
            "Implemented-by: codex:worker-b#medium",
            "Reviewed-by: claude:reviewer-b#high",
            "Orchestrated-by: lead:coord-b#high",
        ])
    );
    assert_eq!(repo.head(&repo.root), master_before);

    // Real `arc land` stamps each layer bottom-to-top with only its own trailers.
    repo.arc(&repo.root)
        .args(["land", &lower, &upper])
        .assert()
        .success();
    let landed_upper = repo.head(&repo.root);
    let landed_lower = git_out(&repo.root, &["rev-parse", "master~1"]);
    assert_ne!(landed_lower, lower_head);
    assert_ne!(landed_upper, upper_head);
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "master^{tree}"]),
        expected_tree
    );

    let lower_msg = git_out(&repo.root, &["show", "-s", "--format=%B", &landed_lower]);
    assert!(
        lower_msg.contains("Planned-by: lead:planner-a#high")
            && lower_msg.contains("Implemented-by: codex:worker-a#medium")
            && lower_msg.contains("Reviewed-by: claude:reviewer-a#high")
            && !lower_msg.contains("worker-b")
            && !lower_msg.contains("coord-b"),
        "lower layer must carry only lower trailers:\n{lower_msg}"
    );
    check_clean_trailers(&repo, &repo.root, &lower_msg);

    let upper_msg = git_out(&repo.root, &["show", "-s", "--format=%B", &landed_upper]);
    assert!(
        upper_msg.contains("Implemented-by: codex:worker-b#medium")
            && upper_msg.contains("Reviewed-by: claude:reviewer-b#high")
            && upper_msg.contains("Orchestrated-by: lead:coord-b#high")
            && !upper_msg.contains("worker-a")
            && !upper_msg.contains("planner-a"),
        "upper layer must carry only upper trailers:\n{upper_msg}"
    );
    check_clean_trailers(&repo, &repo.root, &upper_msg);

    // `--no-trailers` passes through to every landed layer and lands exact SHAs.
    let (l2, l2_wt) = open(&repo, "land-plain-lower");
    commit_file(&l2_wt, "l2.txt", "feat: l2");
    let l2_head = repo.head(&l2_wt);
    stdout(
        repo.arc(&l2_wt)
            .args(["snapshot", &l2, "--solo", "--model", "worker-a#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&l2_wt)
        .args([
            "review",
            &l2,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    let u2_out = stdout(repo.arc(&repo.root).args([
        "begin",
        "land-plain-upper",
        "--base",
        &l2_head,
        "--blocked-by",
        &l2,
    ]));
    let u2 = opened_change_id(&u2_out);
    let u2_wt = repo.home.join(".worktrees/repo-land-plain-upper");
    commit_file(&u2_wt, "u2.txt", "feat: u2");
    let u2_head = repo.head(&u2_wt);
    stdout(
        repo.arc(&u2_wt)
            .args(["snapshot", &u2, "--solo", "--model", "worker-b#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&u2_wt)
        .args([
            "review",
            &u2,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    repo.arc(&repo.root)
        .args(["land", &l2, &u2, "--no-trailers"])
        .assert()
        .success();
    assert_eq!(repo.head(&repo.root), u2_head);
    assert_eq!(git_out(&repo.root, &["rev-parse", "master~1"]), l2_head);

    // Composed (replayed) landing stamps Implemented-by/Planned-by on replayed
    // commits while omitting Reviewed-by on replayed-unread members, and
    // respects `[trailers] auto = false` as well as abandoning cleanly on
    // signing failure.
    let (p1, p1_wt) = open(&repo, "composed-a");
    commit_file(&p1_wt, "ca.txt", "feat: ca");
    stdout(
        repo.arc(&p1_wt)
            .args(["snapshot", &p1, "--solo", "--model", "worker-a#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&p1_wt)
        .args([
            "review",
            &p1,
            "--verdict",
            "approved",
            "--model",
            "rev-a#high",
        ])
        .env("ARC_ACTOR", "rev-a")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    let (p2, p2_wt) = open(&repo, "composed-b");
    commit_file(&p2_wt, "cb.txt", "feat: cb");
    stdout(
        repo.arc(&p2_wt)
            .args(["snapshot", &p2, "--solo", "--model", "worker-b#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&p2_wt)
        .args([
            "review",
            &p2,
            "--verdict",
            "approved",
            "--model",
            "rev-b#high",
        ])
        .env("ARC_ACTOR", "rev-b")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    // Signing failure during `arc land` abandons cleanly and leaves target untouched.
    let before_composed = repo.head(&repo.root);
    git(&repo.root, &["config", "commit.gpgsign", "true"]);
    git(&repo.root, &["config", "gpg.format", "openpgp"]);
    git(&repo.root, &["config", "gpg.program", "/bin/false"]);
    repo.arc(&repo.root)
        .args(["land", &p1, &p2, "--debt", "composed replay"])
        .assert()
        .failure();
    assert_eq!(repo.head(&repo.root), before_composed);
    let p1_status = json_stdout_any_status(repo.arc(&repo.root).args(["status", &p1, "--json"]));
    assert!(
        p1_status["pending_landing"].is_null(),
        "failed landing must abandon cleanly: {p1_status}"
    );
    git(&repo.root, &["config", "commit.gpgsign", "false"]);

    repo.arc(&repo.root)
        .args(["land", &p1, &p2, "--debt", "composed replay"])
        .assert()
        .success();
    let landed_p1_msg = git_out(&repo.root, &["show", "-s", "--format=%B", "master~1"]);
    let landed_p2_msg = git_out(&repo.root, &["show", "-s", "--format=%B", "master"]);
    assert!(
        landed_p1_msg.contains("Implemented-by: codex:worker-a#medium")
            && landed_p1_msg.contains("Reviewed-by: claude:rev-a#high"),
        "non-replayed bottom layer keeps Reviewed-by:\n{landed_p1_msg}"
    );
    assert!(
        landed_p2_msg.contains("Implemented-by: codex:worker-b#medium")
            && !landed_p2_msg.contains("Reviewed-by:"),
        "replayed-unread layer must stamp Implemented-by without transferring Reviewed-by:\n{landed_p2_msg}"
    );

    // `[trailers] auto = false` disables auto-stamping on `arc land`.
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[trailers]\nauto = false\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "chore: disable auto trailers"],
    );

    let (l3, l3_wt) = open(&repo, "land-policy-off");
    commit_file(&l3_wt, "l3.txt", "feat: l3");
    let l3_head = repo.head(&l3_wt);
    stdout(
        repo.arc(&l3_wt)
            .args(["snapshot", &l3, "--solo", "--model", "worker-c#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&l3_wt)
        .args([
            "review",
            &l3,
            "--verdict",
            "approved",
            "--model",
            "rev-c#high",
        ])
        .env("ARC_ACTOR", "rev-c")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();
    repo.arc(&repo.root).args(["land", &l3]).assert().success();
    assert_eq!(repo.head(&repo.root), l3_head);
}

/// Regression (`f01m8d8hp4xkhmkkfs4cgk48grk`): a locked target ref fails the
/// fast-forward multi-ref transaction atomically, leaving the source branch,
/// target ref, and ledger (zero rewrite or closure events) completely clean.
#[test]
fn integrate_ff_locked_target_ref_leaves_source_branch_and_ledger_untouched() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    let (change, wt) = open(&repo, "locked-target");
    commit_file(&wt, "work.txt", "feat: locked target test");
    let orig_head = repo.head(&wt);
    stdout(
        repo.arc(&wt)
            .args(["snapshot", &change, "--solo", "--model", "worker#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&wt)
        .args(["verify", &change, "--all"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    let master_before = repo.head(&repo.root);
    let events_before = event_count(&repo, &change);

    // Hold a lock on `refs/heads/master` so the atomic ref transaction fails.
    let lock_path = repo.root.join(".git/refs/heads/master.lock");
    fs::write(&lock_path, "").unwrap();

    repo.arc(&repo.root)
        .args(["integrate", &change, "--no-cleanup"])
        .assert()
        .failure();

    fs::remove_file(&lock_path).unwrap();

    // Target, source branch, and ledger must all remain at their pre-run state.
    assert_eq!(repo.head(&repo.root), master_before);
    assert_eq!(repo.head(&wt), orig_head);
    assert_eq!(history_rewrite_count(&repo), 0);
    assert_eq!(event_count(&repo, &change), events_before);

    // Once unlocked, integration succeeds cleanly and stamps trailers.
    repo.arc(&repo.root)
        .args(["integrate", &change, "--no-cleanup"])
        .assert()
        .success();
    assert_ne!(repo.head(&repo.root), orig_head);
    assert_eq!(repo.head(&wt), repo.head(&repo.root));
    assert_eq!(history_rewrite_count(&repo), 1);
    let msg = git_out(&repo.root, &["show", "-s", "--format=%B", "master"]);
    assert!(
        msg.contains("Implemented-by: codex:worker#medium")
            && msg.contains("Reviewed-by: claude:rev#high"),
        "{msg}"
    );
}

/// Regression (`f01m8d8ht7e7n8wwjrk3he8j455`): default auto-stamping refuses
/// commits reachable from lightweight or annotated tags in both preview and
/// execution and never silently moves tags.
#[test]
fn integrate_ff_refuses_rewriting_commits_tagged_by_lightweight_or_annotated_tag() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    let (change, wt) = open(&repo, "tagged-source");
    commit_file(&wt, "tagged.txt", "feat: tagged work");
    let orig_head = repo.head(&wt);
    stdout(
        repo.arc(&wt)
            .args(["snapshot", &change, "--solo", "--model", "worker#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&wt)
        .args(["verify", &change, "--all"])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    let master_before = repo.head(&repo.root);

    // Lightweight tag on the approved source commit.
    git(&wt, &["tag", "release", &orig_head]);
    for args in [
        vec!["integrate", &change, "--dry-run"],
        vec!["integrate", &change, "--dry-run", "--json"],
        vec!["integrate", &change, "--no-cleanup"],
    ] {
        repo.arc(&repo.root)
            .args(args)
            .assert()
            .failure()
            .stderr(predicates::str::contains("refs/tags/release"))
            .stderr(predicates::str::contains("--no-trailers"));
    }
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "refs/tags/release"]),
        orig_head
    );
    assert_eq!(repo.head(&repo.root), master_before);
    assert_eq!(repo.head(&wt), orig_head);
    assert_eq!(history_rewrite_count(&repo), 0);

    // Annotated tag on the approved source commit.
    git(&wt, &["tag", "-d", "release"]);
    git(&wt, &["tag", "-a", "v1.0", "-m", "v1.0", &orig_head]);
    let tag_obj = git_out(&repo.root, &["rev-parse", "refs/tags/v1.0"]);
    for args in [
        vec!["integrate", &change, "--dry-run"],
        vec!["integrate", &change, "--no-cleanup"],
    ] {
        repo.arc(&repo.root)
            .args(args)
            .assert()
            .failure()
            .stderr(predicates::str::contains("refs/tags/v1.0"))
            .stderr(predicates::str::contains("--no-trailers"));
    }
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "refs/tags/v1.0"]),
        tag_obj
    );
    assert_eq!(repo.head(&repo.root), master_before);
    assert_eq!(history_rewrite_count(&repo), 0);

    // `--no-trailers` integrates without moving or rewriting the tagged commit.
    repo.arc(&repo.root)
        .args(["integrate", &change, "--no-trailers", "--no-cleanup"])
        .assert()
        .success();
    assert_eq!(repo.head(&repo.root), orig_head);
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "refs/tags/v1.0"]),
        tag_obj
    );
    assert_eq!(history_rewrite_count(&repo), 0);
}

/// Regression (`f01m8d8htffdq8vskbqzmjdg97f`): a divergent `--shape no-ff`
/// merge whose resulting tree differs from the approved commit's tree must
/// withhold `Reviewed-by` in both dry-run previews and live execution while
/// still stamping other applicable roles (`Planned-by`, `Implemented-by`,
/// `Orchestrated-by`).
#[test]
fn integrate_merge_commit_withholds_reviewed_by_when_merge_tree_differs_from_approved_tree() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    let (change, wt) = open(&repo, "divergent-merge");
    stdout(repo.arc(&wt).args([
        "roles",
        &change,
        "--planned-by",
        "lead:planner#high",
        "--orchestrated-by",
        "lead:coordinator#high",
    ]));
    commit_file(&wt, "feature.txt", "feat: feature work");
    let branch_head = repo.head(&wt);
    stdout(
        repo.arc(&wt)
            .args(["snapshot", &change, "--solo", "--model", "worker#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&wt)
        .args([
            "review",
            &change,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    // Advance `master` with a disjoint commit so the merge tree differs from
    // the approved commit's tree.
    commit_file(&repo.root, "disjoint.txt", "chore: disjoint master advance");
    repo.arc(&wt)
        .args(["verify", &change, "--against", "master", "--all"])
        .assert()
        .success();

    // Dry-run (JSON and text) withholds `Reviewed-by` while keeping other roles.
    let plan = json_stdout(repo.arc(&repo.root).args([
        "integrate",
        &change,
        "--shape",
        "no-ff",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(plan["auto_trailers"]["target"], "merge-commit", "{plan}");
    assert_eq!(
        plan["auto_trailers"]["trailers"],
        serde_json::json!([
            "Planned-by: lead:planner#high",
            "Implemented-by: codex:worker#medium",
            "Orchestrated-by: lead:coordinator#high",
        ]),
        "divergent merge preview must omit Reviewed-by: {plan}"
    );
    let dry_text =
        stdout(
            repo.arc(&repo.root)
                .args(["integrate", &change, "--shape", "no-ff", "--dry-run"]),
        );
    assert!(
        !dry_text.contains("Reviewed-by:"),
        "divergent merge text dry-run must omit Reviewed-by:\n{dry_text}"
    );

    // Real divergent `--shape no-ff` merge appends other roles and omits `Reviewed-by`.
    repo.arc(&repo.root)
        .args(["integrate", &change, "--shape", "no-ff", "--no-cleanup"])
        .assert()
        .success();
    assert_eq!(git_out(&repo.root, &["rev-parse", "master^2"]), branch_head);
    let merge_msg = git_out(&repo.root, &["show", "-s", "--format=%B", "master"]);
    assert!(
        merge_msg.contains("Planned-by: lead:planner#high")
            && merge_msg.contains("Implemented-by: codex:worker#medium")
            && merge_msg.contains("Orchestrated-by: lead:coordinator#high"),
        "merge commit missing non-review roles:\n{merge_msg}"
    );
    assert!(
        !merge_msg.contains("Reviewed-by:"),
        "divergent merge commit must not credit reviewer on an unexamined merge tree:\n{merge_msg}"
    );
    check_clean_trailers(&repo, &repo.root, &merge_msg);
}

/// Regression (`f01m8d8jj47pcjakna8ga8wwqvr`): queued `--dry-run` enforces the
/// same shared-history check as live queued execution on stacked changes.
#[test]
fn integrate_queue_dry_run_refuses_stacked_shared_commits_matching_live_queue() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.smoke]\ncommand = \"true\"\n");

    let (lower, lower_wt) = open(&repo, "q-lower");
    commit_file(&lower_wt, "ql.txt", "feat: queue lower");
    let lower_head = repo.head(&lower_wt);
    stdout(
        repo.arc(&lower_wt)
            .args(["snapshot", &lower, "--solo", "--model", "worker#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&lower_wt)
        .args(["verify", &lower, "--all"])
        .assert()
        .success();
    repo.arc(&lower_wt)
        .args([
            "review",
            &lower,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    let upper_out = stdout(repo.arc(&repo.root).args([
        "begin",
        "q-upper",
        "--base",
        &lower_head,
        "--blocked-by",
        &lower,
    ]));
    let upper = opened_change_id(&upper_out);
    let upper_wt = repo.home.join(".worktrees/repo-q-upper");
    commit_file(&upper_wt, "qu.txt", "feat: queue upper");
    let upper_head = repo.head(&upper_wt);
    stdout(
        repo.arc(&upper_wt)
            .args(["snapshot", &upper, "--solo", "--model", "worker#medium"])
            .env("ARC_HARNESS", "codex"),
    );
    repo.arc(&upper_wt)
        .args(["verify", &upper, "--all"])
        .assert()
        .success();
    repo.arc(&upper_wt)
        .args([
            "review",
            &upper,
            "--verdict",
            "approved",
            "--model",
            "rev#high",
        ])
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_HARNESS", "claude")
        .assert()
        .success();

    let master_before = repo.head(&repo.root);

    // Queued `--dry-run` stops at `lower` with the shared-commit refusal,
    // matching live queued execution.
    for args in [
        vec!["integrate", &lower, &upper, "--dry-run"],
        vec!["integrate", &lower, &upper, "--no-cleanup"],
    ] {
        repo.arc(&repo.root)
            .args(args)
            .assert()
            .failure()
            .stdout(predicates::str::contains(format!("stopped: {lower}")))
            .stdout(predicates::str::contains("refs/heads/arc/q-upper"))
            .stdout(predicates::str::contains("arc land"))
            .stdout(predicates::str::contains("--no-trailers"))
            .stdout(predicates::str::contains(format!("not attempted: {upper}")));
    }
    assert_eq!(repo.head(&repo.root), master_before);
    assert_eq!(repo.head(&lower_wt), lower_head);
    assert_eq!(repo.head(&upper_wt), upper_head);
    assert_eq!(history_rewrite_count(&repo), 0);

    // With `--no-trailers`, both dry-run and live queue succeed at original SHAs.
    repo.arc(&repo.root)
        .args(["integrate", &lower, &upper, "--no-trailers", "--dry-run"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &lower, &upper, "--no-trailers", "--no-cleanup"])
        .assert()
        .success();
    assert_eq!(repo.head(&repo.root), upper_head);
    assert_eq!(history_rewrite_count(&repo), 0);
}
