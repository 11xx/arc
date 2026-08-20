//! Mechanics for the cross-harness project journal.
//!
//! The ledger is authoritative and gating; the journal is advisory and
//! contextual. Artifacts stay freeform Markdown and remain readable and
//! writable by a tool-less agent; the append-only event log is versioned
//! JSONL (`journal-events/1` in `events.jsonl`). `arc journal` only encodes
//! the invariants that drift in practice — directory resolution, timestamped
//! filenames, position identity and lifecycle, and journal event semantics.
//! It is a convenience and
//! correctness layer, never a gatekeeper, and it is intentionally decoupled
//! from the change ledger.
//!
//! Lanes are advisory occupancy announced through journal events. Their
//! liveness follows the owner's latest journal activity; they are never locks.

use crate::commands::Ctx;
use crate::config;
use crate::gitio;
use crate::state::{self, ChangeState};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const JOURNAL_LOCK_TIMEOUT: Duration = Duration::from_secs(1);
const JOURNAL_LOCK_RETRY: Duration = Duration::from_millis(10);

/// Serialize journal transitions whose preflight depends on current state.
/// The lock file persists, while the OS releases ownership with this handle,
/// so a crashed writer cannot strand the journal behind a stale marker.
struct JournalTransitionLock(File);

impl Drop for JournalTransitionLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn lock_journal_transition(dir: &Path) -> Result<JournalTransitionLock> {
    let lock_dir = dir.join(".locks");
    std::fs::create_dir_all(&lock_dir)
        .with_context(|| format!("cannot create journal lock dir {}", lock_dir.display()))?;
    let path = lock_dir.join("transition.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open journal transition lock {}", path.display()))?;
    let deadline = Instant::now() + JOURNAL_LOCK_TIMEOUT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(JournalTransitionLock(file)),
            Err(std::fs::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    bail!(
                        "journal transition lock {} is busy; retry the command",
                        path.display()
                    );
                }
                thread::sleep(JOURNAL_LOCK_RETRY);
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error)
                    .with_context(|| format!("cannot lock journal transition {}", path.display()));
            }
        }
    }
}

/// Closed set of artifact kinds. Malformed kinds are rejected by clap at
/// parse time, before anything is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum JournalKind {
    Note,
    Memory,
    Plan,
    Handoff,
    Review,
    Conclusion,
    Todo,
    Later,
    Discussion,
    Decision,
    FeatureRequest,
}

impl JournalKind {
    fn as_str(self) -> &'static str {
        match self {
            JournalKind::Note => "note",
            JournalKind::Memory => "memory",
            JournalKind::Plan => "plan",
            JournalKind::Handoff => "handoff",
            JournalKind::Review => "review",
            JournalKind::Conclusion => "conclusion",
            JournalKind::Todo => "todo",
            JournalKind::Later => "later",
            JournalKind::Discussion => "discussion",
            JournalKind::Decision => "decision",
            JournalKind::FeatureRequest => "feature-request",
        }
    }
}

// Retirement is permanent: these compatibility tombstones never return to
// the active set. Removing one would make historical artifacts unknown again;
// new semantics get a new name.
const RETIRED_JOURNAL_KINDS: [&str; 3] = ["done", "inbox", "spec"];

fn recognized_journal_kinds() -> impl Iterator<Item = &'static str> {
    JournalKind::value_variants()
        .iter()
        .map(|kind| kind.as_str())
        .chain(RETIRED_JOURNAL_KINDS)
}

fn parse_journal_kind_filter(value: &str) -> Result<String, String> {
    if known_kind(value) {
        return Ok(value.to_string());
    }
    Err(format!(
        "unknown journal kind {value:?}; accepted values: {}",
        recognized_journal_kinds().collect::<Vec<_>>().join(", ")
    ))
}

/// Primary kinds that represent work waiting for a future session: they stay
/// in the main `journal open` queue until an explicit `journal consume`.
/// `discussion` is the actionable answer-owed kind: an open debate rides the
/// queue until someone resolves it.
const PRIMARY_ACTIONABLE_KINDS: [&str; 5] = ["todo", "handoff", "inbox", "plan", "discussion"];
const LATER_KIND: &str = "later";
const FEATURE_REQUEST_KIND: &str = "feature-request";

fn is_actionable_kind(kind: &str) -> bool {
    PRIMARY_ACTIONABLE_KINDS.contains(&kind) || kind == LATER_KIND || kind == FEATURE_REQUEST_KIND
}

/// How a consumed artifact was discharged. Advisory vocabulary recorded in
/// the journal line; `done` covers the normal picked-up-and-finished path.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ConsumeOutcome {
    Done,
    Superseded,
    Discarded,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum LaneOutcome {
    Done,
    Handoff,
    Abandoned,
    Expired,
}

impl LaneOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Handoff => "handoff",
            Self::Abandoned => "abandoned",
            Self::Expired => "expired",
        }
    }
}

#[derive(Subcommand)]
pub enum LaneCmd {
    /// Open or replace an advisory work lane
    Open {
        topic: String,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long, default_value = "2h")]
        ttl: String,
        #[arg(long)]
        status: Option<String>,
        /// Read the lane status from a file ('-' for stdin)
        #[arg(long, conflicts_with = "status")]
        status_file: Option<String>,
    },
    /// Renew a lane owned by this session
    Renew {
        topic: String,
        #[arg(long)]
        ttl: Option<String>,
        #[arg(long)]
        status: Option<String>,
        /// Read the lane status from a file ('-' for stdin)
        #[arg(long, conflicts_with = "status")]
        status_file: Option<String>,
    },
    /// Close a lane
    Close {
        topic: String,
        #[arg(long, value_enum, default_value = "done")]
        outcome: LaneOutcome,
        #[arg(long)]
        note: Option<String>,
    },
    /// List current live and stale lanes
    List {
        #[arg(long)]
        json: bool,
    },
}

impl ConsumeOutcome {
    fn as_str(self) -> &'static str {
        match self {
            ConsumeOutcome::Done => "done",
            ConsumeOutcome::Superseded => "superseded",
            ConsumeOutcome::Discarded => "discarded",
        }
    }
}

