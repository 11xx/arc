//! `catchup` reads what it renders once: each journal's event log is read
//! and parsed once, and the Git processes it runs do not grow with the
//! number of changes. Counts come from the file `ARC_TEST_LEDGER_READS`
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
