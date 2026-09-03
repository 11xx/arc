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
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const JOURNAL_LOCK_TIMEOUT: Duration = Duration::from_secs(1);
const JOURNAL_LOCK_RETRY: Duration = Duration::from_millis(10);
const JOURNAL_LOCK_DIR: &str = ".locks";
const JOURNAL_TRANSITION_LOCK: &str = "transition.lock";

/// Serialize journal transitions whose preflight depends on current state.
/// The lock file persists, while the OS releases ownership with this handle,
/// so a crashed writer cannot strand the journal behind a stale marker.
pub(crate) struct JournalTransitionLock(File);

impl Drop for JournalTransitionLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn lock_journal_transition(dir: &Path) -> Result<JournalTransitionLock> {
    let lock_dir = dir.join(JOURNAL_LOCK_DIR);
    std::fs::create_dir_all(&lock_dir)
        .with_context(|| format!("cannot create journal lock dir {}", lock_dir.display()))?;
    let path = lock_dir.join(JOURNAL_TRANSITION_LOCK);
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

pub(crate) fn lock_transition(ctx: &Ctx) -> Result<JournalTransitionLock> {
    lock_journal_transition(&resolve_dir(&ctx.cwd)?)
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
    Incident,
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
            JournalKind::Incident => "incident",
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

/// The stances a position can record when the tool writes its first body line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PositionStance {
    For,
    Against,
    Amend,
}

impl PositionStance {
    fn as_str(self) -> &'static str {
        match self {
            Self::For => "for",
            Self::Against => "against",
            Self::Amend => "amend",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "for" => Some(Self::For),
            "against" => Some(Self::Against),
            "amend" => Some(Self::Amend),
            _ => None,
        }
    }
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
    ///
    /// A lane's owner is the declared harness and session together, so both
    /// must be given. Opening replaces only that owner's own previous lane;
    /// another harness presenting the same session string keeps its lane.
    Open {
        /// Kebab-case topic slug naming the lane
        topic: String,
        /// Journal topics this lane covers, so `journal open` can annotate
        /// the items another session is already holding
        #[arg(long)]
        scope: Option<String>,
        /// How long the lane stays live without activity (e.g. 30m, 2h)
        #[arg(long, default_value = "2h")]
        ttl: String,
        /// What the lane is currently doing, shown beside it
        #[arg(long)]
        status: Option<String>,
        /// Read the lane status from a file ('-' for stdin)
        #[arg(long, conflicts_with = "status")]
        status_file: Option<String>,
    },
    /// Renew a lane owned by this harness and session
    Renew {
        /// The lane to renew
        topic: String,
        /// Replace the liveness window (defaults to the lane's current one)
        #[arg(long)]
        ttl: Option<String>,
        /// Replace what the lane is currently doing
        #[arg(long)]
        status: Option<String>,
        /// Read the lane status from a file ('-' for stdin)
        #[arg(long, conflicts_with = "status")]
        status_file: Option<String>,
    },
    /// Close a lane
    ///
    /// A live lane closes only for the owning harness and session; a stale
    /// one closes for anyone with `--outcome expired`.
    Close {
        /// The lane to close
        topic: String,
        /// How the lane ended
        #[arg(long, value_enum, default_value = "done")]
        outcome: LaneOutcome,
        /// Free-text detail recorded with the closure
        #[arg(long)]
        note: Option<String>,
    },
    /// List current live and stale lanes
    List {
        /// Emit the machine-readable JSON view instead of text
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

/// The arguments every kind-writing verb takes. `note --kind <k>` and the
/// kind's own subcommand are the same write; the subcommand exists so the
/// closed set is legible from `arc journal --help` rather than from a flag's
/// value enum.
#[derive(clap::Args)]
pub struct KindWrite {
    /// Kebab-case topic slug
    pub topic: String,
    /// Body source: a file path, or '-' for stdin (written verbatim)
    #[arg(long)]
    pub body_file: Option<String>,
    /// Optional title; when set, a `# <title>` heading is prepended
    #[arg(long)]
    pub title: Option<String>,
    /// Scaffold template prepended to the body (.arc/templates/<name>.md or a
    /// built-in: sol-low, sol-high, reviewer, discussion)
    #[arg(long, conflicts_with = "no_scaffold")]
    pub scaffold: Option<String>,
    /// Record the body alone, without the kind's default scaffold
    #[arg(long)]
    pub no_scaffold: bool,
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
        #[command(flatten)]
        write: KindWrite,
        /// Artifact kind (closed set). Defaults to `note`; every other kind
        /// has its own subcommand above, which is the same write said
        /// plainly. `discussion` is the exception and has none: a discussion
        /// is argued and read far more often than created, so its verbs are
        /// `position`, `question`, `answer` and the `discussion` summary, and
        /// `--kind discussion` stays the way to open one. It also brings the
        /// `discussion` scaffold unless told otherwise
        #[arg(long, value_enum, default_value = "note")]
        kind: JournalKind,
    },
    /// File a proposal nobody is building yet
    FeatureRequest {
        #[command(flatten)]
        write: KindWrite,
    },
    /// Record work waiting for a session
    Todo {
        #[command(flatten)]
        write: KindWrite,
    },
    /// Hand an unfinished thread to the next session
    Handoff {
        #[command(flatten)]
        write: KindWrite,
    },
    /// Record a plan before it becomes work
    Plan {
        #[command(flatten)]
        write: KindWrite,
    },
    /// Record what a piece of work concluded
    Conclusion {
        #[command(flatten)]
        write: KindWrite,
    },
    /// Record a settled decision, which is what resolves a discussion
    Decision {
        #[command(flatten)]
        write: KindWrite,
    },
    /// Record something that went wrong in the running of work, not in the
    /// code, so a decision's revisit trigger can fire on evidence
    Incident {
        #[command(flatten)]
        write: KindWrite,
    },
    /// Record a durable project fact, surfaced by `catchup` every session
    Memory {
        #[command(flatten)]
        write: KindWrite,
    },
    /// Park a proposal that is real but not now
    Later {
        #[command(flatten)]
        write: KindWrite,
    },
    /// Record a review performed outside the ledger
    Review {
        #[command(flatten)]
        write: KindWrite,
    },
    /// Append a log-only journal line (no artifact file is created)
    Log {
        /// Kebab-case topic slug
        topic: String,
        /// Free-text journal message
        message: String,
    },
    /// Add a position block to an artifact and emit a typed `position` event.
    /// `--stance` writes the `Position: for | against | amend` line above the
    /// body; without it, the body can provide that line itself.
    /// Use `arc journal position <file> --body-file - --question <id> --option
    /// <opt>` to argue under one branch of an open question instead of
    /// unconditionally
    Position {
        /// Artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// Position or item this answers: a position ID, legacy timestamp, or
        /// item slug. Quote the claim answered on the line below the stance
        #[arg(long = "ref")]
        reference: Option<String>,
        /// Body source: a file path, or '-' for stdin (the position argument,
        /// written verbatim below a tool-computed `### Position` heading).
        #[arg(long)]
        body_file: String,
        /// Stance to write above the body and record on the typed event. Omit
        /// this when the body already opens with its own `Position:` line.
        #[arg(long, value_enum)]
        stance: Option<PositionStance>,
        /// Argue under one option of an open question, rather than
        /// unconditionally. Pass the question ID; `--option` names the branch
        #[arg(long)]
        question: Option<String>,
        /// The option this position argues under; requires `--question`
        #[arg(long)]
        option: Option<String>,
    },
    /// Record that an artifact was checked against the project's source at the
    /// current anchor revision. The revision is omitted when the anchor has no
    /// Git head; the stamp appears on the open queue until the artifact is
    /// consumed or the anchor moves.
    Verified {
        /// Artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// Optional context for what the source check established
        #[arg(long)]
        note: Option<String>,
    },
    /// Pose a question on a discussion that only a person can settle, and emit
    /// a typed `question` event. Placement is the design: `opening` is answered
    /// before any position is filed, so everyone argues from the same premise;
    /// `closing` is answered once the argument is in. There is no mid-argument
    /// placement — a question that blocks halfway makes the caller watch a run
    /// they delegated. Argue a closing question on both sides first, with
    /// `arc journal position <file> --body-file - --question <id> --option
    /// <opt>`. Pose one with `arc journal question <file> --placement
    /// opening|closing --option A --option B --body-file -`
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
        /// Who may settle it: `person` (the default, what the prose always
        /// meant), `anyone`, or `delegate` with --delegate <name>. A question
        /// marks what this session should not settle alone, not that no
        /// model may answer — an operator who delegated the call names the
        /// delegate
        #[arg(long, value_parser = ["person", "anyone", "delegate"])]
        settle_by: Option<String>,
        /// Settle-by delegate, as the name allowed to answer (use with
        /// --settle-by delegate)
        #[arg(long, requires = "settle_by")]
        delegate: Option<String>,
    },
    /// List the scaffolds a write can prepend, and print one before using
    /// it. A journal artifact is append-only, so choosing between
    /// `--scaffold`, a kind's default, and `--no-scaffold` blind makes a
    /// wrong guess permanent
    Scaffolds {
        /// Print this scaffold's body instead of listing the names
        #[arg(long, value_name = "NAME")]
        show: Option<String>,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// Every open question, across the journal. arc records what agents cannot
    /// settle alone; it does not ask. This is the view an agent
    /// reads to raise the question through its own harness prompt — file, id,
    /// placement, the options to offer, and which branches were already
    /// argued — and `answer` is where the reply comes back. `--json` carries
    /// the same, for building the prompt without parsing text
    Questions {
        /// Emit structured JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Settle an open question by choosing one of its options, once. Branches
    /// that lost stay in the file: a branch argued and not taken is the only
    /// record that the alternative was explored rather than never considered.
    /// Use `arc journal answer <file> --question <id> --option <chosen>
    /// --body-file -`
    Answer {
        /// Artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// The question being settled
        #[arg(long)]
        question: String,
        /// The option chosen; must be one the question offered. A typo is
        /// refused rather than silently recorded as an answer nobody offered
        #[arg(long, required_unless_present = "other", conflicts_with = "other")]
        option: Option<String>,
        /// An answer none of the offered options expressed, in the answerer's
        /// own words. Recorded as settling the question and as evidence the
        /// option set was inadequate, which `arc journal questions` reports —
        /// a menu the answerer had to step outside of was framed wrong, and
        /// that is worth knowing before the next one is posed
        #[arg(long, value_name = "ANSWER")]
        other: Option<String>,
        /// Body source: a file path, or '-' for stdin (why, written verbatim
        /// below a tool-computed `### Answer` heading)
        #[arg(long)]
        body_file: String,
    },
    /// Replace one field of one recorded entry without rewriting it. The entry
    /// stays in the artifact as it was filed and a `### Correction` block
    /// records the replacement; derived views read the corrected value.
    /// `--target` names the entry: `artifact`, a position ID, a question ID,
    /// or `answer:<question id>`. A consumed artifact accepts corrections — it
    /// is closed to new work, but a wrong record stays wrong
    Correct {
        /// Artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// The entry to correct: `artifact`, a position ID, a question ID, or
        /// `answer:<question id>`
        #[arg(long)]
        target: String,
        /// The field to replace. Which fields exist depends on the target:
        /// `title` on the artifact; `stance`, `option`, `ref`, `actor`, and
        /// `model` on a position; `option`, `actor`, and `model` on an answer;
        /// `actor` and `model` on a question
        #[arg(long, value_parser = CORRECTABLE_FIELDS)]
        field: String,
        /// What the field should have said
        #[arg(long)]
        value: String,
        /// Why the recorded value was wrong
        #[arg(long)]
        note: Option<String>,
    },
    /// Withdraw a recorded entry while leaving it visible. A retracted position
    /// leaves the stance tally and the branches argued under a question; a
    /// retracted answer reopens the question it settled. The block stays in the
    /// artifact, because an argument withdrawn is a different record from an
    /// argument never made. A consumed artifact accepts retractions
    Retract {
        /// Artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// The entry to withdraw: a position ID or `answer:<question id>`
        #[arg(long)]
        target: String,
        /// Body source: a file path, or '-' for stdin (why it is withdrawn,
        /// written verbatim below a tool-computed `### Retraction` heading)
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
    /// Resolve the newest artifact filed under one topic and print it, so a
    /// workflow that appends continuations under a stable topic can read its
    /// own tail back without reproducing arc's ordering rules. The topic must
    /// match exactly; a similarly prefixed topic never matches. The hot
    /// journal wins over the cold archive whenever both hold a match
    Latest {
        /// Kebab-case topic slug, matched exactly
        topic: String,
        /// Restrict to one artifact kind, including kinds no longer written
        #[arg(long)]
        kind: Option<String>,
        /// Emit the resolved identity alongside the body as JSON
        #[arg(long)]
        json: bool,
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
        /// Artifact filename inside the journal dir (a name, not a path)
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
        /// Consume anyway, leaving an unanswered question undecided. Say in
        /// `--note` where the question went, or it goes nowhere
        #[arg(long)]
        drop_questions: bool,
    },
    /// Change a live artifact's workflow kind without rewriting history: a
    /// typed successor is written, linked back with `supersedes`, and the
    /// source is retired as superseded. The safe manual sequence, performed
    /// as one guarded operation rather than hand-composed by every caller.
    /// Promotion to a code change stays with `begin --from-journal`, which
    /// is implementation authorization and never a kind conversion.
    Transition {
        /// Artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// The successor's kind
        #[arg(long, value_enum)]
        to: JournalKind,
        /// Body source for the successor: a file path, or '-' for stdin.
        /// Omitted, the successor carries the source body verbatim under a
        /// heading naming what changed
        #[arg(long)]
        body_file: Option<String>,
        /// Optional title; when set, a `# <title>` heading is prepended
        #[arg(long)]
        title: Option<String>,
        /// Why the kind is changing, recorded in the transition event
        #[arg(long)]
        reason: Option<String>,
        /// Print the successor and the lifecycle effects without writing
        /// anything
        #[arg(long)]
        dry_run: bool,
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
        JournalCmd::Note { write, kind } => write_kind(ctx, kind, write),
        JournalCmd::FeatureRequest { write } => write_kind(ctx, JournalKind::FeatureRequest, write),
        JournalCmd::Todo { write } => write_kind(ctx, JournalKind::Todo, write),
        JournalCmd::Handoff { write } => write_kind(ctx, JournalKind::Handoff, write),
        JournalCmd::Plan { write } => write_kind(ctx, JournalKind::Plan, write),
        JournalCmd::Conclusion { write } => write_kind(ctx, JournalKind::Conclusion, write),
        JournalCmd::Decision { write } => write_kind(ctx, JournalKind::Decision, write),
        JournalCmd::Incident { write } => write_kind(ctx, JournalKind::Incident, write),
        JournalCmd::Memory { write } => write_kind(ctx, JournalKind::Memory, write),
        JournalCmd::Later { write } => write_kind(ctx, JournalKind::Later, write),
        JournalCmd::Review { write } => write_kind(ctx, JournalKind::Review, write),
        JournalCmd::Log { topic, message } => log_line(ctx, &topic, &message),
        JournalCmd::Position {
            filename,
            reference,
            body_file,
            stance,
            question,
            option,
        } => position(
            ctx,
            &filename,
            reference.as_deref(),
            &body_file,
            stance,
            question.as_deref(),
            option.as_deref(),
        ),
        JournalCmd::Verified { filename, note } => verified(ctx, &filename, note.as_deref()),
        JournalCmd::Question {
            filename,
            placement,
            options,
            body_file,
            settle_by,
            delegate,
        } => question(
            ctx,
            &filename,
            &placement,
            &options,
            &body_file,
            settle_by.as_deref(),
            delegate.as_deref(),
        ),
        JournalCmd::Answer {
            filename,
            question,
            option,
            other,
            body_file,
        } => answer(
            ctx,
            &filename,
            &question,
            option.as_deref(),
            other.as_deref(),
            &body_file,
        ),
        JournalCmd::Correct {
            filename,
            target,
            field,
            value,
            note,
        } => correct(ctx, &filename, &target, &field, &value, note.as_deref()),
        JournalCmd::Retract {
            filename,
            target,
            body_file,
        } => retract(ctx, &filename, &target, &body_file),
        JournalCmd::Scaffolds { show, json } => scaffolds(ctx, show.as_deref(), json),
        JournalCmd::Questions { json } => questions(ctx, json),
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
        JournalCmd::Latest { topic, kind, json } => latest(ctx, &topic, kind.as_deref(), json),
        JournalCmd::Discussion { filename, json } => discussion_summary(ctx, &filename, json),
        JournalCmd::Rebind { from } => rebind(ctx, &from),
        JournalCmd::Stamp => stamp(),
        JournalCmd::Lane { command } => lane(ctx, command),
        JournalCmd::Consume {
            filename,
            outcome,
            note,
            decision,
            drop_questions,
        } => consume(
            ctx,
            &filename,
            outcome,
            note.as_deref(),
            decision.as_deref(),
            drop_questions,
        ),
        JournalCmd::Transition {
            filename,
            to,
            body_file,
            title,
            reason,
            dry_run,
        } => transition(
            ctx,
            &filename,
            to,
            body_file.as_deref(),
            title.as_deref(),
            reason.as_deref(),
            dry_run,
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

/// Remove a target journal that holds only its binding and transition-lock
/// metadata, so the adopted journal can be renamed into its place.
fn clear_bound_target(target: &Path) -> Result<()> {
    let lock_dir = target.join(JOURNAL_LOCK_DIR);
    if lock_dir.exists() && !holds_only_transition_lock(&lock_dir)? {
        bail!(
            "cannot replace internal lock dir {} because it holds unexpected content",
            lock_dir.display()
        );
    }
    let transition_lock = lock_dir.join(JOURNAL_TRANSITION_LOCK);
    if transition_lock.is_file() {
        std::fs::remove_file(&transition_lock)
            .with_context(|| format!("cannot remove {}", transition_lock.display()))?;
    }
    if lock_dir.is_dir() {
        std::fs::remove_dir(&lock_dir)
            .with_context(|| format!("cannot replace internal lock dir {}", lock_dir.display()))?;
    }
    let stale = bindings_path(target);
    if stale.is_file() {
        std::fs::remove_file(&stale)
            .with_context(|| format!("cannot remove {}", stale.display()))?;
    }
    std::fs::remove_dir(target)
        .with_context(|| format!("cannot replace empty {}", target.display()))
}

fn holds_only_transition_lock(lock_dir: &Path) -> Result<bool> {
    if !lock_dir.is_dir() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(lock_dir)? {
        let entry = entry?;
        if entry.file_name() != JOURNAL_TRANSITION_LOCK || !entry.file_type()?.is_file() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether a journal holds anything a rebind could destroy by merging.
///
/// Bindings and transition locks are not history. A binding says which project
/// the directory belongs to, which is exactly what a rebind is about to
/// restate. The lock directory is arc-owned coordination metadata. Neither may
/// close the recovery path for a journal freshly created at a moved project's
/// new location.
fn holds_history(dir: &Path) -> Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "bindings.jsonl"
            || (name == JOURNAL_LOCK_DIR && holds_only_transition_lock(&entry.path())?)
        {
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
        // Only arc-owned binding and transition-lock metadata can be here —
        // `holds_history` refused anything else. Clearing it last keeps the
        // window in which the target is unbound as short as the move allows:
        // everything that can fail on its own has already succeeded, and only
        // the rename remains.
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
    if dir.is_dir() && holds_history(dir)? {
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

/// Check the three durable pieces of a kind transition against one another.
///
/// A transition is written as a relation event, a successor artifact, and a
/// superseded retirement event. The writes cannot share one filesystem
/// transaction, so every incomplete combination must remain visible to the
/// repair surface instead of looking like an ordinary queue item.
fn inspect_transition_integrity(
    dir: &Path,
    cold: &Path,
    hot_files: &[String],
    events: &[JournalEvent],
    problems: &mut Vec<DoctorFinding>,
) -> Result<()> {
    let mut relations = HashSet::new();
    let mut relation_counts = HashMap::new();
    for event in events.iter().filter(|event| event.event == "transition") {
        let (Some(successor), Some(source)) = (event.file.as_deref(), event.supersedes.as_deref())
        else {
            continue;
        };
        let key = (source.to_string(), successor.to_string());
        *relation_counts.entry(key.clone()).or_insert(0usize) += 1;
        relations.insert(key.clone());

        let mut issues = Vec::new();
        if !dir.join(source).is_file() && !cold.join(source).is_file() {
            issues.push("the source artifact is missing");
        }
        let successor_path = [dir.join(successor), cold.join(successor)]
            .into_iter()
            .find(|path| path.is_file());
        match successor_path {
            None => issues.push("the successor artifact is missing"),
            Some(path) if artifact_supersedes(&path).as_deref() != Some(source) => {
                issues.push("the successor does not carry the supersedes link")
            }
            Some(_) => {}
        }
        if !events.iter().any(|candidate| {
            candidate.event == "consumed"
                && candidate.file.as_deref() == Some(source)
                && candidate.outcome.as_deref() == Some(ConsumeOutcome::Superseded.as_str())
        }) {
            issues.push("the source has no superseded retirement event");
        }
        if relation_counts[&key] > 1 {
            issues.push("the relation event is duplicated");
        }
        if !issues.is_empty() {
            problems.push(DoctorFinding {
                code: "incomplete-transition",
                detail: format!("{source} -> {successor}: {}", issues.join("; ")),
            });
        }
    }

    let mut artifacts = Vec::new();
    for (storage, names) in [
        ("hot", hot_files.to_vec()),
        ("cold", sorted_artifact_names(cold)?),
    ] {
        for name in names {
            let path = if storage == "hot" {
                dir.join(&name)
            } else {
                cold.join(&name)
            };
            if let Some(source) = artifact_supersedes(&path) {
                artifacts.push((source, name, storage));
            }
        }
    }
    for (source, successor, storage) in artifacts {
        if !relations.contains(&(source.clone(), successor.clone())) {
            problems.push(DoctorFinding {
                code: "incomplete-transition",
                detail: format!(
                    "{storage} artifact {successor} supersedes {source} without a transition relation event"
                ),
            });
        }
    }
    Ok(())
}

/// A fork marker must carry an absolute worktree path. Resolving a relative
/// path against the caller would make the same repository name a different
/// checkout from different worktrees, and there is no recorded anchor that
/// can recover the old meaning safely.
fn inspect_fork_marker_paths(
    dir: &Path,
    hot_files: &[String],
    problems: &mut Vec<DoctorFinding>,
) -> Result<()> {
    for name in hot_files {
        let Some((_, topic, kind)) = parse_artifact_name(name) else {
            continue;
        };
        if kind != JournalKind::Plan.as_str() || !topic.starts_with("fork-") {
            continue;
        }
        let body = std::fs::read_to_string(dir.join(name))
            .with_context(|| format!("cannot read {}", dir.join(name).display()))?;
        let Some(recorded) = body
            .lines()
            .find_map(|line| line.strip_prefix("worktree: "))
        else {
            continue;
        };
        if !Path::new(recorded).is_absolute() {
            problems.push(DoctorFinding {
                code: "invalid-fork-marker",
                detail: format!(
                    "{name}: relative worktree path {recorded:?}; fork markers must record an absolute path"
                ),
            });
        }
    }
    Ok(())
}

/// Every `correction` and `retraction` recorded against one artifact.
fn amendment_events<'a>(events: &'a [JournalEvent], filename: &str) -> Vec<&'a JournalEvent> {
    events
        .iter()
        .filter(|event| {
            event.known()
                && matches!(event.event.as_str(), "correction" | "retraction")
                && event.file.as_deref() == Some(filename)
        })
        .collect()
}

/// Whether the entry an amendment names was recorded on its artifact.
///
/// A position is recorded either as a typed event or as a heading carrying its
/// ID, because the stance tally reads the artifact and the reply graph reads
/// the log; an amendment that either one can resolve is not dangling.
fn amend_target_exists(
    events: &[JournalEvent],
    filename: &str,
    target: &AmendTarget,
    body: Option<&str>,
) -> bool {
    let typed = |kind: &str, id: &str, on: fn(&JournalEvent) -> Option<&str>| {
        events.iter().any(|event| {
            event.known()
                && event.event == kind
                && event.file.as_deref() == Some(filename)
                && on(event) == Some(id)
        })
    };
    match target {
        AmendTarget::Artifact => true,
        AmendTarget::Position(id) => {
            typed("position", id, |event| event.position_id.as_deref())
                || body.is_some_and(|body| {
                    body.lines()
                        .filter_map(position_heading_id)
                        .any(|heading| &heading == id)
                })
        }
        AmendTarget::Question(id) => typed("question", id, |event| event.question_id.as_deref()),
        AmendTarget::Answer(id) => typed("answer", id, |event| event.question_id.as_deref()),
    }
}

/// Report amendments that cannot take effect.
///
/// An amendment naming an entry its artifact never recorded resolves to
/// nothing and is invisible in every derived view, which is exactly how a
/// correction that never took would otherwise be discovered. Two corrections
/// of one field of one entry sharing a timestamp leave which is in force
/// decided by log order alone, which is a weaker claim than the record makes.
fn inspect_amendments(
    dir: &Path,
    events: &[JournalEvent],
    problems: &mut Vec<DoctorFinding>,
    advice: &mut Vec<DoctorFinding>,
) {
    let mut files: Vec<&str> = Vec::new();
    for event in events.iter().filter(|event| {
        event.known() && matches!(event.event.as_str(), "correction" | "retraction")
    }) {
        if let Some(file) = event.file.as_deref() {
            if !files.contains(&file) {
                files.push(file);
            }
        }
    }
    for file in files {
        let body =
            artifact_body_path(dir, file).and_then(|path| std::fs::read_to_string(path).ok());
        for event in amendment_events(events, file) {
            let target = event
                .target
                .as_deref()
                .expect("a known amendment names a target");
            let parsed = event
                .amend_target()
                .expect("a known amendment names a resolvable target");
            if !amend_target_exists(events, file, &parsed, body.as_deref()) {
                problems.push(DoctorFinding {
                    code: "dangling-amendment-target",
                    detail: format!(
                        "{file}: {} names {target}, which it never recorded",
                        event.event
                    ),
                });
            }
        }
    }

    let mut stamps: HashMap<(&str, &str, &str, &str), usize> = HashMap::new();
    for event in events
        .iter()
        .filter(|event| event.known() && event.event == "correction")
    {
        let (Some(file), Some(target), Some(field)) = (
            event.file.as_deref(),
            event.target.as_deref(),
            event.field.as_deref(),
        ) else {
            continue;
        };
        *stamps
            .entry((file, target, field, event.ts.as_str()))
            .or_insert(0) += 1;
    }
    let mut ambiguous: Vec<String> = stamps
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|((file, target, field, ts), count)| {
            format!("{file}: {count} corrections of {field} on {target} at {ts}")
        })
        .collect();
    ambiguous.sort();
    for detail in ambiguous {
        advice.push(DoctorFinding {
            code: "ambiguous-correction",
            detail,
        });
    }
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
    for name in &hot_files {
        if !parse_artifact_name(name)
            .is_some_and(|(_, _, kind)| kind == JournalKind::Discussion.as_str())
        {
            continue;
        }
        let body = std::fs::read_to_string(dir.join(name))
            .with_context(|| format!("cannot read {}", dir.join(name).display()))?;
        let (questions, answers) = unrecorded_question_blocks(&body, &events, name);
        if !questions.is_empty() {
            problems.push(DoctorFinding {
                code: "unrecorded-question-block",
                detail: format!("{name}: {}", questions.join(", ")),
            });
        }
        if !answers.is_empty() {
            problems.push(DoctorFinding {
                code: "unrecorded-answer-block",
                detail: format!("{name}: {}", answers.join(", ")),
            });
        }
    }
    inspect_transition_integrity(&dir, &cold, &hot_files, &events, &mut problems)?;
    inspect_fork_marker_paths(&dir, &hot_files, &mut problems)?;
    inspect_amendments(&dir, &events, &mut problems, &mut advice);

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
                "{} owned by {} idle {}",
                lane.topic,
                lane.owner,
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
    RecordedAnchor,
    SluggedPath,
}

impl ResolutionSource {
    fn as_str(self) -> &'static str {
        match self {
            ResolutionSource::Env => "env",
            ResolutionSource::ConfigPrefix => "config-prefix",
            ResolutionSource::Git => "git",
            ResolutionSource::RecordedAnchor => "recorded-anchor",
            ResolutionSource::SluggedPath => "slugged-path",
        }
    }
}

struct JournalResolution {
    directory: PathBuf,
    source: ResolutionSource,
    anchor: Option<PathBuf>,
}

/// Resolve the journal directory from an explicit directory, a configured
/// stable path scope, Git repository identity, or a recorded root-journal
/// binding.
pub fn resolve_dir(cwd: &Path) -> Result<PathBuf> {
    Ok(resolve(cwd)?.directory)
}

fn resolve(cwd: &Path) -> Result<JournalResolution> {
    if let Some(dir) = std::env::var_os("ARC_JOURNAL_DIR") {
        return Ok(JournalResolution {
            directory: anchor_journal_destination(cwd, PathBuf::from(dir))?,
            source: ResolutionSource::Env,
            anchor: None,
        });
    }
    let cfg = config::load()?;
    let canonical_cwd = std::fs::canonicalize(cwd)
        .with_context(|| format!("cannot canonicalize journal cwd {}", cwd.display()))?;
    // A Git repository is one project even when it has several checkout
    // paths. Match configured journal prefixes against that shared root so a
    // prefix cannot send the primary and a linked worktree to different
    // journals. Non-repository directories retain their cwd-based scopes.
    let repository = repo_root(&canonical_cwd).ok();
    let prefix_path = repository.as_deref().unwrap_or(&canonical_cwd);
    let mut configured = None;
    for (raw_anchor, raw_directory) in &cfg.journal_dirs {
        let anchor_path = config::expand_tilde(raw_anchor)?;
        if !anchor_path.is_absolute() {
            bail!("journal path scope must be absolute: {raw_anchor:?}");
        }
        let Ok(anchor) = std::fs::canonicalize(&anchor_path) else {
            continue;
        };
        if !prefix_path.starts_with(&anchor) {
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
            directory: anchor_journal_destination(cwd, directory)?,
            source: ResolutionSource::ConfigPrefix,
            anchor: Some(anchor),
        });
    }
    if let Some(root) = repository {
        return Ok(JournalResolution {
            directory: cfg.ai_home.join("journals").join(config::path_slug(&root)),
            source: ResolutionSource::Git,
            anchor: Some(root),
        });
    }

    if let Some((directory, source)) = rootless_journal(&cfg, &canonical_cwd)? {
        return Ok(JournalResolution {
            directory,
            source,
            anchor: Some(canonical_cwd),
        });
    }

    bail!(
        "cannot resolve a stable journal anchor from {}: checked ARC_JOURNAL_DIR \
         (environment), [journals.dirs] path prefixes (config), Git discovery, recorded \
         anchors, and the journal this path would slug to; no source matched; set ARC_JOURNAL_DIR or add an absolute path-prefix entry \
         to {}; a path-prefix entry covering a directory with repositories beneath it will \
         shadow their Git discovery",
        canonical_cwd.display(),
        cfg.config_path.display()
    )
}

/// The journal for a directory Git and config cannot anchor, in order of how
/// much the answer is stated rather than derived.
///
/// A binding is the journal's own statement of which project it belongs to,
/// so it is consulted first and an ambiguous one is refused. Failing that, the
/// journal named by slugging this very directory is the one arc would itself
/// create here, which is a forward computation rather than a guess — but only
/// while that journal states nothing about who owns it. A journal that names
/// some other anchor has already answered the question, and answered it `no`.
///
/// `unslug`'s reverse direction is deliberately absent: reversing a lossy slug
/// into a path by walking the filesystem is an inference, and a resolver
/// acting on one could open another project's journal.
fn rootless_journal(
    cfg: &config::Config,
    canonical_cwd: &Path,
) -> Result<Option<(PathBuf, ResolutionSource)>> {
    if let Some(directory) = recorded_anchor_journal(cfg, canonical_cwd)? {
        return Ok(Some((directory, ResolutionSource::RecordedAnchor)));
    }
    let slugged = crate::registry::journals_root(cfg).join(config::path_slug(canonical_cwd));
    if slugged.is_dir() && recorded_anchor(&slugged)?.is_none() {
        return Ok(Some((slugged, ResolutionSource::SluggedPath)));
    }
    Ok(None)
}

/// Find the one root journal whose binding names `canonical_cwd`.
///
/// More than one matching statement is unsafe to resolve.
fn recorded_anchor_journal(cfg: &config::Config, canonical_cwd: &Path) -> Result<Option<PathBuf>> {
    let mut matches = Vec::new();
    for (_, directory) in crate::registry::journal_directories(cfg)? {
        let Some(recorded) = recorded_anchor(&directory)? else {
            continue;
        };
        if Path::new(&recorded) == canonical_cwd {
            matches.push(directory);
        }
    }

    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => {
            let journals = matches
                .iter()
                .map(|directory| directory.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "cannot resolve journal for {}: multiple journals record this anchor: {}",
                canonical_cwd.display(),
                journals
            )
        }
    }
}

/// The main repository root, shared by every worktree. Keying the archive
/// off this (never a worktree path) means two worktrees of one repo always
/// resolve to the same directory.
/// Anchor a journal destination so every checkout of one repository reads the
/// same directory. A relative destination — from `ARC_JOURNAL_DIR` or a
/// configured scope — would otherwise be joined to whatever the caller's cwd
/// happens to be, which is the one input that can still send the primary and
/// a linked worktree to different journals. Outside a repository there is no
/// shared root, so the cwd is the only anchor available.
fn anchor_journal_destination(cwd: &Path, directory: PathBuf) -> Result<PathBuf> {
    if directory.is_absolute() {
        return Ok(directory);
    }
    let base = match std::fs::canonicalize(cwd) {
        Ok(canonical) => repo_root(&canonical).unwrap_or(canonical),
        Err(_) => cwd.to_path_buf(),
    };
    Ok(base.join(directory))
}

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

/// The acting identity when somebody declared one.
///
/// A fallback actor is `git config user.name` — the identity of whoever
/// configured the checkout, not a claim that they acted — so it is left out
/// rather than recorded as a person who did something.
fn declared_actor(ctx: &Ctx) -> Option<String> {
    ctx.actor_source
        .declared()
        .then(|| ctx.actor.trim().to_string())
        .filter(|actor| !actor.is_empty())
}

/// How an appended block names who wrote it.
///
/// A model is the primary attribution wherever one is known, because the
/// record exists to say which model argued what. Without one, a declared actor
/// is a person acting directly and is named as such; with neither, only the
/// harness is known.
fn attribution(ctx: &Ctx, harness: &str) -> String {
    match ctx.model.as_deref().filter(|value| !value.is_empty()) {
        Some(model) => format!("{model} via {harness}"),
        None => match declared_actor(ctx) {
            Some(actor) => actor,
            None => harness.to_string(),
        },
    }
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

fn position_stance_text(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("Position:")
}

fn opening_position_stance(body: &str) -> Option<String> {
    let line = body.lines().find(|line| !line.trim().is_empty())?;
    let rest = position_stance_text(line)?;
    Some(
        rest.split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase(),
    )
}

/// The scaffold a kind carries by default, for kinds whose conventions live in
/// a template rather than in the reader's head.
fn default_scaffold(kind: JournalKind) -> Option<&'static str> {
    match kind {
        JournalKind::Discussion => Some("discussion"),
        _ => None,
    }
}

/// One write, whichever verb named the kind.
pub(crate) fn write_kind(ctx: &Ctx, kind: JournalKind, write: KindWrite) -> Result<i32> {
    note(
        ctx,
        &write.topic,
        kind,
        write.body_file.as_deref(),
        write.title.as_deref(),
        write.scaffold.as_deref(),
        write.no_scaffold,
    )
}

/// File a feature request: the `arc fr` alias for
/// `arc journal feature-request`. The alias exists because a proposal nobody
/// can find the verb for is a proposal that gets written into a transcript
/// instead of the journal. It forwards strictly, so the contract has one
/// owner and the alias can never drift from it.
pub fn feature_request(ctx: &Ctx, write: KindWrite) -> Result<i32> {
    write_kind(ctx, JournalKind::FeatureRequest, write)
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
    stance: Option<PositionStance>,
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
    // Refuse a malformed name or a branch on a non-discussion before the body
    // is read: with `--body-file -` the read waits on stdin, and a caller who
    // mistyped the filename would wait with it rather than being told.
    let (_, kind) = check_artifact_name(filename)?;
    if branch.is_some() && kind != JournalKind::Discussion.as_str() {
        bail!("{filename} is a {kind}, not a discussion");
    }
    // Read the body before touching the filesystem so a bad source path leaves
    // the artifact untouched.
    let body = read_body_verbatim(body_file)?;
    if let Some(requested) = stance {
        if let Some(body_stance) = opening_position_stance(&body) {
            let body_label = if body_stance.is_empty() {
                "<empty>"
            } else {
                body_stance.as_str()
            };
            if body_stance == requested.as_str() {
                bail!(
                    "body already opens with stance {body_label}; --stance {} would emit a duplicate stance line",
                    requested.as_str()
                );
            }
            bail!(
                "body opens with stance {body_label}, but --stance {} requests a different stance",
                requested.as_str()
            );
        }
    }

    // Positions ride an open discussion in the hot directory; a cold archived
    // artifact is a closed record, not an append target.
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let (dir, path, topic, _kind) = open_artifact(ctx, filename)?;
    let existing = read_events(&dir)?;
    // A branch naming a question that was never posed, or an option it never
    // offered, is an orphan: it renders under nothing and silently drops out of
    // every branch count. Refuse it rather than record it.
    if let Some((question, option)) = &branch {
        let artifact = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let (_, unrecorded_answers) = unrecorded_question_blocks(&artifact, &existing, filename);
        if unrecorded_answers.iter().any(|id| id == question) {
            bail!(
                "{question} has a visible answer block with no typed event; repair the journal before extending its branches"
            );
        }
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
    let heading = format!(
        "### Position {position_id} ({}, {ts}){under}",
        attribution(ctx, &harness)
    );
    let position_body = match stance {
        Some(stance) => format!("Position: {}\n{body}", stance.as_str()),
        None => body,
    };
    let block = format!("\n{heading}\n\n{}\n", position_body.trim_end_matches('\n'));

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
    event.stance = stance.map(|stance| stance.as_str().to_string());
    if let Some((question, option)) = branch {
        event.question_id = Some(question);
        event.option = Some(option);
    }
    append_event(ctx, &dir, &event)?;
    println!("{}", path.display());
    Ok(0)
}

/// The syntactic half of the artifact preflight, separable because it costs
/// nothing and touches nothing: a caller who mistyped a name learns so before
/// a body is read from stdin.
fn check_artifact_name(filename: &str) -> Result<(String, String)> {
    if filename.contains(['/', '\\']) {
        bail!("journal takes an artifact filename inside the journal dir, not a path");
    }
    let Some((_, topic, kind)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    Ok((topic, kind))
}

/// Shared preflight for an operation on an open artifact: the filename is a
/// name, the artifact exists, and it has not been consumed. A consumed
/// artifact is a closed record, so a later operation must not edit its history.
fn open_artifact(ctx: &Ctx, filename: &str) -> Result<(PathBuf, PathBuf, String, String)> {
    let (topic, kind) = check_artifact_name(filename)?;
    let dir = resolve_dir(&ctx.cwd)?;
    let path = dir.join(filename);
    if !path.is_file() {
        bail!("no such artifact {} in {}", filename, dir.display());
    }
    if is_consumed(&read_events(&dir)?, filename) {
        bail!(
            "cannot append to consumed artifact {filename}; open a successor discussion, \
             or amend the record with `journal correct` or `journal retract`"
        );
    }
    Ok((dir, path, topic, kind))
}

/// Shared preflight for appending to an open discussion, including the
/// discussion-only kind check.
fn open_discussion(ctx: &Ctx, filename: &str) -> Result<(PathBuf, PathBuf, String)> {
    let (dir, path, topic, kind) = open_artifact(ctx, filename)?;
    if kind != JournalKind::Discussion.as_str() {
        bail!("{filename} is a {kind}, not a discussion");
    }
    Ok((dir, path, topic))
}

/// Open a discussion for settling a question that was already posed. A
/// consumed artifact is closed to new work, positions, and questions, but its
/// unanswered question remains a live obligation and may receive its one
/// settlement block.
///
/// The cold archive is searched after the hot journal, because consumption is
/// what makes an artifact archivable: an open question outlives its source,
/// and the source it outlives is exactly the kind that has already moved.
fn open_discussion_for_answer(ctx: &Ctx, filename: &str) -> Result<(PathBuf, PathBuf, String)> {
    let (topic, kind) = check_artifact_name(filename)?;
    if kind != JournalKind::Discussion.as_str() {
        bail!("{filename} is a {kind}, not a discussion");
    }
    let hot = resolve_dir(&ctx.cwd)?;
    // The events stay in the hot journal, so the answer is recorded there
    // whichever directory holds the body it is appended to.
    let Some(path) = artifact_body_path(&hot, filename) else {
        bail!("no such artifact {} in {}", filename, hot.display());
    };
    Ok((hot, path, topic))
}

/// Record that an open artifact was checked against the source the checker
/// was standing in. An unborn or otherwise headless checkout still gets the
/// verification fact; it simply has no revision to compare later.
///
/// The revision is the head of the checkout that was read. From the primary
/// checkout that is the project anchor. From a fork's worktree it is the
/// fork's own head, recorded with the scope that names it, because a fork
/// carries commits the anchor does not: stamping the anchor there would
/// credit the check to code nobody opened.
fn verified(ctx: &Ctx, filename: &str, note: Option<&str>) -> Result<i32> {
    let resolution = resolve(&ctx.cwd)?;
    let dir = resolution.directory.clone();
    let _transition = lock_journal_transition(&dir)?;
    let (dir, _path, topic, _kind) = open_artifact(ctx, filename)?;
    let fork = crate::commands::fork::current(&ctx.cwd).ok().flatten();
    let (verified_revision, verified_scope) = match &fork {
        Some(fork) => (
            gitio::head_if_present(&fork.worktree)?,
            Some(format!("fork:{}", fork.slug)),
        ),
        None => (
            match resolution.anchor.as_deref() {
                Some(anchor) => gitio::head_if_present(anchor)?,
                None => None,
            },
            None,
        ),
    };
    let mut event = JournalEvent::base(ctx, Utc::now(), &topic, "verified");
    event.file = Some(filename.to_string());
    event.verified_revision = verified_revision;
    event.verified_scope = verified_scope;
    event.note = note.map(str::to_string);
    append_event(ctx, &dir, &event)?;
    println!(
        "verified: {filename}{}{}",
        event
            .verified_revision
            .as_deref()
            .map(|revision| format!(" at {revision}"))
            .unwrap_or_default(),
        fork.map(|fork| format!(" in fork {}", fork.slug))
            .unwrap_or_default()
    );
    Ok(0)
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
    settle_by: Option<&str>,
    delegate: Option<&str>,
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
    let settle_by = match (settle_by, delegate) {
        (Some("person"), None) | (None, None) => None,
        (Some("anyone"), None) => Some("anyone".to_string()),
        (Some("delegate"), Some(name)) if name.trim().is_empty() => {
            bail!("--delegate <name> cannot be empty")
        }
        (Some("delegate"), Some(name)) => Some(format!("delegate:{}", name.trim())),
        (Some("delegate"), None) => {
            bail!("--settle-by delegate needs --delegate <name>: name who may answer")
        }
        (Some(other), _) if other != "person" && other != "anyone" => {
            bail!("unknown --settle-by value {other:?}; expected person, anyone, or delegate")
        }
        _ => bail!("--delegate is valid only with --settle-by delegate"),
    };
    // The heading carries the settle-by when it is not the default, so the
    // artifact itself answers "who may settle this" where the prose reads.
    let settle_label = match &settle_by {
        Some(value) if value != "person" => format!(" — settle by {value}"),
        _ => String::new(),
    };
    let heading = format!(
        "### Question {question_id} ({placement}, {ts}){settle_label} — {}",
        options.join(" | ")
    );
    append_block(&path, &heading, &body)?;

    let mut event = JournalEvent::base(ctx, now, &topic, "question");
    event.file = Some(filename.to_string());
    event.question_id = Some(question_id);
    event.placement = Some(placement.to_string());
    event.options = Some(options.to_vec());
    // Absent means the classic default: a person. Recording it explicitly
    // would rewrite the meaning of every event already on disk.
    event.settle_by = settle_by;
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
    option: Option<&str>,
    other: Option<&str>,
    body_file: &str,
) -> Result<i32> {
    let body = read_body_verbatim(body_file)?;
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let (dir, path, topic) = open_discussion_for_answer(ctx, filename)?;
    let events = read_events(&dir)?;
    let artifact = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let (_, unrecorded_answers) = unrecorded_question_blocks(&artifact, &events, filename);
    if unrecorded_answers.iter().any(|id| id == question_id) {
        bail!(
            "{question_id} has a visible answer block with no typed event; repair the journal before settling it again"
        );
    }

    let Some(posed) = events.iter().find(|event| {
        event.known()
            && event.event == "question"
            && event.file.as_deref() == Some(filename)
            && event.question_id.as_deref() == Some(question_id)
    }) else {
        bail!("no question {question_id} on {filename}");
    };
    let offered = posed.options.clone().unwrap_or_default();
    // An answerer who steps outside the menu is settling the question, not
    // failing to answer it — every harness prompt offers that path, and
    // refusing it would send the answer back through prose arc cannot read.
    // It stays a separate flag so a mistyped option cannot become one
    // silently: a typo would then look like a decision.
    let chosen = match (option, other) {
        (Some(option), None) => {
            if !offered.iter().any(|value| value == option) {
                bail!(
                    "{option:?} is not one of the options {question_id} offered ({}); \
                     pass --other to answer outside them",
                    offered.join(", ")
                );
            }
            Chosen::Offered(option)
        }
        (None, Some(other)) if other.trim().is_empty() => {
            bail!("--other must say what the answer is")
        }
        // It is interpolated into a single-line `### Answer` heading, so a
        // newline would let the answer impersonate the next block heading at
        // exactly the point a parser resumes. Reasoning belongs in the body,
        // which is verbatim and unconstrained.
        (None, Some(other)) if other.contains(['\n', '\r']) => {
            bail!("--other must be a single line; put the reasoning in --body-file")
        }
        (None, Some(other)) => Chosen::OffMenu(other.trim()),
        _ => bail!("pass exactly one of --option or --other"),
    };
    if events.iter().any(|event| {
        event.known()
            && event.event == "answer"
            && event.file.as_deref() == Some(filename)
            && event.question_id.as_deref() == Some(question_id)
    }) {
        bail!("{question_id} is already answered; open a successor question to revisit it");
    }
    let mut unargued: Vec<String> = Vec::new();
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
        // A warning, like every other thing this surface says about
        // participation. Enforcing it would measure the typed binding rather
        // than the arguing: a discussion whose positions argue every branch on
        // the merits without carrying the two flags would be refused, while a
        // thin position filed under each losing branch would satisfy it —
        // which is the choice-between-labels the requirement exists to
        // prevent. The answerer is told what was never argued and decides
        // whether that matters.
        if !missing.is_empty() {
            unargued = missing.iter().map(|option| option.to_string()).collect();
        }
    }

    let now = Utc::now();
    let ts = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let (harness, _) = identity(ctx);
    let who = attribution(ctx, &harness);
    let heading = match chosen {
        Chosen::Offered(option) => format!("### Answer {question_id} = {option} ({who}, {ts})"),
        Chosen::OffMenu(answer) => {
            format!("### Answer {question_id} = (none offered) {answer} ({who}, {ts})")
        }
    };
    append_block(&path, &heading, &body)?;

    let mut event = JournalEvent::base(ctx, now, &topic, "answer");
    event.file = Some(filename.to_string());
    event.question_id = Some(question_id.to_string());
    match chosen {
        Chosen::Offered(option) => event.option = Some(option.to_string()),
        Chosen::OffMenu(answer) => {
            event.option = Some(answer.to_string());
            event.off_menu = Some(true);
        }
    }
    append_event(ctx, &dir, &event)?;
    println!("{}", path.display());
    if !unargued.is_empty() {
        let branches = if unargued.len() == 1 {
            "one branch"
        } else {
            "branches"
        };
        println!(
            "warning: {question_id} was answered with {branches} never argued ({})",
            unargued.join(", ")
        );
    }
    Ok(0)
}

/// Shared preflight for amending an artifact's record.
///
/// Unlike an append, an amendment stays open for as long as the record is
/// read: a consumed artifact is closed to new work, but a wrong entry inside
/// it stays wrong. The cold archive is searched after the hot journal, because
/// consumption is what makes an artifact archivable and its events remain in
/// the hot journal whichever directory holds the body.
fn open_artifact_for_amendment(ctx: &Ctx, filename: &str) -> Result<(PathBuf, PathBuf, String)> {
    let (topic, _kind) = check_artifact_name(filename)?;
    let hot = resolve_dir(&ctx.cwd)?;
    let Some(path) = artifact_body_path(&hot, filename) else {
        bail!("no such artifact {} in {}", filename, hot.display());
    };
    Ok((hot, path, topic))
}

/// One line of replacement text. Every correctable field is a single-line
/// scalar, and a newline in one would let the value impersonate the next block
/// heading at exactly the point a reader resumes.
fn amendment_value(value: &str) -> Result<&str> {
    if value.contains(['\n', '\r']) {
        bail!("--value must be a single line; put the reasoning in --note");
    }
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("--value must say what the field should have said");
    }
    Ok(trimmed)
}

/// A corrected branch still has to name a branch the question offered, which
/// is what the position had to satisfy when it was filed. An answer is exempt:
/// settling a question outside its menu is allowed, so a corrected answer may
/// carry words the menu never did.
fn check_branch_correction(
    events: &[JournalEvent],
    filename: &str,
    target: &AmendTarget,
    value: &str,
) -> Result<()> {
    let AmendTarget::Position(id) = target else {
        return Ok(());
    };
    let question = position_events(events, filename)
        .into_iter()
        .find(|event| event.position_id.as_deref() == Some(id.as_str()))
        .and_then(|event| event.question_id.clone());
    let Some(question) = question else {
        bail!("{id} argues under no question, so it has no branch to correct");
    };
    let offered = events
        .iter()
        .find(|event| {
            event.known()
                && event.event == "question"
                && event.file.as_deref() == Some(filename)
                && event.question_id.as_deref() == Some(question.as_str())
        })
        .and_then(|event| event.options.clone())
        .unwrap_or_default();
    if !offered.iter().any(|option| option == value) {
        bail!(
            "{value:?} is not one of the options {question} offered ({})",
            offered.join(", ")
        );
    }
    Ok(())
}

/// Replace one field of one recorded entry.
///
/// The entry is never rewritten. A `### Correction` block records what the
/// field should have said, the typed event carries the same fact, and every
/// derived view resolves the field through the latest correction of it — so
/// the artifact still reads as the argument that happened, and the views still
/// read as what is true.
fn correct(
    ctx: &Ctx,
    filename: &str,
    target: &str,
    field: &str,
    value: &str,
    note: Option<&str>,
) -> Result<i32> {
    let Some(parsed) = AmendTarget::parse(target) else {
        bail!(
            "{target:?} is not an entry to correct: pass `artifact`, a position ID, \
             a question ID, or `answer:<question id>`"
        );
    };
    if !parsed.correctable_fields().contains(&field) {
        bail!(
            "{target} has no {field} to correct; it carries {}",
            parsed.correctable_fields().join(", ")
        );
    }
    let value = amendment_value(value)?;
    if field == "stance" && PositionStance::parse(value).is_none() {
        bail!("--field stance takes for, against, or amend, not {value:?}");
    }
    let note = note.map(str::trim).filter(|note| !note.is_empty());

    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let (dir, path, topic) = open_artifact_for_amendment(ctx, filename)?;
    let events = read_events(&dir)?;
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    if !amend_target_exists(&events, filename, &parsed, Some(&body)) {
        bail!("no {target} on {filename}");
    }
    if field == "option" {
        check_branch_correction(&events, filename, &parsed, value)?;
    }

    let now = Utc::now();
    let ts = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let (harness, _) = identity(ctx);
    let amendment_id = format!("cor-{}", ulid::Ulid::new().to_string().to_ascii_lowercase());
    let heading = format!(
        "### Correction {amendment_id} ({}, {ts})",
        attribution(ctx, &harness)
    );
    let mut block = format!("Target: {target}\nField: {field}\nValue: {value}\n");
    if let Some(note) = note {
        block.push_str(&format!("\n{note}\n"));
    }
    append_block(&path, &heading, &block)?;

    let mut event = JournalEvent::base(ctx, now, &topic, "correction");
    event.file = Some(filename.to_string());
    event.amendment_id = Some(amendment_id);
    event.target = Some(target.to_string());
    event.field = Some(field.to_string());
    event.value = Some(value.to_string());
    event.note = note.map(str::to_string);
    append_event(ctx, &dir, &event)?;
    println!("{}", path.display());
    Ok(0)
}

/// Withdraw a recorded entry while leaving it visible.
///
/// A retracted position leaves the stance tally and the branches argued under
/// a question, and a retracted answer reopens the question it settled. The
/// block stays where it was filed, because an argument withdrawn and an
/// argument never made are different records and only one of them shows that
/// somebody changed their mind.
fn retract(ctx: &Ctx, filename: &str, target: &str, body_file: &str) -> Result<i32> {
    let Some(parsed) = AmendTarget::parse(target).filter(AmendTarget::retractable) else {
        bail!(
            "{target:?} is not an entry to retract: pass a position ID or \
             `answer:<question id>`"
        );
    };
    // Read the body before touching the filesystem so a bad source path leaves
    // the artifact untouched, and refuse a malformed target before the read so
    // a caller passing `-` is told rather than left waiting on stdin.
    let body = read_body_verbatim(body_file)?;
    if body.trim().is_empty() {
        bail!("a retraction must say why the entry no longer holds");
    }

    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let (dir, path, topic) = open_artifact_for_amendment(ctx, filename)?;
    let events = read_events(&dir)?;
    let artifact = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    if !amend_target_exists(&events, filename, &parsed, Some(&artifact)) {
        bail!("no {target} on {filename}");
    }
    if events.iter().any(|event| {
        event.known()
            && event.event == "retraction"
            && event.file.as_deref() == Some(filename)
            && event.target.as_deref() == Some(target)
    }) {
        bail!("{target} is already retracted");
    }

    let now = Utc::now();
    let ts = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let (harness, _) = identity(ctx);
    let amendment_id = format!("ret-{}", ulid::Ulid::new().to_string().to_ascii_lowercase());
    let heading = format!(
        "### Retraction {amendment_id} ({}, {ts})",
        attribution(ctx, &harness)
    );
    append_block(&path, &heading, &format!("Target: {target}\n\n{body}"))?;

    let mut event = JournalEvent::base(ctx, now, &topic, "retraction");
    event.file = Some(filename.to_string());
    event.amendment_id = Some(amendment_id);
    event.target = Some(target.to_string());
    event.note = Some(body.trim().to_string());
    append_event(ctx, &dir, &event)?;
    println!("{}", path.display());
    Ok(0)
}

#[derive(Serialize)]
struct OpenQuestion {
    file: String,
    topic: String,
    question: String,
    placement: String,
    /// Who may settle it: `person` when absent (the classic default),
    /// `anyone`, or `delegate:<name>`. An agent reading this view knows
    /// whether it may answer or must prompt; a delegate knows the question
    /// is waiting on it specifically.
    #[serde(skip_serializing_if = "Option::is_none")]
    settle_by: Option<String>,
    /// The prose the question was posed with, so a prompt can show what is
    /// being asked without the caller opening the artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    heading: Option<String>,
    /// Who ran the command that posed it, when somebody declared an actor.
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
    /// The subject it was posed for, when the invocation represented one. A
    /// delegate reading this queue tells a question a lead posed for it from
    /// one the lead posed as itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    on_behalf_of: Option<String>,
    asked_at: String,
    /// The options to offer, each with how many positions argued that branch.
    /// A branch nobody argued is visible before the question is answered,
    /// which is the point of arguing them first.
    options: Vec<QuestionOption>,
}

#[derive(Serialize)]
struct QuestionOption {
    option: String,
    positions: usize,
}

#[derive(Serialize)]
struct OpenQuestions {
    schema: &'static str,
    dir: String,
    questions: Vec<OpenQuestion>,
}

/// Every unanswered question across the journal.
///
/// A question is the one thing arc holds that no model may settle, and until
/// now nothing listed them: an agent had to open each discussion to find one.
/// The signal that most needs a person was the only one with no queue.
fn questions(ctx: &Ctx, json: bool) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let open = open_questions(&dir)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&OpenQuestions {
                schema: "arc-journal-questions/1",
                dir: dir.display().to_string(),
                questions: open,
            })?
        );
        return Ok(0);
    }
    if open.is_empty() {
        println!("no question is awaiting settlement");
        return Ok(0);
    }
    println!("questions awaiting settlement ({}):", open.len());
    for question in &open {
        let settle_by = match &question.settle_by {
            None => "person".to_string(),
            Some(value) => value.clone(),
        };
        println!(
            "  {}  {}  {}  {}  settle by: {}",
            question.asked_at,
            question.topic,
            question.placement,
            question.heading.as_deref().unwrap_or(""),
            settle_by
        );
        println!("    {} in {}", question.question, question.file);
        for option in &question.options {
            println!("    - {} ({} argued)", option.option, option.positions);
        }
        println!(
            "    answer: arc journal answer {} --question {} --option <choice> --body-file -",
            question.file, question.question
        );
    }
    Ok(0)
}

