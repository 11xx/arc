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

/// Claim: preparation, environment probes, selectors and the changelog
/// renderer share gate confinement. Rejects any project runner that
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

/// Claim I5: detached and unowned checkouts answer to the primary change's
/// destination, never that change's head. Rejects removing confinement in place
/// and using a free checkout to run an unrestricted measurement.
#[test]
fn free_checkouts_cannot_take_an_in_place_change_heads_authority() {
    if !supported() {
        return;
    }
    for detached in [true, false] {
        let (repo, scratch) = fixture();
        let decoy = scratch.path().join("free-checkout-decoy");
        fs::write(&decoy, "retained").unwrap();
        let script = format!("printf escaped > '{}'", decoy.display());
        repo.redeclare_gates(&declaration(&script));
        open(&repo);
        repo.commit(
            &repo.root,
            ".arc/gates.toml",
            &format!("[gates.guard]\ncommand={script:?}\n"),
            "test: remove confinement at change head",
        );
        let checkout = scratch.path().join("free");
        let mut args = vec!["worktree", "add"];
        if detached {
            args.push("--detach");
        } else {
            args.extend(["-b", "unowned"]);
        }
        args.extend([checkout.to_str().unwrap(), "HEAD"]);
        git(&repo.root, &args);
        let output = repo
            .arc(&checkout)
            .args(["gates", "measure", "--runs", "1"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "free checkout ran unconfined");
        assert_eq!(
            fs::read_to_string(decoy).unwrap(),
            "retained",
            "free checkout selected change roots"
        );
    }
}

/// Claim I5: a free detached checkout without a recorded destination refuses
/// measurement and names a usable invocation. Rejects guessing a primary head.
#[test]
fn an_unowned_detached_checkout_requires_a_destination() {
    let (repo, scratch) = fixture();
    repo.redeclare_gates(&declaration("true"));
    let checkout = scratch.path().join("unowned-detached");
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "--detach",
            checkout.to_str().unwrap(),
            "HEAD",
        ],
    );
    let output = repo
        .arc(&checkout)
        .args(["gates", "measure", "--runs", "1"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "measurement guessed a detached destination"
    );
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostic.contains("no established confinement destination")
            && diagnostic.contains("destination checkout"),
        "{diagnostic}"
    );
}

