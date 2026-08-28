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
use std::path::Path;

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
/// The worktree's own branch decides — not the directory name, which an
/// operator is free to choose differently.
pub fn fork_slug_at(cwd: &Path) -> Result<Option<String>> {
    let Some(branch) = crate::gitio::current_branch(cwd)? else {
        return Ok(None);
    };
    if !is_fork_branch(&branch) {
        return Ok(None);
    }
    Ok(Some(
        branch
            .strip_prefix(FORK_BRANCH_PREFIX)
            .expect("checked by is_fork_branch")
            .to_string(),
    ))
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

/// The fork marker a slug's journal holds, if any. A marker is `plan`-kind
/// under `fork-<slug>`: work-shaped enough to be visible in the queues a
/// session reads, retired by the same consume machinery every actionable
/// kind uses.
fn fork_marker(dir: &Path, slug: &str) -> Option<ForkMarker> {
    let topic = fork_topic(slug);
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            crate::journal::parse_artifact_name(&name)
                .filter(|(_, entry_topic, kind)| entry_topic.as_str() == topic && kind == "plan")
                .map(|_| name)
        })
        .collect();
    // Newest wins; an older marker for the same slug is a re-fork's history.
    names.sort();
    let filename = names.pop()?;
    let body = std::fs::read_to_string(dir.join(&filename)).unwrap_or_default();
    Some(ForkMarker { filename, body })
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
            "branch {branch} already exists; continue it with `arc fork {slug} --adopt` \
             or pick another slug"
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
    let Some(worktree) = crate::gitio::worktree_for_branch(&ctx.cwd, &branch)? else {
        bail!(
            "no worktree has {branch} checked out; a hand-made fork is adoptable \
             only while its worktree exists"
        );
    };
    let dir = crate::journal::resolve_dir(&ctx.cwd)?;
    if fork_marker(&dir, slug).is_none() {
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
/// operator's to keep or delete with Git. Retiring twice is refused — the
/// disposition is a decision, and one decision is what the record holds.
pub fn retire(ctx: &Ctx, slug: &str, outcome: &str, keep_worktree: bool) -> Result<i32> {
    crate::ids::validate_slug(slug)?;
    if outcome.trim().is_empty() {
        bail!("--retire needs a disposition: merged, dropped, or kept, with a word of why");
    }
    let branch = fork_branch(slug);
    let dir = crate::journal::resolve_dir(&ctx.cwd)?;
    let marker = fork_marker(&dir, slug);
    // Retirement consumes the marker, so a consumed marker is a retired
    // fork whatever the body says — the body is never rewritten.
    if let Some(marker) = &marker {
        let events = crate::journal::read_events(&dir)?;
        if crate::journal::is_consumed(&events, &marker.filename) {
            bail!("fork {slug} is already retired; the record stands");
        }
    }
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
            let note = format!("fork {slug} retired: {outcome}");
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
            let note = format!("fork {slug} retired: {outcome}");
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
    if !keep_worktree {
        if let Some(worktree) = crate::gitio::worktree_for_branch(&ctx.cwd, &branch)? {
            crate::gitio::remove_worktree(&ctx.cwd, &worktree)?;
            println!("worktree removed: {}", worktree.display());
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

    let mut slugs: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some((_, topic, kind)) = crate::journal::parse_artifact_name(&name) {
            if kind == "plan"
                && topic.starts_with(FORK_TOPIC_PREFIX)
                && topic.len() > FORK_TOPIC_PREFIX.len()
            {
                slugs.push(topic[FORK_TOPIC_PREFIX.len()..].to_string());
            }
        }
    }
    // Branches with no marker: hand-made forks arc did not create.
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
        let state = match (&entry.worktree, &entry.retired) {
            (Some(path), _) => format!("{} ({path})", entry.branch),
            (None, Some(_)) => format!("{} (retired, no worktree)", entry.branch),
            (None, None) => format!("{} (no worktree)", entry.branch),
        };
        println!(
            "  {}  {}  +{} over {}",
            entry.slug, state, entry.ahead, entry.base_branch
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
    pub ahead: usize,
    pub base_branch: String,
}

fn describe(cwd: &Path, dir: &Path, slug: &str, consumed: &HashSet<&str>) -> ForkEntry {
    let branch = fork_branch(slug);
    let marker = fork_marker(dir, slug);
    let (intent, base_branch) = marker
        .as_ref()
        .map(|marker| {
            (
                marker_field(&marker.body, "intent"),
                marker_field(&marker.body, "base").unwrap_or_else(default_branch),
            )
        })
        .unwrap_or_else(|| (None, default_branch()));
    let retired = marker
        .as_ref()
        .filter(|marker| consumed.contains(marker.filename.as_str()))
        .map(|_| "retired".to_string());
    let worktree = crate::gitio::worktree_for_branch(cwd, &branch)
        .ok()
        .flatten()
        .map(|path| path.display().to_string());
    let ahead = crate::gitio::ahead_count(cwd, &base_branch, &branch).unwrap_or(0);
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

/// The branch an unmarked fork is measured against. The repository's HEAD
/// branch when discoverable, else the convention this repository was built
/// on — a guess, and the `ahead` count that rides on it is advice, not fact.
fn default_branch() -> String {
    "master".to_string()
}

/// The refusal `integrate` prints inside a fork worktree. It names the way
/// out rather than only the wall: a fork merges when its operator moves the
/// work, and the disposition is recorded, not gated.
pub fn integrate_refusal(slug: &str) -> String {
    format!(
        "this is fork worktree {slug}: unintegrated by intent, so arc does not \
         gate or merge it. Move the work onto a change from the base branch \
         (or `git merge` from the target) when it is ready, and record the \
         disposition with `arc fork {slug} --retire <outcome>`."
    )
}
