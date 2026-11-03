use crate::common::*;
use std::os::fd::AsRawFd;
use std::os::unix::fs::symlink;

fn script(command: &str) -> String {
    format!("set -eu\nprintf '%s' \"$ARC_WORKTREE_BUILD_LEASE\" > \"$HOME/build-lease\"\n{command}")
}

fn lease_path(repo: &Repo) -> PathBuf {
    PathBuf::from(fs::read_to_string(repo.home.join("build-lease")).unwrap())
}

fn configured(directory: Option<&Path>, command: Option<&str>, gate: &str, probe: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    let mut preparation = "[prepare]\nmode = 'path-stable'\n".to_owned();
    if let Some(directory) = directory {
        preparation.push_str(&format!("directory = {:?}\n", directory.to_str().unwrap()));
    }
    if let Some(command) = command {
        preparation.push_str(&format!("command = {:?}\n", script(command)));
    }
    fs::write(repo.root.join(".arc/worktrees.toml"), preparation).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!(
            "[gates.stable]\ncommand = {:?}\nenvironment = {:?}\ntimeout = '10s'\n",
            script(gate),
            script(probe)
        ),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(
        &repo.root,
        &["commit", "-m", "test: configure stable build storage"],
    );
    repo
}

fn begin(repo: &Repo, slug: &str, path: &Path) {
    repo.arc(&repo.root)
        .args(["begin", slug, "--worktree", path.to_str().unwrap()])
        .assert()
        .success();
}

