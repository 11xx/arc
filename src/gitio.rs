use crate::commands::fork::is_fork_branch;
use anyhow::{bail, Context, Result};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

thread_local! {
    static COMMAND_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Scope Git subprocesses on this thread to one absolute deadline. Watch uses
/// this so its typed timeout can kill and reap a probe instead of orphaning it.
pub fn with_deadline<T>(deadline: Option<Instant>, f: impl FnOnce() -> Result<T>) -> Result<T> {
    COMMAND_DEADLINE.with(|slot| {
        let previous = slot.replace(deadline);
        let result = f();
        slot.set(previous);
        result
    })
}

/// A Git subprocess with lazy fetching disabled and no way to reach an editor.
///
/// Git launches whatever editor the environment names as soon as it wants a
/// message it was not given: `rebase --continue`, a merge it cannot
/// fast-forward, a commit without one. arc runs Git for an agent, where there
/// is nobody to close a window, and a blocked subprocess is indistinguishable
/// from a slow one. Accepting the message that is already in the file is what
/// every one of these calls means, and `GIT_EDITOR` is what settles it for
/// good: it outranks `core.editor` as well as the environment's own.
///
/// Every Git subprocess arc runs is built here, so there is one answer to
/// this rather than one per call site.
static PROCESSES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many Git commands this process has built.
pub fn processes() -> usize {
    PROCESSES.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn git_command() -> Command {
    PROCESSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut command = Command::new("git");
    command.env("GIT_NO_LAZY_FETCH", "1");
    command.env("GIT_EDITOR", "true");
    command.env("GIT_SEQUENCE_EDITOR", "true");
    command
}

pub fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let mut command = git_command();
    command.args(args).current_dir(cwd);
    let out = command_output(&mut command)
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Run Git and forward its raw output through this process's standard streams.
///
/// Forwarding preserves Git's diff rendering while keeping command output
/// visible to callers that capture Arc itself, including the CLI test harness.
pub fn git_inherit(cwd: &Path, args: &[String]) -> Result<()> {
    let output = git_command()
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    if !output.status.success() {
        bail!("git {} failed with {}", args.join(" "), output.status);
    }
    Ok(())
}

fn command_output(command: &mut Command) -> Result<Output> {
    let deadline = COMMAND_DEADLINE.get();
    let Some(deadline) = deadline else {
        return Ok(command.output()?);
    };

    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    wait_for_output(command.spawn()?, deadline)
}

fn drain_output(
    mut pipe: impl Read + Send + 'static,
) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

/// Drain both pipes while waiting, so a full output buffer cannot hide exit
/// or keep a deadline-bound Git process alive.
fn wait_for_output(mut child: std::process::Child, deadline: Instant) -> Result<Output> {
    let stdout = drain_output(child.stdout.take().context("child has no stdout")?);
    let stderr = drain_output(child.stderr.take().context("child has no stderr")?);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
            }
            answer => {
                let _ = child.kill();
                let _ = child.wait();
                break match answer {
                    Err(error) => Err(error.into()),
                    _ => Err(anyhow::anyhow!("git probe timed out")),
                };
            }
        }
    };
    let output = (|| {
        Ok(Output {
            status: status?,
            stdout: join_before_deadline(stdout, Some(deadline), "stdout reader")??,
            stderr: join_before_deadline(stderr, Some(deadline), "stderr reader")??,
        })
    })();
    if output.is_err() {
        crate::process_group::kill_process_group(child.id())?;
    }
    output
}

fn join_before_deadline<T>(
    worker: thread::JoinHandle<T>,
    deadline: Option<Instant>,
    name: &str,
) -> Result<T> {
    crate::process_group::join_before_deadline(worker, deadline, name)?
        .with_context(|| format!("git probe timed out waiting for {name}"))
}

/// The common Git directory shared by every worktree of one repository. A
/// process that shares its ledger reads asks Git once per directory.
pub fn common_dir(cwd: &Path) -> Result<PathBuf> {
    static RESOLVED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, PathBuf>>,
    > = std::sync::OnceLock::new();
    let shared = crate::store::shares_reads();
    let resolved = RESOLVED.get_or_init(Default::default);
    if shared {
        if let Some(dir) = resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(cwd)
        {
            return Ok(dir.clone());
        }
    }
    let raw = git(cwd, &["rev-parse", "--git-common-dir"])?;
    let p = PathBuf::from(raw);
    let abs = if p.is_absolute() { p } else { cwd.join(p) };
    let dir = std::fs::canonicalize(&abs).unwrap_or(abs);
    if shared {
        resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(cwd.to_path_buf(), dir.clone());
    }
    Ok(dir)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct WorktreeOwner {
    change_id: String,
    token: String,
}

pub fn worktree_admin_dir(checkout: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(git(checkout, &["rev-parse", "--absolute-git-dir"])?)
        .context("cannot resolve worktree administrative directory")
}

/// The registered checkout must resolve to the directory allocated at creation.
pub fn worktree_admin_matches(cwd: &Path, checkout: &Path, admin: &Path) -> bool {
    let Ok(checkout) = std::fs::canonicalize(checkout) else {
        return false;
    };
    worktree_inventory(cwd).is_ok_and(|entries| {
        entries
            .iter()
            .any(|entry| !entry.prunable && entry.path == checkout)
    }) && worktree_admin_dir(&checkout).is_ok_and(|actual| actual == admin)
}

fn worktree_owner_path(checkout: &Path) -> Result<PathBuf> {
    Ok(worktree_admin_dir(checkout)?.join("arc-owner"))
}

/// Bind an arc-created checkout's administrative directory to its change.
pub fn write_worktree_owner(admin: &Path, change_id: &str) -> Result<String> {
    let token = ulid::Ulid::new().to_string();
    write_worktree_owner_with_token(admin, change_id, &token)?;
    Ok(token)
}

/// Publish a reserved local owner token without replacing an existing record.
pub fn write_worktree_owner_with_token(admin: &Path, change_id: &str, token: &str) -> Result<()> {
    let owner = WorktreeOwner {
        change_id: change_id.into(),
        token: token.into(),
    };
    crate::replica::write_exclusive(&admin.join("arc-owner"), &serde_json::to_vec(&owner)?)
}

/// Remove only the owner record belonging to an unrecorded allocation.
pub fn remove_worktree_owner(admin: &Path, change_id: &str, token: &str) -> Result<()> {
    let path = admin.join("arc-owner");
    let owner: WorktreeOwner = serde_json::from_slice(&std::fs::read(&path)?)?;
    if owner.change_id != change_id || owner.token != token {
        bail!(
            "owner record {} changed identity; keeping it",
            path.display()
        );
    }
    std::fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))
}

/// Population errors preserve the allocation and offer shell-safe recovery commands.
pub fn worktree_population_error(branch: &str, path: &Path, adopt: &str) -> String {
    let branch_arg = crate::context::shell_quote(branch);
    let path_arg = crate::context::shell_quote(&path.display().to_string());
    format!(
        "worktree population failed; retained branch {branch} and worktree {}\nadopt: {adopt}\ndiscard: git worktree remove -- {path_arg} && git branch -D -- {branch_arg}\n",
        path.display()
    )
}

/// Ownership requires the change and token in this checkout's own Git directory.
pub fn worktree_owner_matches(checkout: &Path, change_id: &str, token: Option<&str>) -> bool {
    let Some(token) = token else {
        return false;
    };
    let owner = worktree_owner_path(checkout)
        .ok()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<WorktreeOwner>(&bytes).ok());
    owner.is_some_and(|owner| owner.change_id == change_id && owner.token == token)
}

pub fn toplevel(cwd: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"])?))
}

/// Resolve a path under the Git directory, honoring `core.hooksPath` and
/// worktree layout the way Git itself does (e.g. `git_path("hooks")`).
pub fn git_path(cwd: &Path, name: &str) -> Result<PathBuf> {
    let raw = git(cwd, &["rev-parse", "--git-path", name])?;
    let path = PathBuf::from(raw);
    Ok(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

pub fn rev_parse(cwd: &Path, rev: &str) -> Result<String> {
    git(
        cwd,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
}

pub fn head(cwd: &Path) -> Result<String> {
    rev_parse(cwd, "HEAD")
}

pub fn latest_tag(cwd: &Path) -> Result<Option<String>> {
    let output = git_command()
        .args(["describe", "--tags", "--abbrev=0"])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

/// Resolve HEAD when the repository has one. An unborn repository is a
/// normal probe condition, while other Git failures remain errors.
pub fn head_if_present(cwd: &Path) -> Result<Option<String>> {
    let out = git_command()
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    if out.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ));
    }
    if out.status.code() == Some(1) {
        return Ok(None);
    }
    bail!(
        "git rev-parse --verify -q HEAD failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

pub fn branch_head(cwd: &Path, branch: &str) -> Result<String> {
    let name = format!("refs/heads/{branch}");
    if let Some(Object::Found { id, kind, .. }) = shared_object(cwd, &format!("{name}^{{commit}}"))
    {
        if kind == "commit" {
            return Ok(id);
        }
    }
    rev_parse(cwd, &name)
}

pub fn current_branch(cwd: &Path) -> Result<Option<String>> {
    let out = git_command()
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(cwd)
        .output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout)
            .trim()
            .strip_prefix("refs/heads/")
            .map(str::to_string))
    } else {
        Ok(None)
    }
}

/// Repository-relative paths a change touched between two revisions.
pub fn changed_paths(cwd: &Path, base: &str, head: &str) -> Result<Vec<String>> {
    let out = git(cwd, &["diff", "--name-only", &format!("{base}...{head}")])?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

pub fn merge_base(cwd: &Path, a: &str, b: &str) -> Result<String> {
    git(cwd, &["merge-base", a, b])
}

/// What merging one revision into another would produce, computed without a
/// worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    /// Git could not reconcile the text and a person has to.
    pub conflicts: bool,
    /// The tree a clean merge writes. `None` on a conflict, where there is no
    /// single result to name.
    ///
    /// This is the content the merge would ship, and it belongs to neither
    /// side: a change that is behind its target carries a tree no commit on
    /// either branch holds, and so one nothing has been run against.
    pub tree: Option<String>,
}