#[derive(Subcommand)]
pub enum JournalCmd {
    /// Print the resolved journal directory (creates nothing)
    Dir {
        /// Print the cold sibling archive directory
        #[arg(long)]
        archive: bool,
        /// Explain the resolution source and stable anchor
        #[arg(long)]
        explain: bool,
    },
    /// Check the journal for malformed or stale state (read-only)
    Doctor {
        /// Emit structured JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Write a timestamped artifact and append its journal line
    Note {
        /// Kebab-case topic slug
        topic: String,
        /// Artifact kind (closed set). `discussion` argues a proposal to a
        /// decision and carries positions; `feature-request` is an unbuilt
        /// proposal parked for later
        #[arg(long, value_enum)]
        kind: JournalKind,
        /// Body source: a file path, or '-' for stdin (written verbatim)
        #[arg(long)]
        body_file: Option<String>,
        /// Optional title; when set, a `# <title>` heading is prepended
        #[arg(long)]
        title: Option<String>,
        /// Scaffold template prepended to the body (.arc/templates/<name>.md or
        /// a built-in: sol-low, sol-high, reviewer, discussion). `--kind
        /// discussion` uses the `discussion` scaffold unless told otherwise
        #[arg(long, conflicts_with = "no_scaffold")]
        scaffold: Option<String>,
        /// Record the body alone, without the kind's default scaffold
        #[arg(long)]
        no_scaffold: bool,
    },
    /// Append a log-only journal line (no artifact file is created)
    Log {
        /// Kebab-case topic slug
        topic: String,
        /// Free-text journal message
        message: String,
    },
    /// Add a position block to an artifact and emit a typed `position` event.
    /// The body's first line states the stance the tally counts:
    /// `Position: for | against | amend`
    /// Pass `--question <id> --option <opt>` to argue under one branch of an
    /// open question instead of unconditionally
    Position {
        /// Artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// Position or item this answers: a position ID, legacy timestamp, or
        /// item slug. Quote the claim answered on the line below the stance
        #[arg(long = "ref")]
        reference: Option<String>,
        /// Body source: a file path, or '-' for stdin (the position argument,
        /// written verbatim below a tool-computed `### Position` heading).
        /// Its first line states the stance: `Position: for | against | amend`
        #[arg(long)]
        body_file: String,
        /// Argue under one option of an open question, rather than
        /// unconditionally. Pass the question ID; `--option` names the branch
        #[arg(long)]
        question: Option<String>,
        /// The option this position argues under; requires `--question`
        #[arg(long)]
        option: Option<String>,
    },
    /// Pose a question on a discussion that only a person can settle, and emit
    /// a typed `question` event. Placement is the design: `opening` is answered
    /// before any position is filed, so everyone argues from the same premise;
    /// `closing` is answered once the argument is in. There is no mid-argument
    /// placement — a question that blocks halfway makes the caller watch a run
    /// they delegated. Argue a closing question on both sides first, with
    /// `position --question <id> --option <opt>`
    Question {
        /// Artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// When it is answered: `opening` settles a premise before any
        /// position is filed; `closing` settles a choice the argument raised.
        /// There is deliberately no mid-argument placement
        #[arg(long, value_parser = ["opening", "closing"])]
        placement: String,
        /// An answer the question offers; repeat for each. Two or more
        #[arg(long = "option", required = true)]
        options: Vec<String>,
        /// Body source: a file path, or '-' for stdin (what the question asks,
        /// written verbatim below a tool-computed `### Question` heading)
        #[arg(long)]
        body_file: String,
    },
    /// Settle an open question by choosing one of its options, once. Branches
    /// that lost stay in the file: a branch argued and not taken is the only
    /// record that the alternative was explored rather than never considered
    Answer {
        /// Artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// The question being settled
        #[arg(long)]
        question: String,
        /// The option chosen; must be one the question offered
        #[arg(long)]
        option: String,
        /// Body source: a file path, or '-' for stdin (why, written verbatim
        /// below a tool-computed `### Answer` heading)
        #[arg(long)]
        body_file: String,
    },
    /// Dump the journal event log as newline-delimited JSON
    Events {
        /// Cap the number of events (oldest first)
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Newest-first listing of artifacts plus the journal tail (read-only)
    Catchup {
        /// Cap the artifact list and journal tail (default 20)
        #[arg(long)]
        limit: Option<usize>,
        /// Emit structured JSON instead of text
        #[arg(long)]
        json: bool,
        /// List cold archived artifacts (the journal tail remains hot)
        #[arg(long)]
        archived: bool,
    },
    /// List live shared project memories newest first
    Memories {
        /// Emit structured JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// List actionable artifacts in primary, later, and feature-request tiers
    Open {
        /// Restrict to one actionable kind; lower-priority tiers stay separate
        #[arg(long, value_parser = parse_journal_kind_filter)]
        kind: Option<String>,
        /// Emit structured JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// List live artifacts (hot journal dir) newest first, optionally filtered by kind (read-only)
    List {
        /// Restrict to one kind
        #[arg(long, value_parser = parse_journal_kind_filter)]
        kind: Option<String>,
        /// Emit structured JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Print one artifact's raw Markdown body to stdout (read-only)
    Show {
        /// Artifact filename inside the journal dir (a name, not a path)
        filename: String,
    },
    /// Derived summary of a discussion: stance tally, participants, replies,
    /// age, and resolution with a resolver-participation flag (read-only).
    /// The tally counts `Position: for | against | amend` lines inside
    /// `### Position` blocks, and flags blocks that state no stance. Reports
    /// each open question with the positions argued under every option, so a
    /// branch nobody explored is visible before the question is answered.
    /// Resolve a discussion with `journal consume --outcome done --decision
    /// <file>`
    Discussion {
        /// Discussion artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// Emit structured JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Adopt an existing journal for this project, recording the move.
    /// Refuses when both journals hold content: two histories are separable
    /// only while they are apart
    Rebind {
        /// The journal directory to adopt, as `journal doctor` names it
        from: String,
    },
    /// Print the current UTC timestamp in the journal house format (read-only)
    Stamp,
    /// Manage advisory session work lanes
    Lane {
        #[command(subcommand)]
        command: LaneCmd,
    },
    /// Mark an artifact consumed so it leaves the `open` queue
    Consume {
        /// Artifact filename inside the archive dir (a name, not a path)
        filename: String,
        /// How it was discharged
        #[arg(long, value_enum, default_value = "done")]
        outcome: ConsumeOutcome,
        /// Optional context appended to the journal line
        #[arg(long)]
        note: Option<String>,
        /// Decision artifact that records the verdict (valid with --outcome done)
        #[arg(long)]
        decision: Option<String>,
    },
    /// Move artifacts to the cold sibling archive without deleting history
    Archive {
        /// Artifact filename inside the hot dir (a name, not a path)
        #[arg(required_unless_present = "consumed", conflicts_with = "consumed")]
        filename: Option<String>,
        /// Archive every consumed actionable artifact, including lower-priority tiers
        #[arg(long)]
        consumed: bool,
        /// With --consumed, include only artifacts older than this many days
        #[arg(long, requires = "consumed")]
        older_than_days: Option<u64>,
        /// Optional context appended to each journal line
        #[arg(long)]
        note: Option<String>,
    },
}

pub fn run(ctx: &Ctx, cmd: JournalCmd) -> Result<i32> {
    match cmd {
        JournalCmd::Dir { archive, explain } => {
            let resolution = resolve(&ctx.cwd)?;
            let directory = if archive {
                archive_dir(&resolution.directory)
            } else {
                resolution.directory
            };
            if explain {
                println!("source: {}", resolution.source.as_str());
                println!(
                    "anchor: {}",
                    resolution
                        .anchor
                        .as_deref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "none".into())
                );
                println!("directory: {}", directory.display());
            } else {
                println!("{}", directory.display());
            }
            Ok(0)
        }
        JournalCmd::Doctor { json } => doctor(ctx, json),
        JournalCmd::Note {
            topic,
            kind,
            body_file,
            title,
            scaffold,
            no_scaffold,
        } => note(
            ctx,
            &topic,
            kind,
            body_file.as_deref(),
            title.as_deref(),
            scaffold.as_deref(),
            no_scaffold,
        ),
        JournalCmd::Log { topic, message } => log_line(ctx, &topic, &message),
        JournalCmd::Position {
            filename,
            reference,
            body_file,
            question,
            option,
        } => position(
            ctx,
            &filename,
            reference.as_deref(),
            &body_file,
            question.as_deref(),
            option.as_deref(),
        ),
        JournalCmd::Question {
            filename,
            placement,
            options,
            body_file,
        } => question(ctx, &filename, &placement, &options, &body_file),
        JournalCmd::Answer {
            filename,
            question,
            option,
            body_file,
        } => answer(ctx, &filename, &question, &option, &body_file),
        JournalCmd::Events { limit } => events(ctx, limit),
        JournalCmd::Catchup {
            limit,
            json,
            archived,
        } => catchup(ctx, limit.unwrap_or(20), json, archived),
        JournalCmd::Memories { json } => memories(ctx, json),
        JournalCmd::Open { kind, json } => open(ctx, kind, json),
        JournalCmd::List { kind, json } => list(ctx, kind, json),
        JournalCmd::Show { filename } => show(ctx, &filename),
        JournalCmd::Discussion { filename, json } => discussion_summary(ctx, &filename, json),
        JournalCmd::Rebind { from } => rebind(ctx, &from),
        JournalCmd::Stamp => stamp(),
        JournalCmd::Lane { command } => lane(ctx, command),
        JournalCmd::Consume {
            filename,
            outcome,
            note,
            decision,
        } => consume(
            ctx,
            &filename,
            outcome,
            note.as_deref(),
            decision.as_deref(),
        ),
        JournalCmd::Archive {
            filename,
            consumed,
            older_than_days,
            note,
        } => archive(
            ctx,
            filename.as_deref(),
            consumed,
            older_than_days,
            note.as_deref(),
        ),
    }
}

/// Derive the cold sibling by appending `-archive` to the hot directory's
/// final path component, regardless of how the hot directory was configured.
pub fn archive_dir(hot: &Path) -> PathBuf {
    let mut cold = hot.as_os_str().to_os_string();
    cold.push("-archive");
    PathBuf::from(cold)
}

#[derive(Serialize)]
struct DoctorFinding {
    code: &'static str,
    detail: String,
}

#[derive(Serialize)]
struct DoctorReport {
    dir: String,
    problems: Vec<DoctorFinding>,
    advice: Vec<DoctorFinding>,
}

fn known_kind(kind: &str) -> bool {
    recognized_journal_kinds().any(|value| value == kind)
}

/// Whether a directory holds what a journal holds. A rebind moves whatever it
/// is given, so it should not be given an arbitrary directory.
fn looks_like_a_journal(dir: &Path) -> Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().to_string();
        if name == "events.jsonl"
            || name == "bindings.jsonl"
            || parse_artifact_name(&name).is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Remove a target journal that holds nothing but its own binding, so the
/// adopted journal can be renamed into its place.
fn clear_bound_target(target: &Path) -> Result<()> {
    let stale = bindings_path(target);
    if stale.is_file() {
        std::fs::remove_file(&stale)
            .with_context(|| format!("cannot remove {}", stale.display()))?;
    }
    std::fs::remove_dir(target)
        .with_context(|| format!("cannot replace empty {}", target.display()))
}

/// Whether a journal holds anything a rebind could destroy by merging.
///
/// A binding is not history: it says which project the directory belongs to,
/// which is exactly what a rebind is about to restate. Opening a change
/// registers the project, so a journal freshly created at a moved project's new
/// path holds a binding and nothing else — and refusing that would close the
/// recovery path in the one situation it exists for.
fn holds_history(dir: &Path) -> Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().to_string();
        if name == "bindings.jsonl" {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

/// Adopt an orphaned journal for the project standing here.
///
/// The move is recorded rather than performed as an untracked `mv`, so the
/// journal states both where it came from and where it belongs. Nothing is
/// inferred: the operator names the source, and a target that already holds
/// content is refused, because concatenating two event logs destroys which
/// history came from which project.
fn rebind(ctx: &Ctx, from: &str) -> Result<i32> {
    let source = config::expand_tilde(from)?;
    if !source.is_dir() {
        bail!("no journal at {}", source.display());
    }
    let resolution = resolve(&ctx.cwd)?;
    let target = resolution.directory;
    let anchor = resolution
        .anchor
        .context("this project has no stable journal anchor to rebind to")?;
    let anchor = anchor.display().to_string();
    if source == target {
        bail!("{} is already this project's journal", source.display());
    }
    // Every refusal happens before anything moves, so a rebind either does the
    // whole thing or leaves both journals exactly as they were.
    if !looks_like_a_journal(&source)? {
        bail!(
            "{} does not look like a journal: no artifacts and no event log",
            source.display()
        );
    }
    let recorded = recorded_anchor(&source)?;
    if let Some(recorded) = recorded.as_deref() {
        if Path::new(recorded).is_dir() && Path::new(recorded) != Path::new(&anchor) {
            bail!(
                "{} belongs to {recorded}, which still exists. Adopting it would take a live \
                 project's history",
                source.display()
            );
        }
    }
    if target.is_dir() && holds_history(&target)? {
        bail!(
            "target journal {} already holds content; merging two histories is not something \
             a rebind can do without losing which came from where",
            target.display()
        );
    }
    let source_archive = archive_dir(&source);
    let target_archive = archive_dir(&target);
    if target_archive.exists() {
        bail!(
            "{} exists and did not come from {}; move or remove it before rebinding, so cold \
             history stays attached to the journal it belongs to",
            target_archive.display(),
            source.display()
        );
    }
    let previous = recorded.unwrap_or_else(|| source.display().to_string());

    // Order matters more than atomicity here, because no filesystem offers a
    // two-directory move that either happens or does not. So the record is
    // written first, into the journal that is about to travel: a rebind that
    // fails afterwards leaves a journal that states what was attempted, and
    // one that fails here has moved nothing at all.
    let (harness, session) = identity(ctx);
    append_binding(
        ctx,
        &source,
        &JournalBinding {
            schema: BINDING_SCHEMA.to_string(),
            ts: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            event: "rebound".to_string(),
            anchor: anchor.clone(),
            previous_anchor: Some(previous.clone()),
            harness: Some(harness),
            session: Some(session),
        },
    )?;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    // The cold archive moves first: it is the half nobody looks at, so a
    // failure there is recoverable while the hot journal is still in place.
    let mut archive_moved = false;
    if source_archive.is_dir() {
        std::fs::rename(&source_archive, &target_archive).with_context(|| {
            format!(
                "cannot move {} to {}",
                source_archive.display(),
                target_archive.display()
            )
        })?;
        archive_moved = true;
    }
    if target.is_dir() {
        // Only a binding can be here — `holds_history` refused anything else —
        // and it says this directory belongs to the project doing the
        // rebinding, which is what the adopted journal is about to record
        // instead. Clearing it last keeps the window in which the target is
        // unbound as short as the move allows: everything that can fail on its
        // own has already succeeded, and only the rename remains.
        if let Err(error) = clear_bound_target(&target) {
            // The archive has already moved, so put it back before failing:
            // a retry should start from where it began, not from half a move.
            if archive_moved {
                if let Err(rollback) = std::fs::rename(&target_archive, &source_archive) {
                    bail!(
                        "{error}. Its cold archive is now at {} and could not be put back: \
                         {rollback}",
                        target_archive.display()
                    );
                }
            }
            return Err(error);
        }
    }
    if let Err(error) = std::fs::rename(&source, &target) {
        // A rename across filesystems cannot be atomic. Put back what did
        // move, and say precisely what is where if even that fails.
        if archive_moved {
            if let Err(rollback) = std::fs::rename(&target_archive, &source_archive) {
                bail!(
                    "cannot move {} to {}: {error}. Its cold archive is now at {} and could not \
                     be put back: {rollback}",
                    source.display(),
                    target.display(),
                    target_archive.display()
                );
            }
        }
        if let Err(restore) = std::fs::create_dir_all(&target) {
            bail!(
                "cannot move {} to {}: {error}. The empty target could not be recreated \
                 either: {restore}",
                source.display(),
                target.display()
            );
        }
        bail!(
            "cannot move {} to {}: {error}. Nothing moved. If they are on different \
             filesystems, copy the directory across and run this again",
            source.display(),
            target.display()
        );
    }

    println!("{}", target.display());
    println!("rebound: {previous} -> {anchor}");
    Ok(0)
}

/// Journals beside this one that were written for a project of this name,
/// when this one is empty.
///
/// The comparison is between recorded anchors, not between directory names:
/// a slug is a path with its separators flattened, so the last hyphen in it
/// may be part of the project's own name rather than a separator, and two
/// unrelated projects ending in `-api` would otherwise look like one moved
/// project. A journal with no recorded binding is not a candidate — the answer
/// improves as bindings accumulate rather than being guessed at.
///
/// This is a hint and never an instruction: merging two journals that both
/// hold history is worse than leaving one orphaned, because an orphan is at
/// least still separable.
fn split_journal_candidates(dir: &Path, anchor: Option<&Path>) -> Result<Vec<PathBuf>> {
    if dir.is_dir() && std::fs::read_dir(dir)?.next().is_some() {
        return Ok(Vec::new());
    }
    let (Some(root), Some(basename)) = (dir.parent(), anchor.and_then(|a| a.file_name())) else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(found);
    };
    for entry in entries {
        let candidate = entry?.path();
        if candidate == dir || !candidate.is_dir() {
            continue;
        }
        // The cold archive of another journal is not a rename target.
        if candidate
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with("-archive"))
        {
            continue;
        }
        let Some(recorded) = recorded_anchor(&candidate)? else {
            continue;
        };
        // A journal whose project still exists belongs to that project,
        // however its name reads. Only an orphan can be one this project left
        // behind.
        if Path::new(&recorded).is_dir() {
            continue;
        }
        if Path::new(&recorded).file_name() == Some(basename)
            && std::fs::read_dir(&candidate)?.next().is_some()
        {
            found.push(candidate);
        }
    }
    found.sort();
    Ok(found)
}

fn doctor(ctx: &Ctx, json: bool) -> Result<i32> {
    let resolution = resolve(&ctx.cwd)?;
    let dir = resolution.directory.clone();
    let cold = archive_dir(&dir);
    let mut problems = Vec::new();
    let mut advice = Vec::new();

    let jsonl = dir.join("events.jsonl");
    if jsonl.is_file() {
        let text = std::fs::read_to_string(&jsonl)
            .with_context(|| format!("cannot read {}", jsonl.display()))?;
        let mut question_events = Vec::new();
        for (index, line) in text.lines().enumerate() {
            match serde_json::from_str::<JournalEvent>(line) {
                Ok(event) if event.known() => question_events.push((index + 1, event)),
                Ok(_) => problems.push(DoctorFinding {
                    code: "unknown-jsonl-event",
                    detail: format!("events.jsonl line {}", index + 1),
                }),
                Err(_) => problems.push(DoctorFinding {
                    code: "malformed-jsonl",
                    detail: format!("events.jsonl line {}", index + 1),
                }),
            }
        }
        question_events.sort_by_key(|(_, event)| event.timestamp());
        let mut machine = QuestionMachine::default();
        for (line, event) in question_events {
            if !machine.accept(&event) {
                problems.push(DoctorFinding {
                    code: "invalid-question-state",
                    detail: format!("events.jsonl line {line}"),
                });
            }
        }
    }

    let mut hot_files = Vec::new();
    let mut retired_kind_counts = HashMap::new();
    if dir.is_dir() {
        let mut names = Vec::new();
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "events.jsonl" || name == "bindings.jsonl" {
                continue;
            }
            names.push(name);
        }
        names.sort();
        for name in names {
            match parse_artifact_name(&name) {
                None => problems.push(DoctorFinding {
                    code: "malformed-artifact-name",
                    detail: name,
                }),
                Some((_, _, kind)) if RETIRED_JOURNAL_KINDS.contains(&kind.as_str()) => {
                    *retired_kind_counts.entry(kind).or_insert(0) += 1;
                    hot_files.push(name);
                }
                Some((_, _, kind)) if !known_kind(&kind) => {
                    problems.push(DoctorFinding {
                        code: "unknown-artifact-kind",
                        detail: format!("{name}: {kind}"),
                    });
                }
                Some(_) => hot_files.push(name),
            }
        }
    }
    for kind in RETIRED_JOURNAL_KINDS {
        if let Some(count) = retired_kind_counts.get(kind) {
            advice.push(DoctorFinding {
                code: "retired-artifact-kind",
                detail: format!("{kind}: {count} hot artifacts"),
            });
        }
    }

    // Semantic checks run on the event stream: consumption, lane liveness,
    // and artifact references all derive from it.
    let events = read_events(&dir)?;

    // A journal is addressed by the slugged path of its project. When that
    // path stops existing the journal becomes unreachable from anywhere, and
    // nothing errors: `journal open` reports an empty queue, which looks
    // exactly like a project with no backlog.
    let bindings = bindings_path(&dir);
    if bindings.is_file() {
        let text = std::fs::read_to_string(&bindings)
            .with_context(|| format!("cannot read {}", bindings.display()))?;
        for (index, line) in text.lines().enumerate() {
            if serde_json::from_str::<JournalBinding>(line)
                .is_ok_and(|binding| binding.schema == BINDING_SCHEMA)
            {
                continue;
            }
            problems.push(DoctorFinding {
                code: "malformed-binding",
                detail: format!("bindings.jsonl line {}", index + 1),
            });
        }
    }
    match recorded_anchor(&dir)?.as_deref() {
        Some(anchor) if !Path::new(anchor).is_dir() => problems.push(DoctorFinding {
            code: "orphaned-journal",
            detail: format!("bound to {anchor}, which no longer exists"),
        }),
        // A journal with no binding predates the record and gets one the next
        // time anything is written to it, so there is nothing to report.
        _ => {}
    }
    for candidate in split_journal_candidates(&dir, resolution.anchor.as_deref())? {
        advice.push(DoctorFinding {
            code: "split-journal",
            detail: format!(
                "{} holds artifacts for a project of this name; if this project moved, \
                 `arc journal rebind {}` adopts it",
                candidate.display(),
                candidate.display()
            ),
        });
    }
    for event in &events {
        if ["consumed", "archived"].contains(&event.event.as_str()) {
            if let Some(file) = &event.file {
                // Only artifact-shaped names are references; legacy or
                // hand-written prose in the file field is not a dangle.
                if parse_artifact_name(file).is_some()
                    && !dir.join(file).is_file()
                    && !cold.join(file).is_file()
                {
                    problems.push(DoctorFinding {
                        code: "dangling-artifact-reference",
                        detail: format!("{} references {file}", event.event),
                    });
                }
            }
        }
    }

    let archivable = hot_files
        .iter()
        .filter(|name| {
            parse_artifact_name(name).is_some_and(|(_, _, kind)| {
                (is_actionable_kind(&kind) || kind == "memory") && is_consumed(&events, name)
            })
        })
        .count();
    if archivable > 0 {
        advice.push(DoctorFinding {
            code: "archivable-artifacts",
            detail: format!("{archivable} consumed actionable artifacts or retired memories remain in the hot dir; run journal archive --consumed"),
        });
    }

    let now = Utc::now();
    for lane in lanes_from_journal(&events, now)
        .into_iter()
        .filter(|lane| lane.state == "stale")
    {
        let age = now
            .signed_duration_since(lane.last_activity_time)
            .num_seconds()
            .max(0) as u64;
        advice.push(DoctorFinding {
            code: "stale-lane",
            detail: format!(
                "{} owned by {} {} idle {}",
                lane.topic,
                lane.owner_harness,
                lane.owner_session,
                format_age(age)
            ),
        });
    }

    let live_memories = hot_files
        .iter()
        .filter(|name| {
            parse_artifact_name(name).is_some_and(|(_, _, kind)| kind == "memory")
                && !is_consumed(&events, name)
        })
        .count();
    if live_memories > 20 {
        advice.push(DoctorFinding {
            code: "too-many-memories",
            detail: format!("{live_memories} live memories; recall degrades; retire aggressively"),
        });
    }

    let exit = i32::from(!problems.is_empty());
    let report = DoctorReport {
        dir: dir.display().to_string(),
        problems,
        advice,
    };
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("problems:");
        if report.problems.is_empty() {
            println!("  (none)");
        }
        for finding in &report.problems {
            println!("  {}: {}", finding.code, finding.detail);
        }
        println!("advice:");
        if report.advice.is_empty() {
            println!("  (none)");
        }
        for finding in &report.advice {
            println!("  {}: {}", finding.code, finding.detail);
        }
    }
    Ok(exit)
}

#[derive(Debug, Clone, Copy)]
enum ResolutionSource {
    Env,
    ConfigPrefix,
    Git,
}

impl ResolutionSource {
    fn as_str(self) -> &'static str {
        match self {
            ResolutionSource::Env => "env",
            ResolutionSource::ConfigPrefix => "config-prefix",
            ResolutionSource::Git => "git",
        }
    }
}

struct JournalResolution {
    directory: PathBuf,
    source: ResolutionSource,
    anchor: Option<PathBuf>,
}

/// Resolve the journal directory from an explicit directory, a configured
/// stable path scope, or Git repository identity.
pub fn resolve_dir(cwd: &Path) -> Result<PathBuf> {
    Ok(resolve(cwd)?.directory)
}

fn resolve(cwd: &Path) -> Result<JournalResolution> {
    if let Some(dir) = std::env::var_os("ARC_JOURNAL_DIR") {
        return Ok(JournalResolution {
            directory: PathBuf::from(dir),
            source: ResolutionSource::Env,
            anchor: None,
        });
    }
    let cfg = config::load()?;
    let canonical_cwd = std::fs::canonicalize(cwd)
        .with_context(|| format!("cannot canonicalize journal cwd {}", cwd.display()))?;
    let mut configured = None;
    for (raw_anchor, raw_directory) in &cfg.journal_dirs {
        let anchor_path = config::expand_tilde(raw_anchor)?;
        if !anchor_path.is_absolute() {
            bail!("journal path scope must be absolute: {raw_anchor:?}");
        }
        let Ok(anchor) = std::fs::canonicalize(&anchor_path) else {
            continue;
        };
        if !canonical_cwd.starts_with(&anchor) {
            continue;
        }
        let depth = anchor.components().count();
        if configured
            .as_ref()
            .is_none_or(|(best_depth, _, _)| depth > *best_depth)
        {
            configured = Some((depth, anchor, config::expand_tilde(raw_directory)?));
        }
    }
    if let Some((_, anchor, directory)) = configured {
        return Ok(JournalResolution {
            directory,
            source: ResolutionSource::ConfigPrefix,
            anchor: Some(anchor),
        });
    }
    let root = repo_root(&canonical_cwd).with_context(|| {
        format!(
            "cannot resolve a stable journal anchor from {}: Git discovery failed and no \
             [journals.dirs] path scope matched; set ARC_JOURNAL_DIR or add an absolute \
             path-prefix entry to {}",
            canonical_cwd.display(),
            cfg.config_path.display()
        )
    })?;
    Ok(JournalResolution {
        directory: cfg.ai_home.join("journals").join(config::path_slug(&root)),
        source: ResolutionSource::Git,
        anchor: Some(root),
    })
}

/// The main repository root, shared by every worktree. Keying the archive
/// off this (never a worktree path) means two worktrees of one repo always
/// resolve to the same directory.
fn repo_root(cwd: &Path) -> Result<PathBuf> {
    let common = gitio::common_dir(cwd)?;
    let root = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().unwrap_or(&common).to_path_buf()
    } else {
        common
    };
    Ok(root)
}

