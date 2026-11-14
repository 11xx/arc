//! What every test harness of this crate does before its first test, so that
//! no test, helper or program under test can write a repository or file the
//! suite did not create.
//!
//! The kernel enforces it. The harness confines itself and every process it
//! starts to writing beneath the build's target directory and the system
//! temporary directory, so whatever path a command names, by discovery, by
//! `-C`, by `--git-dir` or by a path it computed, the checkout's repository,
//! the operator's Git configuration and other repositories are out of reach.
//! A kernel without the Landlock rights this needs leaves the harness
//! unconfined with a notice; the invariant runner asks for confinement and
//! refuses to run without it.
//!
//! The environment adds what confinement cannot say. `GIT_DIR` and
//! `GIT_WORK_TREE` mean the suite was started from inside a Git operation, and
//! the harness refuses to run. Every other inherited `GIT_` variable is
//! removed, whatever it selects or traces, and the harness sets the few it
//! needs: discovery never climbs into this checkout or the temporary
//! directory from below, `git init` copies no template, the global
//! configuration is a file this process created and the system file is not
//! read. A helper's own settings for its command still apply. Child-mode
//! switches reach a child only from its parent harness.
//!
//! The unit harness and the CLI harness both compile this file.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[path = "harness_entry/confine.rs"]
mod confine;

/// Variables whose presence makes a harness refuse to start.
pub(crate) const REFUSED: [&str; 2] = ["GIT_DIR", "GIT_WORK_TREE"];

/// The prefix of a variable that switches a test into child mode. One reaches
/// a harness only with the token of the harness that started it.
pub(crate) const CHILD_SWITCH: &str = "ARC_TEST_CHILD_";

/// Set by the invariant runner: a harness that cannot confine its writes
/// refuses to run.
pub(crate) const CONFINEMENT: &str = "ARC_TEST_CONFINEMENT";

/// libtest owns `main`, so the entry runs from `.init_array`: before `main`
/// and before any test thread exists, which is also what makes changing the
/// process environment and restricting the process sound.
#[used]
#[link_section = ".init_array"]
static ENTRY: extern "C" fn() = enter;

extern "C" fn enter() {
    for name in REFUSED {
        if std::env::var_os(name).is_some() {
            refuse(&format!(
                "{name} is set: the tests would reach the repository it names. \
                 Run them outside any Git operation, with {name} unset."
            ));
        }
    }
    let parent = parent_token();
    let dropped: Vec<_> = std::env::vars_os()
        .filter(|(name, value)| {
            let name = name.to_string_lossy();
            name.starts_with("GIT_")
                || (name.starts_with(CHILD_SWITCH)
                    && parent.as_deref() != Some(&*value.to_string_lossy()))
        })
        .map(|(name, _)| name)
        .collect();
    for name in dropped {
        std::env::remove_var(name);
    }
    std::env::set_var(
        "GIT_CEILING_DIRECTORIES",
        std::env::join_paths(discovery_ceilings()).expect("ceiling paths hold no separator"),
    );
    std::env::set_var("GIT_CONFIG_GLOBAL", own_global_config());
    std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    std::env::set_var("GIT_TEMPLATE_DIR", "");
    std::env::set_var("GIT_EDITOR", "true");
    std::env::set_var("GIT_SEQUENCE_EDITOR", "true");
    let confinement = confine_writes();
    CONFINED.get_or_init(|| confinement.is_ok());
    if let Err(reason) = confinement {
        if std::env::var_os(CONFINEMENT).is_some_and(|value| value == "required") {
            refuse(&format!(
                "the tests cannot confine their writes, which {CONFINEMENT} requires: {reason}"
            ));
        }
        eprintln!("notice: the tests run unconfined, guarded by their environment only: {reason}");
    }
}

static CONFINED: OnceLock<bool> = OnceLock::new();

/// Whether the kernel confines this process's writes.
pub(crate) fn confined() -> bool {
    CONFINED.get().copied().unwrap_or(false)
}

fn refuse(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

/// The directories Git discovery must not climb into from below, resolved,
/// since Git compares them with the resolved working directory.
pub(crate) fn discovery_ceilings() -> Vec<PathBuf> {
    [checkout(), std::env::temp_dir()]
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect()
}

/// The checkout this suite was built from.
pub(crate) fn checkout() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The build's target directory: the nearest ancestor of this executable that
/// Cargo marked as its cache.
pub(crate) fn target_directory() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    exe.ancestors()
        .find(|dir| dir.join("CACHEDIR.TAG").is_file())
        .map(Path::to_path_buf)
}

