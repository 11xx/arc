use crate::common::*;
use std::os::fd::AsRawFd;

/// A repository declaring a slot pool of `size` whose slots retain `target`.
fn pool(size: u32, extra: &str) -> Repo {
    let repo = Repo::new();
    declare(&repo, size, extra);
    repo
}

fn declare(repo: &Repo, size: u32, extra: &str) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/worktrees.toml"),
        format!("[slots]\nsize = {size}\nretain = [\"target\"]\n{extra}"),
    )
    .unwrap();
    fs::write(repo.root.join(".gitignore"), "target/\n*.local\n").unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "test: declare a slot pool"]);
}

fn slot(repo: &Repo, index: u32) -> PathBuf {
    repo.home
        .join(".worktrees/repo-slots")
        .join(format!("slot-{index:02}"))
}

/// Begin `slug` in a slot, returning the change and its checkout.
fn begin(repo: &Repo, slug: &str) -> (String, PathBuf) {
    let out = stdout(repo.arc(&repo.root).args(["begin", slug, "--slot"]));
    let change = opened_change_id(&out);
    let worktree = out
        .lines()
        .find_map(|line| line.strip_prefix("worktree: "))
        .unwrap();
    (change, PathBuf::from(worktree))
}

fn status(repo: &Repo) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["slots", "status", "--json"]))
}

fn lease_of(repo: &Repo, index: usize) -> serde_json::Value {
    status(repo)["slots"][index]["lease"].clone()
}

fn detached(path: &Path) -> bool {
    !Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(path)
        .status()
        .unwrap()
        .success()
}

/// Whether some process holds `slot`'s use lock.
fn use_lock_busy(slot: &Path) -> bool {
    let name = slot.file_name().unwrap().to_str().unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(slot.with_file_name(format!("{name}.use.lock")))
        .unwrap();
    // SAFETY: the test owns this regular file descriptor.
    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 }
}

fn integrate(repo: &Repo, change: &str, worktree: &Path, extra: &[&str]) {
    repo.commit(worktree, "feature.txt", "feature\n", "feat: add a feature");
    stdout(repo.arc(worktree).args(["snapshot", change]));
    repo.arc(worktree)
        .args(["review", change, "--verdict", "approved"])
        .assert()
        .success();
    let mut command = repo.arc(&repo.root);
    command.args(["integrate", change]).args(extra);
    stdout(&mut command);
}

fn close_abandoned(repo: &Repo, change: &str) -> String {
    stdout(repo.arc(&repo.root).args(["close", change, "--abandoned"]))
}

