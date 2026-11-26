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
    fs::create_dir(scratch.path().join("wider")).unwrap();
    fs::write(scratch.path().join("wider/source"), "readable").unwrap();
    let (mut command, _) = shell(
        scratch.path(),
        "printf forbidden > wider/source",
        &["target"],
    );
    apply_declared(
        &mut command,
        scratch.path(),
        &Declaration {
            writes: vec!["target".into(), "wider".into()],
            required: true,
        },
    )
    .unwrap();
    assert!(!command.status().unwrap().success());
    assert_eq!(
        fs::read_to_string(scratch.path().join("wider/source")).unwrap(),
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

/// Claim I5: a caller without destination authority refuses before choosing any
/// writable roots. Rejects a detached checkout selecting its own declaration.
#[test]
fn unscoped_commands_refuse_the_checkout_declaration() {
    let scratch = scratch();
    fs::create_dir(scratch.path().join(".arc")).unwrap();
    fs::write(
        scratch.path().join(".arc/gates.toml"),
        "[confine]\nwrites=['target']\n",
    )
    .unwrap();
    assert!(crate::child_command::command("git")
        .args(["init", "-q"])
        .current_dir(scratch.path())
        .status()
        .unwrap()
        .success());
    let mut command = crate::child_command::command("true");
    let result = apply(&mut command, scratch.path());
    assert!(
        result.is_err(),
        "unscoped command accepted checkout declaration: {result:?}"
    );
}

/// Claim I12: project-selected build directories never acquire implicit write
/// authority. Rejects trusting ARC_WORKTREE_BUILD as proof of arc ownership.
#[test]
fn project_selected_storage_is_not_an_implicit_root() {
    if !supported() {
        return;
    }
    let scratch = scratch();
    let checkout = scratch.path().join("checkout");
    let decoy = scratch.path().join("decoy");
    fs::create_dir(&checkout).unwrap();
    fs::create_dir(&decoy).unwrap();
    let mut command = crate::child_command::command("true");
    command.env("ARC_WORKTREE_BUILD", &decoy);
    let result = apply_declared(
        &mut command,
        &checkout,
        &Declaration {
            writes: vec!["target".into()],
            required: true,
        },
    );
    assert!(
        result.is_err(),
        "project-selected storage became writable: {result:?}"
    );
}

/// Claim I5: relative writable roots cannot escape the checkout by symlink or
/// parent traversal. Rejects an evaluated head redirecting a destination root.
#[test]
fn relative_roots_cannot_follow_symlinks_or_parents_outside() {
    if !supported() {
        return;
    }
    let scratch = scratch();
    let checkout = scratch.path().join("checkout");
    let decoy = scratch.path().join("decoy");
    fs::create_dir(&checkout).unwrap();
    fs::create_dir(&decoy).unwrap();
    std::os::unix::fs::symlink(&decoy, checkout.join("target")).unwrap();
    for root in ["target", "../decoy"] {
        let mut command = crate::child_command::command("true");
        let result = apply_declared(
            &mut command,
            &checkout,
            &Declaration {
                writes: vec![root.into()],
                required: true,
            },
        );
        assert!(result.is_err(), "relative root escaped: {root}: {result:?}");
    }
}

/// Claim I12: a declaration cannot grant the whole checkout, home, or a path
/// above either. Rejects roots that erase the confinement boundary.
#[test]
fn broad_roots_and_tmpdir_do_not_erase_the_boundary() {
    if !supported() {
        return;
    }
    let scratch = scratch();
    let checkout = scratch.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    assert!(crate::child_command::command("git")
        .args(["init", "--template=", "-q"])
        .current_dir(&checkout)
        .status()
        .unwrap()
        .success());
    for root in [
        ".".to_string(),
        checkout.display().to_string(),
        "~".into(),
        "/".into(),
        checkout.join(".git/arc").display().to_string(),
        checkout.join(".git/hooks").display().to_string(),
        checkout.join(".git/objects").display().to_string(),
    ] {
        let mut command = crate::child_command::command("true");
        let result = apply_declared(
            &mut command,
            &checkout,
            &Declaration {
                writes: vec![root.clone()],
                required: true,
            },
        );
        assert!(
            result.is_err(),
            "overbroad root accepted: {root}: {result:?}"
        );
    }
    let mut command = crate::child_command::command("true");
    command.env("TMPDIR", &checkout);
    let result = apply_declared(
        &mut command,
        &checkout,
        &Declaration {
            writes: vec!["target".into()],
            required: true,
        },
    );
    assert!(result.is_err(), "TMPDIR granted checkout: {result:?}");
}

/// Claim: cache declarations can use Cargo's default home without granting
/// installed executables or Cargo configuration. Rejects the entire Cargo home.
#[test]
fn cargo_cache_roots_exclude_tools_and_support_an_unset_home_variable() {
    let text = include_str!("../../.arc/gates.toml");
    let declared: crate::gates::GatesFile = toml::from_str(text).unwrap();
    let writes = declared.confine.unwrap().writes;
    assert!(
        !writes
            .iter()
            .any(|path| path == "$CARGO_HOME" || path == "${CARGO_HOME}" || path == "~/.cargo"),
        "Cargo home grants installed tools: {writes:?}"
    );
    assert_eq!(
        expand("${ARC_CONFINE_TEST_ABSENT:-~/.cargo}/registry").unwrap(),
        expand("~/.cargo/registry").unwrap()
    );
}

/// Claim: a declared directory under /dev supports creation and removal just
/// like another directory. Rejects stripping directory rights from /dev/shm.
#[test]
fn declared_shared_memory_directory_supports_file_creation() {
    if !supported() {
        return;
    }
    let shared = match tempfile::tempdir_in("/dev/shm") {
        Ok(scratch) => scratch,
        Err(error) => {
            let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("SKIP shared-memory witness: enclosing policy cannot create /dev/shm fixture: {error}\n"));
            return;
        }
    };
    let checkout = scratch();
    let root = shared.path().to_str().unwrap();
    let script = format!("printf shared > '{root}/file' && rm '{root}/file'");
    let (mut command, _) = shell(checkout.path(), &script, &[root]);
    assert!(
        command.status().unwrap().success(),
        "declared shared-memory directory denied creation"
    );
}