/// Writes go beneath the build's target directory and the temporary directory,
/// and to the device nodes a process opens for writing.
fn confine_writes() -> Result<(), String> {
    let target = target_directory().ok_or("the build's target directory is not marked")?;
    let temporary = std::env::temp_dir()
        .canonicalize()
        .map_err(|error| format!("the temporary directory: {error}"))?;
    confine::confine(
        &[&target, &temporary],
        &[
            Path::new("/dev/null"),
            Path::new("/dev/tty"),
            Path::new("/dev/pts"),
        ],
    )
}

/// A global configuration file in a directory this process created, under a
/// fresh name no other process can have chosen, and removed when it exits.
fn own_global_config() -> PathBuf {
    static OWN: OnceLock<PathBuf> = OnceLock::new();
    extern "C" fn remove_own() {
        if let Some(own) = OWN.get() {
            let _ = std::fs::remove_dir_all(own);
        }
    }
    let own = tempfile::Builder::new()
        .prefix("arc-test-git-global-")
        .tempdir()
        .unwrap_or_else(|error| refuse(&format!("the harness's global configuration: {error}")))
        .keep();
    let file = own.join("gitconfig");
    if OWN.set(own).is_ok() {
        // SAFETY: remove_own is an extern "C" function taking no arguments.
        unsafe { libc::atexit(remove_own) };
    }
    file
}

/// This process's identity as a parent: its PID and the kernel's start time
/// for it, so a recycled PID never matches.
pub(crate) fn child_token() -> String {
    token_of(std::process::id() as libc::pid_t).expect("this process has a start time")
}

/// `name` with the token that switches the harness this process starts into
/// child mode.
pub(crate) fn child_switch(name: &str) -> (&str, String) {
    assert!(
        name.starts_with(CHILD_SWITCH),
        "{name} is not a child switch"
    );
    (name, child_token())
}

/// Whether this harness runs in the child mode `name`, which the entry kept
/// only if this process's parent is a harness of the same executable that set
/// it.
pub(crate) fn in_child_mode(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn parent_token() -> Option<String> {
    // SAFETY: getppid has no preconditions and cannot fail.
    let parent = unsafe { libc::getppid() };
    let same = std::fs::read_link(format!("/proc/{parent}/exe")).ok()?
        == std::fs::read_link("/proc/self/exe").ok()?;
    same.then(|| token_of(parent)).flatten()
}

fn token_of(pid: libc::pid_t) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 22, the start time, counted from the field after the command
    // name, which may itself hold spaces and parentheses.
    let started = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
    Some(format!("{pid}:{started}"))
}

#[cfg(test)]
mod tests {
    use super::{child_switch, in_child_mode};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const CHILD: &str = "ARC_TEST_CHILD_HARNESS_ENTRY";
    const SELF: &str =
        "harness_entry::tests::this_harness_keeps_its_git_away_from_an_inherited_repository";
    const PROBE: &str = "ARC_TEST_CHILD_SWITCH_PROBE";

