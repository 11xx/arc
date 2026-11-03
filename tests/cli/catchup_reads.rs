//! `catchup` reads what it renders once: each journal's event log is read
//! and parsed once, each open change's status report is built once, and the
//! Git processes it runs do not grow with the number of changes. Counts come from the file `ARC_TEST_LEDGER_READS`
//! names, which arc writes as it exits.

use super::common::*;
use serde_json::Value;

/// A ledger of `count` integrated changes, each with an owed `nothing-read`
/// debt whose patchset touched its own file, its worktree removed and its
/// branch kept, and a journal item filed for each, beside two open changes
/// with a snapshotted worktree each.
fn ledger(count: usize) -> Repo {
    let repo = Repo::new();
    for index in 0..2 {
        change_with_patchset(&repo, &format!("open-{index}"));
    }
    for index in 0..count {
        let slug = format!("owed-{index}");
        change_with_patchset(&repo, &slug);
        repo.arc(&repo.root)
            .args([
                "debt",
                &slug,
                "--reason",
                "no reviewer reachable",
                "--kind",
                "nothing-read",
            ])
            .assert()
            .success();
        repo.arc(&repo.root)
            .args(["integrate", &slug])
            .assert()
            .success();
        let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
        git(
            &repo.root,
            &["worktree", "remove", worktree.to_str().unwrap()],
        );
        repo.arc(&repo.root)
            .args(["journal", "todo", &format!("follow-up-{index}")])
            .write_stdin(format!("Follow up on change {index}.\n"))
            .args(["--body-file", "-"])
            .assert()
            .success();
    }
    repo
}

/// Run `arc catchup` with `args` and return its read counts and output.
pub(crate) fn counted(repo: &Repo, args: &[&str]) -> (Value, String) {
    let counts = repo.home.join("catchup-reads.json");
    let output = stdout(
        repo.arc(&repo.root)
            .env("ARC_TEST_LEDGER_READS", &counts)
            .arg("catchup")
            .args(args),
    );
    let counts: Value = serde_json::from_slice(&fs::read(&counts).unwrap()).unwrap();
    (counts, output)
}

/// Claim: `catchup`, in text and JSON, reads and parses the project
/// journal's event log once however many sections consult it. Rejects
/// orientation, the filed report and recent sessions each projecting the
/// journal again.
#[test]
fn catchup_projects_the_journal_once() {
    let repo = ledger(3);
    for args in [&[][..], &["--json"][..]] {
        let (counts, output) = counted(&repo, args);
        assert_eq!(
            counts["most_projections_per_journal"], 1,
            "catchup {args:?}: {counts}"
        );
        assert!(output.contains("follow-up-2"), "{output}");
    }
}

/// Claim: the Git processes `catchup` runs, in text and JSON, are the same
/// for a ledger of three owed debts and open changes as for one of six.
/// Rejects a Git process per change, such as a diff per debt to find the
/// paths it touched.
#[test]
fn catchup_git_processes_do_not_grow_with_the_ledger() {
    for args in [&[][..], &["--json"][..]] {
        let processes = |count: usize| {
            let repo = ledger(count);
            let (counts, output) = counted(&repo, args);
            if args.is_empty() {
                assert!(output.contains("owed-0.txt"), "{output}");
            }
            counts["git_processes"].as_u64().unwrap()
        };
        let (small, large) = (processes(3), processes(6));
        assert_eq!(
            small, large,
            "catchup {args:?}: {small} Git processes for 3 changes, {large} for 6"
        );
    }
}

/// Claim: `catchup`, in text and JSON, builds one status report per open
/// change. Rejects the text view building a listed change's report again to
/// print its review subject.
#[test]
fn catchup_builds_each_open_change_status_once() {
    let repo = ledger(3);
    for args in [&[][..], &["--json"][..]] {
        let (counts, output) = counted(&repo, args);
        assert_eq!(counts["status_reports"], 2, "catchup {args:?}: {counts}");
        assert!(output.contains("open-1"), "{output}");
    }
}

