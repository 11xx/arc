//! What every test harness of this crate does before its first test, so that
//! no test, helper or program under test reaches, by accident, a repository or
//! file the suite did not create.
//!
//! The kernel enforces it. The harness confines itself and every process it
//! starts to writing beneath the build's target directory and the system
//! temporary directory, `/tmp`, so whatever path a command names, by
//! discovery, by `-C`, by `--git-dir` or by a path it computed, the
//! checkout's repository, the operator's Git configuration and other
//! repositories are out of reach. The environment never chooses a writable
//! root: an inherited `TMPDIR` naming anything but `/tmp` is replaced by a
//! directory the harness creates there. A target directory that is, or
//! contains, the checkout, the home directory or the directory holding the
//! checkout's shared Git directory, that lies inside that Git directory, in
//! the checkout other than within its `target`, or in the home's
//! configuration, is refused rather than granted. Each comparison is between
//! fully resolved paths, every symbolic link followed on both sides; a
//! protected path that does not exist protects nothing there, and one that
//! cannot be resolved refuses the root. Descriptors the harness inherited
//! above standard error are closed and standard input is read-only, since a
//! descriptor opened before confinement keeps its access. A kernel without
//! the Landlock rights this needs, a refused root, a descriptor the harness
//! cannot close or replace, or any failed call on the way to confinement
//! leaves the harness unconfined with a notice naming why; the invariant
//! runner asks for confinement and the harness then refuses to run.
//!
//! Two limits stand. A machine prepared in advance to defeat the guard, with
//! a hard link or bind mount planted inside the build directory before the
//! run, is outside it; and Landlock does not restrict metadata-only changes:
//! mode, ownership, timestamps and extended attributes. `arc doctor` and
//! `arc catchup` still report a contaminated repository configuration.
//!
//! The environment adds what confinement cannot say. `GIT_DIR` and
//! `GIT_WORK_TREE` mean the suite was started from inside a Git operation, and
//! the harness refuses to run. Every other inherited `GIT_` variable is
//! removed, whatever it selects or traces, and the harness sets the few it
//! needs: discovery never climbs into this checkout or `/tmp` from below,
//! `git init` copies no template, the global configuration is a file this
//! process created and the system file is not read. A helper's own settings for its command still apply. Child-mode
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
/// process environment, its descriptors and its restriction sound.
#[used]
#[link_section = ".init_array"]
static ENTRY: extern "C" fn() = enter;

