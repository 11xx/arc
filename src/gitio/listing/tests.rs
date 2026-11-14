//! Each batched answer is the one the read it stands in for gives on its
//! own: `merge-tree`, `merge-base --is-ancestor`, `rev-list --reverse` and
//! `status --porcelain`, run per question.

use super::*;
use crate::gitio::{self, git};

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    /// A repository whose `master` holds `shared.txt`.
    fn new() -> Fixture {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        for args in [
            &["init", "-q", "-b", "master"][..],
            &["config", "user.name", "Tester"],
            &["config", "user.email", "tester@example.invalid"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(&root, args).unwrap();
        }
        let fixture = Fixture { _tmp: tmp, root };
        fixture.commit("shared.txt", "base\n");
        fixture
    }

    fn commit(&self, file: &str, text: &str) -> String {
        std::fs::write(self.root.join(file), text).unwrap();
        git(&self.root, &["add", "."]).unwrap();
        git(&self.root, &["commit", "-q", "-m", &format!("edit {file}")]).unwrap();
        gitio::head(&self.root).unwrap()
    }

    fn switch(&self, args: &[&str]) {
        let mut command = vec!["switch", "-q"];
        command.extend_from_slice(args);
        git(&self.root, &command).unwrap();
    }

    fn merge(&self, branch: &str) -> String {
        git(&self.root, &["merge", "-q", "--no-ff", "--no-edit", branch]).unwrap();
        gitio::head(&self.root).unwrap()
    }
}

/// A `master` that moved on, and branches cut from its first commit: one
/// that merges cleanly, one that conflicts, one that holds a merge, and one
/// `master` already contains.
struct History {
    fixture: Fixture,
    master: String,
    clean: String,
    conflicting: String,
    merged: String,
    contained: String,
}

fn history() -> History {
    let fixture = Fixture::new();
    let first = gitio::head(&fixture.root).unwrap();
    fixture.switch(&["-c", "contained"]);
    let contained = fixture.commit("contained.txt", "contained\n");
    fixture.switch(&["master"]);
    fixture.merge("contained");
    let master = fixture.commit("shared.txt", "master\n");
    fixture.switch(&["-c", "clean", &first]);
    fixture.commit("clean-1.txt", "one\n");
    let clean = fixture.commit("clean-2.txt", "two\n");
    fixture.switch(&["-c", "conflicting", &first]);
    let conflicting = fixture.commit("shared.txt", "conflicting\n");
    fixture.switch(&["-c", "side", &first]);
    fixture.commit("side.txt", "side\n");
    fixture.switch(&["-c", "merged", &first]);
    fixture.commit("merged.txt", "merged\n");
    let merged = fixture.merge("side");
    fixture.switch(&["master"]);
    History {
        fixture,
        master,
        clean,
        conflicting,
        merged,
        contained,
    }
}

impl History {
    fn heads(&self) -> [&str; 5] {
        [
            &self.clean,
            &self.conflicting,
            &self.merged,
            &self.contained,
            &self.master,
        ]
        .map(String::as_str)
    }
}

/// Rejects a batch that reads a conflicted merge's entries as the next
/// merge's status, or names a tree for a conflict.
#[test]
fn batched_merge_outcomes_are_each_merge_tree_answer() {
    let history = history();
    let root = &history.fixture.root;
    let merges: Vec<Pair> = history
        .heads()
        .into_iter()
        .map(|head| pair(&history.master, head))
        .collect();
    let batched = merge_outcomes(root, merges.iter());
    assert_eq!(batched.len(), merges.len());
    for merge in &merges {
        let alone = gitio::merge_outcome(root, &merge.0, &merge.1).unwrap();
        assert_eq!(batched[merge], alone, "{merge:?}");
    }
    assert!(batched[&pair(&history.master, &history.conflicting)].conflicts);
}

/// Rejects ancestry read from the walk's boundary rather than from what
/// each head reaches, or a contained head taken for one the target lacks.
#[test]
fn walked_ancestry_is_each_merge_base_answer() {
    let history = history();
    let root = &history.fixture.root;
    let heads: BTreeSet<&str> = history.heads().into_iter().collect();
    let walk = Walk::from(root, &history.master, &heads).unwrap();
    for head in heads {
        let alone = (
            gitio::is_ancestor(root, &history.master, head).unwrap(),
            gitio::is_ancestor(root, head, &history.master).unwrap(),
        );
        assert_eq!(walk.ancestry(head), alone, "{head}");
    }
    let behind = gitio::rev_parse(root, "master~1").unwrap();
    let walk = Walk::from(root, &behind, &BTreeSet::from([history.master.as_str()])).unwrap();
    assert_eq!(walk.ancestry(&history.master), (true, false));
}

/// Rejects a range ordered other than `rev-list --reverse` orders it, and a
/// range holding a merge answered from the walk instead of left to Git.
#[test]
fn a_walked_range_is_rev_list_or_left_to_git() {
    let history = history();
    let root = &history.fixture.root;
    let heads: BTreeSet<&str> = history.heads().into_iter().collect();
    let walk = Walk::from(root, &history.master, &heads).unwrap();
    for head in [&history.clean, &history.conflicting, &history.contained] {
        let listed = git(
            root,
            &[
                "rev-list",
                "--reverse",
                &format!("{}..{head}", history.master),
            ],
        )
        .unwrap();
        let listed: Vec<String> = listed.lines().map(str::to_string).collect();
        assert_eq!(walk.range(head), Some(listed), "{head}");
    }
    assert_eq!(walk.range(&history.contained), Some(Vec::new()));
    assert_eq!(walk.range(&history.merged), None);
}

/// Rejects cleanliness attributed to the wrong worktree, untracked or
/// modified files read as clean, and an answer for any worktree once one of
/// them cannot be read.
#[test]
fn batched_cleanliness_is_each_status_answer() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    let worktrees: Vec<PathBuf> = ["clean", "untracked", "modified"]
        .into_iter()
        .map(|name| {
            let path = fixture._tmp.path().join(name);
            git(
                root,
                &["worktree", "add", "-q", "-b", name, path.to_str().unwrap()],
            )
            .unwrap();
            path
        })
        .collect();
    std::fs::write(worktrees[1].join("new.txt"), "new\n").unwrap();
    std::fs::write(worktrees[2].join("shared.txt"), "edited\n").unwrap();
    let batched = cleanliness(root, &worktrees);
    assert_eq!(batched.len(), worktrees.len());
    for worktree in &worktrees {
        assert_eq!(
            batched[worktree],
            gitio::is_clean(worktree).unwrap(),
            "{}",
            worktree.display()
        );
    }
    assert!(batched[&worktrees[0]]);

    let mut unreadable = worktrees.clone();
    unreadable.insert(1, fixture._tmp.path().join("gone"));
    assert!(cleanliness(root, &unreadable).is_empty());
}