    /// Git without automatic maintenance, which would otherwise change a
    /// repository in the background after a commit.
    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(["-c", "maintenance.auto=false"])
            .args(args)
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
    /// test did not create. Started with `GIT_DIR` alone, or `GIT_WORK_TREE`
    /// alone, naming another repository, it refuses before any test and names
    /// that variable. Started with any other inherited `GIT_` variable naming
    /// it, including a trace target and a template whose hook writes there, a
    /// test's own `git init`, which copies no template, `config`, `add` and
    /// `commit` run in the test's directory. Rejects a harness that runs under either refused variable,
    /// or passes an inherited `GIT_` variable to its tests' Git, and so
    /// writes the other repository's config, refs, index, objects or files.
    #[test]
    fn this_harness_keeps_its_git_away_from_an_inherited_repository() {
        if in_child_mode(CHILD) {
            let own = tempfile::TempDir::new().unwrap();
            git(own.path(), &["init", "-q", "repo"]);
            let repo = own.path().join("repo");
            assert!(
                !repo.join(".git/hooks").exists(),
                "git init copied a template"
            );
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
        git(&decoy, &["init", "-q"]);
        git(&decoy, &["config", "user.name", "Decoy"]);
        git(&decoy, &["config", "user.email", "decoy@example.invalid"]);
        git(&decoy, &["config", "commit.gpgsign", "false"]);
        std::fs::write(decoy.join("decoy"), "decoy\n").unwrap();
        git(&decoy, &["add", "decoy"]);
        git(&decoy, &["commit", "-q", "-m", "decoy"]);
        let decoy_git = decoy.join(".git");
        let template = scratch.path().join("template");
        std::fs::create_dir_all(template.join("hooks")).unwrap();
        let hook = template.join("hooks/post-commit");
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\necho hooked >> '{}'\n",
                decoy_git.join("hooked").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let before = files(&decoy_git);
        let run = |envs: &[(&str, PathBuf)]| {
            let (switch, token) = child_switch(CHILD);
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", SELF, "--nocapture"])
                .env(switch, token);
            for (name, value) in envs {
                child.env(name, value);
            }
            child.output().unwrap()
        };

        for (name, value) in [
            ("GIT_DIR", decoy_git.clone()),
            ("GIT_WORK_TREE", decoy.clone()),
        ] {
            let refused = run(&[(name, value)]);
            assert!(
                files(&decoy_git) == before,
                "a harness under {name} wrote the repository it names"
            );
            let stderr = String::from_utf8_lossy(&refused.stderr);
            assert!(
                !refused.status.success() && stderr.contains(&format!("{name} is set")),
                "a harness under {name} ran: {refused:?}"
            );
        }

        let cleared = run(&[
            ("GIT_INDEX_FILE", decoy_git.join("index")),
            ("GIT_OBJECT_DIRECTORY", decoy_git.join("objects")),
            ("GIT_COMMON_DIR", decoy_git.clone()),
            ("GIT_CONFIG", decoy_git.join("config")),
            ("GIT_CONFIG_COUNT", "1".into()),
            ("GIT_CONFIG_KEY_0", "core.bare".into()),
            ("GIT_CONFIG_VALUE_0", "true".into()),
            ("GIT_TRACE2_EVENT", decoy_git.join("trace2.json")),
            ("GIT_TEMPLATE_DIR", template.clone()),
        ]);
        let stdout = String::from_utf8_lossy(&cleared.stdout);
        assert!(
            cleared.status.success() && stdout.contains("1 passed; 0 failed"),
            "a test's own Git failed under inherited variables: {cleared:?}"
        );
        assert!(
            files(&decoy_git) == before,
            "a test's own Git wrote the repository an inherited variable names"
        );
    }

    /// Claim: this harness's writes are confined by the kernel: creating a
    /// file in the checkout it was built from fails, though the checkout is
    /// the working directory every test starts in. Rejects a unit test, or a
    /// production helper a unit test calls, writing the checkout or its
    /// repository by any path it names.
    #[test]
    fn this_harness_cannot_write_beneath_the_checkout() {
        assert!(super::confined(), "this harness runs unconfined");
        let probe = super::checkout().join(format!(".arc-confinement-{}", std::process::id()));
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe);
        if created.is_ok() {
            let _ = std::fs::remove_file(&probe);
        }
        assert_eq!(
            created.map(|_| ()).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::PermissionDenied),
            "a write beneath the checkout"
        );
    }

    /// What `child_switch_probe` saw of its switch, started by `launch`.
    fn probe(launch: &mut Command) -> String {
        let output = launch.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{output:?}");
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("child switch: "))
            .unwrap_or_else(|| panic!("the probe did not report: {stdout}"))
            .to_owned()
    }

    /// Reports whether its child switch reached it; launched only by
    /// `a_child_switch_reaches_a_child_only_from_its_parent`.
    #[test]
    fn child_switch_probe() {
        let kept = if in_child_mode(PROBE) {
            "kept"
        } else {
            "dropped"
        };
        println!("child switch: {kept}");
    }

    /// Claim: a child-mode switch reaches a harness only from the harness that
    /// started it, with that parent's token. A forged token, or the parent's
    /// own token arriving through another process as an exported variable
    /// would, is dropped before any test reads it. Rejects an ambient switch
    /// putting an isolation witness into child mode, where it runs fixture
    /// operations without building the poisoned child it exists to check.
    #[test]
    fn a_child_switch_reaches_a_child_only_from_its_parent() {
        let exe = std::env::current_exe().unwrap();
        let name = "harness_entry::tests::child_switch_probe";
        let launch = |token: &str| {
            let mut command = Command::new(&exe);
            command
                .args(["--exact", name, "--nocapture"])
                .env(PROBE, token);
            command
        };
        let (_, token) = child_switch(PROBE);
        assert_eq!(probe(&mut launch(&token)), "kept");
        assert_eq!(probe(&mut launch("1:0")), "dropped", "a forged token");
        let mut relayed = Command::new("sh");
        relayed
            .args(["-c", "\"$0\" \"$@\"; exit $?"])
            .arg(&exe)
            .args(["--exact", name, "--nocapture"])
            .env(PROBE, &token);
        assert_eq!(
            probe(&mut relayed),
            "dropped",
            "a token relayed by another process"
        );
    }
}
