use super::common::*;
use crate::harness_entry::{child_token, confined, in_child_mode};

const CHILD: &str = "ARC_TEST_CHILD_FIXTURE_ISOLATION";

/// What a fixture does to the repositories it owns: the seed builder's
/// `init`, `config`, `add` and `commit`, a fixture commit, an `arc` command
/// that runs Git beneath it, a shell, and a test's own `git init`, `config`
/// and `commit`.
fn exercise_fixture_helpers() {
    let repo = Repo::new();
    repo.commit(&repo.root, "fixture.txt", "fixture\n", "test: fixture");
    stdout(repo.arc(&repo.root).args(["begin", "isolated"]));
    repo.arc_shell(&repo.root, "arc status isolated >/dev/null")
        .assert()
        .success();
    let own = repo.home.join("own");
    for args in [
        vec!["init", "-q", own.to_str().unwrap()],
        vec!["-C", own.to_str().unwrap(), "config", "user.name", "Own"],
        vec![
            "-C",
            own.to_str().unwrap(),
            "config",
            "user.email",
            "own@example.invalid",
        ],
        vec![
            "-C",
            own.to_str().unwrap(),
            "config",
            "commit.gpgsign",
            "false",
        ],
        vec![
            "-C",
            own.to_str().unwrap(),
            "config",
            "maintenance.auto",
            "false",
        ],
        vec![
            "-C",
            own.to_str().unwrap(),
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "own",
        ],
    ] {
        let output = Command::new("git").args(&args).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }
    assert_eq!(git_out(&own, &["config", "user.name"]), "Own");
}

/// A repository the fixture did not create, and every file under its `.git`.
fn decoy() -> (TempDir, PathBuf, Vec<(PathBuf, Vec<u8>)>) {
    let scratch = TempDir::new().unwrap();
    let decoy = scratch.path().join("decoy");
    fs::create_dir(&decoy).unwrap();
    git(&decoy, &["init", "-q", "--template=", "-b", "master"]);
    git(&decoy, &["config", "user.name", "Decoy"]);
    git(&decoy, &["config", "user.email", "decoy@example.invalid"]);
    git(&decoy, &["config", "commit.gpgsign", "false"]);
    git(&decoy, &["config", "maintenance.auto", "false"]);
    fs::write(decoy.join("decoy.txt"), "decoy\n").unwrap();
    git(&decoy, &["add", "decoy.txt"]);
    git(&decoy, &["commit", "-q", "-m", "decoy"]);
    let files = git_files(&decoy);
    (scratch, decoy, files)
}

fn git_files(repo: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, found: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, found);
            } else {
                found.push((path.clone(), fs::read(path).unwrap()));
            }
        }
    }
    let mut found = Vec::new();
    walk(&repo.join(".git"), &mut found);
    found.sort();
    found
}

/// This harness run once more for the test `name`, in child mode, under `envs`.
fn harness_child(name: &str, envs: &[(&str, PathBuf)]) -> std::process::Output {
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, child_token());
    for (variable, value) in envs {
        child.env(variable, value);
    }
    child.output().unwrap()
}

/// A child harness started with `variable` naming a decoy repository must
/// refuse before any fixture runs, naming the variable, and leave the decoy
/// byte-identical.
fn assert_refused_under(name: &str, variable: &str) {
    let (_scratch, decoy, before) = decoy();
    let value = if variable == "GIT_DIR" {
        decoy.join(".git")
    } else {
        decoy.clone()
    };
    let output = harness_child(name, &[(variable, value)]);
    assert!(
        git_files(&decoy) == before,
        "a fixture wrote the repository {variable} names"
    );
    assert!(
        !output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains(&format!("{variable} is set")),
        "the harness ran under {variable}: {output:?}"
    );
}

/// Claim: a suite started with `GIT_DIR` naming another repository, as Git
/// sets it for the commands it runs, refuses before any fixture runs and
/// names the variable, and that repository's config, refs, index and objects
/// stay byte-identical. Rejects fixture Git that inherits the variable and
/// initializes, configures and commits into the repository it names.
#[test]
fn a_fixture_under_an_inherited_git_dir_leaves_that_repository_untouched() {
    if in_child_mode(CHILD) {
        return exercise_fixture_helpers();
    }
    assert_refused_under(
        "fixture_isolation::a_fixture_under_an_inherited_git_dir_leaves_that_repository_untouched",
        "GIT_DIR",
    );
}