/// Claim: every owed debt over the same commit range names that range's
/// touched paths in `catchup`, both in the debt summary and beneath an open
/// change touching them. Rejects the first debt taking the range's paths and
/// leaving the next with its surfaces unknown and unmatched.
#[test]
fn catchup_names_the_surfaces_of_every_debt_over_one_range() {
    let repo = Repo::new();
    let (_, first, head) = change_with_patchset(&repo, "first");
    stdout(repo.arc(&repo.root).args(["begin", "second"]));
    let second = repo.home.join(".worktrees/repo-second");
    git(&second, &["reset", "--hard", &head]);
    stdout(repo.arc(&second).args(["snapshot", "second"]));
    git(&repo.root, &["merge", "--ff-only", &head]);
    for (slug, worktree) in [("first", &first), ("second", &second)] {
        repo.arc(&repo.root)
            .args([
                "debt",
                slug,
                "--reason",
                "no reviewer reachable",
                "--kind",
                "nothing-read",
            ])
            .assert()
            .success();
        repo.arc(worktree)
            .args(["close", slug, "--assert-integrated", &head])
            .assert()
            .success();
    }
    stdout(repo.arc(&repo.root).args(["begin", "touching"]));
    let touching = repo.home.join(".worktrees/repo-touching");
    repo.commit(&touching, "first.txt", "edited\n", "test: edit first");
    stdout(repo.arc(&touching).args(["snapshot", "touching"]));

    let output = stdout(repo.arc(&repo.root).arg("catchup"));
    let summary = output
        .lines()
        .find(|line| line.starts_with("debt-owed (2):"))
        .unwrap_or_else(|| panic!("no debt summary: {output}"));
    assert!(summary.contains("surfaces (1): first.txt;"), "{summary}");
    assert!(!summary.contains("unknown for"), "{summary}");
    for slug in ["first", "second"] {
        let line = output
            .lines()
            .find(|line| line.starts_with(&format!("    debt {slug}-")))
            .unwrap_or_else(|| panic!("{slug} not beneath touching: {output}"));
        assert!(line.contains("surfaces: first.txt;"), "{line}");
    }
}

/// Claim: `catchup`, in text and JSON, reduces each change at most once
/// however many changes the ledger holds. Rejects the journal queue's
/// inventory reducing every change again beside the ledger sections'
/// shared states.
#[test]
fn catchup_reduces_each_change_once_however_many_changes() {
    let repo = ledger(1);
    for extra in [0, 3] {
        for index in 0..extra {
            begin_without_checkout(&repo, &format!("more-{index}"), &[]);
        }
        let held = super::landing_reads::changes_held(&repo);
        for args in [&["catchup"][..], &["catchup", "--json"][..]] {
            let (record, _, code) = super::landing_reads::traced(&repo, &repo.root, args);
            assert_eq!(code, Some(0), "arc {args:?}: {record}");
            let reduced = record["counts"]["changes_reduced"].as_u64().unwrap();
            assert!(
                reduced <= held,
                "arc {args:?} reduced {reduced} changes of {held}: {record}"
            );
        }
    }
}

/// The change event files the ledger holds.
fn change_events_held(repo: &Repo) -> u64 {
    fs::read_dir(repo.root.join(".git/arc/changes"))
        .unwrap()
        .map(|change| {
            fs::read_dir(change.unwrap().path().join("events"))
                .map(|events| events.count() as u64)
                .unwrap_or(0)
        })
        .sum()
}