/// Claim: switching branches does not evaluate a malformed confinement
/// declaration. Rejects policy validation before any project command.
#[test]
fn a_switch_does_not_evaluate_confinement() {
    let scratch = scratch();
    let cwd = scratch.path();
    let git = |args: &[&str]| {
        let status = crate::child_command::command("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.org",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "-q", "-b", "master"]);
    fs::create_dir(cwd.join(".arc")).unwrap();
    fs::write(cwd.join(".arc/gates.toml"), "[confine]\nwrites = broken\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "test: malformed declaration"]);
    git(&["branch", "side"]);
    assert!(
        crate::gitio::git(cwd, &["switch", "side"]).is_ok(),
        "switch evaluated confinement"
    );
}

/// Claim: a temporary root beneath home is refused as an overlap, not described
/// as an ancestor. Rejects a diagnostic that misidentifies the denied path.
#[test]
fn home_temporary_roots_name_the_actual_overlap() {
    let result = validate_root(
        Path::new("/fixture/home/cache/tmp"),
        Path::new("/fixture/checkout"),
        Path::new("/fixture/home"),
        None,
        &[],
        true,
    )
    .unwrap_err()
    .to_string();
    assert!(
        result.contains("TMPDIR") && result.contains("overlaps the home directory"),
        "{result}"
    );
    assert!(!result.contains("top level or an ancestor"), "{result}");
}

/// Claim: creating an absent ignored Cargo `target` directory for confinement
/// writes a standard `CACHEDIR.TAG` without marking pre-existing, unignored, or
/// non-Cargo directories. Rejects pre-creating `target/` unmarked before Cargo.
#[test]
fn cargo_target_roots_created_by_confinement_get_cachedir_tag() {
    if !supported() {
        return;
    }
    let init_git = |cwd: &Path| {
        let status = crate::child_command::command("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.org",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "init",
                "-q",
                "-b",
                "master",
            ])
            .current_dir(cwd)
            .status()
            .unwrap();
        assert!(status.success());
    };

    // Absent ignored Cargo target gets CACHEDIR.TAG and leaves git status clean.
    let scratch_marked = scratch();
    let checkout = scratch_marked.path().join("marked");
    fs::create_dir(&checkout).unwrap();
    init_git(&checkout);
    fs::write(checkout.join(".gitignore"), "/target\n").unwrap();
    fs::write(
        checkout.join("Cargo.toml"),
        "[package]\nname='pkg'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    let (mut command, _) = shell(&checkout, "true", &["target"]);
    assert!(command.status().unwrap().success());
    let tag = checkout.join("target/CACHEDIR.TAG");
    assert!(
        fs::symlink_metadata(&tag).is_ok_and(|meta| meta.file_type().is_file()),
        "expected regular CACHEDIR.TAG at {}",
        tag.display()
    );
    assert!(fs::read(&tag)
        .unwrap()
        .starts_with(crate::artifacts::CACHE_SIGNATURE));
    let status = crate::gitio::git(&checkout, &["status", "--porcelain"]).unwrap();
    assert!(!status.contains("target"), "{status}");

    // Nested root under an absent Cargo target marks `target/`, not the child.
    let scratch_nested = scratch();
    let nested = scratch_nested.path().join("nested");
    fs::create_dir(&nested).unwrap();
    init_git(&nested);
    fs::write(nested.join(".gitignore"), "target/\n").unwrap();
    fs::write(
        nested.join("Cargo.toml"),
        "[package]\nname='pkg'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    let (mut command, _) = shell(&nested, "true", &["target/nested"]);
    assert!(command.status().unwrap().success());
    assert!(nested.join("target/CACHEDIR.TAG").is_file());
    assert!(!nested.join("target/nested/CACHEDIR.TAG").exists());

    // Unignored Cargo target is not marked, preserving untracked cleanliness.
    let scratch_unignored = scratch();
    let unignored = scratch_unignored.path().join("unignored");
    fs::create_dir(&unignored).unwrap();
    init_git(&unignored);
    fs::write(
        unignored.join("Cargo.toml"),
        "[package]\nname='pkg'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    let (mut command, _) = shell(&unignored, "true", &["target"]);
    assert!(command.status().unwrap().success());
    assert!(!unignored.join("target/CACHEDIR.TAG").exists());

    // Non-Cargo checkout is not marked.
    let scratch_plain = scratch();
    let plain = scratch_plain.path().join("plain");
    fs::create_dir(&plain).unwrap();
    init_git(&plain);
    fs::write(plain.join(".gitignore"), "/target\n").unwrap();
    let (mut command, _) = shell(&plain, "true", &["target"]);
    assert!(command.status().unwrap().success());
    assert!(!plain.join("target/CACHEDIR.TAG").exists());

    // Pre-existing unmarked target directory is left untouched.
    let scratch_existing = scratch();
    let existing = scratch_existing.path().join("existing");
    fs::create_dir_all(existing.join("target")).unwrap();
    init_git(&existing);
    fs::write(existing.join(".gitignore"), "/target\n").unwrap();
    fs::write(
        existing.join("Cargo.toml"),
        "[package]\nname='pkg'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    let (mut command, _) = shell(&existing, "true", &["target"]);
    assert!(command.status().unwrap().success());
    assert!(!existing.join("target/CACHEDIR.TAG").exists());

    // Workspace member `member/target` is not Cargo's workspace target root and stays unmarked.
    let scratch_ws = scratch();
    let ws = scratch_ws.path().join("workspace");
    fs::create_dir_all(ws.join("member")).unwrap();
    init_git(&ws);
    fs::write(ws.join(".gitignore"), "target/\n").unwrap();
    fs::write(
        ws.join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\"]\n",
    )
    .unwrap();
    fs::write(
        ws.join("member/Cargo.toml"),
        "[package]\nname='member'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    let (mut command, _) = shell(&ws, "true", &["member/target"]);
    assert!(command.status().unwrap().success());
    assert!(ws.join("member/target").is_dir());
    assert!(!ws.join("member/target/CACHEDIR.TAG").exists());

    // Literal pathspec-magic directory names work without aborting Git ignore checks.
    let scratch_magic = scratch();
    let magic = scratch_magic.path().join("magic");
    fs::create_dir(&magic).unwrap();
    init_git(&magic);
    fs::write(magic.join(".gitignore"), ":(glob)/\n").unwrap();
    assert!(is_git_ignored(&magic, Path::new(":(glob)/target/CACHEDIR.TAG")).unwrap());
    let (mut command, _) = shell(&magic, "true", &[":(glob)/sub"]);
    assert!(command.status().unwrap().success());
    assert!(magic.join(":(glob)/sub").is_dir());

    // Marker publication via renameat2(RENAME_NOREPLACE) stages under common_dir/arc
    // when available, leaves an existing real directory untouched, and cleans up staging.
    let scratch_pub = scratch();
    let pub_checkout = scratch_pub.path().join("pub-existing");
    fs::create_dir_all(pub_checkout.join("target")).unwrap();
    init_git(&pub_checkout);
    let pub_common = crate::gitio::common_dir(&pub_checkout).unwrap();
    let pub_fd = open_dir_path(&pub_checkout).unwrap();
    let target_name = CString::new("target").unwrap();
    publish_tagged_target_at(
        &pub_fd,
        Some(&pub_common),
        &target_name,
        &pub_checkout.join("target"),
    )
    .unwrap();
    assert!(!pub_checkout.join("target/CACHEDIR.TAG").exists());
    for dir in [&pub_checkout, &pub_common.join("arc")] {
        if dir.is_dir() {
            let leftovers: Vec<_> = fs::read_dir(dir)
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
                "leftover stage dirs in {}: {leftovers:?}",
                dir.display()
            );
        }
    }

    // Legacy `.cargo/config` takes precedence over `.cargo/config.toml` just as Cargo does,
    // both when `config` overrides `target-dir` and when `config` omits it (ignoring `config.toml`).
    let scratch_prec = scratch();
    let prec = scratch_prec.path().join("precedence");
    fs::create_dir_all(prec.join(".cargo")).unwrap();
    init_git(&prec);
    fs::write(prec.join(".gitignore"), "/target\n/out\n").unwrap();
    fs::write(
        prec.join("Cargo.toml"),
        "[package]\nname='pkg'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(
        prec.join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"target\"\n",
    )
    .unwrap();
    fs::write(
        prec.join(".cargo/config"),
        "[build]\ntarget-dir = \"out\"\n",
    )
    .unwrap();
    let (mut command, _) = shell(&prec, "true", &["target"]);
    assert!(command.status().unwrap().success());
    assert!(prec.join("target").is_dir());
    assert!(!prec.join("target/CACHEDIR.TAG").exists());

    let scratch_prec_shadow = scratch();
    let prec_shadow = scratch_prec_shadow.path().join("precedence-shadow");
    fs::create_dir_all(prec_shadow.join(".cargo")).unwrap();
    init_git(&prec_shadow);
    fs::write(prec_shadow.join(".gitignore"), "/target\n/out\n").unwrap();
    fs::write(
        prec_shadow.join("Cargo.toml"),
        "[package]\nname='pkg'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(
        prec_shadow.join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"out\"\n",
    )
    .unwrap();
    fs::write(prec_shadow.join(".cargo/config"), "[term]\nquiet = true\n").unwrap();
    let (mut command, _) = shell(&prec_shadow, "true", &["target"]);
    assert!(command.status().unwrap().success());
    assert!(prec_shadow.join("target/CACHEDIR.TAG").is_file());

    // Configured Cargo build.target-dir override prevents tagging `target/`, and
    // arc-written confinement markers never authorize `artifacts::recognize` cleanup.
    let scratch_override = scratch();
    let overridden = scratch_override.path().join("overridden");
    fs::create_dir_all(overridden.join(".cargo")).unwrap();
    init_git(&overridden);
    fs::write(overridden.join(".gitignore"), "/target\n/cargo-out\n").unwrap();
    fs::write(
        overridden.join("Cargo.toml"),
        "[package]\nname='pkg'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(
        overridden.join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"cargo-out\"\n",
    )
    .unwrap();
    let (mut command, _) = shell(&overridden, "true", &["target"]);
    assert!(command.status().unwrap().success());
    assert!(overridden.join("target").is_dir());
    assert!(!overridden.join("target/CACHEDIR.TAG").exists());
    assert!(
        crate::artifacts::recognize(&checkout, &["!! target/".to_string()], &[]).is_empty(),
        "confinement-written CACHEDIR.TAG must not authorize artifact cleanup alone"
    );

    // Write+search-only (0300) parent directory stays traversable when creating nested roots.
    use std::os::unix::fs::PermissionsExt;
    let scratch_mode = scratch();
    let mode_checkout = scratch_mode.path().join("mode-0300");
    fs::create_dir_all(mode_checkout.join("opaque")).unwrap();
    fs::set_permissions(
        mode_checkout.join("opaque"),
        fs::Permissions::from_mode(0o300),
    )
    .unwrap();
    let mode_cmd = Command::new("true");
    let res = create_checkout_root(
        &mode_cmd,
        &mode_checkout,
        &mode_checkout.join("opaque/child"),
        None,
    );
    fs::set_permissions(
        mode_checkout.join("opaque"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    res.unwrap();
    assert!(mode_checkout.join("opaque/child").is_dir());

    // Symlink in the created root chain is rejected without writing through it.
    let scratch_sym = scratch();
    let sym_checkout = scratch_sym.path().join("checkout");
    let outside = scratch_sym.path().join("outside");
    fs::create_dir(&sym_checkout).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(
        sym_checkout.join("Cargo.toml"),
        "[package]\nname='pkg'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, sym_checkout.join("target")).unwrap();
    let sym_cmd = Command::new("true");
    let err = create_checkout_root(
        &sym_cmd,
        &sym_checkout,
        &sym_checkout.join("target/nested"),
        None,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("not a real directory"),
        "unexpected error: {err:#}"
    );
    assert!(!outside.join("CACHEDIR.TAG").exists());
    assert!(!outside.join("nested").exists());
}
