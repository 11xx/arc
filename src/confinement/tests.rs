use super::*;

fn supported() -> bool {
    match abi_version().and_then(check_abi) {
        Ok(_) => true,
        Err(error) => {
            eprintln!("SKIP real confinement: {error:#}");
            false
        }
    }
}

fn scratch() -> tempfile::TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/confinement-fixtures");
    fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

fn shell(checkout: &Path, script: &str, writes: &[&str]) -> (Command, Receipt) {
    let mut command = crate::child_command::command("sh");
    command.args(["-c", script]).current_dir(checkout);
    let receipt = apply_declared(
        &mut command,
        checkout,
        &Declaration {
            writes: writes.iter().map(|s| s.to_string()).collect(),
            required: true,
        },
    )
    .unwrap()
    .unwrap();
    (command, receipt)
}

/// Claim I12: a project cannot write, truncate, remove, rename, or configure a
/// decoy outside its roots, including through grandchildren or symlinks.
/// Rejects a missing pre-exec restriction or incomplete handled write rights.
#[test]
fn outside_writes_and_descendants_are_denied() {
    if !supported() {
        return;
    }
    let scratch = scratch();
    let checkout = scratch.path().join("checkout");
    let decoy = scratch.path().join("decoy");
    fs::create_dir_all(&checkout).unwrap();
    fs::create_dir_all(&decoy).unwrap();
    let config = decoy.join("config");
    fs::write(&config, "retained").unwrap();
    let mut git = crate::child_command::command("git");
    assert!(git
        .args(["init", "-q"])
        .current_dir(&decoy)
        .status()
        .unwrap()
        .success());
    let git_config = decoy.join(".git/config");
    let before = fs::read(&git_config).unwrap();
    std::os::unix::fs::symlink(&decoy, checkout.join("target-link")).unwrap();
    let path = crate::context::shell_quote(&config.display().to_string());
    let git_path = crate::context::shell_quote(&decoy.display().to_string());
    for script in [
        format!("printf overwritten > {path}"),
        format!("python3 -c 'import os,sys; os.truncate(sys.argv[1],0)' {path}"),
        format!("rm {path}"),
        format!("mv {path} target/moved"),
        format!("git -C {git_path} config guard.changed true"),
        format!(
            "sh -c {}",
            crate::context::shell_quote(&format!(
                "sh -c {}",
                crate::context::shell_quote(&format!("printf overwritten > {path}"))
            ))
        ),
        "printf overwritten > target-link/config".into(),
        "printf source > source-file".into(),
    ] {
        let (mut command, receipt) = shell(&checkout, &script, &["target"]);
        let output = command.output().unwrap();
        assert!(!output.status.success(), "write escaped: {script}");
        assert!(receipt.confined);
        assert_eq!(fs::read_to_string(&config).unwrap(), "retained", "{script}");
        assert_eq!(fs::read(&git_config).unwrap(), before, "{script}");
        assert!(!checkout.join("source-file").exists());
    }
}

/// Claim: allowed roots support ordinary build writes and reading elsewhere;
/// stacking a second ruleset cannot restore the first one's denied writes.
/// Rejects read confinement or widening an inherited domain.
#[test]
fn allowed_writes_reads_and_stacked_rulesets_compose() {
    if !supported() {
        return;
    }
    let scratch = scratch();
    fs::write(scratch.path().join("source"), "readable").unwrap();
    let (mut command, receipt) = shell(scratch.path(), "mkdir target/sub && cat source > target/sub/file && mv target/sub/file target/file && truncate -s 2 target/file && rm target/file && rmdir target/sub", &["target"]);
    assert!(command.status().unwrap().success());
    assert!(receipt.identity.unwrap().starts_with("sha256:"));
    let (mut command, _) = shell(scratch.path(), "printf forbidden > source", &["target"]);
    apply_declared(
        &mut command,
        scratch.path(),
        &Declaration {
            writes: vec![".".into()],
            required: true,
        },
    )
    .unwrap();
    assert!(!command.status().unwrap().success());
    assert_eq!(
        fs::read_to_string(scratch.path().join("source")).unwrap(),
        "readable"
    );
}

