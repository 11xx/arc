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
            .args([
                "snapshot",
                &change,
                "--solo",
                "--model",
                "seed-model",
                "--on-behalf-of",
                "antigravity:gemini-4-neon-high",
            ])
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
        stats["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["identity"] == "antigravity:gemini-4-neon-high"),
        "{stats:#?}"
    );
}