/// The prose a question was posed with, read from its block in the artifact.
///
/// The artifact's first heading is the wrong thing to show: on a scaffolded
/// discussion it is whatever the scaffold opens with, and on any file with
/// several questions it is the same string for all of them. A prompt needs
/// what was actually asked.
/// The file holding an artifact's body, hot journal first and cold archive
/// second. Consumption is what makes an artifact archivable, so anything that
/// reads a body derived from the event stream must look in both: the events
/// outlive the move and the reader would otherwise see the artifact vanish.
fn artifact_body_path(hot: &Path, filename: &str) -> Option<PathBuf> {
    let path = hot.join(filename);
    if path.is_file() {
        return Some(path);
    }
    let archived = archive_dir(hot).join(filename);
    archived.is_file().then_some(archived)
}

fn question_text(path: &Path, question_id: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    // A fenced block quoting the conventions is prose, exactly as it is for
    // position headings; and the id must end at a boundary, or asking for
    // `q-a` would answer with `q-a1`'s prose and prompt somebody with the
    // wrong question.
    let mut fence: Option<(u8, usize)> = None;
    let mut lines = text.lines().skip_while(|line| {
        let trimmed = line.trim_start();
        match (fence, fence_marker(trimmed)) {
            (None, Some((marker, run, _))) => {
                fence = Some((marker, run));
                return true;
            }
            (Some((marker, opened)), Some((closing, run, bare)))
                if marker == closing && run >= opened && bare =>
            {
                fence = None;
                return true;
            }
            (Some(_), _) => return true,
            (None, None) => {}
        }
        !line.strip_prefix("### Question ").is_some_and(|rest| {
            rest.strip_prefix(question_id)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        })
    });
    lines.next()?;
    lines
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
}