/// Claim: `catchup`, in text and JSON, types each change event at most once
/// however many changes the ledger holds: recent sessions take each change's
/// writers from the read that reduced it. The writers still rank: the
/// ledger's session is listed. Rejects recent sessions decoding every
/// change's events again after the ledger sections reduced them.
#[test]
fn catchup_types_each_change_event_once_however_many_changes() {
    let repo = ledger(1);
    for extra in [0, 3] {
        for index in 0..extra {
            begin_without_checkout(&repo, &format!("more-{index}"), &[]);
        }
        let held = change_events_held(&repo);
        for args in [&["catchup"][..], &["catchup", "--json"][..]] {
            let (record, _, code) = super::landing_reads::traced(&repo, &repo.root, args);
            assert_eq!(code, Some(0), "arc {args:?}: {record}");
            let typed = record["ledger"]["change"]["typed"].as_u64().unwrap();
            assert!(
                typed <= held,
                "arc {args:?} typed {typed} change events of {held}: {record}"
            );
        }
        let output = stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
        let catchup: Value = serde_json::from_str(&output).unwrap();
        assert!(
            catchup["recent_sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["harness"] == "test" && row["session"] == "session-a"),
            "{}",
            catchup["recent_sessions"]
        );
    }
}

/// Write a Claude and a Codex recording of session `index`, each made in the
/// repository's checkout.
fn record_sessions(repo: &Repo, index: usize) {
    let claude = repo.home.join(".claude/projects/-fixture");
    fs::create_dir_all(&claude).unwrap();
    let id = format!("11111111-2222-3333-4444-{index:012}");
    let row = serde_json::json!({"type":"user", "sessionId":id, "cwd":repo.root, "timestamp":"2026-01-01T00:00:00Z", "message":{"role":"user", "content":"work"}});
    fs::write(claude.join(format!("{id}.jsonl")), format!("{row}\n")).unwrap();
    let codex = repo.home.join(".codex/sessions/2026/01/01");
    fs::create_dir_all(&codex).unwrap();
    let id = format!("019f0000-0000-7000-8000-{index:012}");
    let meta = serde_json::json!({"type":"session_meta", "timestamp":"2026-01-01T00:00:00Z", "payload":{"id":id, "cwd":repo.root}});
    fs::write(
        codex.join(format!("rollout-2026-01-01T00-00-00-{id}.jsonl")),
        format!("{meta}\n"),
    )
    .unwrap();
}

/// Claim: `catchup`, in text and JSON, builds one project scope and one set
/// of tapes backends for session discovery, however many backends hold
/// recordings and however many recordings they hold. Rejects each backend's
/// listing building its own scope, with the Git processes that resolve it,
/// and its own backend set.
#[test]
fn catchup_discovers_sessions_through_one_scope_and_backend_set() {
    let repo = Repo::new();
    for recorded in [1, 3] {
        for index in 0..recorded {
            record_sessions(&repo, index);
        }
        for args in [&[][..], &["--json"][..]] {
            let counts = repo.home.join("catchup-reads.json");
            let output = stdout(
                repo.arc(&repo.root)
                    .env("CLAUDE_CONFIG_DIR", repo.home.join(".claude"))
                    .env("CODEX_HOME", repo.home.join(".codex"))
                    .env("ARC_TEST_LEDGER_READS", &counts)
                    .arg("catchup")
                    .args(args),
            );
            let counts: Value = serde_json::from_slice(&fs::read(&counts).unwrap()).unwrap();
            for harness in ["claude", "codex"] {
                assert!(output.contains(harness), "catchup {args:?}: {output}");
            }
            assert_eq!(
                (
                    &counts["tapes"]["discovery_scopes"],
                    &counts["tapes"]["discovery_backend_sets"]
                ),
                (&Value::from(1), &Value::from(1)),
                "catchup {args:?} with {recorded} recordings per harness: {counts}"
            );
        }
    }
}

/// Claim: the `ARC_TRACE` record of `catchup` counts the recordings session
/// discovery's backends scanned apart from the project's rows they kept, and
/// times each backend's listing. Rejects a record that counts a recording
/// made outside the project as kept, or leaves listing time unattributed.
#[test]
fn catchup_trace_separates_scanned_recordings_from_kept_rows() {
    let repo = Repo::new();
    for index in 0..2 {
        record_sessions(&repo, index);
    }
    let elsewhere = repo.home.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let id = "99999999-2222-3333-4444-555555555555";
    let row = serde_json::json!({"type":"user", "sessionId":id, "cwd":elsewhere, "timestamp":"2026-01-01T00:00:00Z", "message":{"role":"user", "content":"other"}});
    fs::write(
        repo.home
            .join(format!(".claude/projects/-fixture/{id}.jsonl")),
        format!("{row}\n"),
    )
    .unwrap();
    let output = repo
        .arc(&repo.root)
        .env("CLAUDE_CONFIG_DIR", repo.home.join(".claude"))
        .env("CODEX_HOME", repo.home.join(".codex"))
        .env("ARC_TRACE", "1")
        .args(["catchup", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    let record: Value = stderr
        .lines()
        .find(|line| line.contains("\"arc-perf/1\""))
        .map(|line| serde_json::from_str(line).unwrap())
        .unwrap_or_else(|| panic!("catchup writes a trace record: {stderr}"));
    assert_eq!(record["counts"]["session_candidates"], 5, "{record}");
    assert_eq!(record["counts"]["session_rows"], 4, "{record}");
    assert_eq!(record["phases"]["listing"]["calls"], 2, "{record}");
}
