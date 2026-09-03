//! Forks: unintegrated work branches arc knows about but does not gate.
//!
//! A fork is a worktree on a `fork/<slug>` branch, deliberately outside the
//! change lifecycle: no ledger change is opened, no gates apply, nothing
//! merges. The point is a place to work that arc records without deciding —
//! the operator later chooses what to merge, rebase, or discard. A fork is a
//! fork until the operator says otherwise; nothing here auto-integrates,
//! auto-deletes, or merges.

use super::*;
mod identity;
use anyhow::{bail, Context, Result};
use identity::{canonical_path, ForkIdentity};
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

fn fork_branch(slug: &str) -> String {
    format!("{FORK_BRANCH_PREFIX}{slug}")
}

fn fork_topic(slug: &str) -> String {
    format!("{FORK_TOPIC_PREFIX}{slug}")
}

const FORK_CONTRACT: &str = "Fork contract: unintegrated by intent; external review deferred; \
     the operator decides what to merge, rebase, or discard.";

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
            // A fork marker is authored out of the fork it describes, not
            // distilled from a recorded session.
            source: crate::journal::SourceArgs {
                source: None,
                item_key: None,
            },
            // It is journaled from the repository it names, which is
            // reachable whenever the fork's worktree is.
            spool: false,
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
    let worktree_root = crate::config::load()?.worktrees_dir;
    let worktree_root = if worktree_root.is_absolute() {
        worktree_root
    } else {
        canonical_path(cwd).join(worktree_root)
    };
    let worktree = worktree_root.join(format!("{repo_name}-fork-{slug}"));
    if worktree.exists() {
        bail!("worktree path {} already exists", worktree.display());
    }
    if let Some(parent) = worktree.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    // A fork is the longest-lived checkout arc creates, so the space it will
    // occupy is reported before it exists rather than discovered later.
    crate::worktree_usage::report_root_free(&worktree_root);
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
    let Some(resolved) = identity::by_branch(&ctx.cwd, slug)? else {
        bail!("fork branch {branch} does not exist; nothing to adopt");
    };
    let Some(worktree) = resolved.worktree().map(Path::to_path_buf) else {
        bail!(
            "no live worktree has {branch} checked out with a reliable branch identity; \
             adopt a hand-made fork while its branch is attached so arc can record \
             the worktree path"
        );
    };
    if resolved.marker().is_none() {
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
    let Some(resolved) = identity::by_branch(&ctx.cwd, slug)? else {
        bail!("fork branch {branch} does not exist; nothing to retire");
    };
    let worktree = resolved.worktree().map(Path::to_path_buf);
    let marker = resolved.marker();
    let dir = crate::journal::resolve_dir(&ctx.cwd)?;
    // Retirement consumes the marker, so a consumed marker is a retired
    // fork whatever the body says — the body is never rewritten.
    if let Some(marker) = &marker {
        let events = crate::journal::read_events(&dir)?;
        if crate::journal::is_consumed(&events, marker.filename()) {
            if !keep_worktree {
                if let Some(worktree) = worktree.as_ref() {
                    crate::gitio::remove_worktree(&ctx.cwd, worktree, force)?;
                    println!("worktree removed: {}", worktree.display());
                }
            }
            bail!("fork {slug} is already retired; the record stands");
        }
    }
    if !keep_worktree {
        if let Some(worktree) = worktree.as_ref() {
            if force {
                // --force destroys work arc cannot see; the operator reads
                // what is being destroyed in the same breath as the
                // decision. A summary, not a refusal — the flag decided.
                let lost = uncommitted_summary(worktree);
                if !lost.is_empty() {
                    println!("discarding: {lost}");
                }
            }
            crate::gitio::remove_worktree(&ctx.cwd, worktree, force).with_context(|| {
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
            let written = identity::by_branch(&ctx.cwd, slug)?
                .expect("the fork just journaled for {slug} must resolve");
            let marker = written
                .marker()
                .expect("the marker just written for {slug} must resolve");
            crate::journal::consume(
                ctx,
                marker.filename(),
                crate::journal::ConsumeOutcome::Done,
                Some(&note),
                None,
                false,
                // A fork marker is retired by the fork that owns it. Nothing
                // acknowledges a claim here: a marker somebody has claimed is
                // refused, and released by its holder.
                &[],
            )?;
        }
        Some(marker) => {
            crate::journal::consume(
                ctx,
                marker.filename(),
                crate::journal::ConsumeOutcome::Done,
                Some(&note),
                None,
                false,
                // A fork marker is retired by the fork that owns it. Nothing
                // acknowledges a claim here: a marker somebody has claimed is
                // refused, and released by its holder.
                &[],
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

    // The resolver answers which forks exist and where their checkouts are,
    // once, for the whole repository. Listing used to ask that question a
    // second time here and then resolve each answer again per fork, which is
    // both where the listing defects appeared and why a cheap command cost
    // one resolution per fork plus one.
    let mut forks = identity::resolve_all(cwd)?;
    forks.sort_by(|a, b| a.slug().cmp(b.slug()));
    Ok(forks
        .iter()
        .map(|fork| describe(cwd, fork, &consumed))
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

/// The fork checkouts on disk, for the accounting that measures them. It
/// reads a listing rather than resolving again, so the repository answers the
/// identity question once per command.
pub fn checkouts(entries: &[ForkEntry]) -> Vec<crate::worktree_usage::ForkWorktree> {
    entries
        .iter()
        .filter_map(|entry| {
            entry
                .worktree
                .as_ref()
                .map(|path| crate::worktree_usage::ForkWorktree {
                    slug: entry.slug.clone(),
                    path: std::path::PathBuf::from(path),
                })
        })
        .collect()
}

/// The fork whose checkout the caller stands in, when it is one.
pub struct CurrentFork {
    pub slug: String,
    pub worktree: std::path::PathBuf,
}

/// Answer whether this checkout is a fork's, for the commands whose meaning
/// depends on it. The resolver owns the answer; this only carries it out of
/// the module in the shape a caller can use.
pub fn current(cwd: &Path) -> Result<Option<CurrentFork>> {
    Ok(identity::by_current_path(cwd)?.and_then(|fork| {
        fork.worktree().map(|worktree| CurrentFork {
            slug: fork.slug().to_string(),
            worktree: worktree.to_path_buf(),
        })
    }))
}

/// The identity that opened a fork, and how to reach the session that did.
///
/// A fork outlives the session that made it, and its marker records who was
/// working, through which harness, in which conversation. Reading that back
/// is the difference between a branch nobody can account for and one whose
/// reasoning is a command away. Nothing is inferred: a field the marker event
/// does not carry prints as absent rather than as a plausible guess, and a
/// harness with no stable resume form gets no invented incantation.
pub fn thread(ctx: &Ctx, slug: &str) -> Result<i32> {
    crate::ids::validate_slug(slug)?;
    let branch = fork_branch(slug);
    let Some(resolved) = identity::by_branch(&ctx.cwd, slug)? else {
        bail!("fork branch {branch} does not exist; `arc fork list` names the forks there are");
    };
    println!("fork: {slug}");
    println!("branch: {branch}");
    match resolved.worktree() {
        Some(worktree) => println!("worktree: {}", worktree.display()),
        None => println!("worktree: absent"),
    }

    let dir = crate::journal::resolve_dir(&ctx.cwd)?;
    let events = crate::journal::read_events(&dir)?;
    let recorded = resolved
        .marker()
        .and_then(|marker| crate::journal::recorded_identity(&events, marker.filename()));
    let Some(recorded) = recorded else {
        println!("identity: absent");
        println!("no journal marker records who opened this fork.");
        return Ok(0);
    };
    for (label, value) in [
        ("harness", &recorded.harness),
        ("session", &recorded.session),
        ("model", &recorded.model),
        ("actor", &recorded.actor),
    ] {
        println!("{label}: {}", value.as_deref().unwrap_or("absent"));
    }
    if let Some((harness, session)) = recorded.harness.as_deref().zip(recorded.session.as_deref()) {
        if let Some(resume) = resume_command(harness, session) {
            println!("resume: {resume}");
        }
    }
    Ok(0)
}

/// The command that reopens a session, for the harnesses whose resume form is
/// a stable part of their CLI. A harness without one is printed as identity
/// alone: a wrong incantation costs a reader more than an absent one.
fn resume_command(harness: &str, session: &str) -> Option<String> {
    match harness {
        "claude" => Some(format!("claude --resume {session}")),
        "codex" => Some(format!("codex resume {session}")),
        _ => None,
    }
}

/// Render one resolved fork. It takes an identity rather than a slug, so the
/// listing resolves the repository once instead of once per fork.
fn describe(cwd: &Path, fork: &ForkIdentity, consumed: &HashSet<&str>) -> ForkEntry {
    let marker = fork.marker();
    // A marker's base is the fork's own claim. An unmarked fork uses the
    // primary worktree's branch or origin/HEAD; if neither is safe to use,
    // retain an explicit unknown base instead of guessing a literal.
    let (intent, recorded_base) = marker
        .map(|marker| (marker.field("intent"), marker.field("base")))
        .unwrap_or((None, None));
    let base_branch = recorded_base.or_else(|| crate::gitio::default_branch(cwd));
    let retired = marker
        .filter(|marker| consumed.contains(marker.filename()))
        .map(|_| "retired".to_string());
    let worktree = fork.worktree().map(|path| path.display().to_string());
    let ahead = base_branch
        .as_deref()
        .and_then(|base| crate::gitio::ahead_count(cwd, base, fork.branch()).ok());
    let base_branch = base_branch.unwrap_or_else(|| "unknown".to_string());
    ForkEntry {
        slug: fork.slug().to_string(),
        branch: fork.branch().to_string(),
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
    if let Some(fork) = identity::by_current_path(cwd)? {
        bail!("{}", integrate_refusal(fork.slug()));
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