/// List the scaffolds a write can prepend, or print one.
///
/// `--no-scaffold` implies kinds carry defaults and `--scaffold` names
/// built-ins, but neither said which exist or what any contains. A journal
/// artifact is append-only, so a caller choosing between them was guessing at
/// something it could not undo.
fn scaffolds(ctx: &Ctx, show: Option<&str>, json: bool) -> Result<i32> {
    if let Some(name) = show {
        let body = crate::commands::scaffold_resolve(ctx, name)?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema": "arc-journal-scaffolds/1",
                    "scaffold": name,
                    "body": body,
                }))?
            );
        } else {
            print!("{body}");
        }
        return Ok(0);
    }

    let defaults: Vec<(&str, &str)> = recognized_journal_kinds()
        .filter_map(|kind| {
            crate::commands::scaffold_default_for_kind(kind).map(|name| (kind, name))
        })
        .collect();
    let available = crate::commands::scaffolds_available(ctx);
    if json {
        let rows: Vec<serde_json::Value> = available
            .iter()
            .map(|(name, from_repo)| {
                serde_json::json!({
                    "name": name,
                    "source": if *from_repo { "repository" } else { "built-in" },
                    "purpose": crate::commands::SCAFFOLD_BUILT_IN
                        .iter()
                        .find(|(known, _)| known == name)
                        .map(|(_, purpose)| *purpose),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": "arc-journal-scaffolds/1",
                "scaffolds": rows,
                "kind_defaults": defaults
                    .iter()
                    .map(|(kind, name)| serde_json::json!({"kind": kind, "scaffold": name}))
                    .collect::<Vec<_>>(),
            }))?
        );
        return Ok(0);
    }

    println!("scaffolds (`--scaffold <name>` on any write):");
    for (name, from_repo) in &available {
        let purpose = crate::commands::SCAFFOLD_BUILT_IN
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, purpose)| *purpose)
            .unwrap_or("repository template");
        let source = if *from_repo {
            "  [.arc/templates, shadows any built-in of this name]"
        } else {
            ""
        };
        println!("  {name}  {purpose}{source}");
    }
    println!();
    if defaults.is_empty() {
        println!("no kind prepends one unless asked.");
    } else {
        println!("prepended unless `--no-scaffold`:");
        for (kind, name) in &defaults {
            println!("  --kind {kind}  {name}");
        }
        println!("  every other kind records the body alone.");
    }
    println!();
    println!("read one before writing: `arc journal scaffolds --show <name>`");
    Ok(0)
}

