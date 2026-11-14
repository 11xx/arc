use super::common::*;

const CHILD: &str = "ARC_TEST_FIXTURE_ISOLATION_CHILD";

/// What a fixture does to the repositories it owns: the seed builder's
/// `init`, `config`, `add` and `commit`, a fixture commit, an `arc` command
/// that runs Git beneath it, a shell, and a test's own `git`.
fn exercise_fixture_helpers() {
    let repo = Repo::new();
    repo.commit(&repo.root, "fixture.txt", "fixture\n", "test: fixture");
    stdout(repo.arc(&repo.root).args(["begin", "isolated"]));
    repo.arc_shell(&repo.root, "arc status isolated >/dev/null")
        .assert()
        .success();
    let own = Command::new("git")
        .current_dir(&repo.root)
        .args(["config", "fixture.own", "true"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", repo.home.join(".gitconfig"))
        .output()
        .unwrap();
    assert!(own.status.success(), "{own:?}");
    assert_eq!(git_out(&repo.root, &["config", "fixture.own"]), "true");
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
    child.args(["--exact", name, "--nocapture"]).env(CHILD, "1");
    for (variable, value) in envs {
        child.env(variable, value);
    }
    child.output().unwrap()
}

/// Claim: a suite started from inside a Git operation, with `GIT_DIR` and
/// `GIT_WORK_TREE` naming another repository, refuses before any fixture runs
/// and names the variable, and that repository's config, refs, index and
/// objects stay byte-identical. Rejects fixture Git that inherits the
/// variables and initializes, configures and commits into the repository
/// they name.
#[test]
fn a_fixture_under_an_inherited_git_dir_leaves_that_repository_untouched() {
    if std::env::var_os(CHILD).is_some() {
        return exercise_fixture_helpers();
    }
    let (_scratch, decoy, before) = decoy();
    let output = harness_child(
        "fixture_isolation::a_fixture_under_an_inherited_git_dir_leaves_that_repository_untouched",
        &[
            ("GIT_DIR", decoy.join(".git")),
            ("GIT_WORK_TREE", decoy.clone()),
        ],
    );
    assert!(
        git_files(&decoy) == before,
        "a fixture wrote the repository GIT_DIR names"
    );
    assert!(
        !output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains("GIT_DIR is set"),
        "the harness ran under GIT_DIR: {output:?}"
    );
}

/// Claim: an inherited index, object directory, common directory,
/// configuration file or injected configuration never reaches a fixture
/// command: the seed builder, fixture commits, `arc`, a shell and a test's
/// own `git` all run against the fixture's repository, and the repository
/// the variables name stays byte-identical. Rejects a helper that passes
/// them through and writes that repository's config, refs, index or objects.
#[test]
fn inherited_git_selectors_never_reach_a_fixture_command() {
    if std::env::var_os(CHILD).is_some() {
        return exercise_fixture_helpers();
    }
    let (_scratch, decoy, before) = decoy();
    let decoy_git = decoy.join(".git");
    let output = harness_child(
        "fixture_isolation::inherited_git_selectors_never_reach_a_fixture_command",
        &[
            ("GIT_INDEX_FILE", decoy_git.join("index")),
            ("GIT_OBJECT_DIRECTORY", decoy_git.join("objects")),
            ("GIT_COMMON_DIR", decoy_git.clone()),
            ("GIT_CONFIG", decoy_git.join("config")),
            ("GIT_CONFIG_COUNT", "1".into()),
            ("GIT_CONFIG_KEY_0", "core.bare".into()),
            ("GIT_CONFIG_VALUE_0", "true".into()),
            ("GIT_CONFIG_PARAMETERS", "'core.bare'='true'".into()),
        ],
    );
    assert!(
        git_files(&decoy) == before,
        "a fixture command wrote the repository an inherited selector names"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "fixture helpers failed under inherited selectors: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