/// Claim I12: unavailable confinement and kernels without truncation protection
/// refuse before execution; only required=false permits a labelled fallback.
/// Rejects silent unconfined execution when the minimum ABI is missing.
#[test]
fn unavailable_and_incomplete_kernels_refuse_unless_optional() {
    let scratch = scratch();
    let declaration = Declaration {
        writes: vec!["target".into()],
        required: true,
    };
    for abi in [Err(anyhow::anyhow!("Landlock unavailable: fixture")), Ok(2)] {
        let mut command = crate::child_command::command("sh");
        command
            .args(["-c", "touch marker"])
            .current_dir(scratch.path());
        let error = apply_with_abi(&mut command, scratch.path(), &declaration, abi).unwrap_err();
        assert!(format!("{error:#}").contains("refusing project command"));
        assert!(!scratch.path().join("marker").exists());
    }
    let mut command = crate::child_command::command("true");
    let receipt = apply_with_abi(
        &mut command,
        scratch.path(),
        &Declaration {
            required: false,
            ..declaration
        },
        Ok(2),
    )
    .unwrap()
    .unwrap();
    assert!(!receipt.confined);
    assert!(receipt.unavailable.unwrap().contains("TRUNCATE"));
    assert!(command.status().unwrap().success());
}

/// Claim: absent external cache roots are not created, while paths inside the
/// checkout are created and environment and home expansions select exact roots.
/// Rejects creating an operator directory merely to satisfy a project declaration.
#[test]
fn roots_expand_without_creating_external_paths() {
    if !supported() {
        return;
    }
    let scratch = scratch();
    let checkout = scratch.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    let outside = scratch.path().join("absent");
    let mut command = crate::child_command::command("true");
    let error = apply_declared(
        &mut command,
        &checkout,
        &Declaration {
            writes: vec![outside.display().to_string()],
            required: true,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("external roots must exist"));
    assert!(!outside.exists());
    assert_eq!(
        expand("${HOME}/.cargo").unwrap(),
        PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cargo")
    );
    assert_eq!(expand("~/.cargo").unwrap(), expand("$HOME/.cargo").unwrap());
    let (_, receipt) = shell(&checkout, "true", &["target/nested"]);
    assert!(receipt.writes.contains(&checkout.join("target/nested")));
}

/// Claim I5: a head cannot expand destination roots or drop required confinement,
/// and adopting confinement invalidates an unconfined declaration identity.
/// Rejects reusing an unconfined pass or merging head paths into destination roots.
#[test]
fn destination_roots_and_declaration_identity_are_binding() {
    let parse = |text: &str| toml::from_str::<crate::gates::GatesFile>(text).unwrap();
    let gate = "[gates.check]\ncommand='true'\n";
    let mut destination = parse(&format!("[confine]\nwrites=['target']\n{gate}"));
    let mut unconfined = parse(gate);
    let before = crate::gates::declaration_identity(&unconfined.gates["check"]);
    unconfined.owe_also(
        parse(&format!("[confine]\nwrites=['target']\n{gate}")),
        "target",
        "head",
    );
    assert_ne!(
        before,
        crate::gates::declaration_identity(&unconfined.gates["check"])
    );
    destination.owe_also(
        parse(&format!("[confine]\nwrites=['.']\nrequired=false\n{gate}")),
        "target",
        "head",
    );
    assert_eq!(destination.confine.as_ref().unwrap().writes, ["target"]);
    assert!(destination.confine.as_ref().unwrap().required);
    assert!(
        destination.gates["check"]
            .confine
            .as_ref()
            .unwrap()
            .required
    );
    assert!(crate::gates::coverage_identity(&destination.gates["check"]).is_some());
    let mut project = destination.confine.clone();
    combine(
        &mut project,
        Some(Declaration {
            writes: vec!["/cache".into()],
            required: false,
        }),
    );
    assert_eq!(project.unwrap().writes, ["/cache", "target"]);
}
