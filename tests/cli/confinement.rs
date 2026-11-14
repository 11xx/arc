use super::common::*;

fn supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: a version query has no pointers or descriptor result.
        let abi = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<u8>(),
                0,
                1,
            )
        };
        if abi >= 3 {
            return true;
        }
        eprintln!("SKIP real confinement: Landlock ABI >= 3 unavailable ({abi})");
    }
    #[cfg(not(target_os = "linux"))]
    eprintln!("SKIP real confinement: Linux Landlock unavailable");
    false
}

// Outside the automatically writable system temporary directory, but beneath
// the enclosing project's declared build output so nested confinement composes.
fn fixture() -> (Repo, TempDir) {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/confinement-fixtures");
    fs::create_dir_all(&base).unwrap();
    let scratch = tempfile::tempdir_in(base).unwrap();
    let mut repo = Repo::new();
    let root = scratch.path().join("repo");
    let output = Command::new("cp")
        .args(["-a", "--"])
        .arg(&repo.root)
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    repo.root = root;
    (repo, scratch)
}

fn declaration(script: &str) -> String {
    format!("[confine]\nwrites=['target']\n[gates.guard]\ncommand={script:?}\n")
}

fn open(repo: &Repo) -> String {
    opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "confined",
        "--no-worktree",
    ])))
}