/// A topic is kebab-case-safe when it is one or more lowercase
/// alphanumeric segments joined by single hyphens (no leading, trailing,
/// or doubled hyphens). This keeps filenames parseable and unambiguous.
fn valid_topic(topic: &str) -> bool {
    if topic.is_empty() {
        return false;
    }
    let mut prev_hyphen = true; // guards a leading hyphen
    for ch in topic.chars() {
        if ch == '-' {
            if prev_hyphen {
                return false;
            }
            prev_hyphen = true;
        } else if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            prev_hyphen = false;
        } else {
            return false;
        }
    }
    !prev_hyphen // guards a trailing hyphen
}

fn identity(ctx: &Ctx) -> (String, String) {
    let harness = ctx
        .harness
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string();
    let session = ctx
        .session
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string();
    (harness, session)
}

fn read_body_verbatim(body_file: &str) -> Result<String> {
    if body_file == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("cannot read body from stdin")?;
        Ok(buf)
    } else {
        std::fs::read_to_string(body_file)
            .with_context(|| format!("cannot read body file {body_file}"))
    }
}

/// The scaffold a kind carries by default, for kinds whose conventions live in
/// a template rather than in the reader's head.
fn default_scaffold(kind: JournalKind) -> Option<&'static str> {
    match kind {
        JournalKind::Discussion => Some("discussion"),
        _ => None,
    }
}

fn note(
    ctx: &Ctx,
    topic: &str,
    kind: JournalKind,
    body_file: Option<&str>,
    title: Option<&str>,
    scaffold: Option<&str>,
    no_scaffold: bool,
) -> Result<i32> {
    if !valid_topic(topic) {
        bail!("topic {topic:?} is not kebab-case-safe (use lowercase a-z, 0-9, single hyphens)");
    }
    // A discussion carries its own conventions — the stance line the tally is
    // parsed from, the quoting reply form, the resolution vocabulary — so the
    // scaffold that states them is the default rather than something an author
    // has to know exists.
    let scaffold = match (scaffold, no_scaffold) {
        (Some(name), _) => Some(name),
        (None, true) => None,
        (None, false) => default_scaffold(kind),
    };
    // Read the body before touching the filesystem so a bad source path or
    // scaffold name leaves nothing written. A scaffold template is prepended
    // to the body; a scaffold with no --body-file records the template alone.
    let template = match scaffold {
        Some(name) => crate::commands::scaffold::resolve(ctx, name)?,
        None => String::new(),
    };
    let content = match body_file {
        Some(source) => read_body_verbatim(source)?,
        None => String::new(),
    };
    let body = crate::commands::scaffold::prepended(&template, &content);
    // An artifact with nothing in it is a queue entry that says nothing. A
    // repo-local template may be empty, so the check is on what would be
    // written rather than on which options were passed.
    if body.trim().is_empty() && title.is_none_or(|t| t.trim().is_empty()) {
        bail!("nothing to record: pass --body-file, --scaffold, or --title");
    }

    let dir = resolve_dir(&ctx.cwd)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create archive dir {}", dir.display()))?;

    let now = Utc::now();
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let filename = format!("{stamp}-{topic}-{}.md", kind.as_str());
    let path = dir.join(&filename);

    let contents = match title {
        Some(t) => format!("# {t}\n\n{body}"),
        None => body,
    };
    // Exclusive creation: a same-second same-topic/kind collision must fail
    // loudly rather than silently overwrite a queued artifact.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "cannot create {} (an artifact with this second's timestamp already exists)",
                    path.display()
                )
            })?;
        f.write_all(contents.as_bytes())
            .with_context(|| format!("cannot write {}", path.display()))?;
    }

    let mut event = JournalEvent::base(ctx, now, topic, "note");
    event.file = Some(filename.clone());
    event.title = title.map(str::to_string);
    append_event(ctx, &dir, &event)?;
    println!("{}", path.display());
    Ok(0)
}

fn log_line(ctx: &Ctx, topic: &str, message: &str) -> Result<i32> {
    if !valid_topic(topic) {
        bail!("topic {topic:?} is not kebab-case-safe (use lowercase a-z, 0-9, single hyphens)");
    }
    let dir = resolve_dir(&ctx.cwd)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create archive dir {}", dir.display()))?;
    let now = Utc::now();
    // Free text is always a log event: marker promotion is reserved for the
    // internal consume/archive/lane callers, so a message that happens to
    // begin with "archived " or "consumed " cannot forge a typed event.
    let mut event = JournalEvent::base(ctx, now, topic, "log");
    event.message = Some(message.to_string());
    append_event(ctx, &dir, &event)?;
    Ok(0)
}

/// Add a position to a live artifact and emit a typed `position` event.
/// The Markdown block and the event are the two halves of the design: the
/// block is for people and structural stance parsing; the event supplies the
/// stable position ID, activity time, identity, and reply edge. Advisory and
/// fail-open like every journal write — the block is appended even if the
/// identity is only partially known, and the file stays hand-writable.
fn position(
    ctx: &Ctx,
    filename: &str,
    reference: Option<&str>,
    body_file: &str,
    question: Option<&str>,
    option: Option<&str>,
) -> Result<i32> {
    // A branch needs both halves: which question, and which of its answers.
    // Refusing here keeps a half-declared branch out of the log, where it
    // would render as a position arguing under nothing.
    let branch = match (question, option) {
        (Some(question), Some(option)) => Some((question.to_string(), option.to_string())),
        (None, None) => None,
        (Some(_), None) => bail!("--question needs --option: name the branch this position argues"),
        (None, Some(_)) => {
            bail!("--option needs --question: name the question the branch belongs to")
        }
    };
    if filename.contains(['/', '\\']) {
        bail!("journal position takes an artifact filename inside the journal dir, not a path");
    }
    let Some((_, topic, kind)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    if branch.is_some() && kind != JournalKind::Discussion.as_str() {
        bail!("{filename} is a {kind}, not a discussion");
    }
    // Read the body before touching the filesystem so a bad source path leaves
    // the artifact untouched.
    let body = read_body_verbatim(body_file)?;

    // Positions ride an open discussion in the hot directory; a cold archived
    // artifact is a closed record, not an append target.
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let path = dir.join(filename);
    if !path.is_file() {
        bail!("no such artifact {} in {}", filename, dir.display());
    }
    let existing = read_events(&dir)?;
    if is_consumed(&existing, filename) {
        bail!("cannot append to consumed artifact {filename}; open a successor discussion");
    }
    // A branch naming a question that was never posed, or an option it never
    // offered, is an orphan: it renders under nothing and silently drops out of
    // every branch count. Refuse it rather than record it.
    if let Some((question, option)) = &branch {
        let Some(posed) = existing.iter().find(|event| {
            event.known()
                && event.event == "question"
                && event.file.as_deref() == Some(filename)
                && event.question_id.as_deref() == Some(question.as_str())
        }) else {
            bail!("no question {question} on {filename}");
        };
        let offered = posed.options.clone().unwrap_or_default();
        if !offered.iter().any(|value| value == option) {
            bail!(
                "{option:?} is not one of the options {question} offered ({})",
                offered.join(", ")
            );
        }
        if existing.iter().any(|event| {
            event.event == "answer"
                && event.file.as_deref() == Some(filename)
                && event.question_id.as_deref() == Some(question.as_str())
        }) {
            bail!("{question} is already answered; its branches are closed");
        }
    }

    let now = Utc::now();
    let ts = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let position_id = format!("pos-{}", ulid::Ulid::new().to_string().to_ascii_lowercase());
    let (harness, _) = identity(ctx);
    // The heading is tool-computed so the position timestamp is never authored
    // by hand. The model, when known, is the primary attribution — the whole
    // reason positions carry `### Position <id> (<model> via <harness>, <ts>)`.
    let under = match &branch {
        Some((question, option)) => format!(" under {question}={option}"),
        None => String::new(),
    };
    let heading = match ctx.model.as_deref().filter(|value| !value.is_empty()) {
        Some(model) => format!("### Position {position_id} ({model} via {harness}, {ts}){under}"),
        None => format!("### Position {position_id} ({harness}, {ts}){under}"),
    };
    let block = format!("\n{heading}\n\n{}\n", body.trim_end_matches('\n'));

    // Append-only write: O_APPEND places the block at the current end even if
    // another writer added a position since, so concurrent appends never clobber
    // each other the way a read-modify-write would.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("cannot open {} for append", path.display()))?;
        f.write_all(block.as_bytes())
            .with_context(|| format!("cannot append to {}", path.display()))?;
    }

    let mut event = JournalEvent::base(ctx, now, &topic, "position");
    event.file = Some(filename.to_string());
    event.position_id = Some(position_id);
    event.reference = reference.map(str::to_string);
    if let Some((question, option)) = branch {
        event.question_id = Some(question);
        event.option = Some(option);
    }
    append_event(ctx, &dir, &event)?;
    println!("{}", path.display());
    Ok(0)
}

/// Shared preflight for an append to an open discussion: the filename is a
/// name, the artifact exists, and it has not been consumed. A consumed
/// artifact is a closed record, so appending to one would edit history.
fn open_discussion(ctx: &Ctx, filename: &str) -> Result<(PathBuf, PathBuf, String)> {
    if filename.contains(['/', '\\']) {
        bail!("journal takes an artifact filename inside the journal dir, not a path");
    }
    let Some((_, topic, kind)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    if kind != JournalKind::Discussion.as_str() {
        bail!("{filename} is a {kind}, not a discussion");
    }
    let dir = resolve_dir(&ctx.cwd)?;
    let path = dir.join(filename);
    if !path.is_file() {
        bail!("no such artifact {} in {}", filename, dir.display());
    }
    if is_consumed(&read_events(&dir)?, filename) {
        bail!("cannot append to consumed artifact {filename}; open a successor discussion");
    }
    Ok((dir, path, topic))
}

fn append_block(path: &Path, heading: &str, body: &str) -> Result<()> {
    use std::io::Write;
    let block = format!("\n{heading}\n\n{}\n", body.trim_end_matches('\n'));
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("cannot open {} for append", path.display()))?;
    f.write_all(block.as_bytes())
        .with_context(|| format!("cannot append to {}", path.display()))
}

/// Pose a question only a person can settle.
///
/// Placement is the whole design. An `opening` question is answered before any
/// position exists, so every participant argues from the same premise. A
/// `closing` question is answered once the argument is in, so the answer is
/// made with the reasoning in front of it. There is no mid-argument placement,
/// because a question that blocks mid-run turns a delegated argument into
/// something the caller has to watch.
fn question(
    ctx: &Ctx,
    filename: &str,
    placement: &str,
    options: &[String],
    body_file: &str,
) -> Result<i32> {
    let mut seen = HashSet::new();
    for option in options {
        if option.trim().is_empty() {
            bail!("an option cannot be empty");
        }
        if !seen.insert(option.as_str()) {
            bail!("option {option:?} is offered twice; a choice needs distinct answers");
        }
    }
    if options.len() < 2 {
        bail!("a question needs at least two options; one option is a statement");
    }
    let body = read_body_verbatim(body_file)?;
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let (dir, path, topic) = open_discussion(ctx, filename)?;

    let now = Utc::now();
    let ts = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let question_id = format!("q-{}", ulid::Ulid::new().to_string().to_ascii_lowercase());
    let heading = format!(
        "### Question {question_id} ({placement}, {ts}) — {}",
        options.join(" | ")
    );
    append_block(&path, &heading, &body)?;

    let mut event = JournalEvent::base(ctx, now, &topic, "question");
    event.file = Some(filename.to_string());
    event.question_id = Some(question_id);
    event.placement = Some(placement.to_string());
    event.options = Some(options.to_vec());
    append_event(ctx, &dir, &event)?;
    println!("{}", path.display());
    Ok(0)
}

/// Settle an open question by choosing one of the options it offered.
///
/// The losing branches are not deleted. A branch that was argued and not taken
/// is evidence about the one that was, and it is the only record that the
/// alternative was explored rather than never considered.
fn answer(
    ctx: &Ctx,
    filename: &str,
    question_id: &str,
    option: &str,
    body_file: &str,
) -> Result<i32> {
    let body = read_body_verbatim(body_file)?;
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let (dir, path, topic) = open_discussion(ctx, filename)?;
    let events = read_events(&dir)?;

    let Some(posed) = events.iter().find(|event| {
        event.known()
            && event.event == "question"
            && event.file.as_deref() == Some(filename)
            && event.question_id.as_deref() == Some(question_id)
    }) else {
        bail!("no question {question_id} on {filename}");
    };
    let offered = posed.options.clone().unwrap_or_default();
    if !offered.iter().any(|value| value == option) {
        bail!(
            "{option:?} is not one of the options {question_id} offered ({})",
            offered.join(", ")
        );
    }
    if events.iter().any(|event| {
        event.known()
            && event.event == "answer"
            && event.file.as_deref() == Some(filename)
            && event.question_id.as_deref() == Some(question_id)
    }) {
        bail!("{question_id} is already answered; open a successor question to revisit it");
    }
    if posed.placement.as_deref() == Some("closing") {
        let argued: HashSet<&str> = events
            .iter()
            .filter(|event| {
                event.event == "position"
                    && event.file.as_deref() == Some(filename)
                    && event.question_id.as_deref() == Some(question_id)
            })
            .filter_map(|event| event.option.as_deref())
            .collect();
        let missing: Vec<&str> = offered
            .iter()
            .map(String::as_str)
            .filter(|offered| !argued.contains(offered))
            .collect();
        if !missing.is_empty() {
            bail!(
                "closing question {question_id} still has unargued options: {}",
                missing.join(", ")
            );
        }
    }

    let now = Utc::now();
    let ts = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let (harness, _) = identity(ctx);
    let who = match ctx.model.as_deref().filter(|value| !value.is_empty()) {
        Some(model) => format!("{model} via {harness}"),
        None => harness.to_string(),
    };
    let heading = format!("### Answer {question_id} = {option} ({who}, {ts})");
    append_block(&path, &heading, &body)?;

    let mut event = JournalEvent::base(ctx, now, &topic, "answer");
    event.file = Some(filename.to_string());
    event.question_id = Some(question_id.to_string());
    event.option = Some(option.to_string());
    append_event(ctx, &dir, &event)?;
    println!("{}", path.display());
    Ok(0)
}

fn events(ctx: &Ctx, limit: Option<usize>) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    for event in read_events(&dir)?
        .into_iter()
        .take(limit.unwrap_or(usize::MAX))
    {
        println!("{}", serde_json::to_string(&event)?);
    }
    Ok(0)
}

const JOURNAL_SCHEMA: &str = "journal-events/1";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct JournalEvent {
    schema: String,
    ts: String,
    harness: String,
    session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    topic: String,
    event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    /// Stable reply target for a position. Optional so pre-ID journal events
    /// remain valid `journal-events/1` input.
    #[serde(skip_serializing_if = "Option::is_none")]
    position_id: Option<String>,
    /// The position or item a `position` event answers (`--ref`): a position
    /// ID, a legacy timestamp, or an item slug. The machine-readable half of
    /// the reply-to convention; optional, and never authored for non-position
    /// events.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    reference: Option<String>,
    /// Stable ID of a question. Set on the `question` event that opens one, on
    /// an `answer` event that settles it, and on a `position` event argued
    /// under one of its options. Optional, so every event written before
    /// questions existed remains valid `journal-events/1` input — the same
    /// additive shape `position_id` took.
    #[serde(skip_serializing_if = "Option::is_none")]
    question_id: Option<String>,
    /// Where a question is answered: `opening` before any position is filed,
    /// or `closing` after the argument is in. Never `mid` — a question that
    /// blocks in the middle makes the caller monitor a run they delegated.
    #[serde(skip_serializing_if = "Option::is_none")]
    placement: Option<String>,
    /// The answers a question offers, in the order it offered them.
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Vec<String>>,
    /// One of a question's options: the branch a `position` argues under, or
    /// the branch an `answer` chose.
    #[serde(skip_serializing_if = "Option::is_none")]
    option: Option<String>,
}

/// Which project a journal belongs to, recorded append-only in `bindings.jsonl`
/// beside the artifacts.
///
/// This is metadata about the journal rather than an entry in the project's
/// narrative, so it lives apart from `events.jsonl`: a reader asking what
/// happened on this project should not have to skip over bookkeeping about
/// where the journal itself lives.
#[derive(Debug, Serialize, Deserialize)]
struct JournalBinding {
    schema: String,
    ts: String,
    event: String,
    anchor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_anchor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<String>,
}

const BINDING_SCHEMA: &str = "journal-binding/1";

fn bindings_path(dir: &Path) -> PathBuf {
    dir.join("bindings.jsonl")
}