/// Claim: a suite started with `GIT_WORK_TREE` naming another checkout
/// refuses before any fixture runs and names the variable, and that
/// repository stays byte-identical. Rejects a harness that refuses only
/// `GIT_DIR` and runs its fixtures inside a Git operation's work tree.
#[test]
fn a_fixture_under_an_inherited_git_work_tree_leaves_that_repository_untouched() {
    if in_child_mode(CHILD) {
        return exercise_fixture_helpers();
    }
    assert_refused_under(
        "fixture_isolation::a_fixture_under_an_inherited_git_work_tree_leaves_that_repository_untouched",
        "GIT_WORK_TREE",
    );
}

/// Claim: no inherited `GIT_` variable reaches a fixture command: with the
/// index, object directory, common directory, configuration, injected
/// configuration, a trace target and a template naming another repository,
/// the seed builder, fixture commits, `arc`, a shell and a test's own `git`
/// all run against the fixture's repository, and the repository the
/// variables name stays byte-identical. Rejects a harness that clears a list
/// of variables and passes the rest, which then write that repository's
/// config, refs, index, objects or trace, or run its template's hooks.
#[test]
fn inherited_git_variables_never_reach_a_fixture_command() {
    if in_child_mode(CHILD) {
        return exercise_fixture_helpers();
    }
    let (scratch, decoy, before) = decoy();
    let decoy_git = decoy.join(".git");
    let template = scratch.path().join("template");
    fs::create_dir_all(template.join("hooks")).unwrap();
    let hook = template.join("hooks/post-commit");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\necho hooked >> '{}'\n",
            decoy_git.join("hooked").display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let output = harness_child(
        "fixture_isolation::inherited_git_variables_never_reach_a_fixture_command",
        &[
            ("GIT_INDEX_FILE", decoy_git.join("index")),
            ("GIT_OBJECT_DIRECTORY", decoy_git.join("objects")),
            ("GIT_COMMON_DIR", decoy_git.clone()),
            ("GIT_CONFIG", decoy_git.join("config")),
            ("GIT_CONFIG_COUNT", "1".into()),
            ("GIT_CONFIG_KEY_0", "core.bare".into()),
            ("GIT_CONFIG_VALUE_0", "true".into()),
            ("GIT_CONFIG_PARAMETERS", "'core.bare'='true'".into()),
            ("GIT_TRACE2_EVENT", decoy_git.join("trace2.json")),
            ("GIT_TEMPLATE_DIR", template.clone()),
        ],
    );
    assert!(
        git_files(&decoy) == before,
        "a fixture command wrote the repository an inherited variable names"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "fixture helpers failed under inherited variables: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Whether Git started in `cwd` finds a repository there or above it.
fn discovers_a_repository(cwd: &Path) -> Option<String> {
    let found = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .unwrap();
    found
        .status
        .success()
        .then(|| String::from_utf8_lossy(&found.stdout).trim().to_owned())
}

/// Claim: Git run in a directory beneath this checkout that is not itself a
/// repository finds none, because discovery never climbs into the checkout
/// from below. Rejects a test that forgot `git init` adopting the checkout's
/// repository, where `git config` would rewrite the shared configuration.
#[test]
fn git_beneath_the_checkout_finds_no_repository() {
    let beneath = crate::harness_entry::checkout().join("src");
    assert_eq!(
        discovers_a_repository(&beneath),
        None,
        "Git in {beneath:?} found a repository"
    );
}

/// Claim: Git run in a plain directory under the temporary directory finds
/// no repository, even when the temporary directory lies inside one: the
/// child harness's temporary directory is inside a decoy repository, and
/// discovery stops at it. Rejects a fixture directory a test forgot to
/// initialize adopting whatever repository encloses the temporary directory.
#[test]
fn git_in_a_plain_temporary_directory_finds_no_enclosing_repository() {
    if in_child_mode(CHILD) {
        let plain = TempDir::new().unwrap();
        assert_eq!(
            discovers_a_repository(plain.path()),
            None,
            "Git in {:?} found a repository",
            plain.path()
        );
        return;
    }
    let (_scratch, decoy, _) = decoy();
    let temporary = decoy.join("tmp");
    fs::create_dir(&temporary).unwrap();
    let output = harness_child(
        "fixture_isolation::git_in_a_plain_temporary_directory_finds_no_enclosing_repository",
        &[("TMPDIR", temporary)],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Claim: `git config --global` from a test's own bare `git` writes a file of
/// the harness's own, never the invoking user's global configuration, which
/// stays byte-identical. The child harness runs under a stand-in home with no
/// global configuration variable of its own, as a user's shell would start it.
/// Rejects a test that forgets the helpers' `GIT_CONFIG_GLOBAL` rewriting the
/// operator's identity or signing settings.
#[test]
fn git_config_global_from_a_bare_command_never_reaches_the_users_file() {
    const NAME: &str =
        "fixture_isolation::git_config_global_from_a_bare_command_never_reaches_the_users_file";
    if in_child_mode(CHILD) {
        let own = TempDir::new().unwrap();
        let written = Command::new("git")
            .current_dir(own.path())
            .args(["config", "--global", "user.name", "Fixture Global"])
            .output()
            .unwrap();
        assert!(written.status.success(), "{written:?}");
        return;
    }
    let real = std::env::var_os("HOME").map(PathBuf::from);
    let real_files = |home: &Option<PathBuf>| {
        home.iter()
            .flat_map(|home| [home.join(".gitconfig"), home.join(".config/git/config")])
            .map(|path| fs::read(&path).ok())
            .collect::<Vec<_>>()
    };
    let real_before = real_files(&real);
    let (scratch, temporary) = confined_scratch();
    let home = scratch.path().join("home");
    fs::create_dir_all(home.join(".config/git")).unwrap();
    fs::write(home.join(".gitconfig"), "[user]\n\tname = Operator\n").unwrap();
    let stand_in = Some(home.clone());
    let stand_in_before = real_files(&stand_in);
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", NAME, "--nocapture"])
        .env(CHILD, child_token())
        .env("TMPDIR", &temporary)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        .output()
        .unwrap();
    assert!(
        real_files(&stand_in) == stand_in_before,
        "a bare git config --global wrote the user's global configuration"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        real_files(&real) == real_before,
        "the real global configuration changed"
    );
}

/// Where a confined child harness's decoys live: everything under the
/// scratch directory except the child's own temporary directory, which is
/// all of the scratch the child may write.
const DECOYS: &str = "ARC_TEST_DECOYS";

/// The decoy directory a confined child was given.
fn decoys() -> PathBuf {
    PathBuf::from(std::env::var_os(DECOYS).expect("a confined child names its decoys"))
}

/// A scratch directory whose `child-tmp` is the temporary directory of a
/// confined child harness; anything else in it is outside the child's roots.
fn confined_scratch() -> (TempDir, PathBuf) {
    let scratch = TempDir::new().unwrap();
    let temporary = scratch.path().join("child-tmp");
    fs::create_dir(&temporary).unwrap();
    (scratch, temporary)
}

/// The test `name` run in a child harness whose temporary directory is
/// `scratch`'s `child-tmp`, with `scratch` named as its decoys, under `envs`.
/// The child must pass: its test asserts the refusal it observes.
fn assert_confined_child_passes(name: &str, scratch: &Path, envs: &[(&str, PathBuf)]) {
    let mut all = vec![
        ("TMPDIR", scratch.join("child-tmp")),
        (DECOYS, scratch.to_path_buf()),
    ];
    all.extend(envs.iter().cloned());
    let output = harness_child(name, &all);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `git <args> config arc.confinement.probe 1` from a confined child, aimed at
/// a decoy repository outside its roots, must fail and leave the decoy
/// byte-identical.
fn assert_decoy_config_unwritable(name: &str, aim: fn(&Path) -> Vec<String>) {
    if in_child_mode(CHILD) {
        assert!(confined(), "this harness runs unconfined");
        let decoy = decoys().join("decoy");
        let attempt = Command::new("git")
            .args(aim(&decoy))
            .args(["config", "arc.confinement.probe", "1"])
            .output()
            .unwrap();
        assert!(
            !attempt.status.success(),
            "git config wrote the decoy: {attempt:?}"
        );
        return;
    }
    let (scratch, _) = confined_scratch();
    let decoy = scratch.path().join("decoy");
    git(scratch.path(), &["init", "-q", "--template=", "decoy"]);
    let before = git_files(&decoy);
    assert_confined_child_passes(name, scratch.path(), &[]);
    assert!(git_files(&decoy) == before, "the decoy repository changed");
}

/// Claim: Git aimed by `-C` at a repository outside the harness's writable
/// roots cannot write its configuration: the kernel refuses, the command
/// fails, and the file stays byte-identical. Proven against a decoy outside a
/// confined child's roots. Rejects a fixture or helper naming the real
/// checkout, outright or by a computed path, and rewriting the configuration
/// every worktree of the repository shares.
#[test]
fn git_aimed_by_dash_c_outside_the_roots_cannot_write_its_config() {
    assert_decoy_config_unwritable(
        "fixture_isolation::git_aimed_by_dash_c_outside_the_roots_cannot_write_its_config",
        |decoy| vec!["-C".into(), decoy.display().to_string()],
    );
}

/// Claim: Git aimed by `--git-dir` at a repository outside the harness's
/// writable roots cannot write its configuration: the command fails and the
/// file stays byte-identical. Proven against a decoy. Rejects a fixture naming
/// a real repository directory, which no discovery setting or cleared
/// variable can stop.
#[test]
fn git_aimed_by_git_dir_outside_the_roots_cannot_write_its_config() {
    assert_decoy_config_unwritable(
        "fixture_isolation::git_aimed_by_git_dir_outside_the_roots_cannot_write_its_config",
        |decoy| vec![format!("--git-dir={}", decoy.join(".git").display())],
    );
}

/// Claim: a test cannot create a file beneath its home directory: the kernel
/// refuses it. Proven with a decoy home outside a confined child's roots.
/// Rejects a test, helper or `arc` under test writing the operator's
/// dotfiles, Git configuration, arc ledgers or journals.
#[test]
fn a_write_beneath_the_home_fails() {
    if in_child_mode(CHILD) {
        assert!(confined(), "this harness runs unconfined");
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let created = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(home.join("probe"));
        assert_eq!(
            created.map(|_| ()).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::PermissionDenied)
        );
        return;
    }
    let (scratch, _) = confined_scratch();
    let home = scratch.path().join("home");
    fs::create_dir(&home).unwrap();
    assert_confined_child_passes(
        "fixture_isolation::a_write_beneath_the_home_fails",
        scratch.path(),
        &[("HOME", home.clone())],
    );
    assert_eq!(
        fs::read_dir(&home).unwrap().count(),
        0,
        "the decoy home changed"
    );
}

/// A disposable file outside a confined child's roots, and its bytes.
fn outside_file(scratch: &Path) -> (PathBuf, Vec<u8>) {
    let file = scratch.join("existing");
    fs::write(&file, b"outside the roots\n").unwrap();
    (file, b"outside the roots\n".to_vec())
}

/// Claim: an existing file outside the writable roots cannot be opened for
/// writing, even without truncation: the kernel refuses and its bytes stay.
/// Rejects confinement that guards only creation, under which an existing
/// configuration file is overwritten in place.
#[test]
fn writing_an_existing_file_outside_the_roots_fails() {
    if in_child_mode(CHILD) {
        assert!(confined(), "this harness runs unconfined");
        let opened = fs::OpenOptions::new()
            .write(true)
            .open(decoys().join("existing"));
        assert_eq!(
            opened.map(|_| ()).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::PermissionDenied)
        );
        return;
    }
    let (scratch, _) = confined_scratch();
    let (file, bytes) = outside_file(scratch.path());
    assert_confined_child_passes(
        "fixture_isolation::writing_an_existing_file_outside_the_roots_fails",
        scratch.path(),
        &[],
    );
    assert_eq!(fs::read(file).unwrap(), bytes);
}

/// Claim: an existing file outside the writable roots cannot be truncated by
/// path: the kernel refuses and its bytes stay. Rejects confinement that
/// guards opening for writing but lets `truncate` empty a file it names.
#[test]
fn truncating_an_existing_file_outside_the_roots_fails() {
    if in_child_mode(CHILD) {
        assert!(confined(), "this harness runs unconfined");
        let path = std::ffi::CString::new(
            decoys()
                .join("existing")
                .into_os_string()
                .into_encoded_bytes(),
        )
        .unwrap();
        // SAFETY: path is a NUL-terminated file name.
        let truncated = unsafe { libc::truncate(path.as_ptr(), 0) };
        assert_eq!(truncated, -1, "truncate succeeded");
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        return;
    }
    let (scratch, _) = confined_scratch();
    let (file, bytes) = outside_file(scratch.path());
    assert_confined_child_passes(
        "fixture_isolation::truncating_an_existing_file_outside_the_roots_fails",
        scratch.path(),
        &[],
    );
    assert_eq!(fs::read(file).unwrap(), bytes);
}

/// Claim: a device opened read-only outside the nodes the harness grants
/// cannot receive device ioctls: the kernel refuses them. Rejects
/// confinement that ignores the device-ioctl right, under which a test could
/// grab an input device or reconfigure a terminal it never opened for
/// writing.
#[test]
fn a_device_ioctl_outside_the_granted_nodes_is_refused() {
    if in_child_mode(CHILD) {
        assert!(confined(), "this harness runs unconfined");
        let device = fs::File::open("/dev/zero").unwrap();
        let mut size = std::mem::MaybeUninit::<libc::winsize>::uninit();
        // SAFETY: TIOCGWINSZ only reads a window size into the buffer given;
        // /dev/zero implements no ioctl, so nothing changes either way.
        let result = unsafe {
            libc::ioctl(
                std::os::fd::AsRawFd::as_raw_fd(&device),
                libc::TIOCGWINSZ,
                size.as_mut_ptr(),
            )
        };
        assert_eq!(result, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EACCES),
            "the ioctl reached the device"
        );
        return;
    }
    let (scratch, _) = confined_scratch();
    assert_confined_child_passes(
        "fixture_isolation::a_device_ioctl_outside_the_granted_nodes_is_refused",
        scratch.path(),
        &[],
    );
}

/// Claim: a writable descriptor the harness inherited above standard error
/// is closed before any test runs, so a descriptor opened before confinement
/// on a file outside the roots cannot carry writes past it; the file's bytes
/// stay. Rejects a harness started with descriptor 3 open on the real
/// configuration writing it after confinement.
#[test]
fn an_inherited_writable_descriptor_is_closed_at_entry() {
    if in_child_mode(CHILD) {
        let decoy = decoys().join("existing").canonicalize().unwrap();
        for entry in fs::read_dir("/proc/self/fd").unwrap() {
            let target = fs::read_link(entry.unwrap().path());
            assert!(
                target.ok().as_deref() != Some(decoy.as_path()),
                "a descriptor on the decoy survived the entry"
            );
        }
        return;
    }
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let (scratch, temporary) = confined_scratch();
    let (file, bytes) = outside_file(scratch.path());
    let writable = fs::OpenOptions::new().write(true).open(&file).unwrap();
    let fd = writable.as_raw_fd();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "fixture_isolation::an_inherited_writable_descriptor_is_closed_at_entry",
            "--nocapture",
        ])
        .env(CHILD, child_token())
        .env("TMPDIR", temporary)
        .env(DECOYS, scratch.path());
    // SAFETY: between fork and exec only dup2 runs, which is async-signal-safe;
    // it leaves descriptor 3 open on the decoy without close-on-exec.
    unsafe {
        child.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = child.output().unwrap();
    drop(writable);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(file).unwrap(), bytes);
}

