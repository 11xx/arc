use super::common::*;

const FORMATS: [&str; 2] = ["sha1", "sha256"];

/// Claim: subprocess waits return the actual exit status and kill and reap a
/// hung child before reporting the deadline. Rejects fabricated success,
/// returning before exit and leaving a live process or zombie after a timeout.
#[test]
fn subprocess_waits_preserve_status_and_reap_timed_out_children() {
    let mut child = Command::new("sh").args(["-c", "exit 23"]).spawn().unwrap();
    let status = wait_for_exit_within(&mut child, Duration::from_secs(5));
    let actual = child.wait().unwrap();
    assert_eq!(status.code(), Some(23), "wait fabricated the exit status");
    assert_eq!(status, actual);
    assert_eq!(wait_for_exit(&mut child), actual, "cached exit status");

    let mut child = Command::new("sleep").arg("240").spawn().unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_for_exit_within(&mut child, Duration::from_millis(20))
    }));
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::uninit();
    // SAFETY: info provides writable storage; WNOWAIT leaves cleanup to Child.
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    let reaped =
        waited == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD);
    if !reaped {
        let _ = child.kill();
        child.wait().unwrap();
    }
    assert!(result.is_err(), "hung child did not report its deadline");
    assert!(reaped, "timed-out child was not reaped");
    assert_eq!(HANG_BOUND, Duration::from_secs(120));
}

/// Claim: a selection whose receipt names the selected file reaches ordinary,
/// shell, piped, asynchronous and environment-isolated fixture commands. The
/// child harness accepts the canary only because the receipt binds that file
/// of this build, as the runner's does. Rejects a launch that silently uses
/// Cargo's default executable beside the selected CLI.
#[test]
fn fixture_commands_use_the_selected_executable() {
    const CHILD: &str = "ARC_TEST_CHILD_BINARY_CANARY";
    const MARKER: &str = "selected fixture executable\n";
    if crate::harness_entry::in_child_mode(CHILD) {
        let repo = Repo::new();
        repo.arc(&repo.root)
            .arg("--version")
            .assert()
            .success()
            .stdout(MARKER);
        repo.arc_under_umask(&repo.root, "077")
            .arg("--version")
            .assert()
            .success()
            .stdout(MARKER);
        repo.arc_shell(&repo.root, "arc --version")
            .assert()
            .success()
            .stdout(MARKER);
        for child in [
            repo.spawn_arc_piped(&repo.root, &["--version"]),
            spawn_arc(&repo, &repo.root, &["--version"]),
            Command::new(arc_binary())
                .env_clear()
                .arg("--version")
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        ] {
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(output.stdout, MARKER.as_bytes());
        }
        return;
    }

    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    let canary = repo.home.join("selected-cli");
    fs::write(&canary, format!("#!/bin/sh\nprintf '{MARKER}'\n")).unwrap();
    fs::set_permissions(&canary, fs::Permissions::from_mode(0o700)).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "fixture_seeds::fixture_commands_use_the_selected_executable",
            "--nocapture",
        ])
        .env(CHILD, crate::harness_entry::child_token())
        .env(
            "ARC_TEST_BIN_RECEIPT",
            selection_receipt(&canary, true).unwrap(),
        )
        .env("ARC_TEST_BIN", canary)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success()
            && stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
        "selected executable was bypassed or the child ran no test: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const STALE: &str = "ARC_TEST_CHILD_STALE_SELECTION";

/// A shell script at `path` that records each launch at `launched`.
fn launch_recorder(path: &Path, launched: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, format!("#!/bin/sh\ntouch '{}'\n", launched.display())).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// A child harness given `ARC_TEST_BIN=selected` and `receipt` must refuse
/// before any test, naming the variable and the runner, without launching the
/// selection. `name` is the calling witness, which is what the child runs.
fn assert_selection_refused(name: &str, selected: &Path, receipt: Option<String>, launched: &Path) {
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", name, "--nocapture"])
        .env(STALE, crate::harness_entry::child_token())
        .env("ARC_TEST_BIN", selected)
        .env_remove("ARC_TEST_BIN_RECEIPT");
    if let Some(receipt) = &receipt {
        child.env("ARC_TEST_BIN_RECEIPT", receipt);
    }
    let output = child.output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !launched.exists(),
        "receipt {receipt:?}: the unproven selection ran"
    );
    assert!(
        !output.status.success()
            && stderr.contains("ARC_TEST_BIN selects")
            && stderr.contains("python3 .arc/test-binary.py"),
        "receipt {receipt:?}: the harness did not refuse by name: {output:?}"
    );
}

