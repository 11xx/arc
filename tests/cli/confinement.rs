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
