//! Process-local Git answers and the writes that end their freshness.

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// A canonical checkout and the administrative directory Git selects for it.
/// Resolving both prevents aliases and nested repositories from sharing answers.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct Repository {
    pub directory: PathBuf,
    pub admin: PathBuf,
}

pub(super) fn repository(cwd: &Path) -> Option<Repository> {
    let cwd = cwd.canonicalize().ok()?;
    for directory in cwd.ancestors() {
        let dot = directory.join(".git");
        let admin = if dot.is_dir() {
            dot
        } else if dot.is_file() {
            let content = std::fs::read_to_string(dot).ok()?;
            directory.join(content.trim_end().strip_prefix("gitdir: ")?)
        } else if directory.join("HEAD").is_file() && directory.join("objects").is_dir() {
            directory.to_path_buf()
        } else {
            continue;
        };
        return Some(Repository {
            directory: directory.to_path_buf(),
            admin: admin.canonicalize().ok()?,
        });
    }
    None
}

type Key = (Repository, Vec<String>);
type Slot = Arc<Mutex<Option<Option<Vec<u8>>>>>;

#[derive(Default)]
struct Answers {
    /// Repository locations, successful full-OID resolutions, blobs, trees, and
    /// absent paths proven inside an existing commit's tree remain valid
    /// through Arc's own Git writes. An absent full OID is never stored here
    /// because another process or a later write may create it. Project
    /// commands, lock acquisitions and explicit refreshes discard every entry,
    /// since any may replace a repository or install replacement refs.
    immutable: HashMap<Key, Slot>,
    /// Branch, HEAD and ref answers last until any Git write. Both sides of
    /// that write discard them; none are retained while a write is running.
    /// Lock acquisitions discard them. Authorization observations and deliberate
    /// movement guards discard them before reading, including external moves.
    mutable: HashMap<Key, Slot>,
    writers: usize,
    projects: usize,
}

static ANSWERS: OnceLock<Mutex<Answers>> = OnceLock::new();

fn answers() -> std::sync::MutexGuard<'static, Answers> {
    ANSWERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn optional_bytes(
    cwd: &Path,
    question: &[&str],
    immutable: bool,
    read: impl FnOnce() -> Result<Option<Vec<u8>>>,
) -> Result<Option<Vec<u8>>> {
    maybe_cached_bytes(cwd, question, immutable, || Ok((read()?, true)))
}

/// Answer `question`, retaining the result only when `read` marks it cacheable.
/// Callers use this when a negative answer is only safe to retain after
/// confirming the parent object already exists in the object store.
pub(super) fn maybe_cached_bytes(
    cwd: &Path,
    question: &[&str],
    immutable: bool,
    read: impl FnOnce() -> Result<(Option<Vec<u8>>, bool)>,
) -> Result<Option<Vec<u8>>> {
    let Some(repository) = repository(cwd) else {
        return Ok(read()?.0);
    };
    let slot = {
        let mut held = answers();
        if held.projects != 0 || (!immutable && held.writers != 0) {
            drop(held);
            return Ok(read()?.0);
        }
        let key = (
            repository,
            question.iter().map(|s| (*s).to_owned()).collect(),
        );
        let map = if immutable {
            &mut held.immutable
        } else {
            &mut held.mutable
        };
        Arc::clone(map.entry(key).or_default())
    };
    let mut held = slot.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(answer) = &*held {
        return Ok(answer.clone());
    }
    let (answer, cache) = read()?;
    if cache {
        *held = Some(answer.clone());
    }
    Ok(answer)
}

pub(super) fn text(
    cwd: &Path,
    question: &[&str],
    immutable: bool,
    read: impl FnOnce() -> Result<String>,
) -> Result<String> {
    Ok(String::from_utf8(
        optional_bytes(cwd, question, immutable, || {
            read().map(|s| Some(s.into_bytes()))
        })?
        .expect("text reads return a value"),
    )?)
}

pub(super) fn optional_text(
    cwd: &Path,
    question: &[&str],
    immutable: bool,
    read: impl FnOnce() -> Result<Option<String>>,
) -> Result<Option<String>> {
    maybe_cached_text(cwd, question, immutable, || Ok((read()?, true)))
}

