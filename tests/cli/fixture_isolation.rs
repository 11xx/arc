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
    let scratch = TempDir::new().unwrap();
    let home = scratch.path().join("home");
    fs::create_dir_all(home.join(".config/git")).unwrap();
    fs::write(home.join(".gitconfig"), "[user]\n\tname = Operator\n").unwrap();
    let stand_in = Some(home.clone());
    let stand_in_before = real_files(&stand_in);
    let output = Command::new(std::env::current_exe().unwrap())
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

/// The configuration file this checkout's repository shares with every one of
/// its worktrees, named explicitly rather than discovered.
fn checkout_common_dir() -> PathBuf {
    let checkout = crate::harness_entry::checkout();
    let common = git_out(
        &checkout,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    PathBuf::from(common)
}

/// `git <args> config arc.confinement.probe <pid>` aimed at this checkout must
/// fail, leaving its shared configuration byte-identical. Confinement is
/// checked first, so an unconfined harness fails here without the attempt.
fn assert_checkout_config_unwritable(args: &[String]) {
    assert!(confined(), "this harness runs unconfined");
    let config = checkout_common_dir().join("config");
    let before = fs::read(&config).unwrap();
    let attempt = Command::new("git")
        .args(args)
        .args(["config", "arc.confinement.probe"])
        .arg(std::process::id().to_string())
        .output()
        .unwrap();
    assert!(
        fs::read(&config).unwrap() == before,
        "git {args:?} config changed the checkout's configuration"
    );
    assert!(
        !attempt.status.success(),
        "git {args:?} config reported a write to the checkout's configuration"
    );
}

/// Claim: Git aimed at this checkout by `-C` cannot write its shared
/// configuration: the kernel refuses the write, the command fails, and the
/// file stays byte-identical. Rejects a fixture or helper that names the real
/// checkout, outright or by a path it computed, rewriting the configuration
/// every worktree of the repository shares.
#[test]
fn git_aimed_at_the_checkout_by_dash_c_cannot_write_its_config() {
    let checkout = crate::harness_entry::checkout();
    assert_checkout_config_unwritable(&["-C".into(), checkout.display().to_string()]);
}

/// Claim: Git aimed at this checkout's repository by `--git-dir` cannot write
/// its shared configuration: the command fails and the file stays
/// byte-identical. Rejects a fixture naming the real repository directory,
/// which no discovery setting or cleared variable can stop.
#[test]
fn git_aimed_at_the_checkout_by_git_dir_cannot_write_its_config() {
    assert_checkout_config_unwritable(&[format!("--git-dir={}", checkout_common_dir().display())]);
}

/// Claim: a test cannot create a file beneath the invoking user's real home:
/// the kernel refuses it. Rejects a test, helper or `arc` under test writing
/// the operator's dotfiles, Git configuration, arc ledgers or journals.
#[test]
fn a_write_beneath_the_real_home_fails() {
    assert!(confined(), "this harness runs unconfined");
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
    let probe = home.join(format!(".arc-confinement-{}", std::process::id()));
    let created = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe);
    if created.is_ok() {
        let _ = fs::remove_file(&probe);
    }
    assert_eq!(
        created.map(|_| ()).map_err(|error| error.kind()),
        Err(std::io::ErrorKind::PermissionDenied),
        "a write beneath {home:?}"
    );
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