/// Claim I12: a destination-confined gate cannot alter a decoy repository by
/// naming it explicitly, and its failure names writable roots once.
/// Rejects a gate shell or command holder executing without confinement.
#[test]
fn gate_escape_fails_without_touching_decoy() {
    if !supported() {
        return;
    }
    let (repo, scratch) = fixture();
    let decoy = scratch.path().join("decoy");
    fs::create_dir(&decoy).unwrap();
    git(&decoy, &["init", "-q"]);
    let before = fs::read(decoy.join(".git/config")).unwrap();
    let script = format!(
        "sh -c 'git -C \"{}\" config guard.changed true'",
        decoy.display()
    );
    repo.redeclare_gates(&declaration(&script));
    let change = open(&repo);
    repo.commit(&repo.root, "feature", "feature", "test: confined gate");
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    let output = repo
        .arc(&repo.root)
        .args(["verify", &change, "--all"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("ran confined").count(), 1, "{stderr}");
    assert!(stderr.contains("writable roots:"), "{stderr}");
    assert_eq!(fs::read(decoy.join(".git/config")).unwrap(), before);
    let state = json_stdout(repo.arc(&repo.root).args(["show", &change, "--json"]));
    let receipt = &state["verifications"][0];
    assert_eq!(receipt["confinement"]["confined"], true, "{receipt}");
    assert!(receipt["confinement"]["identity"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert!(receipt["confinement"]["abi"].as_u64().unwrap() >= 3);
}

/// Claim I5: gates obey destination roots even when a change declares the
/// entire checkout writable; an allowed write still passes and is receipted.
/// Rejects reading the change head's writable roots or keeping an unconfined pass.
#[test]
fn destination_roots_bind_runs_and_new_confinement_requires_execution() {
    if !supported() {
        return;
    }
    let (repo, _scratch) = fixture();
    let script =
        "printf allowed > target/result; if test -f attempt; then printf forbidden > escaped; fi";
    repo.redeclare_gates(&declaration(script));
    let change = open(&repo);
    repo.commit(&repo.root, ".gitignore", "target/\n", "test: build outputs");
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    stdout(repo.arc(&repo.root).args(["verify", &change, "--all"]));
    assert_eq!(
        fs::read_to_string(repo.root.join("target/result")).unwrap(),
        "allowed"
    );
    repo.commit(
        &repo.root,
        ".arc/gates.toml",
        &declaration(script).replace("writes=['target']", "writes=['.']\nrequired=false"),
        "test: attempt broader roots",
    );
    repo.commit(&repo.root, "attempt", "write", "test: attempt source write");
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    repo.arc(&repo.root)
        .args(["verify", &change, "--all", "--rerun"])
        .assert()
        .failure();
    assert!(!repo.root.join("escaped").exists());
    let status = json_stdout(repo.arc(&repo.root).args(["status", &change, "--json"]));
    assert!(
        status.to_string().contains("confinement differs"),
        "{status}"
    );

    let (adoption, _scratch) = fixture();
    adoption.redeclare_gates("[gates.guard]\ncommand='true'\n");
    let change = open(&adoption);
    stdout(adoption.arc(&adoption.root).args(["snapshot", &change]));
    stdout(
        adoption
            .arc(&adoption.root)
            .args(["verify", &change, "--all"]),
    );
    adoption.redeclare_gates(&declaration("true"));
    let output =
        stdout(
            adoption
                .arc(&adoption.root)
                .args(["verify", &change, "--all", "--skip-green"]),
        );
    assert!(!output.contains("reused"), "{output}");
    let state = json_stdout(
        adoption
            .arc(&adoption.root)
            .args(["show", &change, "--json"]),
    );
    assert_eq!(
        state["verifications"].as_array().unwrap().len(),
        2,
        "{state}"
    );
    assert_eq!(
        state["verifications"][1]["confinement"]["confined"], true,
        "{state}"
    );
}

/// Claim: preparation, environment probes, selectors, checkout hooks and the
/// changelog renderer share gate confinement. Rejects any project runner that
/// permits an explicit write to a decoy outside its roots.
#[test]
fn auxiliary_project_commands_share_the_write_boundary() {
    if !supported() {
        return;
    }
    let (repo, scratch) = fixture();
    let decoy = scratch.path().join("decoy");
    fs::write(&decoy, "retained").unwrap();
    let script = format!("printf overwritten > '{}'", decoy.display());
    repo.redeclare_gates(&declaration("true"));
    fs::write(
        repo.root.join(".arc/worktrees.toml"),
        format!("[prepare]\nmode='path-stable'\ncommand={script:?}\n"),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/worktrees.toml"]);
    git(&repo.root, &["commit", "-m", "test: preparation boundary"]);
    let change = open(&repo);
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    repo.arc(&repo.root)
        .args(["verify", &change, "--all"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ran confined"));
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
    git(&repo.root, &["rm", ".arc/worktrees.toml"]);
    git(&repo.root, &["commit", "-m", "test: probe boundary"]);
    repo.redeclare_gates(&format!(
        "[confine]\nwrites=['target']\n[gates.guard]\ncommand='true'\nenvironment={script:?}\n"
    ));
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    let output = repo
        .arc(&repo.root)
        .args(["verify", &change, "--all", "--rerun"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("ran confined"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
    // A selector's failed answer runs the gate; the decoy stays untouched.
    repo.redeclare_gates(&format!(
        "[confine]\nwrites=['target']\n[gates.guard]\ncommand='true'\nselector={script:?}\n"
    ));
    repo.commit(&repo.root, "feature", "one", "test: selector baseline");
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    stdout(
        repo.arc(&repo.root)
            .args(["verify", &change, "--all", "--rerun"]),
    );
    repo.commit(&repo.root, "feature", "two", "test: selector boundary");
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    stdout(
        repo.arc(&repo.root)
            .args(["verify", &change, "--all", "--skip-green"]),
    );
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
    repo.declare_gates_locally("");
    fs::write(
        repo.root.join(".arc/changelog.toml"),
        format!("renderer='command'\nrenderer_command=['sh','-c',{script:?}]\n"),
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["changelog"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ran confined"));
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
    use std::os::unix::fs::PermissionsExt;
    let hook = repo.root.join(".git/hooks/post-checkout");
    fs::write(&hook, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    repo.arc(&repo.root)
        .args(["begin", "hook", "--target", "master"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ran confined"));
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
}

/// Claim: publication scratch commands and slot identity/warm commands obey the
/// declared roots; doctor advises rather than rejects an unconfined project.
/// Rejects unconfined execution in a retained or scratch checkout.
#[test]
fn publication_and_slot_commands_are_confined_and_doctor_advises() {
    if !supported() {
        return;
    }
    let (repo, scratch) = fixture();
    let decoy = scratch.path().join("decoy");
    fs::write(&decoy, "retained").unwrap();
    let script = format!("printf overwritten > '{}'", decoy.display());
    repo.redeclare_gates(&declaration(&script));
    repo.arc(&repo.root)
        .args(["publication", "--target", "master"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ran confined"));
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
    repo.commit(
        &repo.root,
        ".arc/worktrees.toml",
        &format!(
            "[slots]\nsize=1\nroot={:?}\nidentity={script:?}\nwarm='true'\n",
            repo.home.join("slots").display().to_string()
        ),
        "test: slot identity",
    );
    repo.arc(&repo.root)
        .args(["slots", "warm"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ran confined"));
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
    repo.commit(
        &repo.root,
        ".arc/worktrees.toml",
        &format!(
            "[slots]\nsize=1\nroot={:?}\nidentity='printf identity'\nwarm={script:?}\n",
            repo.home.join("slots").display().to_string()
        ),
        "test: slot warming",
    );
    repo.arc(&repo.root)
        .args(["slots", "warm"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ran confined"));
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
    let plain = Repo::new();
    plain.redeclare_gates("[gates.check]\ncommand='true'\n");
    let doctor = json_stdout(plain.arc(&plain.root).args(["doctor", "--json"]));
    assert!(
        doctor["advice"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["code"] == "gates-unconfined"),
        "{doctor}"
    );
    assert!(
        !doctor["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["code"] == "gates-unconfined"),
        "{doctor}"
    );
    let gates = json_stdout(repo.arc(&repo.root).args(["gates", "--json"]));
    assert_eq!(gates["confine"]["writes"], serde_json::json!(["target"]));
}

/// Claim I12: invalidating a stale slot identity is confined like warming it.
/// Rejects a holder without a project path executing invalidate with full authority.
#[test]
fn invalidation_cannot_write_outside_destination_roots() {
    if !supported() {
        return;
    }
    let (repo, scratch) = fixture();
    let decoy = scratch.path().join("invalidate-decoy");
    fs::write(&decoy, "retained").unwrap();
    repo.redeclare_gates(&declaration("true"));
    repo.commit(&repo.root, "identity", "one", "test: first identity");
    repo.commit(
        &repo.root,
        ".arc/worktrees.toml",
        &format!(
            "[slots]\nsize=1\nroot={:?}\nidentity='cat identity'\nwarm='true'\ninvalidate={:?}\n",
            repo.home.join("slots").display().to_string(),
            format!("printf escaped > '{}'", decoy.display())
        ),
        "test: declare invalidation",
    );
    stdout(repo.arc(&repo.root).args(["slots", "warm"]));
    repo.commit(&repo.root, "identity", "two", "test: change identity");
    let output = repo
        .arc(&repo.root)
        .args(["slots", "warm"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "unconfined invalidation succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
}

/// Claim I5: a landing's preparation answers to its destination even when its
/// head removes confinement. Rejects a scratch checkout trusting its own table.
#[test]
fn landing_preparation_cannot_drop_destination_confinement() {
    if !supported() {
        return;
    }
    let (repo, scratch) = fixture();
    let decoy = scratch.path().join("landing-decoy");
    fs::write(&decoy, "retained").unwrap();
    repo.redeclare_gates(&declaration("true"));
    let change = open(&repo);
    repo.commit(
        &repo.root,
        ".arc/gates.toml",
        "[gates.guard]\ncommand='true'\n",
        "test: remove head confinement",
    );
    repo.commit(
        &repo.root,
        ".arc/worktrees.toml",
        &format!(
            "[prepare]\nmode='path-stable'\ncommand={:?}\n",
            format!("printf escaped > '{}'", decoy.display())
        ),
        "test: preparation escape",
    );
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    stdout(
        repo.arc(&repo.root)
            .args(["review", &change, "--verdict", "approved"]),
    );
    let target = repo.home.join("target-checkout");
    git(
        &repo.root,
        &["worktree", "add", target.to_str().unwrap(), "master"],
    );
    repo.commit(
        &target,
        "target-only",
        "advance",
        "test: advance landing destination",
    );
    git(
        &repo.root,
        &["worktree", "remove", target.to_str().unwrap()],
    );
    let output = repo
        .arc(&repo.root)
        .args(["land", &change, "--debt", "fixture replay"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "landing ran unconfined: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
}

/// Claim: operator watch/events handlers retain operator authority rather than
/// project roots. Rejects applying project confinement to explicit --exec code.
#[test]
fn operator_event_and_watch_handlers_are_not_project_commands() {
    let (repo, scratch) = fixture();
    let decoy = scratch.path().join("operator-output");
    repo.redeclare_gates(&declaration("true"));
    let change = open(&repo);
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    let command = format!("cat >/dev/null; printf operator > '{}'", decoy.display());
    stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        &change,
        "--type",
        "patchset-added",
        "--exec",
        &command,
    ]));
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "operator");
    fs::remove_file(&decoy).unwrap();
    stdout(repo.arc(&repo.root).args([
        "watch",
        &change,
        "--until",
        "snapshot",
        "--timeout",
        "2",
        "--exec",
        &command,
    ]));
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "operator");
}

/// Claim I12: hooks fired by Git's own commit path are confined individually,
/// while Git retains authority to restore the checkout after a refused hook.
/// Rejects squash running a tracked pre-commit hook with operator write authority.
#[test]
fn git_commit_hooks_cannot_escape_through_squash() {
    if !supported() {
        return;
    }
    let (repo, scratch) = fixture();
    let decoy = scratch.path().join("commit-hook-decoy");
    fs::write(&decoy, "retained").unwrap();
    repo.redeclare_gates(&declaration("true"));
    let change = open(&repo);
    repo.commit(&repo.root, "feature", "one", "test: first feature");
    repo.commit(&repo.root, "feature", "two", "test: second feature");
    fs::create_dir(repo.root.join(".hooks")).unwrap();
    let hook = repo.root.join(".hooks/pre-commit");
    fs::write(
        &hook,
        format!("#!/bin/sh\nprintf escaped > '{}'\n", decoy.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(&repo.root, &["add", ".hooks"]);
    git(&repo.root, &["commit", "-m", "test: tracked hook"]);
    git(&repo.root, &["config", "core.hooksPath", ".hooks"]);
    let head = repo.head(&repo.root);
    let output = repo
        .arc(&repo.root)
        .args(["squash", &change, "--message", "test: squash"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "squash hook was unconfined");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("ran confined"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
    assert_eq!(repo.head(&repo.root), head);
}

/// Claim I5: measurements for open or closed changes use destination roots
/// even when the working table widens them. Rejects measurement/init treating a working declaration as authority.
#[test]
fn measurement_and_init_cannot_choose_working_tree_roots() {
    if !supported() {
        return;
    }
    for (operation, closed) in [
        ("measure", false),
        ("init", false),
        ("measure", true),
        ("init", true),
    ] {
        let (repo, scratch) = fixture();
        let other = scratch.path().join("measurement-other");
        fs::create_dir(&other).unwrap();
        let decoy = other.join("measurement-decoy");
        fs::write(&decoy, "retained").unwrap();
        let script = format!("printf escaped > '{}'", decoy.display());
        repo.redeclare_gates(&declaration(&script));
        let change = open(&repo);
        if closed {
            stdout(repo.arc(&repo.root).args(["close", &change, "--abandoned"]));
        }
        fs::write(
            repo.root.join(".arc/gates.toml"),
            format!(
                "[confine]\nwrites=[{:?}]\n[gates.guard]\ncommand={script:?}\n",
                other.display().to_string()
            ),
        )
        .unwrap();
        let output = repo
            .arc(&repo.root)
            .args(["gates", operation, "--runs", "1"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{operation} used working roots");
        assert_eq!(fs::read_to_string(&decoy).unwrap(), "retained");
    }
}

/// Claim I12: Git's native rebase checkout/rewrite hooks inherit destination
/// confinement. Rejects the fallback rebase command running hooks unconfined.
#[test]
fn git_rebase_hooks_cannot_escape_the_destination_scope() {
    if !supported() {
        return;
    }
    let (repo, scratch) = fixture();
    let decoy = scratch.path().join("rebase-hook-decoy");
    fs::write(&decoy, "retained").unwrap();
    repo.redeclare_gates(&declaration("true"));
    let change = open(&repo);
    repo.commit(&repo.root, "feature", "branch", "test: branch feature");
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    let target = repo.home.join("target-checkout");
    git(
        &repo.root,
        &["worktree", "add", target.to_str().unwrap(), "master"],
    );
    repo.commit(
        &target,
        "target-only",
        "advance",
        "test: advance rebase destination",
    );
    git(
        &repo.root,
        &["worktree", "remove", target.to_str().unwrap()],
    );
    use std::os::unix::fs::PermissionsExt;
    for name in ["post-checkout", "post-rewrite"] {
        let hook = repo.root.join(".git/hooks").join(name);
        fs::write(
            &hook,
            format!("#!/bin/sh\nprintf escaped > '{}'\n", decoy.display()),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = repo
        .arc(&repo.root)
        .env("PATH", path_without_git_replay(&repo))
        .args(["rebase", &change])
        .output()
        .unwrap();
    assert_eq!(
        fs::read_to_string(&decoy).unwrap(),
        "retained",
        "native rebase hook escaped"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("ran confined"),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