/// Claim: a standard input the harness inherited open for writing is
/// replaced by a read-only one before any test runs, so it cannot carry
/// writes to a file outside the roots; the file's bytes stay. Rejects a
/// harness whose writable standard input reaches what confinement guards.
#[test]
fn a_writable_standard_input_is_reopened_read_only() {
    if in_child_mode(CHILD) {
        // SAFETY: F_GETFL reads the status flags of descriptor 0.
        let flags = unsafe { libc::fcntl(0, libc::F_GETFL) };
        assert_eq!(
            flags & libc::O_ACCMODE,
            libc::O_RDONLY,
            "standard input is writable"
        );
        let decoy = decoys().join("existing").canonicalize().unwrap();
        assert_ne!(fs::read_link("/proc/self/fd/0").ok(), Some(decoy));
        return;
    }
    let (scratch, temporary) = confined_scratch();
    let (file, bytes) = outside_file(scratch.path());
    let stdin = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&file)
        .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "fixture_isolation::a_writable_standard_input_is_reopened_read_only",
            "--nocapture",
        ])
        .env(CHILD, child_token())
        .env("TMPDIR", temporary)
        .env(DECOYS, scratch.path())
        .stdin(stdin)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(file).unwrap(), bytes);
}

/// What `confinement_probe` reported from a harness started by `command`,
/// and the harness's output: `None` when the harness refused to start.
fn probe_confinement(command: &mut Command) -> (Option<bool>, std::process::Output) {
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let reported = stdout
        .lines()
        .find_map(|line| line.strip_prefix("confined: "))
        .map(|value| value == "true");
    (reported, output)
}

