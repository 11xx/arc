//! One commit for a receiver that takes one.
//!
//! A squash is a new head, not an edit of an old one: evidence and verdicts
//! bound to the earlier heads stay where they were, and the single commit is
//! recorded as a patchset that is gated and reviewed like any other.

use crate::commands::Ctx;
use crate::gitio;
use crate::model::Payload;
use anyhow::{bail, Context, Result};

pub fn squash(ctx: &Ctx, reference: &str, message: &str) -> Result<()> {
    let message = message.trim();
    if message.is_empty() {
        bail!("--message must name the single commit");
    }
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let st = store.state(&change_id)?;
    // A squash ends in a patchset, so the phase authority is asked about that
    // patchset before the branch moves rather than after.
    super::ensure_append_allowed(
        &st,
        &Payload::PatchsetAdded {
            patchset_id: String::new(),
            base: String::new(),
            head: String::new(),
            merge_base: None,
            brief_ref: None,
            author_name: None,
            author_email: None,
            committer_name: None,
            committer_email: None,
            contributors: Vec::new(),
            claim_id: None,
            claim_actor: None,
            journal_refs: Vec::new(),
            thread: None,
        },
    )?;
    let worktree = gitio::worktree_for_branch(&ctx.cwd, &st.branch)?.with_context(|| {
        format!(
            "no worktree has {} checked out; squash runs in the checkout that holds it",
            st.branch
        )
    })?;
    if !gitio::is_clean(&worktree)? {
        bail!(
            "{} has uncommitted changes; commit or remove them before squashing",
            worktree.display()
        );
    }
    let target_head = gitio::branch_head(&ctx.cwd, &st.target_branch)?;
    let head = gitio::branch_head(&ctx.cwd, &st.branch)?;
    let base = gitio::merge_base(&ctx.cwd, &target_head, &head)?;
    let range = format!("{base}..{head}");
    let count = gitio::git(&ctx.cwd, &["rev-list", "--count", &range])?
        .trim()
        .parse::<usize>()
        .context("git rev-list --count answered something other than a number")?;
    match count {
        0 => bail!("{} adds no commit to {}", st.branch, st.target_branch),
        1 => {
            println!("{}: already one commit on {}", change_id, st.target_branch);
            return Ok(());
        }
        _ => {}
    }
    let tree = tree_of(&ctx.cwd, &head)?;

    // The checkout moves with the branch, so the commit is made there, with
    // the repository's own signing and hook configuration.
    gitio::git(&worktree, &["reset", "--soft", &base])?;
    if let Err(error) = gitio::git(&worktree, &["commit", "-m", message]) {
        restore_tracked_state(&worktree, &head)?;
        return Err(error).context(
            "the single commit failed; restored the original head, index, and tracked files; untracked files are retained",
        );
    }
    let squashed = gitio::branch_head(&ctx.cwd, &st.branch)?;
    let squashed_tree = tree_of(&ctx.cwd, &squashed)?;
    let tracked_dirt = gitio::git(
        &worktree,
        &["status", "--porcelain", "--untracked-files=no"],
    )?;
    if squashed_tree != tree || !tracked_dirt.is_empty() {
        restore_tracked_state(&worktree, &head)?;
        bail!("the single commit or tracked files differ from {head}; restored the original head, index, and tracked files; untracked files are retained");
    }
    println!(
        "squashed: {count} commits since {} into {squashed} (the tree of {head})",
        st.target_branch
    );
    super::review::snapshot(ctx, &change_id, None, None, None, false, Vec::new(), None)
}

fn restore_tracked_state(worktree: &std::path::Path, head: &str) -> Result<()> {
    gitio::git(worktree, &["reset", "--mixed", head])?;
    gitio::git(
        worktree,
        &["restore", "--source", head, "--worktree", "--", "."],
    )?;
    Ok(())
}

fn tree_of(cwd: &std::path::Path, commit: &str) -> Result<String> {
    Ok(gitio::git(
        cwd,
        &["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
    )?
    .trim()
    .to_string())
}