/// Claim: a confined gate's receipt carries the instructions its command
/// retired, as an unconfined gate's does on the same host, so the busy-host
/// rule judges confined projects by their work. Rejects confinement installed
/// where the command holder's instruction counter cannot attach.
#[test]
fn a_confined_measured_gate_still_records_instructions() {
    if !supported() {
        return;
    }
    let mut recorded = Vec::new();
    for confine in ["", "[confine]\nwrites=['target']\n"] {
        let (repo, _scratch) = fixture();
        repo.redeclare_gates(&format!("{confine}[gates.guard]\ncommand='true'\n"));
        let change = open(&repo);
        repo.commit(&repo.root, "feature", "one", "test: measured content");
        stdout(repo.arc(&repo.root).args(["snapshot", &change]));
        stdout(repo.arc(&repo.root).args(["verify", &change, "--all"]));
        let state = json_stdout(repo.arc(&repo.root).args(["show", &change, "--json"]));
        recorded.push(
            state["verifications"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .clone(),
        );
    }
    let [unconfined, confined] = &recorded[..] else {
        unreachable!()
    };
    if unconfined["instructions"].is_null() {
        eprintln!("SKIP instruction counting: this host records none for an unconfined gate");
        return;
    }
    assert_eq!(confined["confinement"]["confined"], true, "{confined}");
    assert!(
        confined["instructions"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "confined gate recorded no instructions: {confined}"
    );
}

/// Claim: `arc begin --worktree` with path-stable Cargo preparation under write
/// confinement leaves the fresh worktree's `target/` marked with `CACHEDIR.TAG`
/// while keeping the checkout clean. Rejects pre-creating `target/` unmarked so
/// Cargo skips writing `CACHEDIR.TAG`.
#[test]
fn path_stable_cargo_preparation_marks_confined_target_directory() {
    if !supported() {
        return;
    }
    let (repo, scratch) = fixture();
    let cargo_home = scratch.path().join("cargo-home");
    fs::create_dir_all(cargo_home.join("registry")).unwrap();
    fs::create_dir_all(cargo_home.join("git")).unwrap();
    fs::write(cargo_home.join(".package-cache"), "").unwrap();
    fs::write(cargo_home.join(".package-cache-mutate"), "").unwrap();
    fs::create_dir_all(repo.root.join("src")).unwrap();
    fs::create_dir_all(repo.root.join("tests")).unwrap();
    fs::write(repo.root.join(".gitignore"), "/target\n").unwrap();
    fs::write(
        repo.root.join("Cargo.toml"),
        "[package]\nname='confined-cargo'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(
        repo.root.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = 'confined-cargo'\nversion = '0.1.0'\n",
    )
    .unwrap();
    fs::write(
        repo.root.join("src/lib.rs"),
        "pub fn ok() -> bool { true }\n",
    )
    .unwrap();
    fs::write(
        repo.root.join("tests/it.rs"),
        "#[test]\nfn passes() { assert!(confined_cargo::ok()); }\n",
    )
    .unwrap();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!(
            "[confine]\nwrites = [\n  \"target\",\n  \"{home}/registry\",\n  \"{home}/git\",\n  \"{home}/.package-cache\",\n  \"{home}/.package-cache-mutate\",\n]\n[gates.check]\ncommand = \"cargo test --offline --locked\"\n",
            home = cargo_home.display()
        ),
    )
    .unwrap();
    fs::write(
        repo.root.join(".arc/worktrees.toml"),
        "[prepare]\nmode = \"path-stable\"\ncommand = \"cargo test --offline --locked --profile dev --no-run --manifest-path \\\"$ARC_WORKTREE_PATH/Cargo.toml\\\"\"\ntimeout = \"2m\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", "."]);
    git(
        &repo.root,
        &[
            "commit",
            "-m",
            "test: confined path-stable cargo preparation",
        ],
    );

    let worktree = scratch.path().join("prepared-wt");
    repo.arc(&repo.root)
        .env("TMPDIR", "/tmp")
        .env("CARGO_HOME", &cargo_home)
        .args([
            "begin",
            "prepared-cargo",
            "--worktree",
            worktree.to_str().unwrap(),
        ])
        .assert()
        .success();

    let tag = worktree.join("target/CACHEDIR.TAG");
    assert!(
        fs::symlink_metadata(&tag).is_ok_and(|meta| meta.file_type().is_file()),
        "fresh worktree target/ is missing CACHEDIR.TAG after confined path-stable preparation"
    );
    assert!(
        fs::read(&tag)
            .unwrap()
            .starts_with(b"Signature: 8a477f597d28d172789f06886806bc55"),
        "CACHEDIR.TAG does not carry the standard cache directory signature"
    );
    assert!(worktree.join("target/debug").is_dir());
    assert_eq!(git_out(&worktree, &["status", "--porcelain"]), "");

    // Fresh tagged target creation also succeeds under umask 0477 (mode 0300 directories)
    // so confined preparation runs and writes inside the freshly tagged `target/` root.
    let (umask_repo, umask_scratch) = fixture();
    fs::write(umask_repo.root.join(".gitignore"), "/target\n").unwrap();
    fs::write(
        umask_repo.root.join("Cargo.toml"),
        "[package]\nname='confined-umask'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::create_dir_all(umask_repo.root.join(".arc")).unwrap();
    fs::write(
        umask_repo.root.join(".arc/gates.toml"),
        "[confine]\nwrites = [\"target\"]\n[gates.check]\ncommand = \"true\"\n",
    )
    .unwrap();
    fs::write(
        umask_repo.root.join(".arc/worktrees.toml"),
        "[prepare]\nmode = \"path-stable\"\ncommand = \"printf ran > \\\"$ARC_WORKTREE_PATH/target/prep-ran\\\"\"\ntimeout = \"1m\"\n",
    )
    .unwrap();
    git(&umask_repo.root, &["add", "."]);
    git(&umask_repo.root, &["commit", "-m", "test: umask 0477 prep"]);
    // Create the worktree under a normal umask, then remove `target/` and run
    // `arc verify --all` under `umask 0477` so staging and `target/` inherit mode 0300.
    // Confined preparation must stage, publish `target/CACHEDIR.TAG`, and execute
    // its command (writing `target/prep-ran`) instead of failing on `target` before start.
    let umask_wt = umask_scratch.path().join("prepared-umask-wt");
    umask_repo
        .arc(&umask_repo.root)
        .env("TMPDIR", "/tmp")
        .args([
            "begin",
            "prepared-umask",
            "--worktree",
            umask_wt.to_str().unwrap(),
        ])
        .assert()
        .success();
    fs::remove_dir_all(umask_wt.join("target")).unwrap();
    let umask_out = umask_repo
        .arc_under_umask(&umask_wt, "0477")
        .env("TMPDIR", "/tmp")
        .args(["verify", "--all"])
        .output()
        .unwrap();
    let umask_stderr = String::from_utf8_lossy(&umask_out.stderr);
    assert!(
        umask_stderr.contains("worktree preparation: path-stable storage")
            && umask_stderr.contains("running: true"),
        "preparation did not succeed under umask 0477:\n{umask_stderr}"
    );
    let umask_tag = umask_wt.join("target/CACHEDIR.TAG");
    let umask_prep = umask_wt.join("target/prep-ran");
    assert!(
        fs::symlink_metadata(&umask_tag).is_ok_and(|m| m.file_type().is_file()),
        "fresh target/CACHEDIR.TAG missing under umask 0477"
    );
    assert!(
        fs::symlink_metadata(&umask_prep).is_ok_and(|m| m.file_type().is_file()),
        "confined preparation command did not write target/prep-ran under umask 0477"
    );
    // Restore permissions so signature inspection and TempDir cleanup succeed.
    let _ = Command::new("chmod")
        .args(["-R", "u+rwX", "--"])
        .arg(umask_scratch.path())
        .status();
    assert!(
        fs::read(&umask_tag)
            .unwrap()
            .starts_with(b"Signature: 8a477f597d28d172789f06886806bc55"),
        "CACHEDIR.TAG created under umask 0477 missing standard signature"
    );
    assert_eq!(fs::read_to_string(&umask_prep).unwrap(), "ran");

    // Same-device mount boundary (bind-mounted `<git-common-dir>/arc`) produces
    // EXDEV on renameat2; confinement cleans up staging and falls back to untagged
    // target creation without aborting preparation.
    let unshare_available = Command::new("unshare")
        .args(["-rm", "true"])
        .status()
        .is_ok_and(|s| s.success());
    if unshare_available {
        let (exdev_repo, exdev_scratch) = fixture();
        fs::write(exdev_repo.root.join(".gitignore"), "/target\n").unwrap();
        fs::write(
            exdev_repo.root.join("Cargo.toml"),
            "[package]\nname='confined-exdev'\nversion='0.1.0'\nedition='2021'\n",
        )
        .unwrap();
        fs::create_dir_all(exdev_repo.root.join(".arc")).unwrap();
        fs::write(
            exdev_repo.root.join(".arc/gates.toml"),
            "[confine]\nwrites = [\"target\"]\n[gates.check]\ncommand = \"true\"\n",
        )
        .unwrap();
        fs::write(
            exdev_repo.root.join(".arc/worktrees.toml"),
            "[prepare]\nmode = \"path-stable\"\ncommand = \"true\"\ntimeout = \"1m\"\n",
        )
        .unwrap();
        git(&exdev_repo.root, &["add", "."]);
        git(
            &exdev_repo.root,
            &["commit", "-m", "test: exdev mount prep"],
        );
        let exdev_wt = exdev_scratch.path().join("prepared-exdev-wt");
        exdev_repo
            .arc(&exdev_repo.root)
            .env("TMPDIR", "/tmp")
            .args([
                "begin",
                "prepared-exdev",
                "--worktree",
                exdev_wt.to_str().unwrap(),
            ])
            .assert()
            .success();
        fs::remove_dir_all(exdev_wt.join("target")).unwrap();
        let arc_dir = exdev_repo.root.join(".git/arc");
        let arc_bin = assert_cmd::cargo::cargo_bin!("arc");
        let mut cmd = Command::new("unshare");
        cmd.args([
            "-rm",
            "sh",
            "-c",
            "mount --bind \"$1\" \"$1\" && shift && exec \"$@\"",
            "_",
        ])
        .arg(&arc_dir)
        .arg(arc_bin)
        .args(["verify", "--all"])
        .current_dir(&exdev_wt)
        .env("HOME", &exdev_repo.home)
        .env("XDG_CONFIG_HOME", exdev_repo.home.join(".config"))
        .env("TMPDIR", "/tmp")
        .env("ARC_ACTOR", "tester")
        .env("ARC_HARNESS", "test")
        .env("ARC_SESSION", "session-a")
        .env("ARC_SANDBOX", &exdev_repo.home)
        .env("ARC_TEST_FIXTURE_IDENTITY", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true");
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "verify failed across same-device bind mount:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(exdev_wt.join("target").is_dir());
        assert!(!exdev_wt.join("target/CACHEDIR.TAG").exists());
        let leftovers: Vec<_> = fs::read_dir(&arc_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".arc-target-stage-")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "leftover stage dirs after EXDEV: {leftovers:?}"
        );
    }
}
