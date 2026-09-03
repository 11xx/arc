mod bundle;
mod chain;
mod commands;
mod config;
mod context;
mod forge;
mod gates;
mod gitio;
mod guide;
mod ids;
mod inbox;
mod journal;
mod model;
mod policy;
mod project;
mod registry;
mod render;
mod rewrite;
mod session_store;
mod state;
mod status;
mod store;
mod worktree_usage;

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use commands::{fork, AnchorArgs, Ctx, ListFormat, QueryArgs};
use model::{
    ActorSource, DebtMissing, DispositionStatus, MessageSeverity, MessageType, ProbePhase,
    ReviewCause, RunOutcome, Severity, Side, Verdict, VerdictRelationKind, VerifyResult,
};
use std::path::PathBuf;

/// Change, review, and integration state over plain Git for agentic
/// coding arcs. Git owns content and history; arc owns the collaboration
/// objects Git lacks: changes, patchsets, findings, verdicts, gates,
/// holds, and a guarded merge.
#[derive(Parser)]
#[command(
    name = "arc",
    version,
    about,
    after_help = "Run `arc` with no arguments for the workflow guide, or `arc catchup` for live project state."
)]
struct Cli {
    /// Acting identity, from ARC_ACTOR when the flag is absent. Falls back to
    /// git user.name, which arc records as an identity nobody declared
    #[arg(long, global = true)]
    actor: Option<String>,
    /// Harness label, e.g. claude, codex, opencode
    #[arg(long, global = true, env = "ARC_HARNESS")]
    harness: Option<String>,
    /// Native session ID of the acting harness thread
    #[arg(long, global = true, env = "ARC_SESSION")]
    session: Option<String>,
    /// Model identity: a model slug with optional #effort, e.g. kimi-k3#high
    #[arg(long, global = true, env = "ARC_MODEL")]
    model: Option<String>,
    /// Subject a lead runs delegated ceremony for; recorded beside the invoker
    #[arg(long = "on-behalf-of", global = true, env = "ARC_ON_BEHALF_OF")]
    on_behalf_of: Option<String>,
    /// Execution boundary: implementer | reviewer | lead
    #[arg(long, global = true, env = "ARC_ROLE")]
    role: Option<String>,
    /// Change to act on, wherever the positional is optional
    #[arg(long = "change", id = "change_flag", global = true)]
    change: Option<String>,
    /// Absent prints the workflow guide: what arc owns, the command
    /// lifecycle, profile selection, and the rules that change what a
    /// session should do.
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionRole {
    Implementer,
    Reviewer,
    Lead,
}

impl ExecutionRole {
    fn parse(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim) {
            None | Some("") | Some("lead") => Ok(Self::Lead),
            Some("implementer") => Ok(Self::Implementer),
            Some("reviewer") => Ok(Self::Reviewer),
            Some(value) => {
                bail!("invalid execution role {value:?}; expected implementer, reviewer, or lead")
            }
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Implementer => "implementer",
            Self::Reviewer => "reviewer",
            Self::Lead => "lead",
        }
    }
}

#[derive(clap::Args)]
struct AnchorOpts {
    /// File path the comment/finding anchors to
    #[arg(long)]
    path: Option<String>,
    /// Which side of the patchset the anchor targets
    #[arg(long, value_enum, default_value = "head")]
    side: Side,
    /// First anchored line
    #[arg(long)]
    line: Option<u32>,
    /// Last anchored line (defaults to --line)
    #[arg(long)]
    line_end: Option<u32>,
    /// Short context snippet or hunk header (line numbers drift; context survives)
    #[arg(long)]
    context: Option<String>,
}

impl AnchorOpts {
    fn to_args(&self) -> AnchorArgs {
        AnchorArgs {
            path: self.path.clone(),
            side: self.side,
            line_start: self.line,
            line_end: self.line_end,
            context: self.context.clone(),
        }
    }
}

#[derive(clap::Args)]
struct BodyOpts {
    /// Inline body text
    #[arg(long)]
    body: Option<String>,
    /// Read body from file ('-' for stdin)
    #[arg(long)]
    body_file: Option<String>,
}

/// CLI spelling of `KeptKind`, kept separate so clap's value names stay a
/// surface decision rather than leaking the ledger's serde spelling.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum KeptKindArg {
    Verified,
    Rejected,
    Constraint,
    Hypothesis,
}