/// In child mode: launch the selected executable once, as a fixture would.
fn launch_selection_in_child() -> bool {
    if crate::harness_entry::in_child_mode(STALE) {
        let repo = Repo::new();
        repo.arc(&repo.root).arg("--version").assert().success();
        return true;
    }
    false
}

/// Claim: an `ARC_TEST_BIN` with no receipt stops the harness before any
/// test, naming the variable and the runner, and the selection never runs.
/// Rejects a plain `cargo test` that silently exercises an exported, stale
/// executable.
#[test]
fn a_selection_without_a_receipt_is_refused() {
    if launch_selection_in_child() {
        return;
    }
    let repo = Repo::new();
    let (stale, launched) = (repo.home.join("stale-arc"), repo.home.join("launched"));
    launch_recorder(&stale, &launched);
    assert_selection_refused(
        "fixture_seeds::a_selection_without_a_receipt_is_refused",
        &stale,
        None,
        &launched,
    );
}

/// Claim: a receipt the runner made for another build of arc stops the
/// harness before any test, and the selection never runs. Rejects a
/// selection exported from an earlier build vouching for itself after the
/// source changed.
#[test]
fn a_selection_receipt_for_another_build_is_refused() {
    if launch_selection_in_child() {
        return;
    }
    let repo = Repo::new();
    let (stale, launched) = (repo.home.join("stale-arc"), repo.home.join("launched"));
    launch_recorder(&stale, &launched);
    let receipt = selection_receipt(&stale, true).unwrap();
    let (build, rest) = receipt.split_once(' ').unwrap();
    assert_selection_refused(
        "fixture_seeds::a_selection_receipt_for_another_build_is_refused",
        &stale,
        Some(format!("{build}0 {rest}")),
        &launched,
    );
}

/// Claim: a receipt for one executable cannot select another: given another
/// file's receipt, the harness stops before any test and the selection never
/// runs. Rejects a valid receipt vouching for whatever path `ARC_TEST_BIN`
/// names.
#[test]
fn a_selection_receipt_for_another_executable_is_refused() {
    if launch_selection_in_child() {
        return;
    }
    let repo = Repo::new();
    let (stale, launched) = (repo.home.join("stale-arc"), repo.home.join("launched"));
    launch_recorder(&stale, &launched);
    let vouched = repo.home.join("vouched-arc");
    launch_recorder(&vouched, &repo.home.join("vouched-launched"));
    assert_selection_refused(
        "fixture_seeds::a_selection_receipt_for_another_executable_is_refused",
        &stale,
        selection_receipt(&vouched, true),
        &launched,
    );
}

/// Claim: an executable replaced after its receipt was made is refused: the
/// harness stops before any test and the replacement never runs. Rejects a
/// receipt that names a path and keeps vouching for whatever file is there.
#[test]
fn a_selection_replaced_after_its_receipt_is_refused() {
    if launch_selection_in_child() {
        return;
    }
    let repo = Repo::new();
    let (stale, launched) = (repo.home.join("stale-arc"), repo.home.join("launched"));
    launch_recorder(&stale, &launched);
    let receipt = selection_receipt(&stale, true);
    let replacement = repo.home.join("replacement-arc");
    launch_recorder(&replacement, &launched);
    fs::write(
        &replacement,
        format!("{}\n", fs::read_to_string(&replacement).unwrap()),
    )
    .unwrap();
    fs::rename(&replacement, &stale).unwrap();
    assert_selection_refused(
        "fixture_seeds::a_selection_replaced_after_its_receipt_is_refused",
        &stale,
        receipt,
        &launched,
    );
}

