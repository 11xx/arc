//! Forks: unintegrated work branches arc knows about but does not gate.
//!
//! A fork is a worktree on a `fork/<slug>` branch, deliberately outside the
//! change lifecycle: no ledger change is opened, no gates apply, nothing
//! merges. The point is a place to work that arc records without deciding —
//! the operator later chooses what to merge, rebase, or discard. A fork is a
//! fork until the operator says otherwise; nothing here auto-integrates,
//! auto-deletes, or merges.

use super::*;
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The branch prefix every fork carries. The prefix is the boundary: a
/// branch outside it is not a fork, and `integrate` inside a fork worktree
/// refuses before it reaches any gate.
pub const FORK_BRANCH_PREFIX: &str = "fork/";

/// The journal topic prefix every fork marker is filed under, so `catchup`
/// and the fork views list them without parsing branch names.
const FORK_TOPIC_PREFIX: &str = "fork-";

/// Whether a branch name names a fork. `fork/` itself is not a branch.
pub fn is_fork_branch(branch: &str) -> bool {
    branch
        .strip_prefix(FORK_BRANCH_PREFIX)
        .is_some_and(|slug| !slug.is_empty())
}

/// The slug of the fork whose worktree `cwd` stands in, when it is one.
///
/// The fork branch is the defining fact. An attached worktree exposes it
/// directly; a detached worktree is identified only by the marker that binds
/// that branch to its exact worktree path. Directory names and HEAD commits
/// are not fork identity.
pub fn fork_slug_at(cwd: &Path) -> Result<Option<String>> {
    // Attached: the branch symbol answers directly.
    if let Some(branch) = crate::gitio::current_branch(cwd)? {
        return Ok(fork_slug_from_branch(&branch));
    }
    // Detached: Git has removed the branch symbol, so use the durable
    // branch-to-path binding in the marker.
    fork_slug_when_detached(cwd)
}

fn fork_slug_from_branch(branch: &str) -> Option<String> {
    if !is_fork_branch(branch) {
        return None;
    }
    Some(
        branch
            .strip_prefix(FORK_BRANCH_PREFIX)
            .expect("checked by is_fork_branch")
            .to_string(),
    )
}

/// Resolve a detached checkout from an existing branch-to-path marker.
fn fork_slug_when_detached(cwd: &Path) -> Result<Option<String>> {
    let root = crate::gitio::toplevel(cwd)?;
    let dir = crate::journal::resolve_dir(cwd)?;
    let root = canonical_path(&root);
    for marker in fork_markers(&dir) {
        let Some(branch) = marker_branch(&marker) else {
            continue;
        };
        if crate::gitio::branch_exists(cwd, &branch)
            && marker_worktree_path(&root, &marker).is_some_and(|path| path == root)
        {
            return Ok(fork_slug_from_branch(&branch));
        }
    }
    Ok(None)
}

/// Every fork marker in the journal, newest first: the path match wants the
/// newest claim about a worktree that may have been re-forked.
fn fork_markers(dir: &Path) -> Vec<ForkMarker> {
    let mut markers: Vec<ForkMarker> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            crate::journal::parse_artifact_name(&name)
                .filter(|(_, topic, kind)| kind == "plan" && topic.starts_with(FORK_TOPIC_PREFIX))
                .map(|_| ForkMarker {
                    filename: name.clone(),
                    body: std::fs::read_to_string(dir.join(&name)).unwrap_or_default(),
                })
        })
        .collect();
    markers.sort_by(|a, b| b.filename.cmp(&a.filename));
    markers
}

fn fork_branch(slug: &str) -> String {
    format!("{FORK_BRANCH_PREFIX}{slug}")
}

fn fork_topic(slug: &str) -> String {
    format!("{FORK_TOPIC_PREFIX}{slug}")
}

const FORK_CONTRACT: &str = "Fork contract: unintegrated by intent; external review deferred; \
     the operator decides what to merge, rebase, or discard.";

/// One fork marker: the newest `plan` artifact under `fork-<slug>`.
struct ForkMarker {
    filename: String,
    body: String,
}

fn marker_branch(marker: &ForkMarker) -> Option<String> {
    let branch = marker_field(&marker.body, "branch")?;
    is_fork_branch(&branch).then_some(branch)
}