/// Every unanswered question in one journal, newest first.
fn open_questions(dir: &Path) -> Result<Vec<OpenQuestion>> {
    let events = read_events(dir)?;
    let answered: HashSet<(&str, &str)> = events
        .iter()
        .filter(|event| event.known() && event.event == "answer")
        .filter_map(|event| Some((event.file.as_deref()?, event.question_id.as_deref()?)))
        .collect();
    let mut open = Vec::new();
    for event in events
        .iter()
        .filter(|event| event.known() && event.event == "question")
    {
        let (Some(file), Some(question)) = (event.file.as_deref(), event.question_id.as_deref())
        else {
            continue;
        };
        if answered.contains(&(file, question)) {
            continue;
        }
        // The same `known()` gate the answered set and the question scan use.
        // This is the number an agent shows a person to say whether a branch
        // was argued, so a malformed event must not inflate it.
        let argued = |option: &str| {
            events
                .iter()
                .filter(|event| {
                    event.known()
                        && event.event == "position"
                        && event.file.as_deref() == Some(file)
                        && event.question_id.as_deref() == Some(question)
                        && event.option.as_deref() == Some(option)
                })
                .count()
        };
        open.push(OpenQuestion {
            heading: artifact_body_path(dir, file).and_then(|path| question_text(&path, question)),
            file: file.to_string(),
            topic: event.topic.clone(),
            question: question.to_string(),
            placement: event.placement.clone().unwrap_or_default(),
            settle_by: event.settle_by.clone(),
            actor: event.actor.clone(),
            on_behalf_of: event.on_behalf_of.clone(),
            asked_at: event.ts.clone(),
            options: event
                .options
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|option| QuestionOption {
                    positions: argued(&option),
                    option,
                })
                .collect(),
        });
    }
    open.sort_by(|left, right| right.asked_at.cmp(&left.asked_at));
    Ok(open)
}

