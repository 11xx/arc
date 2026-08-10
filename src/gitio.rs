use anyhow::{bail, Context, Result};
use std::cell::Cell;
use std::io::Write;
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

pub fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
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
    let output = Command::new("git")
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

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        let now = Instant::now();
        if now >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("git probe timed out");
        }
        thread::sleep((deadline - now).min(Duration::from_millis(10)));
    }
}

/// The common Git directory shared by every worktree of one repository.
pub fn common_dir(cwd: &Path) -> Result<PathBuf> {
    let raw = git(cwd, &["rev-parse", "--git-common-dir"])?;
    let p = PathBuf::from(raw);
    let abs = if p.is_absolute() { p } else { cwd.join(p) };
    Ok(std::fs::canonicalize(&abs).unwrap_or(abs))
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
    let output = Command::new("git")
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
    let out = Command::new("git")
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
    rev_parse(cwd, &format!("refs/heads/{branch}"))
}

pub fn current_branch(cwd: &Path) -> Result<Option<String>> {
    let out = Command::new("git")
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .current_dir(cwd)
        .output()?;
    if out.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

pub fn merge_base(cwd: &Path, a: &str, b: &str) -> Result<String> {
    git(cwd, &["merge-base", a, b])
}

pub fn merge_conflicts(cwd: &Path, target_rev: &str, head_rev: &str) -> Result<bool> {
    let out = Command::new("git")
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
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        code => bail!(
            "git merge-tree failed with exit code {}: {}",
            code.map_or_else(|| "signal".to_string(), |code| code.to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

pub fn blob_oid(cwd: &Path, rev: &str, path: &str) -> Option<String> {
    git(cwd, &["rev-parse", "--verify", &format!("{rev}:{path}")]).ok()
}

pub fn is_clean(cwd: &Path) -> Result<bool> {
    Ok(git(cwd, &["status", "--porcelain"])?.is_empty())
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

pub fn add_worktree(cwd: &Path, path: &Path, branch: &str) -> Result<()> {
    git(
        cwd,
        &[
            "worktree",
            "add",
            path.to_str().context("non-UTF8 worktree path")?,
            branch,
        ],
    )?;
    Ok(())
}

/// The worktree (if any) that has `branch` checked out.
pub fn worktree_for_branch(cwd: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let out = git(cwd, &["worktree", "list", "--porcelain"])?;
    let wanted = format!("refs/heads/{branch}");
    let mut current: Option<PathBuf> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch ") {
            if b == wanted {
                return Ok(current);
            }
        }
    }
    Ok(None)
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

pub fn delete_ref(cwd: &Path, name: &str) -> Result<()> {
    git(cwd, &["update-ref", "-d", name])?;
    Ok(())
}

/// One retention ref per patchset: reviewed heads must stay reachable
/// individually, including across branch rewinds.
pub fn retention_ref(change_id: &str, patchset_id: &str) -> String {
    format!("refs/arc/keep/{change_id}/{patchset_id}")
}

pub fn retention_prefix(change_id: &str) -> String {
    format!("refs/arc/keep/{change_id}/")
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

/// Which of these revisions this repository cannot resolve.
///
/// One `cat-file --batch-check` process answers for every revision at once,
/// because a ledger of any age holds thousands and a check that costs a
/// process each stops being run.
pub fn missing_objects<'a>(
    cwd: &Path,
    revisions: impl Iterator<Item = &'a str>,
) -> Result<Vec<String>> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let revisions: Vec<&str> = revisions.collect();
    if revisions.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = Command::new("git")
        .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("cannot run git cat-file")?;
    let query = revisions.join("\n") + "\n";
    child
        .stdin
        .take()
        .context("git cat-file has no stdin")?
        .write_all(query.as_bytes())
        .context("cannot write to git cat-file")?;
    let output = child.wait_with_output().context("git cat-file failed")?;
    let answers = String::from_utf8_lossy(&output.stdout);

    // One answer line per query line, in order. A resolvable revision answers
    // with its object; a missing one answers "<name> missing".
    let mut missing = Vec::new();
    for (revision, answer) in revisions.iter().zip(answers.lines()) {
        if answer.ends_with(" missing") || answer.ends_with(" ambiguous") {
            missing.push((*revision).to_string());
        }
    }
    Ok(missing)
}

pub fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let out = Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(cwd)
        .output()
        .context("failed to spawn git merge-base")?;
    Ok(out.status.success())
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

pub fn commit_exists(cwd: &Path, oid: &str) -> Result<bool> {
    let object = format!("{oid}^{{commit}}");
    let out = Command::new("git")
        .args(["cat-file", "-e", "--", &object])
        .current_dir(cwd)
        .output()
        .context("failed to spawn git cat-file")?;
    Ok(out.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