pub(super) fn maybe_cached_text(
    cwd: &Path,
    question: &[&str],
    immutable: bool,
    read: impl FnOnce() -> Result<(Option<String>, bool)>,
) -> Result<Option<String>> {
    maybe_cached_bytes(cwd, question, immutable, || {
        let (answer, cache) = read()?;
        Ok((answer.map(String::into_bytes), cache))
    })?
    .map(String::from_utf8)
    .transpose()
    .map_err(Into::into)
}

pub(super) fn reads_are_stable() -> bool {
    answers().writers == 0
}

/// A new observation expires mutable names. Immutable revisions, including
/// absent paths, keep their write freshness rules across observations.
pub(super) fn forget_mutable() {
    answers().mutable.clear();
}

/// Explicit refreshes discard every answer, including repository locations.
pub(super) fn forget() {
    let mut held = answers();
    held.mutable.clear();
    held.immutable.clear();
}

#[derive(Clone, Copy)]
enum Effect {
    GitWrite,
    Project,
}

/// A write excludes cache publication until its scope ends. Git writes expire
/// mutable answers; project commands expire every answer, on entry and exit,
/// including errors and unwinding. Concurrent readers cannot retain an answer
/// obtained while a child may still be changing its repository.
pub(crate) struct WriteScope(Effect);

impl WriteScope {
    pub(crate) fn project() -> Self {
        Self::new(Effect::Project)
    }

    fn new(effect: Effect) -> Self {
        {
            let mut held = answers();
            held.writers += 1;
            held.mutable.clear();
            // Retain populated immutable entries (including absent paths proven
            // inside an existing commit's tree) and prune unpopulated slots.
            held.immutable
                .retain(|_, slot| slot.try_lock().is_ok_and(|answer| answer.is_some()));
            if matches!(effect, Effect::Project) {
                held.projects += 1;
                held.immutable.clear();
            }
        }
        super::forget_shared_answers();
        if matches!(effect, Effect::Project) {
            super::forget_object_history();
        }
        Self(effect)
    }
}

impl Drop for WriteScope {
    fn drop(&mut self) {
        {
            let mut held = answers();
            held.writers -= 1;
            held.mutable.clear();
            held.immutable
                .retain(|_, slot| slot.try_lock().is_ok_and(|answer| answer.is_some()));
            if matches!(self.0, Effect::Project) {
                held.projects -= 1;
                held.immutable.clear();
            }
        }
        super::forget_shared_answers();
        if matches!(self.0, Effect::Project) {
            super::forget_object_history();
        }
    }
}