/// Claim: a second change in a slot builds only the units its diff touches,
/// because the slot keeps its checkout and build between changes. Rejects a
/// slot that starts each change cold (a fresh checkout, or one whose build
/// was removed), where the untouched crate recompiles.
#[test]
fn second_change_in_a_slot_recompiles_only_what_its_diff_touches() {
    let repo = Repo::new();
    for (name, body) in [
        ("alpha", "pub fn alpha() -> u32 { 1 }\n"),
        ("beta", "pub fn beta() -> u32 { 2 }\n"),
    ] {
        fs::create_dir_all(repo.root.join(name).join("src")).unwrap();
        fs::write(
            repo.root.join(name).join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        fs::write(repo.root.join(name).join("src/lib.rs"), body).unwrap();
    }
    fs::write(
        repo.root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"alpha\", \"beta\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    let lockfile = cargo(&repo.root, &["generate-lockfile", "--offline"]);
    assert!(
        lockfile.status.success(),
        "{}",
        String::from_utf8_lossy(&lockfile.stderr)
    );
    declare(&repo, 1, "");

    let (first, worktree) = begin(&repo, "first");
    fs::write(
        worktree.join("alpha/src/lib.rs"),
        "pub fn alpha() -> u32 { 10 }\n",
    )
    .unwrap();
    let built = cargo(&worktree, &["build", "--offline"]);
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    integrate(&repo, &first, &worktree, &[]);

    let (second, again) = begin(&repo, "second");
    assert_eq!(again, worktree, "the second change did not reuse the slot");
    let _ = second;
    fs::write(
        again.join("beta/src/lib.rs"),
        "pub fn beta() -> u32 { 20 }\n",
    )
    .unwrap();
    let output = cargo(&again, &["build", "--offline", "--message-format", "json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fresh: std::collections::BTreeMap<String, bool> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .map(|message| {
            (
                message["target"]["name"].as_str().unwrap().to_string(),
                message["fresh"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        fresh.get("alpha"),
        Some(&true),
        "the untouched crate recompiled: {fresh:?}"
    );
    assert_eq!(
        fresh.get("beta"),
        Some(&false),
        "the changed crate did not rebuild: {fresh:?}"
    );
}

/// Cargo under the caller's own toolchain environment, building into the
/// checkout's `target`.
fn cargo(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO"))
        .args(args)
        .current_dir(cwd)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .output()
        .unwrap()
}

/// Claim: begin ranks a slot by its successful preparation at the target,
/// not by where its checkout stands. Rejects ranking by `HEAD` (which picks
/// the first slot, at the target but never prepared) and ranking that
/// ignores builds (which keeps slot order).
#[test]
fn begin_prefers_a_slot_built_at_the_target() {
    let repo = pool(
        3,
        "\n[prepare]\ncommand = 'test ! -e \"$HOME/fail-prepare\"'\n",
    );
    fs::write(repo.home.join("fail-prepare"), "").unwrap();
    let (cold, cold_path) = begin(&repo, "cold");
    fs::remove_file(repo.home.join("fail-prepare")).unwrap();
    let (warm, warm_path) = begin(&repo, "warm");
    assert_eq!(cold_path, slot(&repo, 1));
    assert_eq!(warm_path, slot(&repo, 2));
    close_abandoned(&repo, &cold);
    close_abandoned(&repo, &warm);
    let target = repo.head(&repo.root);
    assert_eq!(
        repo.head(&cold_path),
        target,
        "the unprepared slot left the target"
    );
    let slots = status(&repo);
    assert!(slots["slots"][0]["built"].is_null(), "{slots:#}");
    assert_eq!(slots["slots"][1]["built"]["revision"], target);

    let (_, chosen) = begin(&repo, "next");
    assert_eq!(
        chosen,
        slot(&repo, 2),
        "begin did not pick the slot built at the target"
    );
}

/// Claim: an exhausted pool refuses with its table and never hands out a
/// leased slot. Rejects reallocating the held slot to the second change.
#[test]
fn exhausted_pool_refuses_with_the_slot_table() {
    let repo = pool(1, "");
    let (first, _) = begin(&repo, "first");
    let token = lease_of(&repo, 0)["token"].clone();
    repo.arc(&repo.root)
        .args(["begin", "second", "--slot"])
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("no free slot")
                .and(predicates::str::contains("slot-01"))
                .and(predicates::str::contains(first.as_str())),
        );
    let lease = lease_of(&repo, 0);
    assert_eq!(lease["change_id"], first.as_str());
    assert_eq!(lease["token"], token);
    assert!(!git_out(&repo.root, &["branch", "--list", "arc/second"]).contains("second"));
}

/// Claim: integration keeps a slot's checkout and retained build, detaches
/// it at the landed commit, deletes the merged branch and frees the lease,
/// even under --cleanup. Rejects removing the checkout like an ordinary
/// worktree, or leaving it on the merged branch.
#[test]
fn integrate_cleanup_keeps_the_slot_detached_and_free() {
    let repo = pool(1, "");
    let (change, worktree) = begin(&repo, "landing");
    fs::create_dir_all(worktree.join("target")).unwrap();
    fs::write(worktree.join("target/build"), "retained\n").unwrap();
    integrate(&repo, &change, &worktree, &["--cleanup"]);
    assert!(
        worktree.join("target/build").is_file(),
        "the retained build was removed"
    );
    assert!(detached(&worktree), "the slot stayed on a branch");
    assert_eq!(repo.head(&worktree), repo.head(&repo.root));
    assert!(git_out(&repo.root, &["branch", "--list", "arc/landing"]).is_empty());
    assert!(lease_of(&repo, 0).is_null(), "the lease was not released");
    let events = stdout(repo.arc(&repo.root).args(["log", &change]));
    assert!(events.contains("slot-released"), "{events}");
}

/// Claim: abandoning a change with source edits keeps its slot leased and
/// begin skips it, while a clean abandoned slot is detached and free.
/// Rejects releasing a slot that holds uncommitted work.
#[test]
fn abandoned_slot_is_released_only_when_clean() {
    let repo = pool(2, "");
    let (dirty, dirty_path) = begin(&repo, "dirty");
    fs::write(dirty_path.join("notes.txt"), "work in progress\n").unwrap();
    let out = close_abandoned(&repo, &dirty);
    assert!(out.contains("kept slot slot-01"), "{out}");
    assert_eq!(lease_of(&repo, 0)["change_id"], dirty.as_str());
    let (_, next) = begin(&repo, "next");
    assert_eq!(next, slot(&repo, 2), "begin took the dirty slot");

    let repo = pool(1, "");
    let (clean, clean_path) = begin(&repo, "clean");
    close_abandoned(&repo, &clean);
    assert!(detached(&clean_path));
    assert!(lease_of(&repo, 0).is_null());
    let (_, again) = begin(&repo, "again");
    assert_eq!(again, clean_path);
}

/// Claim: an ignored file outside `retain` left by the previous change makes
/// begin refuse, naming the slot and the file, and keep the lease. Rejects
/// silently inheriting the previous change's ignored inputs.
#[test]
fn ignored_input_outside_retain_refuses_reuse() {
    let repo = pool(1, "");
    let (first, path) = begin(&repo, "first");
    fs::write(path.join("cabal.project.local"), "flags: -debug\n").unwrap();
    close_abandoned(&repo, &first);
    assert!(lease_of(&repo, 0).is_null());
    repo.arc(&repo.root)
        .args(["begin", "second", "--slot"])
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("cabal.project.local")
                .and(predicates::str::contains("slot-01"))
                .and(predicates::str::contains("arc slots recover")),
        );
    assert!(detached(&path), "begin switched a refused slot");
    assert!(
        !lease_of(&repo, 0).is_null(),
        "the refused slot's lease was dropped"
    );
}

/// Claim: a pool root belongs to one repository; a second clone configured
/// with the same root refuses without touching the first's checkout or
/// lease. Rejects a clone with the same history taking over the slot.
#[test]
fn second_clone_sharing_a_root_refuses() {
    let shared = TempDir::new().unwrap();
    let root = shared.path().join("pool");
    let extra = format!("root = {:?}\n", root.to_str().unwrap());
    let first = pool(1, &extra);
    let second = pool(1, &extra);
    let mut command = first.arc(&first.root);
    let out = stdout(
        command
            .env_remove("ARC_SANDBOX")
            .args(["begin", "one", "--slot"]),
    );
    let worktree = root.join("slot-01");
    assert!(
        out.contains(&format!("worktree: {}", worktree.display())),
        "{out}"
    );
    let head = first.head(&worktree);
    let state = fs::read(root.join("slot-01.json")).unwrap();

    second
        .arc(&second.root)
        .env_remove("ARC_SANDBOX")
        .args(["begin", "two", "--slot"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("belongs to the repository"));
    assert_eq!(first.head(&worktree), head);
    assert_eq!(fs::read(root.join("slot-01.json")).unwrap(), state);
    assert_eq!(
        git_out(&worktree, &["symbolic-ref", "--short", "HEAD"]),
        "arc/one"
    );
}

/// Claim: a gate's descendant keeps the slot in use after the arc that ran
/// the gate is killed, even when it detached and closed every descriptor,
/// and release waits for it to exit. Rejects a use lock that ends with the
/// launching arc or the gate's own shell.
#[test]
fn killed_launcher_keeps_the_slot_in_use_until_descendants_exit() {
    let repo = pool(1, "");
    let (change, path) = begin(&repo, "held");
    repo.commit(&path, "work.txt", "work\n", "feat: work");
    stdout(repo.arc(&path).args(["snapshot", &change]));
    let home = repo.home.display().to_string();
    let grandchild = format!(
        "import os, time\nos.closerange(0, 65536)\nopen({home:?} + '/grand.pid', 'w').write(str(os.getpid()))\n\
         deadline = time.time() + 120\n\
         while not os.path.exists({home:?} + '/go') and time.time() < deadline:\n    time.sleep(0.05)\n"
    );
    fs::write(repo.home.join("grand.py"), grandchild).unwrap();
    let gate = format!(
        "( setsid python3 {home}/grand.py & ) ; while ! test -s {home}/grand.pid; do sleep 0.05; done; \
         sleep 1; : > {home}/gate-exiting"
    );
    let mut launcher = spawn_arc(&repo, &path, &["verify", &change, "--command", &gate]);
    let pid_file = repo.home.join("grand.pid");
    let deadline = Instant::now() + Duration::from_secs(30);
    while fs::read_to_string(&pid_file).map_or(true, |text| text.is_empty()) {
        assert!(
            Instant::now() < deadline,
            "the gate never started its grandchild"
        );
        thread::sleep(Duration::from_millis(20));
    }
    launcher.kill().unwrap();
    wait_for_exit(&mut launcher);
    // Past the gate shell's own exit, only the detached grandchild remains.
    let exiting = repo.home.join("gate-exiting");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !exiting.exists() {
        assert!(Instant::now() < deadline, "the gate shell never finished");
        thread::sleep(Duration::from_millis(20));
    }
    thread::sleep(Duration::from_millis(500));
    assert!(
        use_lock_busy(&path),
        "the slot was freed while the grandchild runs"
    );

    let out = close_abandoned(&repo, &change);
    assert!(out.contains("kept slot slot-01"), "{out}");
    repo.arc(&repo.root)
        .args(["begin", "next", "--slot"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no free slot"));

    fs::write(repo.home.join("go"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while use_lock_busy(&path) {
        assert!(
            Instant::now() < deadline,
            "the slot stayed in use after its grandchild exited"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let token = lease_of(&repo, 0)["token"].as_str().unwrap().to_string();
    stdout(
        repo.arc(&repo.root)
            .args(["slots", "release", "slot-01", "--token", &token]),
    );
    let (_, again) = begin(&repo, "next");
    assert_eq!(again, path);
}

/// Claim: a changed identity discards the retained build before the next
/// change uses it, through `invalidate` when declared and otherwise by
/// removing the `retain` paths; an unchanged identity keeps it. Rejects a
/// slot that hands an incompatible build to the next change.
#[test]
fn identity_change_makes_the_next_build_cold() {
    for invalidate in [false, true] {
        let extra = if invalidate {
            "identity = 'cat \"$HOME/toolchain\"'\ninvalidate = 'rm -rf target && printf ran > \"$HOME/invalidated\"'\n"
        } else {
            "identity = 'cat \"$HOME/toolchain\"'\n"
        };
        let repo = pool(1, extra);
        fs::write(repo.home.join("toolchain"), "one\n").unwrap();
        let build = |path: &Path| {
            fs::create_dir_all(path.join("target")).unwrap();
            fs::write(path.join("target/stamp"), "built\n").unwrap();
        };
        let (first, path) = begin(&repo, "first");
        build(&path);
        close_abandoned(&repo, &first);
        let (second, path) = begin(&repo, "second");
        assert!(
            path.join("target/stamp").is_file(),
            "an unchanged identity lost its build"
        );
        close_abandoned(&repo, &second);

        fs::write(repo.home.join("toolchain"), "two\n").unwrap();
        let out = stdout(repo.arc(&repo.root).args(["begin", "third", "--slot"]));
        assert!(out.contains("identity changed"), "{out}");
        assert!(
            !path.join("target/stamp").exists(),
            "the next build would be warm"
        );
        assert_eq!(
            repo.home.join("invalidated").is_file(),
            invalidate,
            "invalidate ran {}",
            if invalidate { "never" } else { "undeclared" }
        );
    }
}

/// Claim: an identity change refuses when the build lives in a project
/// directory arc does not own and no invalidate command is declared.
#[test]
fn identity_change_refuses_external_storage_without_invalidate() {
    let repo = pool(
        1,
        "identity = 'cat \"$HOME/toolchain\"'\n\n[prepare]\nmode = 'path-stable'\ndirectory = '../../external-build'\n",
    );
    fs::write(repo.home.join("toolchain"), "one\n").unwrap();
    let (first, _) = begin(&repo, "first");
    close_abandoned(&repo, &first);
    fs::write(repo.home.join("toolchain"), "two\n").unwrap();
    repo.arc(&repo.root)
        .args(["begin", "second", "--slot"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not arc's to clear"));
}

/// Claim: `[slots] default = true` makes begin lease a slot for a worktree
/// checkout without --slot, and release and recover refuse an obsolete
/// token.
#[test]
fn default_pool_and_obsolete_tokens() {
    let repo = pool(1, "default = true\n");
    let out = stdout(repo.arc(&repo.root).args(["begin", "implicit"]));
    let change = opened_change_id(&out);
    assert!(
        out.contains(&format!("worktree: {}", slot(&repo, 1).display())),
        "{out}"
    );
    for verb in ["release", "recover"] {
        repo.arc(&repo.root)
            .args(["slots", verb, "slot-01", "--token", "stale"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("obsolete"));
    }
    let token = lease_of(&repo, 0)["token"].as_str().unwrap().to_string();
    repo.arc(&repo.root)
        .args(["slots", "release", "slot-01", "--token", &token])
        .assert()
        .failure()
        .stderr(predicates::str::contains(format!(
            "holds open change {change}"
        )));
}

/// Claim: a gate in a slot records its command's own exit through the
/// holder, and a background child left in its process group neither delays
/// completion nor outlives it. Rejects a holder whose exit stands in for the
/// command's, or one that keeps the gate waiting on its group.
#[test]
fn gates_in_a_slot_record_their_command_exit() {
    let repo = pool(1, "");
    let (change, path) = begin(&repo, "gates");
    repo.commit(&path, "work.txt", "work\n", "feat: work");
    stdout(repo.arc(&path).args(["snapshot", &change]));
    repo.arc(&path)
        .args(["verify", &change, "--command", "exit 3"])
        .assert()
        .stdout(predicates::str::contains("verification: Fail"));
    let marker = repo.home.join("background.pid");
    let started = Instant::now();
    repo.arc(&path)
        .args([
            "verify",
            &change,
            "--command",
            &format!(
                "sleep 60 >/dev/null 2>&1 & echo $! > {}; exit 0",
                marker.display()
            ),
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("verification: Pass"));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the gate waited on its background child"
    );
    let pid = fs::read_to_string(&marker).unwrap();
    let alive = Path::new(&format!("/proc/{}", pid.trim())).exists()
        && !fs::read_to_string(format!("/proc/{}/stat", pid.trim()))
            .unwrap_or_default()
            .contains(") Z ");
    assert!(!alive, "the gate's background child outlived it");
    assert!(
        !use_lock_busy(&path),
        "the finished gate left the slot in use"
    );
}

/// The `arc slots recover` command a refusal printed, as a shell line.
fn recover_command(stderr: &str) -> String {
    let start = stderr.find("`arc slots recover ").expect(stderr) + 1;
    let end = start + stderr[start..].find('`').unwrap();
    stderr[start..end].to_string()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

/// Claim: `arc slots warm` builds the best free slot that is not built at
/// the target, including a cold slot whose checkout already stands there,
/// skips a slot whose matching build is at the target, records `built` and
/// frees the slot, and runs at the lowest CPU priority. Rejects treating a
/// checkout at the target as warm, re-warming a current slot, and a warm
/// that leaves its slot leased.
#[test]
fn warm_builds_the_best_stale_free_slot() {
    let repo = pool(
        2,
        "warm = '{ nice; ionice 2>/dev/null || echo none; } > \"$HOME/warm-$(basename \"$PWD\")\"'\n",
    );
    let (first, worktree) = begin(&repo, "first");
    integrate(&repo, &first, &worktree, &[]);
    let target = repo.head(&repo.root);
    assert_eq!(repo.head(&worktree), target);
    assert!(status(&repo)["slots"][0]["built"].is_null());

    let out = stdout(repo.arc(&repo.root).args(["slots", "warm"]));
    assert!(
        out.contains("warmed slot-01"),
        "the cold slot at the target was skipped: {out}"
    );
    let slots = status(&repo);
    assert_eq!(slots["slots"][0]["built"]["revision"], target);
    assert!(
        slots["slots"][0]["lease"].is_null(),
        "the warm kept its lease"
    );
    assert_eq!(slots["slots"][0]["phase"], "free");
    assert!(detached(&worktree));
    let priority = fs::read_to_string(repo.home.join("warm-slot-01")).unwrap();
    let mut lines = priority.lines();
    assert_eq!(
        lines.next(),
        Some("19"),
        "the warm ran at a normal priority"
    );
    assert!(
        matches!(lines.next(), Some("idle" | "none")),
        "the warm ran outside the idle I/O class: {priority}"
    );

    let out = stdout(repo.arc(&repo.root).args(["slots", "warm"]));
    assert!(
        out.contains("warmed slot-02"),
        "the current slot was warmed again: {out}"
    );
    assert_eq!(status(&repo)["slots"][1]["built"]["revision"], target);
    assert!(detached(&slot(&repo, 2)));
    let out = stdout(repo.arc(&repo.root).args(["slots", "warm"]));
    assert!(out.contains("nothing to warm"), "{out}");

    repo.commit(&repo.root, "next.txt", "next\n", "feat: move the target");
    let moved = repo.head(&repo.root);
    let out = stdout(repo.arc(&repo.root).args(["slots", "warm", "--limit", "2"]));
    assert!(
        out.contains("warmed slot-01") && out.contains("warmed slot-02"),
        "{out}"
    );
    let slots = status(&repo);
    for index in 0..2 {
        assert_eq!(slots["slots"][index]["built"]["revision"], moved);
    }
}

/// Claim: a running warm holds its slot exclusively: a concurrent begin
/// skips that slot, or refuses when no other is free, while the warm
/// command, running under a holder that shares the warm's exclusive use,
/// starts and completes. Rejects a holder that waits for the lock its
/// launcher holds, and a begin that takes a slot being warmed.
#[test]
fn a_running_warm_holds_its_slot_against_begin() {
    let repo = pool(
        2,
        "warm = 'touch \"$HOME/warm-started\"; i=0; while [ ! -e \"$HOME/warm-go\" ] && [ $i -lt 1200 ]; do sleep 0.1; i=$((i+1)); done; test -e \"$HOME/warm-go\"'\n",
    );
    let target = repo.head(&repo.root);
    let mut warm = spawn_arc(&repo, &repo.root, &["slots", "warm"]);
    wait_until("the warm command starts", || {
        repo.home.join("warm-started").exists()
    });
    assert!(use_lock_busy(&slot(&repo, 1)));

    let (_, taken) = begin(&repo, "beside");
    assert_eq!(taken, slot(&repo, 2), "begin took the slot being warmed");
    repo.arc(&repo.root)
        .args(["begin", "crowded", "--slot"])
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("no free slot").and(predicates::str::contains("warming")),
        );

    fs::write(repo.home.join("warm-go"), "").unwrap();
    assert!(wait_for_exit(&mut warm).success(), "the warm failed");
    let slots = status(&repo);
    assert_eq!(slots["slots"][0]["built"]["revision"], target);
    assert!(slots["slots"][0]["lease"].is_null());
    let (_, next) = begin(&repo, "next");
    assert_eq!(next, slot(&repo, 1));
}

/// Claim: a failing warm keeps its lease, reports the failure and the
/// recovery command, and records no build; that command frees the slot.
/// Rejects a failed warm that frees its slot or records it as built.
#[test]
fn a_failing_warm_keeps_its_lease_until_recovered() {
    let repo = pool(1, "warm = 'exit 7'\n");
    let output = repo
        .arc(&repo.root)
        .args(["slots", "warm"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("exited 7"), "{stderr}");
    let slots = status(&repo);
    assert_eq!(slots["slots"][0]["phase"], "warming", "{slots:#}");
    assert!(
        !slots["slots"][0]["lease"].is_null(),
        "the failed warm freed its slot"
    );
    assert!(
        slots["slots"][0]["built"].is_null(),
        "the failed warm recorded a build"
    );
    repo.arc(&repo.root)
        .args(["begin", "blocked", "--slot"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no free slot"));

    stdout(&mut repo.arc_shell(&repo.root, &recover_command(&stderr)));
    let slots = status(&repo);
    assert!(slots["slots"][0]["lease"].is_null());
    assert_eq!(slots["slots"][0]["phase"], "free");
    assert!(detached(&slot(&repo, 1)));
    let (_, again) = begin(&repo, "again");
    assert_eq!(again, slot(&repo, 1));
}

/// Claim: without `[slots] warm`, warming refuses naming the key.
#[test]
fn warm_without_a_command_refuses_naming_the_key() {
    let repo = pool(1, "");
    repo.arc(&repo.root)
        .args(["slots", "warm"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("`warm` in [slots]"));
}

/// Claim: a gate in a slot after a warm reuses the warmed build: every unit
/// of a fixture workspace is fresh, so warming filled the storage the gate
/// consumes. Rejects a warm that builds anywhere but the slot's own build.
#[test]
fn gates_after_a_warm_reuse_the_warmed_build() {
    let repo = Repo::new();
    for (name, body) in [
        ("alpha", "pub fn alpha() -> u32 { 1 }\n"),
        ("beta", "pub fn beta() -> u32 { 2 }\n"),
    ] {
        fs::create_dir_all(repo.root.join(name).join("src")).unwrap();
        fs::write(
            repo.root.join(name).join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        fs::write(repo.root.join(name).join("src/lib.rs"), body).unwrap();
    }
    fs::write(
        repo.root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"alpha\", \"beta\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    let lockfile = cargo(&repo.root, &["generate-lockfile", "--offline"]);
    assert!(lockfile.status.success());
    let cargo = format!(
        "env -u CARGO_TARGET_DIR -u CARGO_BUILD_TARGET_DIR {:?} build --offline",
        env!("CARGO")
    );
    declare(&repo, 1, &format!("warm = '{cargo}'\n"));

    let out = stdout(repo.arc(&repo.root).args(["slots", "warm"]));
    assert!(out.contains("warmed slot-01"), "{out}");
    let (change, worktree) = begin(&repo, "after-warm");
    assert_eq!(worktree, slot(&repo, 1));
    repo.commit(&worktree, "NOTES.md", "notes\n", "docs: notes");
    stdout(repo.arc(&worktree).args(["snapshot", &change]));
    let units = repo.home.join("units.json");
    stdout(repo.arc(&worktree).args([
        "verify",
        &change,
        "--command",
        &format!("{cargo} --message-format json > {}", units.display()),
    ]));
    let fresh: std::collections::BTreeMap<String, bool> = fs::read_to_string(&units)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .map(|message| {
            (
                message["target"]["name"].as_str().unwrap().to_string(),
                message["fresh"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        fresh.get("alpha").zip(fresh.get("beta")),
        Some((&true, &true)),
        "the gate rebuilt what the warm built: {fresh:?}"
    );
}