fn read_bindings(dir: &Path) -> Result<Vec<JournalBinding>> {
    let path = bindings_path(dir);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    // Fails open like the event log: one bad line is not a reason to refuse
    // every command that touches the journal.
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<JournalBinding>(line).ok())
        .filter(|binding| binding.schema == BINDING_SCHEMA)
        .collect())
}

fn append_binding(ctx: &Ctx, dir: &Path, binding: &JournalBinding) -> Result<()> {
    use std::io::Write;
    let _ = ctx;
    let path = bindings_path(dir);
    let mut line = serde_json::to_string(binding)?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("cannot append to {}", path.display()))
}

impl JournalEvent {
    fn base(ctx: &Ctx, now: DateTime<Utc>, topic: &str, event: &str) -> Self {
        let (harness, session) = identity(ctx);
        Self {
            schema: JOURNAL_SCHEMA.to_string(),
            ts: now.to_rfc3339_opts(SecondsFormat::Secs, true),
            harness,
            session,
            // Model identity is optional end to end: absent means absent,
            // never "unknown".
            model: ctx
                .model
                .as_deref()
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            topic: topic.to_string(),
            event: event.to_string(),
            message: None,
            file: None,
            title: None,
            outcome: None,
            note: None,
            decision: None,
            ttl_seconds: None,
            scope: None,
            status: None,
            position_id: None,
            reference: None,
            question_id: None,
            placement: None,
            options: None,
            option: None,
        }
    }

    fn timestamp(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.ts)
            .ok()
            .map(|v| v.with_timezone(&Utc))
    }

    fn known(&self) -> bool {
        if self.schema != JOURNAL_SCHEMA || self.timestamp().is_none() || !valid_topic(&self.topic)
        {
            return false;
        }
        match self.event.as_str() {
            "log" => self.message.is_some(),
            "note" => self.file.is_some(),
            "position" => {
                self.file.is_some()
                    && self.placement.is_none()
                    && self.options.is_none()
                    && match (self.question_id.as_deref(), self.option.as_deref()) {
                        (None, None) => true,
                        (Some(question), Some(option)) => {
                            valid_question_id(question)
                                && !option.trim().is_empty()
                                && self.file_is_discussion()
                        }
                        _ => false,
                    }
            }
            "consumed" => {
                self.file.is_some()
                    && self
                        .outcome
                        .as_deref()
                        .is_some_and(|value| ["done", "superseded", "discarded"].contains(&value))
            }
            "archived" => self.file.is_some(),
            // A question is only a question if it names itself, says when it is
            // answered, and offers a choice. Two options is the floor: one
            // option is a statement.
            "question" => {
                self.file_is_discussion()
                    && self.question_id.as_deref().is_some_and(valid_question_id)
                    && self
                        .placement
                        .as_deref()
                        .is_some_and(|value| ["opening", "closing"].contains(&value))
                    && self
                        .options
                        .as_ref()
                        .is_some_and(|values| valid_question_options(values))
                    && self.option.is_none()
            }
            "answer" => {
                self.file_is_discussion()
                    && self.question_id.as_deref().is_some_and(valid_question_id)
                    && self
                        .option
                        .as_deref()
                        .is_some_and(|option| !option.trim().is_empty())
                    && self.placement.is_none()
                    && self.options.is_none()
            }
            "lane-opened" => self.ttl_seconds.is_some() && self.scope.is_some(),
            "lane-renewed" => true,
            "lane-closed" => self
                .outcome
                .as_deref()
                .is_some_and(|value| ["done", "handoff", "abandoned", "expired"].contains(&value)),
            _ => false,
        }
    }

    fn file_is_discussion(&self) -> bool {
        self.file.as_deref().is_some_and(|file| {
            parse_artifact_name(file)
                .is_some_and(|(_, _, kind)| kind == JournalKind::Discussion.as_str())
        })
    }
}

fn valid_question_id(value: &str) -> bool {
    value.strip_prefix("q-").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

fn valid_question_options(values: &[String]) -> bool {
    let mut seen = HashSet::new();
    values.len() >= 2
        && values
            .iter()
            .all(|value| !value.trim().is_empty() && seen.insert(value.as_str()))
}

struct QuestionProgress {
    placement: String,
    options: Vec<String>,
    argued: HashSet<String>,
    answered: bool,
}

#[derive(Default)]
struct QuestionMachine {
    questions: HashMap<(String, String), QuestionProgress>,
}

impl QuestionMachine {
    /// Accept events in journal order while preserving the question state
    /// machine. Structurally valid JSON is still advisory input: a dangling
    /// answer or a mutation after settlement is ignored and reported by
    /// `journal doctor`, never promoted into derived state.
    fn accept(&mut self, event: &JournalEvent) -> bool {
        if !event.known() {
            return false;
        }
        match event.event.as_str() {
            "question" => {
                let key = (
                    event.file.clone().expect("known question has a file"),
                    event.question_id.clone().expect("known question has an id"),
                );
                if self.questions.contains_key(&key) {
                    return false;
                }
                self.questions.insert(
                    key,
                    QuestionProgress {
                        placement: event
                            .placement
                            .clone()
                            .expect("known question has placement"),
                        options: event.options.clone().expect("known question has options"),
                        argued: HashSet::new(),
                        answered: false,
                    },
                );
                true
            }
            "position" => {
                let (Some(question), Some(option)) =
                    (event.question_id.as_deref(), event.option.as_deref())
                else {
                    return true;
                };
                let key = (
                    event.file.clone().expect("known position has a file"),
                    question.to_string(),
                );
                let Some(progress) = self.questions.get_mut(&key) else {
                    return false;
                };
                if progress.answered || !progress.options.iter().any(|offered| offered == option) {
                    return false;
                }
                progress.argued.insert(option.to_string());
                true
            }
            "answer" => {
                let key = (
                    event.file.clone().expect("known answer has a file"),
                    event.question_id.clone().expect("known answer has an id"),
                );
                let Some(progress) = self.questions.get_mut(&key) else {
                    return false;
                };
                let option = event.option.as_deref().expect("known answer has an option");
                if progress.answered || !progress.options.iter().any(|offered| offered == option) {
                    return false;
                }
                if progress.placement == "closing"
                    && progress
                        .options
                        .iter()
                        .any(|offered| !progress.argued.contains(offered))
                {
                    return false;
                }
                progress.answered = true;
                true
            }
            _ => true,
        }
    }
}

fn append_journal(
    dir: &Path,
    ctx: &Ctx,
    now: chrono::DateTime<Utc>,
    topic: &str,
    message: &str,
    filename: Option<&str>,
) -> Result<()> {
    let mut event = JournalEvent::base(ctx, now, topic, "log");
    if let Some(name) = filename {
        event.event = "note".to_string();
        event.file = Some(name.to_string());
        if !message.starts_with("wrote ") {
            event.title = Some(message.to_string());
        }
    } else if let Some((file, rest)) = message
        .strip_prefix("consumed ")
        .and_then(|rest| rest.split_once(" ["))
    {
        if let Some((outcome, note)) = rest.split_once(']') {
            event.event = "consumed".to_string();
            event.file = Some(file.to_string());
            event.outcome = Some(outcome.to_string());
            event.note = parse_optional_text(note).flatten();
        } else {
            event.message = Some(message.to_string());
        }
    } else if let Some(rest) = message.strip_prefix("archived ") {
        let (file, note) = rest
            .split_once(": ")
            .map_or((rest, None), |(f, n)| (f, Some(n)));
        event.event = "archived".to_string();
        event.file = Some(file.to_string());
        event.note = note.map(str::to_string);
    } else if let Some(marker) = parse_lane_marker(message) {
        match marker {
            LaneMarker::Opened { ttl, scope, status } => {
                event.event = "lane-opened".to_string();
                event.ttl_seconds = Some(ttl);
                event.scope = Some(scope);
                event.status = status;
            }
            LaneMarker::Renewed { ttl, status } => {
                event.event = "lane-renewed".to_string();
                event.ttl_seconds = ttl;
                event.status = status;
            }
            LaneMarker::Closed { outcome, note } => {
                event.event = "lane-closed".to_string();
                event.outcome = Some(outcome);
                event.note = note;
            }
        }
    } else {
        event.message = Some(message.to_string());
    }
    append_event(ctx, dir, &event)
}

/// The path this journal is bound to, as last recorded.
pub(crate) fn recorded_anchor(dir: &Path) -> Result<Option<String>> {
    Ok(read_bindings(dir)?.pop().map(|binding| binding.anchor))
}

/// Record which project this journal belongs to, once, the first time anything
/// is written to it. A journal is addressed by the slugged path of its Git
/// anchor, so moving the project silently starts a fresh one and strands the
/// old — and nothing in the old directory says where it came from. Advisory
/// like the rest of the journal: a failure to record the binding never unwinds
/// the artifact that was the point of the command.
fn ensure_bound(ctx: &Ctx, dir: &Path) -> Result<()> {
    // The question is whether a binding was recorded, not whether a file
    // exists: an empty or unreadable bindings file would otherwise suppress
    // the record forever.
    let recorded = recorded_anchor(dir)?;
    let Some(anchor) = resolve(&ctx.cwd)?.anchor else {
        return Ok(());
    };
    if let Some(recorded) = recorded.as_deref() {
        // Two different paths can slug to one journal directory, because the
        // slug maps `/` and `.` alike. A dead anchor there names a project that
        // is gone while this one resolves to the very same journal, so the
        // record is simply wrong about who owns it.
        //
        // Restating is safe only while there is no history to inherit. arc
        // cannot tell "this project moved between colliding paths" from "a
        // different project of that name was deleted", and in the second case a
        // silent restatement would hand one project another's artifacts. So a
        // journal holding history keeps its dead anchor and is reported as an
        // orphan; adopting it stays `rebind`'s explicit job.
        let stale = !Path::new(recorded).is_dir();
        if !stale || recorded == anchor.to_string_lossy() || holds_history(dir)? {
            return Ok(());
        }
    }
    let (harness, session) = identity(ctx);
    append_binding(
        ctx,
        dir,
        &JournalBinding {
            schema: BINDING_SCHEMA.to_string(),
            ts: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            event: "bound".to_string(),
            anchor: anchor.display().to_string(),
            previous_anchor: recorded,
            harness: Some(harness),
            session: Some(session),
        },
    )
}

fn append_event(ctx: &Ctx, dir: &Path, event: &JournalEvent) -> Result<()> {
    if let Err(error) = ensure_bound(ctx, dir) {
        eprintln!("warning: could not record this journal's project binding: {error:#}");
    }
    write_event(dir, event)
}

/// Register this project, so that opening a change is enough to make it
/// discoverable. The journal root is what enumerates projects, so a repository
/// whose ledger has changes but whose journal was never written would be
/// invisible to every cross-project view — the one place arc's own structure,
/// rather than habit, has to guarantee the project is known.
///
/// Advisory: nothing about opening a change depends on this succeeding, and a
/// journal that already carries a binding is left exactly as it is.
pub(crate) fn register_project(ctx: &Ctx) -> Result<()> {
    let dir = resolve_dir(&ctx.cwd)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create journal dir {}", dir.display()))?;
    ensure_bound(ctx, &dir)
}

fn write_event(dir: &Path, event: &JournalEvent) -> Result<()> {
    use std::io::Write;
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    let journal_path = dir.join("events.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&journal_path)
        .with_context(|| format!("cannot open {}", journal_path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("cannot append to {}", journal_path.display()))?;
    Ok(())
}

const DEFAULT_LANE_TTL: u64 = 2 * 60 * 60;

#[derive(Debug, PartialEq, Eq)]
enum LaneMarker {
    Opened {
        ttl: u64,
        scope: Vec<String>,
        status: Option<String>,
    },
    Renewed {
        ttl: Option<u64>,
        status: Option<String>,
    },
    Closed {
        outcome: String,
        note: Option<String>,
    },
}

fn parse_ttl(value: &str) -> Option<u64> {
    let (number, unit) = value.split_at(value.len().checked_sub(1)?);
    let number: u64 = number.parse().ok()?;
    if number == 0 {
        return None;
    }
    number.checked_mul(match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return None,
    })
}

fn parse_optional_text(rest: &str) -> Option<Option<String>> {
    if rest.is_empty() {
        Some(None)
    } else {
        rest.strip_prefix(": ").map(|text| Some(text.to_string()))
    }
}

fn take_bracketed_ttl(rest: &str) -> Option<(u64, &str)> {
    let rest = rest.strip_prefix('[')?;
    let end = rest.find(']')?;
    let ttl = parse_ttl(&rest[..end])?;
    Some((ttl, &rest[end + 1..]))
}

fn parse_lane_marker(message: &str) -> Option<LaneMarker> {
    if let Some(mut rest) = message.strip_prefix("lane opened") {
        let mut ttl = DEFAULT_LANE_TTL;
        if let Some(after_space) = rest.strip_prefix(' ') {
            if after_space.starts_with('[') {
                (ttl, rest) = take_bracketed_ttl(after_space)?;
            }
        }
        let mut scope = Vec::new();
        if let Some(after_scope) = rest.strip_prefix(" scope=") {
            let (topics, tail) = match after_scope.split_once(": ") {
                Some((topics, status)) => (topics, Some(status)),
                None => (after_scope, None),
            };
            scope = topics.split(',').map(str::to_string).collect();
            if scope.is_empty() || scope.iter().any(|topic| !valid_topic(topic)) {
                return None;
            }
            return Some(LaneMarker::Opened {
                ttl,
                scope,
                status: tail.map(str::to_string),
            });
        }
        return Some(LaneMarker::Opened {
            ttl,
            scope,
            status: parse_optional_text(rest)?,
        });
    }
    if let Some(mut rest) = message.strip_prefix("lane renewed") {
        let mut ttl = None;
        if let Some(after_space) = rest.strip_prefix(' ') {
            if after_space.starts_with('[') {
                let (parsed, tail) = take_bracketed_ttl(after_space)?;
                ttl = Some(parsed);
                rest = tail;
            }
        }
        return Some(LaneMarker::Renewed {
            ttl,
            status: parse_optional_text(rest)?,
        });
    }
    if let Some(rest) = message.strip_prefix("lane closed ") {
        let rest = rest.strip_prefix('[')?;
        let end = rest.find(']')?;
        let outcome = &rest[..end];
        if !["done", "handoff", "abandoned", "expired"].contains(&outcome) {
            return None;
        }
        return Some(LaneMarker::Closed {
            outcome: outcome.to_string(),
            note: parse_optional_text(&rest[end + 1..])?,
        });
    }
    None
}

fn read_events(dir: &Path) -> Result<Vec<JournalEvent>> {
    let mut events = Vec::new();
    let jsonl = dir.join("events.jsonl");
    if jsonl.is_file() {
        let text = std::fs::read_to_string(&jsonl)
            .with_context(|| format!("cannot read {}", jsonl.display()))?;
        events.extend(text.lines().filter_map(|line| {
            let event: JournalEvent = serde_json::from_str(line).ok()?;
            event.known().then_some(event)
        }));
    }
    events.sort_by_key(JournalEvent::timestamp);
    let mut machine = QuestionMachine::default();
    Ok(events
        .into_iter()
        .filter(|event| machine.accept(event))
        .collect())
}

fn event_message(event: &JournalEvent) -> String {
    match event.event.as_str() {
        "log" => event.message.clone().unwrap_or_default(),
        "note" => event
            .title
            .clone()
            .unwrap_or_else(|| "wrote artifact".into()),
        "question" => format!(
            "asked {} [{}] ({}) on {}",
            event.question_id.as_deref().unwrap_or_default(),
            event.placement.as_deref().unwrap_or_default(),
            event.options.clone().unwrap_or_default().join(" | "),
            event.file.as_deref().unwrap_or_default(),
        ),
        "answer" => format!(
            "answered {} = {} on {}",
            event.question_id.as_deref().unwrap_or_default(),
            event.option.as_deref().unwrap_or_default(),
            event.file.as_deref().unwrap_or_default(),
        ),
        "consumed" => format!(
            "consumed {} [{}]{}",
            event.file.as_deref().unwrap_or_default(),
            event.outcome.as_deref().unwrap_or_default(),
            event
                .note
                .as_deref()
                .map(|v| format!(": {v}"))
                .unwrap_or_default()
        ),
        "archived" => format!(
            "archived {}{}",
            event.file.as_deref().unwrap_or_default(),
            event
                .note
                .as_deref()
                .map(|v| format!(": {v}"))
                .unwrap_or_default()
        ),
        "lane-opened" => format!(
            "lane opened [{}]{}{}",
            format_age(event.ttl_seconds.unwrap_or(DEFAULT_LANE_TTL)),
            event
                .scope
                .as_deref()
                .filter(|v| !v.is_empty())
                .map(|v| format!(" scope={}", v.join(",")))
                .unwrap_or_default(),
            event
                .status
                .as_deref()
                .map(|v| format!(": {v}"))
                .unwrap_or_default()
        ),
        "lane-renewed" => format!(
            "lane renewed{}{}",
            event
                .ttl_seconds
                .map(|v| format!(" [{}]", format_age(v)))
                .unwrap_or_default(),
            event
                .status
                .as_deref()
                .map(|v| format!(": {v}"))
                .unwrap_or_default()
        ),
        "lane-closed" => format!(
            "lane closed [{}]{}",
            event.outcome.as_deref().unwrap_or_default(),
            event
                .note
                .as_deref()
                .map(|v| format!(": {v}"))
                .unwrap_or_default()
        ),
        _ => String::new(),
    }
}

fn render_event(event: &JournalEvent) -> String {
    let mut line = format!(
        "- {} {} {} {}: {}",
        event.ts,
        event.harness,
        event.session,
        event.topic,
        event_message(event)
    );
    if event.event == "note" {
        if let Some(file) = &event.file {
            line.push_str(&format!(" ({file})"));
        }
    }
    line
}

#[derive(Clone, Serialize)]
pub(crate) struct LaneEntry {
    pub(crate) topic: String,
    pub(crate) owner_harness: String,
    pub(crate) owner_session: String,
    pub(crate) state: String,
    pub(crate) opened_at: String,
    pub(crate) last_activity: String,
    pub(crate) ttl_seconds: u64,
    pub(crate) scope: Vec<String>,
    pub(crate) status: Option<String>,
    #[serde(skip)]
    opened_time: DateTime<Utc>,
    #[serde(skip)]
    last_activity_time: DateTime<Utc>,
}

fn lanes_from_journal(events: &[JournalEvent], now: DateTime<Utc>) -> Vec<LaneEntry> {
    struct ActiveLane {
        topic: String,
        owner_harness: String,
        owner_session: String,
        opened_time: DateTime<Utc>,
        ttl_seconds: u64,
        scope: Vec<String>,
        status: Option<String>,
    }

    let mut last_activity = HashMap::new();
    for event in events {
        if let Some(timestamp) = event.timestamp() {
            last_activity.insert(event.session.clone(), timestamp);
        }
    }
    let mut active: HashMap<String, ActiveLane> = HashMap::new();
    for event in events.iter().filter(|event| event.known()) {
        let marker = match event.event.as_str() {
            "lane-opened" => Some(LaneMarker::Opened {
                ttl: event.ttl_seconds.unwrap_or(DEFAULT_LANE_TTL),
                scope: event.scope.clone().unwrap_or_default(),
                status: event.status.clone(),
            }),
            "lane-renewed" => Some(LaneMarker::Renewed {
                ttl: event.ttl_seconds,
                status: event.status.clone(),
            }),
            "lane-closed" => Some(LaneMarker::Closed {
                outcome: event.outcome.clone().unwrap_or_default(),
                note: event.note.clone(),
            }),
            _ => None,
        };
        let Some(marker) = marker else {
            continue;
        };
        let Some(timestamp) = event.timestamp() else {
            continue;
        };
        match marker {
            LaneMarker::Opened { ttl, scope, status } => {
                active.retain(|_, lane| lane.owner_session != event.session);
                active.insert(
                    event.topic.clone(),
                    ActiveLane {
                        topic: event.topic.clone(),
                        owner_harness: event.harness.clone(),
                        owner_session: event.session.clone(),
                        opened_time: timestamp,
                        ttl_seconds: ttl,
                        scope,
                        status,
                    },
                );
            }
            LaneMarker::Renewed { ttl, status } => {
                if let Some(lane) = active.get_mut(&event.topic) {
                    if let Some(ttl) = ttl {
                        lane.ttl_seconds = ttl;
                    }
                    if status.is_some() {
                        lane.status = status;
                    }
                }
            }
            LaneMarker::Closed { .. } => {
                active.remove(&event.topic);
            }
        }
    }

    let mut lanes: Vec<_> = active
        .into_values()
        .map(|lane| {
            let activity = last_activity
                .get(&lane.owner_session)
                .copied()
                .unwrap_or(lane.opened_time);
            let elapsed = now.signed_duration_since(activity).num_seconds().max(0) as u64;
            LaneEntry {
                topic: lane.topic,
                owner_harness: lane.owner_harness,
                owner_session: lane.owner_session,
                state: if elapsed < lane.ttl_seconds {
                    "live".to_string()
                } else {
                    "stale".to_string()
                },
                opened_at: lane.opened_time.to_rfc3339_opts(SecondsFormat::Secs, true),
                last_activity: activity.to_rfc3339_opts(SecondsFormat::Secs, true),
                ttl_seconds: lane.ttl_seconds,
                scope: lane.scope,
                status: lane.status,
                opened_time: lane.opened_time,
                last_activity_time: activity,
            }
        })
        .collect();
    lanes.sort_by(|a, b| {
        (a.state == "stale")
            .cmp(&(b.state == "stale"))
            .then_with(|| b.opened_time.cmp(&a.opened_time))
    });
    lanes
}

/// Seconds between an artifact's creation stamp (`%Y%m%dT%H%M%SZ`, the leading
/// filename component) and `now`. `None` when the stamp does not parse, so a
/// malformed name degrades to no age rather than a bogus one.
fn artifact_age_seconds(now: DateTime<Utc>, stamp: &str) -> Option<u64> {
    let created = NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ")
        .ok()?
        .and_utc();
    Some(now.signed_duration_since(created).num_seconds().max(0) as u64)
}

/// An artifact's filing time, read from the stamp its name carries.
fn parse_artifact_timestamp(stamp: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ")
        .ok()
        .map(|naive| naive.and_utc())
}

/// A caller-supplied boundary for "what is new", in either the journal's own
/// stamp format or RFC 3339. arc stores no previous-run marker: the delta's
/// memory belongs to whoever scheduled the run, so the command stays derived.
pub(crate) fn parse_since(raw: &str) -> Result<DateTime<Utc>> {
    if let Some(parsed) = parse_artifact_timestamp(raw) {
        return Ok(parsed);
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| {
            anyhow::anyhow!("expected a journal stamp (20260101T000000Z) or an RFC 3339 timestamp")
        })
}

/// A discussion waits from its newest typed position, falling back to artifact
/// creation before anyone has answered. Malformed timestamps fail open to the
/// creation age just like malformed event lines are ignored by `read_events`.
fn discussion_age_seconds(
    now: DateTime<Utc>,
    stamp: &str,
    filename: &str,
    events: &[JournalEvent],
) -> Option<u64> {
    events
        .iter()
        .rev()
        .find(|event| event.event == "position" && event.file.as_deref() == Some(filename))
        .and_then(JournalEvent::timestamp)
        .map(|activity| now.signed_duration_since(activity).num_seconds().max(0) as u64)
        .or_else(|| artifact_age_seconds(now, stamp))
}

fn format_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

fn render_lanes(lanes: &[LaneEntry], now: DateTime<Utc>) {
    println!("lanes:");
    if lanes.is_empty() {
        println!("  (none)");
    }
    for lane in lanes {
        let age = now
            .signed_duration_since(lane.last_activity_time)
            .num_seconds()
            .max(0) as u64;
        let activity = if lane.state == "live" {
            format!(
                "live (updated {} ago, ttl {})",
                format_age(age),
                format_age(lane.ttl_seconds)
            )
        } else {
            format!(
                "stale (idle {}, ttl {})",
                format_age(age),
                format_age(lane.ttl_seconds)
            )
        };
        let scope = if lane.scope.is_empty() {
            String::new()
        } else {
            format!("  +scope: {}", lane.scope.join(", "))
        };
        println!(
            "  {}  {} {}  {}{}",
            lane.topic, lane.owner_harness, lane.owner_session, activity, scope
        );
        if let Some(status) = &lane.status {
            println!("    {status}");
        }
    }
}

fn require_lane_session(ctx: &Ctx) -> Result<String> {
    let (_, session) = identity(ctx);
    if session == "unknown" {
        bail!("journal lane requires a session identity (--session or ARC_SESSION)");
    }
    Ok(session)
}

fn lane(ctx: &Ctx, command: LaneCmd) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let now = Utc::now();
    let journal_events = read_events(&dir)?;
    let lanes = lanes_from_journal(&journal_events, now);
    match command {
        LaneCmd::Open {
            topic,
            scope,
            ttl,
            status,
            status_file,
        } => {
            let status = match (status, status_file) {
                (None, None) => None,
                (status, status_file) => Some(crate::commands::read_body(status, status_file)?),
            };
            let session = require_lane_session(ctx)?;
            if !valid_topic(&topic) {
                bail!("topic {topic:?} is not kebab-case-safe (use lowercase a-z, 0-9, single hyphens)");
            }
            parse_ttl(&ttl).context("ttl must be a positive integer followed by s, m, or h")?;
            let scope: Vec<String> = scope
                .as_deref()
                .map(|value| value.split(',').map(str::to_string).collect())
                .unwrap_or_default();
            if scope.iter().any(|topic| !valid_topic(topic)) {
                bail!("scope topics must be comma-separated kebab-case-safe topics");
            }
            if let Some(overlap) = lanes.iter().find(|lane| {
                lane.state == "live"
                    && lane.owner_session != session
                    && (lane.topic == topic || lane.scope.contains(&topic))
            }) {
                eprintln!(
                    "warning: topic {topic} is covered by live lane {} owned by {} {}",
                    overlap.topic, overlap.owner_harness, overlap.owner_session
                );
            }
            let mut message = format!("lane opened [{ttl}]");
            if !scope.is_empty() {
                message.push_str(&format!(" scope={}", scope.join(",")));
            }
            if let Some(status) = status {
                message.push_str(&format!(": {status}"));
            }
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("cannot create archive dir {}", dir.display()))?;
            append_journal(&dir, ctx, now, &topic, &message, None)?;
        }
        LaneCmd::Renew {
            topic,
            ttl,
            status,
            status_file,
        } => {
            let status = match (status, status_file) {
                (None, None) => None,
                (status, status_file) => Some(crate::commands::read_body(status, status_file)?),
            };
            let session = require_lane_session(ctx)?;
            let current = lanes
                .iter()
                .find(|lane| lane.topic == topic)
                .with_context(|| format!("lane {topic} does not exist or is already closed"))?;
            if current.owner_session != session {
                bail!(
                    "lane {topic} is owned by {} {}",
                    current.owner_harness,
                    current.owner_session
                );
            }
            if let Some(ttl) = &ttl {
                parse_ttl(ttl).context("ttl must be a positive integer followed by s, m, or h")?;
            }
            let mut message = "lane renewed".to_string();
            if let Some(ttl) = ttl {
                message.push_str(&format!(" [{ttl}]"));
            }
            if let Some(status) = status {
                message.push_str(&format!(": {status}"));
            }
            append_journal(&dir, ctx, now, &topic, &message, None)?;
        }
        LaneCmd::Close {
            topic,
            outcome,
            note,
        } => {
            let session = require_lane_session(ctx)?;
            let current = lanes
                .iter()
                .find(|lane| lane.topic == topic)
                .with_context(|| format!("lane {topic} does not exist or is already closed"))?;
            if current.owner_session != session {
                let idle = now
                    .signed_duration_since(current.last_activity_time)
                    .num_seconds()
                    .max(0) as u64;
                if !matches!(outcome, LaneOutcome::Expired) || current.state != "stale" {
                    bail!(
                        "lane {topic} conflict: owner {} {}, session {}, idle {}, ttl {}",
                        current.owner_harness,
                        current.owner_session,
                        session,
                        format_age(idle),
                        format_age(current.ttl_seconds)
                    );
                }
            }
            let mut message = format!("lane closed [{}]", outcome.as_str());
            if let Some(note) = note {
                message.push_str(&format!(": {note}"));
            }
            append_journal(&dir, ctx, now, &topic, &message, None)?;
        }
        LaneCmd::List { json } => {
            if json {
                #[derive(Serialize)]
                struct LaneList<'a> {
                    dir: String,
                    lanes: &'a [LaneEntry],
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&LaneList {
                        dir: dir.display().to_string(),
                        lanes: &lanes
                    })?
                );
            } else {
                render_lanes(&lanes, now);
            }
        }
    }
    Ok(0)
}

