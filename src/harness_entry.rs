//! What every test harness of this crate does before its first test, so that
//! no fixture can reach a repository it did not create.
//!
//! Git exports repository-selecting variables to the commands it runs itself:
//! hooks, `rebase --exec` steps, aliases. A Git started under one reads and
//! writes the repository the variable names, not the one its working
//! directory is in, so a fixture's `git init` or `git config` lands there.
//! `GIT_DIR` and `GIT_WORK_TREE` mean the suite was started from inside a Git
//! operation, and the harness refuses to run. The rest are removed from the
//! process environment, which every helper and every test's own command
//! inherits, so no command has to remove them itself.
//!
//! The unit harness and the CLI harness both compile this file.

use std::ffi::OsStr;

/// Variables whose presence makes a harness refuse to start.
pub(crate) const REFUSED: [&str; 2] = ["GIT_DIR", "GIT_WORK_TREE"];

/// Variables through which Git selects a repository, work tree, index, object
/// directory, alternates, namespace, common directory or configuration.
/// `GIT_CONFIG_KEY_<n>` and `GIT_CONFIG_VALUE_<n>` belong with
/// `GIT_CONFIG_COUNT`.
const SELECTORS: [&str; 11] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
];

pub(crate) fn selects_repository(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    SELECTORS.contains(&&*name)
        || name.starts_with("GIT_CONFIG_KEY_")
        || name.starts_with("GIT_CONFIG_VALUE_")
}

/// libtest owns `main`, so the entry runs from `.init_array`: before `main`
/// and before any test thread exists, which is also what makes changing the
/// process environment sound.
#[used]
#[link_section = ".init_array"]
static ENTRY: extern "C" fn() = enter;

extern "C" fn enter() {
    for name in REFUSED {
        if std::env::var_os(name).is_some() {
            eprintln!(
                "{name} is set: the tests would reach the repository it names. \
                 Run them outside any Git operation, with {} unset.",
                REFUSED.join(" and ")
            );
            std::process::exit(2);
        }
    }
    let inherited: Vec<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| selects_repository(name))
        .collect();
    for name in inherited {
        std::env::remove_var(name);
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const CHILD: &str = "ARC_TEST_HARNESS_ENTRY_CHILD";
    const SELF: &str =
        "harness_entry::tests::this_harness_keeps_its_git_away_from_an_inherited_repository";

    /// Git without automatic maintenance, which would otherwise change a
    /// repository in the background after a commit.
    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(["-c", "maintenance.auto=false"])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", cwd.join("fixture-global-config"))
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    /// Every file under `dir` with its bytes, by path.
    fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut found = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    found.push((path.clone(), std::fs::read(path).unwrap()));
                }
            }
        }
        found.sort();
        found
    }

    /// Claim: this harness never lets a test's Git reach a repository the
    /// test did not create. Started with `GIT_DIR` and `GIT_WORK_TREE` naming
    /// another repository, it refuses before any test and names the variable;
    /// started with the index, object directory, common directory or
    /// configuration naming it, a test's own `git init`, `config`, `add` and
    /// `commit` run in the test's directory. Rejects a harness that runs its
    /// tests under either set and writes the other repository's config,
    /// refs, index or objects.
    #[test]
    fn this_harness_keeps_its_git_away_from_an_inherited_repository() {
        if std::env::var_os(CHILD).is_some() {
            let own = tempfile::TempDir::new().unwrap();
            git(own.path(), &["init", "-q", "--template=", "repo"]);
            let repo = own.path().join("repo");
            git(&repo, &["config", "user.name", "Fixture"]);
            git(&repo, &["config", "user.email", "fixture@example.invalid"]);
            git(&repo, &["config", "commit.gpgsign", "false"]);
            std::fs::write(repo.join("file"), "fixture\n").unwrap();
            git(&repo, &["add", "file"]);
            git(&repo, &["commit", "-q", "-m", "fixture"]);
            assert!(repo
                .join(".git/refs/heads")
                .read_dir()
                .unwrap()
                .next()
                .is_some());
            return;
        }
        let scratch = tempfile::TempDir::new().unwrap();
        let decoy = scratch.path().join("decoy");
        std::fs::create_dir(&decoy).unwrap();
        git(&decoy, &["init", "-q", "--template="]);
        git(&decoy, &["config", "user.name", "Decoy"]);
        git(&decoy, &["config", "user.email", "decoy@example.invalid"]);
        git(&decoy, &["config", "commit.gpgsign", "false"]);
        std::fs::write(decoy.join("decoy"), "decoy\n").unwrap();
        git(&decoy, &["add", "decoy"]);
        git(&decoy, &["commit", "-q", "-m", "decoy"]);
        let decoy_git = decoy.join(".git");
        let before = files(&decoy_git);
        let run = |envs: &[(&str, PathBuf)]| {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", SELF, "--nocapture"]).env(CHILD, "1");
            for (name, value) in envs {
                child.env(name, value);
            }
            child.output().unwrap()
        };

        let refused = run(&[
            ("GIT_DIR", decoy_git.clone()),
            ("GIT_WORK_TREE", decoy.clone()),
        ]);
        assert!(
            files(&decoy_git) == before,
            "a harness under GIT_DIR wrote the repository it names"
        );
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            !refused.status.success() && stderr.contains("GIT_DIR is set"),
            "a harness under GIT_DIR ran: {refused:?}"
        );

        let cleared = run(&[
            ("GIT_INDEX_FILE", decoy_git.join("index")),
            ("GIT_OBJECT_DIRECTORY", decoy_git.join("objects")),
            ("GIT_COMMON_DIR", decoy_git.clone()),
            ("GIT_CONFIG", decoy_git.join("config")),
            ("GIT_CONFIG_COUNT", "1".into()),
            ("GIT_CONFIG_KEY_0", "core.bare".into()),
            ("GIT_CONFIG_VALUE_0", "true".into()),
        ]);
        let stdout = String::from_utf8_lossy(&cleared.stdout);
        assert!(
            cleared.status.success() && stdout.contains("1 passed; 0 failed"),
            "a test's own Git failed under inherited selectors: {cleared:?}"
        );
        assert!(
            files(&decoy_git) == before,
            "a test's own Git wrote the repository an inherited selector names"
        );
    }
}
