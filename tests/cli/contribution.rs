//! Contribution mode: the receiver owns the history shape and the merge, so
//! `integrate` records a change ready to send instead of merging it.

use crate::common::*;

fn contribution_repo(history: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.smoke]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare gates"]);
    let policy = repo.home.join("operator-policy.toml");
    fs::write(
        &policy,
        format!("[contribution]\nhistory = \"{history}\"\n"),
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["policy", "write", "--body-file", policy.to_str().unwrap()])
        .assert()
        .success();
    repo
}

fn commit_file(worktree: &Path, file: &str, content: &str) {
    fs::write(worktree.join(file), content).unwrap();
    git(worktree, &["add", "--", file]);
    git(worktree, &["commit", "-m", &format!("feat: {file}")]);
}

fn gate_and_approve(repo: &Repo, worktree: &Path, slug: &str) {
    stdout(repo.arc(worktree).args(["snapshot", slug]));
    repo.arc(worktree)
        .args(["verify", slug, "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
}

fn open(repo: &Repo, slug: &str) -> PathBuf {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    repo.home.join(".worktrees").join(format!("repo-{slug}"))
}

#[test]
fn integrate_records_a_contribution_ready_to_send_and_merges_nothing() {
    let repo = contribution_repo("preserve");
    let worktree = open(&repo, "send-me");
    commit_file(&worktree, "one.txt", "one\n");
    commit_file(&worktree, "two.txt", "two\n");
    gate_and_approve(&repo, &worktree, "send-me");
    let target_before = git_out(&repo.root, &["rev-parse", "master"]);

    repo.arc(&repo.root)
        .args(["integrate", "send-me", "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains("would record"))
        .stdout(predicates::str::contains("nothing would be merged"));
    repo.arc(&repo.root)
        .args(["integrate", "send-me"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ready to send"));

    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), target_before);
    let head = git_out(&worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "send-me", "--json"]));
    assert_eq!(status["ready_to_send"]["head"], head.as_str(), "{status}");
    assert_eq!(status["next_action"], "await_receiver", "{status}");
    assert!(status["closure"].is_null(), "{status}");
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 4, "{config}");

    // A second run at the same head records nothing new.
    repo.arc(&repo.root)
        .args(["integrate", "send-me"])
        .assert()
        .success()
        .stdout(predicates::str::contains("already ready to send"));
}

#[test]
fn squash_history_needs_one_commit_and_squash_makes_it_a_new_patchset() {
    let repo = contribution_repo("squash");
    let worktree = open(&repo, "squash-me");
    commit_file(&worktree, "a.txt", "a\n");
    commit_file(&worktree, "b.txt", "b\n");
    gate_and_approve(&repo, &worktree, "squash-me");
    let tree = git_out(&worktree, &["rev-parse", "HEAD^{tree}"]);

    repo.arc(&repo.root)
        .args(["integrate", "squash-me"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("arc squash"));

    repo.arc(&worktree)
        .args(["squash", "squash-me", "-m", "feat: a and b"])
        .assert()
        .success();
    assert_eq!(git_out(&worktree, &["rev-parse", "HEAD^{tree}"]), tree);
    assert_eq!(
        git_out(&worktree, &["rev-list", "--count", "master..HEAD"]).trim(),
        "1"
    );
    let status = json_stdout(repo.arc(&repo.root).args(["status", "squash-me", "--json"]));
    assert_eq!(status["latest_patchset"]["id"], "ps-02", "{status}");
    assert_eq!(status["has_valid_approval"], false, "{status}");

    repo.arc(&worktree)
        .args(["verify", "squash-me", "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "squash-me", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "squash-me"])
        .assert()
        .success()
        .stdout(predicates::str::contains("squash history"));
}

#[test]
fn a_merge_commit_in_contributed_history_is_refused() {
    let repo = contribution_repo("preserve");
    let worktree = open(&repo, "merged-in");
    commit_file(&worktree, "side.txt", "side\n");
    git(&worktree, &["checkout", "-q", "-b", "side-work", "HEAD~1"]);
    commit_file(&worktree, "other.txt", "other\n");
    git(&worktree, &["checkout", "-q", "arc/merged-in"]);
    git(
        &worktree,
        &[
            "merge",
            "--no-ff",
            "-q",
            "-m",
            "merge side work",
            "side-work",
        ],
    );
    gate_and_approve(&repo, &worktree, "merged-in");

    repo.arc(&repo.root)
        .args(["integrate", "merged-in"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("merge commit"));
}

#[test]
fn audit_squash_restores_tracked_state_after_a_hook_changes_it() {
    use std::os::unix::fs::PermissionsExt;
    for hook_exit in [0, 1] {
        let repo = contribution_repo("squash");
        let worktree = open(&repo, "hook-failure");
        commit_file(&worktree, "a.txt", "original\n");
        commit_file(&worktree, "b.txt", "second\n");
        gate_and_approve(&repo, &worktree, "hook-failure");
        let original_head = repo.head(&worktree);
        let original_tree = git_out(&worktree, &["write-tree"]);
        let hook = repo.root.join(".git/hooks/pre-commit");
        fs::write(&hook, format!(
            "#!/bin/sh\nprintf 'hook edit\\n' > a.txt\ngit add a.txt\nprintf 'retained\\n' > hook-output.txt\nexit {hook_exit}\n"
        )).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        repo.arc(&worktree)
            .args(["squash", "-m", "feat: combine"])
            .assert()
            .failure();
        assert_eq!(repo.head(&worktree), original_head);
        assert_eq!(git_out(&worktree, &["write-tree"]), original_tree);
        assert_eq!(
            fs::read_to_string(worktree.join("a.txt")).unwrap(),
            "original\n"
        );
        assert_eq!(
            fs::read_to_string(worktree.join("hook-output.txt")).unwrap(),
            "retained\n"
        );
        assert_eq!(
            git_out(
                &worktree,
                &["status", "--porcelain", "--untracked-files=no"]
            ),
            ""
        );
        let status = json_stdout(repo.arc(&worktree).args(["status", "--json"]));
        assert_eq!(status["latest_patchset"]["id"], "ps-01", "{status}");
    }
}

#[test]
fn squash_preserves_untracked_content_obstructing_a_tracked_path() {
    use std::os::unix::fs::PermissionsExt;
    for hook_exit in [0, 1] {
        for replace_parent in [false, true] {
            let repo = contribution_repo("squash");
            let worktree = open(&repo, "obstructed");
            fs::create_dir(worktree.join("dir")).unwrap();
            commit_file(&worktree, "dir/a.txt", "original\n");
            commit_file(&worktree, "b.txt", "second\n");
            gate_and_approve(&repo, &worktree, "obstructed");
            let original_head = repo.head(&worktree);
            let original_tree = git_out(&worktree, &["write-tree"]);
            let hook = repo.root.join(".git/hooks/pre-commit");
            let mutation = if replace_parent {
                "rm -r dir\nprintf 'preserved\\n' > dir"
            } else {
                "rm dir/a.txt\nmkdir dir/a.txt\nprintf 'preserved\\n' > dir/a.txt/output.txt"
            };
            fs::write(&hook, format!("#!/bin/sh\n{mutation}\nexit {hook_exit}\n")).unwrap();
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
            let result = repo
                .arc(&worktree)
                .args(["squash", "-m", "feat: combine"])
                .output()
                .unwrap();
            assert!(!result.status.success());
            let recoveries = repo.root.join(".git/arc/squash-recovery");
            assert!(
                recoveries.is_dir(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let saved = fs::read_dir(recoveries)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let output = saved.join(if replace_parent {
                "dir"
            } else {
                "dir/a.txt/output.txt"
            });
            assert_eq!(fs::read_to_string(output).unwrap(), "preserved\n");
            assert!(String::from_utf8_lossy(&result.stderr).contains(saved.to_str().unwrap()));
            assert_eq!(repo.head(&worktree), original_head);
            assert_eq!(git_out(&worktree, &["write-tree"]), original_tree);
            assert_eq!(
                fs::read_to_string(worktree.join("dir/a.txt")).unwrap(),
                "original\n"
            );
            let status = json_stdout(repo.arc(&worktree).args(["status", "--json"]));
            assert_eq!(status["latest_patchset"]["id"], "ps-01");
        }
    }
}