extern "C" fn enter() {
    let descriptors = close_inherited_descriptors();
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
    let temporary = own_temporary_directory();
    let ceilings = discovery_ceilings();
    std::env::set_var(
        "GIT_CEILING_DIRECTORIES",
        std::env::join_paths(ceilings.as_deref().unwrap_or_default())
            .expect("ceiling paths hold no separator"),
    );
    // Without a global configuration of its own, Git reads an empty one and
    // cannot write one.
    let global = temporary.clone().and_then(|_| own_global_config());
    std::env::set_var(
        "GIT_CONFIG_GLOBAL",
        global.as_deref().unwrap_or(Path::new("/dev/null")),
    );
    let roots = descriptors
        .and(ceilings)
        .and(global)
        .and(temporary)
        .and_then(writable_roots);
    std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    std::env::set_var("GIT_TEMPLATE_DIR", "");
    std::env::set_var("GIT_EDITOR", "true");
    std::env::set_var("GIT_SEQUENCE_EDITOR", "true");
    let confinement = roots.and_then(|roots| {
        let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
        confine::confine(&roots)
    });
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

/// Close every descriptor above standard error and make standard input
/// read-only: a descriptor opened before confinement keeps the access it was
/// opened with, and the harness needs none of them. What Cargo passes, its
/// jobserver among it, serves the build, not the tests. Where `close_range`
/// is refused, each descriptor `/proc/self/fd` lists is closed instead. Only
/// `EBADF` establishes that a descriptor is absent; one left open, a standard
/// input still writable, or any failed listing or status query is a failure
/// to confine.
fn close_inherited_descriptors() -> Result<(), String> {
    // SAFETY: close_range takes integer bounds and closes only descriptors.
    if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) } != 0 {
        let refused = std::io::Error::last_os_error();
        let listing = |error: &dyn std::fmt::Display| {
            format!("close_range: {refused}; listing /proc/self/fd: {error}")
        };
        let mut listed = Vec::new();
        for entry in std::fs::read_dir("/proc/self/fd").map_err(|error| listing(&error))? {
            let name = entry.map_err(|error| listing(&error))?.file_name();
            let fd: libc::c_int = name
                .to_str()
                .and_then(|name| name.parse().ok())
                .ok_or_else(|| listing(&format!("{name:?} names no descriptor")))?;
            if fd > 2 {
                listed.push(fd);
            }
        }
        for fd in listed {
            // SAFETY: closing a descriptor number; the listing's own, already
            // closed, fails with EBADF.
            if unsafe { libc::close(fd) } == 0 {
                continue;
            }
            let error = std::io::Error::last_os_error();
            match descriptor_status(fd, libc::F_GETFD) {
                Ok(None) => {}
                Ok(Some(_)) => {
                    return Err(format!(
                    "inherited descriptor {fd} stays open: close_range: {refused}; close: {error}"
                ))
                }
                Err(query) => {
                    return Err(format!(
                        "inherited descriptor {fd} may stay open: close_range: {refused}; \
                         close: {error}; {query}"
                    ))
                }
            }
        }
    }
    let flags = match descriptor_status(0, libc::F_GETFL) {
        Ok(Some(flags)) => flags,
        Ok(None) => return Ok(()),
        Err(query) => return Err(format!("standard input may be writable: {query}")),
    };
    if flags & libc::O_ACCMODE == libc::O_RDONLY {
        return Ok(());
    }
    // SAFETY: the path is NUL-terminated; dup2 replaces descriptor 0 with a
    // read-only /dev/null, and the extra descriptor is closed after.
    unsafe {
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
        if null < 0 {
            let error = std::io::Error::last_os_error();
            return Err(format!("standard input stays writable: /dev/null: {error}"));
        }
        if libc::dup2(null, 0) != 0 {
            let error = std::io::Error::last_os_error();
            libc::close(null);
            return Err(format!("standard input stays writable: {error}"));
        }
        if libc::close(null) != 0 {
            let error = std::io::Error::last_os_error();
            return Err(format!("a read-only /dev/null stays open: {error}"));
        }
    }
    Ok(())
}

/// `fcntl(fd, query)` for a status query: the answer, `None` when `EBADF`
/// establishes that `fd` is not open, or the error of any other failure.
fn descriptor_status(fd: libc::c_int, query: libc::c_int) -> Result<Option<libc::c_int>, String> {
    // SAFETY: F_GETFD and F_GETFL only read a descriptor's flags.
    let answer = unsafe { libc::fcntl(fd, query) };
    if answer >= 0 {
        return Ok(Some(answer));
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EBADF) => Ok(None),
        _ => Err(format!("querying descriptor {fd}: {error}")),
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
/// since Git compares them with the resolved working directory. One that does
/// not exist has nothing beneath it to discover.
pub(crate) fn discovery_ceilings() -> Result<Vec<PathBuf>, String> {
    let mut ceilings = Vec::new();
    for ceiling in [checkout(), PathBuf::from(SYSTEM_TEMPORARY)] {
        ceilings.extend(resolve(&ceiling)?);
    }
    Ok(ceilings)
}

/// The checkout this suite was built from.
pub(crate) fn checkout() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The build's target directory: the nearest ancestor of this executable that
/// Cargo marked as its cache. A marker that cannot be looked up is an error,
/// not a reason to climb past it to a wider directory.
pub(crate) fn target_directory() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|error| format!("this executable: {error}"))?;
    for dir in exe.ancestors().skip(1) {
        let marker = dir.join("CACHEDIR.TAG");
        match std::fs::metadata(&marker) {
            Ok(metadata) if metadata.is_file() => return Ok(dir.to_path_buf()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", marker.display())),
        }
    }
    Err("the build's target directory is not marked".to_owned())
}

