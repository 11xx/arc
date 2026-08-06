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
mod render;
mod session_store;
mod state;
mod status;
mod store;

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use commands::{AnchorArgs, Ctx, ListFormat, QueryArgs};
use model::{
    DispositionStatus, MessageSeverity, MessageType, ProbePhase, ReviewCause, Severity, Side,
    Verdict, VerifyResult,
};

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
    /// Acting identity (defaults to git user.name)
    #[arg(long, global = true, env = "ARC_ACTOR")]
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
        /// Do not create a worktree
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
    },
    /// List changes
    List {
        #[arg(long)]
        open: bool,
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
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        tag: Vec<String>,
        #[arg(long, value_enum)]
        verdict: Option<Verdict>,
        #[arg(long)]
        actor: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        /// Report changes whose patchset, integration, or closure commit
        /// matches this revision (unique prefix accepted)
        #[arg(long)]
        commit: Option<String>,
        /// Only changes that integrated owing a review nobody has recorded yet
        #[arg(long = "audit-debt")]
        audit_debt: bool,
        #[arg(long)]
        json: bool,
    },
    /// Render one change (Markdown, or full state with --json)
    Show {
        change: Option<String>,
        #[arg(long)]
        tag: Vec<String>,
        #[arg(long)]
        json: bool,
        /// Replay state as of this event ID ("what did the actor see?")
        #[arg(long, conflicts_with = "tag")]
        at: Option<String>,
    },
    /// Print the change's ledger events one line each, in ledger order
    Log {
        change: Option<String>,
        /// Newest event first
        #[arg(long)]
        reverse: bool,
    },
    /// Derived ledger analytics: stage, review, and gate durations
    Stats {
        /// Report a single change
        #[arg(long, conflicts_with_all = ["tag", "all"])]
        change: Option<String>,
        /// Report every change carrying this tag
        #[arg(long, conflicts_with_all = ["change", "all"])]
        tag: Option<String>,
        /// Report all changes (the default)
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Render a recorded patchset using Git's native diff output
    Diff {
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
        /// Git pathspecs, passed after -- to git diff
        #[arg(index = 2, last = true)]
        paths: Vec<String>,
    },
    /// List findings in text, JSON, or SARIF 2.1.0 form
    Findings {
        change: Option<String>,
        #[arg(long, value_enum, default_value = "text")]
        format: commands::FindingsFormat,
    },
    /// Record or read a change-scoped implementation contract
    Brief {
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
        /// Prepend a scaffold template (.arc/templates/<name>.md or a built-in:
        /// sol-low, sol-high, reviewer, discussion)
        #[arg(long)]
        scaffold: Option<String>,
        /// Journal plan artifact implemented by this brief
        #[arg(long)]
        plan_ref: Option<String>,
        /// Opaque plan slice slug implemented by this brief
        #[arg(long)]
        plan_slice: Option<String>,
        /// JSON array of named acceptance probes bound to this brief ('-' for stdin)
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
        /// Replace the generated [Unreleased] block in CHANGELOG.md
        #[arg(long)]
        write: bool,
    },
    /// Append a structured cross-change announcement (never policy input)
    Message {
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
        #[arg(long)]
        change: Option<String>,
        #[arg(long = "type", value_enum)]
        message_type: Option<MessageType>,
        #[arg(long, value_enum)]
        severity: Option<MessageSeverity>,
        /// Only messages created at or after this ISO 8601 instant
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Lead-facing queue rollup across open changes, including active claim work (arc-inbox/2 schema)
    Inbox {
        /// Restrict to changes assigned to this harness
        #[arg(long = "assigned-to")]
        assigned_to: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show a tagged program in dependency order (arc-chain/1 schema)
    Chain {
        tag: String,
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
        change: String,
        #[arg(long = "blocked-by")]
        blocked_by: Vec<String>,
        #[arg(long = "remove-blocked-by")]
        remove_blocked_by: Vec<String>,
        #[arg(long)]
        tag: Vec<String>,
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
    /// Machine-readable status report (the versioned arc-status/6 schema)
    Status {
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
    BlockerStatus { change: Option<String> },
    /// Dependency probe: exit 0 ready, 1 blocked, 2 on lookup/ledger errors
    IsBlocked { change: Option<String> },
    /// Replay raw ledger events as NDJSON, optionally following new events
    Events {
        /// Continue emitting matching events appended after the replay
        #[arg(long)]
        follow: bool,
        /// Limit events to one exact change ID or unique prefix
        #[arg(long)]
        change: Option<String>,
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
        #[arg(long, value_enum, value_delimiter = ',', required = true)]
        until: Vec<commands::WatchUntil>,
        /// Fail with exit 2 after this many seconds
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: Option<u64>,
        /// Run a shell command once when a condition is reached
        #[arg(long = "exec")]
        exec_command: Option<String>,
    },
    /// Export one change as a deterministic, versioned JSON bundle
    Export {
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
    /// Acquire or renew an advisory executor claim
    Claim {
        change: Option<String>,
        /// Lease duration (positive integer with s, m, or h suffix; default 2h)
        #[arg(long)]
        ttl: Option<String>,
        /// Override one stage budget as <name>=<duration> (repeatable)
        #[arg(long = "stage-budget")]
        stage_budget: Vec<String>,
        /// Explicitly displace an active stale claim
        #[arg(long)]
        takeover: bool,
    },
    /// Release the current advisory executor claim
    ReleaseClaim { change: Option<String> },
    /// Record typed executor progress (requires an owned live claim)
    #[command(allow_missing_positional = true)]
    Stage {
        change: Option<String>,
        #[arg(value_enum)]
        stage: commands::StageArg,
        /// Acquire a default claim first when this session has no live claim
        #[arg(long)]
        claim: bool,
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
    },
    /// Add a discussion comment
    Comment {
        change: Option<String>,
        #[command(flatten)]
        body: BodyOpts,
        #[arg(long)]
        patchset: Option<String>,
        #[command(flatten)]
        anchor: AnchorOpts,
    },
    /// Record a standalone review finding
    Finding {
        change: Option<String>,
        /// One-sentence statement of the defect
        #[arg(long)]
        summary: String,
        #[command(flatten)]
        body: BodyOpts,
        #[arg(long)]
        blocking: bool,
        #[arg(long, value_enum, default_value = "major")]
        severity: Severity,
        #[arg(long)]
        patchset: Option<String>,
        #[command(flatten)]
        anchor: AnchorOpts,
    },
    /// Reply to a comment or finding event
    #[command(allow_missing_positional = true)]
    Reply {
        change: Option<String>,
        event_id: String,
        #[command(flatten)]
        body: BodyOpts,
    },
    /// Record a finding disposition (supersedes current tips automatically)
    #[command(allow_missing_positional = true)]
    Resolve {
        change: Option<String>,
        finding: String,
        #[arg(long, value_enum)]
        status: DispositionStatus,
        /// Fixing commit, when one exists
        #[arg(long)]
        commit: Option<String>,
        #[arg(long)]
        evidence: Option<String>,
    },
    /// Read review state, or record a verdict with an optional findings batch
    Review {
        change: Option<String>,
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
        /// Patchset under review (defaults to the latest)
        #[arg(long)]
        patchset: Option<String>,
        /// Root cause of requested rework; repeat for a mixed round
        #[arg(long, value_enum)]
        cause: Vec<ReviewCause>,
        /// JSON array of findings ('-' for stdin); IDs are assigned by arc
        #[arg(long)]
        findings_json: Option<String>,
    },
    /// Run a declared gate (or ad hoc command) and record the evidence
    Verify {
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
    },
    /// Finish implementation: snapshot, verify all gates, then print check state
    Done { change: Option<String> },
    /// Print shell exports for an explicitly detected harness session
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
        change: Option<String>,
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
        change: Option<String>,
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
    Prompt { change: Option<String> },
    /// Set an integration hold
    Hold {
        change: String,
        #[arg(long, required_unless_present = "reason_file")]
        reason: Option<String>,
        /// Read the hold reason from a file ('-' for stdin)
        #[arg(long, conflicts_with = "reason")]
        reason_file: Option<String>,
    },
    /// Release the active hold
    ReleaseHold {
        change: String,
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
        /// change still owes. The obligation survives closure and
        /// `arc query --audit-debt` finds it; discharge it with `arc audit`.
        #[arg(long = "audit-debt", value_name = "REASON")]
        audit_debt: Option<String>,
    },
    /// Record a review obligation this change carries but has not discharged
    AuditDebt {
        change: String,
        /// What review is owed, and why it could not run
        #[arg(long)]
        reason: String,
    },
    /// Record a review performed after integration (never a late verdict)
    Audit {
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
    },
    /// Close a change without arc performing the merge
    Close {
        change: String,
        /// Already integrated at this revision
        #[arg(long)]
        integrated: Option<String>,
        #[arg(long)]
        abandoned: bool,
        /// Superseded by another change
        #[arg(long)]
        superseded: Option<String>,
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
    /// Aggregate open changes or inboxes across a configured data_root
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    /// Advise (never execute) rebases for open dependents of a change
    Restack {
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
        #[arg(long)]
        json: bool,
    },
    /// Cross-harness project journal mechanics (plain Markdown stays the contract)
    Journal {
        #[command(subcommand)]
        cmd: journal::JournalCmd,
    },
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// Per-repo open-change rows across the data_root
    List {
        #[arg(long)]
        json: bool,
    },
    /// The inbox rollup for every repo under the data_root
    Inbox {
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
        change: String,
        #[arg(long)]
        host: String,
        #[arg(long = "base-repo")]
        base_repo: String,
        #[arg(long = "base-ref")]
        base_ref: String,
        #[arg(long = "head-repo")]
        head_repo: String,
        #[arg(long = "head-ref")]
        head_ref: String,
        /// same-repository-only (default) | allowed-base-repo=<owner/name>
        #[arg(long, default_value = "same-repository-only")]
        policy: String,
    },
    /// Record the observed post-creation PR tuple (validated, fail-closed)
    Link {
        change: String,
        #[arg(long)]
        pr: u64,
        #[arg(long)]
        url: String,
        #[arg(long = "base-repo")]
        base_repo: String,
        #[arg(long = "base-ref")]
        base_ref: String,
        #[arg(long = "head-repo")]
        head_repo: String,
        #[arg(long = "head-ref")]
        head_ref: String,
        #[arg(long = "head-sha")]
        head_sha: String,
    },
    /// Record the observed hosted-check rollup at an exact PR head
    Checks {
        change: String,
        #[arg(long = "pr-head")]
        pr_head: String,
        #[arg(long, value_enum)]
        state: forge::ForgeCheckState,
        #[arg(long)]
        detail: Option<String>,
    },
    /// Record the observed PR lifecycle state
    PrState {
        change: String,
        #[arg(long, value_enum)]
        state: forge::ForgePrState,
        /// Required when state is merged
        #[arg(long = "merge-sha")]
        merge_sha: Option<String>,
    },
}

fn role_refusal(role: ExecutionRole, command: &Cmd) -> Option<(&'static str, &'static str)> {
    // Brief reads are open to every role, while writes are lead-only; the
    // handler must inspect --body-file, so Brief cannot live in this deny-list.
    match role {
        ExecutionRole::Lead => None,
        ExecutionRole::Reviewer => match command {
            Cmd::Integrate { .. } => Some(("integrate", "lead")),
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
            error.print().ok();
            if kind == clap::error::ErrorKind::InvalidSubcommand {
                if let Some(path) = nested_subcommand_path(typed.as_deref()) {
                    eprintln!("  tip: a similar subcommand exists: '{path}'\n");
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
    let actor = match cli.actor {
        Some(a) => a,
        None => gitio::git(&cwd, &["config", "user.name"])
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".into()),
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
                session.get_or_insert(detected.session);
                if model.is_none() {
                    model = detected.model;
                }
            }
        }
    }
    let ctx = Ctx {
        cwd,
        actor,
        harness,
        session,
        model,
        // An empty --on-behalf-of is the same as absent: today's behavior.
        on_behalf_of: cli.on_behalf_of.filter(|value| !value.trim().is_empty()),
    };

    let infer = |change: Option<&str>| -> Result<String> {
        let store = store::Store::discover(&ctx.cwd)?;
        context::resolve_change_or_infer(&store, &ctx.cwd, change)
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
            audit_debt,
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
                        audit_debt,
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
                change
            };
            commands::show_selection(&ctx, role, change.as_deref(), tag, json, at.as_deref())?;
            Ok(0)
        }
        Cmd::Log { change, reverse } => {
            let change = infer(change.as_deref())?;
            commands::log(&ctx, &change, reverse)?;
            Ok(0)
        }
        Cmd::Stats {
            change,
            tag,
            all: _,
            json,
        } => {
            let selection = match (change, tag) {
                (Some(change), None) => commands::StatsSelection::Change(change),
                (None, Some(tag)) => commands::StatsSelection::Tag(tag),
                (None, None) => commands::StatsSelection::All,
                (Some(_), Some(_)) => unreachable!("clap rejects --change with --tag"),
            };
            commands::stats(&ctx, selection, json)?;
            Ok(0)
        }
        Cmd::Diff {
            change,
            patchset,
            stat,
            findings,
            between,
            since_approved,
            paths,
        } => {
            let change = infer(change.as_deref())?;
            commands::diff(
                &ctx,
                &change,
                patchset,
                stat,
                findings,
                between,
                since_approved,
                paths,
            )?;
            Ok(0)
        }
        Cmd::Findings { change, format } => {
            let change = infer(change.as_deref())?;
            commands::findings(&ctx, &change, format)?;
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
            change.as_deref(),
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
            event_type,
            since,
            exec_command,
        } => {
            commands::events(
                &ctx,
                follow,
                change.as_deref(),
                event_type.as_deref(),
                since,
                exec_command.as_deref(),
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
        } => {
            let quorum = match (any, all) {
                (true, false) => Some(commands::WatchQuorum::Any),
                (false, true) => Some(commands::WatchQuorum::All),
                _ => None,
            };
            commands::watch(
                &ctx,
                change.as_deref(),
                &tag,
                quorum,
                &until,
                timeout,
                exec_command.as_deref(),
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
                change
            };
            commands::check_selection(&ctx, change.as_deref(), tag, explain, json)
        }
        Cmd::Claim {
            change,
            ttl,
            stage_budget,
            takeover,
        } => {
            let change = infer(change.as_deref())?;
            commands::claim(&ctx, &change, ttl, stage_budget, takeover)
        }
        Cmd::ReleaseClaim { change } => {
            let change = infer(change.as_deref())?;
            commands::release_claim(&ctx, &change)
        }
        Cmd::Stage {
            change,
            stage,
            claim,
            note,
            note_file,
            blocker,
        } => {
            let change = infer(change.as_deref())?;
            let note = match (note, note_file) {
                (None, None) => None,
                (note, note_file) => Some(commands::read_body(note, note_file)?),
            };
            commands::stage(&ctx, &change, stage, note, blocker, claim)
        }
        Cmd::Snapshot {
            change,
            base,
            brief_version,
            verify,
            gate,
            all,
        } => {
            let change = infer(change.as_deref())?;
            commands::snapshot_with_verify(&ctx, &change, base, brief_version, verify, gate, all)
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
        } => {
            let change = infer(change.as_deref())?;
            commands::resolve(&ctx, &change, finding, status, commit, evidence)?;
            Ok(0)
        }
        Cmd::Review {
            change,
            verdict,
            json,
            body,
            snapshot,
            patchset,
            cause,
            findings_json,
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
                        body,
                        patchset,
                        causes: cause,
                        findings_json,
                        snapshot_first: snapshot,
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
                },
            )
        }
        Cmd::Done { change } => {
            let change = infer(change.as_deref())?;
            commands::done(&ctx, &change)
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
                change.as_deref(),
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
        } => {
            let change = infer(change.as_deref())?;
            commands::rescue(&ctx, &change, json, take, transcript, tail)
        }
        Cmd::Prompt { change } => {
            context::prompt(&ctx, change.as_deref())?;
            Ok(0)
        }
        Cmd::Hold {
            change,
            reason,
            reason_file,
        } => {
            let reason = commands::read_body(reason, reason_file)?;
            commands::hold(&ctx, &change, reason)?;
            Ok(0)
        }
        Cmd::ReleaseHold { change, reason } => {
            commands::release_hold(&ctx, &change, reason)?;
            Ok(0)
        }
        Cmd::Integrate {
            change,
            tag,
            into,
            message,
            cleanup,
            dry_run,
            audit_debt,
        } => {
            // Declared before the merge so the obligation is on the ledger
            // even if integration then fails for an unrelated reason.
            if let Some(reason) = audit_debt {
                let change = change
                    .as_deref()
                    .context("--audit-debt names one change; it cannot apply to a --tag series")?;
                commands::declare_audit_debt(&ctx, change, reason)?;
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
        Cmd::AuditDebt { change, reason } => {
            commands::declare_audit_debt(&ctx, &change, reason)?;
            Ok(0)
        }
        Cmd::Audit {
            change,
            verdict,
            body,
            body_file,
            findings_json,
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
                },
            )?;
            Ok(0)
        }
        Cmd::Close {
            change,
            integrated,
            abandoned,
            superseded,
        } => {
            commands::close(&ctx, &change, integrated, abandoned, superseded)?;
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
            } => {
                commands::forge_pr_state(&ctx, &change, state, merge_sha)?;
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
        Cmd::Journal { cmd } => journal::run(&ctx, cmd),
    }
}