fn canonical_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn marker_worktree_path(root: &Path, marker: &ForkMarker) -> Option<PathBuf> {
    let recorded = PathBuf::from(marker_field(&marker.body, "worktree")?);
    let path = if recorded.is_absolute() {
        recorded
    } else {
        root.join(recorded)
    };
    Some(canonical_path(&path))
}

/// Resolve the worktree of a fork from its branch association or its marker's
/// exact path. Detached worktrees have no branch association, and their HEAD
/// is not an identity because another worktree can have the same commit.
fn fork_worktree(cwd: &Path, slug: &str, marker: Option<&ForkMarker>) -> Result<Option<PathBuf>> {
    let branch = fork_branch(slug);
    let inventory = crate::gitio::worktree_inventory(cwd)?;
    if let Some(entry) = inventory
        .iter()
        .find(|entry| entry.branch.as_deref() == Some(branch.as_str()))
    {
        return Ok(Some(entry.path.clone()));
    }

    let Some(marker) =
        marker.filter(|marker| marker_branch(marker).as_deref() == Some(branch.as_str()))
    else {
        return Ok(None);
    };
    let root = crate::gitio::toplevel(cwd)?;
    let Some(recorded) = marker_worktree_path(&root, marker) else {
        return Ok(None);
    };
    Ok(inventory
        .into_iter()
        .find(|entry| entry.branch.is_none() && canonical_path(&entry.path) == recorded)
        .map(|entry| entry.path))
}

/// The fork marker a branch's journal holds, if any. A marker is a `plan`
/// artifact whose body records the exact fork branch and worktree path; its
/// topic keeps it visible in the journal queues and the consume machinery
/// retires it with the other actionable kinds.
fn fork_marker(dir: &Path, slug: &str) -> Option<ForkMarker> {
    let branch = fork_branch(slug);
    fork_markers(dir)
        .into_iter()
        .find(|marker| marker_branch(marker).as_deref() == Some(branch.as_str()))
}

/// One line from a marker body, as `key: value`.
fn marker_field(body: &str, key: &str) -> Option<String> {
    body.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}: ")))
        .map(str::to_string)
}

fn journal_marker(ctx: &Ctx, slug: &str, title: &str, body: &str) -> Result<()> {
    // `KindWrite.body_file` takes a path or stdin, not inline text, so the
    // body rides through a sibling temp file: same filesystem as the
    // journal, cleaned up on every path out of the scope.
    let dir = crate::journal::resolve_dir(&ctx.cwd)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create journal dir {}", dir.display()))?;
    let temp = tempfile_name(&dir);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("cannot stage {}", temp.display()))?;
        f.write_all(body.as_bytes())
            .with_context(|| format!("cannot write {}", temp.display()))?;
    }
    let written = crate::journal::write_kind(
        ctx,
        crate::journal::JournalKind::Plan,
        crate::journal::KindWrite {
            topic: fork_topic(slug),
            body_file: Some(temp.display().to_string()),
            title: Some(title.to_string()),
            scaffold: None,
            no_scaffold: true,
        },
    );
    let _ = std::fs::remove_file(&temp);
    if written? != 0 {
        bail!("fork marker could not be journaled");
    }
    Ok(())
}