/// A harness started by `start` with a home its roots would grant must refuse
/// when confinement is required, and run unconfined with the notice when not.
fn assert_home_root_refused(start: impl Fn() -> Command) {
    let (reported, output) = probe_confinement(start().env("ARC_TEST_CONFINEMENT", "required"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        reported.is_none()
            && output.status.code() == Some(2)
            && stderr.contains("would grant the home directory"),
        "a required confinement granted the home: {output:?}"
    );
    let (reported, output) = probe_confinement(start().env_remove("ARC_TEST_CONFINEMENT"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        reported == Some(false) && stderr.contains("notice: the tests run unconfined"),
        "a confinement granted the home: {output:?}"
    );
}

const PROBE: &str = "harness_entry::tests::confinement_probe";

/// Claim: a temporary directory that is the home directory is never granted
/// for writing: with confinement required the harness refuses, naming what
/// the root would grant; otherwise it runs unconfined with the notice. Proven
/// with a decoy home. Rejects `TMPDIR=$HOME` turning confinement into a grant
/// of the whole home.
#[test]
fn a_temporary_directory_that_is_the_home_is_never_granted() {
    let (scratch, _) = confined_scratch();
    let home = scratch.path().join("home");
    fs::create_dir(&home).unwrap();
    assert_home_root_refused(|| {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", PROBE, "--nocapture"])
            .env("HOME", &home)
            .env("TMPDIR", &home);
        command
    });
    assert_eq!(
        fs::read_dir(&home).unwrap().count(),
        0,
        "the decoy home changed"
    );
}

/// Claim: a build directory that is the home directory is never granted for
/// writing: a harness whose target directory is its home refuses when
/// confinement is required and otherwise runs unconfined with the notice.
/// Proven with this harness linked into a decoy home marked as Cargo's cache.
/// Rejects `CARGO_TARGET_DIR=$HOME` turning confinement into a grant of the
/// whole home.
#[test]
fn a_target_directory_that_is_the_home_is_never_granted() {
    let home = tempfile::Builder::new()
        .prefix("home-as-target-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    fs::write(
        home.path().join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    fs::create_dir(home.path().join("deps")).unwrap();
    let linked = home.path().join("deps/harness");
    fs::hard_link(std::env::current_exe().unwrap(), &linked).unwrap();
    assert_home_root_refused(|| {
        let mut command = Command::new(&linked);
        command
            .args(["--exact", PROBE, "--nocapture"])
            .env("HOME", home.path());
        command
    });
}

/// Claim: confinement leaves a fixture's own repository under the temporary
/// directory fully usable: the seed builder, commits, `arc`, a shell and a
/// test's own `git init`, `config` and `commit` all succeed there. Rejects a
/// confinement that refuses the writes fixtures legitimately make.
#[test]
fn a_fixture_repository_under_the_temporary_directory_works_confined() {
    assert!(confined(), "this harness runs unconfined");
    exercise_fixture_helpers();
}