#[derive(Serialize)]
struct ArtifactEntry {
    file: String,
    timestamp: String,
    topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    heading: Option<String>,
    /// Seconds since the artifact's latest position event for discussions, or
    /// creation stamp for other kinds. Absent only if neither timestamp parses.
    #[serde(skip_serializing_if = "Option::is_none")]
    age_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lane: Option<ArtifactLane>,
    /// The open change that has taken this item up, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    change: Option<ChangeRef>,
}

#[derive(Serialize)]
struct ChangeRef {
    id: String,
    status: String,
}

#[derive(Serialize)]
pub(crate) struct ContextJournalItem {
    pub(crate) file: String,
    pub(crate) topic: String,
    pub(crate) kind: String,
    pub(crate) heading: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ContextJournal {
    pub(crate) lanes: Vec<LaneEntry>,
    pub(crate) open_items: Vec<ContextJournalItem>,
}

impl ContextJournal {
    pub(crate) fn render_markdown(&self) {
        println!("\n## Journal\n");
        println!("### Live lanes");
        if self.lanes.is_empty() {
            println!("- (none)");
        } else {
            for lane in &self.lanes {
                println!(
                    "- {}: {}/{}{}",
                    lane.topic,
                    lane.owner_harness,
                    lane.owner_session,
                    lane.status
                        .as_deref()
                        .map(|status| format!(" — {status}"))
                        .unwrap_or_default()
                );
            }
        }
        println!("\n### Open items");
        if self.open_items.is_empty() {
            println!("- (none)");
        } else {
            for item in &self.open_items {
                println!(
                    "- {} [{}]{}",
                    item.file,
                    item.kind,
                    item.heading
                        .as_deref()
                        .map(|heading| format!(" — {heading}"))
                        .unwrap_or_default()
                );
            }
        }
    }
}

/// Read the live lanes and unconsumed actionable journal items relevant to a
/// change slug. This is advisory context for `arc resume`, never policy input.
pub(crate) fn context_for_change(ctx: &Ctx, slug: &str) -> Result<ContextJournal> {
    let dir = resolve_dir(&ctx.cwd)?;
    let events = read_events(&dir)?;
    let lanes = lanes_from_journal(&events, Utc::now())
        .into_iter()
        .filter(|lane| lane.state == "live")
        .collect();
    let mut open_items = Vec::new();
    if dir.is_dir() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))?
        {
            let name = entry?.file_name().to_string_lossy().to_string();
            let Some((_, topic, kind)) = parse_artifact_name(&name) else {
                continue;
            };
            if topic.contains(slug) && is_actionable_kind(&kind) && !is_consumed(&events, &name) {
                open_items.push(ContextJournalItem {
                    heading: first_heading(&dir.join(&name)),
                    file: name,
                    topic,
                    kind,
                });
            }
        }
    }
    open_items.sort_by(|left, right| right.file.cmp(&left.file));
    Ok(ContextJournal { lanes, open_items })
}

#[derive(Clone, Serialize)]
struct ArtifactLane {
    topic: String,
    owner_harness: String,
    owner_session: String,
    this_session: bool,
}

#[derive(Serialize)]
struct Catchup {
    dir: String,
    lanes: Vec<LaneEntry>,
    memories: Vec<ArtifactEntry>,
    files: Vec<ArtifactEntry>,
    journal_tail: Vec<String>,
}

/// Split `<ts>-<topic>-<kind>.md` into its parts.
fn parse_artifact_name(name: &str) -> Option<(String, String, String)> {
    let stem = name.strip_suffix(".md")?;
    let first = stem.find('-')?;
    let ts = &stem[..first];
    let remainder = &stem[first + 1..];
    let known = recognized_journal_kinds()
        .filter_map(|kind| {
            remainder
                .strip_suffix(kind)
                .and_then(|topic| topic.strip_suffix('-'))
                .map(|topic| (topic, kind))
        })
        .max_by_key(|(_, kind)| kind.len());
    let (topic, kind) = match known {
        Some(parts) => parts,
        None => {
            let last = remainder.rfind('-')?;
            (&remainder[..last], &remainder[last + 1..])
        }
    };
    if ts.is_empty() || topic.is_empty() || kind.is_empty() {
        return None;
    }
    Some((ts.to_string(), topic.to_string(), kind.to_string()))
}

fn first_heading(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find(|l| l.trim_start().starts_with('#'))
        .map(|l| l.trim().to_string())
}