fn tempfile_name(dir: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!(
        ".fork-marker-{}-{}.tmp",
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

pub fn begin(ctx: &Ctx, slug: &str, base_branch: Option<&str>) -> Result<i32> {
    crate::ids::validate_slug(slug)?;
    let cwd = &ctx.cwd;
    let base = match base_branch {
        Some(branch) => branch.to_string(),
        None => crate::gitio::current_branch(cwd)?
            .filter(|branch| !is_fork_branch(branch))
            .unwrap_or_else(|| "master".to_string()),
    };
    if is_fork_branch(&base) {
        bail!("base branch {base:?} is itself a fork; fork from an integrated branch");
    }
    let branch = fork_branch(slug);
    if crate::gitio::branch_exists(cwd, &branch) {
        bail!(
            "branch {branch} already exists; continue it with \
             `arc fork adopt {slug}` or pick another slug"
        );
    }

    let toplevel = crate::gitio::toplevel(cwd)?;
    let repo_name = toplevel
        .file_name()
        .context("cannot determine repository name")?
        .to_string_lossy()
        .into_owned();
    let worktree = crate::config::load()?
        .worktrees_dir
        .join(format!("{repo_name}-fork-{slug}"));
    if worktree.exists() {
        bail!("worktree path {} already exists", worktree.display());
    }
    if let Some(parent) = worktree.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    // The branch is created by the worktree add itself: one command, no
    // window where a branch exists with no checkout.
    crate::gitio::add_worktree_new_branch(cwd, &worktree, &branch, &base)?;

    // The marker is a journal artifact, not a ledger event: a fork is intent,
    // not gated work, and the ledger records only what the gates decided.
    journal_marker(
        ctx,
        slug,
        &format!("fork {slug}"),
        &format!(
            "branch: {branch}\nworktree: {}\nbase: {base}\nstatus: open\n\n\
             {FORK_CONTRACT}\n",
            worktree.display(),
        ),
    )?;
    println!("fork: {slug}");
    println!("branch: {branch}");
    println!("base: {base}");
    println!("worktree: {}", worktree.display());
    println!("cd {}", worktree.display());
    println!();
    println!("{FORK_CONTRACT} `arc integrate` refuses inside a fork worktree.");
    Ok(0)
}

pub fn adopt(ctx: &Ctx, slug: &str, intent: Option<&str>) -> Result<i32> {
    crate::ids::validate_slug(slug)?;
    let branch = fork_branch(slug);
    let dir = crate::journal::resolve_dir(&ctx.cwd)?;
    let marker = fork_marker(&dir, slug);
    let Some(worktree) = fork_worktree(&ctx.cwd, slug, marker.as_ref())? else {
        bail!(
            "no worktree has {branch} checked out with a reliable branch identity; \
             adopt a hand-made fork while its branch is attached so arc can record \
             the worktree path"
        );
    };
    if marker.is_none() {
        journal_marker(
            ctx,
            slug,
            &format!("fork {slug} (adopted)"),
            &format!(
                "branch: {branch}\nworktree: {}\nstatus: adopted\n{}\n\n\
                 {FORK_CONTRACT}\n",
                worktree.display(),
                intent
                    .map(|text| format!("intent: {text}\n"))
                    .unwrap_or_default(),
            ),
        )?;
        println!("adopted: {slug} at {}", worktree.display());
    } else {
        println!("already journaled: {slug} at {}", worktree.display());
    }
    println!("{FORK_CONTRACT}");
    Ok(0)
}

/// Record the fork's disposition and remove the worktree. The branch is kept:
/// a merge or a discard happened somewhere else, and the commits are the
/// operator's to keep or delete with Git.
///
/// The worktree is removed before the marker is consumed. Retirement is a
/// claim about the fork; the disk has to have acted before the record says
/// so, or a failed removal leaves a record that says retired above a
/// worktree still on disk — invisible in every surface that filters retired
/// forks, which is exactly where worktree cost goes to hide. A removal that
/// fails (untracked files are Git's own refusal) leaves nothing recorded,
/// so the retry is ordinary rather than a workaround; `--force` is the
/// operator's deliberate discard of work arc cannot see, never arc's.
///
/// A retire that ran with `--keep-worktree` can be finished later: the
/// record already stands, and removing the leftover worktree is not a
/// second decision.
pub fn retire(
    ctx: &Ctx,
    slug: &str,
    outcome: &str,
    keep_worktree: bool,
    force: bool,
) -> Result<i32> {
    crate::ids::validate_slug(slug)?;
    if outcome.trim().is_empty() {
        bail!("retire needs a disposition: merged, dropped, or kept, with a word of why");
    }
    let branch = fork_branch(slug);
    if !crate::gitio::branch_exists(&ctx.cwd, &branch) {
        bail!("fork branch {branch} does not exist; nothing to retire");
    }
    let dir = crate::journal::resolve_dir(&ctx.cwd)?;
    let marker = fork_marker(&dir, slug);
    // Retirement consumes the marker, so a consumed marker is a retired
    // fork whatever the body says — the body is never rewritten.
    if let Some(marker) = &marker {
        let events = crate::journal::read_events(&dir)?;
        if crate::journal::is_consumed(&events, &marker.filename) {
            if !keep_worktree {
                if let Some(worktree) = fork_worktree(&ctx.cwd, slug, Some(marker))? {
                    crate::gitio::remove_worktree(&ctx.cwd, &worktree, force)?;
                    println!("worktree removed: {}", worktree.display());
                }
            }
            bail!("fork {slug} is already retired; the record stands");
        }
    }
    if !keep_worktree {
        if let Some(worktree) = fork_worktree(&ctx.cwd, slug, marker.as_ref())? {
            if force {
                // --force destroys work arc cannot see; the operator reads
                // what is being destroyed in the same breath as the
                // decision. A summary, not a refusal — the flag decided.
                let lost = uncommitted_summary(&worktree);
                if !lost.is_empty() {
                    println!("discarding: {lost}");
                }
            }
            crate::gitio::remove_worktree(&ctx.cwd, &worktree, force).with_context(|| {
                format!(
                    "cannot remove {}; it holds work arc cannot see — move or delete it, \
                     or pass --force to discard it",
                    worktree.display()
                )
            })?;
            println!("worktree removed: {}", worktree.display());
        }
    }
    let note = format!("fork {slug} retired: {outcome}");
    match marker {
        None => {
            // Retiring an unjournaled fork is legitimate: a hand-made fork
            // wound down straight to a record. The marker is written and
            // consumed in the same breath — a retirement must not leave the
            // open queue claiming a retired fork is live work.
            journal_marker(
                ctx,
                slug,
                &format!("fork {slug} (retired)"),
                &format!(
                    "branch: {branch}\nstatus: retired\noutcome: {outcome}\n\n\
                     Retired before it was ever journaled.\n"
                ),
            )?;
            let marker =
                fork_marker(&dir, slug).expect("the marker just written for {slug} must resolve");
            crate::journal::consume(
                ctx,
                &marker.filename,
                crate::journal::ConsumeOutcome::Done,
                Some(&note),
                None,
                false,
            )?;
        }
        Some(marker) => {
            crate::journal::consume(
                ctx,
                &marker.filename,
                crate::journal::ConsumeOutcome::Done,
                Some(&note),
                None,
                false,
            )?;
        }
    }
    println!("retired: {slug} [{outcome}]");
    println!("branch kept: {branch}");
    Ok(0)
}

/// Every fork this repository knows about: a marker in the journal, a
/// `fork/*` branch, or both — a fork the operator made by hand is not
/// invisible.
pub fn list_entries(ctx: &Ctx) -> Result<Vec<ForkEntry>> {
    let cwd = &ctx.cwd;
    let dir = crate::journal::resolve_dir(cwd)?;

    let events = crate::journal::read_events(&dir)?;
    let consumed: HashSet<&str> = events
        .iter()
        .filter(|event| event.event == "consumed")
        .filter_map(|event| event.file.as_deref())
        .collect();

    // A fork/<slug> branch is the fact; a fork-<slug> marker is a claim
    // about one. Any journal plan under a fork-* topic would otherwise be
    // reported as a fork with an invented branch — three fabrications in
    // one line of output — so the branch list is the primary source and a
    // marker only annotates a branch that exists.
    let mut slugs: Vec<String> = Vec::new();
    let out = crate::gitio::git(
        cwd,
        &["branch", "--list", "--format=%(refname:short)", "fork/*"],
    )?;
    for branch in out.lines().filter(|line| is_fork_branch(line)) {
        slugs.push(branch[FORK_BRANCH_PREFIX.len()..].to_string());
    }
    slugs.sort();
    slugs.dedup();

    Ok(slugs
        .iter()
        .map(|slug| describe(cwd, &dir, slug, &consumed))
        .collect())
}

pub fn list(ctx: &Ctx, json: bool) -> Result<i32> {
    let entries = list_entries(ctx)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "arc-forks/1",
                "forks": entries,
            }))?
        );
        return Ok(0);
    }
    if entries.is_empty() {
        println!("no forks");
        return Ok(0);
    }
    println!("forks ({}):", entries.len());
    for entry in &entries {
        // The same fact --json carries: retirement is visible even when a
        // worktree survives it, or text and JSON would disagree about one
        // fork — and the surviving worktree is exactly the half-state an
        // operator needs to know is still on disk.
        let state = match (&entry.worktree, &entry.retired) {
            (Some(path), Some(_)) => {
                format!("{} (retired, worktree remains: {path})", entry.branch)
            }
            (Some(path), None) => format!("{} ({path})", entry.branch),
            (None, Some(_)) => format!("{} (retired, no worktree)", entry.branch),
            (None, None) => format!("{} (no worktree)", entry.branch),
        };
        let ahead = entry
            .ahead
            .map(|count| format!("+{count}"))
            .unwrap_or_else(|| "+?".to_string());
        println!(
            "  {}  {}  {} over {}",
            entry.slug, state, ahead, entry.base_branch
        );
        if let Some(intent) = &entry.intent {
            println!("    {intent}");
        }
    }
    Ok(0)
}