/// `path` fully resolved, every symbolic link along it followed, or `None`
/// when nothing exists there. Every comparison between a candidate root and
/// a protected path is between resolved paths. A protected path that does not
/// exist protects nothing there; a candidate root must exist. Any other
/// failure to resolve is an error, which refuses the root.
fn resolve(path: &Path) -> Result<Option<PathBuf>, String> {
    match path.canonicalize() {
        Ok(resolved) => Ok(Some(resolved)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("resolving {}: {error}", path.display())),
    }
}

/// A candidate root, resolved; one that does not exist is refused.
fn resolve_root(root: &Path) -> Result<PathBuf, String> {
    resolve(root)?.ok_or_else(|| format!("the writable root {} does not exist", root.display()))
}

/// The system temporary directory, the one writable root the environment
/// does not choose.
pub(crate) const SYSTEM_TEMPORARY: &str = "/tmp";

/// The system temporary directory, resolved. An inherited `TMPDIR` naming
/// anything else is never a root: the harness sets `TMPDIR` to a directory it
/// creates there, removed when it exits, so every test and child uses that.
/// It lives under the system temporary directory rather than the build
/// directory because Unix socket paths a test binds beneath it must stay
/// within their 108 bytes.
fn own_temporary_directory() -> Result<PathBuf, String> {
    static OWN: OnceLock<PathBuf> = OnceLock::new();
    extern "C" fn remove_own() {
        if let Some(own) = OWN.get() {
            let _ = std::fs::remove_dir_all(own);
        }
    }
    let system = Path::new(SYSTEM_TEMPORARY)
        .canonicalize()
        .map_err(|error| format!("{SYSTEM_TEMPORARY}: {error}"))?;
    // An inherited directory that cannot be resolved is not taken for /tmp.
    let inherited = std::env::temp_dir().canonicalize().ok();
    if inherited.as_deref() != Some(system.as_path()) {
        let own = tempfile::Builder::new()
            .prefix("arc-test-tmp-")
            .tempdir_in(&system)
            .map_err(|error| format!("a temporary directory under {SYSTEM_TEMPORARY}: {error}"))?
            .keep();
        std::env::set_var("TMPDIR", &own);
        if OWN.set(own).is_ok() {
            remove_at_exit(remove_own)?;
        }
    }
    Ok(system)
}

/// The roots the harness may write beneath: the build's target directory,
/// checked against what it must never grant, and the system temporary
/// directory.
fn writable_roots(temporary: PathBuf) -> Result<Vec<PathBuf>, String> {
    let target = target_directory()?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let configuration = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    check_target(
        &target,
        &checkout(),
        home.as_deref(),
        configuration.as_deref(),
    )?;
    check_root(&temporary, &checkout(), home.as_deref())?;
    Ok(vec![target, temporary])
}

/// Why the build's target directory `target` cannot be granted: what
/// `root_refusal` says, or it lies inside the checkout other than within
/// Cargo's own build directory, or inside the home's configuration.
pub(crate) fn target_refusal(
    target: &Path,
    checkout: &Path,
    home: Option<&Path>,
    configuration: Option<&Path>,
) -> Option<String> {
    check_target(target, checkout, home, configuration).err()
}

fn check_target(
    target: &Path,
    checkout: &Path,
    home: Option<&Path>,
    configuration: Option<&Path>,
) -> Result<(), String> {
    check_root(target, checkout, home)?;
    let target = resolve_root(target)?;
    if let Some(checkout) = resolve(checkout)? {
        // Cargo's build directory is exempt only where it is itself: a
        // `target` that resolves elsewhere in the checkout exempts nothing.
        let build = checkout.join("target");
        let exempt = resolve(&build)?.filter(|resolved| *resolved == build);
        if target.starts_with(&checkout) && !exempt.is_some_and(|build| target.starts_with(build)) {
            return Err(format!(
                "the target directory {} lies inside the checkout's source tree",
                target.display()
            ));
        }
    }
    let configurations = [
        home.map(|home| home.join(".config")),
        configuration.map(Path::to_path_buf),
    ];
    for configuration in configurations.into_iter().flatten() {
        if let Some(resolved) = resolve(&configuration)? {
            if target.starts_with(&resolved) {
                return Err(format!(
                    "the target directory {} lies inside the home's configuration, {}",
                    target.display(),
                    resolved.display()
                ));
            }
        }
    }
    Ok(())
}