pub fn merge_outcome(cwd: &Path, target_rev: &str, head_rev: &str) -> Result<MergeOutcome> {
    let out = git_command()
        .args([
            "merge-tree",
            "--write-tree",
            "--no-messages",
            target_rev,
            head_rev,
        ])
        .current_dir(cwd)
        .output()
        .context("failed to spawn git merge-tree")?;
    match out.status.code() {
        // The tree id is the first line of stdout; anything after it describes
        // conflicts, and a clean merge has none to describe.
        Some(0) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let tree = stdout
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .context("git merge-tree reported a clean merge without naming a tree")?
                .to_string();
            Ok(MergeOutcome {
                conflicts: false,
                tree: Some(tree),
            })
        }
        Some(1) => Ok(MergeOutcome {
            conflicts: true,
            tree: None,
        }),
        code => bail!(
            "git merge-tree failed with exit code {}: {}",
            code.map_or_else(|| "signal".to_string(), |code| code.to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// The paths an operation writes between two trees: every file it adds or
/// changes, and every parent directory those files need.
///
/// A deletion is not a write. Without rename detection a rename reads as an
/// add beside a delete, and the add is the half that lands on disk.
pub struct WriteSet {
    pub files: Vec<String>,
    pub parents: Vec<String>,
}

pub fn write_set(cwd: &Path, from_rev: &str, to_rev: &str) -> Result<WriteSet> {
    let out = git(
        cwd,
        &[
            "diff",
            "--no-renames",
            "--name-only",
            "--diff-filter=d",
            "-z",
            from_rev,
            to_rev,
        ],
    )?;
    let mut files = Vec::new();
    let mut parents = BTreeSet::new();
    for path in out.split('\0').filter(|path| !path.is_empty()) {
        files.push(path.to_string());
        let mut parent = Path::new(path).parent();
        while let Some(dir) = parent.filter(|dir| !dir.as_os_str().is_empty()) {
            parents.insert(dir.to_string_lossy().into_owned());
            parent = dir.parent();
        }
    }
    for file in &files {
        parents.remove(file);
    }
    Ok(WriteSet {
        files,
        parents: parents.into_iter().collect(),
    })
}

/// Untracked and ignored paths in a checkout that an operation would write.
///
/// A path Git has in its index is the checkout's own content: rewriting it is
/// what the operation is for. A path it does not is bytes Git does not carry,
/// and writing there either fails the operation or, when the path is ignored,
/// destroys it silently. That silent case is why this exists.
pub fn write_overlap(checkout: &Path, writes: &WriteSet) -> Result<Vec<String>> {
    let candidates = writes
        .files
        .iter()
        .chain(writes.parents.iter())
        .cloned()
        .collect::<Vec<_>>();
    let tracked = tracked_paths(checkout, &candidates)?;
    let mut collisions = Vec::new();
    for path in &writes.files {
        let full = checkout.join(path);
        let Ok(metadata) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        if !metadata.is_dir() {
            if !tracked.contains(path) {
                collisions.push(path.clone());
            }
            continue;
        }
        // A file where the checkout holds a directory replaces it. That is an
        // ordinary merge when everything beneath is tracked, and a silent
        // loss of ignored bytes otherwise.
        if !git(
            checkout,
            &["status", "--porcelain", "-z", "--ignored", "--", path],
        )?
        .is_empty()
        {
            collisions.push(path.clone());
        }
    }
    for path in &writes.parents {
        let full = checkout.join(path);
        let Ok(metadata) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        if !metadata.is_dir() && !tracked.contains(path) {
            collisions.push(path.clone());
        }
    }
    collisions.sort();
    collisions.dedup();
    Ok(collisions)
}

/// Untracked and ignored paths a checkout holds, as Git reports them. Ignored
/// directories are collapsed to their top entry, which is what a report can
/// name without walking a tree Git already walked.
pub fn untracked_and_ignored(checkout: &Path) -> Result<Vec<String>> {
    let out = git(checkout, &["status", "--porcelain", "-z", "--ignored"])?;
    let mut paths = Vec::new();
    for entry in out.split('\0') {
        let (tag, path) = entry.split_at_checked(2).unwrap_or((entry, ""));
        if tag == "??" || tag == "!!" {
            paths.push(path.trim_start().to_string());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The subset of `paths` Git has in the index of `checkout`.
fn tracked_paths(checkout: &Path, paths: &[String]) -> Result<BTreeSet<String>> {
    let mut tracked = BTreeSet::new();
    // Batched so a large change cannot overrun a command line.
    for chunk in paths.chunks(512) {
        let mut args = vec!["ls-files", "-z", "--"];
        args.extend(chunk.iter().map(String::as_str));
        let out = git(checkout, &args)?;
        tracked.extend(
            out.split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_string),
        );
    }
    Ok(tracked)
}

/// Write a commit holding `tree` under the given parents, touching no ref.
///
/// It exists so a tree belonging to no branch can be checked out, named by
/// evidence, and pinned. The parents record which two sides produced it, so a
/// reader can see what it is a merge of.
pub fn commit_tree_with_parents(
    cwd: &Path,
    tree: &str,
    parents: &[&str],
    message: &str,
) -> Result<String> {
    let mut args = vec!["commit-tree", tree];
    for parent in parents {
        args.push("-p");
        args.push(parent);
    }
    args.push("-m");
    args.push(message);
    git(cwd, &args)
}

/// Write a commit holding `tree` onto `parent`, naming `committer` as its
/// committer, touching no ref. The committer's email, and the author, are
/// Git's configured identity.
pub fn commit_tree_committed_by(
    cwd: &Path,
    tree: &str,
    parent: &str,
    message: &str,
    committer: &str,
) -> Result<String> {
    let mut command = git_command();
    command
        .args(["commit-tree", tree, "-p", parent, "-m", message])
        .current_dir(cwd)
        .env("GIT_COMMITTER_NAME", committer);
    let out = command_output(&mut command)
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    if !out.status.success() {
        bail!(
            "git commit-tree failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Who did something to a commit, and when, exactly as the commit object
/// spells it.
///
/// The date keeps Git's own `<seconds> <±hhmm>` spelling rather than a parsed
/// timestamp. A rewrite that reformatted it would move the commit's offset to
/// whatever the rewriting machine happened to be set to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitStamp {
    pub name: String,
    pub email: String,
    pub date: String,
}

impl CommitStamp {
    /// The date in the form `git commit-tree` reads from the environment.
    fn git_date(&self) -> String {
        format!("@{}", self.date)
    }

    /// The identity as a commit or tag object header value spells it.
    fn git_line(&self) -> String {
        format!("{} <{}> {}", self.name, self.email, self.date)
    }

    fn parse(line: &str, what: &str) -> Result<Self> {
        let open = line
            .find('<')
            .with_context(|| format!("commit {what} {line:?} names no email"))?;
        let close = line[open..]
            .find('>')
            .map(|offset| open + offset)
            .with_context(|| format!("commit {what} {line:?} names no email"))?;
        Ok(Self {
            name: line[..open].trim_end().to_string(),
            email: line[open + 1..close].to_string(),
            date: line[close + 1..].trim().to_string(),
        })
    }
}

/// One commit header as the object spells it: the field name, and the whole
/// block it occupies — its `field value` line plus each continuation line
/// indented by one space, newline-separated and without a trailing newline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawHeader {
    pub field: String,
    pub block: String,
}

/// The header fields this module reads into named parts. Everything else is
/// carried and never inspected.
const INTERPRETED_HEADERS: [&str; 5] = ["tree", "parent", "author", "committer", "encoding"];

/// A commit object as the database holds it: everything a rewrite has to
/// carry forward unchanged.
///
/// The message is bytes. A commit message is not required to be UTF-8 — that
/// is what the `encoding` header is for — and a rewrite that decoded and
/// re-encoded one would change the object it claims to have only re-signed.
///
/// The headers are the object's own lines, in the object's own order, and they
/// are what a recreation writes back. The named fields beside them are the
/// same information parsed, because `commit-tree` takes the tree, the parents
/// and the identity stamps as arguments and environment rather than as text.
/// Two representations of one thing is the price of not being the only writer;
/// the verbatim one is authoritative, and a commit whose parsed form does not
/// spell its headers back exactly is recreated from the headers.
#[derive(Debug, Clone)]
pub struct RawCommit {
    pub tree: String,
    pub parents: Vec<String>,
    pub author: CommitStamp,
    pub committer: CommitStamp,
    pub encoding: Option<String>,
    /// Every header the object carries, in order. The signature headers are
    /// absent by construction: a recreated commit is signed afresh or not at
    /// all, so carrying the old `gpgsig` forward would attest to an object
    /// that no longer exists.
    pub headers: Vec<RawHeader>,
    pub message: Vec<u8>,
}

impl RawCommit {
    /// The field name of each header this module does not interpret, for a
    /// caller that has to say which headers a commit carries. `mergetag`,
    /// which a merge of a signed tag carries, is the common one.
    pub fn extra_header_fields(&self) -> Vec<String> {
        self.headers
            .iter()
            .filter(|header| !INTERPRETED_HEADERS.contains(&header.field.as_str()))
            .map(|header| header.field.clone())
            .collect()
    }

    /// Whether `git commit-tree` writes this object's non-parent bytes back
    /// exactly.
    ///
    /// `commit-tree` emits the headers it has flags for, in its own order,
    /// with the identity lines rebuilt from the parts it was given. That
    /// reproduces most commits and not all of them: a carried header, a
    /// header before `encoding`, or an identity line spelled with unusual
    /// whitespace all come back as different bytes — a different object,
    /// presented as the same commit with a new signature. Parent values are
    /// exempt because replacing them is what a rewrite does; their position
    /// is not.
    pub fn commit_tree_writes_it_back(&self) -> bool {
        let mut wanted: Vec<(&str, String)> = vec![("tree", format!("tree {}", self.tree))];
        wanted.extend(self.parents.iter().map(|_| ("parent", String::new())));
        wanted.push(("author", format!("author {}", self.author.git_line())));
        wanted.push((
            "committer",
            format!("committer {}", self.committer.git_line()),
        ));
        if let Some(encoding) = &self.encoding {
            wanted.push(("encoding", format!("encoding {encoding}")));
        }
        self.headers.len() == wanted.len()
            && self
                .headers
                .iter()
                .zip(&wanted)
                .all(|(have, (field, block))| {
                    have.field == *field && (*field == "parent" || &have.block == block)
                })
    }
}

/// Every commit in `base..head`, oldest first, with its message.
pub fn range_messages(cwd: &Path, base: &str, head: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let commits = git(cwd, &["rev-list", "--reverse", &format!("{base}..{head}")])?;
    let revisions: Vec<_> = commits.lines().collect();
    let read = read_commits(cwd, &revisions)?;
    Ok(revisions
        .into_iter()
        .zip(read)
        .map(|(revision, commit)| (revision.to_string(), commit.message))
        .collect())
}

/// Read one commit object whole.
pub fn read_commit(cwd: &Path, rev: &str) -> Result<RawCommit> {
    let raw = git_bytes(cwd, &["cat-file", "commit", rev])?;
    parse_commit_object(&raw)
        .with_context(|| format!("cannot read commit {rev} in {}", cwd.display()))
}

/// Read commit objects in query order through one `cat-file --batch` process.
/// Annotated tags are peeled to commits, as with `read_commit`.
pub fn read_commits(cwd: &Path, revisions: &[&str]) -> Result<Vec<RawCommit>> {
    if revisions.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = String::new();
    for revision in revisions {
        if revision.is_empty() || revision.contains(['\n', '\r', '\0']) {
            bail!("invalid commit revision {revision:?}");
        }
        query.push_str(revision);
        query.push_str("^{commit}\n");
    }
    let output = git_piped(cwd, &["cat-file", "--batch"], query.as_bytes())?;
    let mut remaining = output.as_slice();
    let mut commits = Vec::with_capacity(revisions.len());
    for revision in revisions {
        let commit = (|| -> Result<RawCommit> {
            let newline = remaining
                .iter()
                .position(|byte| *byte == b'\n')
                .context("batch response has no header")?;
            let header = std::str::from_utf8(&remaining[..newline])?;
            let fields: Vec<_> = header.split_whitespace().collect();
            if fields.len() != 3 || fields[1] != "commit" {
                bail!("unexpected batch response: {header}");
            }
            let size: usize = fields[2].parse().context("invalid batch object size")?;
            remaining = &remaining[newline + 1..];
            if remaining.get(size) != Some(&b'\n') {
                bail!("batch response has a truncated object or missing terminator");
            }
            let raw = &remaining[..size];
            remaining = &remaining[size + 1..];
            parse_commit_object(raw)
        })()
        .with_context(|| format!("cannot read commit {revision} in {}", cwd.display()))?;
        commits.push(commit);
    }
    if !remaining.is_empty() {
        bail!("batch response contains extra objects");
    }
    Ok(commits)
}

/// Split a commit object into its headers and its message.
///
/// A header value may continue onto following lines, each indented by one
/// space; that is how a signature and a `mergetag` are stored. The message
/// begins after the first empty line and is taken verbatim to the end of the
/// object.
///
/// Every header is kept verbatim, in the order the object holds them, so a
/// commit is recreated with the headers it had rather than with the ones arc
/// knows about. The exceptions are the signature headers, which describe an
/// object that a recreation replaces.
fn parse_commit_object(raw: &[u8]) -> Result<RawCommit> {
    let split = raw
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .context("commit object has no message")?;
    let (headers, message) = (&raw[..split], &raw[split + 2..]);
    let headers = std::str::from_utf8(headers).context("commit headers are not UTF-8")?;
    let mut tree = None;
    let mut parents = Vec::new();
    let mut author = None;
    let mut committer = None;
    let mut encoding = None;
    let mut kept: Vec<RawHeader> = Vec::new();
    // Whether the header being read is one to keep, so its continuation lines
    // travel with it. Only a signature is dropped, and its continuation lines
    // go with it.
    let mut keeping = false;
    for line in headers.lines() {
        if line.starts_with(' ') {
            if keeping {
                let header = kept.last_mut().expect("a kept header to extend");
                header.block.push('\n');
                header.block.push_str(line);
            }
            continue;
        }
        let (field, value) = line.split_once(' ').unwrap_or((line, ""));
        keeping = !matches!(field, "gpgsig" | "gpgsig-sha256");
        match field {
            "tree" => tree = Some(value.to_string()),
            "parent" => parents.push(value.to_string()),
            "author" => author = Some(CommitStamp::parse(value, "author")?),
            "committer" => committer = Some(CommitStamp::parse(value, "committer")?),
            "encoding" => encoding = Some(value.to_string()),
            _ => {}
        }
        if keeping {
            kept.push(RawHeader {
                field: field.to_string(),
                block: line.to_string(),
            });
        }
    }
    Ok(RawCommit {
        tree: tree.context("commit object names no tree")?,
        parents,
        author: author.context("commit object names no author")?,
        committer: committer.context("commit object names no committer")?,
        encoding,
        headers: kept,
        message: message.to_vec(),
    })
}

/// Write a commit holding `tree` under the given parents, carrying the given
/// author, committer, dates, encoding and message bytes, and signed when a
/// key is asked for. Touches no ref.
///
/// Recreating a commit this way changes its signature and its parents and
/// nothing else, so a commit rebuilt with the same inputs and no signature
/// has the same object id it started with.
///
/// `commit-tree` writes the headers it has flags for, in its own order, from
/// the parts it is given, so a commit whose header text it cannot spell back
/// is refused here and belongs to [`write_commit_object`], which assembles the
/// object itself. Refusing keeps the guarantee above true: a path that
/// silently reordered a header or reformatted an identity line would produce a
/// commit that is not the one it claims to have only re-signed.
pub fn commit_tree_as(
    cwd: &Path,
    commit: &RawCommit,
    sign: Option<Option<&str>>,
) -> Result<String> {
    let RawCommit {
        tree,
        parents,
        author,
        committer,
        encoding,
        headers: _,
        message,
    } = commit;
    if !commit.commit_tree_writes_it_back() {
        bail!(
            "git commit-tree cannot write back the headers of the commit holding tree {tree}: {}",
            commit
                .headers
                .iter()
                .map(|header| header.field.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let mut args: Vec<String> = Vec::new();
    // The encoding header is written from configuration, so it travels as
    // configuration rather than as a flag `commit-tree` does not have.
    if let Some(encoding) = encoding {
        args.push("-c".into());
        args.push(format!("i18n.commitEncoding={encoding}"));
    }
    args.push("commit-tree".into());
    args.push(tree.clone());
    for parent in parents {
        args.push("-p".into());
        args.push(parent.clone());
    }
    if let Some(key) = sign {
        args.push(match key {
            Some(key) => format!("-S{key}"),
            None => "-S".into(),
        });
    }
    let mut command = git_command();
    command
        .args(&args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", &author.name)
        .env("GIT_AUTHOR_EMAIL", &author.email)
        .env("GIT_AUTHOR_DATE", author.git_date())
        .env("GIT_COMMITTER_NAME", &committer.name)
        .env("GIT_COMMITTER_EMAIL", &committer.email)
        .env("GIT_COMMITTER_DATE", committer.git_date())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("cannot run git commit-tree")?;
    let mut stdin = child.stdin.take().context("git commit-tree has no stdin")?;
    let message = message.to_vec();
    // The message goes out on its own thread for the same reason every other
    // piped Git call here does: a message larger than the pipe buffer
    // deadlocks a writer that is not being read from.
    let writer = thread::spawn(move || stdin.write_all(&message));
    let out = child.wait_with_output().context("git commit-tree failed")?;
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("the commit-tree writer thread panicked"))??;
    if !out.status.success() {
        bail!(
            "git commit-tree failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Write the same commit [`commit_tree_as`] would, by assembling the object
/// text and hashing it, so every header the commit carries survives.
///
/// The header order is the object's own, with the signature last, which is
/// why a commit whose headers `commit-tree` would have written in that same
/// order comes out with the object id `commit-tree` would have given it.
pub fn write_commit_object(
    cwd: &Path,
    commit: &RawCommit,
    sign: Option<Option<&str>>,
) -> Result<String> {
    let unsigned = commit_object_text(commit, None);
    let object = match sign {
        Some(key) => {
            let signature = openpgp_sign(cwd, &unsigned, key)?;
            commit_object_text(commit, Some(&signature))
        }
        None => unsigned,
    };
    hash_object(cwd, "commit", &object)
}

/// One commit object, exactly as the database holds it: every header line the
/// commit carried, in the order it carried them, with the `parent` lines
/// replaced by the parents the commit now has and the signature written last.
///
/// Only two things about the header text are ever arc's own. Parents are, and
/// they go where the object had them — after `tree` for a commit that had
/// none, since that is where Git puts the first one. The signature is, and it
/// goes last, since that is where Git puts it and it covers everything above.
fn commit_object_text(commit: &RawCommit, signature: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    let parents = |out: &mut Vec<u8>| {
        for parent in &commit.parents {
            out.extend_from_slice(b"parent ");
            out.extend_from_slice(parent.as_bytes());
            out.push(b'\n');
        }
    };
    let had_parents = commit.headers.iter().any(|header| header.field == "parent");
    let mut written = commit.parents.is_empty();
    for header in &commit.headers {
        if header.field == "parent" {
            if !written {
                parents(&mut out);
                written = true;
            }
            continue;
        }
        out.extend_from_slice(header.block.as_bytes());
        out.push(b'\n');
        if !written && !had_parents && header.field == "tree" {
            parents(&mut out);
            written = true;
        }
    }
    if let Some(signature) = signature {
        out.extend_from_slice(&folded_header("gpgsig", signature));
    }
    out.push(b'\n');
    out.extend_from_slice(&commit.message);
    out
}

/// A header whose value spans lines, written the way Git stores one: the
/// first line after the field name, every later line indented by one space.
fn folded_header(field: &str, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, line) in value
        .strip_suffix(b"\n")
        .unwrap_or(value)
        .split(|byte| *byte == b'\n')
        .enumerate()
    {
        if index == 0 {
            out.extend_from_slice(field.as_bytes());
        }
        out.push(b' ');
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

/// An annotated tag object as the database holds it.
///
/// The message is bytes for the same reason a commit's is, and the headers
/// under the tag name — the tagger line, and an `encoding` where there is one
/// — travel verbatim, so re-pointing a tag carries its authorship and date
/// rather than restating them.
#[derive(Debug, Clone)]
pub struct RawTag {
    pub object: String,
    pub kind: String,
    pub name: String,
    pub headers: Vec<String>,
    pub message: Vec<u8>,
    /// Whether the original carried a signature over its own text.
    pub signed: bool,
}

/// Read one annotated tag object whole.
pub fn read_tag(cwd: &Path, rev: &str) -> Result<RawTag> {
    let raw = git_bytes(cwd, &["cat-file", "tag", rev])?;
    parse_tag_object(&raw).with_context(|| format!("cannot read tag {rev} in {}", cwd.display()))
}

const PGP_SIGNATURE: &[u8] = b"-----BEGIN PGP SIGNATURE-----";

fn parse_tag_object(raw: &[u8]) -> Result<RawTag> {
    let split = raw
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .context("tag object has no message")?;
    let (headers, body) = (&raw[..split], &raw[split + 2..]);
    let headers = std::str::from_utf8(headers).context("tag headers are not UTF-8")?;
    let mut object = None;
    let mut kind = None;
    let mut name = None;
    let mut carried: Vec<String> = Vec::new();
    for line in headers.lines() {
        if line.starts_with(' ') {
            if let Some(block) = carried.last_mut() {
                block.push('\n');
                block.push_str(line);
            }
            continue;
        }
        let (field, value) = line.split_once(' ').unwrap_or((line, ""));
        match field {
            "object" => object = Some(value.to_string()),
            "type" => kind = Some(value.to_string()),
            "tag" => name = Some(value.to_string()),
            _ => carried.push(line.to_string()),
        }
    }
    // A tag's signature is appended to its message rather than held in a
    // header, so the message is everything up to where the signature starts.
    let signature = body
        .windows(PGP_SIGNATURE.len())
        .position(|window| window == PGP_SIGNATURE);
    Ok(RawTag {
        object: object.context("tag object names no object")?,
        kind: kind.context("tag object names no type")?,
        name: name.context("tag object names no tag")?,
        headers: carried,
        message: body[..signature.unwrap_or(body.len())].to_vec(),
        signed: signature.is_some(),
    })
}

/// Write an annotated tag object, signed when a key is asked for. Touches no
/// ref.
pub fn write_tag_object(cwd: &Path, tag: &RawTag, sign: Option<Option<&str>>) -> Result<String> {
    let mut object = tag_object_text(tag);
    if let Some(key) = sign {
        let signature = openpgp_sign(cwd, &object, key)?;
        object.extend_from_slice(&signature);
    }
    hash_object(cwd, "tag", &object)
}

/// One tag object up to its signature, which is what a tag signature covers.
fn tag_object_text(tag: &RawTag) -> Vec<u8> {
    let mut out = Vec::new();
    for (field, value) in [
        ("object", &tag.object),
        ("type", &tag.kind),
        ("tag", &tag.name),
    ] {
        out.extend_from_slice(format!("{field} {value}\n").as_bytes());
    }
    for block in &tag.headers {
        out.extend_from_slice(block.as_bytes());
        out.push(b'\n');
    }
    out.push(b'\n');
    out.extend_from_slice(&tag.message);
    out
}

/// Store an object of the given type and answer with its name.
fn hash_object(cwd: &Path, kind: &str, object: &[u8]) -> Result<String> {
    let out = git_piped(cwd, &["hash-object", "-t", kind, "-w", "--stdin"], object)?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Git with `input` on its standard input, answering with its standard
/// output. The editor pins every other Git call here gets apply to these too.
fn git_piped(cwd: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let mut command = git_command();
    command.current_dir(cwd);
    piped_output_with(command, "git", &owned, input)
}

/// Replace the note `commit` holds under `refs/notes/<name>` with `text`.
pub fn set_note(cwd: &Path, name: &str, commit: &str, text: &str) -> Result<()> {
    let notes = format!("--ref={name}");
    git_piped(
        cwd,
        &["notes", &notes, "add", "-f", "-F", "-", commit],
        text.as_bytes(),
    )?;
    Ok(())
}

/// The note `commit` holds under `refs/notes/<name>`, if any.
pub fn read_note(cwd: &Path, name: &str, commit: &str) -> Result<Option<String>> {
    let notes = format!("--ref={name}");
    let output = git_command()
        .args(["notes", &notes, "show", commit])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// Every commit that holds a note under `refs/notes/<name>`.
pub fn noted_commits(cwd: &Path, name: &str) -> Result<Vec<String>> {
    let notes = format!("--ref={name}");
    Ok(git(cwd, &["notes", &notes, "list"])?
        .lines()
        .filter_map(|line| line.split_once(' ').map(|(_, commit)| commit.to_string()))
        .collect())
}

/// An armored detached signature over `payload`, made by the OpenPGP program
/// and key Git itself would use.
///
/// `--key` names the key when the caller has one; otherwise it is
/// `user.signingkey`, and where that is unset the signing program's own
/// default key. A repository configured for a signature format this cannot
/// produce is refused rather than signed with the wrong one.
fn openpgp_sign(cwd: &Path, payload: &[u8], key: Option<&str>) -> Result<Vec<u8>> {
    let format = config_value(cwd, "gpg.format")?.unwrap_or_else(|| "openpgp".to_string());
    if format != "openpgp" {
        bail!(
            "gpg.format is {format}; carrying a commit's other headers through a rewrite \
             assembles the object here, and only an openpgp signature can be made over it"
        );
    }
    let program = config_value(cwd, "gpg.program")?.unwrap_or_else(|| "gpg".to_string());
    let key = match key {
        Some(key) => Some(key.to_string()),
        None => config_value(cwd, "user.signingkey")?,
    };
    let mut args: Vec<String> = vec![
        "--status-fd=2".into(),
        "--detach-sign".into(),
        "--armor".into(),
    ];
    if let Some(key) = &key {
        args.push("--local-user".into());
        args.push(key.clone());
    }
    let signature = piped_output(&program, &args, payload, |command| {
        command.current_dir(cwd);
    })
    .with_context(|| format!("cannot sign with {program}"))?;
    if !signature.starts_with(PGP_SIGNATURE) {
        bail!("{program} produced no armored signature");
    }
    Ok(signature)
}

/// One Git configuration value, or nothing where it is unset.
fn config_value(cwd: &Path, key: &str) -> Result<Option<String>> {
    let mut command = git_command();
    command.args(["config", "--get", key]).current_dir(cwd);
    let out = command_output(&mut command)
        .with_context(|| format!("failed to read git config in {}", cwd.display()))?;
    if !out.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

/// Every Git configuration entry whose key matches `key_pattern`, as
/// `(key, value)`. Nothing matching, or a configuration Git cannot read, is
/// no entries.
fn config_entries(cwd: &Path, key_pattern: &str) -> Result<Vec<(String, String)>> {
    let mut command = git_command();
    command
        .args(["config", "-z", "--get-regexp", key_pattern])
        .current_dir(cwd);
    let out = command_output(&mut command)
        .with_context(|| format!("failed to read git config in {}", cwd.display()))?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter_map(|entry| entry.split_once('\n'))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect())
}

/// Run a program with `input` on its standard input, answering with its
/// standard output.
///
/// The input goes out on its own thread: an input larger than the pipe buffer
/// deadlocks a writer nobody is reading from.
fn piped_output(
    program: &str,
    args: &[String],
    input: &[u8],
    setup: impl FnOnce(&mut Command),
) -> Result<Vec<u8>> {
    let mut command = Command::new(program);
    setup(&mut command);
    piped_output_with(command, program, args, input)
}

/// The same, for a caller that has already built the command.
fn piped_output_with(
    mut command: Command,
    program: &str,
    args: &[String],
    input: &[u8],
) -> Result<Vec<u8>> {
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let deadline = COMMAND_DEADLINE.get();
    if deadline.is_some() {
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;
    let child_id = child.id();
    let mut stdin = child
        .stdin
        .take()
        .with_context(|| format!("{program} has no stdin"))?;
    let input = input.to_vec();
    let writer = thread::spawn(move || stdin.write_all(&input));
    let output = match deadline {
        Some(deadline) => wait_for_output(child, deadline),
        None => child.wait_with_output().map_err(anyhow::Error::from),
    };
    let out = output.with_context(|| format!("{program} failed"))?;
    let written = join_before_deadline(writer, deadline, &format!("{program} writer"));
    if written.is_err() && deadline.is_some() {
        crate::process_group::kill_process_group(child_id)?;
    }
    written??;
    if !out.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// A tracked file's text as committed at `rev`, or `None` when that revision
/// holds no such path.
pub fn file_at(cwd: &Path, rev: &str, path: &str) -> Result<Option<String>> {
    let object = format!("{rev}:{path}");
    match shared_object(cwd, &object) {
        Some(Object::Missing) => return Ok(None),
        Some(Object::Found { kind, bytes, .. }) if kind == "blob" => {
            return String::from_utf8(bytes)
                .map(Some)
                .with_context(|| format!("{object} is not UTF-8"));
        }
        _ => {}
    }
    let out = git_command()
        .args(["rev-parse", "--verify", "-q", &object])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    if out.status.code() == Some(1) {
        return Ok(None);
    }
    if !out.status.success() {
        bail!(
            "git rev-parse --verify -q {object} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let blob = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let bytes = git_bytes(cwd, &["cat-file", "blob", &blob])?;
    String::from_utf8(bytes)
        .map(Some)
        .with_context(|| format!("{object} is not UTF-8"))
}

/// A blob's bytes, exactly as the object store holds them.
pub fn blob_bytes(cwd: &Path, blob: &str) -> Result<Vec<u8>> {
    git_bytes(cwd, &["cat-file", "blob", blob])
}

/// Git's raw output, for the callers that read objects rather than text.
fn git_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = git_command();
    command.args(args).current_dir(cwd);
    let out = command_output(&mut command)
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// How each commit reachable from `rev` is signed, newest first: the commit,
/// Git's one-letter verification result, and the key that signed it.
///
/// The verification letter and the key are what Git itself reports, including
/// `N` for no signature and an empty key where there is none to name.
pub fn signature_survey(cwd: &Path, rev: &str) -> Result<Vec<(String, String, String)>> {
    let out = git(cwd, &["log", "--format=%H%x00%G?%x00%GK", rev])?;
    out.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut fields = line.split('\0');
            let commit = fields.next().unwrap_or_default().to_string();
            let verification = fields.next().unwrap_or_default().to_string();
            let key = fields.next().unwrap_or_default().to_string();
            if commit.is_empty() {
                bail!("git log returned a row naming no commit");
            }
            Ok((commit, verification, key))
        })
        .collect()
}

/// Every commit from `from` through `rev`, parents before children.
///
/// `from` is included, and its own ancestry is not: excluding the parents of
/// `from` is what makes the range start there rather than at the root, and it
/// is spelled that way so a root commit — which has no parents to exclude —
/// needs no special case.
pub fn commits_from(cwd: &Path, from: &str, rev: &str) -> Result<Vec<String>> {
    let out = git(
        cwd,
        &[
            "rev-list",
            "--topo-order",
            "--reverse",
            rev,
            &format!("^{from}^@"),
        ],
    )?;
    Ok(out.lines().map(str::to_string).collect())
}

/// One local branch or tag.
#[derive(Debug, Clone)]
pub struct LocalRef {
    pub name: String,
    /// What the ref names, which for an annotated tag is the tag object.
    pub object: String,
    /// The object type, which separates a tag pointing straight at a commit
    /// from an annotated tag object, which names a commit but is not one.
    pub kind: String,
    /// The commit the ref leads to: the object itself where that is already a
    /// commit, and what an annotated tag peels to otherwise. A rewrite maps
    /// commits, so this is the id to look up.
    pub commit: String,
}

/// Every local branch and tag.
pub fn local_refs(cwd: &Path) -> Result<Vec<LocalRef>> {
    let out = git(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(*objectname)",
            "refs/heads/",
            "refs/tags/",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let name = fields.next()?.to_string();
            let object = fields.next()?.to_string();
            let kind = fields.next()?.to_string();
            let peeled = fields.next().unwrap_or_default();
            let commit = if peeled.is_empty() {
                object.clone()
            } else {
                peeled.to_string()
            };
            Some(LocalRef {
                name,
                object,
                kind,
                commit,
            })
        })
        .collect())
}

pub fn blob_oid(cwd: &Path, rev: &str, path: &str) -> Option<String> {
    git(cwd, &["rev-parse", "--verify", &format!("{rev}:{path}")]).ok()
}

/// The tree a commit points at, for comparing what was committed with what was
/// actually there.
pub fn commit_tree(cwd: &Path, rev: &str) -> Result<String> {
    git(cwd, &["rev-parse", &format!("{rev}^{{tree}}")])
}

/// The tree a checkout would have to reproduce to match this worktree:
/// tracked content with its staged and unstaged edits, plus files Git is not
/// yet tracking. Ignored files and the contents of submodules are outside it,
/// exactly as they are outside a commit.
///
/// Evidence recorded against a commit describes a tree no checkout of that
/// commit reproduces whenever the worktree was dirty, which is the ordinary
/// shape of agent execution. This writes the tree into the object database and
/// the caller keeps a ref to it, so the evidence names something that stays
/// resolvable; the scratch index means the real one is untouched.
pub fn worktree_tree(cwd: &Path) -> Result<String> {
    // Inside the Git directory rather than the system temp dir: the index has
    // to be on the same filesystem as the object store it feeds, and this one
    // is already private to the repository.
    let index_path =
        git_path(cwd, "arc-scratch-index")?.with_extension(crate::ids::new_identifier());
    let index = index_path.display().to_string();

    // From the top of the worktree, so `add -A` means the whole worktree
    // rather than the subtree a caller happened to run from.
    let top = toplevel(cwd)?;
    let run = |args: &[&str]| -> Result<String> {
        let output = git_command()
            .args(args)
            .current_dir(&top)
            .env("GIT_INDEX_FILE", &index)
            .output()
            .with_context(|| format!("cannot run git {}", args.join(" ")))?;
        if !output.status.success() {
            anyhow::bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let result = (|| {
        // An unborn HEAD has no tree to read; the add below still captures
        // everything present.
        let _ = run(&["read-tree", "HEAD"]);
        run(&["add", "-A"])?;
        run(&["write-tree"])
    })();
    let _ = std::fs::remove_file(&index_path);
    result
}

pub fn is_clean(cwd: &Path) -> Result<bool> {
    Ok(git(cwd, &["status", "--porcelain"])?.is_empty())
}

/// Tracked and untracked dirt, recorded separately so a waiver reason is
/// self-evident at the moment of waiving and the frequency premise becomes
/// measurable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dirt {
    pub tracked: bool,
    pub untracked: bool,
}

/// What kind of dirt a worktree carries.
///
/// One bool cannot say which, so the claim that this wedges overwhelmingly on
/// untracked-only dirt could not be checked. `git status --porcelain` already
/// exempts ignored paths, so what it reports as untracked is versionable
/// content nobody added — most often the forgotten `git add` the build may
/// already be reading.
pub fn dirt(cwd: &Path) -> Result<Dirt> {
    let status = git(cwd, &["status", "--porcelain"])?;
    let mut dirt = Dirt::default();
    for line in status.lines().filter(|line| !line.trim().is_empty()) {
        if line.starts_with("??") {
            dirt.untracked = true;
        } else {
            dirt.tracked = true;
        }
    }
    Ok(dirt)
}

pub fn branch_exists(cwd: &Path, branch: &str) -> bool {
    git(
        cwd,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

pub fn create_branch(cwd: &Path, branch: &str, base: &str) -> Result<()> {
    git(cwd, &["branch", branch, base])?;
    Ok(())
}

pub fn checkout(cwd: &Path, branch: &str) -> Result<()> {
    git(cwd, &["switch", "--no-guess", "--", branch])?;
    Ok(())
}

pub fn add_worktree(cwd: &Path, path: &Path, branch: &str) -> Result<()> {
    git(
        cwd,
        &[
            "worktree",
            "add",
            "--no-checkout",
            path.to_str().context("non-UTF8 worktree path")?,
            branch,
        ],
    )?;
    Ok(())
}

/// Allocate an empty worktree and create `branch` at `start` in one Git call.
/// The `-b` must precede the path: `worktree add <path>
/// <new-branch>` treats the branch as an existing commit-ish and fails with
/// `invalid reference`, so a branch cannot be minted positionally.
pub fn add_worktree_new_branch(cwd: &Path, path: &Path, branch: &str, start: &str) -> Result<()> {
    git(
        cwd,
        &[
            "worktree",
            "add",
            "--no-checkout",
            "-b",
            branch,
            path.to_str().context("non-UTF8 worktree path")?,
            start,
        ],
    )?;
    Ok(())
}

/// Populate an allocated worktree; checkout hooks run after its files are written.
pub fn populate_worktree(path: &Path) -> Result<()> {
    git(path, &["read-tree", "--reset", "-u", "HEAD"])?;
    let format = git(path, &["rev-parse", "--show-object-format"])?;
    let null_oid = match format.as_str() {
        "sha1" => "0".repeat(40),
        "sha256" => "0".repeat(64),
        _ => bail!("unsupported Git object format {format:?}"),
    };
    let head = git(path, &["rev-parse", "HEAD"])?;
    git(
        path,
        &[
            "hook",
            "run",
            "--ignore-missing",
            "post-checkout",
            "--",
            &null_oid,
            &head,
            "1",
        ],
    )?;
    Ok(())
}

/// Remove a worktree. Fails loudly when Git refuses — uncommitted or
/// untracked files make it refuse — so a fork worktree holding work arc
/// cannot see is never destroyed silently. The operator's `--force` is the
/// decision that gets past this, not a flag arc reaches for itself.
pub fn remove_worktree(cwd: &Path, path: &Path, force: bool) -> Result<()> {
    let mut args = vec!["worktree".to_string(), "remove".to_string()];
    if force {
        args.push("--force".to_string());
    }
    args.push(path.to_str().context("non-UTF8 worktree path")?.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    git(cwd, &refs)?;
    Ok(())
}

/// How many commits `branch` carries that `base` does not. Either side
/// failing to resolve is an error, not a zero a reader would sum.
///
/// Discover the branch a repository presents as its integration branch.
///
/// Git lists the primary worktree first, so its checked-out branch is the
/// strongest local signal. When that worktree is detached, `origin/HEAD` is
/// used when it names a non-fork branch. `None` means no safe branch was
/// discoverable; callers must not substitute a literal branch name, because
/// it could resolve to an unrelated history.
pub fn default_branch(cwd: &Path) -> Option<String> {
    // `origin/HEAD` is a property of the repository, so it is asked first: the
    // primary worktree's branch is whatever someone happens to have checked
    // out, and a fork measured against that changes its answer when an
    // unrelated checkout moves.
    let declared = git(
        cwd,
        &["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .and_then(|branch| branch.strip_prefix("origin/").map(str::to_string))
    .filter(|branch| !branch.is_empty() && !is_working_branch(cwd, branch));
    if declared.is_some() {
        return declared;
    }

    // Without a declared default the primary checkout is the best remaining
    // evidence, but only when it holds an integration branch. A branch arc
    // itself creates is work in progress and never something a fork
    // integrates into, so it yields no base rather than a misleading one.
    primary_worktree_branch(cwd)
        .ok()
        .flatten()
        .filter(|branch| !branch.is_empty())
        .filter(|branch| !is_working_branch(cwd, branch))
}

/// Forks, conventional change branches, and ledger-recorded branches carry
/// work rather than an implicit integration target. Unreadable ownership
/// evidence prevents guessing a target.
fn is_working_branch(cwd: &Path, branch: &str) -> bool {
    if is_fork_branch(branch) || branch.starts_with("arc/") {
        return true;
    }
    let recorded = (|| -> Result<bool> {
        let root = crate::store::Store::resolve_root(cwd)?;
        let Some(store) = crate::store::Store::open_at_for(&root, cwd)? else {
            return Ok(false);
        };
        let read = store.read()?;
        for id in read.change_ids()? {
            if read.state(&id)?.branch == branch {
                return Ok(true);
            }
        }
        Ok(false)
    })();
    recorded.unwrap_or(true)
}

pub fn ahead_count(cwd: &Path, base: &str, branch: &str) -> Result<usize> {
    let out = git(cwd, &["rev-list", "--count", &format!("{base}..{branch}")])?;
    out.trim()
        .parse::<usize>()
        .with_context(|| format!("cannot parse rev-list count {out:?}"))
}

/// One local branch with what an ownership report needs: its tip and when
/// that tip was committed. Reading refs only.
#[derive(Debug, Clone)]
pub struct BranchTip {
    pub name: String,
    /// The tip's committer timestamp, when Git records one.
    pub committed_at: Option<i64>,
}

/// Every local branch with its tip. One `for-each-ref` over `refs/heads/`;
/// no tree or file content is read.
pub fn branch_tips(cwd: &Path) -> Result<Vec<BranchTip>> {
    let out = git(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(committerdate:unix)",
            "refs/heads/",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let name = fields.next()?.to_string();
            let committed_at = fields.next().and_then(|value| value.trim().parse().ok());
            Some(BranchTip { name, committed_at })
        })
        .collect())
}

/// Commits each side of `target...branch` carries alone, as `(behind,
/// ahead)`: how far the target has moved past the branch, and how much work
/// the branch holds that the target does not.
pub fn divergence(cwd: &Path, target: &str, branch: &str) -> Result<(usize, usize)> {
    let out = git(
        cwd,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{target}...{branch}"),
        ],
    )?;
    let mut parts = out.split_whitespace();
    let behind = parts
        .next()
        .and_then(|value| value.parse().ok())
        .context("git rev-list --left-right returned no left count")?;
    let ahead = parts
        .next()
        .and_then(|value| value.parse().ok())
        .context("git rev-list --left-right returned no right count")?;
    Ok((behind, ahead))
}

/// Each local branch's `(behind, ahead)` against `target`, as [`divergence`]
/// counts it, from one `for-each-ref`; `None` when this Git cannot answer
/// them that way.
pub fn branch_divergences(
    cwd: &Path,
    target: &str,
) -> Option<std::collections::HashMap<String, (usize, usize)>> {
    let format = format!("--format=%(refname:short)%00%(ahead-behind:{target})");
    let out = git(cwd, &["for-each-ref", &format, "refs/heads/"]).ok()?;
    out.lines()
        .map(|line| {
            let (name, counts) = line.split_once('\0')?;
            let (ahead, behind) = counts.trim().split_once(' ')?;
            Some((
                name.to_string(),
                (behind.parse().ok()?, ahead.parse().ok()?),
            ))
        })
        .collect()
}

/// The upstream a local branch is configured to track (`branch.<name>.remote`
/// and `branch.<name>.merge`), as `(short name, full ref name)`. None when it
/// tracks nothing. Configuration only: the named ref need not exist.
pub fn branch_upstream(cwd: &Path, branch: &str) -> Result<Option<(String, String)>> {
    let out = git(
        cwd,
        &[
            "for-each-ref",
            "--format=%(upstream:short)%00%(upstream)",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    let mut fields = out.lines().next().unwrap_or_default().split('\0');
    Ok(match (fields.next(), fields.next()) {
        (Some(short), Some(full)) if !full.is_empty() => {
            Some((short.to_string(), full.to_string()))
        }
        _ => None,
    })
}

/// One remote's fetch refspecs: where its refs are written, and which of its
/// refs a negative refspec leaves out.
#[derive(Default)]
struct RemoteFetch {
    /// `(source, destination)` of each positive refspec.
    mapped: Vec<(String, String)>,
    /// Source patterns of the negative refspecs.
    excluded: Vec<String>,
}

/// Every remote's `remote.<name>.fetch` refspecs, to read a remote-tracking
/// ref back to the ref a fetch writes it from.
pub struct FetchMap(BTreeMap<String, RemoteFetch>);

impl FetchMap {
    /// The refspecs the repository's configuration gives its remotes.
    pub fn read(cwd: &Path) -> Result<Self> {
        let entries = config_entries(cwd, r"^remote\..+\.fetch$")?;
        Ok(Self::from_refspecs(entries.iter().filter_map(
            |(key, refspec)| {
                let remote = key.strip_prefix("remote.")?.strip_suffix(".fetch")?;
                Some((remote, refspec.as_str()))
            },
        )))
    }

    /// The refspecs as `(remote, refspec)` pairs. A refspec Git would reject
    /// or that fetches into no ref maps nothing: one with an empty side, more
    /// than one `*` in a side, or a `*` in one side only.
    pub fn from_refspecs<'a>(refspecs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut remotes: BTreeMap<String, RemoteFetch> = BTreeMap::new();
        for (remote, refspec) in refspecs {
            let fetch = remotes.entry(remote.to_string()).or_default();
            if let Some(pattern) = refspec.strip_prefix('^') {
                if pattern.matches('*').count() <= 1 && !pattern.contains(':') {
                    fetch.excluded.push(pattern.to_string());
                }
                continue;
            }
            let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
            let Some((source, destination)) = refspec.split_once(':') else {
                continue;
            };
            let globs = destination.matches('*').count();
            if !source.is_empty()
                && !destination.is_empty()
                && globs <= 1
                && source.matches('*').count() == globs
            {
                fetch
                    .mapped
                    .push((source.to_string(), destination.to_string()));
            }
        }
        Self(remotes)
    }

    /// The `(remote, source ref)` pairs whose fetch writes `reference`: a
    /// remote's refspec maps the ref through its destination, and no negative
    /// refspec of that remote excludes the source. Several pairs when several
    /// refspecs write the one ref; none when no refspec does, as for a ref
    /// left by an earlier configuration or made by hand.
    pub fn sources(&self, reference: &str) -> Vec<(&str, String)> {
        let mut found = Vec::new();
        for (remote, fetch) in &self.0 {
            for (source, destination) in &fetch.mapped {
                let Some(captured) = glob_capture(destination, reference) else {
                    continue;
                };
                let source = source.replacen('*', captured, 1);
                let excluded = fetch
                    .excluded
                    .iter()
                    .any(|pattern| glob_capture(pattern, &source).is_some());
                if !excluded {
                    found.push((remote.as_str(), source));
                }
            }
        }
        found.sort();
        found.dedup();
        found
    }
}

/// What the `*` of a refspec pattern stands for in `name` (empty for a
/// pattern without one), or None where the pattern does not match it.
fn glob_capture<'a>(pattern: &str, name: &'a str) -> Option<&'a str> {
    match pattern.split_once('*') {
        None => (pattern == name).then_some(""),
        Some((head, tail)) => name.strip_prefix(head)?.strip_suffix(tail),
    }
}

/// Modified tracked files and untracked files in a worktree, by count.
/// `ls-files` reads the index's stat data and lists untracked paths; it does
/// not hash file contents, so a worktree holding a large dirty file costs
/// nothing to describe.
pub fn worktree_dirt(cwd: &Path) -> Result<(usize, usize)> {
    let modified = git(cwd, &["ls-files", "--modified", "--exclude-standard"])?;
    let untracked = git(cwd, &["ls-files", "--others", "--exclude-standard"])?;
    Ok((modified.lines().count(), untracked.lines().count()))
}

/// One entry from `git worktree list --porcelain`. A detached worktree has no
/// branch association; its path is still useful when another durable record
/// names that exact checkout. Git marks an entry `prunable` after its path
/// disappears, so consumers can distinguish a live checkout from stale
/// administrative state.
#[derive(Debug, Clone)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub prunable: bool,
}

/// Read Git's current worktree inventory, preserving the order Git reports.
pub fn worktree_inventory(cwd: &Path) -> Result<Vec<WorktreeEntry>> {
    let out = git(cwd, &["worktree", "list", "--porcelain"])?;
    let mut entries = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;
    let mut current_prunable = false;
    for line in out.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(path) = current_path.replace(PathBuf::from(path)) {
                entries.push(WorktreeEntry {
                    path,
                    branch: current_branch.take(),
                    prunable: current_prunable,
                });
            }
            current_branch = None;
            current_prunable = false;
        } else if let Some(branch) = line.strip_prefix("branch ") {
            current_branch = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string(),
            );
        } else if line == "detached" {
            current_branch = None;
        } else if line == "prunable" || line.starts_with("prunable ") {
            current_prunable = true;
        }
    }
    if let Some(path) = current_path {
        entries.push(WorktreeEntry {
            path,
            branch: current_branch,
            prunable: current_prunable,
        });
    }
    Ok(entries)
}

/// The live worktree (if any) that has `branch` checked out.
pub fn worktree_for_branch(cwd: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let inventory = worktree_inventory(cwd)?;
    Ok(inventory
        .iter()
        .find(|entry| {
            !entry.prunable && entry.path.exists() && entry.branch.as_deref() == Some(branch)
        })
        .map(|entry| entry.path.clone()))
}

/// The primary worktree path (the first entry from `git worktree list`),
/// including when its HEAD is detached.
pub fn primary_worktree(cwd: &Path) -> Result<PathBuf> {
    let out = git(cwd, &["worktree", "list", "--porcelain"])?;
    out.lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .context("git worktree list did not contain a primary worktree")
}

pub fn update_ref(cwd: &Path, name: &str, value: &str) -> Result<()> {
    git(cwd, &["update-ref", name, value])?;
    Ok(())
}

/// One ref move: the value the ref is expected to hold, and the one it is to
/// hold instead.
pub struct RefUpdate {
    pub name: String,
    pub old: String,
    pub new: String,
}

/// Move every named ref in one Git transaction: either all of them land or
/// none of them does.
///
/// Each move states the value its ref is expected to hold, so a ref something
/// else moved in the meantime fails the whole transaction rather than being
/// overwritten. Moving refs one at a time instead leaves a repository whose
/// refs sit on two histories whenever anything interrupts the sequence, and
/// no reader can tell which of them is the one that was meant.
pub fn update_refs(cwd: &Path, updates: &[RefUpdate]) -> Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    let mut input = String::from("start\n");
    for RefUpdate { name, old, new } in updates {
        input.push_str(&format!("update {name} {new} {old}\n"));
    }
    // `prepare` takes the lock on every ref and `commit` releases it, so a
    // failure anywhere in between is a transaction Git rolls back itself.
    input.push_str("prepare\ncommit\n");
    git_piped(cwd, &["update-ref", "--stdin"], input.as_bytes())?;
    Ok(())
}

/// What a ref holds, or nothing where the repository has no such ref. An
/// annotated tag answers with its tag object, which is what its ref holds.
pub fn ref_value(cwd: &Path, name: &str) -> Result<Option<String>> {
    let mut command = git_command();
    command
        .args(["rev-parse", "--verify", "--quiet", name])
        .current_dir(cwd);
    let out = command_output(&mut command)
        .with_context(|| format!("failed to read {name} in {}", cwd.display()))?;
    if !out.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

pub fn delete_ref(cwd: &Path, name: &str) -> Result<()> {
    git(cwd, &["update-ref", "-d", name])?;
    Ok(())
}

/// Where the pins that hold each patchset's head live.
pub const KEEP_PREFIX: &str = "refs/arc/keep/";

/// One retention ref per patchset: reviewed heads must stay reachable
/// individually, including across branch rewinds.
pub fn retention_ref(change_id: &str, patchset_id: &str) -> String {
    format!("{KEEP_PREFIX}{change_id}/{patchset_id}")
}

pub fn retention_prefix(change_id: &str) -> String {
    format!("{KEEP_PREFIX}{change_id}/")
}

/// The ref that keeps a recorded tree reachable. A tree named only in the
/// ledger is a string; Git does not read JSON, and a garbage collection would
/// take the evidence away while the claim to it remained.
pub fn tree_retention_ref(change_id: &str, event_id: &str) -> String {
    format!("refs/arc/tree/{change_id}/{event_id}")
}

pub fn tree_retention_prefix(change_id: &str) -> String {
    format!("refs/arc/tree/{change_id}/")
}

/// The ref that keeps a synthesized merge reachable while evidence cites it.
///
/// The commit is on no branch, so nothing else holds it; naming it by its own
/// id makes re-evaluating the same merge idempotent rather than accumulating a
/// ref per run.
pub fn merge_retention_ref(change_id: &str, commit: &str) -> String {
    format!("refs/arc/tree/{change_id}/merge-{commit}")
}

/// Where every commit arc holds as evidence of what was read lives.
pub const RETAINED_PREFIX: &str = "refs/arc/retained/";

/// The ref that keeps a commit the ledger cites reachable once a rewrite has
/// replaced it.
///
/// A pin names the commit a change is at now; this names one it was at when
/// something read it. The commit is part of the name, so one subject can hold
/// several and a retry that finds the ref already there has nothing to do.
/// `subject` is the patchset the commit was the head of, or the event that
/// recorded a verdict on it.
pub fn retained_ref(change_id: &str, subject: &str, commit: &str) -> String {
    format!("{RETAINED_PREFIX}{change_id}/{subject}/{commit}")
}

pub fn retained_prefix(change_id: &str) -> String {
    format!("{RETAINED_PREFIX}{change_id}/")
}

/// The object id a ref has when it does not exist, in the width of `like`.
/// A ref transaction created from it creates the ref, and one moving to it
/// deletes the ref.
pub fn absent_oid(like: &str) -> String {
    "0".repeat(like.len())
}

/// Which of these commits no ref reaches. Reflogs are not refs: a commit
/// only a reflog holds is one the next garbage collection takes.
///
/// One walk from every candidate that stops at whatever any ref already
/// reaches, so the cost follows the unreachable part of the history rather
/// than the whole of it. Every candidate has to be a commit this repository
/// holds; [`commits_among`] says which are.
pub fn unreachable_from_refs(cwd: &Path, commits: &[&str]) -> Result<BTreeSet<String>> {
    if commits.is_empty() {
        return Ok(BTreeSet::new());
    }
    let candidates: BTreeSet<&str> = commits.iter().copied().collect();
    // Stdin keeps a long ledger independent of the process argument limit.
    let input = commits
        .iter()
        .map(|commit| format!("{commit}\n"))
        .collect::<String>();
    let out = git_piped(
        cwd,
        &["rev-list", "--stdin", "--not", "--all"],
        input.as_bytes(),
    )?;
    Ok(String::from_utf8(out)?
        .lines()
        .filter(|line| candidates.contains(line))
        .map(str::to_string)
        .collect())
}

/// Every object reachable from a revision.
///
/// One walk answers for every pin at once. A tree recorded as evidence may be
/// any commit's tree in the integrated history, not only the tip's, so
/// comparing against the tip alone would keep pins for trees Git is already
/// holding.
pub fn reachable_objects(cwd: &Path, rev: &str) -> Result<std::collections::HashSet<String>> {
    let listing = git(cwd, &["rev-list", "--objects", rev])?;
    Ok(listing
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect())
}

/// All refs under a prefix as (refname, object id) pairs.
pub fn list_refs(cwd: &Path, prefix: &str) -> Result<Vec<(String, String)>> {
    let out = git(
        cwd,
        &["for-each-ref", "--format=%(refname) %(objectname)", prefix],
    )?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.to_string()))
        })
        .collect())
}

/// Revisions reachable from the included commits but not the excluded tips.
/// Stdin keeps large ref sets independent of the process argument limit.
pub fn rev_list_excluding(
    cwd: &Path,
    included: &[&str],
    excluded: &[&str],
) -> Result<BTreeSet<String>> {
    let mut input = String::new();
    for revision in included {
        input.push_str(&format!("{revision}\n"));
    }
    for revision in excluded {
        input.push_str(&format!("^{revision}\n"));
    }
    let out = git_piped(cwd, &["rev-list", "--stdin"], input.as_bytes())?;
    Ok(String::from_utf8(out)?
        .lines()
        .map(str::to_string)
        .collect())
}

/// Which of these revisions this repository cannot resolve.
///
/// One `cat-file --batch-check` process answers for every revision at once,
/// because a ledger of any age holds thousands and a check that costs a
/// process each stops being run.
pub fn missing_objects<'a>(
    cwd: &Path,
    revisions: impl Iterator<Item = &'a str>,
) -> Result<Vec<String>> {
    Ok(batch_check(cwd, revisions)?
        .into_iter()
        .filter(|(_, answer)| answer.ends_with(" missing") || answer.ends_with(" ambiguous"))
        .map(|(revision, _)| revision)
        .collect())
}

/// Which of these revisions name a commit this repository holds.
pub fn commits_among<'a>(
    cwd: &Path,
    revisions: impl Iterator<Item = &'a str>,
) -> Result<Vec<String>> {
    Ok(batch_check(cwd, revisions)?
        .into_iter()
        .filter(|(_, answer)| answer.ends_with(" commit"))
        .map(|(revision, _)| revision)
        .collect())
}

/// The commit graph reachable from some revisions, read in one pass.
pub struct CommitGraph {
    /// Each revision that names a commit, with that commit's id.
    pub tips: std::collections::HashMap<String, String>,
    /// Each reachable commit's children.
    children: std::collections::HashMap<String, Vec<String>>,
}

impl CommitGraph {
    /// One `cat-file --batch-check` resolves the revisions, and one
    /// `rev-list --parents` lists every commit reachable from those that name
    /// a commit. A revision naming no commit is absent from `tips`.
    pub fn reachable_from<'a>(
        cwd: &Path,
        revisions: impl Iterator<Item = &'a str>,
    ) -> Result<Self> {
        let mut tips = std::collections::HashMap::new();
        for (revision, answer) in batch_check(cwd, revisions)? {
            if let Some((id, "commit")) = answer.split_once(' ') {
                tips.insert(revision, id.to_string());
            }
        }
        let mut children: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        if !tips.is_empty() {
            let mut query: Vec<&str> = tips.values().map(String::as_str).collect();
            query.sort_unstable();
            query.dedup();
            let input = query.join("\n") + "\n";
            let listed = git_piped(cwd, &["rev-list", "--parents", "--stdin"], input.as_bytes())?;
            for line in String::from_utf8_lossy(&listed).lines() {
                let mut ids = line.split_whitespace();
                let Some(commit) = ids.next() else {
                    continue;
                };
                children.entry(commit.to_string()).or_default();
                for parent in ids {
                    children
                        .entry(parent.to_string())
                        .or_default()
                        .push(commit.to_string());
                }
            }
        }
        Ok(Self { tips, children })
    }

    /// Every commit that has `commit` as an ancestor, `commit` included.
    pub fn descendants(&self, commit: &str) -> std::collections::HashSet<String> {
        let mut seen = std::collections::HashSet::new();
        if !self.children.contains_key(commit) {
            return seen;
        }
        let mut pending = vec![commit.to_string()];
        while let Some(next) = pending.pop() {
            if seen.insert(next.clone()) {
                pending.extend(self.children.get(&next).into_iter().flatten().cloned());
            }
        }
        seen
    }
}

/// Each revision with the `cat-file --batch-check` answer for it, in order.
fn batch_check<'a>(
    cwd: &Path,
    revisions: impl Iterator<Item = &'a str>,
) -> Result<Vec<(String, String)>> {
    use std::io::Write;
    use std::process::Stdio;

    // A revision containing a newline would become two queries and shift every
    // later answer, so those are refused rather than silently misaligned.
    let revisions: Vec<&str> = revisions
        .filter(|revision| !revision.contains(['\n', '\r']))
        .collect();
    if revisions.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = git_command()
        .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run git cat-file")?;
    let query = revisions.join("\n") + "\n";
    let mut stdin = child.stdin.take().context("git cat-file has no stdin")?;
    // The query goes out on its own thread so this one can drain stdout while
    // it is still being written. `cat-file` blocks once its answer pipe fills,
    // and a caller that only starts reading after the last query byte is sent
    // deadlocks against it — both processes asleep writing to a full pipe.
    let writer = std::thread::spawn(move || stdin.write_all(query.as_bytes()));
    let output = child.wait_with_output().context("git cat-file failed")?;
    let wrote = writer
        .join()
        .map_err(|_| anyhow::anyhow!("the git cat-file writer thread panicked"))?;
    // A git that died early makes the write fail with a broken pipe, and its
    // own stderr says more about why than that symptom does. The write failure
    // is worth reporting only when git itself was fine.
    if output.status.success() {
        wrote.context("cannot write to git cat-file")?;
    }
    if !output.status.success() {
        // Reporting nothing missing because the probe failed would turn a
        // broken repository into a clean bill of health.
        anyhow::bail!(
            "git cat-file failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let answers: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    if answers.len() != revisions.len() {
        anyhow::bail!(
            "git cat-file answered {} of {} revisions; refusing to guess which",
            answers.len(),
            revisions.len()
        );
    }

    // One answer line per query line, in order. A resolvable revision answers
    // with its object; anything else names why it could not be resolved.
    Ok(revisions
        .into_iter()
        .map(str::to_string)
        .zip(answers)
        .collect())
}

pub fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let out = git_command()
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(cwd)
        .output()
        .context("failed to spawn git merge-base")?;
    Ok(out.status.success())
}

/// Whether a rebase is stopped part-way in this worktree, waiting for a person.
///
/// Git records that state as a directory beside the index, and `--git-path`
/// resolves it for the worktree rather than for the repository's common dir,
/// so a linked worktree answers about itself.
pub fn rebase_in_progress(cwd: &Path) -> Result<bool> {
    for state_dir in ["rebase-merge", "rebase-apply"] {
        let path = git(cwd, &["rev-parse", "--git-path", state_dir])?;
        if cwd.join(path).exists() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Paths Git could not reconcile, as the index reports them.
pub fn unmerged_paths(cwd: &Path) -> Result<Vec<String>> {
    Ok(git(cwd, &["diff", "--name-only", "--diff-filter=U"])?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// How a rebase ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseOutcome {
    /// Every commit replayed; the branch is on the new base.
    Replayed,
    /// Git stopped on a conflict and left the rebase in progress for a person
    /// to resolve. Nothing is aborted: the partial replay is their work.
    Stopped,
}

/// Replay the worktree's branch onto `onto`.
///
/// A rebase that stops on a conflict is an outcome, not a failure: it leaves
/// state a person continues from. Anything else — an unknown revision, a
/// refusal — is an error, and leaves no rebase in progress to distinguish it.
pub fn rebase(cwd: &Path, onto: &str) -> Result<RebaseOutcome> {
    let Err(refusal) = git(cwd, &["rebase", onto]) else {
        return Ok(RebaseOutcome::Replayed);
    };
    if rebase_in_progress(cwd)? {
        return Ok(RebaseOutcome::Stopped);
    }
    Err(refusal)
}

pub fn commit_count(cwd: &Path, base: &str, head: &str) -> Result<usize> {
    git(cwd, &["rev-list", "--count", &format!("{base}..{head}")])?
        .parse()
        .context("git rev-list --count returned a non-numeric commit count")
}

/// The branch checked out in the primary worktree (the main checkout,
/// always first in `git worktree list`). None when it is detached.
pub fn primary_worktree_branch(cwd: &Path) -> Result<Option<String>> {
    let out = git(cwd, &["worktree", "list", "--porcelain"])?;
    let mut in_first = false;
    for line in out.lines() {
        if line.starts_with("worktree ") {
            if in_first {
                break; // reached the second worktree
            }
            in_first = true;
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            return Ok(Some(b.to_string()));
        }
    }
    Ok(None)
}

pub fn commit_parents(cwd: &Path, rev: &str) -> Result<Vec<String>> {
    let out = git(cwd, &["rev-list", "--parents", "-n", "1", rev])?;
    let mut ids = out.split_whitespace().map(str::to_string);
    ids.next();
    Ok(ids.collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitIdentity {
    pub author_name: String,
    pub author_email: String,
    pub committer_name: String,
    pub committer_email: String,
}

pub fn commit_identity(cwd: &Path, rev: &str) -> Result<CommitIdentity> {
    let out = git(
        cwd,
        &["show", "-s", "--format=%an%x00%ae%x00%cn%x00%ce", rev],
    )?;
    let mut fields = out.split('\0');
    let identity = CommitIdentity {
        author_name: fields
            .next()
            .context("git omitted author name")?
            .to_string(),
        author_email: fields
            .next()
            .context("git omitted author email")?
            .to_string(),
        committer_name: fields
            .next()
            .context("git omitted committer name")?
            .to_string(),
        committer_email: fields
            .next()
            .context("git omitted committer email")?
            .to_string(),
    };
    if fields.next().is_some() {
        bail!("git returned unexpected commit identity fields");
    }
    Ok(identity)
}

/// The tree a revision names, or nothing when the object store holds no such
/// tree. A commit answers with its tree.
pub fn resolve_tree(cwd: &Path, rev: &str) -> Result<Option<String>> {
    if rev.is_empty() || rev.starts_with('-') {
        return Ok(None);
    }
    ref_value(cwd, &format!("{rev}^{{tree}}"))
}

/// The raw type of a held object; missing objects remain absent claims.
pub fn object_type(cwd: &Path, oid: &str) -> Result<Option<String>> {
    let answers = batch_check(cwd, std::iter::once(oid))?;
    let answer = &answers.first().context("invalid object query")?.1;
    let kind = answer
        .rsplit_once(' ')
        .context("git returned no object type")?
        .1;
    Ok((kind != "missing").then(|| kind.to_string()))
}

/// Whether a claimed object ID is spelled as a full ID: 40 or 64 lowercase hex
/// digits. Abbreviations, uppercase spellings and revision expressions name no
/// object by themselves.
pub fn is_full_object_id(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64) && oid.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether a claimed full ID is, here, the raw commit it names. A held tag or
/// blob is not one, and an object this repository lacks stays a claim.
pub fn held_commit(cwd: &Path, oid: &str) -> Result<bool> {
    if !is_full_object_id(oid) {
        return Ok(false);
    }
    Ok(object_type(cwd, oid)?.as_deref() == Some("commit"))
}

pub fn commit_exists(cwd: &Path, oid: &str) -> Result<bool> {
    let object = format!("{oid}^{{commit}}");
    let out = git_command()
        .args(["cat-file", "-e", "--", &object])
        .current_dir(cwd)
        .output()
        .context("failed to spawn git cat-file")?;
    Ok(out.status.success())
}

pub fn commit_tree_unsigned(
    cwd: &Path,
    tree: &str,
    parents: &[&str],
    message: &str,
) -> Result<String> {
    let mut args = vec![
        "-c",
        "commit.gpgsign=false",
        "commit-tree",
        "--no-gpg-sign",
        tree,
    ];
    for parent in parents {
        args.extend(["-p", parent]);
    }
    args.extend(["-m", message]);
    git(cwd, &args)
}

/// An object as `cat-file --batch` answers a name.
enum Object {
    Found {
        id: String,
        kind: String,
        bytes: Vec<u8>,
    },
    Missing,
}

/// One `cat-file --batch` kept open for a checkout, answering one name at a
/// time.
struct ObjectReader {
    _child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
}

impl ObjectReader {
    fn spawn(cwd: &Path) -> Option<Self> {
        let mut child = git_command()
            .args(["cat-file", "--batch"])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take()?;
        let stdout = std::io::BufReader::new(child.stdout.take()?);
        Some(Self {
            _child: child,
            stdin,
            stdout,
        })
    }

    /// The object `name` resolves to, `Missing`, or `None` for any other
    /// answer, such as an ambiguous name, which the caller asks Git for on
    /// its own to report as Git does.
    fn read(&mut self, name: &str) -> Option<Object> {
        use std::io::BufRead;
        writeln!(self.stdin, "{name}").ok()?;
        self.stdin.flush().ok()?;
        let mut header = String::new();
        self.stdout.read_line(&mut header).ok()?;
        let header = header.strip_suffix('\n')?;
        if header.strip_suffix(" missing") == Some(name) {
            return Some(Object::Missing);
        }
        let mut fields = header.split(' ');
        let (Some(id), Some(kind), Some(size), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return None;
        };
        let size: usize = size.parse().ok()?;
        let mut bytes = vec![0; size + 1];
        self.stdout.read_exact(&mut bytes).ok()?;
        bytes.pop().filter(|end| *end == b'\n')?;
        Some(Object::Found {
            id: id.to_string(),
            kind: kind.to_string(),
            bytes,
        })
    }
}

static OBJECT_READERS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, ObjectReader>>,
> = std::sync::OnceLock::new();

/// While the process shares its reads, the object `name` names, read through
/// the one `cat-file --batch` this process keeps for `cwd`. `None` when reads
/// are not shared, the name cannot be one query line, or the reader failed;
/// a failed reader is dropped and the next lookup starts another.
fn shared_object(cwd: &Path, name: &str) -> Option<Object> {
    if !crate::store::shares_reads() || name.contains(['\n', '\r']) {
        return None;
    }
    let mut readers = OBJECT_READERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let reader = match readers.entry(cwd.to_path_buf()) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => entry.insert(ObjectReader::spawn(cwd)?),
    };
    let answer = reader.read(name);
    if answer.is_none() {
        readers.remove(cwd);
    }
    answer
}

/// The paths `git diff --name-only <base>...<head>` lists for each range,
/// keyed by the range. One `cat-file --batch-check` resolves every revision,
/// one `rev-list --parents` reads the history the heads reach, and one
/// `diff-tree --stdin` diffs every range whose base its head reaches, where
/// the merge base is the base itself. Any other range is diffed on its own.
/// A range Git cannot diff is absent.
pub fn changed_paths_of(
    cwd: &Path,
    ranges: &[(String, String)],
) -> std::collections::HashMap<(String, String), Vec<String>> {
    let mut answers = std::collections::HashMap::new();
    let mut alone: Vec<&(String, String)> = Vec::new();
    let mut batched: Vec<(&(String, String), String, String)> = Vec::new();
    let names: Vec<String> = ranges
        .iter()
        .flat_map(|(base, head)| {
            [
                format!("{base}^{{commit}}"),
                format!("{head}^{{commit}}"),
                format!("{base}^{{tree}}"),
                format!("{head}^{{tree}}"),
            ]
        })
        .collect();
    let resolved: std::collections::HashMap<String, String> =
        batch_check(cwd, names.iter().map(String::as_str))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(name, answer)| {
                let (id, kind) = answer.split_once(' ')?;
                matches!(kind, "commit" | "tree").then(|| (name, id.to_string()))
            })
            .collect();
    let commit = |rev: &str| resolved.get(&format!("{rev}^{{commit}}"));
    let tree = |rev: &str| resolved.get(&format!("{rev}^{{tree}}"));
    let heads: BTreeSet<&String> = ranges.iter().filter_map(|(_, head)| commit(head)).collect();
    let parents = parent_map(cwd, heads.into_iter().map(String::as_str));
    for range in ranges {
        let (base, head) = range;
        match (commit(base), commit(head), tree(base), tree(head), &parents) {
            (Some(base_id), Some(head_id), Some(base_tree), Some(head_tree), Some(parents))
                if reaches(parents, head_id, base_id) =>
            {
                batched.push((range, base_tree.clone(), head_tree.clone()));
            }
            _ => alone.push(range),
        }
    }
    if !batched.is_empty() {
        let input: String = batched
            .iter()
            .map(|(_, base, head)| format!("{base} {head}\n"))
            .collect();
        let mut args = vec!["diff-tree", "--stdin", "-r", "--name-only"];
        args.extend(rename_detection(cwd));
        match git_piped(cwd, &args, input.as_bytes()) {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output);
                let mut lines = text.lines().peekable();
                for (range, base, head) in &batched {
                    let header = format!("{base} {head}");
                    if lines.next_if(|line| *line == header).is_none() {
                        break;
                    }
                    let mut paths = Vec::new();
                    while let Some(line) = lines
                        .next_if(|line| batched.iter().all(|(_, b, h)| *line != format!("{b} {h}")))
                    {
                        let line = line.trim();
                        if !line.is_empty() {
                            paths.push(line.to_string());
                        }
                    }
                    answers.insert((*range).clone(), paths);
                }
            }
            Err(_) => alone.extend(batched.iter().map(|(range, _, _)| *range)),
        }
        alone.extend(
            batched
                .iter()
                .map(|(range, _, _)| *range)
                .filter(|range| !answers.contains_key(*range)),
        );
    }
    for range in alone {
        if let Ok(paths) = changed_paths(cwd, &range.0, &range.1) {
            answers.insert(range.clone(), paths);
        }
    }
    answers
}

/// The rename detection `git diff` applies by default, as `diff.renames`
/// sets it, for a plumbing diff that applies none of its own.
fn rename_detection(cwd: &Path) -> Option<&'static str> {
    let setting = git(cwd, &["config", "--get", "diff.renames"]).unwrap_or_default();
    match setting.trim().to_ascii_lowercase().as_str() {
        "false" | "no" | "off" | "0" => None,
        "copy" | "copies" => Some("-C"),
        _ => Some("-M"),
    }
}

/// Each commit the `heads` reach, with its parents, from one
/// `rev-list --parents`; `None` when Git could not list them.
fn parent_map<'a>(
    cwd: &Path,
    heads: impl Iterator<Item = &'a str>,
) -> Option<std::collections::HashMap<String, Vec<String>>> {
    let input: String = heads.map(|head| format!("{head}\n")).collect();
    if input.is_empty() {
        return Some(Default::default());
    }
    let listed = git_piped(cwd, &["rev-list", "--parents", "--stdin"], input.as_bytes()).ok()?;
    Some(
        String::from_utf8_lossy(&listed)
            .lines()
            .filter_map(|line| {
                let mut ids = line.split_whitespace().map(str::to_string);
                Some((ids.next()?, ids.collect()))
            })
            .collect(),
    )
}

/// Whether `ancestor` is `from` or reachable from it through `parents`.
fn reaches(
    parents: &std::collections::HashMap<String, Vec<String>>,
    from: &str,
    ancestor: &str,
) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut pending = vec![from];
    while let Some(commit) = pending.pop() {
        if commit == ancestor {
            return true;
        }
        if seen.insert(commit) {
            pending.extend(
                parents
                    .get(commit)
                    .into_iter()
                    .flatten()
                    .map(String::as_str),
            );
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository holding one commit, with the machine's own configuration
    /// out of scope.
    fn fixture(tmp: &tempfile::TempDir, config: &[&str]) -> PathBuf {
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let mut setup: Vec<Vec<&str>> = vec![
            vec!["init", "-b", "master"],
            vec!["config", "user.name", "Tester"],
            vec!["config", "user.email", "tester@example.invalid"],
            vec!["config", "commit.gpgsign", "false"],
        ];
        if !config.is_empty() {
            let mut line = vec!["config"];
            line.extend_from_slice(config);
            setup.push(line);
        }
        for args in setup {
            git(&root, &args).unwrap();
        }
        std::fs::write(root.join("one.txt"), "one\n").unwrap();
        git(&root, &["add", "."]).unwrap();
        git(
            &root,
            &[
                "commit",
                "--cleanup=verbatim",
                "-m",
                "feat: one  \n\nbody\n",
            ],
        )
        .unwrap();
        root
    }

    #[test]
    fn a_held_commit_is_the_raw_commit_a_lowercase_full_id_names() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = fixture(&tmp, &[]);
        let commit = git(&root, &["rev-parse", "HEAD"]).unwrap();
        git(&root, &["tag", "-a", "-m", "annotated", "v1"]).unwrap();
        let tag = git(&root, &["rev-parse", "v1"]).unwrap();
        let blob = git(&root, &["rev-parse", "HEAD:one.txt"]).unwrap();
        assert!(held_commit(&root, &commit).unwrap());
        for refused in [
            tag,
            blob,
            commit.to_uppercase(),
            commit[..12].to_string(),
            "HEAD".to_string(),
            "b".repeat(40),
        ] {
            assert!(!held_commit(&root, &refused).unwrap(), "{refused}");
        }
        assert!(is_full_object_id(&"0".repeat(64)));
        assert!(!is_full_object_id(&"A".repeat(40)));
    }

    #[test]
    fn batched_commits_match_individual_reads_across_a_merge() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = fixture(&tmp, &[]);
        let base = git(&root, &["rev-parse", "HEAD"]).unwrap();
        git(&root, &["checkout", "-b", "side"]).unwrap();
        git(
            &root,
            &[
                "commit",
                "--allow-empty",
                "-m",
                "side\n\nmultiple lines\nbody",
            ],
        )
        .unwrap();
        git(&root, &["checkout", "master"]).unwrap();
        git(&root, &["commit", "--allow-empty", "-m", "main"]).unwrap();
        git(
            &root,
            &["merge", "--no-ff", "side", "-m", "merge\n\nmerge body"],
        )
        .unwrap();
        let range = git(&root, &["rev-list", "--reverse", &format!("{base}..HEAD")]).unwrap();
        let revisions: Vec<_> = range.lines().collect();
        let batched = read_commits(&root, &revisions).unwrap();
        assert_eq!(batched.len(), 3);
        assert_eq!(batched.last().unwrap().parents.len(), 2);
        for (revision, commit) in revisions.iter().zip(batched) {
            let individual = read_commit(&root, revision).unwrap();
            assert_eq!(commit.tree, individual.tree);
            assert_eq!(commit.parents, individual.parents);
            assert_eq!(commit.author, individual.author);
            assert_eq!(commit.committer, individual.committer);
            assert_eq!(commit.encoding, individual.encoding);
            assert_eq!(commit.headers, individual.headers);
            assert_eq!(commit.message, individual.message);
        }
        assert!(read_commits(&root, &[]).unwrap().is_empty());
        assert!(read_commits(&root, &["absent"]).is_err());
        assert!(read_commits(&root, &["HEAD^{tree}"]).is_err());
        assert!(read_commits(&root, &["HEAD\nHEAD"]).is_err());
        git(&root, &["tag", "-a", "fixture-tag", "-m", "fixture"]).unwrap();
        let tagged = read_commits(&root, &["fixture-tag"]).unwrap();
        assert_eq!(
            tagged[0].message,
            read_commit(&root, "fixture-tag").unwrap().message
        );
        with_deadline(Some(Instant::now() + Duration::from_secs(30)), || {
            let repeated = read_commits(&root, &vec!["HEAD"; 20_000])?;
            assert_eq!(repeated.len(), 20_000);
            assert!(repeated
                .iter()
                .all(|commit| commit.message == tagged[0].message));
            Ok(())
        })
        .unwrap();
    }

    /// A Git command that wants a message must take the one it already has
    /// rather than reach for an editor, whatever the repository or the
    /// environment names as one. `git commit --amend` is the shortest command
    /// that asks: without a message on the line, it opens an editor.
    #[test]
    fn a_git_command_never_reaches_an_editor() {
        let tmp = tempfile::TempDir::new().unwrap();
        // An editor that fails is how an editor being consulted at all is
        // made visible; a windowed one would instead block until somebody
        // closed it.
        let root = fixture(&tmp, &["core.editor", "false"]);
        git(&root, &["commit", "--amend"]).expect("an amend must not consult an editor");
    }

    /// Assembling a commit object here has to agree with `commit-tree` on
    /// every byte, or the header order, the identity lines or the message
    /// boundary are wrong and a carried header would come with a silent
    /// change to something else.
    #[test]
    fn a_hand_built_commit_is_the_one_commit_tree_writes() {
        let tmp = tempfile::TempDir::new().unwrap();
        // The fixture's message carries an interior blank line and trailing
        // whitespace, so the boundary between the headers and the message is
        // checked rather than assumed.
        let root = fixture(&tmp, &[]);

        let head = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let commit = read_commit(&root, &head).unwrap();
        assert!(commit.extra_header_fields().is_empty());
        assert!(commit.commit_tree_writes_it_back());
        assert_eq!(write_commit_object(&root, &commit, None).unwrap(), head);
        assert_eq!(commit_tree_as(&root, &commit, None).unwrap(), head);
    }

    /// Git's own order is the common one, not the only one. A header before
    /// `encoding`, or an identity line with whitespace of its own, is part of
    /// the object: reassembling it in a canonical order, or from parsed name
    /// and email, yields a different commit presented as the same one
    /// re-signed. `commit-tree` cannot write either back, and must say so
    /// rather than produce the tidier object.
    #[test]
    fn a_header_order_and_identity_spelling_are_the_object_s_own() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = fixture(&tmp, &[]);
        let raw = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
                    author A  <a@example.invalid> 1700000000 +0000\n\
                    committer C <c@example.invalid> 1700000001 +0000\n\
                    mergetag object 2222222222222222222222222222222222222222\n\
                    \x20type commit\n\
                    encoding ISO-8859-1\n\
                    \nthe message\n";
        let commit = parse_commit_object(raw).unwrap();
        // The author's name ends where the email begins, so the second space
        // belongs to neither part and only the raw line still holds it.
        assert_eq!(commit.author.name, "A");
        assert_eq!(
            commit
                .headers
                .iter()
                .map(|h| h.field.as_str())
                .collect::<Vec<_>>(),
            ["tree", "author", "committer", "mergetag", "encoding"]
        );
        assert!(!commit.commit_tree_writes_it_back());
        assert!(commit_tree_as(&root, &commit, None).is_err());
        assert_eq!(
            String::from_utf8_lossy(&commit_object_text(&commit, None)),
            String::from_utf8_lossy(raw)
        );

        // Whitespace alone is enough: a commit carrying no unusual header is
        // still not one `commit-tree` can write back.
        let plain = parse_commit_object(
            b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
              author A  <a@example.invalid> 1700000000 +0000\n\
              committer C <c@example.invalid> 1700000001 +0000\n\
              \nthe message\n",
        )
        .unwrap();
        assert!(plain.extra_header_fields().is_empty());
        assert!(!plain.commit_tree_writes_it_back());

        // A rewrite replaces the parents and nothing else, and a commit that
        // had none takes them where Git writes the first one.
        let mut reparented = commit.clone();
        reparented.parents = vec!["1".repeat(40)];
        assert_eq!(
            String::from_utf8_lossy(&commit_object_text(&reparented, None)),
            String::from_utf8_lossy(raw).replace(
                "author A ",
                &format!("parent {}\nauthor A ", "1".repeat(40))
            )
        );
    }

    /// A header outside the set `commit-tree` has flags for is carried, and
    /// carrying it is the only difference: recreating the commit unsigned
    /// reproduces its object id.
    #[test]
    fn an_uninterpreted_header_is_carried_verbatim() {
        let raw = b"tree 0000000000000000000000000000000000000000\n\
                    parent 1111111111111111111111111111111111111111\n\
                    author A <a@example.invalid> 1700000000 +0000\n\
                    committer C <c@example.invalid> 1700000001 +0000\n\
                    mergetag object 2222222222222222222222222222222222222222\n\
                    \x20type commit\n\
                    \x20tag v1\n\
                    \x20\n\
                    \x20a release\n\
                    gpgsig -----BEGIN PGP SIGNATURE-----\n\
                    \x20\n\
                    \x20abc\n\
                    \x20-----END PGP SIGNATURE-----\n\
                    \nthe message\n";
        let commit = parse_commit_object(raw).unwrap();
        assert_eq!(commit.extra_header_fields(), vec!["mergetag".to_string()]);
        assert_eq!(
            commit
                .headers
                .iter()
                .find(|header| header.field == "mergetag")
                .unwrap()
                .block,
            "mergetag object 2222222222222222222222222222222222222222\n type commit\n tag v1\n \n a release"
        );
        assert_eq!(commit.message, b"the message\n");
        // The signature covered the object it was read from, so a recreation
        // carries everything but that.
        let rebuilt = commit_object_text(&commit, None);
        assert_eq!(
            String::from_utf8_lossy(&rebuilt),
            String::from_utf8_lossy(raw).replace(
                "gpgsig -----BEGIN PGP SIGNATURE-----\n \n abc\n -----END PGP SIGNATURE-----\n",
                ""
            )
        );
        // Signing writes the header Git writes: the first line after the
        // field name, the rest indented by one space.
        let signed = commit_object_text(&commit, Some(b"-----BEGIN PGP SIGNATURE-----\n\nabc\n"));
        assert!(String::from_utf8_lossy(&signed)
            .contains("gpgsig -----BEGIN PGP SIGNATURE-----\n \n abc\n\nthe message\n"));
    }

    #[cfg(unix)]
    #[test]
    fn inherited_probe_pipes_obey_the_deadline() {
        for script in ["sleep 2 & wait", "sleep 2 & exit 0"] {
            let started = Instant::now();
            with_deadline(Some(started + Duration::from_millis(100)), || {
                let mut command = Command::new("sh");
                command.args(["-c", script]);
                let result = command_output(&mut command);
                assert!(started.elapsed() < Duration::from_secs(1));
                assert!(format!("{:#}", result.unwrap_err()).contains("timed out"));
                Ok(())
            })
            .unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn inherited_piped_probe_streams_obey_the_deadline() {
        for script in [
            "sleep 2 & wait",
            "sleep 2 <&0 >/dev/null 2>/dev/null & exit 0",
        ] {
            let started = Instant::now();
            with_deadline(Some(started + Duration::from_millis(100)), || {
                let result = piped_output(
                    "sh",
                    &["-c".into(), script.into()],
                    &vec![b'x'; 1024 * 1024],
                    |_| {},
                );
                assert!(started.elapsed() < Duration::from_secs(1));
                assert!(format!("{:#}", result.unwrap_err()).contains("timed out"));
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn piped_probe_respects_the_git_deadline() {
        let started = Instant::now();
        with_deadline(Some(started + Duration::from_millis(50)), || {
            let error = piped_output("sleep", &["5".to_string()], &[], |_| {}).unwrap_err();
            assert!(format!("{error:#}").contains("timed out"));
            Ok(())
        })
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn deadline_kills_and_reaps_a_slow_probe() {
        let started = Instant::now();
        with_deadline(Some(started + Duration::from_millis(50)), || {
            let mut command = Command::new("sleep");
            command.arg("5");
            let error = command_output(&mut command).unwrap_err();
            assert!(error.to_string().contains("timed out"));
            Ok(())
        })
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// A query larger than the pipe buffers on both sides must still answer.
    ///
    /// Sending the whole query before reading any of it deadlocks: `cat-file`
    /// sleeps writing answers into a full pipe while the caller sleeps writing
    /// queries into another one. Twenty thousand revisions puts both sides well
    /// past the 64 KiB a pipe holds, so this fails by timing out rather than by
    /// hanging the suite.
    #[test]
    fn a_query_larger_than_the_pipe_buffer_still_answers() {
        let revisions: Vec<String> = (0..20_000).map(|n| format!("{n:040x}")).collect();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let answered = missing_objects(Path::new("."), revisions.iter().map(String::as_str))
                .map(|missing| missing.len());
            let _ = sender.send(answered);
        });
        let answered = receiver
            .recv_timeout(Duration::from_secs(60))
            .expect("missing_objects deadlocked against git cat-file")
            .expect("missing_objects failed");
        // Every one of them is fabricated, so every one is unresolvable.
        assert_eq!(answered, 20_000);
    }

    /// The sources of `reference` as `remote → source` strings.
    fn sources(map: &FetchMap, reference: &str) -> Vec<String> {
        map.sources(reference)
            .into_iter()
            .map(|(remote, source)| format!("{remote} → {source}"))
            .collect()
    }

    #[test]
    fn a_fetch_refspec_maps_a_tracking_ref_to_its_source() {
        let map = FetchMap::from_refspecs([
            ("origin", "+refs/heads/*:refs/remotes/origin/*"),
            ("pinned", "refs/heads/main:refs/remotes/pinned/trunk"),
            ("alias", "+refs/heads/develop:refs/remotes/alias/delivery"),
            ("mirror", "+refs/heads/*:refs/remotes/shadow/*"),
            ("suffixed", "+refs/heads/f*o:refs/remotes/suffixed/f*o"),
        ]);
        let cases = [
            ("refs/remotes/origin/main", vec!["origin → refs/heads/main"]),
            (
                "refs/remotes/origin/arc/alpha",
                vec!["origin → refs/heads/arc/alpha"],
            ),
            (
                "refs/remotes/pinned/trunk",
                vec!["pinned → refs/heads/main"],
            ),
            ("refs/remotes/pinned/main", vec![]),
            (
                "refs/remotes/alias/delivery",
                vec!["alias → refs/heads/develop"],
            ),
            (
                "refs/remotes/shadow/arc/alpha",
                vec!["mirror → refs/heads/arc/alpha"],
            ),
            (
                "refs/remotes/suffixed/fizzo",
                vec!["suffixed → refs/heads/fizzo"],
            ),
            ("refs/remotes/suffixed/fizz", vec![]),
            ("refs/remotes/unknown/main", vec![]),
        ];
        for (reference, expected) in cases {
            assert_eq!(sources(&map, reference), expected, "{reference}");
        }
    }

    #[test]
    fn several_fetch_lines_and_nested_remote_names_each_contribute() {
        let map = FetchMap::from_refspecs([
            ("origin", "+refs/heads/*:refs/remotes/origin/*"),
            ("origin", "+refs/heads/develop:refs/remotes/origin/x/foo"),
            ("origin/x", "+refs/heads/*:refs/remotes/origin/x/*"),
        ]);
        assert_eq!(
            sources(&map, "refs/remotes/origin/main"),
            ["origin → refs/heads/main"]
        );
        assert_eq!(
            sources(&map, "refs/remotes/origin/x/foo"),
            [
                "origin → refs/heads/develop",
                "origin → refs/heads/x/foo",
                "origin/x → refs/heads/foo",
            ]
        );
        assert_eq!(
            sources(&map, "refs/remotes/origin/x/bar"),
            ["origin → refs/heads/x/bar", "origin/x → refs/heads/bar"]
        );
    }

    #[test]
    fn a_negative_refspec_excludes_its_own_remotes_sources() {
        let map = FetchMap::from_refspecs([
            ("origin", "+refs/heads/*:refs/remotes/origin/*"),
            ("origin", "^refs/heads/secret"),
            ("origin", "^refs/heads/wip/*"),
            ("backup", "+refs/heads/*:refs/remotes/backup/*"),
        ]);
        assert!(map.sources("refs/remotes/origin/secret").is_empty());
        assert!(map.sources("refs/remotes/origin/wip/a/b").is_empty());
        assert_eq!(
            sources(&map, "refs/remotes/origin/secrets"),
            ["origin → refs/heads/secrets"]
        );
        assert_eq!(
            sources(&map, "refs/remotes/backup/secret"),
            ["backup → refs/heads/secret"]
        );
    }

    #[test]
    fn a_refspec_git_would_reject_or_that_fetches_nowhere_maps_nothing() {
        let map = FetchMap::from_refspecs([
            ("origin", "refs/heads/main"),
            ("origin", "refs/heads/main:"),
            ("origin", ":refs/remotes/origin/main"),
            ("origin", "refs/heads/*:refs/remotes/origin/fixed"),
            ("origin", "refs/heads/fixed:refs/remotes/origin/*"),
            ("origin", "refs/heads/*/*:refs/remotes/origin/*/*"),
            ("origin", "^refs/heads/a:refs/remotes/origin/a"),
            ("origin", ""),
        ]);
        for reference in [
            "refs/remotes/origin/main",
            "refs/remotes/origin/fixed",
            "refs/remotes/origin/a",
            "refs/remotes/origin/x/y",
        ] {
            assert!(map.sources(reference).is_empty(), "{reference}");
        }
    }

    #[test]
    fn the_fetch_map_reads_every_remotes_configured_refspecs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = fixture(&tmp, &[]);
        for remote in ["origin", "nested/mirror"] {
            git(
                &root,
                &["remote", "add", remote, "https://example.invalid/r"],
            )
            .unwrap();
        }
        git(
            &root,
            &[
                "config",
                "--add",
                "remote.origin.fetch",
                "^refs/heads/hidden",
            ],
        )
        .unwrap();
        git(
            &root,
            &[
                "config",
                "--add",
                "remote.origin.fetch",
                "+refs/heads/x:refs/remotes/origin/y",
            ],
        )
        .unwrap();
        let map = FetchMap::read(&root).unwrap();
        assert_eq!(
            sources(&map, "refs/remotes/origin/main"),
            ["origin → refs/heads/main"]
        );
        assert!(map.sources("refs/remotes/origin/hidden").is_empty());
        assert_eq!(
            sources(&map, "refs/remotes/origin/y"),
            ["origin → refs/heads/x", "origin → refs/heads/y"]
        );
        assert_eq!(
            sources(&map, "refs/remotes/nested/mirror/main"),
            ["nested/mirror → refs/heads/main"]
        );
    }
}