impl From<KeptKindArg> for model::KeptKind {
    fn from(value: KeptKindArg) -> Self {
        match value {
            KeptKindArg::Verified => model::KeptKind::Verified,
            KeptKindArg::Rejected => model::KeptKind::Rejected,
            KeptKindArg::Constraint => model::KeptKind::Constraint,
            KeptKindArg::Hypothesis => model::KeptKind::Hypothesis,
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Open a change: create (or adopt) its branch and worktree
    Begin {
        /// Kebab-case slug naming the outcome (also the ID prefix)
        slug: String,
        /// Human title (defaults to the slug with spaces)
        #[arg(long)]
        title: Option<String>,
        /// Workflow profile: direct | local | forge | release
        #[arg(long, default_value = "local")]
        profile: String,
        /// Integration target branch (defaults to the current branch)
        #[arg(long)]
        target: Option<String>,
        /// Base revision (defaults to the target head)
        #[arg(long)]
        base: Option<String>,
        /// Branch name (defaults to arc/<slug>)
        #[arg(long)]
        branch: Option<String>,
        /// Worktree path (defaults to ~/.worktrees/<repo>-<slug>)
        #[arg(long)]
        worktree: Option<String>,
        /// Use a clean checkout on the target branch in place; otherwise do not switch
        #[arg(long)]
        no_worktree: bool,
        /// Track an existing branch instead of creating one
        #[arg(long)]
        adopt: Option<String>,
        /// Change that must integrate before this one is ready (repeatable)
        #[arg(long = "blocked-by")]
        blocked_by: Vec<String>,
        /// Batch/query tag (repeatable)
        #[arg(long)]
        tag: Vec<String>,
        /// Open from an actionable journal artifact, consuming it
        #[arg(long = "from-journal")]
        from_journal: Option<String>,
        /// Require an independent verdict whatever this change turns out to
        /// touch. One-way: nothing lowers it afterwards
        #[arg(long)]
        dangerous: bool,
        /// Open the change declaring that integration is not yet the goal
        #[arg(long)]
        iterating: bool,
    },
    /// List changes
    List {
        /// List only changes that are still open
        #[arg(long)]
        open: bool,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
        #[arg(long, value_enum, default_value = "default")]
        format: ListFormat,
    },
    /// Filter changes and print matching IDs (or JSON)
    Query {
        /// open | closed | integrated | abandoned | superseded
        #[arg(long)]
        status: Option<String>,
        /// Only changes whose integration target is this branch
        #[arg(long)]
        target: Option<String>,
        /// Only changes carrying every tag given (repeatable)
        #[arg(long)]
        tag: Vec<String>,
        /// Only changes whose latest verdict is this
        #[arg(long, value_enum)]
        verdict: Option<Verdict>,
        /// Only changes opened by this actor
        #[arg(long)]
        actor: Option<String>,
        /// Only changes opened from this harness
        #[arg(long)]
        harness: Option<String>,
        /// Report changes whose patchset, integration, or closure commit
        /// matches this revision (unique prefix accepted)
        #[arg(long)]
        commit: Option<String>,
        /// Only changes that integrated owing a review nobody has recorded yet
        #[arg(long = "debt")]
        debt: bool,
        /// Only changes whose gating approval was recorded as owed
        /// corroboration and has not received it — from an independent
        /// approval of the same patchset, or from an audit
        #[arg(long)]
        provisional: bool,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// Render one change (Markdown, or full state with --json)
    Show {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Render every change carrying this tag instead of one change
        #[arg(long)]
        tag: Vec<String>,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
        /// Replay state as of this event ID ("what did the actor see?")
        #[arg(long, conflicts_with = "tag")]
        at: Option<String>,
    },
    /// Print the change's recorded facts one line each, in ledger order. A
    /// review batch records several, so it renders as several lines. This is
    /// the ledger, not Git history: for commits, use `git log`
    Log {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Newest event first
        #[arg(long)]
        reverse: bool,
        /// Accepted so the Git habit lands somewhere useful: arc log is
        /// already one line per fact, so this changes nothing
        #[arg(long, hide = true)]
        oneline: bool,
    },
    /// Derived ledger analytics: stage, review, and gate durations
    Stats {
        /// Report a single change
        #[arg(long, id = "change_flag", conflicts_with_all = ["tag", "all"])]
        change: Option<String>,
        /// Report every change carrying this tag
        #[arg(long, conflicts_with_all = ["change_flag", "all"])]
        tag: Option<String>,
        /// Report all changes (the default)
        #[arg(long)]
        all: bool,
        /// One row per delegated identity instead of per change: patchsets
        /// contributed, rework rounds they opened, verdicts issued
        #[arg(long = "by-model")]
        by_model: bool,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// Render a recorded patchset using Git's native diff output
    Diff {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        #[arg(index = 1)]
        change: Option<String>,
        /// Patchset to render (defaults to the latest snapshot)
        #[arg(long)]
        patchset: Option<String>,
        /// Pass --stat through to git diff
        #[arg(long)]
        stat: bool,
        /// Render unresolved finding anchors after the diff
        #[arg(long)]
        findings: bool,
        /// Compare two recorded patchsets instead of a patchset base and head
        #[arg(long, num_args = 2, value_names = ["OLDER", "NEWER"], conflicts_with_all = ["since_approved", "patchset"])]
        between: Option<Vec<String>>,
        /// Compare the last approved patchset with the latest snapshot
        #[arg(long, conflicts_with = "patchset")]
        since_approved: bool,
        /// Render the exact recorded integration range of a closed change
        #[arg(long, conflicts_with_all = ["patchset", "between", "since_approved", "findings"])]
        integrated: bool,
        /// Base revision for an integration that recorded none
        #[arg(long, requires = "integrated")]
        base: Option<String>,
        /// Git pathspecs, passed after -- to git diff
        #[arg(index = 2, last = true)]
        paths: Vec<String>,
    },
    /// List findings in text, JSON, or SARIF 2.1.0 form
    Findings {
        /// List post-integration audit findings instead of the shipped ones
        #[arg(long)]
        audit: bool,
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        #[arg(long, value_enum, default_value = "text")]
        format: commands::FindingsFormat,
    },
    /// Record or read a change-scoped implementation contract
    Brief {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Read a new brief body from a file ('-' for stdin)
        #[arg(long)]
        body_file: Option<String>,
        /// Optional title for a newly recorded brief
        #[arg(long)]
        title: Option<String>,
        /// Revision whose source and premises this brief was checked against
        #[arg(long)]
        base: Option<String>,
        /// Read one derived brief version instead of the latest
        #[arg(long)]
        version: Option<usize>,
        /// Prepend a scaffold template: .arc/templates/<name>.md, or a built-in
        /// (sol-low, sol-high, reviewer, discussion)
        #[arg(long)]
        scaffold: Option<String>,
        /// Journal plan artifact implemented by this brief
        #[arg(long)]
        plan_ref: Option<String>,
        /// Opaque plan slice slug implemented by this brief
        #[arg(long)]
        plan_slice: Option<String>,
        /// Named acceptance probes bound to this brief: a JSON array inline, a
        /// path to one, or '-' for stdin
        #[arg(long)]
        probes_json: Option<String>,
        /// Earlier ledger fact that caused this version: finding:<id>,
        /// verdict:<event>, or blocked-on:<event> (repeatable)
        #[arg(long)]
        caused_by: Vec<String>,
        /// External cause, when no earlier ledger object represents the reason
        #[arg(long)]
        cause_note: Option<String>,
    },
    /// Record, read, or project changelog entries
    Changelog {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Free-form category for a newly recorded changelog entry
        #[arg(long)]
        category: Option<String>,
        /// Read a new entry body from a file ('-' for stdin)
        #[arg(long)]
        body_file: Option<String>,
        /// Emit a read result as JSON
        #[arg(long)]
        json: bool,
        /// Include recording identity in human-readable output
        #[arg(long, conflicts_with = "json")]
        provenance: bool,
        /// Override the latest-tag release boundary
        #[arg(long)]
        since: Option<String>,
        /// Replace the generated [Unreleased] block in CHANGELOG.md; entries
        /// wrap at 75 columns, continuations indented under their marker
        #[arg(long)]
        write: bool,
    },
    /// Append a structured cross-change announcement (never policy input)
    Message {
        /// Change the announcement is recorded against
        change: String,
        /// Announcement class
        #[arg(long = "type", value_enum)]
        message_type: MessageType,
        /// Required single-line summary
        #[arg(long)]
        summary: String,
        /// Optional longer detail
        #[arg(long)]
        detail: Option<String>,
        /// Optional JSON object stored verbatim as metadata
        #[arg(long)]
        json: Option<String>,
        /// Advisory severity
        #[arg(long, value_enum, default_value = "info")]
        severity: MessageSeverity,
    },
    /// Scan messages across open and closed changes (newest first)
    Messages {
        /// Only messages recorded against this change
        #[arg(long, id = "change_flag")]
        change: Option<String>,
        #[arg(long = "type", value_enum)]
        message_type: Option<MessageType>,
        #[arg(long, value_enum)]
        severity: Option<MessageSeverity>,
        /// Only messages created at or after this ISO 8601 instant
        #[arg(long)]
        since: Option<String>,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// Lead-facing queue rollup: open changes, active claims, and outstanding debt (arc-inbox/8 schema)
    Inbox {
        /// Restrict to changes assigned to this harness
        #[arg(long = "assigned-to")]
        assigned_to: Option<String>,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// Show a tagged program in dependency order (arc-chain/4 schema)
    Chain {
        /// The tag naming the program to render
        tag: String,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
        /// Include final-patchset review provenance
        #[arg(long)]
        review: bool,
    },
    /// Atomically claim the highest-priority ready change
    Take {
        /// Require every supplied tag (repeatable)
        #[arg(long)]
        tag: Vec<String>,
        /// Lease duration (positive integer with s, m, or h suffix; default 2h)
        #[arg(long)]
        ttl: Option<String>,
        /// Print the full status JSON for the taken change
        #[arg(long)]
        json: bool,
    },
    /// Append dependency, tag, or assignment metadata to an open change
    Metadata {
        /// Change the metadata is appended to
        change: String,
        /// Declare a change that must integrate before this one is ready (repeatable)
        #[arg(long = "blocked-by")]
        blocked_by: Vec<String>,
        /// Withdraw a declared prerequisite (repeatable)
        #[arg(long = "remove-blocked-by")]
        remove_blocked_by: Vec<String>,
        /// Add a batch/query tag (repeatable)
        #[arg(long)]
        tag: Vec<String>,
        /// Withdraw a tag (repeatable)
        #[arg(long = "remove-tag")]
        remove_tag: Vec<String>,
        /// Assign to a harness (advisory; latest wins; "" clears)
        #[arg(long)]
        assign: Option<String>,
        /// Scheduling priority (higher values are taken first; default 0)
        #[arg(long)]
        priority: Option<i32>,
        /// Print the current metadata as arc-metadata/1 JSON
        #[arg(long)]
        json: bool,
    },
    /// Declare that this change is being iterated on, or clear the declaration
    Iterating {
        /// Change whose iteration declaration is changed
        change: String,
        /// Clear the iteration declaration
        #[arg(long)]
        off: bool,
    },
    /// Machine-readable status report (the versioned arc-status/15 schema)
    Status {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Accepted for compatibility; status output is always JSON
        #[arg(long)]
        json: bool,
        /// Print one dotted object-key/array-index path
        #[arg(long, conflicts_with = "fields")]
        get: Option<String>,
        /// Print a top-level JSON field subset
        #[arg(long, conflicts_with = "get")]
        fields: Option<String>,
        /// Replay state as of this event ID ("what did the actor see?")
        #[arg(long)]
        at: Option<String>,
    },
    /// Report whether declared prerequisite changes have integrated
    BlockerStatus {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
    },
    /// Dependency probe: exit 0 ready, 1 blocked, 2 on lookup/ledger errors
    IsBlocked {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
    },
    /// Replay raw ledger events as NDJSON, optionally following new events
    Events {
        /// Continue emitting matching events appended after the replay
        #[arg(long)]
        follow: bool,
        /// Limit events to one exact change ID or unique prefix
        #[arg(long, id = "change_flag")]
        change: Option<String>,
        /// Limit events to the changes carrying all supplied tags (repeatable)
        #[arg(long)]
        tag: Vec<String>,
        /// Read the repository's own events instead of a change's
        #[arg(long, conflicts_with_all = ["change_flag", "tag"])]
        repository: bool,
        /// Limit events to one raw kebab-case event_type value
        #[arg(long = "type")]
        event_type: Option<String>,
        /// Emit only events whose ULID is strictly greater than this cursor
        #[arg(long)]
        since: Option<ulid::Ulid>,
        /// Run a shell command for every emitted event
        #[arg(long = "exec")]
        exec_command: Option<String>,
    },
    /// Wait for a change, or a tagged series, to reach a ledger-derived condition
    Watch {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Watch every change carrying all supplied tags (repeatable)
        #[arg(long)]
        tag: Vec<String>,
        /// With --tag: return when any one member reaches a condition
        #[arg(long, conflicts_with = "all")]
        any: bool,
        /// With --tag: return when every member has reached a condition
        #[arg(long)]
        all: bool,
        /// Condition to wait for (repeatable, comma-separated): `snapshot`,
        /// `stalled`, `reviewed`, `approved`, `gates-green`, `ready`,
        /// `blocked`, `brief-recorded`, `integrated`, or `closed`.
        ///
        /// `reviewed` returns on any verdict against the patchset under
        /// review, whatever it concluded, and names the verdict event so the
        /// caller can read which. `approved` returns on the latest approving
        /// verdict, including a provisional approval and its reason.
        /// `gates-green` waits for every required gate to be green at the
        /// current head. `blocked` and `brief-recorded` name their events.
        /// `ready` is stricter and different: approved, gates green, no
        /// blockers — a review asking for changes never satisfies it, so
        /// waiting on `ready` for a dispatched review cannot tell a reviewer
        /// still working from one that answered.
        #[arg(long, value_enum, value_delimiter = ',', required = true)]
        until: Vec<commands::WatchUntil>,
        /// Fail with exit 2 after this many seconds
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: Option<u64>,
        /// Run a shell command once when a condition is reached
        #[arg(long = "exec")]
        exec_command: Option<String>,
        /// Emit the outcome as one JSON object, naming the change, the
        /// condition, and the event that satisfied it
        #[arg(long)]
        json: bool,
    },
    /// Export one change as a deterministic, versioned JSON bundle
    Export {
        /// Change to export as a versioned bundle
        change: String,
        /// Output file ('-' for stdout)
        #[arg(long)]
        output: String,
    },
    /// Import a versioned JSON bundle into this repository's local store
    Import {
        /// Input file ('-' for stdin)
        input: String,
        /// Validate and report without writing events or retention refs
        #[arg(long)]
        dry_run: bool,
    },
    /// Integration preflight; exit code identifies the first blocker
    Check {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Check every change carrying all supplied tags
        #[arg(long)]
        tag: Vec<String>,
        /// Print a full readiness checklist of every gate condition
        #[arg(long)]
        explain: bool,
        /// Emit all blockers and the resulting exit code as JSON
        #[arg(long, conflicts_with = "tag")]
        json: bool,
    },
    /// Acquire or renew an advisory executor claim on a change or a journal
    /// artifact
    Claim {
        /// Change to act on, or a journal artifact filename ending in `.md`.
        /// Omitted, the change is inferred from the current branch, then from
        /// the worktree the command runs in
        change: Option<String>,
        /// Lease duration (positive integer with s, m, or h suffix; default 2h)
        #[arg(long)]
        ttl: Option<String>,
        /// Override one stage budget as <name>=<duration> (repeatable; a
        /// change's stages are budgeted, an artifact's lease is not)
        #[arg(long = "stage-budget")]
        stage_budget: Vec<String>,
        /// Explicitly displace a claim that may be taken over: a stale one on
        /// a change, an expired one on an artifact
        #[arg(long)]
        takeover: bool,
    },
    /// Release the advisory executor claim on a change or a journal artifact
    ReleaseClaim {
        /// Change to act on, or a journal artifact filename ending in `.md`.
        /// Omitted, the change is inferred from the current branch, then from
        /// the worktree the command runs in
        change: Option<String>,
        /// How work on an artifact stopped (artifacts only; default paused).
        /// `paused` leaves it open for a successor, `abandoned` ends this
        /// approach, `expired` closes a lease that has run out
        #[arg(long, value_parser = ["paused", "abandoned", "expired"])]
        outcome: Option<String>,
    },
    /// Record typed executor progress (requires an owned live claim)
    #[command(allow_missing_positional = true)]
    Stage {
        /// Change to act on, or a journal artifact filename ending in `.md`.
        /// Omitted, the change is inferred from the current branch, then from
        /// the worktree the command runs in
        change: Option<String>,
        #[arg(value_enum)]
        stage: commands::StageArg,
        /// Acquire a default claim first when this session has no live claim
        #[arg(long)]
        claim: bool,
        /// Free-text detail recorded with the stage
        #[arg(long)]
        note: Option<String>,
        /// Read the stage note from a file ('-' for stdin)
        #[arg(long, conflicts_with = "note")]
        note_file: Option<String>,
        /// Structured blocked-on referent: brief:vN, finding:ID, change:ID, or external
        #[arg(long)]
        blocker: Option<String>,
    },
    /// Record the current branch head as a new patchset
    Snapshot {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Override the recorded base revision
        #[arg(long)]
        base: Option<String>,
        /// Brief version this patchset implements (defaults to latest)
        #[arg(long)]
        brief_version: Option<usize>,
        /// Run verification after recording the patchset
        #[arg(long)]
        verify: bool,
        /// Gate name from .arc/gates.toml (repeatable with --verify)
        #[arg(long)]
        gate: Vec<String>,
        /// Run every gate declared for the change profile
        #[arg(long)]
        all: bool,
        /// Explicitly set the contributors for this patchset
        #[arg(
            long = "contributors",
            value_name = "ACTOR[,ACTOR...]",
            value_delimiter = ',',
            conflicts_with = "solo"
        )]
        contributors: Option<Vec<String>>,
        /// Record the invoking actor as the sole contributor
        #[arg(long, conflicts_with = "contributors")]
        solo: bool,
        /// Amend one patchset's contributors before any verdict exists
        #[arg(
            long,
            value_name = "PATCHSET",
            conflicts_with_all = ["base", "brief_version", "verify", "gate", "all"]
        )]
        amend: Option<String>,
    },
    /// Keep a fact this work discovered, so `arc resume` hands it back to a
    /// compacted or cold session instead of it being re-derived
    Keep {
        /// What kind of fact: a premise checked, an approach abandoned, a
        /// constraint discovered, or something believed but not established
        #[arg(long, value_enum)]
        kind: KeptKindArg,
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        #[command(flatten)]
        body: BodyOpts,
        /// What established it. A fact with no evidence reads as a claim
        #[arg(long)]
        evidence: Option<String>,
    },
    /// Add a discussion comment
    Comment {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        #[command(flatten)]
        body: BodyOpts,
        /// Patchset the comment is about (defaults to the latest)
        #[arg(long)]
        patchset: Option<String>,
        #[command(flatten)]
        anchor: AnchorOpts,
    },
    /// Record a standalone review finding
    Finding {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// One-sentence statement of the defect
        #[arg(long)]
        summary: String,
        #[command(flatten)]
        body: BodyOpts,
        /// Record it as blocking, so integration refuses until a disposition
        /// releases it: resolved, accepted-risk, or obsolete
        #[arg(long)]
        blocking: bool,
        #[arg(long, value_enum, default_value = "major")]
        severity: Severity,
        /// Patchset the finding is against (defaults to the latest)
        #[arg(long)]
        patchset: Option<String>,
        #[command(flatten)]
        anchor: AnchorOpts,
    },
    /// Reply to a comment or finding event
    #[command(allow_missing_positional = true)]
    Reply {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// The comment or finding event being replied to
        event_id: String,
        #[command(flatten)]
        body: BodyOpts,
    },
    /// Record a shipped or audit finding disposition (supersedes current tips automatically)
    #[command(allow_missing_positional = true)]
    Resolve {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// The finding being disposed of
        finding: String,
        #[arg(long, value_enum)]
        status: DispositionStatus,
        /// Fixing commit, when one exists
        #[arg(long)]
        commit: Option<String>,
        /// What supports the disposition: a probe, a command, or the reasoning
        #[arg(long)]
        evidence: Option<String>,
        /// The verification event that justifies it: a full event ID on this
        /// change, recorded or reused. Prefixes are not resolved, and this
        /// neither implies nor is implied by --evidence
        #[arg(long, value_name = "ID")]
        evidence_event: Option<String>,
    },
    /// Read review state, or record a verdict with an optional findings batch
    Review {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// The conclusion recorded
        #[arg(long, value_enum)]
        verdict: Option<Verdict>,
        /// Emit the read view as a versioned JSON object
        #[arg(long, conflicts_with = "verdict")]
        json: bool,
        #[command(flatten)]
        body: BodyOpts,
        /// Snapshot the clean change worktree before recording the verdict
        #[arg(long)]
        snapshot: bool,
        /// Patchset under review, by id or by the revision it recorded.
        /// Defaults to the latest — which is what the verdict then claims,
        /// whatever the reviewer actually read
        #[arg(long)]
        patchset: Option<String>,
        /// Root cause of requested rework; repeat for a mixed round
        #[arg(long, value_enum)]
        cause: Vec<ReviewCause>,
        /// JSON array of findings ('-' for stdin); IDs are assigned by arc
        #[arg(long)]
        findings_json: Option<String>,
        /// What this verdict does to the verdicts already standing on the
        /// change. `supersedes` replaces them; `corroborates` supports one
        /// without becoming a second authority, which is what discharging a
        /// provisional approval is. Ignored when no verdict stands yet
        #[arg(long, value_enum, default_value = "supersedes")]
        relation: VerdictRelationKind,
        /// Say this verdict is owed corroboration, and why. It gates like any
        /// other verdict — independence and staleness are unchanged — but the
        /// change carries a recorded obligation until somebody else supplies
        /// a second judgment, and `arc query --provisional` finds it until
        /// they do. Use it when the reviewer's
        /// judgment has not been validated: an unproven model, a rushed pass,
        /// a reviewer outside their competence. arc never infers this; naming
        /// which reviewers are proven would be a routing opinion it does not
        /// hold
        #[arg(long, value_name = "REASON")]
        provisional: Option<String>,
        /// The routing version that selected this reviewer. Recorded as a
        /// coordinate and nothing else: arc joins it against no roster and
        /// reads no quality from it. Omitted, the review is unrouted
        #[arg(long = "route-version", value_name = "VERSION")]
        route_version: Option<String>,
    },
    /// Run a declared gate (or ad hoc command) and record the evidence. Gate
    /// evidence only counts at the change's own head, so a gate run from
    /// anywhere else is refused rather than recorded where status will ignore
    /// it; `--attest` records evidence arc did not run
    Verify {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Run every gate declared for the change profile
        #[arg(long)]
        all: bool,
        /// Run all declared gates concurrently and append evidence in name order
        #[arg(long)]
        parallel: bool,
        /// With --all, record reuse of passing evidence already green at the current head
        #[arg(long = "skip-green")]
        skip_green: bool,
        /// Gate name from .arc/gates.toml
        #[arg(long)]
        gate: Option<String>,
        /// Ad hoc command (recorded, but not a declared gate)
        #[arg(long)]
        command: Option<String>,
        /// Named acceptance probe declared by a brief
        #[arg(long)]
        probe: Option<String>,
        /// Brief version containing --probe (defaults to latest)
        #[arg(long)]
        brief_version: Option<usize>,
        /// Acceptance-probe evidence phase (defaults to final)
        #[arg(long, value_enum)]
        probe_phase: Option<ProbePhase>,
        /// Record externally observed evidence without running the command
        /// (e.g. a sandboxed executor or another host ran the gate)
        #[arg(long)]
        attest: bool,
        /// The attested result; required with --attest, rejected without it
        #[arg(long, value_enum)]
        result: Option<VerifyResult>,
        /// Revision actually tested; required with --attest
        #[arg(long)]
        tested_revision: Option<String>,
        /// Host or environment that executed the attested command
        #[arg(long)]
        execution_host: Option<String>,
        /// Stable identity of the external runner or job
        #[arg(long)]
        runner: Option<String>,
        /// Optional note recorded alongside the evidence
        #[arg(long)]
        note: Option<String>,
        /// Let evidence from a dirty worktree count, saying why.
        ///
        /// Dirt is fatal by default: a passing run whose tree no checkout
        /// reproduces is recorded and declines to satisfy the gate. The waiver
        /// binds the way the evidence binds — to this head alone — so the next
        /// commit ends it rather than leaving a standing exemption. It is
        /// visible to a reviewer, who is free to disagree with it.
        #[arg(long = "waive-dirty", value_name = "REASON")]
        waive_dirty: Option<String>,
        /// Earlier failing evidence for this same check that this run answers.
        ///
        /// A gate that has only ever passed and a gate watched to fail and
        /// then fixed leave the same record. Naming the failure separates
        /// them. The event must be a failing verification of the same gate or
        /// command on this change; its revision comes from the event itself.
        /// Requires --predicted, and is advisory: it changes no gate result,
        /// readiness decision, or exit code.
        #[arg(long = "falsified-by", value_name = "EVENT_ID")]
        falsified_by: Option<String>,
        /// Why the check was expected to fail, stated before it ran.
        ///
        /// A reason read off the failure afterwards restates the output; one
        /// stated beforehand is a claim that could have been wrong, which is
        /// what makes the pass that followed mean something. Requires
        /// --falsified-by.
        #[arg(long, value_name = "REASON")]
        predicted: Option<String>,
        /// Run every required gate against the merge with this branch, not
        /// against the change's own head.
        ///
        /// A change that is behind its target merges to content neither branch
        /// committed, and evidence at the head says nothing about it. This
        /// synthesizes that merge, checks it out on its own, runs the declared
        /// gates there, and records the result against the merged tree. The
        /// scratch checkout is removed whatever the gates do. The evidence is
        /// spent as soon as the target moves again, because that is a
        /// different merge.
        #[arg(long, value_name = "BRANCH")]
        against: Option<String>,
    },
    /// Finish implementation: snapshot, verify all gates, then print check state
    Done {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
    },
    /// Replay a change's branch onto its target, then snapshot the new head
    Rebase {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Run every required gate at the replayed head
        #[arg(long)]
        verify: bool,
    },
    /// Print shell exports for a detected harness session, for `eval`:
    /// `eval "$(arc env)"`.
    ///
    /// Detection reads the session variable a harness exports for itself —
    /// `CLAUDE_SESSION_ID` or `CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`,
    /// `OPENCODE_SESSION`, or `PI_SESSION_ID` — and then that harness's own
    /// session store for the model, and the effort where the store records
    /// one. Not every harness
    /// exports one, and a harness that does may not in every mode. OpenCode
    /// v2 exports none and is recognized by `OPENCODE_TERMINAL` or its
    /// process ancestry, printing the harness export with the session left
    /// as a comment to set by hand.
    ///
    /// With nothing to detect at all it prints the export template as a
    /// comment and exits non-zero, which is a report that identity must be
    /// set by hand rather than a failure. Every value it emits can be set
    /// directly: explicit identity always wins over a detected one
    Env,
    /// Print a shell completion script to stdout
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Render the man page (arc.1) into a directory
    Mangen {
        /// Output directory (created if absent)
        out_dir: std::path::PathBuf,
    },
    /// Resume one change with its brief, live state, and journal context
    Resume {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
        /// Print one dotted object-key/array-index path
        #[arg(long, conflicts_with = "fields")]
        get: Option<String>,
        /// Print a top-level JSON field subset
        #[arg(long, conflicts_with = "get")]
        fields: Option<String>,
    },
    /// Recover work abandoned by another session
    Rescue {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
        /// Include the claimed session's sensitive transcript
        #[arg(long)]
        transcript: bool,
        /// Maximum transcript turns to include
        #[arg(long, default_value_t = 5, requires = "transcript")]
        tail: usize,
        /// Take over another session's stale or expired claim
        #[arg(long)]
        take: bool,
    },
    /// Print one stable statusline summary for the current change
    Prompt {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
    },
    /// Fork this repository: a worktree on a fork/<slug> branch, outside
    /// the change lifecycle. Unintegrated by intent; the operator decides
    /// what to merge, rebase, or discard
    Fork {
        #[command(subcommand)]
        command: ForkCmd,
    },
    /// Set an integration hold
    Hold {
        /// Change whose integration is held
        change: String,
        /// Why integration is being held, recorded with the hold
        #[arg(long, required_unless_present = "reason_file")]
        reason: Option<String>,
        /// Read the hold reason from a file ('-' for stdin)
        #[arg(long, conflicts_with = "reason")]
        reason_file: Option<String>,
    },
    /// Release one hold by the event that set it (a unique prefix is enough)
    ReleaseHold {
        /// Change whose hold is released
        change: String,
        /// The hold event to release (a unique prefix is enough)
        hold: String,
        /// Why the hold is being released, recorded with the release
        #[arg(long)]
        reason: Option<String>,
    },
    /// Guarded merge of one change or a dependency-ordered tagged series
    Integrate {
        /// Change to integrate; omit only when selecting with --tag
        change: Option<String>,
        /// Integrate every change carrying all supplied tags, in dependency order
        #[arg(long)]
        tag: Vec<String>,
        /// Merge into this branch instead of the recorded target
        #[arg(long)]
        into: Option<String>,
        /// Merge commit message (defaults to "merge(<slug>): <title>")
        #[arg(long)]
        message: Option<String>,
        /// Remove the change worktree and branch after a verified merge
        #[arg(long)]
        cleanup: bool,
        /// Report what would happen without merging, closing, or writing
        #[arg(long)]
        dry_run: bool,
        /// Integrate without an independent verdict, recording the review this
        /// change still owes. It stands in for a verdict nobody recorded — and
        /// for a self-approval policy would reject — in the same invocation.
        /// It never overrules a reviewer who read this patchset and asked for
        /// changes: that is a verdict, not a missing one. The obligation
        /// survives closure and `arc query --debt` finds it; discharge it
        /// with `arc audit`.
        #[arg(long = "debt", value_name = "REASON")]
        debt: Option<String>,
        /// What kind of deficit the debt records. Omitted, arc derives it from
        /// the ledger; a declared kind wins, because arc cannot tell a merge
        /// resolution from a repair
        #[arg(long = "kind", value_enum, requires = "debt")]
        debt_kind: Option<DebtMissing>,
    },
    /// Record a review obligation this change carries but has not discharged
    Debt {
        /// Change that owes the review
        change: String,
        /// What review is owed, and why it could not run
        #[arg(long)]
        reason: String,
        /// What kind of deficit this records. Omitted, arc derives it from the
        /// ledger; a declared kind wins, because arc cannot tell a merge
        /// resolution from a repair
        #[arg(long, value_enum)]
        kind: Option<DebtMissing>,
    },
    /// Record a review performed after integration (never a late verdict)
    Audit {
        /// Change whose integrated revision was reviewed
        change: String,
        #[arg(long, value_enum)]
        verdict: Verdict,
        /// Inline body text
        #[arg(long)]
        body: Option<String>,
        /// Read body from file ('-' for stdin)
        #[arg(long, conflicts_with = "body")]
        body_file: Option<String>,
        /// Findings batch as JSON ('-' for stdin)
        #[arg(long = "findings-json")]
        findings_json: Option<String>,
        /// The routing version that selected this auditor. Recorded as a
        /// coordinate and nothing else. Omitted, the audit is unrouted
        #[arg(long = "route-version", value_name = "VERSION")]
        route_version: Option<String>,
    },
    /// Close a change without arc performing the merge
    Close {
        /// Change to close
        change: String,
        /// Assert an integration arc did not perform, at this revision. Carries
        /// no authorization: arc did not guard this merge
        #[arg(long = "assert-integrated")]
        assert_integrated: Option<String>,
        /// The patchset that was integrated (defaults to the latest)
        #[arg(long, requires = "assert_integrated")]
        patchset: Option<String>,
        /// The branch it was integrated into (defaults to the target branch)
        #[arg(long, requires = "assert_integrated")]
        into: Option<String>,
        /// Where the target stood before. Read from a merge commit's first
        /// parent; a fast-forward has none to read, so name it or the event
        /// records no base
        #[arg(long = "target-before", requires = "assert_integrated")]
        target_before: Option<String>,
        /// Close as abandoned: the work stopped and nothing was merged
        #[arg(long)]
        abandoned: bool,
        /// Superseded by another change
        #[arg(long)]
        superseded: Option<String>,
    },
    /// Record a Git history rewrite that happened to this repository
    History {
        #[command(subcommand)]
        cmd: HistoryCmd,
    },
    /// Record and list caller-declared review passes
    Pass {
        #[command(subcommand)]
        cmd: PassCmd,
    },
    /// Record delegated run dispatches and their terminal outcomes
    Run {
        #[command(subcommand)]
        cmd: RunCmd,
    },
    /// Record and validate observed forge (hosted-PR) facts
    Forge {
        #[command(subcommand)]
        cmd: ForgeCmd,
    },
    /// Show the resolved configuration and store location as JSON
    Config {
        /// Probe every local path required to write the ledger
        #[arg(long)]
        check_writable: bool,
        /// Emit the writability probe as structured JSON
        #[arg(long)]
        json: bool,
    },
    /// Check the append-only ledger for malformed or stale state (read-only)
    Doctor {
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
        /// Show every item behind grouped advice
        #[arg(long, conflicts_with = "json")]
        verbose: bool,
    },
    /// Manage the opt-in Git hook pack (never installed automatically)
    Hooks {
        #[command(subcommand)]
        cmd: HooksCmd,
    },
    /// Aggregate changes, inboxes, or backlog across every known project
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    /// Advise (never execute) rebases for open dependents of a change
    Restack {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Print the rebase commands without running them
        #[arg(long)]
        advise: bool,
    },
    /// Internal hook entry point invoked by installed hook scripts
    #[command(hide = true)]
    HookRun {
        /// Hook name (e.g. post-commit, prepare-commit-msg)
        name: String,
        /// Remaining hook arguments, passed through verbatim
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Orient a session: the ledger queue, the journal backlog, and live lanes
    Catchup {
        /// Cap the changes listed per ledger bucket; the journal queue is
        /// always rendered in full, since finding it is the point
        #[arg(long, default_value = "10")]
        limit: usize,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// File a feature request in the project journal: the discoverable alias
    /// for `arc journal note --kind feature-request`. Read the queue back with
    /// `arc journal open` or `arc journal list --kind feature-request`
    Fr {
        /// The alias mounts the kind verb's own arguments rather than
        /// restating them, so the two cannot drift apart.
        #[command(flatten)]
        write: journal::KindWrite,
    },
    /// Cross-harness project journal mechanics (plain Markdown stays the contract)
    Journal {
        #[command(subcommand)]
        cmd: journal::JournalCmd,
    },
}

#[derive(Subcommand)]
enum ForkCmd {
    /// Create the fork worktree and journal its marker
    Begin {
        /// Kebab-case slug naming the fork (and the fork/<slug> branch)
        slug: String,
        /// Branch to fork from; omitted, the current branch (master when
        /// standing on a fork, which is not a base)
        #[arg(long)]
        from: Option<String>,
    },
    /// Journal a marker for a hand-made fork/<slug> worktree
    Adopt {
        /// The fork slug — the part of the branch name after fork/
        slug: String,
        /// What the fork is for, recorded in the marker
        #[arg(long)]
        intent: Option<String>,
    },
    /// Record the fork's disposition and remove its worktree
    Retire {
        /// The fork slug
        slug: String,
        /// The disposition: merged, dropped, kept — with a word of why
        outcome: String,
        /// Keep the worktree on disk; the default removes it, the branch
        /// always stays
        #[arg(long)]
        keep_worktree: bool,
        /// Discard untracked work the removal refuses to destroy. The
        /// operator's decision, never arc's
        #[arg(long)]
        force: bool,
    },
    /// List every fork this repository knows about
    List {
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// Who opened this fork, and how to reopen their session
    ///
    /// The marker records the harness, session, model, and actor that made
    /// the fork. A field the marker does not carry prints as absent, and a
    /// resume line appears only for a harness whose resume form is stable.
    Thread {
        /// The fork slug — the part of the branch name after fork/
        slug: String,
    },
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// Per-repo open-change rows across the data_root
    List {
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// The inbox rollup for every repo under the data_root
    Inbox {
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// Ledger and journal backlog across every project, ranked by what is
    /// blocked on a decision rather than on work
    Backlog {
        /// Count only journal items filed at or after this journal stamp
        /// (20260101T000000Z) or RFC 3339 timestamp, so the tiers read as
        /// arrivals rather than as what is outstanding. Changes awaiting a
        /// verdict and debt are always reported in full: a blocker
        /// matters more the longer it has been one
        #[arg(long)]
        since: Option<String>,
        /// Name every actionable artifact, instead of counting them
        #[arg(long)]
        items: bool,
        /// Report only projects whose canonical anchor is beneath this path
        #[arg(long, value_name = "PATH", conflicts_with_all = ["here", "global"])]
        under: Option<PathBuf>,
        /// Report only projects beneath the current directory
        #[arg(long, conflicts_with_all = ["under", "global"])]
        here: bool,
        /// Report every registered project, the default when no scope is set
        #[arg(long, conflicts_with_all = ["under", "here"])]
        global: bool,
        /// Name every unreachable journal, including temporary and scratch anchors
        #[arg(long)]
        unreachable: bool,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum HooksCmd {
    /// Install the arc hook scripts into this repository's hooks dir
    Install {
        /// Replace a foreign hook, saving it as <hook>.pre-arc
        #[arg(long)]
        force: bool,
    },
    /// Remove arc-authored hook scripts (leaves foreign hooks untouched)
    Uninstall,
    /// Report which hooks are installed and whether arc manages them
    Status,
}

#[derive(Subcommand)]
enum ForgeCmd {
    /// Declare the explicit projection tuple and policy for a change
    Declare {
        /// Change the projection is declared for
        change: String,
        /// Forge host the pull request will live on (e.g. github.com)
        #[arg(long)]
        host: String,
        /// Repository the pull request merges into, as owner/name
        #[arg(long = "base-repo")]
        base_repo: String,
        /// Branch the pull request merges into
        #[arg(long = "base-ref")]
        base_ref: String,
        /// Repository the pull request is opened from, as owner/name
        #[arg(long = "head-repo")]
        head_repo: String,
        /// Branch the pull request is opened from
        #[arg(long = "head-ref")]
        head_ref: String,
        /// same-repository-only (default) | allowed-base-repo=<owner/name>
        #[arg(long, default_value = "same-repository-only")]
        policy: String,
    },
    /// Record the observed post-creation PR tuple (validated, fail-closed)
    Link {
        /// Change the pull request was opened for
        change: String,
        /// Pull request number as the forge assigned it
        #[arg(long)]
        pr: u64,
        /// Canonical URL of the pull request
        #[arg(long)]
        url: String,
        /// Repository it merges into, as observed, in owner/name form
        #[arg(long = "base-repo")]
        base_repo: String,
        /// Branch it merges into, as observed
        #[arg(long = "base-ref")]
        base_ref: String,
        /// Repository it was opened from, as observed, in owner/name form
        #[arg(long = "head-repo")]
        head_repo: String,
        /// Branch it was opened from, as observed
        #[arg(long = "head-ref")]
        head_ref: String,
        /// Exact commit at the pull request head when it was read
        #[arg(long = "head-sha")]
        head_sha: String,
    },
    /// Record the observed hosted-check rollup at an exact PR head
    Checks {
        /// Change whose hosted checks were read
        change: String,
        /// Exact commit the rollup was read at
        #[arg(long = "pr-head")]
        pr_head: String,
        /// The rollup the forge reported
        #[arg(long, value_enum)]
        state: forge::ForgeCheckState,
        /// Free-text detail recorded with the rollup, such as a failing job
        #[arg(long)]
        detail: Option<String>,
    },
    /// Record the observed PR lifecycle state
    PrState {
        /// Change whose pull request state was read
        change: String,
        /// The lifecycle state the forge reported
        #[arg(long, value_enum)]
        state: forge::ForgePrState,
        /// Required when state is merged
        #[arg(long = "merge-sha")]
        merge_sha: Option<String>,
        /// The forge-link event this state was read at (defaults to the
        /// current link); the head is taken from that link
        #[arg(long)]
        link: Option<String>,
    },
}

#[derive(Subcommand)]
enum HistoryCmd {
    /// Record a rewrite the operator performed, with its commit map. arc never
    /// rewrites history, offers to, or computes the mapping
    Rewrite {
        /// Commit map (`<old> <new>` per line, as git filter-repo writes), or
        /// '-' for stdin
        #[arg(long)]
        map: String,
        /// Why the history was rewritten
        #[arg(long)]
        reason: String,
        /// What performed the rewrite
        #[arg(long)]
        tool: Option<String>,
    },
    /// Show where a recorded revision ended up
    Resolve {
        /// A revision a rewrite may have moved; the surviving one is printed
        revision: String,
    },
}

#[derive(Subcommand)]
enum PassCmd {
    /// Declare the exact change and patchset members of a review pass
    Open {
        /// Exact change and patchset reference, repeated for every member
        #[arg(long = "member", required = true)]
        member: Vec<String>,
        /// Optional note about the declared pass
        #[arg(long)]
        note: Option<String>,
    },
    /// Declare that a review pass ended successfully
    Complete {
        /// Pass ID printed by arc pass open
        pass_id: String,
        /// Optional note about the completed pass
        #[arg(long)]
        note: Option<String>,
    },
    /// Declare that a review pass ended without completion
    Abandon {
        /// Pass ID printed by arc pass open
        pass_id: String,
        /// Why the pass was abandoned
        #[arg(long, required = true)]
        reason: String,
    },
    /// List every recorded review pass, newest first
    List {
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum RunCmd {
    /// Record that a caller dispatched a run through a resolved route.
    /// Exactly one subject is named: rounds are numbered within it, so a run
    /// against nothing belongs to no sequence
    Dispatch {
        /// Resolved route used for dispatch
        #[arg(long)]
        route: String,
        /// Worktree path given to the run
        #[arg(long)]
        worktree: String,
        /// Change the run is against
        #[arg(long, id = "change_flag")]
        change: Option<String>,
        /// Fork slug the run is against, for work outside the lifecycle
        #[arg(long)]
        fork: Option<String>,
        /// Commit range the run is against, as <base>..<head>, for work with
        /// no ledger change
        #[arg(long)]
        range: Option<String>,
        /// Brief event given to the run, when one exists
        #[arg(long = "brief-event")]
        brief_event_id: Option<String>,
        /// Free-text dispatch note
        #[arg(long)]
        note: Option<String>,
    },
    /// Record the terminal outcome of a dispatched run, with what the round
    /// reviewed, raised, and deliberately left
    End {
        /// RunDispatched event ID being closed
        dispatch_event_id: String,
        /// Terminal outcome supplied by the caller
        #[arg(long, value_enum)]
        outcome: RunOutcome,
        /// Revision the round reviewed
        #[arg(long = "reviewed-head")]
        reviewed_head: Option<String>,
        /// Findings raised for repair: a JSON array of objects with a
        /// `summary` and an optional `severity`, from a file or '-' for stdin
        #[arg(long = "raised-json")]
        raised_json: Option<String>,
        /// Findings deferred: the same array, each object additionally
        /// carrying a required `why` and an optional `id` (one is minted when
        /// absent)
        #[arg(long = "deferred-json")]
        deferred_json: Option<String>,
        /// Deferral this round takes up, by ID; repeat for each. The deferral
        /// must be open on the same subject
        #[arg(long)]
        collects: Vec<String>,
        /// Free-text ending note
        #[arg(long)]
        note: Option<String>,
    },
    /// List every dispatched run grouped by subject, with rounds numbered and
    /// deferrals still open
    List {
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
}

fn role_refusal(role: ExecutionRole, command: &Cmd) -> Option<(&'static str, &'static str)> {
    // Brief reads are open to every role, while writes are lead-only; the
    // handler must inspect --body-file, so Brief cannot live in this deny-list.
    match role {
        ExecutionRole::Lead => None,
        ExecutionRole::Reviewer => match command {
            Cmd::Integrate { .. } => Some(("integrate", "lead")),
            Cmd::Debt { .. } => Some(("debt", "lead")),
            Cmd::Close { .. } => Some(("close", "lead")),
            _ => None,
        },
        ExecutionRole::Implementer => match command {
            Cmd::Review {
                verdict: Some(_), ..
            } => Some(("review", "reviewer or lead")),
            Cmd::Resolve { .. } => Some(("resolve", "reviewer or lead")),
            Cmd::Hold { .. } => Some(("hold", "reviewer or lead")),
            Cmd::ReleaseHold { .. } => Some(("release-hold", "reviewer or lead")),
            Cmd::Audit { .. } => Some(("audit", "reviewer or lead")),
            Cmd::Debt { .. } => Some(("debt", "lead")),
            Cmd::Close { .. } => Some(("close", "lead")),
            Cmd::Integrate { .. } => Some(("integrate", "lead")),
            _ => None,
        },
    }
}

fn nested_subcommand_path(typed: Option<&str>) -> Option<&'static str> {
    match typed {
        Some("dir") => Some("journal dir"),
        Some("note") => Some("journal note"),
        Some("append") => Some("journal position"),
        Some("memories") => Some("journal memories"),
        Some("open") => Some("journal open"),
        Some("consume") => Some("journal consume"),
        Some("archive") => Some("journal archive"),
        Some("stamp") => Some("journal stamp"),
        Some("lane") => Some("journal lane"),
        Some("discussion") => Some("journal discussion"),
        // Every kind is a verb under `arc journal`, so typing the kind at the
        // top level is the likeliest miss a cold session makes. The set is
        // closed, which makes this a lookup rather than a guess. Names that
        // are already top-level commands — `review`, `log`, `list`, `show` —
        // are deliberately absent: they resolve, and redirecting them would
        // be wrong.
        Some("feature-request") => Some("journal feature-request"),
        Some("todo") => Some("journal todo"),
        Some("handoff") => Some("journal handoff"),
        Some("plan") => Some("journal plan"),
        Some("conclusion") => Some("journal conclusion"),
        Some("decision") => Some("journal decision"),
        Some("memory") => Some("journal memory"),
        Some("later") => Some("journal later"),
        Some("question") => Some("journal question"),
        Some("questions") => Some("journal questions"),
        Some("answer") => Some("journal answer"),
        Some("position") => Some("journal position"),
        Some("latest") => Some("journal latest"),
        Some("source") => Some("journal source"),
        Some("source-attach") => Some("journal source-attach"),
        Some("install") => Some("hooks install"),
        Some("uninstall") => Some("hooks uninstall"),
        _ => None,
    }
}

fn main() {
    // Rust ignores SIGPIPE by default, so a downstream reader closing the
    // pipe (`arc list --format compact | head`) surfaces as a panic on the
    // next write instead of a clean exit. arc is a pipeline citizen and must
    // die silently like git or cat, so restore the default SIGPIPE
    // disposition before any output can be produced.
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let kind = error.kind();
            let typed = std::env::args().nth(1);
            // An exact redirect replaces clap's guess rather than printing
            // beside it. Its similarity search reaches for whatever is
            // closest in spelling — it answers `questions` with
            // `completions` — so two tips would leave the caller choosing
            // between a right one and a wrong one.
            let redirect = (kind == clap::error::ErrorKind::InvalidSubcommand)
                .then(|| nested_subcommand_path(typed.as_deref()))
                .flatten();
            match redirect {
                Some(path) => {
                    eprintln!(
                        concat!(
                            "error: unrecognized subcommand '{}'\n\n",
                            "  tip: it lives under another command: 'arc {}'\n\n",
                            "For more information, try '--help'.\n"
                        ),
                        typed.as_deref().unwrap_or(""),
                        path
                    );
                }
                None => {
                    error.print().ok();
                }
            }
            std::process::exit(error.exit_code());
        }
    };
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
    }
}

fn run(cli: Cli) -> Result<i32> {
    let role = ExecutionRole::parse(cli.role.as_deref())?;
    // No subcommand is not an error: it is the request to be oriented.
    let Some(cmd) = cli.cmd else {
        guide::print();
        return Ok(0);
    };
    if let Some((command, required)) = role_refusal(role, &cmd) {
        eprintln!(
            "role refusal: {} may not {command} (requires {required})",
            role.as_str()
        );
        return Ok(9);
    }

    let cwd = std::env::current_dir()?;
    // The environment is read here rather than through clap's `env`, so that
    // the flag and the variable stay distinguishable even when they carry the
    // same value.
    let from_env = std::env::var("ARC_ACTOR")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let (actor, actor_source) = match (cli.actor.filter(|value| !value.trim().is_empty()), from_env)
    {
        (Some(declared), _) => (declared, ActorSource::Flag),
        (None, Some(declared)) => (declared, ActorSource::Env),
        (None, None) => (
            gitio::git(&cwd, &["config", "user.name"])
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "unknown".into()),
            ActorSource::GitFallback,
        ),
    };
    let mut harness = cli.harness;
    let mut session = cli.session;
    // An empty --model is the same as absent.
    let mut model = cli.model.filter(|value| !value.trim().is_empty());
    if config::load()
        .map(|config| config.identity_detect)
        .unwrap_or(false)
    {
        if let Some(detected) = context::detect_identity() {
            if harness
                .as_deref()
                .is_none_or(|explicit| explicit == detected.harness)
            {
                harness.get_or_insert(detected.harness);
                // A harness recognized without its cooperation carries no
                // session id; recording the harness alone is the honest half
                // of the detection, not a partial failure.
                if let Some(detected_session) = detected.session {
                    session.get_or_insert(detected_session);
                }
                if model.is_none() {
                    model = detected.model;
                }
            }
        }
    }
    let ctx = Ctx {
        cwd,
        actor,
        actor_source,
        fallback_announced: std::cell::Cell::new(false),
        harness,
        session,
        model,
        // An empty --on-behalf-of is the same as absent: today's behavior.
        on_behalf_of: cli.on_behalf_of.filter(|value| !value.trim().is_empty()),
    };

    // Neighbouring commands take the change as a flag, so `--change` works
    // wherever the positional is optional rather than being guessed at against
    // clap's nearest-option suggestion.
    let flag_change = cli.change;
    // The change a command was pointed at, however it was spelled. Two
    // spellings naming different changes is a mistake, not a precedence
    // question — but a slug, an ID, and a unique prefix of one change are one
    // reference, so they are compared after resolution.
    let select = |positional: Option<String>| -> Result<Option<String>> {
        let (Some(positional), Some(flag)) = (&positional, &flag_change) else {
            return Ok(positional.or_else(|| flag_change.clone()));
        };
        let store = store::Store::discover(&ctx.cwd)?;
        let (left, right) = (
            store.resolve_change(positional)?,
            store.resolve_change(flag)?,
        );
        if left != right {
            bail!("change given twice and they disagree: {positional:?} as an argument, {flag:?} as --change");
        }
        Ok(Some(left))
    };
    let infer = |change: Option<&str>| -> Result<String> {
        let selected = select(change.map(str::to_string))?;
        let store = store::Store::discover(&ctx.cwd)?;
        context::resolve_change_or_infer(&store, &ctx.cwd, selected.as_deref())
    };
    // Which store a subject positional addresses. Only an explicit name can
    // be an artifact: an omitted subject is inferred from the branch, and a
    // branch names a change.
    let artifact = |ctx: &Ctx, subject: Option<&str>| -> Option<String> {
        subject.and_then(|subject| journal::artifact_subject(&ctx.cwd, subject))
    };

    match cmd {
        Cmd::Begin {
            slug,
            title,
            profile,
            target,
            base,
            branch,
            worktree,
            no_worktree,
            adopt,
            blocked_by,
            tag,
            from_journal,
            dangerous,
            iterating,
        } => {
            commands::begin(
                &ctx,
                &slug,
                title,
                &profile,
                target,
                base,
                branch,
                worktree,
                no_worktree,
                adopt,
                blocked_by,
                tag,
                from_journal,
                dangerous,
                iterating,
            )?;
            Ok(0)
        }
        Cmd::List { open, json, format } => {
            commands::list(&ctx, open, json, format)?;
            Ok(0)
        }
        Cmd::Query {
            status,
            target,
            tag,
            verdict,
            actor,
            harness,
            commit,
            debt,
            provisional,
            json,
        } => {
            if let Some(commit) = commit {
                commands::query_commit(&ctx, &commit)?;
            } else {
                commands::query(
                    &ctx,
                    QueryArgs {
                        status,
                        target,
                        tags: tag,
                        verdict,
                        actor,
                        harness,
                        debt,
                        provisional,
                        json,
                    },
                )?;
            }
            Ok(0)
        }
        Cmd::Show {
            change,
            tag,
            json,
            at,
        } => {
            let change = if tag.is_empty() {
                Some(infer(change.as_deref())?)
            } else {
                // With --tag the command refuses a change; the flag has to
                // reach it to be refused.
                select(change)?
            };
            commands::show_selection(&ctx, role, change.as_deref(), tag, json, at.as_deref())?;
            Ok(0)
        }
        Cmd::Log {
            change,
            reverse,
            oneline,
        } => {
            if oneline {
                eprintln!(
                    "tip: arc log already prints one line per ledger fact; \
                     for commits, use git log --oneline"
                );
            }
            let change = infer(change.as_deref())?;
            commands::log(&ctx, &change, reverse)?;
            Ok(0)
        }
        Cmd::Stats {
            change,
            tag,
            all,
            by_model,
            json,
        } => {
            // clap rejects the pair on the subcommand, but a global `--change`
            // placed before it never reaches that check.
            if all && (change.is_some() || tag.is_some()) {
                bail!("--all reports every change; it cannot be combined with --change or --tag");
            }
            let selection = match (change, tag) {
                (Some(change), None) => commands::StatsSelection::Change(change),
                (None, Some(tag)) => commands::StatsSelection::Tag(tag),
                (None, None) => commands::StatsSelection::All,
                // clap rejects the pair on the subcommand, but a global
                // `--change` placed before it never reaches that check.
                (Some(_), Some(_)) => bail!("--change and --tag are mutually exclusive"),
            };
            commands::stats(&ctx, selection, json, by_model)?;
            Ok(0)
        }
        Cmd::Diff {
            change,
            patchset,
            stat,
            findings,
            between,
            since_approved,
            integrated,
            base,
            paths,
        } => {
            let change = infer(change.as_deref())?;
            commands::diff(
                &ctx,
                &change,
                commands::DiffArgs {
                    patchset,
                    stat,
                    findings,
                    between,
                    since_approved,
                    integrated,
                    base,
                    paths,
                },
            )?;
            Ok(0)
        }
        Cmd::Findings {
            change,
            format,
            audit,
        } => {
            let change = infer(change.as_deref())?;
            commands::findings(&ctx, &change, format, audit)?;
            Ok(0)
        }
        Cmd::Brief {
            change,
            body_file,
            title,
            base,
            version,
            scaffold,
            plan_ref,
            plan_slice,
            probes_json,
            caused_by,
            cause_note,
        } => {
            let change = infer(change.as_deref())?;
            commands::brief(
                &ctx,
                role,
                &change,
                body_file,
                title,
                base,
                version,
                scaffold,
                plan_ref,
                plan_slice,
                probes_json,
                caused_by,
                cause_note,
            )
        }
        Cmd::Changelog {
            change,
            category,
            body_file,
            json,
            provenance,
            since,
            write,
        } => commands::changelog(
            &ctx,
            role,
            select(change)?.as_deref(),
            category,
            body_file,
            json,
            provenance,
            since,
            write,
        ),
        Cmd::Message {
            change,
            message_type,
            summary,
            detail,
            json,
            severity,
        } => {
            commands::message(&ctx, &change, message_type, summary, detail, json, severity)?;
            Ok(0)
        }
        Cmd::Messages {
            change,
            message_type,
            severity,
            since,
            json,
        } => {
            commands::messages(&ctx, change.as_deref(), message_type, severity, since, json)?;
            Ok(0)
        }
        Cmd::Inbox { assigned_to, json } => {
            commands::inbox(&ctx, assigned_to, json)?;
            Ok(0)
        }
        Cmd::Chain { tag, json, review } => {
            commands::chain(&ctx, tag, json, review)?;
            Ok(0)
        }
        Cmd::Take { tag, ttl, json } => commands::take(&ctx, tag, ttl, json),
        Cmd::Metadata {
            change,
            blocked_by,
            remove_blocked_by,
            tag,
            remove_tag,
            assign,
            priority,
            json,
        } => {
            let has_mutation = !blocked_by.is_empty()
                || !remove_blocked_by.is_empty()
                || !tag.is_empty()
                || !remove_tag.is_empty()
                || assign.is_some()
                || priority.is_some();
            if json && has_mutation {
                anyhow::bail!("--json cannot be combined with metadata mutation flags");
            }
            if has_mutation {
                commands::metadata(
                    &ctx,
                    &change,
                    blocked_by,
                    remove_blocked_by,
                    tag,
                    remove_tag,
                    assign,
                    priority,
                )?;
            } else {
                commands::read_metadata(&ctx, &change, json)?;
            }
            Ok(0)
        }
        Cmd::Iterating { change, off } => {
            commands::iterating(&ctx, &change, off)?;
            Ok(0)
        }
        Cmd::Status {
            change,
            json: _,
            get,
            fields,
            at,
        } => {
            let change = infer(change.as_deref())?;
            commands::status_cmd(
                &ctx,
                &change,
                get.as_deref(),
                fields.as_deref(),
                at.as_deref(),
            )?;
            Ok(0)
        }
        Cmd::BlockerStatus { change } => {
            let change = infer(change.as_deref())?;
            commands::blocker_status_cmd(&ctx, &change)?;
            Ok(0)
        }
        Cmd::IsBlocked { change } => {
            match infer(change.as_deref()).and_then(|change| commands::is_blocked(&ctx, &change)) {
                Ok(code) => Ok(code),
                Err(error) => {
                    eprintln!("error: {error:#}");
                    Ok(2)
                }
            }
        }
        Cmd::Events {
            follow,
            change,
            tag,
            repository,
            event_type,
            since,
            exec_command,
        } => {
            commands::events(
                &ctx,
                commands::EventsArgs {
                    follow,
                    change: change.as_deref(),
                    tags: &tag,
                    repository_scope: repository,
                    event_type: event_type.as_deref(),
                    since,
                    exec_command: exec_command.as_deref(),
                },
            )?;
            Ok(0)
        }
        Cmd::Watch {
            change,
            tag,
            any,
            all,
            until,
            timeout,
            exec_command,
            json,
        } => {
            let quorum = match (any, all) {
                (true, false) => Some(commands::WatchQuorum::Any),
                (false, true) => Some(commands::WatchQuorum::All),
                _ => None,
            };
            commands::watch(
                &ctx,
                select(change)?.as_deref(),
                commands::WatchArgs {
                    tags: &tag,
                    quorum,
                    until: &until,
                    timeout_secs: timeout,
                    exec_command: exec_command.as_deref(),
                    json,
                },
            )
        }
        Cmd::Export { change, output } => {
            commands::export_bundle(&ctx, &change, &output)?;
            Ok(0)
        }
        Cmd::Import { input, dry_run } => commands::import_bundle(&ctx, &input, dry_run),
        Cmd::Check {
            change,
            tag,
            explain,
            json,
        } => {
            let change = if tag.is_empty() {
                Some(infer(change.as_deref())?)
            } else {
                // With --tag the command refuses a change; the flag has to
                // reach it to be refused.
                select(change)?
            };
            commands::check_selection(&ctx, change.as_deref(), tag, explain, json)
        }
        Cmd::Claim {
            change,
            ttl,
            stage_budget,
            takeover,
        } => match artifact(&ctx, change.as_deref()) {
            Some(file) => {
                if !stage_budget.is_empty() {
                    bail!(
                        "--stage-budget applies to a change; an artifact's lease is the \
                         whole of what expires"
                    );
                }
                journal::claim_artifact(&ctx, &file, ttl.as_deref(), takeover)
            }
            None => {
                let change = infer(change.as_deref())?;
                commands::claim(&ctx, &change, ttl, stage_budget, takeover)
            }
        },
        Cmd::ReleaseClaim { change, outcome } => match artifact(&ctx, change.as_deref()) {
            Some(file) => {
                journal::release_artifact_claim(&ctx, &file, outcome.as_deref().unwrap_or("paused"))
            }
            None => {
                if outcome.is_some() {
                    bail!(
                        "--outcome applies to a journal artifact; a change claim is released \
                         without one because the change records its own lifecycle"
                    );
                }
                let change = infer(change.as_deref())?;
                commands::release_claim(&ctx, &change)
            }
        },
        Cmd::Stage {
            change,
            stage,
            claim,
            note,
            note_file,
            blocker,
        } => {
            let note = match (note, note_file) {
                (None, None) => None,
                (note, note_file) => Some(commands::read_body(note, note_file)?),
            };
            match artifact(&ctx, change.as_deref()) {
                Some(file) => {
                    journal::stage_artifact(&ctx, &file, stage.into(), note, blocker, claim)
                }
                None => {
                    let change = infer(change.as_deref())?;
                    commands::stage(&ctx, &change, stage, note, blocker, claim)
                }
            }
        }
        Cmd::Snapshot {
            change,
            base,
            brief_version,
            verify,
            gate,
            all,
            contributors,
            solo,
            amend,
        } => {
            let change = infer(change.as_deref())?;
            if let Some(patchset) = amend {
                commands::review::amend_attribution(&ctx, &change, patchset, contributors, solo)?;
                Ok(0)
            } else {
                commands::snapshot_with_verify(
                    &ctx,
                    &change,
                    base,
                    brief_version,
                    verify,
                    gate,
                    all,
                    contributors,
                    solo,
                )
            }
        }
        Cmd::Keep {
            kind,
            change,
            body,
            evidence,
        } => {
            let change = infer(change.as_deref())?;
            let text = commands::read_body(body.body, body.body_file)?;
            commands::keep(&ctx, &change, kind.into(), text, evidence)?;
            Ok(0)
        }
        Cmd::Comment {
            change,
            body,
            patchset,
            anchor,
        } => {
            let change = infer(change.as_deref())?;
            let text = commands::read_body(body.body, body.body_file)?;
            commands::comment(&ctx, &change, text, patchset, &anchor.to_args())?;
            Ok(0)
        }
        Cmd::Finding {
            change,
            summary,
            body,
            blocking,
            severity,
            patchset,
            anchor,
        } => {
            let change = infer(change.as_deref())?;
            let text = match (&body.body, &body.body_file) {
                (None, None) => None,
                _ => Some(commands::read_body(body.body, body.body_file)?),
            };
            commands::finding(
                &ctx,
                &change,
                summary,
                text,
                blocking,
                severity,
                patchset,
                &anchor.to_args(),
            )?;
            Ok(0)
        }
        Cmd::Reply {
            change,
            event_id,
            body,
        } => {
            let change = infer(change.as_deref())?;
            let text = commands::read_body(body.body, body.body_file)?;
            commands::reply(&ctx, &change, event_id, text)?;
            Ok(0)
        }
        Cmd::Resolve {
            change,
            finding,
            status,
            commit,
            evidence,
            evidence_event,
        } => {
            let change = infer(change.as_deref())?;
            commands::resolve(
                &ctx,
                &change,
                finding,
                status,
                commit,
                evidence,
                evidence_event,
            )?;
            Ok(0)
        }
        Cmd::Review {
            change,
            verdict,
            relation,
            json,
            body,
            snapshot,
            patchset,
            cause,
            findings_json,
            provisional,
            route_version,
        } => {
            let change = infer(change.as_deref())?;
            if let Some(verdict) = verdict {
                let body = match (&body.body, &body.body_file) {
                    (None, None) => None,
                    _ => Some(commands::read_body(body.body, body.body_file)?),
                };
                commands::review(
                    &ctx,
                    &change,
                    commands::ReviewArgs {
                        verdict,
                        relation,
                        body,
                        patchset,
                        causes: cause,
                        findings_json,
                        snapshot_first: snapshot,
                        provisional,
                        route_version,
                    },
                )?;
            } else {
                if body.body.is_some()
                    || body.body_file.is_some()
                    || snapshot
                    || patchset.is_some()
                    || !cause.is_empty()
                    || findings_json.is_some()
                {
                    anyhow::bail!("--verdict is required for the review write path");
                }
                commands::read_review(&ctx, &change, json)?;
            }
            Ok(0)
        }
        Cmd::Verify {
            change,
            all,
            parallel,
            skip_green,
            gate,
            command,
            probe,
            brief_version,
            probe_phase,
            attest,
            result,
            tested_revision,
            execution_host,
            runner,
            note,
            waive_dirty,
            falsified_by,
            predicted,
            against,
        } => {
            let change = infer(change.as_deref())?;
            commands::verify(
                &ctx,
                &change,
                commands::VerifyArgs {
                    all,
                    parallel,
                    skip_green,
                    gate,
                    command,
                    probe,
                    brief_version,
                    probe_phase,
                    attest,
                    result,
                    tested_revision,
                    execution_host,
                    runner,
                    note,
                    waive_dirty,
                    falsified_by,
                    predicted,
                    against,
                },
            )
        }
        Cmd::Done { change } => {
            let change = infer(change.as_deref())?;
            commands::done(&ctx, &change)
        }
        Cmd::Rebase { change, verify } => {
            let change = infer(change.as_deref())?;
            commands::rebase(&ctx, &change, verify)
        }
        Cmd::Env => Ok(context::print_env()),
        Cmd::Completions { shell } => {
            let mut command = Cli::command();
            clap_complete::generate(shell, &mut command, "arc", &mut std::io::stdout());
            Ok(0)
        }
        Cmd::Mangen { out_dir } => {
            std::fs::create_dir_all(&out_dir)
                .with_context(|| format!("cannot create {}", out_dir.display()))?;
            let mut buffer = Vec::new();
            clap_mangen::Man::new(Cli::command()).render(&mut buffer)?;
            let path = out_dir.join("arc.1");
            std::fs::write(&path, buffer)
                .with_context(|| format!("cannot write {}", path.display()))?;
            println!("{}", path.display());
            Ok(0)
        }
        Cmd::Resume {
            change,
            json,
            get,
            fields,
        } => {
            context::resume(
                &ctx,
                select(change)?.as_deref(),
                json,
                get.as_deref(),
                fields.as_deref(),
            )?;
            Ok(0)
        }
        Cmd::Rescue {
            change,
            json,
            transcript,
            tail,
            take,
        } => match artifact(&ctx, change.as_deref()) {
            Some(file) => {
                if transcript {
                    bail!(
                        "--transcript reads the session recorded against a change; an                          artifact's record of what happened is its checkpoints"
                    );
                }
                journal::rescue_artifact(&ctx, &file, json, take)
            }
            None => {
                let change = infer(change.as_deref())?;
                commands::rescue(&ctx, &change, json, take, transcript, tail)
            }
        },
        Cmd::Prompt { change } => {
            context::prompt(&ctx, select(change)?.as_deref())?;
            Ok(0)
        }
        Cmd::Fork { command } => match command {
            ForkCmd::Begin { slug, from } => fork::begin(&ctx, &slug, from.as_deref()),
            ForkCmd::Adopt { slug, intent } => fork::adopt(&ctx, &slug, intent.as_deref()),
            ForkCmd::Retire {
                slug,
                outcome,
                keep_worktree,
                force,
            } => fork::retire(&ctx, &slug, &outcome, keep_worktree, force),
            ForkCmd::List { json } => fork::list(&ctx, json),
            ForkCmd::Thread { slug } => fork::thread(&ctx, &slug),
        },
        Cmd::Hold {
            change,
            reason,
            reason_file,
        } => {
            let reason = commands::read_body(reason, reason_file)?;
            commands::hold(&ctx, &change, reason)?;
            Ok(0)
        }
        Cmd::ReleaseHold {
            change,
            hold,
            reason,
        } => {
            commands::release_hold(&ctx, &change, &hold, reason)?;
            Ok(0)
        }
        Cmd::Integrate {
            change,
            tag,
            into,
            message,
            cleanup,
            dry_run,
            debt,
            debt_kind,
        } => {
            let change = select(change)?;
            if debt.is_some() && !tag.is_empty() {
                if change.is_some() {
                    bail!("provide a change or --tag, not both");
                }
                bail!("--debt names one change; it cannot apply to a --tag series");
            }
            if debt.is_some() && change.is_none() {
                bail!("--debt requires a change");
            }
            // A fork refusal means no integration happened, so it must not
            // create an audit obligation for work that never shipped.
            fork::ensure_not_fork(&ctx.cwd)?;
            // Declared before the merge so the obligation is on the ledger
            // even if integration then fails for an unrelated reason — but
            // never under --dry-run, which promises to write nothing.
            if let Some(reason) = debt.filter(|_| !dry_run) {
                let change = change
                    .as_deref()
                    .expect("debt selection was validated above");
                commands::declare_debt(&ctx, change, reason, debt_kind)?;
            }
            commands::integrate(
                &ctx,
                change.as_deref(),
                tag,
                into,
                message,
                cleanup,
                dry_run,
            )
        }
        Cmd::Debt {
            change,
            reason,
            kind,
        } => {
            commands::declare_debt(&ctx, &change, reason, kind)?;
            Ok(0)
        }
        Cmd::Audit {
            change,
            verdict,
            body,
            body_file,
            findings_json,
            route_version,
        } => {
            // An audit body is optional; read_body refuses an absent one.
            let body = match (&body, &body_file) {
                (None, None) => None,
                _ => Some(commands::read_body(body, body_file)?),
            };
            commands::audit(
                &ctx,
                &change,
                commands::AuditArgs {
                    verdict,
                    body,
                    findings_json,
                    route_version,
                },
            )?;
            Ok(0)
        }
        Cmd::Close {
            change,
            assert_integrated,
            patchset,
            into,
            target_before,
            abandoned,
            superseded,
        } => {
            commands::close(
                &ctx,
                &change,
                commands::CloseArgs {
                    assert_integrated,
                    patchset,
                    into,
                    target_before,
                    abandoned,
                    superseded_by: superseded,
                },
            )?;
            Ok(0)
        }
        Cmd::Forge { cmd } => match cmd {
            ForgeCmd::Declare {
                change,
                host,
                base_repo,
                base_ref,
                head_repo,
                head_ref,
                policy,
            } => {
                commands::forge_declare(
                    &ctx, &change, host, base_repo, base_ref, head_repo, head_ref, policy,
                )?;
                Ok(0)
            }
            ForgeCmd::Link {
                change,
                pr,
                url,
                base_repo,
                base_ref,
                head_repo,
                head_ref,
                head_sha,
            } => commands::forge_link(
                &ctx,
                &change,
                commands::ForgeLinkArgs {
                    pr_number: pr,
                    url,
                    base_repo,
                    base_ref,
                    head_repo,
                    head_ref,
                    head_sha,
                },
            ),
            ForgeCmd::Checks {
                change,
                pr_head,
                state,
                detail,
            } => {
                commands::forge_checks(&ctx, &change, pr_head, state, detail)?;
                Ok(0)
            }
            ForgeCmd::PrState {
                change,
                state,
                merge_sha,
                link,
            } => {
                commands::forge_pr_state(&ctx, &change, state, merge_sha, link)?;
                Ok(0)
            }
        },
        Cmd::History { cmd } => match cmd {
            HistoryCmd::Rewrite { map, reason, tool } => {
                commands::record_rewrite(&ctx, &map, reason, tool)?;
                Ok(0)
            }
            HistoryCmd::Resolve { revision } => commands::resolve_rewritten(&ctx, &revision),
        },
        Cmd::Pass { cmd } => match cmd {
            PassCmd::Open { member, note } => {
                commands::open_pass(&ctx, member, note)?;
                Ok(0)
            }
            PassCmd::Complete { pass_id, note } => {
                commands::complete_pass(&ctx, pass_id, note)?;
                Ok(0)
            }
            PassCmd::Abandon { pass_id, reason } => {
                commands::abandon_pass(&ctx, pass_id, reason)?;
                Ok(0)
            }
            PassCmd::List { json } => commands::list_passes(&ctx, json).map(|_| 0),
        },
        Cmd::Run { cmd } => match cmd {
            RunCmd::Dispatch {
                route,
                worktree,
                change,
                fork,
                range,
                brief_event_id,
                note,
            } => {
                commands::dispatch_run(
                    &ctx,
                    commands::DispatchInput {
                        route,
                        worktree,
                        change,
                        fork,
                        range,
                        brief_event_id,
                        note,
                    },
                )?;
                Ok(0)
            }
            RunCmd::End {
                dispatch_event_id,
                outcome,
                reviewed_head,
                raised_json,
                deferred_json,
                collects,
                note,
            } => {
                commands::end_run(
                    &ctx,
                    &dispatch_event_id,
                    commands::EndingInput {
                        outcome,
                        reviewed_head,
                        raised_json,
                        deferred_json,
                        collects,
                        note,
                    },
                )?;
                Ok(0)
            }
            RunCmd::List { json } => {
                commands::list_runs(&ctx, json)?;
                Ok(0)
            }
        },
        Cmd::Config {
            check_writable,
            json,
        } => {
            if check_writable {
                return commands::check_writable(&ctx, json);
            }
            let cfg = config::load()?;
            let store_root = store::Store::resolve_root(&ctx.cwd)
                .map(|p| p.display().to_string())
                .ok();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ai_home": cfg.ai_home.display().to_string(),
                    "config_file": cfg.config_path.display().to_string(),
                    "config_file_exists": cfg.config_path.is_file(),
                    "worktrees_dir": cfg.worktrees_dir.display().to_string(),
                    "data_root": cfg.data_root.map(|p| p.display().to_string()),
                    "store_root_for_cwd": store_root,
                }))?
            );
            Ok(0)
        }
        Cmd::Doctor { json, verbose } => commands::run_doctor(&ctx, json, verbose),
        Cmd::Hooks { cmd } => match cmd {
            HooksCmd::Install { force } => {
                commands::hooks_install(&ctx, force)?;
                Ok(0)
            }
            HooksCmd::Uninstall => {
                commands::hooks_uninstall(&ctx)?;
                Ok(0)
            }
            HooksCmd::Status => {
                commands::hooks_status(&ctx)?;
                Ok(0)
            }
        },
        Cmd::HookRun { name, args } => Ok(commands::hook_run(&ctx, &name, &args)),
        Cmd::Workspace { cmd } => {
            let (view, json) = match cmd {
                WorkspaceCmd::List { json } => (commands::WorkspaceView::List, json),
                WorkspaceCmd::Inbox { json } => (commands::WorkspaceView::Inbox, json),
                WorkspaceCmd::Backlog {
                    since,
                    items,
                    under,
                    here,
                    global: _,
                    unreachable,
                    json,
                } => {
                    let scope = match (under, here) {
                        (Some(path), false) => commands::WorkspaceScope::Under(path),
                        (None, true) => commands::WorkspaceScope::Under(std::env::current_dir()?),
                        (None, false) => commands::WorkspaceScope::Global,
                        (Some(_), true) => unreachable!("clap rejects conflicting scopes"),
                    };
                    (
                        commands::WorkspaceView::Backlog {
                            since,
                            items,
                            scope,
                            show_unreachable: unreachable,
                        },
                        json,
                    )
                }
            };
            commands::workspace(&ctx, view, json)?;
            Ok(0)
        }
        Cmd::Restack { change, advise } => {
            let change = infer(change.as_deref())?;
            commands::restack(&ctx, &change, advise)?;
            Ok(0)
        }
        Cmd::Catchup { limit, json } => commands::catchup(&ctx, limit, json),
        Cmd::Fr { write } => journal::feature_request(&ctx, write),
        Cmd::Journal { cmd } => journal::run(&ctx, cmd),
    }
}