/// Why `root` cannot be granted for writing: it is, or contains, the
/// checkout, the home directory or the directory holding the checkout's
/// shared Git directory, it lies inside that Git directory, or one of them
/// cannot be resolved. `None` when it grants none of them.
pub(crate) fn root_refusal(root: &Path, checkout: &Path, home: Option<&Path>) -> Option<String> {
    check_root(root, checkout, home).err()
}

fn check_root(root: &Path, checkout: &Path, home: Option<&Path>) -> Result<(), String> {
    let root = resolve_root(root)?;
    let checkout = resolve(checkout)?;
    let shared = match &checkout {
        Some(checkout) => shared_git_directory(checkout)?,
        None => None,
    };
    let home = match home {
        Some(home) => resolve(home)?,
        None => None,
    };
    let protected = [
        checkout.map(|checkout| ("the checkout", checkout)),
        home.map(|home| ("the home directory", home)),
        shared.as_ref().and_then(|dir| dir.parent()).map(|holder| {
            (
                "the directory holding the shared Git directory",
                holder.to_path_buf(),
            )
        }),
    ];
    for (what, path) in protected.into_iter().flatten() {
        if path.starts_with(&root) {
            return Err(format!(
                "the writable root {} would grant {what}, {}",
                root.display(),
                path.display()
            ));
        }
    }
    if let Some(shared) = shared.filter(|shared| root.starts_with(shared)) {
        return Err(format!(
            "the writable root {} lies inside the shared Git directory {}",
            root.display(),
            shared.display()
        ));
    }
    Ok(())
}

/// The Git directory every worktree of the resolved `checkout`'s repository
/// shares, resolved, read from the checkout's `.git` without running Git.
/// `None` when the checkout has no `.git`.
fn shared_git_directory(checkout: &Path) -> Result<Option<PathBuf>, String> {
    let dot_git = checkout.join(".git");
    let unreadable = |error: std::io::Error| format!("{}: {error}", dot_git.display());
    match std::fs::metadata(&dot_git) {
        Ok(metadata) if metadata.is_dir() => return resolve(&dot_git),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unreadable(error)),
    }
    let pointer = std::fs::read_to_string(&dot_git).map_err(unreadable)?;
    let own = pointer
        .strip_prefix("gitdir:")
        .map(|own| checkout.join(own.trim()))
        .ok_or_else(|| format!("{} names no Git directory", dot_git.display()))?;
    let commondir = own.join("commondir");
    match std::fs::read_to_string(&commondir) {
        Ok(common) => resolve(&own.join(common.trim())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => resolve(&own),
        Err(error) => Err(format!("{}: {error}", commondir.display())),
    }
}

/// A global configuration file in a directory this process created, under a
/// fresh name no other process can have chosen, and removed when it exits.
fn own_global_config() -> Result<PathBuf, String> {
    static OWN: OnceLock<PathBuf> = OnceLock::new();
    extern "C" fn remove_own() {
        if let Some(own) = OWN.get() {
            // The file, then the directory, so removal needs no listing.
            let _ = std::fs::remove_file(own.join("gitconfig"));
            if std::fs::remove_dir(own).is_err() {
                let _ = std::fs::remove_dir_all(own);
            }
        }
    }
    let own = tempfile::Builder::new()
        .prefix("arc-test-git-global-")
        .tempdir()
        .map_err(|error| format!("the harness's global configuration: {error}"))?
        .keep();
    let file = own.join("gitconfig");
    if OWN.set(own).is_ok() {
        remove_at_exit(remove_own)?;
    }
    Ok(file)
}

