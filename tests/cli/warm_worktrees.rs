use crate::common::*;

fn configured(command: &str, timeout: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/worktrees.toml"),
        format!("[prepare]\ncommand = {command:?}\ntimeout = {timeout:?}\n"),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/worktrees.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: declare worktree preparation"],
    );
    repo
}

fn begin(repo: &Repo, slug: &str) -> (String, PathBuf) {
    let output = repo.arc(&repo.root).args(["begin", slug]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let worktree = stdout
        .lines()
        .find_map(|line| line.strip_prefix("worktree: "))
        .unwrap();
    (
        String::from_utf8(output.stderr).unwrap(),
        PathBuf::from(worktree),
    )
}

/// A hook receives the current source identity and isolated build storage;
/// creating a checkout without running its declared preparation is rejected.
#[test]
fn begin_prepares_source_bound_storage_without_creating_gate_evidence() {
    let repo = configured(
        "printf '%s\\n' \"$ARC_WORKTREE_PATH\" \"$ARC_WORKTREE_REVISION\" \"$ARC_WORKTREE_TREE\" \"$ARC_WORKTREE_CACHE\" \"$ARC_WORKTREE_BUILD\" \"${CARGO_TARGET_DIR-unset}\" > \"$HOME/prepared\"",
        "10s",
    );
    let (stderr, worktree) = begin(&repo, "warm");
    assert!(
        repo.home.join("prepared").exists(),
        "declared preparation did not run"
    );
    let receipt = fs::read_to_string(repo.home.join("prepared")).unwrap();
    let fields: Vec<_> = receipt.lines().collect();
    assert_eq!(fields[0], worktree.to_str().unwrap());
    assert_eq!(fields[1], repo.head(&worktree));
    assert_eq!(fields[2], git_out(&worktree, &["rev-parse", "HEAD^{tree}"]));
    assert!(Path::new(fields[3]).join("owner").is_file());
    assert!(Path::new(fields[4]).join("source").is_file());
    assert_ne!(fields[3], fields[4]);
    assert_eq!(fields[5], "unset");
    assert!(
        stderr.contains("worktree preparation: prepared"),
        "{stderr}"
    );
    let state = json_stdout(repo.arc(&worktree).args(["show", "--json"]));
    assert_eq!(state["verifications"].as_array().unwrap().len(), 0);
    assert_eq!(git_out(&worktree, &["status", "--porcelain"]), "");
}

/// Concurrent checkouts may share compiler-cache data, but only one hook
/// owns the lease; a busy cache gives the other checkout a cold start.
#[test]
fn busy_cache_lease_falls_back_cold_without_running_the_hook() {
    use std::os::fd::AsRawFd;

    let repo = configured(
        "printf '%s' \"$ARC_WORKTREE_CACHE\" > \"$HOME/cache\"; printf run >> \"$HOME/runs\"",
        "5s",
    );
    begin(&repo, "first");
    assert!(
        repo.home.join("cache").exists(),
        "declared preparation did not run"
    );
    let cache = fs::read_to_string(repo.home.join("cache")).unwrap();
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(Path::new(&cache).join("lease"))
        .unwrap();
    // SAFETY: this fixture owns the open lease descriptor until it is dropped.
    assert_eq!(
        unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let (stderr, _) = begin(&repo, "second");
    assert!(
        stderr.contains("preparation cache lease is busy"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(repo.home.join("runs")).unwrap(), "run");
    drop(lease);
    begin(&repo, "third");
    assert_eq!(
        fs::read_to_string(repo.home.join("runs")).unwrap(),
        "runrun"
    );
}

/// Gates and their identity probes consume the prepared source's storage;
/// repeated gates reuse preparation, and a source commit prepares anew.
#[test]
fn gates_share_preparation_storage_and_new_source_prepares_again() {
    let repo = configured(
        "printf '%s' \"$ARC_WORKTREE_TREE\" > \"$ARC_WORKTREE_BUILD/prepared-tree\"; printf run >> \"$HOME/runs\"",
        "5s",
    );
    let command = "test \"$(cat \"$ARC_WORKTREE_BUILD/prepared-tree\")\" = \"$ARC_WORKTREE_TREE\"";
    let probe = "printf '%s' \"$ARC_WORKTREE_TREE\"";
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!("[gates.source]\ncommand = {command:?}\nenvironment = {probe:?}\n"),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: gate prepared source"]);
    let (_, worktree) = begin(&repo, "round-trip");
    repo.arc(&worktree).args(["snapshot"]).assert().success();
    for _ in 0..2 {
        repo.arc(&worktree)
            .args(["verify", "--all"])
            .assert()
            .success()
            .stdout(predicates::str::contains("gates: 1/1 pass"));
    }
    assert_eq!(fs::read_to_string(repo.home.join("runs")).unwrap(), "run");
    repo.commit(
        &worktree,
        "README.md",
        "next source\n",
        "test: move source tree",
    );
    repo.arc(&worktree).args(["snapshot"]).assert().success();
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gates: 1/1 pass"));
    assert_eq!(
        fs::read_to_string(repo.home.join("runs")).unwrap(),
        "runrun"
    );
}

/// A failed optimization must leave a usable cold checkout, including when
/// a descendant keeps its parent's output pipe open beyond the deadline.
#[test]
fn failed_and_timed_out_preparation_continue_cold_and_stop_owned_children() {
    let repo = configured("printf unavailable >&2; exit 7", "5s");
    let (stderr, worktree) = begin(&repo, "failed");
    assert!(worktree.is_dir());
    assert!(stderr.contains("continuing cold"), "{stderr}");
    assert!(stderr.contains("unavailable"), "{stderr}");

    fs::write(
        repo.root.join(".arc/worktrees.toml"),
        "[prepare]\ncommand = '(sleep 2; printf leaked > \"$HOME/leaked\") & exit 0'\ntimeout = '1s'\n",
    ).unwrap();
    git(&repo.root, &["add", ".arc/worktrees.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: bound preparation descendants"],
    );
    let started = Instant::now();
    let (stderr, _) = begin(&repo, "timeout");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(stderr.contains("preparation exceeded 1s"), "{stderr}");
    thread::sleep(Duration::from_millis(1250));
    assert!(
        !repo.home.join("leaked").exists(),
        "preparation child survived its lease"
    );
}

/// A failed seed can leave partial outputs for inspection, but subsequent
/// gates must build in fresh storage rather than execute those outputs.
#[test]
fn failed_preparation_outputs_are_not_reused_by_gates() {
    let repo = configured(
        "printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/attempt-build\"; printf partial > \"$ARC_WORKTREE_BUILD/partial\"; exit 7",
        "5s",
    );
    let command = "test ! -f \"$ARC_WORKTREE_BUILD/partial\" && mkdir -p \"$ARC_WORKTREE_BUILD\" && printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/gate-build\"";
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!("[gates.cold]\ncommand = {command:?}\n"),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: gate cold source"]);
    let (_, worktree) = begin(&repo, "cold-output");
    repo.arc(&worktree).args(["snapshot"]).assert().success();
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gates: 1/1 pass"));
    let prepared = fs::read_to_string(repo.home.join("attempt-build")).unwrap();
    let cold = fs::read_to_string(repo.home.join("gate-build")).unwrap();
    assert_ne!(prepared, cold);
    assert!(Path::new(&prepared).join("partial").exists());
    assert!(!Path::new(&cold).join("partial").exists());
}

/// Preparation may seed isolated build storage, but cannot replace tracked
/// source, hide it behind an index flag, or inject a new source executable.
#[test]
fn preparation_refuses_source_index_and_untracked_path_mutations() {
    for (slug, command) in [
        (
            "tracked",
            "printf changed > \"$ARC_WORKTREE_PATH/README.md\"",
        ),
        (
            "index",
            "git -C \"$ARC_WORKTREE_PATH\" update-index --assume-unchanged README.md",
        ),
        (
            "head",
            "git -C \"$ARC_WORKTREE_PATH\" commit --allow-empty -m preparation",
        ),
        (
            "untracked",
            "printf injected > \"$ARC_WORKTREE_PATH/test-program\"",
        ),
    ] {
        let repo = configured(command, "5s");
        repo.arc(&repo.root)
            .args(["begin", slug])
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "worktree preparation changed source, index, HEAD or untracked source paths",
            ));
    }
}

/// Two checkouts of one Cargo package must execute their own source after
/// preparation; a complete shared target directory cannot stand in for it.
#[test]
fn same_package_worktrees_build_and_execute_distinct_source() {
    let command = "cargo build --offline --locked --quiet --manifest-path \"$ARC_WORKTREE_PATH/Cargo.toml\" --target-dir \"$ARC_WORKTREE_BUILD/target\" && \"$ARC_WORKTREE_BUILD/target/debug/warm-source\" >> \"$HOME/built-sources\" && printf '%s\\n' \"$ARC_WORKTREE_BUILD\" >> \"$HOME/build-paths\"";
    let repo = configured(command, "30s");
    fs::create_dir_all(repo.root.join("src")).unwrap();
    fs::write(
        repo.root.join("Cargo.toml"),
        "[package]\nname='warm-source'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(
        repo.root.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = 'warm-source'\nversion = '0.1.0'\n",
    )
    .unwrap();
    fs::write(
        repo.root.join("src/main.rs"),
        "fn main() { println!(\"alpha\"); }\n",
    )
    .unwrap();
    git(&repo.root, &["add", "Cargo.toml", "Cargo.lock", "src"]);
    git(&repo.root, &["commit", "-m", "test: first package source"]);
    let (first, _) = begin(&repo, "alpha");
    assert!(
        repo.home.join("built-sources").exists(),
        "declared preparation did not build the first source: {first}"
    );
    repo.commit(
        &repo.root,
        "src/main.rs",
        "fn main() { println!(\"beta\"); }\n",
        "test: second package source",
    );
    let (second, _) = begin(&repo, "beta");
    assert!(
        second.contains("worktree preparation: prepared"),
        "{second}"
    );
    assert_eq!(
        fs::read_to_string(repo.home.join("built-sources")).unwrap(),
        "alpha\nbeta\n"
    );
    let builds = fs::read_to_string(repo.home.join("build-paths")).unwrap();
    let builds: Vec<_> = builds.lines().collect();
    assert_eq!(builds.len(), 2);
    assert_ne!(builds[0], builds[1]);
}