fn verify(repo: &Repo, path: &Path) {
    repo.arc(path).args(["snapshot"]).assert().success();
    repo.arc(path)
        .args(["verify", "--all"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gates: 1/1 pass"));
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + HANG_BOUND;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "command did not reach {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn build_path(repo: &Repo) -> PathBuf {
    PathBuf::from(fs::read_to_string(repo.home.join("build")).unwrap())
}

fn assert_busy(repo: &Repo) {
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lease_path(repo))
        .unwrap();
    // SAFETY: the fixture owns this regular file descriptor.
    let status = unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(status, -1, "another writer acquired active build storage");
    assert_eq!(
        std::io::Error::last_os_error().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

/// No-command storage stays at one path through three source trees. A tree
/// or configuration digest in that path loses the retained sentinel.
#[test]
fn stable_default_path_survives_source_and_configuration_changes_without_receipts() {
    let gate = "printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/build\"; test ! -e \"$ARC_WORKTREE_BUILD/prepared\"; if test -e \"$HOME/previous\"; then test -f \"$ARC_WORKTREE_BUILD/sentinel\"; fi; printf retained > \"$ARC_WORKTREE_BUILD/sentinel\"; printf yes > \"$HOME/previous\"; printf '%s\\n' \"$ARC_WORKTREE_BUILD\" >> \"$HOME/paths\"";
    let repo = configured(None, None, gate, "printf '%s' \"$ARC_WORKTREE_BUILD\"");
    let slot = repo.home.join("fixed-source");
    begin(&repo, "three-trees", &slot);
    for index in 0..3 {
        if index > 0 {
            repo.commit(
                &slot,
                "README.md",
                &format!("source {index}\n"),
                "test: advance source",
            );
        }
        if index == 2 {
            let config = slot.join(".arc/worktrees.toml");
            fs::write(
                &config,
                format!("{}timeout = '9s'\n", fs::read_to_string(&config).unwrap()),
            )
            .unwrap();
            git(&slot, &["add", ".arc/worktrees.toml"]);
            git(
                &slot,
                &["commit", "-m", "test: adjust preparation configuration"],
            );
        }
        verify(&repo, &slot);
    }
    let paths = fs::read_to_string(repo.home.join("paths")).unwrap();
    let paths: Vec<_> = paths.lines().collect();
    assert_eq!(paths.len(), 3);
    assert!(paths.iter().all(|path| *path == paths[0]));
    let build = build_path(&repo);
    assert!(build.starts_with(repo.root.join(".git/arc/cache/path-stable-worktrees")));
    assert!(!build.join("prepared").exists());
}

/// Git removes an owned worktree's administrative directory during cleanup;
/// reallocating the same source slot must recover its retained build output.
#[test]
fn default_storage_survives_integrate_cleanup_and_fixed_path_reallocation() {
    let gate = "printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/build\"; if test -f \"$HOME/expect-retained\"; then test -f \"$ARC_WORKTREE_BUILD/sentinel\"; fi; printf retained > \"$ARC_WORKTREE_BUILD/sentinel\"";
    let repo = configured(None, None, gate, "printf stable");
    let slot = repo.home.join("fixed-source");
    begin(&repo, "first-slot", &slot);
    repo.commit(&slot, "README.md", "advanced\n", "test: advance fixed slot");
    verify(&repo, &slot);
    let build = build_path(&repo);
    let admin = PathBuf::from(git_out(&slot, &["rev-parse", "--absolute-git-dir"]));
    repo.arc(&slot)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "first-slot", "--cleanup"])
        .assert()
        .success();
    assert!(!slot.exists());
    assert!(!admin.exists());
    assert!(build.join("sentinel").is_file());
    fs::write(repo.home.join("expect-retained"), "yes").unwrap();
    begin(&repo, "second-slot", &slot);
    verify(&repo, &slot);
    assert_eq!(build_path(&repo), build);
}

/// A literal directory can serve several source slots in the same project;
/// its identity must not bind to one source path or source-tree receipt.
#[test]
fn explicit_directory_is_shared_within_project_and_rejects_foreign_project() {
    let repo = configured(
        Some(Path::new("../shared-build")),
        None,
        "printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/build\"",
        "printf stable",
    );
    let first = repo.home.join("first");
    let second = repo.home.join("second");
    begin(&repo, "first", &first);
    verify(&repo, &first);
    let build = build_path(&repo);
    fs::write(build.join("sentinel"), "retained").unwrap();
    begin(&repo, "second", &second);
    verify(&repo, &second);
    assert_eq!(build_path(&repo), build);
    let foreign = configured(
        Some(&build),
        None,
        "printf wrongly-ran > \"$HOME/foreign-ran\"",
        "printf stable",
    );
    foreign
        .arc(&foreign.root)
        .args(["begin", "foreign"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ownership does not match"));
    assert_eq!(
        fs::read_to_string(build.join("sentinel")).unwrap(),
        "retained"
    );
    assert!(!foreign.home.join("foreign-ran").exists());
}

/// Status must observe the environment without compiling. A changed tree has
/// no preparation receipt, while the probe still receives the stable path.
#[test]
fn status_probe_does_not_run_preparation_after_source_changes() {
    let hook =
        "printf hook >> \"$HOME/hooks\"; printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/build\"";
    let probe = "printf probe >> \"$HOME/probes\"; printf '%s' \"$ARC_WORKTREE_BUILD\"";
    let repo = configured(None, Some(hook), "test -d \"$ARC_WORKTREE_BUILD\"", probe);
    let slot = repo.home.join("slot");
    begin(&repo, "probe", &slot);
    verify(&repo, &slot);
    let hooks = fs::read(repo.home.join("hooks")).unwrap();
    repo.commit(
        &slot,
        "README.md",
        "changed\n",
        "test: advance probe source",
    );
    repo.arc(&slot).args(["snapshot"]).assert().success();
    repo.arc(&slot)
        .args(["status", "--json"])
        .assert()
        .success();
    assert_eq!(fs::read(repo.home.join("hooks")).unwrap(), hooks);
    assert!(repo.home.join("probes").exists());
    assert!(!build_path(&repo).join("prepared").exists());
}

/// Gate and probe writers retain the same exclusive lease through the last
/// output pipe. A second Arc invocation cannot consume active outputs.
#[test]
fn stable_gate_and_probe_hold_exclusive_build_lease() {
    for probe in [false, true] {
        let blocking = "printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/build\"; if test -f \"$HOME/block\"; then printf ready > \"$HOME/ready\"; waits=0; while test ! -f \"$HOME/release\"; do waits=$((waits + 1)); test \"$waits\" -lt 500; sleep 0.02; done; fi; printf stable";
        let gate = if probe { "printf gate" } else { blocking };
        let environment = if probe { blocking } else { "printf stable" };
        let repo = configured(None, None, gate, environment);
        let slot = repo.home.join("slot");
        begin(&repo, "lease", &slot);
        verify(&repo, &slot);
        fs::write(repo.home.join("block"), "yes").unwrap();
        let args = if probe {
            vec!["status", "--json"]
        } else {
            vec!["verify", "--all", "--rerun"]
        };
        let mut child = spawn_arc_keeping_stderr(&repo, &slot, &args, "session-a");
        wait_for(&repo.home.join("ready"));
        assert_busy(&repo);
        repo.arc(&slot)
            .args(["verify", "--all", "--rerun"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("path-stable build lease is busy"));
        fs::write(repo.home.join("release"), "yes").unwrap();
        let status = wait_for_exit(&mut child);
        assert!(status.success(), "{}", child_stderr(&mut child));
    }
}

/// Killing Arc must not release a directory while its gate or probe keeps
/// writing. The owned child must inherit the held open-file description.
#[test]
fn cleaning_build_outputs_does_not_release_the_active_lease() {
    let gate = "if test -f \"$HOME/block\"; then if mkdir \"$HOME/writer\" 2>/dev/null; then rm -rf -- \"$ARC_WORKTREE_BUILD\"; mkdir -p -- \"$ARC_WORKTREE_BUILD\"; printf ready > \"$HOME/ready\"; waits=0; while test ! -f \"$HOME/release\"; do waits=$((waits + 1)); test \"$waits\" -lt 500; sleep 0.02; done; else printf raced > \"$HOME/raced\"; fi; fi; printf stable";
    let repo = configured(None, None, gate, "printf stable");
    let slot = repo.home.join("slot");
    begin(&repo, "compiler-clean", &slot);
    verify(&repo, &slot);
    fs::write(repo.home.join("block"), "yes").unwrap();
    let mut child =
        spawn_arc_keeping_stderr(&repo, &slot, &["verify", "--all", "--rerun"], "session-a");
    wait_for(&repo.home.join("ready"));
    let competing = repo
        .arc(&slot)
        .args(["verify", "--all", "--rerun"])
        .output()
        .unwrap();
    let raced = repo.home.join("raced").exists();
    fs::write(repo.home.join("release"), "yes").unwrap();
    assert!(wait_for_exit(&mut child).success());
    assert!(
        !competing.status.success(),
        "a compiler clean released the active lease: {competing:?}"
    );
    assert!(!raced, "a second writer entered cleaned build storage");
}

#[test]
fn owned_gate_and_probe_keep_lease_after_arc_parent_dies() {
    for probe in [false, true] {
        let writer = "printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/build\"; test -e \"/proc/self/fd/$ARC_WORKTREE_BUILD_LEASE_FD\"; if test -f \"$HOME/block\"; then printf ready > \"$HOME/ready\"; waits=0; while test ! -f \"$HOME/release\"; do waits=$((waits + 1)); test \"$waits\" -lt 500; sleep 0.02; done; printf completed > \"$HOME/completed\"; fi; printf stable";
        let gate = if probe { "true" } else { writer };
        let environment = if probe { writer } else { "printf stable" };
        let repo = configured(None, None, gate, environment);
        let slot = repo.home.join("slot");
        begin(&repo, "parent-death", &slot);
        verify(&repo, &slot);
        fs::write(repo.home.join("block"), "yes").unwrap();
        let args = if probe {
            vec!["status", "--json"]
        } else {
            vec!["verify", "--all", "--rerun"]
        };
        let mut child = spawn_arc_keeping_stderr(&repo, &slot, &args, "session-a");
        wait_for(&repo.home.join("ready"));
        child.kill().unwrap();
        child.wait().unwrap();
        assert_busy(&repo);
        repo.arc(&slot)
            .args(["verify", "--all", "--rerun"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("path-stable build lease is busy"));
        fs::write(repo.home.join("release"), "yes").unwrap();
        wait_for(&repo.home.join("completed"));
        wait_for_lease_release(&lease_path(&repo));
    }
}

fn wait_for_lease_release(path: &Path) {
    let deadline = Instant::now() + HANG_BOUND;
    loop {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        // SAFETY: the fixture owns this regular file descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "writer retained lease after exiting"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// A preparation child retains both the project directory and shared cache
/// after Arc dies; its partial state cannot become a successful preparation.
#[test]
fn owned_preparation_keeps_build_and_cache_leases_after_arc_parent_dies() {
    let hook = "printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/build\"; printf '%s' \"$ARC_WORKTREE_CACHE\" > \"$HOME/cache\"; test -e \"/proc/self/fd/$ARC_WORKTREE_BUILD_LEASE_FD\"; printf ready > \"$HOME/ready\"; waits=0; while test ! -f \"$HOME/release\"; do waits=$((waits + 1)); test \"$waits\" -lt 500; sleep 0.02; done; printf completed > \"$HOME/completed\"";
    let repo = configured(None, Some(hook), "true", "printf stable");
    let slot = repo.home.join("slot");
    let mut child = spawn_arc_keeping_stderr(
        &repo,
        &repo.root,
        &["begin", "dead-hook", "--worktree", slot.to_str().unwrap()],
        "session-a",
    );
    wait_for(&repo.home.join("ready"));
    child.kill().unwrap();
    child.wait().unwrap();
    assert_busy(&repo);
    let cache = PathBuf::from(fs::read_to_string(repo.home.join("cache")).unwrap());
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(cache.join("lease"))
        .unwrap();
    // SAFETY: the fixture owns this regular file descriptor.
    assert_eq!(
        unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().kind(),
        std::io::ErrorKind::WouldBlock
    );
    fs::write(repo.home.join("release"), "yes").unwrap();
    wait_for(&repo.home.join("completed"));
    wait_for_lease_release(&lease_path(&repo));
    wait_for_lease_release(&cache.join("lease"));
    assert!(lease_path(&repo).with_extension("incomplete").is_file());
}

/// Failed incremental preparation retains partial files for inspection and
/// refuses both gates and identity probes until a successful hook repairs it.
#[test]
fn failed_stable_preparation_refuses_partial_outputs_and_can_retry() {
    let hook = "printf hook >> \"$HOME/hooks\"; printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/build\"; if test -f \"$HOME/fail\"; then printf partial > \"$ARC_WORKTREE_BUILD/partial\"; exit 7; fi";
    let repo = configured(
        None,
        Some(hook),
        "printf ran >> \"$HOME/gates-ran\"",
        "printf stable",
    );
    let slot = repo.home.join("slot");
    begin(&repo, "failure", &slot);
    verify(&repo, &slot);
    fs::remove_file(repo.home.join("gates-ran")).unwrap();
    fs::write(repo.home.join("fail"), "yes").unwrap();
    repo.arc(&slot)
        .args(["verify", "--all"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("path-stable preparation failed"));
    let build = build_path(&repo);
    assert!(build.join("partial").is_file());
    assert!(lease_path(&repo).with_extension("incomplete").is_file());
    assert!(!repo.home.join("gates-ran").exists());
    let hooks = fs::read(repo.home.join("hooks")).unwrap();
    let status = json_stdout(repo.arc(&slot).args(["status", "--json"]));
    assert_eq!(status["gates"][0]["green_at_head"], false, "{status}");
    assert!(
        status["gates"][0]["environment"]["probe_failed"]
            .as_str()
            .is_some_and(|detail| detail.contains("path-stable preparation is incomplete")),
        "{status}"
    );
    assert_eq!(fs::read(repo.home.join("hooks")).unwrap(), hooks);
    fs::remove_file(repo.home.join("fail")).unwrap();
    verify(&repo, &slot);
    assert!(build.join("partial").is_file());
    assert!(!lease_path(&repo).with_extension("incomplete").exists());
}

/// Source checkout paths are not disposable build storage, even behind a
/// symlink. Git common administrative storage is the permitted exception.
#[test]
fn stable_directory_refuses_source_containment_and_unsafe_lease_files() {
    for directory in ["build", "../slot/build"] {
        let repo = configured(Some(Path::new(directory)), None, "true", "printf stable");
        let slot = repo.home.join("slot");
        repo.arc(&repo.root)
            .args(["begin", "inside", "--worktree", slot.to_str().unwrap()])
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "must be outside the source checkout",
            ));
        assert!(!slot.join("build").exists());
    }
    for fifo in [false, true] {
        let repo = configured(Some(Path::new("../shared")), None, "true", "printf stable");
        let slot = repo.home.join("slot");
        begin(&repo, "unsafe", &slot);
        verify(&repo, &slot);
        let lease_file = lease_path(&repo);
        fs::remove_file(&lease_file).unwrap();
        let target = repo.home.join("untouched");
        fs::write(&target, "retain").unwrap();
        if fifo {
            let lease = std::ffi::CString::new(lease_file.to_str().unwrap()).unwrap();
            // SAFETY: the CString is a valid, live Unix path.
            assert_eq!(unsafe { libc::mkfifo(lease.as_ptr(), 0o600) }, 0);
        } else {
            symlink(&target, &lease_file).unwrap();
        }
        let started = Instant::now();
        repo.arc(&slot).args(["verify", "--all"]).assert().failure();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(fs::read_to_string(target).unwrap(), "retain");
    }
}

/// Incremental hooks preserve the checkout's source, index and HEAD contract.
#[test]
fn project_timestamp_restoration_may_refresh_index_stat_cache() {
    let hook = "touch \"$ARC_WORKTREE_PATH/README.md\"; git -C \"$ARC_WORKTREE_PATH\" update-index --refresh";
    let repo = configured(None, Some(hook), "true", "printf stable");
    let slot = repo.home.join("slot");
    begin(&repo, "stat-cache", &slot);
    verify(&repo, &slot);
}

#[test]
fn stable_preparation_refuses_source_mutation() {
    for command in [
        "printf changed > \"$ARC_WORKTREE_PATH/README.md\"",
        "git -C \"$ARC_WORKTREE_PATH\" update-index --assume-unchanged README.md",
        "git -C \"$ARC_WORKTREE_PATH\" commit --allow-empty -m preparation",
        "printf injected > \"$ARC_WORKTREE_PATH/program\"",
    ] {
        let repo = configured(None, Some(command), "true", "printf stable");
        repo.arc(&repo.root)
            .args(["begin", "mutation"])
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "worktree preparation changed source, index, HEAD or untracked source paths",
            ));
    }
}

/// A timed-out incremental hook retains its partial directory, stops owned
/// descendants, and cannot pass those outputs to a gate as a cold fallback.
#[test]
fn stable_timeout_retains_partial_outputs_and_stops_owned_children() {
    let hook = "printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/build\"; printf partial > \"$ARC_WORKTREE_BUILD/partial\"; (sleep 2; printf leaked > \"$HOME/leaked\") & exit 0";
    let repo = configured(None, Some(hook), "true", "printf stable");
    let config = repo.root.join(".arc/worktrees.toml");
    fs::write(
        &config,
        format!("{}timeout = '1s'\n", fs::read_to_string(&config).unwrap()),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/worktrees.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: bound incremental preparation"],
    );
    let started = Instant::now();
    repo.arc(&repo.root)
        .args(["begin", "timeout"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("preparation exceeded 1s"));
    assert!(started.elapsed() < Duration::from_secs(5));
    let build = build_path(&repo);
    assert!(build.join("partial").is_file());
    assert!(lease_path(&repo).with_extension("incomplete").is_file());
    wait_for_lease_release(&lease_path(&repo));
    thread::sleep(Duration::from_millis(1250));
    assert!(!repo.home.join("leaked").exists());
}

/// Git common administrative data may live under the main checkout while
/// remaining outside the tracked source and owned worktree cleanup scope.
#[test]
fn explicit_common_administrative_directory_is_allowed() {
    let repo = configured(
        Some(Path::new(".git/build-slot")),
        None,
        "printf '%s' \"$ARC_WORKTREE_BUILD\" > \"$HOME/build\"",
        "printf stable",
    );
    repo.arc(&repo.root)
        .args(["begin", "common-storage", "--no-worktree"])
        .assert()
        .success();
    verify(&repo, &repo.root);
    assert_eq!(build_path(&repo), repo.root.join(".git/build-slot"));
}