/// All artifact filenames in `dir`, newest first: filenames lead with a
/// lexically sortable UTC stamp, so descending string order is newest-first.
/// `events.jsonl` and other non-artifact files never parse and drop out.
fn sorted_artifact_names(dir: &Path) -> Result<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    if dir.is_dir() {
        for entry in
            std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?
        {
            let name = entry?.file_name().to_string_lossy().to_string();
            if parse_artifact_name(&name).is_some() {
                names.push(name);
            }
        }
    }
    names.sort();
    names.reverse();
    Ok(names)
}

fn live_memories(dir: &Path) -> Result<Vec<ArtifactEntry>> {
    let journal = read_events(dir)?;
    let mut names = Vec::new();
    if dir.is_dir() {
        for entry in
            std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?
        {
            let name = entry?.file_name().to_string_lossy().to_string();
            if parse_artifact_name(&name).is_some_and(|(_, _, kind)| kind == "memory")
                && !is_consumed(&journal, &name)
            {
                names.push(name);
            }
        }
    }
    names.sort();
    names.reverse();
    Ok(names
        .into_iter()
        .filter_map(|name| {
            let (timestamp, topic, _) = parse_artifact_name(&name)?;
            Some(ArtifactEntry {
                heading: first_heading(&dir.join(&name)),
                file: name,
                timestamp,
                topic,
                kind: None,
                age_seconds: None,
                lane: None,
                change: None,
            })
        })
        .collect())
}

fn render_memories(memories: &[ArtifactEntry]) {
    println!("memory:");
    if memories.is_empty() {
        println!("  (none)");
    }
    for memory in memories {
        println!(
            "  {}  {}  {}",
            memory.timestamp,
            memory.topic,
            memory.heading.as_deref().unwrap_or("")
        );
    }
}

fn memories(ctx: &Ctx, json: bool) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let memories = live_memories(&dir)?;
    if json {
        #[derive(Serialize)]
        struct Memories {
            dir: String,
            memories: Vec<ArtifactEntry>,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Memories {
                dir: dir.display().to_string(),
                memories,
            })?
        );
    } else {
        render_memories(&memories);
    }
    Ok(0)
}

fn catchup(ctx: &Ctx, limit: usize, json: bool, archived: bool) -> Result<i32> {
    let hot_dir = resolve_dir(&ctx.cwd)?;
    let dir = if archived {
        archive_dir(&hot_dir)
    } else {
        hot_dir.clone()
    };
    let mut files: Vec<ArtifactEntry> = Vec::new();
    if dir.is_dir() {
        for name in sorted_artifact_names(&dir)?.into_iter().take(limit) {
            if let Some((ts, topic, kind)) = parse_artifact_name(&name) {
                let heading = first_heading(&dir.join(&name));
                files.push(ArtifactEntry {
                    file: name,
                    timestamp: ts,
                    topic,
                    kind: Some(kind),
                    heading,
                    age_seconds: None,
                    lane: None,
                    change: None,
                });
            }
        }
    }

    let journal_tail = journal_tail(&hot_dir, limit)?;
    let now = Utc::now();
    let lanes = lanes_from_journal(&read_events(&hot_dir)?, now);
    let memories = if archived {
        Vec::new()
    } else {
        live_memories(&hot_dir)?
    };

    if json {
        let out = Catchup {
            dir: dir.display().to_string(),
            lanes,
            memories,
            files,
            journal_tail,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        render_lanes(&lanes, now);
        if !archived {
            render_memories(&memories);
        }
        println!("dir: {}", dir.display());
        println!("artifacts (newest first):");
        if files.is_empty() {
            println!("  (none)");
        }
        for f in &files {
            let heading = f.heading.as_deref().unwrap_or("");
            println!(
                "  {}  {}  {}  {}",
                f.timestamp,
                f.topic,
                f.kind.as_deref().unwrap_or(""),
                heading
            );
        }
        println!("journal tail:");
        if journal_tail.is_empty() {
            println!("  (none)");
        }
        for line in &journal_tail {
            println!("  {line}");
        }
    }
    Ok(0)
}

/// The outcome an artifact was consumed with, if it was consumed at all.
/// Last consume wins: the `consume` command refuses an already-consumed
/// artifact, but the journal is hand-writable and derived views must be
/// robust to a re-consume event anyway — display the latest, not the
/// earliest.
fn consumption(events: &[JournalEvent], filename: &str) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|event| {
            event.known()
                && event.event == "consumed"
                && event.file.as_deref() == Some(filename)
                && event
                    .outcome
                    .as_deref()
                    .is_some_and(|outcome| ["done", "superseded", "discarded"].contains(&outcome))
        })
        .and_then(|event| event.outcome.clone())
}

fn is_consumed(events: &[JournalEvent], filename: &str) -> bool {
    consumption(events, filename).is_some()
}

#[derive(Serialize)]
pub(crate) struct OpenItems {
    dir: String,
    open: Vec<ArtifactEntry>,
    later: Vec<ArtifactEntry>,
    feature_requests: Vec<ArtifactEntry>,
}

impl OpenItems {
    pub(crate) fn dir(&self) -> &str {
        &self.dir
    }

    pub(crate) fn tier_counts(&self) -> (usize, usize, usize) {
        (
            self.open.len(),
            self.later.len(),
            self.feature_requests.len(),
        )
    }

    /// The primary tier's oldest entry, in whole days. Across projects this is
    /// what makes a small queue visible: one item waiting a month does not look
    /// like a backlog from inside its own project.
    pub(crate) fn oldest_open_days(&self) -> Option<u64> {
        self.open
            .iter()
            .filter_map(|entry| entry.age_seconds)
            .max()
            .map(|seconds| seconds / 86_400)
    }

    /// Tier counts restricted to artifacts filed at or after `cutoff` — the
    /// delta question, what is new, asked of one project's queue.
    ///
    /// An artifact whose stamp cannot be read is counted as new. A delta that
    /// silently drops what it cannot date would under-report, and this queue
    /// exists to stop work going unseen.
    pub(crate) fn tier_counts_since(&self, cutoff: DateTime<Utc>) -> (usize, usize, usize) {
        let count = |entries: &Vec<ArtifactEntry>| {
            entries
                .iter()
                .filter(|entry| {
                    parse_artifact_timestamp(&entry.timestamp).is_none_or(|filed| filed >= cutoff)
                })
                .count()
        };
        (
            count(&self.open),
            count(&self.later),
            count(&self.feature_requests),
        )
    }

    /// The primary tier's newest entries as `(file, kind, heading-or-topic)`,
    /// for callers that surface a pointer rather than the whole queue.
    pub(crate) fn primary_preview(&self, limit: usize) -> Vec<(String, String, String)> {
        self.open
            .iter()
            .take(limit)
            .map(|entry| {
                (
                    entry.file.clone(),
                    entry.kind.clone().unwrap_or_default(),
                    entry.heading.clone().unwrap_or_else(|| entry.topic.clone()),
                )
            })
            .collect()
    }
}

/// The actionable journal queue, split into its three tiers. Shared by
/// `journal open` and by every view that surfaces the backlog beside
/// ledger state.
pub(crate) fn collect_open(ctx: &Ctx, kind: Option<&str>) -> Result<OpenItems> {
    let dir = resolve_dir(&ctx.cwd)?;
    collect_open_in(ctx, &dir, &ctx.cwd, kind)
}

/// The same queue for an explicitly named journal directory and project, so a
/// cross-project view can read a backlog it is not standing in.
pub(crate) fn collect_open_in(
    ctx: &Ctx,
    dir: &Path,
    project: &Path,
    kind: Option<&str>,
) -> Result<OpenItems> {
    if let Some(kind) = kind {
        if !is_actionable_kind(kind) {
            bail!(
                "--kind {} is not actionable; the open queue tracks {}",
                kind,
                PRIMARY_ACTIONABLE_KINDS
                    .iter()
                    .copied()
                    .chain(std::iter::once(LATER_KIND))
                    .chain(std::iter::once(FEATURE_REQUEST_KIND))
                    .collect::<Vec<_>>()
                    .join("|")
            );
        }
    }
    let dir = dir.to_path_buf();
    let mut open: Vec<ArtifactEntry> = Vec::new();
    let mut later: Vec<ArtifactEntry> = Vec::new();
    let mut feature_requests: Vec<ArtifactEntry> = Vec::new();
    let now = Utc::now();
    let journal = read_events(&dir)?;
    let lanes = lanes_from_journal(&journal, now);
    let changes = open_changes_for_annotation(project);
    let (_, caller_session) = identity(ctx);
    if dir.is_dir() {
        let mut open_names: Vec<String> = Vec::new();
        let mut later_names: Vec<String> = Vec::new();
        let mut feature_request_names: Vec<String> = Vec::new();
        for name in sorted_artifact_names(&dir)? {
            let Some((_, _, file_kind)) = parse_artifact_name(&name) else {
                continue;
            };
            let wanted = match kind {
                Some(kind) => file_kind == kind,
                None => is_actionable_kind(&file_kind),
            };
            if wanted && !is_consumed(&journal, &name) {
                if file_kind == LATER_KIND {
                    later_names.push(name);
                } else if file_kind == FEATURE_REQUEST_KIND {
                    feature_request_names.push(name);
                } else {
                    open_names.push(name);
                }
            }
        }
        for name in open_names {
            if let Some((ts, topic, file_kind)) = parse_artifact_name(&name) {
                let heading = first_heading(&dir.join(&name));
                let change = change_annotation(&changes, &topic, &name);
                open.push(ArtifactEntry {
                    lane: lane_for_topic(&lanes, &topic, &caller_session),
                    change,
                    age_seconds: if file_kind == JournalKind::Discussion.as_str() {
                        discussion_age_seconds(now, &ts, &name, &journal)
                    } else {
                        artifact_age_seconds(now, &ts)
                    },
                    file: name,
                    timestamp: ts,
                    topic,
                    kind: Some(file_kind),
                    heading,
                });
            }
        }
        for name in later_names {
            if let Some((ts, topic, file_kind)) = parse_artifact_name(&name) {
                let heading = first_heading(&dir.join(&name));
                let change = change_annotation(&changes, &topic, &name);
                later.push(ArtifactEntry {
                    lane: lane_for_topic(&lanes, &topic, &caller_session),
                    change,
                    age_seconds: artifact_age_seconds(now, &ts),
                    file: name,
                    timestamp: ts,
                    topic,
                    kind: Some(file_kind),
                    heading,
                });
            }
        }
        for name in feature_request_names {
            if let Some((ts, topic, file_kind)) = parse_artifact_name(&name) {
                let heading = first_heading(&dir.join(&name));
                let change = change_annotation(&changes, &topic, &name);
                feature_requests.push(ArtifactEntry {
                    lane: lane_for_topic(&lanes, &topic, &caller_session),
                    change,
                    age_seconds: artifact_age_seconds(now, &ts),
                    file: name,
                    timestamp: ts,
                    topic,
                    kind: Some(file_kind),
                    heading,
                });
            }
        }
    }

    Ok(OpenItems {
        dir: dir.display().to_string(),
        open,
        later,
        feature_requests,
    })
}

fn open(ctx: &Ctx, kind: Option<String>, json: bool) -> Result<i32> {
    let items = collect_open(ctx, kind.as_deref())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        println!("dir: {}", items.dir);
        println!("{OPEN_TIER_LEGEND}");
        println!("open items (newest first):");
        if items.open.is_empty() {
            println!("  (none)");
        }
        for f in &items.open {
            render_open_entry(f);
        }
        println!("later items (newest first):");
        if items.later.is_empty() {
            println!("  (none)");
        }
        for f in &items.later {
            render_open_entry(f);
        }
        println!("feature requests (newest first):");
        if items.feature_requests.is_empty() {
            println!("  (none)");
        }
        for f in &items.feature_requests {
            render_open_entry(f);
        }
    }
    Ok(0)
}

/// What the three tiers mean, printed above the queue so a reader who has
/// never seen the kind vocabulary can act on it. `open` carries work a future
/// session is expected to pick up; `later` and `feature-request` are parked
/// until someone chooses to spend on them.
const OPEN_TIER_LEGEND: &str =
    "tiers: open = todo|handoff|plan|discussion, plus artifacts of the retired \
inbox kind (work awaiting a session); later = parked; \
feature-request = unbuilt proposals. \
A discussion argues a proposal to a decision and collects positions; \
a feature-request is the unbuilt proposal itself. \
Take one up with `arc begin <slug> --from-journal <file>`.";

/// One `journal open` line: the creation stamp and its age, then topic, kind,
/// heading, and any change/lane annotations.
fn render_open_entry(f: &ArtifactEntry) {
    let age = f.age_seconds.map_or_else(String::new, |seconds| {
        format!(" ({} old)", format_age(seconds))
    });
    println!(
        "  {}{}  {}  {}  {}{}{}",
        f.timestamp,
        age,
        f.topic,
        f.kind.as_deref().unwrap_or(""),
        f.heading.as_deref().unwrap_or(""),
        render_change(f.change.as_ref()),
        render_artifact_lane(f.lane.as_ref())
    );
}

fn lane_for_topic(lanes: &[LaneEntry], topic: &str, caller_session: &str) -> Option<ArtifactLane> {
    lanes
        .iter()
        .find(|lane| {
            lane.state == "live"
                && (lane.topic == topic || lane.scope.iter().any(|item| item == topic))
        })
        .map(|lane| ArtifactLane {
            topic: lane.topic.clone(),
            owner_harness: lane.owner_harness.clone(),
            owner_session: lane.owner_session.clone(),
            this_session: lane.owner_session == caller_session,
        })
}

fn render_change(change: Option<&ChangeRef>) -> String {
    match change {
        Some(change) => format!(" [change {}: {}]", change.id, change.status),
        None => String::new(),
    }
}

fn render_artifact_lane(lane: Option<&ArtifactLane>) -> String {
    let Some(lane) = lane else {
        return String::new();
    };
    if lane.this_session {
        format!(" [lane: {} — this session]", lane.topic)
    } else {
        let short_session: String = lane.owner_session.chars().take(8).collect();
        format!(
            " [lane: {} — {} {}, external]",
            lane.topic, lane.owner_harness, short_session
        )
    }
}

#[derive(Serialize)]
struct ListEntry {
    file: String,
    timestamp: String,
    topic: String,
    kind: String,
    heading: Option<String>,
    consumed: Option<String>,
}

#[derive(Serialize)]
struct ListItems {
    dir: String,
    artifacts: Vec<ListEntry>,
}

/// The general artifact listing the kind-specific views (`open`, `memories`)
/// are special cases of: every artifact in the hot journal dir, newest first,
/// with its consumption state. Read-only; derives everything from filenames
/// and the event log.
fn list(ctx: &Ctx, kind: Option<String>, json: bool) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let events = read_events(&dir)?;
    let mut artifacts: Vec<ListEntry> = Vec::new();
    for name in sorted_artifact_names(&dir)? {
        let Some((ts, topic, file_kind)) = parse_artifact_name(&name) else {
            continue;
        };
        if let Some(kind) = kind.as_deref() {
            if file_kind != kind {
                continue;
            }
        }
        let heading = first_heading(&dir.join(&name));
        let consumed = consumption(&events, &name);
        artifacts.push(ListEntry {
            file: name,
            timestamp: ts,
            topic,
            kind: file_kind,
            heading,
            consumed,
        });
    }

    if json {
        let out = ListItems {
            dir: dir.display().to_string(),
            artifacts,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("dir: {}", dir.display());
        println!("artifacts (newest first):");
        if artifacts.is_empty() {
            println!("  (none)");
        }
        for entry in &artifacts {
            let heading = entry.heading.as_deref().unwrap_or("");
            let consumed = entry
                .consumed
                .as_deref()
                .map(|outcome| format!("  [consumed: {outcome}]"))
                .unwrap_or_default();
            println!(
                "  {}  {}  {}  {}{}",
                entry.timestamp, entry.topic, entry.kind, heading, consumed
            );
        }
    }
    Ok(0)
}

/// Print one artifact's raw Markdown body: the read side of `note`. Resolves
/// the hot journal dir first, then the cold sibling archive.
fn show(ctx: &Ctx, filename: &str) -> Result<i32> {
    if parse_artifact_name(filename).is_none() {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    }
    print!("{}", read_artifact_body(ctx, filename)?);
    Ok(0)
}

/// Read one artifact's raw body from the hot journal dir, then the cold
/// archive. For callers that thread an artifact's content elsewhere — `show`
/// prints it, `begin --from-journal` seeds a brief from it. Rejects path
/// separators so the argument stays a filename inside the journal dir.
pub fn read_artifact_body(ctx: &Ctx, filename: &str) -> Result<String> {
    if filename.contains(['/', '\\']) {
        bail!("artifact reference must be a filename inside the journal dir, not a path");
    }
    let hot = resolve_dir(&ctx.cwd)?;
    for dir in [hot.clone(), archive_dir(&hot)] {
        let path = dir.join(filename);
        if path.is_file() {
            return std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read {}", path.display()));
        }
    }
    bail!(
        "no such artifact {filename} in {} or its cold archive",
        hot.display()
    )
}