/// Claim: fixtures start with empty hook and exclude locations and commits
/// spawn no automatic maintenance, while explicit garbage collection keeps
/// the same valid head. Rejects inactive template copies, background maintenance
/// processes and disabling the explicit pruning that retention tests require.
#[test]
fn fixtures_omit_inactive_templates_and_automatic_maintenance() {
    for format in FORMATS {
        let repo = Repo::with_object_format(format);
        assert!(
            fs::read_dir(repo.root.join(".git/hooks"))
                .unwrap()
                .next()
                .is_none(),
            "{format} fixture copied inactive hook templates"
        );
        assert!(fs::read(repo.root.join(".git/info/exclude"))
            .unwrap()
            .is_empty());
        let trace = repo.home.join("git-trace.jsonl");
        let output = Command::new("git")
            .current_dir(&repo.root)
            .args(["commit", "--allow-empty", "-m", "test: fixture maintenance"])
            .envs(NO_EDITOR)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", repo.home.join(".gitconfig"))
            .env("XDG_CONFIG_HOME", repo.home.join(".config"))
            .env("GIT_TRACE2_EVENT", &trace)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let events = fs::read_to_string(trace).unwrap();
        let automatic = events.lines().any(|line| {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            event["event"] == "child_start"
                && event["argv"].as_array().is_some_and(|arguments| {
                    arguments
                        .iter()
                        .any(|argument| argument.as_str() == Some("maintenance"))
                })
        });
        assert!(!automatic, "{format} commit spawned automatic maintenance");
        let head = repo.head(&repo.root);
        git(&repo.root, &["gc", "--prune=now", "--quiet"]);
        assert_eq!(repo.head(&repo.root), head, "{format} explicit collection");
        git(&repo.root, &["fsck", "--no-progress"]);
    }
}

/// Every file under each of `dirs`, by path, with a digest of its bytes.
fn files(dirs: &[&Path]) -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, found: &mut Vec<(PathBuf, String)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, found);
            } else {
                let digest = hex::encode(Sha256::digest(fs::read(&path).unwrap()));
                found.push((path, digest));
            }
        }
    }
    let mut found = Vec::new();
    for dir in dirs {
        walk(dir, &mut found);
    }
    found.sort();
    found
}

/// Rejects a fixture that runs Git itself, or rebuilds its seed: the Git
/// commands spent on fixtures stay at the seed's initializer, once per object
/// format, however many fixtures the suite creates.
#[test]
fn fixtures_run_no_git_beyond_one_seed_per_object_format() {
    for format in FORMATS {
        drop(Repo::with_object_format(format));
        let (spawns, fixtures) = git_spawns_during(|| {
            (0..3)
                .map(|_| Repo::with_object_format(format))
                .collect::<Vec<_>>()
        });
        assert_eq!(spawns, 0, "{format} fixtures ran Git");
        assert_eq!(seed(format).initializer_spawns, 6, "{format} seed");
        assert_eq!(
            git_out(&fixtures[0].root, &["rev-parse", "--show-object-format"]),
            format
        );
    }
}

/// Rejects a copied index whose stat data or seal still describes the seed: a
/// fresh fixture is clean to plumbing that trusts stat data, and a status
/// leaves its index as it found it, as a fresh commit's index is.
#[test]
fn a_copied_fixture_is_clean_without_a_refresh() {
    for format in FORMATS {
        let repo = Repo::with_object_format(format);
        let git_files = files(&[&repo.root.join(".git")]);
        assert_eq!(git_out(&repo.root, &["diff-files", "--name-only"]), "");
        assert_eq!(git_out(&repo.root, &["status", "--porcelain"]), "");
        assert_eq!(
            files(&[&repo.root.join(".git")]),
            git_files,
            "{format} status rewrote the copied repository"
        );
        git(&repo.root, &["fsck", "--no-progress"]);
    }
}

/// Rejects fixtures that share mutable state with each other or the seed:
/// changing one fixture's config, refs, index, worktree administration and
/// ledger changes no file of another fixture or of the seed.
#[test]
fn mutating_one_fixture_leaves_another_and_the_seed_unchanged() {
    for format in FORMATS {
        let mutated = Repo::with_object_format(format);
        let bystander = Repo::with_object_format(format);
        let bystander_dirs = [bystander.root.as_path(), bystander.home.as_path()];
        let bystander_files = files(&bystander_dirs);
        let seed_files = files(&[&seed(format).root]);

        git(&mutated.root, &["config", "user.name", "Mutated"]);
        mutated.commit(&mutated.root, "README.md", "mutated\n", "test: mutate");
        let side = mutated.home.join("side");
        git(
            &mutated.root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                side.to_str().unwrap(),
            ],
        );
        begin_change(&mutated, "mutation", None);

        let mutated_git = mutated.root.join(".git");
        for file in ["config", "index", "refs/heads/master", "refs/heads/side"] {
            assert!(
                fs::read(mutated_git.join(file)).ok()
                    != fs::read(bystander.root.join(".git").join(file)).ok(),
                "{format} mutation did not reach {file}"
            );
        }
        assert!(mutated_git.join("worktrees").is_dir());
        assert!(mutated_git.join("arc/changes").is_dir());
        assert_eq!(
            files(&bystander_dirs),
            bystander_files,
            "{format} bystander"
        );
        assert_eq!(files(&[&seed(format).root]), seed_files, "{format} seed");
    }
}