/// Register `remove`, which removes a directory this process created, to run
/// when it exits.
fn remove_at_exit(remove: extern "C" fn()) -> Result<(), String> {
    // SAFETY: remove is an extern "C" function taking no arguments.
    match unsafe { libc::atexit(remove) } {
        0 => Ok(()),
        _ => Err("a directory of the harness's own would outlive it".to_owned()),
    }
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
    let identity = |exe: String| {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(exe)
            .ok()
            .map(|file| (file.dev(), file.ino()))
    };
    let same = identity(format!("/proc/{parent}/exe"))? == identity("/proc/self/exe".into())?;
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

    const CONFINED_CHILD: &str = "ARC_TEST_CHILD_UNIT_CONFINEMENT";

    /// Claim: this harness's writes are confined by the kernel: a child
    /// harness linked into a stand-in target directory cannot create a file
    /// beside that directory, outside its roots. Rejects a unit test, or a
    /// production helper a unit test calls, writing a file or repository it
    /// did not create by any path it names.
    #[test]
    fn this_harness_confines_its_writes() {
        if in_child_mode(CONFINED_CHILD) {
            assert!(super::confined(), "this harness runs unconfined");
            let target = super::target_directory().unwrap();
            let outside = target.parent().unwrap().join("outside");
            let created = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(outside);
            assert_eq!(
                created.map(|_| ()).map_err(|error| error.kind()),
                Err(std::io::ErrorKind::PermissionDenied)
            );
            return;
        }
        let scratch_parent = super::target_directory().unwrap().join("tmp");
        std::fs::create_dir_all(&scratch_parent).unwrap();
        let scratch = tempfile::Builder::new()
            .prefix("stand-in-")
            .tempdir_in(scratch_parent)
            .unwrap();
        let target = scratch.path().join("target");
        std::fs::create_dir_all(target.join("deps")).unwrap();
        std::fs::write(
            target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        let harness = target.join("deps/harness");
        std::fs::hard_link(std::env::current_exe().unwrap(), &harness).unwrap();
        let (switch, token) = child_switch(CONFINED_CHILD);
        let output = Command::new(&harness)
            .args([
                "--exact",
                "harness_entry::tests::this_harness_confines_its_writes",
                "--nocapture",
            ])
            .env(switch, token)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            !scratch.path().join("outside").exists(),
            "the child wrote outside its roots"
        );
    }

    /// A decoy checkout as a linked worktree, `home/checkout`, whose shared Git
    /// directory is `home/main/.git`, inside the home `home`.
    fn decoy_worktree() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let scratch = tempfile::TempDir::new().unwrap();
        let home = scratch.path().join("home");
        let main = home.join("main");
        let own = main.join(".git/worktrees/checkout");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("commondir"), "../..\n").unwrap();
        let checkout = home.join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", own.display()),
        )
        .unwrap();
        (scratch, home, main, checkout)
    }

    /// Claim: a writable root that is, or contains, the home directory is
    /// refused, naming what it would grant, and a root beside the home is
    /// accepted. Rejects `CARGO_TARGET_DIR` naming the home, or
    /// a directory above it, turning confinement into a grant of the home.
    #[test]
    fn a_root_granting_the_home_is_refused() {
        let (_scratch, home, _main, checkout) = decoy_worktree();
        let elsewhere = tempfile::TempDir::new().unwrap();
        let outside_checkout = elsewhere.path().join("checkout");
        std::fs::create_dir(&outside_checkout).unwrap();
        for root in [home.clone(), home.parent().unwrap().to_path_buf()] {
            let refusal = super::root_refusal(&root, &outside_checkout, Some(&home));
            assert!(
                refusal
                    .as_deref()
                    .is_some_and(|r| r.contains("the home directory")),
                "{root:?}: {refusal:?}"
            );
        }
        std::fs::create_dir(checkout.join("target")).unwrap();
        assert_eq!(
            super::root_refusal(&checkout.join("target"), &checkout, Some(&home)),
            None
        );
    }

    /// Claim: a writable root that is, or contains, the checkout is refused,
    /// and a root beneath the checkout, as its target directory is, is
    /// accepted. Rejects a root granting the checkout's tracked files.
    #[test]
    fn a_root_granting_the_checkout_is_refused() {
        let (_scratch, _home, _main, checkout) = decoy_worktree();
        for root in [checkout.clone(), checkout.parent().unwrap().to_path_buf()] {
            let refusal = super::root_refusal(&root, &checkout, None);
            assert!(
                refusal
                    .as_deref()
                    .is_some_and(|r| r.contains("the checkout")),
                "{root:?}: {refusal:?}"
            );
        }
        std::fs::create_dir(checkout.join("target")).unwrap();
        assert_eq!(
            super::root_refusal(&checkout.join("target"), &checkout, None),
            None
        );
    }

    /// Claim: a writable root that is, or contains, the directory holding the
    /// checkout's shared Git directory is refused, even when the checkout is
    /// a linked worktree elsewhere. Rejects a root granting the main checkout
    /// whose `.git` every worktree shares.
    #[test]
    fn a_root_granting_the_shared_git_directory_is_refused() {
        let (_scratch, _home, main, checkout) = decoy_worktree();
        let refusal = super::root_refusal(&main, &checkout, None);
        assert!(
            refusal
                .as_deref()
                .is_some_and(|r| r.contains("the directory holding the shared Git directory")),
            "{refusal:?}"
        );
    }

    /// Claim: a writable root inside the checkout's shared Git directory is
    /// refused. Rejects a root granting the repository's configuration, refs
    /// or objects from below.
    #[test]
    fn a_root_inside_the_shared_git_directory_is_refused() {
        let (_scratch, _home, main, checkout) = decoy_worktree();
        let refusal = super::root_refusal(&main.join(".git/worktrees"), &checkout, None);
        assert!(
            refusal
                .as_deref()
                .is_some_and(|r| r.contains("lies inside the shared Git directory")),
            "{refusal:?}"
        );
    }

    /// Claim: a target directory inside the checkout other than within its
    /// `target` build directory is refused, naming the source tree, whether
    /// named directly or through a `target` that is a symbolic link into the
    /// sources, and one within a real `target` is accepted. Rejects
    /// `CARGO_TARGET_DIR=<checkout>/src` granting the checkout's sources,
    /// which contain no protected path, and an exemption compared after
    /// resolution that a symbolic link turns into the source tree.
    #[test]
    fn a_target_inside_the_source_tree_is_refused() {
        let (_scratch, home, _main, checkout) = decoy_worktree();
        std::fs::create_dir_all(checkout.join("src/build")).unwrap();
        std::fs::create_dir_all(checkout.join("target/nested")).unwrap();
        let (_linked_scratch, linked_home, _main, linked) = decoy_worktree();
        std::fs::create_dir_all(linked.join("src/build")).unwrap();
        std::os::unix::fs::symlink("src", linked.join("target")).unwrap();
        for (target, checkout, home) in [
            (checkout.join("src/build"), &checkout, &home),
            (linked.join("target/build"), &linked, &linked_home),
        ] {
            let refusal = super::target_refusal(&target, checkout, Some(home), None);
            assert!(
                refusal
                    .as_deref()
                    .is_some_and(|r| r.contains("inside the checkout's source tree")),
                "{target:?}: {refusal:?}"
            );
        }
        assert_eq!(
            super::target_refusal(
                &checkout.join("target/nested"),
                &checkout,
                Some(&home),
                None
            ),
            None
        );
    }

    /// Claim: a target directory inside the home's configuration, under
    /// `.config` or `XDG_CONFIG_HOME`, is refused, naming it. Rejects
    /// `CARGO_TARGET_DIR=$HOME/.config/git` granting the operator's Git
    /// configuration, which contains no protected path.
    #[test]
    fn a_target_inside_the_home_configuration_is_refused() {
        let (scratch, home, _main, checkout) = decoy_worktree();
        let configuration = scratch.path().join("configuration");
        for dir in [
            home.join(".config/git"),
            configuration.join("cargo"),
            home.join("build"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for target in [home.join(".config/git"), configuration.join("cargo")] {
            let refusal =
                super::target_refusal(&target, &checkout, Some(&home), Some(&configuration));
            assert!(
                refusal
                    .as_deref()
                    .is_some_and(|r| r.contains("inside the home's configuration")),
                "{target:?}: {refusal:?}"
            );
        }
        assert_eq!(
            super::target_refusal(
                &home.join("build"),
                &checkout,
                Some(&home),
                Some(&configuration)
            ),
            None
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

    /// Reports whether this harness confines its writes; launched by the
    /// witnesses that start a harness under a root it must not grant.
    #[test]
    fn confinement_probe() {
        println!("confined: {}", super::confined());
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
