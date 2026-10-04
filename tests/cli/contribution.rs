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
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    for hook_exit in [0, 1] {
        for replace_parent in [false, true] {
            let repo = contribution_repo("squash");
            let worktree = open(&repo, "obstructed");
            fs::create_dir(worktree.join("dir")).unwrap();
            commit_file(&worktree, "dir/a.txt", "original\n");
            commit_file(&worktree, "b.txt", "second\n");
            let byte_name = std::ffi::OsStr::from_bytes(b"byte-\xff.txt");
            fs::write(worktree.join(byte_name), "byte-named content\n").unwrap();
            git(&worktree, &["add", "--", "."]);
            git(&worktree, &["commit", "-m", "test: retain byte-named path"]);
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
            assert_eq!(
                fs::read_to_string(worktree.join(byte_name)).unwrap(),
                "byte-named content\n"
            );
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

#[test]
fn change_roles_assert_withdraw_and_validate() {
    let repo = Repo::new();
    open(&repo, "roles");
    repo.arc(&repo.root)
        .args([
            "roles",
            "roles",
            "--planned-by",
            "lead:planner#high",
            "--planned-by",
            "Person <p@example.invalid>",
            "--orchestrated-by",
            "lead:coordinator",
        ])
        .assert()
        .success();
    let events = json_stdout(repo.arc(&repo.root).args(["show", "roles", "--json"]));
    assert_eq!(
        events["contribution_roles"]["Planned-by"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let log = stdout(repo.arc(&repo.root).args(["log", "roles"]));
    assert_eq!(log.matches("change-roles-asserted").count(), 1);
    let block = stdout(repo.arc(&repo.root).args(["trailers", "roles"]));
    assert!(block.contains("Planned-by: lead:planner#high"));
    assert!(block.contains("Planned-by: Person <p@example.invalid>"));
    assert!(block.contains("Orchestrated-by: lead:coordinator"));
    repo.arc(&repo.root)
        .args([
            "roles",
            "roles",
            "--withdraw",
            "Planned-by=lead:planner#high",
        ])
        .assert()
        .success();
    let block = stdout(repo.arc(&repo.root).args(["trailers", "roles"]));
    assert!(!block.contains("Planned-by: lead:planner#high"));
    repo.arc(&repo.root)
        .args(["roles", "roles", "--planned-by", "unattributed"])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["roles", "roles", "--withdraw", "Reviewed-by=lead:reviewer"])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["roles", "roles"])
        .assert()
        .failure();
}

#[test]
fn change_trailers_order_declared_snapshots_and_unresolved_notes() {
    let repo = Repo::new();
    let wt = open(&repo, "credit");
    stdout(repo.arc(&wt).args([
        "roles",
        "credit",
        "--orchestrated-by",
        "lead:coordinator",
        "--planned-by",
        "lead:planner",
    ]));
    commit_file(&wt, "one", "one");
    stdout(
        repo.arc(&wt)
            .args(["snapshot", "credit"])
            .env("ARC_MODEL", "worker#medium"),
    );
    commit_file(&wt, "two", "two");
    stdout(repo.arc(&wt).args(["snapshot", "credit"]));
    commit_file(&wt, "three", "three");
    stdout(
        repo.arc(&wt)
            .args(["snapshot", "credit"])
            .env_remove("ARC_ACTOR")
            .env("ARC_MODEL", "unclaimed#high"),
    );
    commit_file(&wt, "four", "four");
    stdout(
        repo.arc(&wt)
            .args(["snapshot", "credit"])
            .env_remove("ARC_HARNESS")
            .env("ARC_MODEL", "unknown-harness#low"),
    );
    let result = json_stdout(repo.arc(&wt).args(["trailers", "credit", "--json"]));
    assert_eq!(result["schema"], "arc-trailers/1");
    assert_eq!(
        result["trailers"],
        serde_json::json!([
            "Planned-by: lead:planner",
            "Implemented-by: test:worker#medium",
            "Orchestrated-by: lead:coordinator"
        ])
    );
    assert!(result["notes"].as_array().unwrap().iter().any(|n| n
        .as_str()
        .unwrap()
        .contains("ps-02")
        && n.as_str().unwrap().contains("model")));
    assert!(result["notes"].as_array().unwrap().iter().any(|n| n
        .as_str()
        .unwrap()
        .contains("review credit stays in the ledger")));
    for (patchset, coordinate) in [("ps-03", "declared identity"), ("ps-04", "harness")] {
        assert!(
            result["notes"].as_array().unwrap().iter().any(|note| {
                let note = note.as_str().unwrap();
                note.contains(patchset) && note.contains(coordinate)
            }),
            "{result}"
        );
    }
    let out = stdout(repo.arc(&wt).args(["trailers", "credit"]));
    assert!(out.find("Planned-by:").unwrap() < out.find("Implemented-by:").unwrap());
    assert!(out.find("Implemented-by:").unwrap() < out.find("Orchestrated-by:").unwrap());
    assert!(!out.contains("Reviewed-by:"));
}

#[test]
fn change_role_advisory_disappears_after_trailer_rewrite() {
    let repo = contribution_repo("preserve");
    let wt = open(&repo, "advice");
    stdout(
        repo.arc(&wt)
            .args(["roles", "advice", "--planned-by", "lead:planner"]),
    );
    commit_file(&wt, "one", "one");
    gate_and_approve(&repo, &wt, "advice");
    let block = stdout(repo.arc(&wt).args(["trailers", "advice"]));
    assert!(!block.contains("Reviewed-by:"));
    let report = json_stdout_any_status(repo.arc(&wt).args(["check", "advice", "--json"]));
    assert!(
        report["advisories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["code"] == "missing-contribution-trailers"),
        "{report}"
    );
    repo.arc(&wt)
        .args(["integrate", "advice", "--dry-run"])
        .assert()
        .success()
        .stderr(predicates::str::contains("arc rewrite trailers"));
    repo.arc(&wt)
        .args(["integrate", "advice"])
        .assert()
        .success()
        .stderr(predicates::str::contains("arc rewrite trailers"));
    stdout(repo.arc(&wt).args([
        "rewrite",
        "trailers",
        "--from",
        "HEAD",
        "--no-sign",
        "--append",
        "Planned-by: lead:planner",
    ]));
    let report = json_stdout_any_status(repo.arc(&wt).args(["check", "advice", "--json"]));
    assert!(
        !report["advisories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["code"] == "missing-contribution-trailers"),
        "{report}"
    );
}

#[test]
fn begin_and_brief_assert_change_roles() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args([
                "begin",
                "shorthand",
                "--planned-by",
                "lead:planner",
                "--orchestrated-by",
            ])
            .env("ARC_MODEL", "coordinator#high"),
    );
    let wt = repo.home.join(".worktrees/repo-shorthand");
    fs::write(repo.home.join("brief.md"), "Implement the bounded task.").unwrap();
    stdout(repo.arc(&wt).args([
        "brief",
        "shorthand",
        "--planned-by",
        "Person <p@example.invalid>",
        "--body-file",
        repo.home.join("brief.md").to_str().unwrap(),
    ]));
    let block = stdout(repo.arc(&wt).args(["trailers", "shorthand"]));
    assert!(block.contains("Planned-by: lead:planner"));
    assert!(block.contains("Planned-by: Person <p@example.invalid>"));
    assert!(block.contains("Orchestrated-by: test:coordinator#high"));
    repo.arc(&repo.root)
        .args(["begin", "invalid", "--planned-by", "bad"])
        .assert()
        .failure();
    assert!(!git_out(&repo.root, &["branch", "--list", "arc/invalid"]).contains("arc/invalid"));
    repo.arc(&repo.root)
        .args(["begin", "unresolved", "--orchestrated-by"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unresolved"));
    assert!(git_out(&repo.root, &["branch", "--list", "arc/unresolved"]).is_empty());
}

#[test]
fn change_roles_stamp_store_format_on_record_and_import() {
    let repo = Repo::new();
    open(&repo, "portable-roles");
    stdout(
        repo.arc(&repo.root)
            .args(["roles", "portable-roles", "--planned-by", "lead:planner"]),
    );
    let config = repo.root.join(".git/arc/config.json");
    let stamp: serde_json::Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    assert_eq!(stamp["schema_version"], 7, "{stamp}");
    let bundle = repo.home.join("roles.json");
    stdout(repo.arc(&repo.root).args([
        "export",
        "portable-roles",
        "--output",
        bundle.to_str().unwrap(),
    ]));
    let other = Repo::new();
    stdout(other.arc(&other.root).args(["begin", "seed"]));
    stdout(
        other
            .arc(&other.root)
            .args(["import", bundle.to_str().unwrap()]),
    );
    let stamp: serde_json::Value =
        serde_json::from_slice(&fs::read(other.root.join(".git/arc/config.json")).unwrap())
            .unwrap();
    assert_eq!(stamp["schema_version"], 7, "{stamp}");
    let block = stdout(other.arc(&other.root).args(["trailers", "portable-roles"]));
    assert_eq!(block, "Planned-by: lead:planner\n");
}