#[derive(Debug, serde::Serialize)]
pub struct ForkEntry {
    pub slug: String,
    pub branch: String,
    /// The fork worktree's path, when it is still checked out. A fork whose
    /// worktree was removed by hand still has its branch and marker; this is
    /// `None` rather than an invented path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// Free text from when the fork was created or adopted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// Present once the fork's disposition has been recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retired: Option<String>,
    /// Commits the fork branch carries that its base branch does not.
    /// `None` when the count cannot be computed — a missing base branch,
    /// a failed rev-list, or no safely discoverable base — because an
    /// uncomputable number is not zero, and a zero a reader would sum is a
    /// lie about work that may exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<usize>,
    /// The recorded or discovered integration branch. `"unknown"` means no
    /// marker or safe repository-level discovery supplied a branch.
    pub base_branch: String,
}

fn describe(cwd: &Path, dir: &Path, slug: &str, consumed: &HashSet<&str>) -> ForkEntry {
    let branch = fork_branch(slug);
    let marker = fork_marker(dir, slug);
    // A marker's base is the fork's own claim. An unmarked fork uses the
    // primary worktree's branch or origin/HEAD; if neither is safe to use,
    // retain an explicit unknown base instead of guessing a literal.
    let (intent, recorded_base) = marker
        .as_ref()
        .map(|marker| {
            (
                marker_field(&marker.body, "intent"),
                marker_field(&marker.body, "base"),
            )
        })
        .unwrap_or_else(|| (None, None));
    let base_branch = recorded_base.or_else(|| crate::gitio::default_branch(cwd));
    let retired = marker
        .as_ref()
        .filter(|marker| consumed.contains(marker.filename.as_str()))
        .map(|_| "retired".to_string());
    let worktree = fork_worktree(cwd, slug, marker.as_ref())
        .ok()
        .flatten()
        .map(|path| path.display().to_string());
    let ahead = base_branch
        .as_deref()
        .and_then(|base| crate::gitio::ahead_count(cwd, base, &branch).ok());
    let base_branch = base_branch.unwrap_or_else(|| "unknown".to_string());
    ForkEntry {
        slug: slug.to_string(),
        branch,
        worktree,
        intent,
        retired,
        ahead,
        base_branch,
    }
}

