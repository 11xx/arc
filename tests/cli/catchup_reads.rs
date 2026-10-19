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