/// Observational Git invocations preserve answers. Other Git invocations
/// expire mutable answers; executable hooks and project commands expire all.
pub(crate) fn command_scope(command: &Command) -> Option<WriteScope> {
    let program = Path::new(command.get_program()).file_name()?;
    if program != "git" {
        let observer = matches!(program.to_str(), Some("df" | "du" | "findmnt"))
            || (program == "arc" && command.get_args().eq([std::ffi::OsStr::new("--version")]));
        return (!observer).then(WriteScope::project);
    }
    let mut args = command.get_args().map(|s| s.to_string_lossy());
    let mut subcommand = None;
    while let Some(arg) = args.next() {
        if matches!(arg.as_ref(), "-c" | "-C" | "--git-dir" | "--work-tree") {
            args.next();
        } else if !arg.starts_with('-') {
            subcommand = Some(arg);
            break;
        }
    }
    let read = match subcommand.as_deref() {
        Some(
            "rev-parse" | "cat-file" | "diff" | "diff-tree" | "log" | "show" | "rev-list"
            | "merge-base" | "merge-tree" | "ls-files" | "ls-tree" | "for-each-ref" | "show-ref"
            | "status" | "describe" | "check-ignore" | "check-attr",
        ) => true,
        Some("symbolic-ref") => {
            let args: Vec<_> = args.collect();
            args.iter().filter(|arg| !arg.starts_with('-')).count() == 1
                && args.iter().all(|arg| {
                    !arg.starts_with('-') || matches!(arg.as_ref(), "-q" | "--quiet" | "--short")
                })
        }
        Some("worktree") => args.next().is_some_and(|s| s == "list"),
        Some("for-each-repo") => {
            args.find(|arg| arg == "--").is_some()
                && args
                    .map(|arg| arg.into_owned())
                    .eq(["status", "--porcelain=v2", "--branch"].map(str::to_owned))
        }
        Some("config") => {
            let args: Vec<_> = args.collect();
            let write = args.iter().any(|arg| {
                matches!(
                    arg.as_ref(),
                    "--add"
                        | "--replace-all"
                        | "--unset"
                        | "--unset-all"
                        | "--rename-section"
                        | "--remove-section"
                        | "--edit"
                        | "-e"
                        | "set"
                        | "unset"
                        | "edit"
                        | "rename-section"
                        | "remove-section"
                )
            });
            !write
                && (args.iter().any(|arg| {
                    matches!(
                        arg.as_ref(),
                        "--get" | "--get-all" | "--get-regexp" | "--list" | "-l" | "get" | "list"
                    )
                }) || args.iter().filter(|arg| !arg.starts_with('-')).count() == 1)
        }
        Some("branch") => args.any(|s| matches!(s.as_ref(), "--list" | "--show-current")),
        _ => false,
    };
    if read {
        return None;
    }
    let changes_resolution = matches!(subcommand.as_deref(), Some("config" | "replace" | "init"))
        || command
            .get_args()
            .any(|arg| arg.to_string_lossy().starts_with("refs/replace/"));
    let cwd = command
        .get_current_dir()
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok());
    let hook = cwd.as_deref().is_some_and(|cwd| {
        text(cwd, &["hooks_directory"], true, || {
            super::git_path(cwd, "hooks").map(|path| path.to_string_lossy().into_owned())
        })
        .is_ok_and(|hooks| {
            use std::os::unix::fs::PermissionsExt;
            [
                "applypatch-msg",
                "pre-applypatch",
                "post-applypatch",
                "pre-commit",
                "pre-merge-commit",
                "prepare-commit-msg",
                "commit-msg",
                "post-commit",
                "pre-rebase",
                "post-checkout",
                "post-merge",
                "pre-push",
                "post-rewrite",
                "reference-transaction",
                "post-index-change",
            ]
            .iter()
            .any(|name| {
                std::fs::metadata(Path::new(&hooks).join(name)).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
        })
    });
    Some(WriteScope::new(if changes_resolution || hook {
        Effect::Project
    } else {
        Effect::GitWrite
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitio::{
        self, blob_bytes, commit_tree, common_dir, current_branch, file_at, head, ref_value,
        rev_parse, toplevel, worktree_admin_dir,
    };
    use crate::perf::Counted;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        gitio::git(&root, &["init", "-b", "master"]).unwrap();
        gitio::git(&root, &["config", "core.hooksPath", "/dev/null"]).unwrap();
        commit(&root, "first");
        (directory, root)
    }

    fn commit(root: &Path, text: &str) {
        std::fs::write(root.join("file"), text).unwrap();
        gitio::git(root, &["add", "file"]).unwrap();
        gitio::git(
            root,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=tester@arc-fixture.dev",
                "commit",
                "-m",
                text,
            ],
        )
        .unwrap();
    }

    /// Claim: aliases share the invoking checkout's answers, while a nested
    /// repository has its own. Rejects raw path keys and relative common-dir
    /// answers interpreted relative to a later caller's subdirectory.
    #[test]
    fn canonical_paths_keep_repository_identity() {
        let (directory, root) = fixture();
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let subdir = root.join("subdir");
        std::fs::create_dir(&subdir).unwrap();
        for cwd in [&root, &alias, &subdir] {
            assert_eq!(
                text(cwd, &["canonical-test"], true, || { Ok("answer".into()) }).unwrap(),
                "answer"
            );
            assert_eq!(common_dir(cwd).unwrap(), root.join(".git"));
            assert_eq!(worktree_admin_dir(cwd).unwrap(), root.join(".git"));
            assert_eq!(toplevel(cwd).unwrap(), root);
        }
        assert_eq!(repository(&alias), repository(&root));
        assert_eq!(repository(&subdir), repository(&root));
        gitio::git(&subdir, &["init", "-b", "nested"]).unwrap();
        assert_eq!(
            text(&subdir, &["canonical-test"], true, || Ok("nested".into())).unwrap(),
            "nested"
        );
        assert_eq!(common_dir(&subdir).unwrap(), subdir.join(".git"));
        assert_eq!(toplevel(&subdir).unwrap(), subdir);
    }

    /// Invariant I8. Claim: a commit, checkout, ref move and ref deletion are visible to the
    /// next HEAD, branch and ref reads in the same invocation. Rejects answers
    /// retained from before Arc wrote Git state, including a previously absent ref.
    #[test]
    fn git_writes_refresh_head_branch_and_refs() {
        let (_directory, root) = fixture();
        let first = head(&root).unwrap();
        assert_eq!(gitio::head_if_present(&root).unwrap(), Some(first.clone()));
        assert_eq!(current_branch(&root).unwrap().as_deref(), Some("master"));
        assert_eq!(ref_value(&root, "refs/heads/later").unwrap(), None);
        commit(&root, "second");
        let second = head(&root).unwrap();
        assert_ne!(first, second, "HEAD must follow the commit");
        assert_eq!(gitio::head_if_present(&root).unwrap(), Some(second.clone()));
        gitio::create_branch(&root, "later", &first).unwrap();
        assert_eq!(
            ref_value(&root, "refs/heads/later").unwrap(),
            Some(first.clone())
        );
        assert_eq!(rev_parse(&root, "later").unwrap(), first);
        assert_eq!(gitio::branch_head(&root, "later").unwrap(), first);
        gitio::update_ref(&root, "refs/heads/later", &second).unwrap();
        assert_eq!(rev_parse(&root, "later").unwrap(), second);
        assert_eq!(gitio::branch_head(&root, "later").unwrap(), second);
        assert_eq!(
            ref_value(&root, "refs/heads/later").unwrap(),
            Some(second.clone())
        );
        gitio::checkout(&root, "later").unwrap();
        assert_eq!(current_branch(&root).unwrap().as_deref(), Some("later"));
        gitio::checkout(&root, "master").unwrap();
        gitio::delete_ref(&root, "refs/heads/later").unwrap();
        assert_eq!(ref_value(&root, "refs/heads/later").unwrap(), None);
    }

    /// Claim: immutable file, blob and tree answers remain tied to their full
    /// revision, while names follow later commits. Rejects a file or tree read
    /// from the earlier HEAD and a missing object cached before its creation.
    #[test]
    fn object_reads_follow_the_resolved_revision() {
        let (_directory, root) = fixture();
        let first = head(&root).unwrap();
        let tree = commit_tree(&root, "HEAD").unwrap();
        let blob = gitio::blob_oid(&root, &first, "file").unwrap();
        assert_eq!(
            file_at(&root, "HEAD", "file").unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(
            file_at(&root, &first, "file").unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(blob_bytes(&root, &blob).unwrap(), b"first");
        assert_eq!(file_at(&root, &first, "absent").unwrap(), None);
        commit(&root, "second");
        assert_eq!(
            file_at(&root, "HEAD", "file").unwrap().as_deref(),
            Some("second")
        );
        assert_ne!(commit_tree(&root, "HEAD").unwrap(), tree);
        assert_eq!(
            file_at(&root, &first, "file").unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(blob_bytes(&root, &blob).unwrap(), b"first");
        assert_eq!(commit_tree(&root, &first).unwrap(), tree);
        let missing = "0".repeat(40);
        assert!(rev_parse(&root, &missing).is_err());
        assert_eq!(ref_value(&root, &missing).unwrap(), Some(missing));
    }

    /// Invariant I8. Claim: a project command discards immutable object answers even when it
    /// fails. Rejects stale blobs and trees after that command installs a Git
    /// replacement object under an already-read full revision.
    #[test]
    fn failed_project_commands_discard_immutable_answers() {
        let (_directory, root) = fixture();
        let first = head(&root).unwrap();
        commit(&root, "second");
        let second = head(&root).unwrap();
        let first_blob = gitio::blob_oid(&root, &first, "file").unwrap();
        let second_blob = gitio::blob_oid(&root, &second, "file").unwrap();
        assert_eq!(blob_bytes(&root, &first_blob).unwrap(), b"first");
        assert_eq!(
            file_at(&root, &first, "file").unwrap().as_deref(),
            Some("first")
        );
        let tree = commit_tree(&root, &first).unwrap();
        let mut command = crate::child_command::command("sh");
        command.current_dir(&root).args([
            "-c",
            "git replace \"$1\" \"$2\"; git replace \"$3\" \"$4\"; exit 7",
            "sh",
            &first,
            &second,
            &first_blob,
            &second_blob,
        ]);
        assert_eq!(command.counted_output().unwrap().status.code(), Some(7));
        assert_eq!(
            file_at(&root, &first, "file").unwrap().as_deref(),
            Some("second")
        );
        assert_ne!(commit_tree(&root, &first).unwrap(), tree);
        assert_eq!(blob_bytes(&root, &first_blob).unwrap(), b"second");
    }

    /// Claim: reads performed during a project command are not retained after
    /// it ends. Rejects publishing an intermediate HEAD as the later answer.
    #[test]
    fn project_scopes_do_not_publish_inflight_answers() {
        let (_directory, root) = fixture();
        let first = head(&root).unwrap();
        {
            let _scope = WriteScope::project();
            assert_eq!(head(&root).unwrap(), first);
            commit(&root, "second");
            assert_ne!(head(&root).unwrap(), first);
        }
        assert_ne!(head(&root).unwrap(), first);
    }
    /// Invariant I8. Claim: status reads after a gate observe the commit the gate made.
    /// Rejects a pre-gate HEAD, tree or dirty worktree answer surviving execution.
    #[test]
    fn gate_commit_refreshes_status_reads() {
        let (_directory, root) = fixture();
        let before = head(&root).unwrap();
        let tree = commit_tree(&root, "HEAD").unwrap();
        let command = "printf second > file && git add file && git -c commit.gpgsign=false -c user.name=Tester -c user.email=tester@arc-fixture.dev commit -qm gate && printf gate-identity";
        let _confinement = crate::confinement::Scope::new(&None);
        crate::gates::environment_probe(&root, command, Some(10)).unwrap();
        assert_ne!(
            head(&root).unwrap(),
            before,
            "gate commit must reach the status read"
        );
        assert_ne!(commit_tree(&root, "HEAD").unwrap(), tree);
        assert_eq!(
            file_at(&root, "HEAD", "file").unwrap().as_deref(),
            Some("second")
        );
        assert_eq!(gitio::dirt(&root).unwrap(), gitio::Dirt::default());
    }

    /// Claim: a full commit ID missing before an object write resolves after it.
    /// Rejects a cached failure that hides an object Arc created in this invocation.
    #[test]
    fn full_id_absence_expires_when_the_object_is_written() {
        let (_directory, root) = fixture();
        let mut raw = gitio::git_bytes(&root, &["cat-file", "commit", "HEAD"]).unwrap();
        raw.extend_from_slice(b"another message\n");
        let id = String::from_utf8(
            gitio::git_piped(&root, &["hash-object", "-t", "commit", "--stdin"], &raw).unwrap(),
        )
        .unwrap();
        let id = id.trim();
        assert!(rev_parse(&root, id).is_err());
        assert_eq!(file_at(&root, id, "file").unwrap(), None);
        gitio::git_piped(
            &root,
            &["hash-object", "-w", "-t", "commit", "--stdin"],
            &raw,
        )
        .unwrap();
        assert_eq!(rev_parse(&root, id).unwrap(), id);
        assert_eq!(
            file_at(&root, id, "file").unwrap().as_deref(),
            Some("first")
        );
    }
    /// Claim: a Git write that runs a repository hook discards immutable answers.
    /// Rejects a stale full-revision file after a reference hook installs a replacement.
    #[test]
    fn reference_hooks_expire_immutable_answers() {
        use std::os::unix::fs::PermissionsExt;
        let (_directory, root) = fixture();
        let first = head(&root).unwrap();
        commit(&root, "second");
        let second = head(&root).unwrap();
        let hooks = root.join(".git/active-hooks");
        std::fs::create_dir(&hooks).unwrap();
        let hook = hooks.join("reference-transaction");
        std::fs::write(&hook, format!("#!/bin/sh\nif test \"$1\" = committed && test ! -e .git/hook-ran; then touch .git/hook-ran; git replace {first} {second}; fi\n")).unwrap();
        std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        gitio::git(
            &root,
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        )
        .unwrap();
        assert_eq!(
            file_at(&root, &first, "file").unwrap().as_deref(),
            Some("first")
        );
        gitio::update_ref(&root, "refs/arc/test", &second).unwrap();
        assert_eq!(
            file_at(&root, &first, "file").unwrap().as_deref(),
            Some("second")
        );
    }
    /// Claim: project commands refresh shared object streams and immutable range
    /// answers. Rejects an old file or changed-path list after a replacement ref.
    #[test]
    fn shared_readers_and_ranges_refresh_after_project_commands() {
        const CHILD: &str = "ARC_TEST_SHARED_ANSWER_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = crate::child_command::command(std::env::current_exe().unwrap())
                .env(CHILD, "1")
                .args(["--exact", "gitio::answers::tests::shared_readers_and_ranges_refresh_after_project_commands", "--nocapture"])
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        crate::store::share_reads();
        let (_directory, root) = fixture();
        let first = head(&root).unwrap();
        commit(&root, "second");
        let second = head(&root).unwrap();
        gitio::create_branch(&root, "alternative", &first).unwrap();
        gitio::checkout(&root, "alternative").unwrap();
        std::fs::write(root.join("other"), "other").unwrap();
        gitio::git(&root, &["add", "other"]).unwrap();
        commit(&root, "first");
        let alternative = head(&root).unwrap();
        let range = (first, second.clone());
        assert_eq!(
            file_at(&root, &second, "file").unwrap().as_deref(),
            Some("second")
        );
        assert_eq!(
            gitio::changed_paths_of(&root, std::slice::from_ref(&range))[&range],
            vec!["file"]
        );
        let mut command = crate::child_command::command("sh");
        command.current_dir(&root).args([
            "-c",
            "git replace \"$1\" \"$2\"",
            "sh",
            &second,
            &alternative,
        ]);
        assert!(command.counted_output().unwrap().status.success());
        let file = file_at(&root, &second, "file").unwrap();
        let paths = gitio::changed_paths_of(&root, std::slice::from_ref(&range))[&range].clone();
        gitio::close_object_readers();
        assert_eq!(
            (file, paths),
            (Some("first".to_owned()), vec!["other".to_owned()])
        );
    }
    /// Invariant I8. Claim: acquiring an uncontended lock observes a ref move
    /// made by a holder that already released it. Rejects preserving a pre-lock
    /// answer merely because this caller never saw contention.
    #[test]
    fn uncontended_lock_refreshes_the_previous_holders_ref_move() {
        let (_directory, root) = fixture();
        let first = head(&root).unwrap();
        commit(&root, "second");
        let second = head(&root).unwrap();
        assert_eq!(gitio::branch_head(&root, "master").unwrap(), second);
        let path = root.join(".git/previous-holder.lock");
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        holder.lock().unwrap();
        let status = crate::child_command::command("git")
            .current_dir(&root)
            .args(["update-ref", "refs/heads/master", &first, &second])
            .status()
            .unwrap();
        assert!(status.success());
        holder.unlock().unwrap();
        let next = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        crate::store::wait_for_lock(
            &next,
            &path,
            "fixture",
            std::time::Duration::from_secs(1),
            crate::store::LockMode::Exclusive,
        )
        .unwrap();
        assert_eq!(
            head(&root).unwrap(),
            first,
            "the previous holder moved HEAD before this uncontended acquisition"
        );
        assert_eq!(gitio::branch_head(&root, "master").unwrap(), first);
    }

    /// Claim: an absent path at an existing full commit ID is cached across
    /// repeated probes, Git writes and fresh mutable observations, while a
    /// mutable ref like HEAD never returns a stale negative answer.
    #[test]
    fn absent_path_at_existing_full_commit_is_cached_without_staling_mutable_refs() {
        const CHILD: &str = "ARC_TEST_ABSENT_PATH_CACHED_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = crate::child_command::command(std::env::current_exe().unwrap())
                .env(CHILD, "1")
                .args([
                    "--exact",
                    "gitio::answers::tests::absent_path_at_existing_full_commit_is_cached_without_staling_mutable_refs",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (_directory, root) = fixture();
        let first = head(&root).unwrap();
        assert_eq!(file_at(&root, &first, "absent.txt").unwrap(), None);
        let after_first = gitio::processes();
        assert_eq!(file_at(&root, &first, "absent.txt").unwrap(), None);
        assert_eq!(
            gitio::processes(),
            after_first,
            "repeated probe of absent path at existing full commit must not spawn Git"
        );
        assert_eq!(file_at(&root, "HEAD", "absent.txt").unwrap(), None);

        std::fs::write(root.join("absent.txt"), "present now\n").unwrap();
        gitio::git(&root, &["add", "absent.txt"]).unwrap();
        commit(&root, "add absent.txt");

        assert_eq!(
            file_at(&root, "HEAD", "absent.txt").unwrap().as_deref(),
            Some("present now\n"),
            "mutable ref HEAD must observe newly committed path"
        );
        let after_head = gitio::processes();
        assert_eq!(
            gitio::fresh_read(|| file_at(&root, &first, "absent.txt")).unwrap(),
            None
        );
        assert_eq!(
            gitio::processes(),
            after_head,
            "proven absent path at existing full commit survives Git writes and fresh_read"
        );
        gitio::close_object_readers();
    }

    /// Claim: an absent path at a nonexistent full commit ID is never cached as
    /// immutable, so a commit created by another process within one lock span
    /// (whether loose or packed) is observed immediately without invalidation.
    #[test]
    fn absent_path_at_nonexistent_full_commit_observes_external_creation() {
        let (_directory, root) = fixture();
        let base_raw = gitio::git_bytes(&root, &["cat-file", "commit", "HEAD"]).unwrap();
        for (suffix, repack) in [
            (b"external loose\n".as_slice(), false),
            (b"external packed\n", true),
        ] {
            let mut raw = base_raw.clone();
            raw.extend_from_slice(suffix);
            let id = String::from_utf8(
                gitio::git_piped(&root, &["hash-object", "-t", "commit", "--stdin"], &raw).unwrap(),
            )
            .unwrap();
            let id = id.trim().to_string();
            assert_eq!(file_at(&root, &id, "file").unwrap(), None);

            // Write the commit through raw std::process::Command so no Arc
            // WriteScope, lock acquisition or fresh_read fires in this process.
            let mut writer = std::process::Command::new("git")
                .args(["hash-object", "-w", "-t", "commit", "--stdin"])
                .current_dir(&root)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            {
                use std::io::Write;
                writer.stdin.take().unwrap().write_all(&raw).unwrap();
            }
            assert!(writer.wait_with_output().unwrap().status.success());
            if repack {
                assert!(std::process::Command::new("git")
                    .args(["update-ref", "refs/temp/external-pack", &id])
                    .current_dir(&root)
                    .status()
                    .unwrap()
                    .success());
                assert!(std::process::Command::new("git")
                    .args(["repack", "-ad", "-q"])
                    .current_dir(&root)
                    .status()
                    .unwrap()
                    .success());
            }

            assert_eq!(
                file_at(&root, &id, "file").unwrap().as_deref(),
                Some("first"),
                "externally created commit (repack={repack}) must not be hidden by a cached absence"
            );
            assert_eq!(rev_parse(&root, &id).unwrap(), id);
        }
    }

    /// Claim: a missing loose blob or missing subtree object for a tracked path
    /// is never cached as an absent file; restoring the missing object without
    /// triggering invalidation makes `file_at` immediately succeed, and a
    /// symlink intermediate component is proven absent.
    #[test]
    fn missing_blob_or_subtree_never_caches_tracked_file_as_absent() {
        let (_directory, root) = fixture();
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/policy.toml"), "rules = true\n").unwrap();
        std::os::unix::fs::symlink("nested", root.join("link")).unwrap();
        gitio::git(&root, &["add", "nested/policy.toml", "link"]).unwrap();
        gitio::git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=tester@arc-fixture.dev",
                "commit",
                "-m",
                "add nested policy and symlink",
            ],
        )
        .unwrap();
        let commit = head(&root).unwrap();
        let blob_id =
            gitio::git(&root, &["rev-parse", "--verify", "HEAD:nested/policy.toml"]).unwrap();
        let subtree_id = gitio::git(&root, &["rev-parse", "--verify", "HEAD:nested"]).unwrap();

        // Symlink intermediate component is proven absent, matching Git.
        assert_eq!(file_at(&root, &commit, "link/policy.toml").unwrap(), None);

        // Missing loose blob must error (not return Ok(None) or cache absence),
        // and restoring the loose file directly must immediately succeed.
        let loose_blob = root
            .join(".git/objects")
            .join(&blob_id[..2])
            .join(&blob_id[2..]);
        let saved_blob = std::fs::read(&loose_blob).unwrap();
        std::fs::remove_file(&loose_blob).unwrap();
        gitio::close_object_readers();
        assert!(
            file_at(&root, &commit, "nested/policy.toml").is_err(),
            "missing loose blob must fail rather than report absent"
        );
        std::fs::write(&loose_blob, &saved_blob).unwrap();
        assert_eq!(
            file_at(&root, &commit, "nested/policy.toml")
                .unwrap()
                .as_deref(),
            Some("rules = true\n")
        );

        // Missing loose subtree must not cache `None` as immutable; restoring
        // the subtree object directly must immediately return the file.
        forget();
        let loose_tree = root
            .join(".git/objects")
            .join(&subtree_id[..2])
            .join(&subtree_id[2..]);
        let saved_tree = std::fs::read(&loose_tree).unwrap();
        std::fs::remove_file(&loose_tree).unwrap();
        gitio::close_object_readers();
        assert_eq!(file_at(&root, &commit, "nested/policy.toml").unwrap(), None);
        std::fs::write(&loose_tree, &saved_tree).unwrap();
        assert_eq!(
            file_at(&root, &commit, "nested/policy.toml")
                .unwrap()
                .as_deref(),
            Some("rules = true\n")
        );
        gitio::close_object_readers();
    }

    /// Claim: a Git ref write in one worktree invalidates mutable branch and
    /// ref answers held for sibling linked worktrees of the same repository.
    #[test]
    fn cross_worktree_ref_write_invalidates_sibling_worktree_reads() {
        let (directory, root) = fixture();
        let first = head(&root).unwrap();
        let sibling = directory.path().join("sibling");
        gitio::add_worktree_new_branch(&root, &sibling, "side", &first).unwrap();

        assert_eq!(gitio::branch_head(&sibling, "master").unwrap(), first);
        assert!(gitio::branch_exists(&sibling, "side"));
        assert!(!gitio::branch_exists(&sibling, "created-elsewhere"));

        commit(&root, "second");
        let second = head(&root).unwrap();
        gitio::create_branch(&root, "created-elsewhere", &second).unwrap();

        assert_eq!(gitio::branch_head(&sibling, "master").unwrap(), second);
        assert!(gitio::branch_exists(&sibling, "created-elsewhere"));
        assert_eq!(
            gitio::branch_head(&sibling, "created-elsewhere").unwrap(),
            second
        );
        gitio::close_object_readers();
    }
}