/// What a forced removal will destroy, as one line: untracked files, and
/// tracked files carrying uncommitted modifications. A summary a reader can
/// act on before the removal, never a refusal — `--force` already decided.
fn uncommitted_summary(worktree: &Path) -> String {
    let status = crate::gitio::git(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .unwrap_or_default();
    let mut untracked = 0usize;
    let mut modified = 0usize;
    for line in status.lines() {
        let state = &line[..2.min(line.len())];
        if state.starts_with('?') {
            untracked += 1;
        } else if !state.trim().is_empty() {
            modified += 1;
        }
    }
    match (untracked, modified) {
        (0, 0) => String::new(),
        (u, 0) => format!("{u} untracked file(s)"),
        (0, m) => format!("{m} file(s) with uncommitted changes"),
        (u, m) => format!("{u} untracked file(s), {m} file(s) with uncommitted changes"),
    }
}

/// The refusal `integrate` prints inside a fork worktree. It names the way
/// out rather than only the wall: a fork merges when its operator moves the
/// work, and the disposition is recorded, not gated.
pub fn ensure_not_fork(cwd: &Path) -> Result<()> {
    if let Some(slug) = fork_slug_at(cwd)? {
        bail!("{}", integrate_refusal(&slug));
    }
    Ok(())
}

pub fn integrate_refusal(slug: &str) -> String {
    format!(
        "this is fork worktree {slug}: unintegrated by intent, so arc does not \
         gate or merge it. Move the work onto a change from the base branch \
         (or `git merge` from the target) when it is ready, and record the \
         disposition with `arc fork retire {slug} <outcome>`."
    )
}
