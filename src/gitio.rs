use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to spawn git in {}", cwd.display()))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
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

pub fn rev_parse(cwd: &Path, rev: &str) -> Result<String> {
    git(
        cwd,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
}

pub fn head(cwd: &Path) -> Result<String> {
    rev_parse(cwd, "HEAD")
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

pub fn commit_exists(cwd: &Path, oid: &str) -> Result<bool> {
    let object = format!("{oid}^{{commit}}");
    let out = Command::new("git")
        .args(["cat-file", "-e", "--", &object])
        .current_dir(cwd)
        .output()
        .context("failed to spawn git cat-file")?;
    Ok(out.status.success())
}
