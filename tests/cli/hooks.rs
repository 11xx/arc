use crate::common::*;

/// The directory holding the freshly-built test binary, so an installed hook
/// script's bare `arc` resolves to it rather than any globally installed arc.
fn arc_bin_dir() -> PathBuf {
    let bin = std::env::var_os("CARGO_BIN_EXE_arc").expect("cargo provides the arc binary");
    PathBuf::from(bin).parent().unwrap().to_path_buf()
}

/// Commit through real Git so installed hooks fire, with the test binary on
/// PATH. Returns combined stdout+stderr for asserting hook output.
fn commit_firing_hooks(repo: &Repo, cwd: &Path, file: &str, content: &str, msg: &str) -> String {
    fs::write(cwd.join(file), content).unwrap();
    git(cwd, &["add", "."]);
    let path = format!(
        "{}:{}",
        arc_bin_dir().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("git")
        .args(["commit", "-m", msg])
        .current_dir(cwd)
        .env("PATH", path)
        .env("HOME", &repo.home)
        .env("ARC_ACTOR", "tester")
        .env("ARC_HARNESS", "test")
        .env("ARC_SESSION", "session-a")
        .env_remove("ARC_DATA_ROOT")
        .env_remove("ARC_DATA_DIR")
        .env_remove("AI_HOME")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn install_writes_both_hooks_and_status_reports_them() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["hooks", "status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("post-commit: absent"));

    repo.arc(&repo.root)
        .args(["hooks", "install"])
        .assert()
        .success();

    let status = stdout(repo.arc(&repo.root).args(["hooks", "status"]));
    assert!(status.contains("post-commit: arc-managed"), "{status}");
    assert!(
        status.contains("prepare-commit-msg: arc-managed"),
        "{status}"
    );
}

#[test]
fn install_refuses_foreign_hook_without_force() {
    let repo = Repo::new();
    let hooks = repo.root.join(".git/hooks");
    fs::create_dir_all(&hooks).unwrap();
    fs::write(hooks.join("post-commit"), "#!/bin/sh\necho mine\n").unwrap();

    repo.arc(&repo.root)
        .args(["hooks", "install"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not arc-managed"));

    // --force replaces it and preserves the original as .pre-arc.
    repo.arc(&repo.root)
        .args(["hooks", "install", "--force"])
        .assert()
        .success();
    assert!(hooks.join("post-commit.pre-arc").is_file());
}

#[test]
fn commit_on_change_branch_gains_the_trailer() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["hooks", "install"])
        .assert()
        .success();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "feat-x", "--no-worktree"]),
    );
    git(&repo.root, &["checkout", "arc/feat-x"]);

    commit_firing_hooks(&repo, &repo.root, "feat-x.txt", "work\n", "feat: work");

    let msg = git_out(&repo.root, &["log", "-1", "--format=%B"]);
    let change_id = stdout(
        repo.arc(&repo.root)
            .args(["status", "feat-x", "--get", "change_id"]),
    );
    assert!(
        msg.contains(&format!("Arc-Change: {}", change_id.trim())),
        "commit message missing trailer:\n{msg}"
    );
}

#[test]
fn post_commit_warns_when_a_commit_staled_an_approval() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["hooks", "install"])
        .assert()
        .success();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "feat-x", "--no-worktree"]),
    );
    git(&repo.root, &["checkout", "arc/feat-x"]);
    commit_firing_hooks(&repo, &repo.root, "feat-x.txt", "work\n", "feat: work");
    repo.arc(&repo.root)
        .args(["snapshot", "feat-x"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();

    // The next commit moves the head past the approved snapshot.
    let output = commit_firing_hooks(&repo, &repo.root, "feat-x.txt", "more\n", "feat: more");
    assert!(
        output.contains("approval on feat-x") && output.contains("is now stale"),
        "post-commit did not warn:\n{output}"
    );
}

#[test]
fn query_commit_finds_change_by_patchset_head() {
    let repo = Repo::new();
    let (_id, wt, head) = change_with_patchset(&repo, "feat-x");
    let _ = wt;

    let out = stdout(
        repo.arc(&repo.root)
            .args(["query", "--commit", &head[..12]]),
    );
    assert!(out.contains("feat-x"), "{out}");
}
