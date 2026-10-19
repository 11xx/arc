//! The status reads `catchup` makes for each open change (branch heads,
//! merge outcomes, ancestry, commit ranges, worktree cleanliness and touched
//! paths) are answered by Git commands batched across the listing, so the
//! number of Git commands arc runs does not grow with the number of open
//! changes. Cleanliness is per worktree in Git itself: its batch runs one
//! `git status` child per worktree, and that is the only Git process that
//! grows with them.

use super::catchup_reads::counted;
use super::common::*;
use std::collections::BTreeMap;

/// A ledger of `count` open changes, each with a snapshotted worktree, the
/// first of them carrying an untracked file, beside one integrated change
/// with an owed debt that moved the target past every open change's base.
fn open_changes(count: usize) -> Repo {
    let repo = Repo::new();
    for index in 0..count {
        change_with_patchset(&repo, &format!("open-{index}"));
    }
    let dirty = repo.home.join(".worktrees/repo-open-0/scratch.txt");
    fs::write(dirty, "not added\n").unwrap();
    change_with_patchset(&repo, "owed");
    repo.arc(&repo.root)
        .args([
            "debt",
            "owed",
            "--reason",
            "no reviewer reachable",
            "--kind",
            "nothing-read",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "owed"])
        .assert()
        .success();
    repo
}

/// Claim: `catchup`, in text and JSON, runs as many Git commands for one
/// open change as for four, each with its own worktree. Rejects a listing
/// that builds each open change's status report with its own Git commands,
/// such as a `merge-tree`, two `merge-base --is-ancestor`, a `rev-list` of
/// its range and a `status --porcelain` per change.
#[test]
fn catchup_git_processes_do_not_grow_with_open_changes() {
    for args in [&[][..], &["--json"][..]] {
        let processes = |count: usize| {
            let repo = open_changes(count);
            let (counts, output) = counted(&repo, args);
            for index in 0..count {
                assert!(output.contains(&format!("open-{index}-")), "{output}");
            }
            counts["git_processes"].as_u64().unwrap()
        };
        let (one, four) = (processes(1), processes(4));
        assert_eq!(
            one, four,
            "catchup {args:?}: {one} Git processes for 1 open change, {four} for 4"
        );
    }
}

/// Every Git process `catchup` with `args` starts, children of the commands
/// arc runs included, counted by the command name Git's trace2 records.
fn git_processes_by_name(repo: &Repo, args: &[&str]) -> BTreeMap<String, usize> {
    let events = repo.home.join("catchup-trace2.jsonl");
    let _ = fs::remove_file(&events);
    stdout(
        repo.arc(&repo.root)
            .env("GIT_TRACE2_EVENT", &events)
            .arg("catchup")
            .args(args),
    );
    let mut names = BTreeMap::new();
    for line in fs::read_to_string(&events).unwrap().lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        if event["event"] == "cmd_name" {
            *names
                .entry(event["name"].as_str().unwrap().to_string())
                .or_insert(0) += 1;
        }
    }
    names
}

/// Claim: of every Git process `catchup` starts, in text and JSON, children
/// of its batched commands included, three more open worktrees add exactly
/// three `git status`, one per worktree's cleanliness. Rejects per-change Git
/// work hidden beneath one batched command, which a count of the commands
/// arc runs cannot see.
#[test]
fn catchup_git_children_grow_only_by_one_status_per_worktree() {
    for args in [&[][..], &["--json"][..]] {
        let (one, four) = (
            git_processes_by_name(&open_changes(1), args),
            git_processes_by_name(&open_changes(4), args),
        );
        let mut expected = one.clone();
        *expected.entry("status".to_string()).or_insert(0) += 3;
        assert_eq!(
            four, expected,
            "catchup {args:?}: one open change {one:?}, four {four:?}"
        );
    }
}