/// Validate that a filename identifies an existing plan in the hot journal or
/// its cold archive.
pub fn validate_plan_artifact(ctx: &Ctx, filename: &str) -> Result<()> {
    if filename.contains(['/', '\\']) {
        bail!("plan reference must be a journal artifact filename, not a path");
    }
    let Some((_, _, kind)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    if kind != "plan" {
        bail!("{filename:?} is a {kind} artifact, not a plan");
    }
    read_artifact_body(ctx, filename)?;
    Ok(())
}

#[derive(Default, Serialize)]
struct StanceTally {
    #[serde(rename = "for")]
    in_favor: usize,
    against: usize,
    amend: usize,
    other: usize,
    /// Position blocks stating no `Position:` line at all. They are the reason
    /// a tally can read as settled while counting nothing: without this, an
    /// undercount is shaped exactly like a real result.
    unstated: usize,
}

#[derive(Serialize)]
struct Resolution {
    outcome: String,
    resolver: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision: Option<String>,
    /// Whether the resolving session also authored a position — the norm is
    /// that a contested discussion is resolved by a non-author of the winning
    /// position, so this flag surfaces a resolver who argued a side.
    resolver_participated: bool,
}

/// One question on a discussion, with what has been argued under it.
///
/// `branches` counts positions per option rather than listing them, because
/// the position ids are already in `rounds`; what a reader needs here is
/// whether an option was explored at all. An option with no positions is a
/// branch nobody argued, which is the difference between a choice made between
/// two explored futures and one made between two labels.
#[derive(Serialize)]
struct DiscussionQuestion {
    id: String,
    placement: String,
    options: Vec<String>,
    branches: Vec<DiscussionBranch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    answered: Option<String>,
    /// An opening question is meant to settle a premise before anyone argues.
    /// Set when it is still open and positions exist anyway — the argument
    /// started without the premise it was supposed to rest on.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    argued_before_answered: bool,
}

#[derive(Serialize)]
struct DiscussionBranch {
    option: String,
    positions: usize,
}

#[derive(Serialize)]
struct DiscussionSummary {
    schema: &'static str,
    file: String,
    topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    age_seconds: Option<u64>,
    /// `### Position` headings in the file — every position, however added.
    positions: usize,
    stances: StanceTally,
    /// Distinct `<model via harness>` identities from typed `position` events.
    /// Hand-written positions that never ran `journal position` are not counted
    /// here (they still count toward `positions` and `stances`).
    participants: Vec<String>,
    /// Typed `position` events that named a `--ref`.
    reply_refs: usize,
    /// Position blocks in the file that no `position` event carries: written by
    /// hand, recorded before ids existed, or carrying an id the log never saw.
    /// They are real positions — counted in `positions` and `stances` — that
    /// nothing can reference and therefore nothing can report as answered.
    unplaced: usize,
    /// Positions the event log records whose ID has no recognized matching
    /// heading. The heading may be gone or malformed; only the mismatch is
    /// established. These positions still appear in `rounds` and `unanswered`.
    detached: usize,
    /// Questions posed on this discussion, oldest first, each with the branches
    /// argued under it and the answer if one has been given.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    questions: Vec<DiscussionQuestion>,
    rounds: Vec<DiscussionRound>,
    unanswered: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<Resolution>,
}

#[derive(Serialize)]
struct DiscussionRound {
    depth: usize,
    positions: Vec<String>,
    participants: Vec<String>,
}

/// The identity label a position or resolution carries: `<model> via <harness>`
/// when the model is known, else the bare harness.
fn event_identity_label(event: &JournalEvent) -> String {
    match event.model.as_deref().filter(|value| !value.is_empty()) {
        Some(model) => format!("{model} via {}", event.harness),
        None => event.harness.clone(),
    }
}

const MAX_DISCUSSION_DEPTH: usize = 256;

fn position_depth(
    start: usize,
    positions: &[&JournalEvent],
    positions_by_id: &HashMap<&str, usize>,
) -> usize {
    let mut current = start;
    let mut depth = 1;
    let mut visited = HashMap::new();

    loop {
        if let Some(entry_depth) = visited.insert(current, depth) {
            return entry_depth;
        }
        let Some(parent) = positions[current]
            .reference
            .as_deref()
            .and_then(|reference| positions_by_id.get(reference))
            .copied()
        else {
            return depth;
        };
        if depth >= MAX_DISCUSSION_DEPTH {
            return MAX_DISCUSSION_DEPTH;
        }
        depth += 1;
        current = parent;
    }
}

fn discussion_rounds(positions: &[&JournalEvent]) -> (Vec<DiscussionRound>, Vec<String>) {
    let positions_by_id: HashMap<&str, usize> = positions
        .iter()
        .enumerate()
        .filter_map(|(index, event)| event.position_id.as_deref().map(|id| (id, index)))
        .collect();
    let mut rounds: Vec<DiscussionRound> = Vec::new();

    for (index, event) in positions.iter().enumerate() {
        let Some(position_id) = event.position_id.as_ref() else {
            continue;
        };
        let depth = position_depth(index, positions, &positions_by_id);
        if !rounds.iter().any(|round| round.depth == depth) {
            rounds.push(DiscussionRound {
                depth,
                positions: Vec::new(),
                participants: Vec::new(),
            });
            rounds.sort_by_key(|round| round.depth);
        }
        let round = rounds
            .iter_mut()
            .find(|round| round.depth == depth)
            .expect("round was inserted");
        round.positions.push(position_id.clone());
        let participant = event_identity_label(event);
        if !round.participants.contains(&participant) {
            round.participants.push(participant);
        }
    }

    let answered: HashSet<&str> = positions
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            let reference = event.reference.as_deref()?;
            let target = positions_by_id.get(reference)?;
            (*target != index).then_some(reference)
        })
        .collect();
    let unanswered = positions
        .iter()
        .filter_map(|event| event.position_id.as_ref())
        .filter(|position_id| !answered.contains(position_id.as_str()))
        .cloned()
        .collect();

    (rounds, unanswered)
}

/// A thematic break, or the underline of a setext heading. Either one ends the
/// block above it, so a stance below belongs to the next section. Markdown
/// allows the marks to be spaced (`* * *`), so spacing is not what separates a
/// rule from prose.
fn is_horizontal_rule(trimmed: &str) -> bool {
    let marks: Vec<u8> = trimmed
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    marks.len() >= 3
        && matches!(marks[0], b'-' | b'=' | b'_' | b'*')
        && marks.iter().all(|b| *b == marks[0])
}

/// A fence marker: its character, the length of its run, and whether anything
/// follows it.
///
/// A fence closes only on its own character, only on a run at least as long as
/// the one that opened it, and only when nothing follows the run — an opener
/// may carry an info string, and a closer may not, so a `` ```rust `` line
/// inside a fence is content rather than its end.
fn fence_marker(trimmed: &str) -> Option<(u8, usize, bool)> {
    let first = trimmed.as_bytes().first().copied()?;
    if !matches!(first, b'`' | b'~') {
        return None;
    }
    let run = trimmed.bytes().take_while(|b| *b == first).count();
    let bare = trimmed[run..].trim().is_empty();
    (run >= 3).then_some((first, run, bare))
}

fn is_position_heading(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix("### Position") else {
        return false;
    };
    rest.is_empty() || rest.starts_with(char::is_whitespace)
}

/// The `pos-…` id a position heading carries, when it carries one. A heading
/// written by hand may carry none, and one written before ids existed carries
/// none either; both are headings the reply graph has no way to name.
fn position_heading_id(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("### Position")?.trim_start();
    // Take the whole word and require all of it to be an id, rather than
    // reading up to the first character that cannot be one. Stopping early
    // turns `pos-punct!` into `pos-punct`, which then matches a recorded
    // position the heading does not name — and a false match here hides an
    // unplaceable heading, which is the one thing this count exists to show.
    let token = rest.split_whitespace().next()?;
    let suffix = token.strip_prefix("pos-")?;
    (!suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| token.to_string())
}

/// Count position blocks, and the stance each one states.
///
/// A block's stance is the first non-blank line under its heading, which is
/// what the convention asks for and what every surface documents. A block whose
/// first line argues instead of voting is counted as `unstated`, so a tally
/// that undercounts says so instead of reading as a settled result — and a
/// `Position:` line anywhere else, including inside a fenced example, is prose.
fn position_structure(body: &str) -> (usize, StanceTally, Vec<Option<String>>) {
    let mut heading_ids: Vec<Option<String>> = Vec::new();
    let mut positions = 0;
    let mut tally = StanceTally::default();
    let mut open_block = false;
    let mut decided = false;
    let mut fence: Option<(u8, usize)> = None;
    // A block ends at the next heading, at a horizontal rule, or at the end of
    // the file.
    let close_block = |open_block: &mut bool, decided: bool, tally: &mut StanceTally| {
        if *open_block && !decided {
            tally.unstated += 1;
        }
        *open_block = false;
    };
    for line in body.lines() {
        // A fenced block quotes the conventions rather than exercising them —
        // the scaffold that teaches the stance line is itself such a quote.
        let trimmed = line.trim_start();
        match (fence, fence_marker(trimmed)) {
            (None, Some((marker, run, _))) => {
                fence = Some((marker, run));
                // A block that opens with a quotation has not opened with a
                // stance, whatever it says once the quotation ends.
                if open_block && !decided {
                    decided = true;
                    tally.unstated += 1;
                }
                continue;
            }
            (Some((marker, opened)), Some((closing, run, bare)))
                if marker == closing && run >= opened && bare =>
            {
                fence = None;
                continue;
            }
            (Some(_), _) => continue,
            (None, None) => {}
        }
        if is_horizontal_rule(trimmed) {
            close_block(&mut open_block, decided, &mut tally);
            continue;
        }
        if is_position_heading(line) {
            close_block(&mut open_block, decided, &mut tally);
            heading_ids.push(position_heading_id(line));
            positions += 1;
            open_block = true;
            decided = false;
            continue;
        }
        if trimmed.starts_with('#') {
            close_block(&mut open_block, decided, &mut tally);
            continue;
        }
        if !open_block || decided || trimmed.is_empty() {
            continue;
        }
        decided = true;
        let Some(rest) = trimmed.strip_prefix("Position:") else {
            // The block opens by arguing rather than voting.
            tally.unstated += 1;
            continue;
        };
        match rest
            .split_whitespace()
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("for") => tally.in_favor += 1,
            Some("against") => tally.against += 1,
            Some("amend") => tally.amend += 1,
            Some(_) => tally.other += 1,
            // `Position:` with nothing after it states no stance either.
            None => tally.unstated += 1,
        }
    }
    close_block(&mut open_block, decided, &mut tally);
    (positions, tally, heading_ids)
}