/// Which side of the offered menu an answer came from.
#[derive(Clone, Copy)]
enum Chosen<'a> {
    Offered(&'a str),
    OffMenu(&'a str),
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
pub(crate) struct JournalEvent {
    schema: String,
    ts: String,
    harness: String,
    session: String,
    /// Who acted, when somebody declared it. A journal event records the
    /// identity it was given rather than choosing among them: a person acting
    /// directly has an actor and no model, and an event carrying neither says
    /// so by omission instead of naming a person nobody named. An actor arc
    /// fell back to is not a claim that anyone acted, so it is not recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
    /// The subject a delegated invocation was run for (`--on-behalf-of`),
    /// recorded beside the actor and never in place of one. A lead filing a
    /// note or a position for an executor is two facts — who ran the command,
    /// and whose work it records — and an event keeping only the first names
    /// the wrong participant to every reader after it. Absent means the
    /// invocation represented nobody: no subject is inferred from prose,
    /// model, or session.
    #[serde(skip_serializing_if = "Option::is_none")]
    on_behalf_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    topic: String,
    pub(crate) event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision: Option<String>,
    /// The project-anchor revision checked by a `verified` event. Optional so
    /// an event written before verification stamps existed, or on an unborn
    /// anchor, remains valid `journal-events/1` input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verified_revision: Option<String>,
    /// Which checkout `verified_revision` names, when it is not the project
    /// anchor: `fork:<slug>` for a check made inside a fork's worktree. The
    /// anchor's head and a fork's head are different code, and a stamp that
    /// did not say which one was read would credit the check to source
    /// nobody looked at. Absent means the anchor, which is what every stamp
    /// written without the field meant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verified_scope: Option<String>,
    /// The project whose journal holds the artifact named in `decision`,
    /// when it is not this one. Recorded as the registry slug, which is
    /// stable against a label that follows a directory rename.
    ///
    /// Optional, like the fields below it, so an events file written before
    /// cross-journal references existed remains valid `journal-events/1`
    /// input. The schema version marks what a reader must accept, and a
    /// reader that accepts everything it accepted before has not changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_project: Option<String>,
    /// The kind of the artifact named in `decision`, so a reader knows what
    /// resolved the work without opening another project's journal.
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_kind: Option<String>,
    /// A digest of the referenced artifact's bytes when it was cited, so a
    /// later reader can tell a resolution that still says what it said from
    /// one that has been rewritten underneath.
    #[serde(skip_serializing_if = "Option::is_none")]
    decision_digest: Option<String>,
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
    /// Stance explicitly written by `journal position --stance`. Optional so
    /// position events written before the flag remain valid `journal-events/1`
    /// input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stance: Option<String>,
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
    /// Typed link from a kind-transition successor to the artifact it
    /// supersedes. The successor's first line carries the same fact in
    /// prose; this is the machine-readable half, so a reader can resolve the
    /// relationship without parsing Markdown. Optional, so events written
    /// before transitions existed remain valid input.
    #[serde(skip_serializing_if = "Option::is_none")]
    supersedes: Option<String>,
    /// The answers a question offers, in the order it offered them.
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Vec<String>>,
    /// True on an `answer` whose text is not one of the options the question
    /// offered, so `option` carries the answerer's own words rather than a
    /// branch. Recorded because a menu somebody had to step outside of was
    /// framed wrong, and that is worth knowing before the next one is posed.
    #[serde(skip_serializing_if = "Option::is_none")]
    off_menu: Option<bool>,
    /// One of a question's options: the branch a `position` argues under, or
    /// the branch an `answer` chose.
    #[serde(skip_serializing_if = "Option::is_none")]
    option: Option<String>,
    /// Who may settle the question: absent (or `person`) means a person,
    /// `anyone` lets any session answer, `delegate:<name>` names the session
    /// or actor delegated the call. A question marks what this session
    /// should not settle alone; the prose that claimed no model may answer
    /// was a narrower rule than the mechanism ever enforced. Optional, so
    /// every question written before the field existed keeps its meaning.
    #[serde(skip_serializing_if = "Option::is_none")]
    settle_by: Option<String>,
    /// What a `correction` or `retraction` acts on, within the artifact named
    /// by `file`: `artifact`, a position ID, a question ID, or
    /// `answer:<question id>`. Both events are append-only amendments whose
    /// effect lives in derived views, so the entry they name is never
    /// rewritten and stays readable as it was filed.
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    /// The one field a `correction` replaces on its target. Which fields a
    /// target has is closed, so a correction naming a field its target does
    /// not carry is not a correction of anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    field: Option<String>,
    /// What a `correction` puts in place of the recorded field.
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    /// Stable ID of an amendment, carried by the block it wrote so the visible
    /// Markdown and the typed event name the same one. Optional, like every
    /// identifier a hand-written event may omit.
    #[serde(skip_serializing_if = "Option::is_none")]
    amendment_id: Option<String>,
}

/// Every field a correction may name, over all targets. Which of them a given
/// target actually carries is `AmendTarget::correctable_fields`.
const CORRECTABLE_FIELDS: [&str; 6] = ["stance", "option", "actor", "model", "ref", "title"];

/// What a `correction` or `retraction` names inside one artifact.
///
/// The vocabulary is closed because an amendment that cannot be resolved to a
/// recorded entry corrects nothing: `journal doctor` reports one whose target
/// is absent, and every derived view ignores it.
#[derive(Clone, PartialEq, Eq, Debug)]
enum AmendTarget {
    /// The artifact itself, rather than any entry inside it.
    Artifact,
    Position(String),
    Question(String),
    /// The settlement of the named question.
    Answer(String),
}

impl AmendTarget {
    fn parse(value: &str) -> Option<Self> {
        if value == "artifact" {
            return Some(Self::Artifact);
        }
        if let Some(question) = value.strip_prefix("answer:") {
            return valid_question_id(question).then(|| Self::Answer(question.to_string()));
        }
        if valid_question_id(value) {
            return Some(Self::Question(value.to_string()));
        }
        valid_position_id(value).then(|| Self::Position(value.to_string()))
    }

    /// The fields this target carries, and so the only ones a correction may
    /// replace on it. A title belongs to an artifact, a stance and a reply
    /// reference to a position, a chosen branch to a position or an answer;
    /// the identity a thing was recorded under belongs to every entry an
    /// author filed.
    fn correctable_fields(&self) -> &'static [&'static str] {
        match self {
            Self::Artifact => &["title"],
            Self::Position(_) => &["stance", "option", "actor", "model", "ref"],
            Self::Question(_) => &["actor", "model"],
            Self::Answer(_) => &["option", "actor", "model"],
        }
    }

    /// A retraction withdraws an argument or a settlement. An artifact is
    /// withdrawn by consuming it, and a question by answering it, so neither
    /// is a retraction target.
    fn retractable(&self) -> bool {
        matches!(self, Self::Position(_) | Self::Answer(_))
    }
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
            actor: declared_actor(ctx),
            on_behalf_of: ctx.on_behalf_of.clone(),
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
            verified_revision: None,
            verified_scope: None,
            decision_project: None,
            decision_kind: None,
            decision_digest: None,
            ttl_seconds: None,
            scope: None,
            status: None,
            supersedes: None,
            settle_by: None,
            position_id: None,
            stance: None,
            reference: None,
            question_id: None,
            placement: None,
            options: None,
            off_menu: None,
            option: None,
            target: None,
            field: None,
            value: None,
            amendment_id: None,
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
            "verified" => self.file.is_some(),
            "position" => {
                self.file.is_some()
                    && self
                        .stance
                        .as_deref()
                        .is_none_or(|stance| PositionStance::parse(stance).is_some())
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
                    && self.settle_by.as_deref().is_none_or(valid_settle_by)
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
            // An amendment names one entry and one replacement for it. Both
            // halves are required: a correction missing either corrects
            // nothing, and a retraction without a reason withdraws an argument
            // while hiding why it no longer holds.
            "correction" => {
                self.file.is_some()
                    && self
                        .value
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                    && match (self.amend_target(), self.field.as_deref()) {
                        (Some(target), Some(field)) => target.correctable_fields().contains(&field),
                        _ => false,
                    }
            }
            "retraction" => {
                self.file.is_some()
                    && self
                        .note
                        .as_deref()
                        .is_some_and(|note| !note.trim().is_empty())
                    && self
                        .amend_target()
                        .is_some_and(|target| target.retractable())
            }
            "lane-opened" => self.ttl_seconds.is_some() && self.scope.is_some(),
            "lane-renewed" => true,
            "lane-closed" => self
                .outcome
                .as_deref()
                .is_some_and(|value| ["done", "handoff", "abandoned", "expired"].contains(&value)),
            "transition" => {
                self.file.is_some()
                    && self
                        .supersedes
                        .as_deref()
                        .is_some_and(|source| parse_artifact_name(source).is_some())
                    && parse_artifact_name(self.file.as_deref().unwrap_or_default()).is_some()
                    && self.supersedes.as_deref() != self.file.as_deref()
            }
            _ => false,
        }
    }

    /// The entry this event amends, when it names one that can be resolved.
    fn amend_target(&self) -> Option<AmendTarget> {
        self.target.as_deref().and_then(AmendTarget::parse)
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

fn valid_position_id(value: &str) -> bool {
    value.strip_prefix("pos-").is_some_and(|suffix| {
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

fn valid_settle_by(value: &str) -> bool {
    value == "person"
        || value == "anyone"
        || value
            .strip_prefix("delegate:")
            .is_some_and(|name| !name.trim().is_empty())
}

/// One question's replay state. Branch coverage is not tracked here: whether
/// every option was argued is advice at answer time, not a condition on the
/// answer being real, so the reader must not discard an answer for lacking it.
struct QuestionProgress {
    options: Vec<String>,
    /// Options a position has argued under, so a repeated branch position does
    /// not count twice.
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
                // An off-menu answer carries the answerer's words rather than
                // a branch, so it is not checked against the options — being
                // outside them is the whole content of the event.
                let off_menu = event.off_menu.unwrap_or(false);
                if progress.answered
                    || (!off_menu && !progress.options.iter().any(|offered| offered == option))
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

pub(crate) fn read_events(dir: &Path) -> Result<Vec<JournalEvent>> {
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
        "verified" => format!(
            "verified {}{}{}{}",
            event.file.as_deref().unwrap_or_default(),
            event
                .verified_revision
                .as_deref()
                .map(|revision| format!(" at {revision}"))
                .unwrap_or_default(),
            event
                .verified_scope
                .as_deref()
                .and_then(|scope| scope.strip_prefix("fork:"))
                .map(|slug| format!(" in fork {slug}"))
                .unwrap_or_default(),
            event
                .note
                .as_deref()
                .map(|value| format!(": {value}"))
                .unwrap_or_default()
        ),
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
        "transition" => format!(
            "transitioned {} -> {}{}",
            event.supersedes.as_deref().unwrap_or_default(),
            event.file.as_deref().unwrap_or_default(),
            event
                .note
                .as_deref()
                .map(|value| format!(": {value}"))
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

/// Who holds a lane. Session strings are minted by each harness independently,
/// so two harnesses can present the same one; the harness is therefore part of
/// the identity rather than a label beside it. Activity, renewal, closure,
/// takeover, and the one-lane-per-owner rule all key on the pair.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct LaneOwner {
    #[serde(rename = "owner_harness")]
    pub(crate) harness: String,
    #[serde(rename = "owner_session")]
    pub(crate) session: String,
}

impl std::fmt::Display for LaneOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.harness, self.session)
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct LaneEntry {
    pub(crate) topic: String,
    #[serde(flatten)]
    pub(crate) owner: LaneOwner,
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
        owner: LaneOwner,
        opened_time: DateTime<Utc>,
        ttl_seconds: u64,
        scope: Vec<String>,
        status: Option<String>,
    }

    let mut last_activity: HashMap<LaneOwner, DateTime<Utc>> = HashMap::new();
    for event in events {
        if let Some(timestamp) = event.timestamp() {
            last_activity.insert(event_owner(event), timestamp);
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
                let owner = event_owner(event);
                active.retain(|_, lane| lane.owner != owner);
                active.insert(
                    event.topic.clone(),
                    ActiveLane {
                        topic: event.topic.clone(),
                        owner,
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
                .get(&lane.owner)
                .copied()
                .unwrap_or(lane.opened_time);
            let elapsed = now.signed_duration_since(activity).num_seconds().max(0) as u64;
            LaneEntry {
                topic: lane.topic,
                owner: lane.owner,
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

pub(crate) fn format_age(seconds: u64) -> String {
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
        println!("  {}  {}  {}{}", lane.topic, lane.owner, activity, scope);
        if let Some(status) = &lane.status {
            println!("    {status}");
        }
    }
}

/// The owner a recorded event belongs to.
fn event_owner(event: &JournalEvent) -> LaneOwner {
    LaneOwner {
        harness: event.harness.clone(),
        session: event.session.clone(),
    }
}

/// The caller's own lane identity. Both halves must be declared: an undeclared
/// harness would otherwise let a caller match a lane on its session string
/// alone, which is exactly the collision the owner pair exists to prevent.
fn require_lane_owner(ctx: &Ctx) -> Result<LaneOwner> {
    let (harness, session) = identity(ctx);
    if session == "unknown" {
        bail!("journal lane requires a session identity (--session or ARC_SESSION)");
    }
    if harness == "unknown" {
        bail!("journal lane requires a harness identity (--harness or ARC_HARNESS)");
    }
    Ok(LaneOwner { harness, session })
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
            let owner = require_lane_owner(ctx)?;
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
                    && lane.owner != owner
                    && (lane.topic == topic || lane.scope.contains(&topic))
            }) {
                eprintln!(
                    "warning: topic {topic} is covered by live lane {} owned by {}",
                    overlap.topic, overlap.owner
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
            let owner = require_lane_owner(ctx)?;
            let current = lanes
                .iter()
                .find(|lane| lane.topic == topic)
                .with_context(|| format!("lane {topic} does not exist or is already closed"))?;
            if current.owner != owner {
                bail!("lane {topic} is owned by {}", current.owner);
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
            let owner = require_lane_owner(ctx)?;
            let current = lanes
                .iter()
                .find(|lane| lane.topic == topic)
                .with_context(|| format!("lane {topic} does not exist or is already closed"))?;
            if current.owner != owner {
                let idle = now
                    .signed_duration_since(current.last_activity_time)
                    .num_seconds()
                    .max(0) as u64;
                if !matches!(outcome, LaneOutcome::Expired) || current.state != "stale" {
                    bail!(
                        "lane {topic} conflict: owner {}, caller {}, idle {}, ttl {}",
                        current.owner,
                        owner,
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

#[derive(Clone, Serialize)]
pub(crate) struct ArtifactEntry {
    pub(crate) file: String,
    pub(crate) timestamp: String,
    pub(crate) topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    pub(crate) heading: Option<String>,
    /// Seconds since the artifact's latest position event for discussions, or
    /// creation stamp for other kinds. Absent only if neither timestamp parses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) age_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) lane: Option<ArtifactLane>,
    /// The open change that has taken this item up, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) change: Option<ChangeRef>,
    /// The latest source check for this artifact, if one was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) verification: Option<VerificationStamp>,
}

#[derive(Clone, Serialize)]
pub(crate) struct VerificationStamp {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) revision: Option<String>,
    /// Which checkout `revision` names, when it is not the project anchor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<String>,
    pub(crate) timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) actor: Option<String>,
    /// The subject the check was recorded for, when the invocation
    /// represented one. It stands beside the actor rather than replacing it,
    /// so a queue row can say a lead stamped this for an executor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) on_behalf_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    pub(crate) harness: String,
    pub(crate) session: String,
    /// `Some(false)` is current, `Some(true)` is moved, and `None` is unknown.
    pub(crate) moved: Option<bool>,
}

#[derive(Clone, Serialize)]
pub(crate) struct ChangeRef {
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
                    lane.owner.harness,
                    lane.owner.session,
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
pub(crate) struct ArtifactLane {
    topic: String,
    #[serde(flatten)]
    owner: LaneOwner,
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
pub(crate) fn parse_artifact_name(name: &str) -> Option<(String, String, String)> {
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
                verification: None,
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
                    verification: None,
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

pub(crate) fn is_consumed(events: &[JournalEvent], filename: &str) -> bool {
    consumption(events, filename).is_some()
}

/// Who filed an artifact, as the event that filed it recorded them.
pub(crate) struct RecordedIdentity {
    pub(crate) harness: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) actor: Option<String>,
    pub(crate) model: Option<String>,
}

/// A harness or session arc could not read was recorded as the literal
/// `unknown`. That is an absence, and a reader is told so rather than shown a
/// word that reads like a name.
fn present(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value != "unknown").then(|| value.to_string())
}

/// The identity on the event that first named `filename` — the write that
/// created the artifact, not whatever touched it last.
pub(crate) fn recorded_identity(
    events: &[JournalEvent],
    filename: &str,
) -> Option<RecordedIdentity> {
    let event = events
        .iter()
        .find(|event| event.file.as_deref() == Some(filename))?;
    Some(RecordedIdentity {
        harness: present(&event.harness),
        session: present(&event.session),
        actor: event.actor.clone(),
        model: event.model.clone(),
    })
}

fn verification_stamp(
    events: &[JournalEvent],
    filename: &str,
    current_revision: Option<&str>,
) -> Option<VerificationStamp> {
    let event = events
        .iter()
        .rev()
        .find(|event| event.event == "verified" && event.file.as_deref() == Some(filename))?;
    // A revision read inside a fork is not on the anchor's line of history,
    // so the anchor having moved says nothing about whether the check still
    // holds. An unanswerable comparison is left unanswered.
    let moved = match (
        event.verified_scope.as_deref(),
        event.verified_revision.as_deref(),
        current_revision,
    ) {
        (None, Some(verified), Some(current)) => Some(verified != current),
        _ => None,
    };
    Some(VerificationStamp {
        revision: event.verified_revision.clone(),
        scope: event.verified_scope.clone(),
        timestamp: event.ts.clone(),
        actor: event.actor.clone(),
        on_behalf_of: event.on_behalf_of.clone(),
        model: event.model.clone(),
        harness: event.harness.clone(),
        session: event.session.clone(),
        moved,
    })
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

    /// Every actionable artifact, by tier, for a caller that surfaces the
    /// queue rather than its size.
    pub(crate) fn tiers(&self) -> (&[ArtifactEntry], &[ArtifactEntry], &[ArtifactEntry]) {
        (&self.open, &self.later, &self.feature_requests)
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
                .filter(|entry| artifact_is_since(entry, cutoff))
                .count()
        };
        (
            count(&self.open),
            count(&self.later),
            count(&self.feature_requests),
        )
    }

    /// The same three tiers restricted to artifacts filed at or after
    /// `cutoff`, by the same rule `tier_counts_since` counts by.
    pub(crate) fn tiers_since(
        &self,
        cutoff: DateTime<Utc>,
    ) -> (
        Vec<&ArtifactEntry>,
        Vec<&ArtifactEntry>,
        Vec<&ArtifactEntry>,
    ) {
        (
            self.open
                .iter()
                .filter(|entry| artifact_is_since(entry, cutoff))
                .collect(),
            self.later
                .iter()
                .filter(|entry| artifact_is_since(entry, cutoff))
                .collect(),
            self.feature_requests
                .iter()
                .filter(|entry| artifact_is_since(entry, cutoff))
                .collect(),
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

fn artifact_is_since(entry: &ArtifactEntry, cutoff: DateTime<Utc>) -> bool {
    parse_artifact_timestamp(&entry.timestamp).is_none_or(|filed| filed >= cutoff)
}

/// The actionable journal queue, split into its three tiers. Shared by
/// `journal open` and by every view that surfaces the backlog beside
/// ledger state.
pub(crate) fn collect_open(ctx: &Ctx, kind: Option<&str>) -> Result<OpenItems> {
    let resolution = resolve(&ctx.cwd)?;
    let project = resolution.anchor.unwrap_or_else(|| ctx.cwd.clone());
    collect_open_in(ctx, &resolution.directory, &project, kind)
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
    // Queue rendering is advisory. If a configured journal has no reachable
    // Git anchor, retain its stamp and leave the movement comparison unknown.
    let current_revision = if journal.iter().any(|event| event.event == "verified") {
        gitio::head_if_present(project).ok().flatten()
    } else {
        None
    };
    let changes = open_changes_for_annotation(project);
    let (caller_harness, caller_session) = identity(ctx);
    let caller = LaneOwner {
        harness: caller_harness,
        session: caller_session,
    };
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
                let verification = verification_stamp(&journal, &name, current_revision.as_deref());
                open.push(ArtifactEntry {
                    lane: lane_for_topic(&lanes, &topic, &caller),
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
                    verification,
                });
            }
        }
        for name in later_names {
            if let Some((ts, topic, file_kind)) = parse_artifact_name(&name) {
                let heading = first_heading(&dir.join(&name));
                let change = change_annotation(&changes, &topic, &name);
                let verification = verification_stamp(&journal, &name, current_revision.as_deref());
                later.push(ArtifactEntry {
                    lane: lane_for_topic(&lanes, &topic, &caller),
                    change,
                    age_seconds: artifact_age_seconds(now, &ts),
                    file: name,
                    timestamp: ts,
                    topic,
                    kind: Some(file_kind),
                    heading,
                    verification,
                });
            }
        }
        for name in feature_request_names {
            if let Some((ts, topic, file_kind)) = parse_artifact_name(&name) {
                let heading = first_heading(&dir.join(&name));
                let change = change_annotation(&changes, &topic, &name);
                let verification = verification_stamp(&journal, &name, current_revision.as_deref());
                feature_requests.push(ArtifactEntry {
                    lane: lane_for_topic(&lanes, &topic, &caller),
                    change,
                    age_seconds: artifact_age_seconds(now, &ts),
                    file: name,
                    timestamp: ts,
                    topic,
                    kind: Some(file_kind),
                    heading,
                    verification,
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
/// heading, and any change/lane/verification annotations. Shared with the
/// workspace backlog, so a queue reads the same whichever command printed it.
pub(crate) fn render_open_entry(f: &ArtifactEntry) {
    let age = f.age_seconds.map_or_else(String::new, |seconds| {
        format!(" ({} old)", format_age(seconds))
    });
    println!(
        "  {}{}  {}  {}  {}{}{}{}",
        f.timestamp,
        age,
        f.topic,
        f.kind.as_deref().unwrap_or(""),
        f.heading.as_deref().unwrap_or(""),
        render_change(f.change.as_ref()),
        render_artifact_lane(f.lane.as_ref()),
        render_verification(f.verification.as_ref())
    );
}

fn lane_for_topic(lanes: &[LaneEntry], topic: &str, caller: &LaneOwner) -> Option<ArtifactLane> {
    lanes
        .iter()
        .find(|lane| {
            lane.state == "live"
                && (lane.topic == topic || lane.scope.iter().any(|item| item == topic))
        })
        .map(|lane| ArtifactLane {
            topic: lane.topic.clone(),
            owner: lane.owner.clone(),
            this_session: lane.owner == *caller,
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
        let short_session: String = lane.owner.session.chars().take(8).collect();
        format!(
            " [lane: {} — {} {}, external]",
            lane.topic, lane.owner.harness, short_session
        )
    }
}

/// The stamp as a queue row reads it: who checked this, how long ago, and
/// whether the answer still holds. A row is scanned, not studied, so the
/// revision is abbreviated and the session is left to `--json` — a line long
/// enough to wrap costs more than the identifiers it carries are worth.
fn render_verification(verification: Option<&VerificationStamp>) -> String {
    let Some(verification) = verification else {
        return String::new();
    };
    let age = DateTime::parse_from_rfc3339(&verification.timestamp)
        .ok()
        .map(|timestamp| {
            Utc::now()
                .signed_duration_since(timestamp.with_timezone(&Utc))
                .num_seconds()
                .max(0) as u64
        })
        .map(format_age)
        .map(|age| format!(" {age} ago"))
        .unwrap_or_default();
    let checker = verification
        .actor
        .clone()
        .or_else(|| verification.model.clone())
        .unwrap_or_else(|| verification.harness.clone());
    // Where a check was made is part of what it claims: a fork's head is not
    // the anchor's, and the row says so instead of letting a reader assume
    // the revision is one they can look up on the project's line of history.
    if let Some(scope) = verification.scope.as_deref() {
        let scope = scope
            .strip_prefix("fork:")
            .map(|slug| format!("fork {slug}"));
        if let (Some(revision), Some(scope)) = (&verification.revision, scope) {
            return format!(
                " [verified at {} in {scope}{age} by {checker}]",
                short_revision(revision)
            );
        }
    }
    // Only a stamp that has stopped holding needs saying. A current one is
    // already what "verified" means, and an unrevisioned one cannot be
    // compared at all — the missing revision says so without a second word.
    match (&verification.revision, verification.moved) {
        (None, _) => format!(" [verified by {checker}{age}, no revision]"),
        (Some(revision), Some(false)) => format!(
            " [verified at {}{age} by {checker}]",
            short_revision(revision)
        ),
        (Some(revision), Some(true)) => format!(
            " [verified at {}{age} by {checker}; anchor moved since]",
            short_revision(revision)
        ),
        (Some(revision), None) => format!(
            " [verified at {}{age} by {checker}; anchor comparison unknown]",
            short_revision(revision)
        ),
    }
}

fn short_revision(revision: &str) -> &str {
    &revision[..revision.len().min(8)]
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
        // Somebody holding a topic rather than a filename is one command from
        // the answer, so the error names it. Teaching the grammar and stopping
        // leaves the caller to guess which verb takes the other shape.
        if valid_topic(filename) {
            bail!(
                "{filename:?} is a topic, not a journal artifact name\n\
                 tip: `arc journal latest {filename}` resolves its newest artifact, \
                 `arc journal list` browses them all"
            );
        }
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    }
    print!("{}", read_artifact_body(ctx, filename)?);
    Ok(0)
}

#[derive(Serialize)]
struct LatestArtifact {
    schema: &'static str,
    file: String,
    dir: String,
    storage: &'static str,
    timestamp: String,
    topic: String,
    kind: String,
    heading: Option<String>,
    consumed: Option<String>,
    body: String,
}

/// Resolve the newest artifact under one topic. Hot storage is searched
/// before the cold archive and a hot match wins outright: an archived
/// artifact is by definition older work, so a newer archived stamp would
/// still be the wrong answer for "where did this topic get to".
fn latest(ctx: &Ctx, topic: &str, kind: Option<&str>, json: bool) -> Result<i32> {
    let hot = resolve_dir(&ctx.cwd)?;
    let cold = archive_dir(&hot);
    let events = read_events(&hot)?;

    for (dir, storage) in [(&hot, "hot"), (&cold, "cold")] {
        let found = sorted_artifact_names(dir)?.into_iter().find_map(|name| {
            let (ts, file_topic, file_kind) = parse_artifact_name(&name)?;
            (file_topic == topic && kind.is_none_or(|kind| file_kind == kind))
                .then_some((name, ts, file_kind))
        });
        let Some((name, timestamp, file_kind)) = found else {
            continue;
        };
        let path = dir.join(&name);
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if !json {
            print!("{body}");
            return Ok(0);
        }
        let resolved = LatestArtifact {
            schema: "arc-journal-latest/1",
            heading: first_heading(&path),
            consumed: consumption(&events, &name),
            file: name,
            dir: dir.display().to_string(),
            storage,
            timestamp,
            topic: topic.to_string(),
            kind: file_kind,
            body,
        };
        println!("{}", serde_json::to_string_pretty(&resolved)?);
        return Ok(0);
    }

    match kind {
        Some(kind) => bail!(
            "no {kind} artifact under topic {topic:?} in {} or its cold archive",
            hot.display()
        ),
        None => bail!(
            "no artifact under topic {topic:?} in {} or its cold archive",
            hot.display()
        ),
    }
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
    answered: Option<DiscussionAnswer>,
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

/// The branch a question settled on, and who settled it.
///
/// `actor` is whoever ran the command; `on_behalf_of` is the subject they ran
/// it for. Both are carried because collapsing them would credit a delegated
/// answer to the wrong side of the delegation, and a reader asking whether a
/// question was answered by someone who argued it needs the pair.
#[derive(Serialize)]
struct DiscussionAnswer {
    option: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_behalf_of: Option<String>,
}

/// One position in a round: its stable id, and the same identity pair an
/// answer carries.
#[derive(Serialize)]
struct DiscussionPosition {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_behalf_of: Option<String>,
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
    /// Visible question and answer blocks whose typed transition never landed.
    /// A command can be interrupted between the Markdown and JSONL appends;
    /// surfacing the IDs keeps that partial write out of silent derived state.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unrecorded_question_blocks: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unrecorded_answer_blocks: Vec<String>,
    rounds: Vec<DiscussionRound>,
    unanswered: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<Resolution>,
}

#[derive(Default)]
struct QuestionBlockIds {
    questions: HashSet<String>,
    answers: HashSet<String>,
}

fn question_block_ids(body: &str) -> QuestionBlockIds {
    let mut ids = QuestionBlockIds::default();
    for line in body.lines() {
        if let Some(id) = line
            .strip_prefix("### Question ")
            .and_then(|rest| rest.split_once(" (").map(|(id, _)| id))
            .filter(|id| valid_question_id(id))
        {
            ids.questions.insert(id.to_string());
        }
        if let Some(id) = line
            .strip_prefix("### Answer ")
            .and_then(|rest| rest.split_once(" = ").map(|(id, _)| id))
            .filter(|id| valid_question_id(id))
        {
            ids.answers.insert(id.to_string());
        }
    }
    ids
}

fn unrecorded_question_blocks(
    body: &str,
    events: &[JournalEvent],
    filename: &str,
) -> (Vec<String>, Vec<String>) {
    let blocks = question_block_ids(body);
    let questions: HashSet<&str> = events
        .iter()
        .filter(|event| event.event == "question" && event.file.as_deref() == Some(filename))
        .filter_map(|event| event.question_id.as_deref())
        .collect();
    let answers: HashSet<&str> = events
        .iter()
        .filter(|event| event.event == "answer" && event.file.as_deref() == Some(filename))
        .filter_map(|event| event.question_id.as_deref())
        .collect();
    let mut unrecorded_questions = blocks
        .questions
        .into_iter()
        .filter(|id| !questions.contains(id.as_str()))
        .collect::<Vec<_>>();
    let mut unrecorded_answers = blocks
        .answers
        .into_iter()
        .filter(|id| !answers.contains(id.as_str()))
        .collect::<Vec<_>>();
    unrecorded_questions.sort();
    unrecorded_answers.sort();
    (unrecorded_questions, unrecorded_answers)
}

#[derive(Serialize)]
struct DiscussionRound {
    depth: usize,
    positions: Vec<DiscussionPosition>,
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

/// Typed `position` events for one artifact, in ledger order.
fn position_events<'a>(events: &'a [JournalEvent], filename: &str) -> Vec<&'a JournalEvent> {
    events
        .iter()
        .filter(|event| {
            event.known() && event.event == "position" && event.file.as_deref() == Some(filename)
        })
        .collect()
}

/// Distinct identities that filed a position, first appearance first.
fn discussion_participants(positions: &[&JournalEvent]) -> Vec<String> {
    let mut participants: Vec<String> = Vec::new();
    for event in positions {
        let label = event_identity_label(event);
        if !participants.contains(&label) {
            participants.push(label);
        }
    }
    participants
}

/// The two facts that say a decision was never actually tested: one voice
/// argued it, and no position answered another position.
///
/// Derived here rather than at each caller so the warning `consume` prints and
/// the view `discussion` renders cannot drift apart — a warning that disagreed
/// with the view a resolver just read would be worse than none.
fn untested_discussion(events: &[JournalEvent], filename: &str) -> Vec<String> {
    let positions = position_events(events, filename);
    if positions.is_empty() {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    let participants = discussion_participants(&positions);
    if participants.len() == 1 {
        warnings.push(format!(
            "every position came from one participant ({})",
            participants[0]
        ));
    }
    let (_, _, answered) = discussion_rounds(&positions);
    if answered.is_empty() {
        warnings.push("no position answered another position".to_string());
    }
    warnings
}

fn discussion_rounds(
    positions: &[&JournalEvent],
) -> (Vec<DiscussionRound>, Vec<String>, HashSet<String>) {
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
        round.positions.push(DiscussionPosition {
            id: position_id.clone(),
            actor: event.actor.clone(),
            on_behalf_of: event.on_behalf_of.clone(),
        });
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

    let answered = answered
        .into_iter()
        .map(str::to_string)
        .collect::<HashSet<_>>();

    (rounds, unanswered, answered)
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
    valid_position_id(token).then(|| token.to_string())
}

fn position_stance_value(line: &str) -> Option<&str> {
    position_stance_text(line).and_then(|rest| rest.split_whitespace().next())
}

/// Count position blocks, and the stance each one states.
///
/// A block's stance is the first non-blank line under its heading, which is
/// where `journal position --stance` writes it and where a hand-written body
/// can provide it. A block whose first line argues instead of voting is counted
/// as `unstated`, so a tally that undercounts says so instead of reading as a
/// settled result — and a `Position:` line anywhere else, including inside a
/// fenced example, is prose.
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
            // An attributed heading ends the block it belongs to, fence or no
            // fence. A body that opens a fence and never closes it — a stray
            // closing marker with no opener is the common way — would
            // otherwise swallow every position after it, and the tally would
            // quietly report the smaller number. One participant's malformed
            // Markdown must not hide another participant's stance.
            //
            // Only a heading carrying a recorded id does this, so a fenced
            // example quoting the conventions stays prose: the scaffold that
            // teaches the stance line is exactly such a quote.
            (Some(_), _) if position_heading_id(line).is_some() => fence = None,
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
        let Some(value) = position_stance_value(trimmed) else {
            // The block opens by arguing rather than voting.
            tally.unstated += 1;
            continue;
        };
        match PositionStance::parse(value) {
            Some(PositionStance::For) => tally.in_favor += 1,
            Some(PositionStance::Against) => tally.against += 1,
            Some(PositionStance::Amend) => tally.amend += 1,
            None => tally.other += 1,
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
    let Some((ts, topic, _kind)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    // The view reads what the write allows: `journal position` accepts any
    // live artifact (only branch positions need a discussion), so the view
    // renders any artifact too. A feature request that collected stances is
    // legible as what it became; question and resolution sections simply
    // stay empty where the write side never created them.
    let body = read_artifact_body(ctx, filename)?;
    let dir = resolve_dir(&ctx.cwd)?;
    let events = read_events(&dir)?;

    let (positions, stances, heading_ids) = position_structure(&body);
    let (unrecorded_question_blocks, unrecorded_answer_blocks) =
        unrecorded_question_blocks(&body, &events, filename);

    let position_events = position_events(&events, filename);
    let participants = discussion_participants(&position_events);
    let reply_refs = position_events
        .iter()
        .filter(|event| event.reference.is_some())
        .count();
    let (rounds, unanswered, _) = discussion_rounds(&position_events);
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
                .and_then(|event| {
                    Some(DiscussionAnswer {
                        option: event.option.clone()?,
                        actor: event.actor.clone(),
                        on_behalf_of: event.on_behalf_of.clone(),
                    })
                });
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
        schema: "journal-discussion/2",
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
        unrecorded_question_blocks,
        unrecorded_answer_blocks,
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
        let (blocks, verb) = if summary.stances.unstated == 1 {
            ("block", "states")
        } else {
            ("blocks", "state")
        };
        println!(
            "unstated: {} position {blocks} {verb} no stance, so the tally undercounts \
             (`journal position --stance <for|against|amend>` writes the line; a body \
             edited by hand opens with it)",
            summary.stances.unstated,
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
            Some(answer) => format!("answered {}", answer.option),
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
    if !summary.unrecorded_question_blocks.is_empty() {
        println!(
            "unrecorded question blocks: {} — visible Markdown has no typed transition; run journal doctor",
            summary.unrecorded_question_blocks.join(", ")
        );
    }
    if !summary.unrecorded_answer_blocks.is_empty() {
        println!(
            "unrecorded answer blocks: {} — visible Markdown has no typed transition; run journal doctor",
            summary.unrecorded_answer_blocks.join(", ")
        );
    }
    println!("rounds (same-depth positions could not have read each other):");
    for round in &summary.rounds {
        println!(
            "  round {}: {} — {}",
            round.depth,
            round
                .positions
                .iter()
                .map(|position| position.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
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

/// Question ids posed on this artifact that no `answer` event has settled.
///
/// Only typed questions are visible here. A question written as prose is
/// invisible to every derived view, which is the reason the guide asks for the
/// verb rather than the paragraph.
fn unanswered_questions(events: &[JournalEvent], filename: &str) -> Vec<String> {
    let answered: HashSet<&str> = events
        .iter()
        .filter(|event| {
            event.known() && event.event == "answer" && event.file.as_deref() == Some(filename)
        })
        .filter_map(|event| event.question_id.as_deref())
        .collect();
    let mut open = events
        .iter()
        .filter(|event| {
            event.known() && event.event == "question" && event.file.as_deref() == Some(filename)
        })
        .filter_map(|event| event.question_id.clone())
        .filter(|id| !answered.contains(id.as_str()))
        .collect::<Vec<_>>();
    open.sort();
    open.dedup();
    open
}

pub(crate) fn consume(
    ctx: &Ctx,
    filename: &str,
    outcome: ConsumeOutcome,
    note: Option<&str>,
    decision: Option<&str>,
    drop_questions: bool,
) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let target_kind = parse_artifact_name(filename).map(|(_, _, kind)| kind);
    let mut decision_filename = None;
    let mut decision_project = None;
    let mut decision_kind = None;
    let mut decision_digest = None;
    let mut decision_project_label = None;
    if let Some(decision) = decision {
        if !matches!(outcome, ConsumeOutcome::Done) {
            bail!("--decision is valid only with --outcome done");
        }
        if decision.contains(['/', '\\']) {
            bail!("--decision takes an artifact filename, not a path");
        }
        let (project_prefix, artifact) = match decision.split_once("::") {
            Some((project, artifact)) => (Some(project), artifact),
            None => (None, decision),
        };
        let Some((_, _, kind)) = parse_artifact_name(artifact) else {
            bail!("{artifact:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
        };
        let conclusion_allowed = target_kind.as_deref().is_some_and(|target_kind| {
            is_actionable_kind(target_kind) && target_kind != JournalKind::Discussion.as_str()
        });
        if kind != JournalKind::Decision.as_str()
            && !(conclusion_allowed && kind == JournalKind::Conclusion.as_str())
        {
            bail!("{artifact} is a {kind} artifact, not a decision");
        }

        let (decision_dir, other_project, project_label) = match project_prefix {
            Some(prefix) => {
                let cfg = config::load()?;
                let projects = crate::registry::projects(&cfg)?;
                let matches: Vec<_> = projects
                    .iter()
                    .filter(|project| project.slug == prefix || project.label() == prefix)
                    .collect();
                match matches.as_slice() {
                    [] => bail!("no registered project matched {prefix:?} by slug or label"),
                    [project] => {
                        let other = project.journal_dir != dir;
                        (
                            project.journal_dir.clone(),
                            other.then(|| project.slug.clone()),
                            other.then(|| project.label()),
                        )
                    }
                    _ => {
                        let candidates = matches
                            .iter()
                            .map(|project| format!("{} ({})", project.slug, project.label()))
                            .collect::<Vec<_>>()
                            .join(", ");
                        bail!(
                            "project prefix {prefix:?} matched multiple registered projects: {candidates}"
                        )
                    }
                }
            }
            None => (dir.clone(), None, None),
        };
        let decision_path = if decision_dir.join(artifact).is_file() {
            decision_dir.join(artifact)
        } else if archive_dir(&decision_dir).join(artifact).is_file() {
            archive_dir(&decision_dir).join(artifact)
        } else {
            bail!(
                "no such decision artifact {artifact} in {} or its cold archive",
                decision_dir.display()
            );
        };
        let bytes = std::fs::read(&decision_path).with_context(|| {
            format!("cannot read decision artifact {}", decision_path.display())
        })?;
        decision_filename = Some(artifact.to_string());
        decision_project = other_project;
        decision_project_label = project_label;
        decision_kind = Some(kind);
        decision_digest = Some(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))));
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
    let events = read_events(&dir)?;
    if is_consumed(&events, filename) {
        bail!("{filename} is already consumed (see the journal)");
    }
    // An artifact can carry two different things: work to do, and a question
    // to settle. Consumption disposes of the work correctly, and would drop
    // the question into a file no queue lists — a derived view that silently
    // under-reports, which is the failure this whole surface exists to avoid.
    let open = unanswered_questions(&events, filename);
    if !open.is_empty() && !drop_questions {
        bail!(
            "{filename} holds {} unanswered question{}: {}\n\
             tip: settle it with `arc journal answer {filename} --question <id> \
             --option <choice> --body-file -`, re-file it as its own artifact, \
             or pass --drop-questions and say in --note where it went",
            open.len(),
            if open.len() == 1 { "" } else { "s" },
            open.join(", ")
        );
    }
    let mut event = JournalEvent::base(ctx, Utc::now(), &topic, "consumed");
    event.file = Some(filename.to_string());
    event.outcome = Some(outcome.as_str().to_string());
    event.note = note.map(str::to_string);
    event.decision = decision_filename;
    event.decision_project = decision_project;
    event.decision_kind = decision_kind;
    event.decision_digest = decision_digest;
    append_event(ctx, &dir, &event)?;
    println!("consumed: {filename} [{}]", outcome.as_str());
    // A one-participant discussion is a legitimate way to settle something,
    // and the resolver is already required to be a person or a lead. What the
    // resolver should not do is resolve one without being told which it was:
    // `discussion` renders these two facts, and until now `consume` read
    // neither, so one round by one model was disposed of with exactly the
    // ceremony a reversal survived. Discarding earns the warning too — more
    // so, since nothing at all is kept.
    if target_kind.as_deref() == Some(JournalKind::Discussion.as_str()) {
        for warning in untested_discussion(&events, filename) {
            println!("warning: {warning}");
        }
    }
    if let (Some(decision), Some(kind)) =
        (event.decision.as_deref(), event.decision_kind.as_deref())
    {
        let project = decision_project_label
            .as_deref()
            .map(|label| format!("{label}::"))
            .unwrap_or_default();
        println!("resolved by: {project}{decision}   ({kind})");
    }
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
    let events = read_events(&dir)?;
    if is_consumed(&events, filename) {
        bail!("{filename} is already consumed (see the journal)");
    }
    // Promotion consumes the source as superseded, which never claims a
    // question was settled — and the artifact's body travels onto the change
    // as its seed brief, so the question goes with the work. That is why this
    // warns where `consume` refuses: `consume` claims disposal, promotion does
    // not. Either way the ids are named, because the artifact leaves the open
    // queue in both.
    let open = unanswered_questions(&events, filename);
    if !open.is_empty() {
        eprintln!(
            "warning: {filename} holds {} unanswered question{} ({}) that this change does not \
             settle; re-file it as its own artifact or it leaves the open queue undecided",
            open.len(),
            if open.len() == 1 { "" } else { "s" },
            open.join(", ")
        );
    }
    Ok(kind)
}

/// The kinds a transition may produce. A transition retires one live
/// workflow artifact and opens its successor; a `decision` is how a
/// discussion *ends*, not what it becomes, and a `note`/`memory`/`review`
/// are records rather than work — so the matrix admits exactly the
/// actionable kinds plus the record kinds a proposal turns into.
fn transition_allowed_target(kind: JournalKind) -> bool {
    matches!(
        kind,
        JournalKind::Todo
            | JournalKind::Plan
            | JournalKind::Handoff
            | JournalKind::Later
            | JournalKind::FeatureRequest
            | JournalKind::Discussion
    )
}

fn artifact_supersedes(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let source = text.lines().next()?.strip_prefix("supersedes: ")?.trim();
    parse_artifact_name(source).map(|_| source.to_string())
}

fn find_transition_artifact(
    dir: &Path,
    topic: &str,
    target_kind: &str,
    source: &str,
) -> Result<Option<String>> {
    let mut found = None;
    for name in sorted_artifact_names(dir)? {
        let Some((_, artifact_topic, artifact_kind)) = parse_artifact_name(&name) else {
            continue;
        };
        if artifact_topic != topic
            || artifact_kind != target_kind
            || artifact_supersedes(&dir.join(&name)).as_deref() != Some(source)
        {
            continue;
        }
        if found.is_some() {
            bail!(
                "multiple {} successors supersede {}; repair the journal before retrying",
                target_kind,
                source
            );
        }
        found = Some(name);
    }
    Ok(found)
}

fn write_successor(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| {
            format!(
                "cannot create {} (an artifact with this second's timestamp already exists)",
                path.display()
            )
        })?;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))
}

fn append_transition_retirement(
    ctx: &Ctx,
    dir: &Path,
    topic: &str,
    source: &str,
    successor: &str,
) -> Result<()> {
    let mut consumed = JournalEvent::base(ctx, Utc::now(), topic, "consumed");
    consumed.file = Some(source.to_string());
    consumed.outcome = Some(ConsumeOutcome::Superseded.as_str().to_string());
    consumed.note = Some(format!("transitioned to {successor}"));
    append_event(ctx, dir, &consumed)
}

/// Change a live artifact's kind as one guarded operation: record the relation,
/// create the successor, and retire the source. The relation is written first,
/// so an interruption leaves the source actionable with a pending fact rather
/// than silently creating an untracked second queue item. A retry completes a
/// pending relation, while the journal doctor reports every incomplete pair.
#[allow(clippy::too_many_arguments)]
fn transition(
    ctx: &Ctx,
    filename: &str,
    to: JournalKind,
    body_file: Option<&str>,
    title: Option<&str>,
    reason: Option<&str>,
    dry_run: bool,
) -> Result<i32> {
    if !transition_allowed_target(to) {
        bail!(
            "--to {} is not a transition target; a decision is how a discussion ends, \
             not what it becomes, and note/memory/review are records rather than work",
            to.as_str()
        );
    }
    if filename.contains(['/', '\\']) {
        bail!("transition takes an artifact filename inside the journal dir, not a path");
    }
    let Some((_, topic, from_kind)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    if !is_actionable_kind(&from_kind) {
        bail!(
            "{filename} is a {from_kind} artifact, not an actionable item ({})",
            PRIMARY_ACTIONABLE_KINDS
                .iter()
                .copied()
                .chain(std::iter::once(LATER_KIND))
                .chain(std::iter::once(FEATURE_REQUEST_KIND))
                .collect::<Vec<_>>()
                .join("|")
        );
    }
    if from_kind == to.as_str() {
        bail!("{} is already a {} artifact", filename, from_kind);
    }
    let dir = resolve_dir(&ctx.cwd)?;
    let source_path = dir.join(filename);
    if !source_path.is_file() {
        bail!("no such artifact {} in {}", filename, dir.display());
    }
    let events = read_events(&dir)?;
    if is_consumed(&events, filename) {
        bail!("{filename} is already consumed (see the journal)");
    }
    // An unanswered question is not settled by a kind change. It remains
    // attached to the consumed source, where the answer operation can still
    // append its settlement and keep the original provenance visible.
    let open = unanswered_questions(&events, filename);
    if !open.is_empty() && !dry_run {
        eprintln!(
            "warning: {filename} holds {} unanswered question{} ({}) that the successor \
             does not settle; it remains answerable on the consumed source",
            open.len(),
            if open.len() == 1 { "" } else { "s" },
            open.join(", ")
        );
    }

    // The successor's body: the caller's body when given, otherwise the
    // source's own body under a heading that names what changed. Same-second
    // same-topic collisions are impossible against the source (kinds differ),
    // but a same-second sibling could collide — exclusive create fails loudly
    // rather than overwriting.
    let now = Utc::now();
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let successor_name = format!("{stamp}-{topic}-{}.md", to.as_str());
    let successor_path = dir.join(&successor_name);
    let source_bytes = std::fs::read(&source_path)
        .with_context(|| format!("cannot read {}", source_path.display()))?;
    let inherited = String::from_utf8_lossy(&source_bytes).into_owned();
    let body = match body_file {
        Some(source) => read_body_verbatim(source)?,
        None => {
            let heading = format!(
                "\nTransitioned from `{filename}`: kind {} → {}.",
                from_kind,
                to.as_str()
            );
            format!("{inherited}{heading}\n")
        }
    };
    let contents = match title {
        Some(t) => format!("# {t}\n\n{body}"),
        None => body,
    };
    let supersession = format!("supersedes: {filename}\n\n",);
    if dry_run {
        println!("successor: {}", successor_path.display());
        println!("first lines:");
        for line in format!(
            "{supersession}{}",
            contents.lines().next().unwrap_or_default()
        )
        .lines()
        {
            println!("  {line}");
        }
        println!(
            "effects: write {} · transition event {} → {} · consume {} [superseded]",
            successor_path.display(),
            from_kind,
            to.as_str(),
            filename
        );
        return Ok(0);
    }
    let _transition = lock_journal_transition(&dir)?;
    let events = read_events(&dir)?;
    if is_consumed(&events, filename) {
        bail!("{filename} is already consumed (see the journal)");
    }

    if let Some(existing) = events
        .iter()
        .rev()
        .find(|event| event.event == "transition" && event.supersedes.as_deref() == Some(filename))
    {
        let successor = existing
            .file
            .as_deref()
            .context("transition event has no successor filename")?;
        let Some((_, existing_topic, existing_kind)) = parse_artifact_name(successor) else {
            bail!("transition event names malformed successor {successor}");
        };
        if existing_topic != topic || existing_kind != to.as_str() {
            bail!(
                "{filename} already has a transition to {successor}; retry that transition instead"
            );
        }
        // The successor may have been consumed and archived between the
        // interrupted transition and this retry. Resolving it across both
        // stores is what stops the retry recreating a hot duplicate that
        // shadows the real one.
        match artifact_body_path(&dir, successor) {
            Some(path) if artifact_supersedes(&path).as_deref() != Some(filename) => bail!(
                "transition successor {} exists without the supersedes link to {}",
                path.display(),
                filename
            ),
            Some(_) => {}
            None => write_successor(&dir.join(successor), &format!("{supersession}{contents}"))?,
        }
        append_transition_retirement(ctx, &dir, &topic, filename, successor)?;
        println!(
            "completed transition: {filename} → {successor} ({} superseded)",
            from_kind
        );
        return Ok(0);
    }

    let successor_name =
        find_transition_artifact(&dir, &topic, to.as_str(), filename)?.unwrap_or(successor_name);
    let successor_path = dir.join(&successor_name);
    if successor_path.exists() && artifact_supersedes(&successor_path).as_deref() != Some(filename)
    {
        bail!(
            "cannot create {} (an artifact with this second's timestamp already exists)",
            successor_path.display()
        );
    }

    // The relation is the durable intent. If the next write is interrupted,
    // the journal doctor can name it and a retry can finish it without
    // inventing another successor.
    let mut event = JournalEvent::base(ctx, now, &topic, "transition");
    event.file = Some(successor_name.clone());
    event.supersedes = Some(filename.to_string());
    event.note = reason.map(str::to_string);
    append_event(ctx, &dir, &event)?;
    if !successor_path.exists() {
        write_successor(&successor_path, &format!("{supersession}{contents}"))?;
    }
    append_transition_retirement(ctx, &dir, &topic, filename, &successor_name)?;
    println!(
        "transitioned: {filename} → {successor_name} ({} superseded)",
        from_kind
    );
    Ok(0)
}

/// Append a journal `consumed` event marking an artifact superseded by the
/// change opened from it. The artifact file itself is never edited.
pub fn consume_superseded_by_change(
    ctx: &Ctx,
    filename: &str,
    change_id: &str,
    _transition: &JournalTransitionLock,
) -> Result<()> {
    let Some((_, topic, _)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name");
    };
    let dir = resolve_dir(&ctx.cwd)?;
    if is_consumed(&read_events(&dir)?, filename) {
        bail!("{filename} is already consumed (see the journal)");
    }
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
    /// Questions no agent may settle. Reported first, because a session that
    /// picks up work while one waits can do everything except the thing that
    /// is actually blocked.
    open_questions: Vec<OpenQuestion>,
    #[serde(flatten)]
    queue: OpenItems,
}

pub(crate) fn orientation(ctx: &Ctx) -> Result<Orientation> {
    let dir = resolve_dir(&ctx.cwd)?;
    let now = Utc::now();
    Ok(Orientation {
        lanes: lanes_from_journal(&read_events(&dir)?, now),
        memories: live_memories(&dir)?,
        open_questions: open_questions(&dir)?,
        queue: collect_open(ctx, None)?,
    })
}

impl Orientation {
    pub(crate) fn render(&self) {
        render_lanes(&self.lanes, Utc::now());
        if !self.open_questions.is_empty() {
            println!(
                "questions awaiting settlement ({}):",
                self.open_questions.len()
            );
            for question in &self.open_questions {
                let settle_by = match &question.settle_by {
                    None => "person",
                    Some(value) => value.as_str(),
                };
                if settle_by == "person" {
                    println!(
                        "  {}  {}  {}",
                        question.question,
                        question.topic,
                        question.heading.as_deref().unwrap_or("")
                    );
                } else {
                    println!(
                        "  {}  {}  {}  (settle by: {settle_by})",
                        question.question,
                        question.topic,
                        question.heading.as_deref().unwrap_or("")
                    );
                }
            }
            println!("  `arc journal questions` for the options, `journal answer` to settle one");
        }
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
mod question_text_tests {
    use super::question_text;
    use std::io::Write;

    fn fixture(body: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file
    }

    /// The id must end at a boundary. Matching a prefix would answer a
    /// request for `q-a` with `q-a1`'s prose, and prompt somebody with a
    /// question they were not asked.
    #[test]
    fn a_longer_id_sharing_a_prefix_is_not_matched() {
        let file = fixture(concat!(
            "### Question q-a1 (opening, t) - x | y\n\nLonger.\n\n",
            "### Question q-a (opening, t) - x | y\n\nShorter.\n",
        ));
        assert_eq!(
            question_text(file.path(), "q-a").as_deref(),
            Some("Shorter.")
        );
        assert_eq!(
            question_text(file.path(), "q-a1").as_deref(),
            Some("Longer.")
        );
    }

    /// A fenced block quoting the conventions is prose, exactly as it is for
    /// position headings.
    #[test]
    fn a_fenced_example_is_not_read_as_the_question() {
        let file = fixture(concat!(
            "```\n### Question q-a (opening, t) - x | y\n\nAn example.\n```\n\n",
            "### Question q-a (opening, t) - x | y\n\nThe real one.\n",
        ));
        assert_eq!(
            question_text(file.path(), "q-a").as_deref(),
            Some("The real one.")
        );
    }

    /// A block that was hand-edited away degrades to absent rather than to
    /// the next block's prose.
    #[test]
    fn a_missing_block_yields_nothing() {
        let file = fixture("# Title\n\nNo question blocks here.\n");
        assert_eq!(question_text(file.path(), "q-a"), None);
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
