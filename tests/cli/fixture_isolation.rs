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

/// The harness `exe` run for the test `name`, in child mode, under `envs`.
fn harness_child_of(exe: &Path, name: &str, envs: &[(&str, PathBuf)]) -> std::process::Output {
    let mut child = Command::new(exe);
    child
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, child_token());
    for (variable, value) in envs {
        child.env(variable, value);
    }
    child.output().unwrap()
}

/// This harness run once more for the test `name`, in child mode, under `envs`.
fn harness_child(name: &str, envs: &[(&str, PathBuf)]) -> std::process::Output {
    harness_child_of(&std::env::current_exe().unwrap(), name, envs)
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
/// no repository, even when the inherited `TMPDIR` lies inside one: the child
/// harness inherits a `TMPDIR` inside a decoy repository, works in a
/// temporary directory of its own under the system one, and discovery never
/// climbs into the system one. Rejects a fixture directory a test forgot to
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
    let (scratch, harness) = stand_in_target();
    let home = scratch.path().join("home");
    fs::create_dir_all(home.join(".config/git")).unwrap();
    fs::write(home.join(".gitconfig"), "[user]\n\tname = Operator\n").unwrap();
    let stand_in = Some(home.clone());
    let stand_in_before = real_files(&stand_in);
    let output = Command::new(&harness)
        .args(["--exact", NAME, "--nocapture"])
        .env(CHILD, child_token())
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

/// Where a confined child harness's decoys live: the scratch directory that
/// holds its stand-in target directory, all of it outside the child's roots
/// except that target.
const DECOYS: &str = "ARC_TEST_DECOYS";

/// The decoy directory a confined child was given.
fn decoys() -> PathBuf {
    PathBuf::from(std::env::var_os(DECOYS).expect("a confined child names its decoys"))
}

/// A scratch directory under the build's own temporary directory holding a
/// stand-in target directory, `target`, marked as Cargo's cache, with this
/// harness linked into it. A child started from the link writes only beneath
/// that stand-in and the system temporary directory, so everything else in
/// the scratch is outside its roots.
fn stand_in_target() -> (TempDir, PathBuf) {
    let scratch = tempfile::Builder::new()
        .prefix("stand-in-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let target = scratch.path().join("target");
    fs::create_dir_all(target.join("deps")).unwrap();
    fs::write(
        target.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    let harness = target.join("deps/harness");
    fs::hard_link(std::env::current_exe().unwrap(), &harness).unwrap();
    (scratch, harness)
}

/// A started child harness must pass: its test asserts what it observes.
fn assert_child_passed(output: &std::process::Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The test `name` run in a child harness started from `harness`, linked in
/// `scratch`'s stand-in target, with `scratch` named as its decoys, under
/// `envs`. The child must pass: its test asserts the refusal it observes.
fn assert_confined_child_passes(
    name: &str,
    (scratch, harness): &(TempDir, PathBuf),
    envs: &[(&str, PathBuf)],
) {
    let mut all = vec![(DECOYS, scratch.path().to_path_buf())];
    all.extend(envs.iter().cloned());
    assert_child_passed(&harness_child_of(harness, name, &all));
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
    let stand_in = stand_in_target();
    let decoy = stand_in.0.path().join("decoy");
    git(stand_in.0.path(), &["init", "-q", "--template=", "decoy"]);
    let before = git_files(&decoy);
    assert_confined_child_passes(name, &stand_in, &[]);
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

/// Whether creating `path` was refused by the kernel.
fn creation_refused(path: &Path) -> bool {
    let created = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path);
    matches!(created, Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
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
        assert!(creation_refused(&home.join("probe")), "a file was created");
        return;
    }
    let stand_in = stand_in_target();
    let home = stand_in.0.path().join("home");
    fs::create_dir(&home).unwrap();
    assert_confined_child_passes(
        "fixture_isolation::a_write_beneath_the_home_fails",
        &stand_in,
        &[("HOME", home.clone())],
    );
    assert_eq!(
        fs::read_dir(&home).unwrap().count(),
        0,
        "the decoy home changed"
    );
}

/// Claim: an inherited `TMPDIR` is never a writable root, wherever it points:
/// the harness works in a temporary directory of its own under the system
/// one, and a file cannot be created in the inherited directory. Proven with
/// `TMPDIR` naming a decoy home's `.config/git`, which neither is nor holds
/// the home. Rejects a root the environment chooses granting the operator's
/// configuration, or the checkout's sources, from inside.
#[test]
fn an_inherited_temporary_directory_is_never_a_writable_root() {
    if in_child_mode(CHILD) {
        assert!(confined(), "this harness runs unconfined");
        let inherited = decoys().join("home/.config/git");
        let own = std::env::temp_dir();
        assert_eq!(
            own.parent(),
            Some(Path::new(crate::harness_entry::SYSTEM_TEMPORARY)),
            "the harness kept the inherited temporary directory"
        );
        assert!(
            creation_refused(&inherited.join("config")),
            "a file was created in the inherited temporary directory"
        );
        return;
    }
    let stand_in = stand_in_target();
    let home = stand_in.0.path().join("home");
    let inherited = home.join(".config/git");
    fs::create_dir_all(&inherited).unwrap();
    assert_confined_child_passes(
        "fixture_isolation::an_inherited_temporary_directory_is_never_a_writable_root",
        &stand_in,
        &[("HOME", home), ("TMPDIR", inherited.clone())],
    );
    assert_eq!(
        fs::read_dir(&inherited).unwrap().count(),
        0,
        "the inherited temporary directory changed"
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
    let stand_in = stand_in_target();
    let (file, bytes) = outside_file(stand_in.0.path());
    assert_confined_child_passes(
        "fixture_isolation::writing_an_existing_file_outside_the_roots_fails",
        &stand_in,
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
    let stand_in = stand_in_target();
    let (file, bytes) = outside_file(stand_in.0.path());
    assert_confined_child_passes(
        "fixture_isolation::truncating_an_existing_file_outside_the_roots_fails",
        &stand_in,
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
    assert_confined_child_passes(
        "fixture_isolation::a_device_ioctl_outside_the_granted_nodes_is_refused",
        &stand_in_target(),
        &[],
    );
}

/// A system call a child is denied: its number, an argument that must hold
/// a value for the denial to apply, and the error it fails with.
type Denial = (libc::c_long, Option<(usize, u32)>, libc::c_int);

/// A seccomp program failing each call `denials` names, and allowing the
/// rest. Arguments are compared by their low 32 bits.
fn seccomp_program(denials: &[Denial]) -> Vec<libc::sock_filter> {
    let statement = |code: u32, k: u32| libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    };
    let jump_unless = |k: u32, skip: u8| libc::sock_filter {
        code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
        jt: 0,
        jf: skip,
        k,
    };
    let load = |offset: u32| statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, offset);
    let mut program = Vec::new();
    for &(call, argument, errno) in denials {
        program.push(load(0));
        let deny = statement(
            libc::BPF_RET | libc::BPF_K,
            libc::SECCOMP_RET_ERRNO | errno as u32,
        );
        match argument {
            None => program.extend([jump_unless(call as u32, 1), deny]),
            Some((index, value)) => program.extend([
                jump_unless(call as u32, 3),
                load(16 + 8 * index as u32),
                jump_unless(value, 1),
                deny,
            ]),
        }
    }
    program.push(statement(
        libc::BPF_RET | libc::BPF_K,
        libc::SECCOMP_RET_ALLOW,
    ));
    program
}

/// Install, between fork and exec of `command`, a seccomp filter denying
/// `denials`, as a container or sandbox policy might.
fn deny_calls(command: &mut Command, denials: &[Denial]) {
    use std::os::unix::process::CommandExt;
    let program = seccomp_program(denials);
    // SAFETY: between fork and exec only prctl runs, which is
    // async-signal-safe, on a program built before the fork.
    unsafe {
        command.pre_exec(move || {
            let filter = libc::sock_fprog {
                len: program.len() as u16,
                filter: program.as_ptr() as *mut libc::sock_filter,
            };
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &filter as *const libc::sock_fprog,
                ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// `close_range` refused as a kernel without it refuses it.
const NO_CLOSE_RANGE: Denial = (libc::SYS_close_range, None, libc::ENOSYS);

/// Make `command` start with descriptor 3 open on `file` for writing.
fn inherit_writable_descriptor(command: &mut Command, file: &Path) -> fs::File {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    let writable = fs::OpenOptions::new().write(true).open(file).unwrap();
    let fd = writable.as_raw_fd();
    // SAFETY: between fork and exec only dup2 runs, which is async-signal-safe;
    // it leaves descriptor 3 open on the file without close-on-exec.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    writable
}

/// The child's side of the descriptor witnesses: confined, with no
/// descriptor open on the decoy file.
fn assert_no_descriptor_on_the_decoy() {
    assert!(confined(), "this harness runs unconfined");
    let decoy = decoys().join("existing").canonicalize().unwrap();
    for entry in fs::read_dir("/proc/self/fd").unwrap() {
        let target = fs::read_link(entry.unwrap().path());
        assert!(
            target.ok().as_deref() != Some(decoy.as_path()),
            "a descriptor on the decoy survived the entry"
        );
    }
}

/// The test `name` run confined in a stand-in target with descriptor 3
/// inherited open for writing on a decoy file, and `denials` in force. The
/// child must pass, and the file's bytes stay.
fn assert_inherited_descriptor_closed(name: &str, denials: &[Denial]) {
    let stand_in = stand_in_target();
    let (file, bytes) = outside_file(stand_in.0.path());
    let mut child = Command::new(&stand_in.1);
    child
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, child_token())
        .env(DECOYS, stand_in.0.path());
    let writable = inherit_writable_descriptor(&mut child, &file);
    deny_calls(&mut child, denials);
    let output = child.output().unwrap();
    drop(writable);
    assert_child_passed(&output);
    assert_eq!(fs::read(file).unwrap(), bytes);
}

/// Claim: a writable descriptor the harness inherited above standard error
/// is closed before any test runs, so a descriptor opened before confinement
/// on a file outside the roots cannot carry writes past it; the file's bytes
/// stay. Rejects a harness started with descriptor 3 open on the real
/// configuration writing it after confinement.
#[test]
fn an_inherited_writable_descriptor_is_closed_at_entry() {
    if in_child_mode(CHILD) {
        return assert_no_descriptor_on_the_decoy();
    }
    assert_inherited_descriptor_closed(
        "fixture_isolation::an_inherited_writable_descriptor_is_closed_at_entry",
        &[],
    );
}

/// Claim: where `close_range` is refused, an inherited writable descriptor
/// is still closed before any test runs, one by one from those
/// `/proc/self/fd` lists, and the harness runs confined. Rejects a harness
/// that, on a kernel or under a policy without `close_range`, keeps the
/// descriptor open while reporting itself confined.
#[test]
fn an_inherited_descriptor_is_closed_where_close_range_is_refused() {
    if in_child_mode(CHILD) {
        return assert_no_descriptor_on_the_decoy();
    }
    assert_inherited_descriptor_closed(
        "fixture_isolation::an_inherited_descriptor_is_closed_where_close_range_is_refused",
        &[NO_CLOSE_RANGE],
    );
}

/// A harness started by `start`, which cannot finish its entry for the
/// reason `reason`, must refuse naming it when confinement is required, and
/// run unconfined with the notice naming it when not.
fn assert_unconfined_for(reason: &str, start: impl Fn() -> Command) {
    let (reported, output) = probe_confinement(start().env("ARC_TEST_CONFINEMENT", "required"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        reported.is_none() && output.status.code() == Some(2) && stderr.contains(reason),
        "a required confinement ran without refusing for {reason}: {output:?}"
    );
    let (reported, output) = probe_confinement(start().env_remove("ARC_TEST_CONFINEMENT"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        reported == Some(false)
            && stderr.contains("notice: the tests run unconfined")
            && stderr.contains(reason),
        "a harness reported itself confined despite {reason}: {output:?}"
    );
}

/// Claim: an inherited descriptor that can be neither range-closed nor
/// closed is a failure to confine: with confinement required the harness
/// refuses, naming the descriptor; otherwise it runs unconfined with the
/// notice naming it, and the decoy file's bytes stay. Rejects a harness that
/// ignores a refused close and reports itself confined with a writable
/// descriptor outside its roots still open.
#[test]
fn a_descriptor_that_cannot_be_closed_leaves_the_harness_unconfined() {
    let stand_in = stand_in_target();
    let (file, bytes) = outside_file(stand_in.0.path());
    let held = std::cell::RefCell::new(Vec::new());
    assert_unconfined_for("inherited descriptor 3 stays open", || {
        let mut command = Command::new(&stand_in.1);
        command.args(["--exact", PROBE, "--nocapture"]);
        held.borrow_mut()
            .push(inherit_writable_descriptor(&mut command, &file));
        deny_calls(
            &mut command,
            &[NO_CLOSE_RANGE, (libc::SYS_close, Some((0, 3)), libc::EPERM)],
        );
        command
    });
    drop(held);
    assert_eq!(fs::read(file).unwrap(), bytes);
}

/// Claim: a standard input the harness inherited open for writing is
/// replaced by a read-only one before any test runs, so it cannot carry
/// writes to a file outside the roots; the file's bytes stay. Rejects a
/// harness whose writable standard input reaches what confinement guards.
#[test]
fn a_writable_standard_input_is_reopened_read_only() {
    if in_child_mode(CHILD) {
        assert!(confined(), "this harness runs unconfined");
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
    let stand_in = stand_in_target();
    let (file, bytes) = outside_file(stand_in.0.path());
    let stdin = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&file)
        .unwrap();
    let output = Command::new(&stand_in.1)
        .args([
            "--exact",
            "fixture_isolation::a_writable_standard_input_is_reopened_read_only",
            "--nocapture",
        ])
        .env(CHILD, child_token())
        .env(DECOYS, stand_in.0.path())
        .stdin(stdin)
        .output()
        .unwrap();
    assert_child_passed(&output);
    assert_eq!(fs::read(file).unwrap(), bytes);
}

/// Claim: a writable standard input that cannot be replaced is a failure to
/// confine: with confinement required the harness refuses, naming it;
/// otherwise it runs unconfined with the notice naming it, and the decoy
/// file's bytes stay. Rejects a harness that ignores a refused `dup2` and
/// reports itself confined with standard input writable outside its roots.
#[test]
fn a_standard_input_that_cannot_be_replaced_leaves_the_harness_unconfined() {
    let stand_in = stand_in_target();
    let (file, bytes) = outside_file(stand_in.0.path());
    assert_unconfined_for("standard input stays writable", || {
        let stdin = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&file)
            .unwrap();
        let mut command = Command::new(&stand_in.1);
        command.args(["--exact", PROBE, "--nocapture"]).stdin(stdin);
        deny_calls(
            &mut command,
            &[
                #[cfg(target_arch = "x86_64")]
                (libc::SYS_dup2, Some((1, 0)), libc::EPERM),
                (libc::SYS_dup3, Some((1, 0)), libc::EPERM),
            ],
        );
        command
    });
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

const PROBE: &str = "harness_entry::tests::confinement_probe";

/// Claim: a build directory that is the home directory is never granted for
/// writing: a harness whose target directory is its home refuses when
/// confinement is required and otherwise runs unconfined with the notice.
/// Proven with this harness linked into a stand-in target that is also the
/// home. Rejects `CARGO_TARGET_DIR=$HOME` turning confinement into a grant of
/// the whole home.
#[test]
fn a_target_directory_that_is_the_home_is_never_granted() {
    let (_scratch, harness) = stand_in_target();
    let home = harness.parent().unwrap().parent().unwrap().to_path_buf();
    assert_unconfined_for("would grant the home directory", || {
        let mut command = Command::new(&harness);
        command
            .args(["--exact", PROBE, "--nocapture"])
            .env("HOME", &home);
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
