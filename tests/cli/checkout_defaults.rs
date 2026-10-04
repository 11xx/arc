use super::common::{git, git_out, json_stdout, stdout, Repo};
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
        let state = json_stdout(repo.arc(&repo.root).args(["show", "checkout", "--json"]));
        assert_eq!(
            state["worktree_created"],
            mode == "worktree",
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
        let state = json_stdout(repo.arc(&repo.root).args(["show", "forced", "--json"]));
        assert_eq!(state["worktree_created"], true);
        let admin = git_out(&path, &["rev-parse", "--absolute-git-dir"]);
        let owner: serde_json::Value = serde_json::from_slice(
            &fs::read(std::path::Path::new(&admin).join("arc-owner")).unwrap(),
        )
        .unwrap();
        assert_eq!(owner["change_id"], state["change_id"]);
        assert_eq!(owner["token"], state["worktree_owner_token"]);
        assert!(!owner["token"].as_str().unwrap().is_empty());
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
        let state = json_stdout(repo.arc(&repo.root).args(["show", "adopted", "--json"]));
        assert_eq!(state["worktree_created"], false);
        assert!(state.get("worktree_owner_token").is_none());
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

#[test]
fn begin_configured_in_place_records_no_creation() {
    let repo = Repo::new();
    user(&repo, "[change]\ncheckout='in-place'\n");
    stdout(repo.arc(&repo.root).args(["begin", "configured"]));
    let state = json_stdout(repo.arc(&repo.root).args(["show", "configured", "--json"]));
    assert_eq!(state["worktree_created"], false);
}

#[test]
fn begin_worktree_populates_an_empty_tree() {
    let repo = Repo::new();
    git(&repo.root, &["rm", "README.md"]);
    git(&repo.root, &["commit", "-m", "test: empty tree"]);
    let id =
        crate::common::opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "empty"])));
    let worktree = repo.home.join(".worktrees/repo-empty");
    assert!(git_out(&worktree, &["status", "--porcelain"]).is_empty());
    let state = json_stdout(repo.arc(&repo.root).args(["show", &id, "--json"]));
    assert_eq!(state["worktree_created"], true);
}

#[test]
fn begin_worktree_hook_matches_native_creation() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    let hook = repo.root.join(".git/hooks/post-checkout");
    fs::write(&hook, "#!/bin/sh\nprintf '%s\\n' \"$@\" > hook-args\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let native = repo.home.join("native");
    git(
        &repo.root,
        &["worktree", "add", "--detach", native.to_str().unwrap()],
    );
    let expected = fs::read_to_string(native.join("hook-args")).unwrap();
    assert_eq!(
        expected.lines().next().unwrap(),
        "0".repeat(git_out(&repo.root, &["rev-parse", "HEAD"]).len())
    );
    stdout(repo.arc(&repo.root).args(["begin", "probe", "--worktree"]));
    assert_eq!(
        fs::read_to_string(repo.home.join(".worktrees/repo-probe/hook-args")).unwrap(),
        expected
    );
}

#[test]
fn begin_worktree_population_failure_reports_recovery() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    git(&repo.root, &["branch", "release"]);
    stdout(repo.arc(&repo.root).args(["fork", "begin", "source"]));
    let fork_checkout = repo.home.join(".worktrees/repo-fork-source");
    repo.commit(
        &fork_checkout,
        "fork-work",
        "source work",
        "feat: fork work",
    );
    let fork_head = repo.head(&fork_checkout);
    let journal = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "opening",
                "--kind",
                "todo",
                "--body-file",
                "-",
            ])
            .write_stdin("Opening context"),
    );
    let journal_file = std::path::Path::new(journal.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "dependency", "--no-worktree"]),
    );
    let base = repo.head(&repo.root);
    let dependency = json_stdout(repo.arc(&repo.root).args(["show", "dependency", "--json"]));
    let hook = repo.root.join(".git/hooks/post-checkout");
    fs::write(&hook, "#!/bin/sh\necho preserved > local-work\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let output = repo
        .arc(&repo.root)
        .args([
            "begin",
            "probe",
            "--worktree",
            "--title",
            "Repair 'quoted' scope",
            "--target",
            "release",
            "--base",
            &base,
            "--profile",
            "release",
            "--from-journal",
            &journal_file,
            "--from-fork",
            "source",
            "--dangerous",
            "--iterating",
            "--blocked-by",
            "dependency",
            "--tag",
            "repair",
            "--tag",
            "release",
            "--role",
            "lead",
            "--on-behalf-of",
            "scope owner",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    let worktree = repo.home.join(".worktrees/repo-probe");
    assert!(error.contains("retained branch arc/probe"), "{error}");
    assert!(error.contains(worktree.to_str().unwrap()), "{error}");
    assert!(error.contains("git worktree remove"), "{error}");
    assert!(error.contains("git branch -D"), "{error}");
    let discard = error
        .lines()
        .find_map(|line| line.trim().strip_prefix("discard: "))
        .unwrap();
    assert!(
        discard.ends_with("git branch -D -- 'arc/probe'"),
        "{discard}"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("local-work")).unwrap(),
        "preserved\n"
    );
    let recovery = error
        .lines()
        .find_map(|line| line.trim().strip_prefix("adopt: "))
        .expect("adopt recovery");
    stdout(&mut repo.arc_shell(&repo.root, recovery));
    let admin = git_out(&worktree, &["rev-parse", "--absolute-git-dir"]);
    assert!(!std::path::Path::new(&admin).join("arc-owner").exists());
    let state = json_stdout(repo.arc(&repo.root).args(["show", "probe", "--json"]));
    assert_eq!(state["worktree_created"], false);
    assert_eq!(state["title"], "Repair 'quoted' scope");
    assert_eq!(state["target_branch"], "release");
    assert_eq!(state["base"], base);
    assert_eq!(state["profile"], "release");
    assert_eq!(state["dangerous"], true);
    assert_eq!(state["iterating"], true);
    assert_eq!(state["journal_ref"], journal_file);
    assert_eq!(state["from_fork"]["slug"], "source");
    assert_eq!(state["from_fork"]["head"], fork_head);
    assert!(worktree.join("fork-work").is_file());
    assert_eq!(
        state["blocked_by"],
        serde_json::json!([dependency["change_id"]])
    );
    assert_eq!(state["tags"], serde_json::json!(["release", "repair"]));
    assert!(recovery.contains("--role 'lead'"), "{recovery}");
    assert!(
        recovery.contains("--on-behalf-of 'scope owner'"),
        "{recovery}"
    );
}
