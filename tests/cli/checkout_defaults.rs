use super::common::{git, git_out, stdout, Repo};
use std::fs;

fn user(repo: &Repo, text: &str) {
    let dir = repo.home.join(".local/ai/arc");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), text).unwrap();
}

#[test]
fn begin_checkout_defaults_from_each_source() {
    for (source, mode) in [
        ("builtin", "in-place"),
        ("user", "in-place"),
        ("user", "worktree"),
        ("project", "in-place"),
        ("project", "worktree"),
        ("operator", "in-place"),
        ("operator", "worktree"),
    ] {
        let repo = Repo::new();
        user(&repo, "");
        let table = if source == "user" {
            "change"
        } else {
            "defaults.change"
        };
        let text = format!("[{table}]\ncheckout='{mode}'\nbranch_prefix='topic-'\n");
        match source {
            "user" => user(&repo, &text),
            "project" => {
                // The invoking checkout differs from the target's committed defaults.
                fs::create_dir_all(repo.root.join(".arc")).unwrap();
                fs::write(repo.root.join(".arc/policy.toml"), &text).unwrap();
                git(&repo.root, &["add", ".arc/policy.toml"]);
                git(&repo.root, &["commit", "-m", "test: project default"]);
                git(&repo.root, &["checkout", "-b", "invoking"]);
                fs::write(
                    repo.root.join(".arc/policy.toml"),
                    "[defaults.change]\ncheckout='worktree'\nbranch_prefix='wrong/'\n",
                )
                .unwrap();
                git(&repo.root, &["add", ".arc/policy.toml"]);
                git(&repo.root, &["commit", "-m", "test: invoking default"]);
            }
            "operator" => {
                fs::create_dir_all(repo.root.join(".git/arc")).unwrap();
                fs::write(repo.root.join(".git/arc/operator-policy.toml"), &text).unwrap();
            }
            _ => {}
        }
        stdout(
            repo.arc(&repo.root)
                .args(["begin", "checkout", "--target", "master"]),
        );
        let branch = if source == "builtin" {
            "arc/checkout"
        } else {
            "topic-checkout"
        };
        assert_eq!(
            git_out(&repo.root, &["branch", "--show-current"]),
            if mode == "in-place" {
                branch
            } else if source == "project" {
                "invoking"
            } else {
                "master"
            },
            "{source}:{mode}"
        );
        let worktree = repo.home.join(".worktrees/repo-checkout");
        assert_eq!(worktree.exists(), mode == "worktree", "{source}:{mode}");
        if mode == "worktree" {
            assert_eq!(git_out(&worktree, &["branch", "--show-current"]), branch);
        }
    }
}

#[test]
fn begin_worktree_override_with_and_without_path() {
    for explicit in [false, true] {
        let repo = Repo::new();
        user(&repo, "[change]\ncheckout='in-place'\n");
        let path = if explicit {
            repo.home.join("chosen")
        } else {
            repo.home.join(".worktrees/repo-forced")
        };
        let mut cmd = repo.arc(&repo.root);
        cmd.args(["begin", "forced", "--worktree"]);
        if explicit {
            cmd.arg(&path);
        }
        stdout(&mut cmd);
        assert!(path.exists());
        assert_eq!(git_out(&repo.root, &["branch", "--show-current"]), "master");
    }
}

#[test]
fn begin_branch_prefix_and_explicit_branch() {
    for explicit in [false, true] {
        let repo = Repo::new();
        user(
            &repo,
            "[change]\ncheckout='in-place'\nbranch_prefix='topic-'\n",
        );
        let mut cmd = repo.arc(&repo.root);
        cmd.args(["begin", "prefix"]);
        if explicit {
            cmd.args(["--branch", "chosen"]);
        }
        stdout(&mut cmd);
        assert_eq!(
            git_out(&repo.root, &["branch", "--show-current"]),
            if explicit { "chosen" } else { "topic-prefix" }
        );
    }
}

#[test]
fn begin_adopt_keeps_existing_checkout_semantics() {
    for (mode, explicit) in [("in-place", false), ("worktree", false), ("in-place", true)] {
        let repo = Repo::new();
        user(&repo, &format!("[change]\ncheckout='{mode}'\n"));
        git(&repo.root, &["branch", "existing"]);
        let mut cmd = repo.arc(&repo.root);
        cmd.args(["begin", "adopted", "--adopt", "existing"]);
        if explicit {
            cmd.arg("--no-worktree");
        }
        stdout(&mut cmd);
        assert_eq!(
            git_out(&repo.root, &["branch", "--show-current"]),
            if explicit { "existing" } else { "master" }
        );
        assert!(!repo.home.join(".worktrees/repo-adopted").exists());
    }
}

#[test]
fn begin_checkout_flags_conflict() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "conflict", "--worktree", "--no-worktree"])
        .assert()
        .failure();
    assert_eq!(git_out(&repo.root, &["branch", "--show-current"]), "master");
    assert!(git_out(&repo.root, &["branch", "--list", "arc/conflict"]).is_empty());
}

#[test]
fn recorded_branch_is_not_guessed_as_target_after_prefix_change() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "recorded", "--branch", "chosen", "--no-worktree"]),
    );
    user(&repo, "[change]\nbranch_prefix='changed/'\n");
    let detached = repo.home.join("detached");
    git(
        &repo.root,
        &["worktree", "add", "--detach", detached.to_str().unwrap()],
    );
    let fork = repo
        .arc(&detached)
        .args(["fork", "begin", "probe"])
        .output()
        .unwrap();
    assert!(
        !fork.status.success(),
        "recorded change branch must not become the guessed target"
    );
    assert!(
        String::from_utf8_lossy(&fork.stderr).contains("no integration branch is discoverable"),
        "{}",
        String::from_utf8_lossy(&fork.stderr)
    );
}

#[test]
fn builtin_checkout_is_independent_of_profile() {
    for profile in ["direct", "local", "forge", "release"] {
        let repo = Repo::new();
        user(&repo, "");
        stdout(
            repo.arc(&repo.root)
                .args(["begin", "profile", "--profile", profile]),
        );
        assert_eq!(
            git_out(&repo.root, &["branch", "--show-current"]),
            "arc/profile"
        );
        assert!(!repo.home.join(".worktrees/repo-profile").exists());
    }
}