/// Derived summary of a discussion. Structural counts (positions, stances) come
/// from the artifact so hand-written positions are included; participation and
/// reply metrics come from the typed `position` events; resolution and the
/// resolver-participation flag come from the consumed event correlated against
/// those position events. Read-only.
fn discussion_summary(ctx: &Ctx, filename: &str, json: bool) -> Result<i32> {
    let Some((ts, topic, kind)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    if kind != JournalKind::Discussion.as_str() {
        bail!("{filename} is a {kind}, not a discussion");
    }
    let body = read_artifact_body(ctx, filename)?;
    let dir = resolve_dir(&ctx.cwd)?;
    let events = read_events(&dir)?;

    let (positions, stances, heading_ids) = position_structure(&body);

    // Typed position events for this file, in ledger order.
    let position_events: Vec<&JournalEvent> = events
        .iter()
        .filter(|event| {
            event.known() && event.event == "position" && event.file.as_deref() == Some(filename)
        })
        .collect();
    let mut participants: Vec<String> = Vec::new();
    for event in &position_events {
        let label = event_identity_label(event);
        if !participants.contains(&label) {
            participants.push(label);
        }
    }
    let reply_refs = position_events
        .iter()
        .filter(|event| event.reference.is_some())
        .count();
    let (rounds, unanswered) = discussion_rounds(&position_events);
    // The tally reads the file and the graph reads the event log, so they can
    // disagree about how many positions exist. The difference is the honest
    // denominator gap, and reporting it is what keeps `unanswered` from reading
    // as the whole answer when it is only the part with ids.
    // Questions, oldest first. A question is only ever posed once, so the
    // event log order is the order they were asked.
    let questions: Vec<DiscussionQuestion> = events
        .iter()
        .filter(|event| {
            event.known() && event.event == "question" && event.file.as_deref() == Some(filename)
        })
        .filter_map(|posed| {
            let id = posed.question_id.clone()?;
            let options = posed.options.clone().unwrap_or_default();
            let answered = events
                .iter()
                .find(|event| {
                    event.known()
                        && event.event == "answer"
                        && event.file.as_deref() == Some(filename)
                        && event.question_id.as_deref() == Some(id.as_str())
                })
                .and_then(|event| event.option.clone());
            let branches = options
                .iter()
                .map(|option| DiscussionBranch {
                    option: option.clone(),
                    positions: position_events
                        .iter()
                        .filter(|event| {
                            event.question_id.as_deref() == Some(id.as_str())
                                && event.option.as_deref() == Some(option.as_str())
                        })
                        .count(),
                })
                .collect();
            let argued_before_answered = posed.placement.as_deref() == Some("opening")
                && answered.is_none()
                && positions > 0;
            Some(DiscussionQuestion {
                id,
                placement: posed.placement.clone().unwrap_or_default(),
                options,
                branches,
                answered,
                argued_before_answered,
            })
        })
        .collect();

    // The file and the event log can disagree in both directions, and each
    // direction is a different fact. Compare the ids rather than the counts:
    // subtraction clamped one direction to zero and mislabelled the other.
    let recorded: HashSet<&str> = position_events
        .iter()
        .filter_map(|event| event.position_id.as_deref())
        .collect();
    let heading_id_set: HashSet<&str> = heading_ids.iter().filter_map(|id| id.as_deref()).collect();
    // A heading no event carries: written by hand with no id, written before
    // ids existed, or carrying an id the log never recorded. Whichever way, the
    // graph cannot name it, so nothing can answer it.
    let unplaced = heading_ids
        .iter()
        .filter(|id| match id {
            None => true,
            Some(id) => !recorded.contains(id.as_str()),
        })
        .count();
    // A recorded ID with no recognized matching heading. The heading may have
    // been removed or may still exist in malformed form; the ID sets prove the
    // mismatch, not its cause.
    let detached = recorded
        .iter()
        .filter(|id| !heading_id_set.contains(*id))
        .count();

    // Resolution: the newest consumed event for this file, if any. The resolver
    // participated when a position event shares its harness-native session.
    let resolution = events
        .iter()
        .rev()
        .find(|event| {
            event.known() && event.event == "consumed" && event.file.as_deref() == Some(filename)
        })
        .map(|event| Resolution {
            outcome: event.outcome.clone().unwrap_or_default(),
            resolver: event_identity_label(event),
            decision: event.decision.clone(),
            resolver_participated: position_events.iter().any(|position| {
                position.harness == event.harness && position.session == event.session
            }),
        });

    let summary = DiscussionSummary {
        schema: "journal-discussion/1",
        age_seconds: discussion_age_seconds(Utc::now(), &ts, filename, &events),
        file: filename.to_string(),
        topic,
        positions,
        stances,
        participants,
        reply_refs,
        unplaced,
        detached,
        questions,
        rounds,
        unanswered,
        resolution,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(0);
    }

    println!("discussion: {} (topic {})", summary.file, summary.topic);
    if let Some(age) = summary.age_seconds {
        println!("age: {} old", format_age(age));
    }
    println!(
        "positions: {} — for {}, against {}, amend {}, other {}",
        summary.positions,
        summary.stances.in_favor,
        summary.stances.against,
        summary.stances.amend,
        summary.stances.other
    );
    if summary.stances.unstated > 0 {
        println!(
            "unstated: {} position block{} state no stance, so the tally undercounts \
             (a position's first body line reads `Position: for | against | amend`)",
            summary.stances.unstated,
            if summary.stances.unstated == 1 {
                ""
            } else {
                "s"
            }
        );
    }
    let participants = if summary.participants.is_empty() {
        "(none via journal position)".to_string()
    } else {
        summary.participants.join(", ")
    };
    println!(
        "participants: {participants} ({} reply-ref{})",
        summary.reply_refs,
        if summary.reply_refs == 1 { "" } else { "s" }
    );
    for question in &summary.questions {
        let state = match &question.answered {
            Some(option) => format!("answered {option}"),
            None => "open".to_string(),
        };
        println!(
            "question {} ({}, {state}):",
            question.id, question.placement
        );
        for branch in &question.branches {
            println!(
                "  {}: {} position{}",
                branch.option,
                branch.positions,
                if branch.positions == 1 { "" } else { "s" }
            );
        }
        if question.argued_before_answered {
            println!(
                "  note: an opening question is meant to settle a premise before anyone \
                 argues, and positions were filed while this one was still open"
            );
        }
    }
    println!("rounds (same-depth positions could not have read each other):");
    for round in &summary.rounds {
        println!(
            "  round {}: {} — {}",
            round.depth,
            round.positions.join(", "),
            round.participants.join(", ")
        );
    }
    println!(
        "unanswered: {}",
        if summary.unanswered.is_empty() {
            "(none)".to_string()
        } else {
            summary.unanswered.join(", ")
        }
    );
    if summary.unplaced > 0 {
        println!(
            "unplaced: {} position{} in the file that no event carries, so \
             nothing can reference {} and nothing above counts {} as answered",
            summary.unplaced,
            if summary.unplaced == 1 { "" } else { "s" },
            if summary.unplaced == 1 { "it" } else { "them" },
            if summary.unplaced == 1 { "it" } else { "them" },
        );
    }
    if summary.detached > 0 {
        println!(
            "detached: {} recorded position{} with no recognized matching heading id, \
             counted in the rounds above; any unmatched text is reported as unplaced",
            summary.detached,
            if summary.detached == 1 { "" } else { "s" },
        );
    }
    match &summary.resolution {
        Some(resolution) => {
            println!(
                "resolution: {} by {} — resolver {}",
                resolution.outcome,
                resolution.resolver,
                if resolution.resolver_participated {
                    "also authored a position"
                } else {
                    "did not author a position"
                }
            );
            if let Some(decision) = &resolution.decision {
                println!("decision: {decision}");
            }
        }
        None => println!("resolution: open"),
    }
    Ok(0)
}

/// The journal house timestamp: RFC 3339 seconds in UTC with a `Z` suffix —
/// the exact spelling `JournalEvent.ts` uses, so an inline prose stamp and an
/// event-log stamp are lexically identical and cross-greppable. Dated inline
/// headings come from here: the tool computes `now`, an agent never authors
/// a clock value it could skip or fabricate.
fn stamp() -> Result<i32> {
    println!("{}", Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
    Ok(0)
}

fn consume(
    ctx: &Ctx,
    filename: &str,
    outcome: ConsumeOutcome,
    note: Option<&str>,
    decision: Option<&str>,
) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    if let Some(decision) = decision {
        if !matches!(outcome, ConsumeOutcome::Done) {
            bail!("--decision is valid only with --outcome done");
        }
        if decision.contains(['/', '\\']) {
            bail!("--decision takes an artifact filename, not a path");
        }
        let Some((_, _, kind)) = parse_artifact_name(decision) else {
            bail!("{decision:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
        };
        if kind != JournalKind::Decision.as_str() {
            bail!("{decision} is a {kind} artifact, not a decision");
        }
        if !dir.join(decision).is_file() && !archive_dir(&dir).join(decision).is_file() {
            bail!(
                "no such decision artifact {decision} in {} or its cold archive",
                dir.display()
            );
        }
    }
    if filename.contains(['/', '\\']) {
        bail!("consume takes an artifact filename inside the archive dir, not a path");
    }
    let Some((_, topic, _)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    if !dir.join(filename).is_file() {
        bail!("no such artifact {} in {}", filename, dir.display());
    }
    if is_consumed(&read_events(&dir)?, filename) {
        bail!("{filename} is already consumed (see the journal)");
    }
    let mut event = JournalEvent::base(ctx, Utc::now(), &topic, "consumed");
    event.file = Some(filename.to_string());
    event.outcome = Some(outcome.as_str().to_string());
    event.note = note.map(str::to_string);
    event.decision = decision.map(str::to_string);
    append_event(ctx, &dir, &event)?;
    println!("consumed: {filename} [{}]", outcome.as_str());
    Ok(0)
}

fn archive(
    ctx: &Ctx,
    filename: Option<&str>,
    consumed: bool,
    older_than_days: Option<u64>,
    note: Option<&str>,
) -> Result<i32> {
    let hot = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&hot)?;
    if consumed {
        let journal = read_events(&hot)?;
        let mut names = Vec::new();
        if hot.is_dir() {
            for entry in
                std::fs::read_dir(&hot).with_context(|| format!("cannot read {}", hot.display()))?
            {
                let name = entry?.file_name().to_string_lossy().to_string();
                let Some((timestamp, _, kind)) = parse_artifact_name(&name) else {
                    continue;
                };
                if (is_actionable_kind(&kind) || kind == "memory")
                    && is_consumed(&journal, &name)
                    && older_than_days.is_none_or(|days| timestamp_older_than(&timestamp, days))
                {
                    names.push(name);
                }
            }
        }
        names.sort();
        for name in names {
            archive_one(ctx, &hot, &name, note)?;
            println!("{name}");
        }
        return Ok(0);
    }

    let filename = filename.context("archive requires a filename or --consumed")?;
    archive_one(ctx, &hot, filename, note)?;
    println!("{filename}");
    Ok(0)
}

fn timestamp_older_than(timestamp: &str, days: u64) -> bool {
    let parsed = NaiveDateTime::parse_from_str(timestamp, "%Y%m%dT%H%M%SZ")
        .or_else(|_| NaiveDateTime::parse_from_str(timestamp, "%Y%m%dT%H%M%S"));
    let Ok(parsed) = parsed else {
        return false;
    };
    let timestamp = DateTime::<Utc>::from_naive_utc_and_offset(parsed, Utc);
    let Ok(days) = i64::try_from(days) else {
        return false;
    };
    timestamp < Utc::now() - chrono::Duration::days(days)
}

fn archive_one(ctx: &Ctx, hot: &Path, filename: &str, note: Option<&str>) -> Result<()> {
    if filename.contains(['/', '\\']) {
        bail!("archive takes an artifact filename inside the hot dir, not a path");
    }
    let Some((_, topic, kind)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    let source = hot.join(filename);
    if !source.is_file() {
        bail!("no such artifact {} in {}", filename, hot.display());
    }
    let consumed = is_consumed(&read_events(hot)?, filename);
    if is_actionable_kind(&kind) && !consumed {
        bail!("{filename} is actionable and must be consumed before it can be archived");
    }
    if kind == "memory" && !consumed {
        bail!("{filename} must be consumed before it can be archived");
    }

    let cold = archive_dir(hot);
    let destination = cold.join(filename);
    if destination.exists() {
        bail!(
            "archive destination already exists: {}",
            destination.display()
        );
    }
    std::fs::create_dir_all(&cold)
        .with_context(|| format!("cannot create archive dir {}", cold.display()))?;
    std::fs::rename(&source, &destination).with_context(|| {
        format!(
            "cannot move {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    let mut message = format!("archived {filename}");
    if let Some(note) = note {
        message.push_str(&format!(": {note}"));
    }
    append_journal(hot, ctx, Utc::now(), &topic, &message, None)?;
    Ok(())
}

fn journal_tail(dir: &Path, limit: usize) -> Result<Vec<String>> {
    let lines: Vec<String> = read_events(dir)?
        .iter()
        .filter(|event| event.known())
        .map(render_event)
        .collect();
    let start = lines.len().saturating_sub(limit);
    Ok(lines[start..].to_vec())
}

// --- Ledger bridge: begin --from-journal, lifecycle auto-log, annotation ---

/// Verify a journal artifact exists and is an open, unconsumed actionable
/// item suitable to open a change from. Errors otherwise.
pub fn require_open_actionable(ctx: &Ctx, filename: &str) -> Result<String> {
    if filename.contains(['/', '\\']) {
        bail!("--from-journal takes an artifact filename inside the journal dir, not a path");
    }
    let Some((_, _, kind)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    if !is_actionable_kind(&kind) {
        bail!(
            "{filename} is a {kind} artifact, not an actionable item ({}|{})",
            PRIMARY_ACTIONABLE_KINDS.join("|"),
            LATER_KIND
        );
    }
    let dir = resolve_dir(&ctx.cwd)?;
    if !dir.join(filename).is_file() {
        bail!("no such artifact {} in {}", filename, dir.display());
    }
    if is_consumed(&read_events(&dir)?, filename) {
        bail!("{filename} is already consumed (see the journal)");
    }
    Ok(kind)
}

/// Append a journal `consumed` event marking an artifact superseded by the
/// change opened from it. The artifact file itself is never edited.
pub fn consume_superseded_by_change(ctx: &Ctx, filename: &str, change_id: &str) -> Result<()> {
    let Some((_, topic, _)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name");
    };
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let message = format!("consumed {filename} [superseded]: change {change_id}");
    append_journal(&dir, ctx, Utc::now(), &topic, &message, None)
}

/// Best-effort lifecycle narration into the advisory journal. Does nothing
/// unless `[journal] auto_log` is set. A write failure is a warning, never a
/// propagated error: the authoritative ledger transition already succeeded.
pub fn auto_log(ctx: &Ctx, topic: &str, message: &str) {
    let enabled = config::load()
        .map(|cfg| cfg.journal_auto_log)
        .unwrap_or(false);
    if !enabled || !valid_topic(topic) {
        return;
    }
    if let Err(error) = try_auto_log(ctx, topic, message) {
        eprintln!("warning: journal auto-log failed: {error:#}");
    }
}

fn try_auto_log(ctx: &Ctx, topic: &str, message: &str) -> Result<()> {
    let dir = resolve_dir(&ctx.cwd)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create journal dir {}", dir.display()))?;
    append_journal(&dir, ctx, Utc::now(), topic, message, None)
}

/// Open changes in this repo, for annotating journal items. Empty on any
/// lookup failure (outside a repo, unreadable ledger): annotation is a
/// convenience layer that must never make `journal open` fail.
fn open_changes_for_annotation(cwd: &Path) -> Vec<ChangeState> {
    let Ok(store) = Store::discover(cwd) else {
        return Vec::new();
    };
    let Ok(ids) = store.list_change_ids() else {
        return Vec::new();
    };
    ids.into_iter()
        .filter_map(|id| store.load_events(&id).ok())
        .filter_map(|events| state::reduce(&events).ok())
        .filter(|state| !state.is_closed())
        .collect()
}

/// `[change <id>: <stage|state>]` for an item covered by an open change,
/// matched by topic-slug equality or an explicit `journal_ref`.
fn change_annotation(changes: &[ChangeState], topic: &str, filename: &str) -> Option<ChangeRef> {
    let change = changes
        .iter()
        .find(|state| state.slug == topic || state.journal_ref.as_deref() == Some(filename))?;
    let status = match change
        .claim
        .as_ref()
        .and_then(|claim| claim.progress.as_ref())
    {
        Some(progress) => format!("{:?}", progress.stage).to_lowercase(),
        None => "open".to_string(),
    };
    Some(ChangeRef {
        id: change.change_id.clone(),
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_validation() {
        assert!(valid_topic("delegation-blocker-ux"));
        assert!(valid_topic("m5"));
        assert!(valid_topic("plan-01"));
        assert!(!valid_topic(""));
        assert!(!valid_topic("-lead"));
        assert!(!valid_topic("lead-"));
        assert!(!valid_topic("a--b"));
        assert!(!valid_topic("Has-Caps"));
        assert!(!valid_topic("has space"));
        assert!(!valid_topic("has/slash"));
    }

    #[test]
    fn artifact_name_parsing() {
        assert_eq!(
            parse_artifact_name("20260717T062830Z-topic-note.md"),
            Some((
                "20260717T062830Z".to_string(),
                "topic".to_string(),
                "note".to_string()
            ))
        );
        // Legacy stamp without the trailing Z still parses.
        assert_eq!(
            parse_artifact_name("20260717T062830-topic-plan.md"),
            Some((
                "20260717T062830".to_string(),
                "topic".to_string(),
                "plan".to_string()
            ))
        );
        assert_eq!(
            parse_artifact_name("20260717T062830Z-topic-feature-request.md"),
            Some((
                "20260717T062830Z".to_string(),
                "topic".to_string(),
                "feature-request".to_string()
            ))
        );
        assert_eq!(
            parse_artifact_name("20260717T062830Z-topic-with-hyphens-feature-request.md"),
            Some((
                "20260717T062830Z".to_string(),
                "topic-with-hyphens".to_string(),
                "feature-request".to_string()
            ))
        );
        assert_eq!(
            parse_artifact_name("20260717T062830Z-topic-unknown-kind.md"),
            Some((
                "20260717T062830Z".to_string(),
                "topic-unknown".to_string(),
                "kind".to_string()
            ))
        );
        assert_eq!(parse_artifact_name("journal.md"), None);
        assert_eq!(parse_artifact_name("no-suffix"), None);
    }

    #[test]
    fn lane_marker_parsing_accepts_contract_shapes_and_rejects_malformed_markers() {
        assert_eq!(
            parse_lane_marker("lane opened [30m] scope=alpha,beta: working"),
            Some(LaneMarker::Opened {
                ttl: 1800,
                scope: vec!["alpha".to_string(), "beta".to_string()],
                status: Some("working".to_string()),
            })
        );
        assert_eq!(
            parse_lane_marker("lane opened"),
            Some(LaneMarker::Opened {
                ttl: DEFAULT_LANE_TTL,
                scope: vec![],
                status: None,
            })
        );
        assert_eq!(
            parse_lane_marker("lane renewed [1h]: still working"),
            Some(LaneMarker::Renewed {
                ttl: Some(3600),
                status: Some("still working".to_string()),
            })
        );
        assert_eq!(
            parse_lane_marker("lane renewed"),
            Some(LaneMarker::Renewed {
                ttl: None,
                status: None,
            })
        );
        assert_eq!(
            parse_lane_marker("lane closed [handoff]: passed on"),
            Some(LaneMarker::Closed {
                outcome: "handoff".to_string(),
                note: Some("passed on".to_string()),
            })
        );
        assert_eq!(
            parse_lane_marker("lane closed [done]"),
            Some(LaneMarker::Closed {
                outcome: "done".to_string(),
                note: None,
            })
        );
        assert_eq!(parse_lane_marker("quoted lane opened [2h]"), None);
        assert_eq!(parse_lane_marker("lane opened [0s]"), None);
        assert_eq!(parse_lane_marker("lane opened [2d]"), None);
        assert_eq!(parse_lane_marker("lane opened scope=Bad_Topic"), None);
    }
}

/// The journal half of `arc catchup`: live lanes, shared memories, and the
/// actionable queue in full. Rendered beside ledger state so one command
/// answers "what is waiting on this project" from both stores at once.
#[derive(Serialize)]
pub(crate) struct Orientation {
    lanes: Vec<LaneEntry>,
    memories: Vec<ArtifactEntry>,
    #[serde(flatten)]
    queue: OpenItems,
}

pub(crate) fn orientation(ctx: &Ctx) -> Result<Orientation> {
    let dir = resolve_dir(&ctx.cwd)?;
    let now = Utc::now();
    Ok(Orientation {
        lanes: lanes_from_journal(&read_events(&dir)?, now),
        memories: live_memories(&dir)?,
        queue: collect_open(ctx, None)?,
    })
}

impl Orientation {
    pub(crate) fn render(&self) {
        render_lanes(&self.lanes, Utc::now());
        render_memories(&self.memories);
        println!("journal: {}", self.queue.dir());
        println!("  {OPEN_TIER_LEGEND}");
        for (label, entries) in [
            ("open", &self.queue.open),
            ("later", &self.queue.later),
            ("feature-request", &self.queue.feature_requests),
        ] {
            println!("{label} ({}):", entries.len());
            if entries.is_empty() {
                println!("  (none)");
            }
            for entry in entries {
                render_open_entry(entry);
            }
        }
    }
}

#[cfg(test)]
mod heading_id_tests {
    use super::position_heading_id;

    /// The id is the whole word or nothing. Reading up to the first character
    /// that cannot be in an id would turn a malformed heading into a valid one,
    /// and a heading that falsely matches a recorded position hides the very
    /// disagreement `unplaced` exists to report.
    #[test]
    fn a_heading_id_is_the_whole_word_or_nothing() {
        assert_eq!(
            position_heading_id("### Position pos-01abc (m via h, t)").as_deref(),
            Some("pos-01abc")
        );
        assert_eq!(
            position_heading_id("### Position pos-manual").as_deref(),
            Some("pos-manual")
        );
        // No suffix is not an id, however much it looks like a prefix.
        assert_eq!(position_heading_id("### Position pos-"), None);
        // Trailing punctuation belongs to the word, so the word is not an id.
        assert_eq!(position_heading_id("### Position pos-punct!"), None);
        assert_eq!(position_heading_id("### Position pos-a.b"), None);
        // A heading with no id at all: the ordinary hand-written case.
        assert_eq!(position_heading_id("### Position (m via h, t)"), None);
        assert_eq!(position_heading_id("## Position pos-01abc"), None);
    }
}
