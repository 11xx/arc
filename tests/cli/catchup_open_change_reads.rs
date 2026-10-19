//! The status reads `catchup` makes for each open change (branch heads,
//! merge outcomes, ancestry, commit ranges, worktree cleanliness and touched
//! paths) are answered by Git commands batched across the listing, so the
//! number of Git processes does not grow with the number of open changes.

use super::catchup_reads::counted;
use super::common::*;

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

/// Claim: `catchup`, in text and JSON, runs as many Git processes for one
/// open change as for four, each with its own worktree. Rejects a listing
/// that builds each open change's status report with its own Git reads,
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
