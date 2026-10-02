mod approval;
mod blockers;
mod bundle;
mod candidate;
mod chain;
mod changelog_render;
mod commands;
mod config;
mod context;
mod declarations;
mod explain;
mod forge;
mod gates;
mod gitio;
mod guide;
mod ids;
mod inbox;
mod integration;
mod journal;
mod journal_exchange;
mod model;
mod policy;
mod process_group;
mod project;
mod registry;
mod relations;
mod render;
mod replica;
mod rewrite;
mod selection;
mod session_store;
mod state;
mod status;
mod store;
mod trailers;
mod worktree_usage;

use anyhow::{bail, Context, Result};
use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};
use commands::{fork, AnchorArgs, Ctx, ListFormat, QueryArgs};
use model::{
    ActorSource, DebtMissing, DispositionStatus, ExternalVerdict, MessageSeverity, MessageType,
    ProbePhase, ReviewCause, RunOutcome, Severity, Side, Verdict, VerdictRelationKind,
    VerifyResult,
};
use std::path::{Path, PathBuf};

/// Persistent context and guarded workflow state over plain Git for agentic coding arcs.
///
/// Reconstruct project and workflow context across sessions and harnesses:
/// work in progress, decisions, next steps, and what is safe to integrate.
/// Git owns content and history; arc owns the journal, change ledger,
/// review findings, verification evidence, and guarded integration.
#[derive(Parser)]
#[command(
    name = "arc",
    version,
    about,
    after_help = "Run `arc` with no arguments for the workflow guide, or `arc catchup` for live project state."
)]
struct Cli {
    /// Acting identity, from ARC_ACTOR when the flag is absent. Falls back to
    /// <harness>:<session> when both are known, else git user.name; arc
    /// records either as an identity nobody declared
    #[arg(long, global = true)]
    actor: Option<String>,
    /// Harness label, e.g. claude, codex, opencode
    #[arg(long, global = true, env = "ARC_HARNESS")]
    harness: Option<String>,
    /// Native session ID of the acting harness thread
    #[arg(long, global = true, env = "ARC_SESSION")]
    session: Option<String>,
    /// Private web link for the acting session; recorded only in arc events
    #[arg(long, global = true, env = "ARC_SESSION_LINK")]
    session_link: Option<String>,
    /// Declare a model slug with optional #effort. Without this flag or
    /// ARC_MODEL, each invocation resolves the acting session store once when
    /// it writes. Declarations retain any observed disagreement and its
    /// coordinate
    #[arg(long, global = true)]
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
    /// Absolute prefix that stands in for the home directory: every root arc
    /// writes by default (ledger under a data root, journal, registry,
    /// configuration, worktrees, temp) lives beneath it
    #[arg(long, global = true, env = config::SANDBOX_VAR)]
    sandbox: Option<String>,
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

/// Who a patchset's work belongs to, declared as it is recorded.
#[derive(clap::Args)]
struct AttributionOpts {
    /// Explicitly set the contributors for this patchset. Recording a
    /// patchset while another actor holds a live claim on the change requires
    /// this or --solo
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
}

/// Cross-links a patchset records to where its work was framed.
#[derive(clap::Args)]
struct LinkOpts {
    /// Journal artifact this patchset was framed by (repeatable): a filename
    /// in this project's journal, or `<journal-dir>::<file>` for one in the
    /// journal at the absolute `<journal-dir>` another project's `arc journal
    /// dir` prints. The link records the reference as given. Its body digest
    /// is read from the owning journal when the patchset is recorded, and a
    /// name that resolves to no artifact is refused naming each other known
    /// journal that holds it; a path is refused naming the reference that
    /// resolves it. Given once or more, only the flagged artifacts are
    /// linked, `via: flag`.
    /// Omitted, the patchset links the artifact the change was opened from
    /// (`via: begin`) and the plan its brief names (`via: brief`), one link
    /// per file, a file named by both linked once as `begin`; a default that
    /// no longer resolves is left out with a warning on stderr. A rerun at an
    /// unchanged head with neither this nor --thread keeps existing links
    #[arg(long = "journal-ref", value_name = "FILE")]
    journal_ref: Vec<String>,
    /// External thread this work belongs to, as SCHEME:ID. Arc stores the
    /// identifiers and never fetches or resolves them
    #[arg(long, value_name = "SCHEME:ID")]
    thread: Option<String>,
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
    ///
    /// Without --base, the change derives its base from the local target.
    /// When that target lacks commits its configured upstream holds, as the
    /// last fetch left the remote-tracking ref (arc reads local refs only,
    /// never the network), the change still opens and stderr warns, naming
    /// both revisions and the fix: for a target strictly behind, the
    /// fast-forward (`git merge --ff-only` in the checkout holding it, or
    /// `git fetch . <upstream>:<target>` when none does); for a diverged one,
    /// that no fast-forward exists; then `arc rebase` onto the moved target.
    /// A target level with or ahead of its upstream, or tracking none, gets
    /// no warning.
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
        /// Base revision (defaults to the target head). An explicit base
        /// skips the comparison of the target with its upstream
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
        /// Open from an actionable journal artifact, consuming it. A bare
        /// filename resolves in this project's journal; `<journal-dir>::<file>`
        /// resolves `<file>` in the journal at the absolute `<journal-dir>`
        /// that project's `arc journal dir` prints, and records the promotion
        /// in that journal, naming this repository and change. A journal that
        /// is not one, or a file it does not hold, is refused naming both; a
        /// filename this project's journal does not hold, naming each other
        /// known journal that holds it; a path, naming the reference that
        /// resolves it. The change records the reference as given and the digest of its body
        /// read now, and each later snapshot links it `via: begin` unless
        /// given --journal-ref
        #[arg(long = "from-journal")]
        from_journal: Option<String>,
        /// Open by promoting a fork's work onto a new branch, recording the
        /// fork slug and the source base, head, and tree. The fork keeps its
        /// branch, worktree, and marker; no review credit crosses the link
        #[arg(long = "from-fork")]
        from_fork: Option<String>,
        /// Require an independent verdict whatever this change turns out to
        /// touch. One-way: nothing lowers it afterwards
        #[arg(long)]
        dangerous: bool,
        /// Open the change declaring that integration is not yet the goal
        #[arg(long)]
        iterating: bool,
    },
    /// List changes
    /// Unreadable changes are named with their errors on stderr; readable
    /// changes remain in the output
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
    /// Unreadable changes are named with their errors on stderr; readable
    /// changes remain in the output
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
    /// Aggregate reads name unreadable changes with their errors on stderr
    /// and retain readable changes; a selected unreadable change fails
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
    /// What one change knew and what it was accepted on, from the ledger and
    /// the journal. Writes nothing: no event, no ref, no journal entry, no
    /// lock.
    ///
    /// Eight slots, always all printed: contract (the brief in force, its
    /// planner credit, plan, base revision, and causes, and the selection
    /// and promotion behind each patchset a candidate promotion recorded),
    /// supplied context (the journal artifact the change was opened from,
    /// and every patchset's journal references with the operation that
    /// supplied each, `begin`, `brief`, `flag`, or `unrecorded`, and its
    /// thread), declared facts (every kept fact, with the events it cites),
    /// observed reads, rejected alternatives (kept facts of kind `rejected`,
    /// and each registration answering the same brief version as a promoted
    /// candidate, `declared` with its judgements or `inferred` while
    /// unjudged; judgements are read as they stand now, whatever `--at`
    /// names), evaluation (each counted gate's evidence event, tree,
    /// environment digest, timeout, reuse, and falsification), coverage at
    /// acceptance (the verdicts, waiver, and debt the integration event's
    /// authorization recorded), and later knowledge (audits, audit findings
    /// and dispositions, debts, and debt discharges recorded after the
    /// integration).
    ///
    /// Every row carries a standing: `recorded` (an event records it),
    /// `declared` (somebody stated it and arc did not check it), `inferred`
    /// (arc derived it from other records; no event states it), `absent`
    /// (nothing records it), or `unavailable` (its source cannot be read
    /// now). Every standing but `recorded` gives its reason, and an empty
    /// slot prints its absence and why.
    ///
    /// A journal reference is resolved against the journal now, hot or
    /// cold: `same` when the body digest equals the recorded one, `amended`
    /// with both digests when not, `missing` when the name resolves to
    /// nothing. The artifact a change was opened from resolves the same way
    /// against the digest its opening recorded, and is `recorded`; a change
    /// opened before arc recorded that digest keeps it `declared`, with its
    /// digest shown as `current`. A kept fact's citations name records; the
    /// fact stays `declared`.
    ///
    /// A counted pass's falsification lists a declared one (with its
    /// predicted reason) and the failure arc inferred it follows (standing
    /// `inferred`, with the failing event, its revision, and the rule that
    /// derived it) as separate entries, or `none` when neither was recorded.
    ///
    /// A discharge of a debt by a later review that is not an approval reads
    /// `fulfilled, not approved`. Coverage at acceptance never takes later
    /// knowledge into its values; it only points at the discharge.
    Explain {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Bound the view by this event ID on the change. For an integrated
        /// change, supplied context, declared facts, rejected alternatives,
        /// and later knowledge show only what was recorded by the event,
        /// while contract, evaluation, and coverage at acceptance keep their
        /// integration basis; the integration event itself, or any earlier
        /// one, leaves later knowledge empty. For a change not integrated it
        /// replays the change as of the event, as `arc show --at` does. An
        /// event not on the change is refused
        #[arg(long)]
        at: Option<String>,
        /// Emit `arc-explain/1` JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Print the change's recorded facts one line each, in ledger order. A
    /// review batch records several, so it renders as several lines. This is
    /// the ledger, not Git history: for commits, use `git log`. Model sources,
    /// observation coordinates, and declaration disagreements accompany each fact
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
    /// Derived ledger analytics: stage, review, and gate durations, and how
    /// often provenance is recorded
    /// Aggregate reads name unreadable changes with their errors on stderr
    /// and retain readable changes; a selected unreadable change fails
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
        #[arg(long = "by-model", conflicts_with = "provenance")]
        by_model: bool,
        /// How often each provenance record was written, one line per class,
        /// each count beside the population it was counted in. With `--json`,
        /// `arc-stats-provenance/1`.
        ///
        /// Falsification: passing non-probe `verification-recorded` events
        /// carrying a declared `falsification`, only `falsification_inferred`,
        /// or neither; and, of neither, those an earlier failure of the same
        /// gate (or command, when unnamed) on the same change precedes, which
        /// were recorded before arc derived the inference.
        ///
        /// Journal refs: patchsets carrying `journal_refs`, of all patchsets,
        /// of those on changes opened from a journal artifact, and of those on
        /// changes opened without one; and every reference by its `via`
        /// (begin, brief, flag, or unrecorded).
        ///
        /// Rejected alternatives: integrated changes with at least one kept
        /// fact of kind `rejected`, of integrated changes.
        ///
        /// Plan-linked briefs: brief versions carrying a `plan_ref`, of all
        /// versions; and changes whose in-force brief carries one, of the
        /// selected changes.
        ///
        /// Cited kept facts: kept facts citing at least one event, of all kept
        /// facts.
        ///
        /// Read records: changes with at least one `context-read` on their
        /// ledger, of the selected changes.
        #[arg(long)]
        provenance: bool,
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
        /// Read a new brief body from a file ('-' for stdin). A write records
        /// the next version: v1 on a change with no brief, otherwise one past
        /// the latest
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
        /// Scaffold template around the body: .arc/templates/<name>.md, or a
        /// built-in (sol-low, sol-high, reviewer, discussion). The body takes
        /// the place of the template's `{{body}}` line, or follows a template
        /// without one
        #[arg(long)]
        scaffold: Option<String>,
        /// Journal plan artifact implemented by this brief: a filename in
        /// this project's journal, or `<journal-dir>::<file>` for a plan in the
        /// journal at the absolute `<journal-dir>` another project's `arc
        /// journal dir` prints. The brief records the reference as given;
        /// a plan in another journal is read there, and that journal records
        /// the brief as a promotion naming this repository and change. A
        /// filename this project's journal does not hold is refused naming
        /// each other known journal that holds it; a path, naming the
        /// reference that resolves it
        #[arg(long)]
        plan_ref: Option<String>,
        /// Opaque plan slice slug implemented by this brief
        #[arg(long)]
        plan_slice: Option<String>,
        /// Named acceptance probes bound to this brief: a JSON array inline, a
        /// path to one, or '-' for stdin
        #[arg(long)]
        probes_json: Option<String>,
        /// Earlier ledger fact that caused this version (repeatable):
        /// finding:<id>, verdict:<event> naming a changes-requested verdict,
        /// or blocked-on:<event>, each on this change. v1 refuses a cause;
        /// every later version requires at least one, from this flag or
        /// --cause-note
        #[arg(long)]
        caused_by: Vec<String>,
        /// External cause, when no earlier ledger object represents the
        /// reason. It is a cause like --caused-by: refused on v1, and enough
        /// alone for any later version
        #[arg(long)]
        cause_note: Option<String>,
        /// A read the contract requires of whoever answers it (repeatable):
        /// a journal artifact, `<file>` or `<journal-dir>::<file>`, at
        /// `@sha256:<hex>` or at the body digest read now; or a file at a
        /// revision, `<revision>:<path>[:<from>-<to>]`, recorded as its blob
        /// and the one-based inclusive line range, whole when none is given.
        /// A locator that resolves to nothing is refused, an artifact filename
        /// this project's journal does not hold naming each other known
        /// journal that holds it, and a path to an artifact naming the
        /// reference that resolves it. `arc candidate
        /// select` meets a requirement only with a tool read at that version
        /// covering that extent. An artifact requirement is met by digest
        /// equality with the text a tool returned, so a tool that decorates
        /// what it returns, like a line-numbered `Read`, never meets one
        #[arg(long = "must-read")]
        must_read: Vec<String>,
        /// Emit the versioned structured brief projection.
        #[arg(long)]
        json: bool,
    },
    /// Record, read, or project changelog entries
    ///
    /// While an open change has no changelog record, `arc status`, `arc
    /// check`, and `arc integrate` advise one under the code
    /// `no-changelog-entry`; the advice never blocks. Recording an entry, or
    /// recording with --none that the change needs none, answers it. The
    /// latest record wins, and a --none record projects nothing.
    ///
    /// The projection is rendered by the renderer `.arc/changelog.toml`
    /// selects: the built-in `keep-a-changelog`, or `command`, whose
    /// `renderer_command` argv runs from the repository root without a shell,
    /// with the authority of whoever runs arc changelog. It reads an
    /// arc-changelog-render-request/1 document on stdin and answers on stdout;
    /// `renderer_timeout` bounds it (default 60s), and its process group is
    /// killed at the deadline. `--json` never runs it.
    ///
    /// Read-only projections name unreadable changes with their errors on
    /// stderr and retain readable entries. --write requires every change to
    /// be readable; an unreadable selected change fails
    Changelog {
        /// Change whose entry is recorded or read. Omitted, recording infers
        /// it from the current branch, then from the worktree the command
        /// runs in, while a read projects every change's entry instead
        change: Option<String>,
        /// Free-form category for a newly recorded changelog entry
        #[arg(long)]
        category: Option<String>,
        /// Read a new entry body from a file ('-' for stdin)
        #[arg(long)]
        body_file: Option<String>,
        /// Record that the change needs no changelog entry, replacing any
        /// entry recorded before. Requires --reason
        #[arg(long, requires = "reason", conflicts_with_all = ["category", "body_file"])]
        none: bool,
        /// Why the change needs no changelog entry; only with --none
        #[arg(long, requires = "none")]
        reason: Option<String>,
        /// Emit a read result as JSON
        #[arg(long)]
        json: bool,
        /// Include recording identity in human-readable output
        #[arg(long, conflicts_with = "json")]
        provenance: bool,
        /// Override the latest-tag release boundary
        #[arg(long)]
        since: Option<String>,
        /// Write the configured target. The built-in renderer replaces the
        /// generated [Unreleased] block in CHANGELOG.md; each paragraph and
        /// list item of an entry is refilled to 75 columns, continuations
        /// indented under their marker, and a fenced block keeps its lines. The
        /// block is judged paragraph by paragraph, on words rather than the
        /// column they are wrapped at, and refused, naming each paragraph,
        /// while it holds prose no recorded entry produced. The block runs to
        /// the next release heading or the end of the file; a missing target,
        /// or one with no [Unreleased] heading, is refused with exit 1 and
        /// nothing written. A command renderer's stdout replaces the whole
        /// target, atomically, only after it exits 0 with a non-empty answer
        #[arg(long)]
        write: bool,
        /// Keep the block's unrecorded paragraphs whole above the projected
        /// entries, under an unrecorded marker, instead of refusing to write.
        /// Built-in renderer only
        #[arg(long)]
        keep_unrecorded: bool,
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
    /// Aggregate reads name unreadable changes with their errors on stderr
    /// and retain readable changes; a selected unreadable change fails
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
    /// Aggregate reads name unreadable changes with their errors on stderr
    /// and retain readable changes; a selected unreadable change fails
    Inbox {
        /// Restrict to changes assigned to this harness
        #[arg(long = "assigned-to")]
        assigned_to: Option<String>,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// Show a tagged program in dependency order (arc-chain/4 schema)
    /// Aggregate reads name unreadable changes with their errors on stderr
    /// and retain readable changes; a selected unreadable change fails
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
    /// Machine-readable status report (the versioned arc-status/27 schema).
    /// An unreadable selected change fails; unreadable neighboring changes
    /// are named with their errors on stderr and omitted from the dependency
    /// observation. An unreadable prerequisite cannot count as integrated
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
    /// Aggregate reads name unreadable changes with their errors on stderr
    /// and retain readable changes; a selected unreadable change fails
    BlockerStatus {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
    },
    /// Dependency probe: exit 0 ready, 1 blocked, 2 on lookup/ledger errors
    /// Aggregate reads name unreadable changes with their errors on stderr
    /// and retain readable changes; a selected unreadable change fails
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
        /// Run `sh -c <cmd>` for every emitted event, with its NDJSON line on
        /// stdin and ARC_EVENT_ID, ARC_EVENT_TYPE, and ARC_CHANGE_ID set. A
        /// failing handler is a warning and never stops the stream
        #[arg(long = "exec")]
        exec_command: Option<String>,
    },
    /// Wait for a change, or a tagged series, to reach a ledger-derived condition
    /// Aggregate reads name unreadable changes with their errors on stderr
    /// and retain readable changes; a selected unreadable change fails
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
        /// `stalled` is a stage clock, not an activity check: it holds once
        /// the change's live claim has sat in one stage longer than that
        /// stage's budget (`arc claim --stage-budget`), counted from its last
        /// `arc stage`, or from the claim while it is still at `launch`. Only
        /// `arc stage`, re-reporting the current stage included, or a
        /// snapshot under the claim restarts the clock; output, logs, and
        /// claim renewals do not. The reached line and the JSON name the
        /// stage, its age, and its budget. On a journal artifact, `stalled`
        /// means the claim's lease ran out.
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
        /// Run `sh -c <cmd>` once when a condition is reached, with a JSON
        /// diagnostic naming the winning condition on stdin and ARC_EVENT_TYPE
        /// set to `watch-reached`. ARC_EVENT_ID and ARC_CHANGE_ID carry the
        /// diagnostic's values, empty where it names none
        #[arg(long = "exec")]
        exec_command: Option<String>,
        /// Emit the outcome as one JSON object, naming the change, the
        /// condition, and the event that satisfied it; `stalled` adds
        /// `stage`, `age_seconds`, and `budget_seconds`
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
        /// Export only the events after this history checksum, which the
        /// receiving store must already hold. Repository events travel in
        /// every bundle, even when the change suffix is empty
        #[arg(long, value_name = "SHA256")]
        since: Option<String>,
    },
    /// Import arc-bundle/6 or /5 into this repository's local store.
    /// Events without model provenance retain that absence. Repository map
    /// withdrawals apply even to maps already held; invalid withdrawal
    /// targets are refused before anything is written
    Import {
        /// Input file ('-' for stdin)
        input: String,
        /// Validate and report without writing events or retention refs
        #[arg(long)]
        dry_run: bool,
    },
    /// Pair independent stores and move integration authority with files
    Replica {
        #[command(subcommand)]
        cmd: ReplicaCmd,
    },
    /// Integration preflight; exit code identifies the first blocker
    ///
    /// Unreadable neighboring changes are named with their errors on stderr;
    /// an unreadable selected change fails and an unreadable prerequisite
    /// cannot authorize integration
    #[command(after_help = blockers::exit_status_help())]
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
    /// artifact. Journal artifact claims require a declared actor
    /// (--actor/ARC_ACTOR), harness (--harness/ARC_HARNESS), and session
    /// (--session/ARC_SESSION).
    Claim {
        /// Change to act on, or a journal artifact filename ending in `.md`.
        /// Omitted, the change is inferred from the current branch, then from
        /// the worktree the command runs in
        change: Option<String>,
        /// Lease duration (positive integer with s, m, or h suffix; default 2h)
        #[arg(long)]
        ttl: Option<String>,
        /// Override one stage budget as <name>=<duration> (repeatable; a
        /// change's stages are budgeted, an artifact's lease is not). Defaults:
        /// launch=60s, started=5m, spec-read=2m, implementing=30m,
        /// verifying=15m, blocked-on=15m, snapshotted=1h
        #[arg(long = "stage-budget")]
        stage_budget: Vec<String>,
        /// Explicitly displace a claim that may be taken over: a stale one on
        /// a change, an expired one on an artifact
        #[arg(long)]
        takeover: bool,
        /// Why a lease that is not yet reclaimable is cut short, recorded on
        /// the displaced claim and verified by nobody (requires --takeover;
        /// `harness-status-absent` and `delegate-exit:<handle>` are the
        /// expected evidence, and any text is accepted)
        #[arg(long)]
        because: Option<String>,
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
    /// Rewrite a change as one commit on its base, with the same tree, and
    /// record it as a new patchset. Evidence and verdicts on the earlier
    /// heads stay with those heads; the single commit is gated and reviewed
    /// like any other patchset. A failed commit or hook-modified tree restores
    /// the original head, index, and tracked files; untracked files are retained.
    /// Obstructing paths are moved to a reported recovery directory under
    /// <git-common-dir>/arc/squash-recovery/. Over another actor's live
    /// claim, a squash without --contributors or --solo refuses before the
    /// branch moves
    Squash {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        /// Message for the single commit
        #[arg(long, short = 'm')]
        message: String,
        #[command(flatten)]
        attribution: AttributionOpts,
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
        #[command(flatten)]
        attribution: AttributionOpts,
        /// Amend one patchset's contributors before any verdict exists
        #[arg(
            long,
            value_name = "PATCHSET",
            conflicts_with_all = [
                "base",
                "brief_version",
                "verify",
                "gate",
                "all",
                "journal_ref",
                "thread"
            ]
        )]
        amend: Option<String>,
        #[command(flatten)]
        links: LinkOpts,
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
        /// An event on this change the fact rests on (repeatable, exact id):
        /// a verification or reused verification, a verdict, external
        /// verdict, or audit verdict, a finding or audit finding, a
        /// disposition or audit disposition, or an earlier kept fact. An id
        /// that names no event on this change, or an event of any other kind,
        /// is refused and nothing is kept. A citation names the record the
        /// fact rests on; it does not make the fact verified, and `--kind`
        /// means what it means without one
        #[arg(long = "cites", value_name = "EVENT")]
        cites: Vec<String>,
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
    #[command(group(clap::ArgGroup::new("snapshot_attribution").args(["contributors", "solo"]).requires("snapshot")))]
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
        /// Snapshot the clean change worktree before recording the verdict;
        /// --contributors or --solo attribute that patchset
        #[arg(long)]
        snapshot: bool,
        #[command(flatten)]
        attribution: AttributionOpts,
        /// Patchset under review, by id or by the revision it recorded.
        /// Defaults to the latest — which is what the verdict then claims,
        /// whatever the reviewer actually read
        #[arg(long)]
        patchset: Option<String>,
        /// Root cause of requested rework; repeat for a mixed round. Required
        /// with `--verdict changes-requested` and refused with any other
        /// verdict
        #[arg(long, value_enum)]
        cause: Vec<ReviewCause>,
        /// Findings batch: a path to a JSON array, or '-' for stdin. Each
        /// element is an object with a required `severity` (critical, major,
        /// minor, or note) and `summary` (string), and optional `blocking`
        /// (bool, default false), `body` (string), and `anchor`. An anchor is
        /// an object with a required `path` and optional `side` (base or head,
        /// default head), `line_start`, `line_end`, and `context`. An unknown
        /// field of a finding or anchor that looks like a misspelling of one
        /// it omits (`blocker` for `blocking`, `title` for `summary`, `path`
        /// for `anchor`, `line` for `line_start`, `lines` for `line_start` and
        /// `line_end`, or one edit away) refuses the batch; any other is
        /// ignored with a warning, except a finding's `id`, ignored silently
        /// because IDs are assigned by arc. An approval cannot carry a
        /// blocking finding
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
    /// Record a review decision made outside the local ledger
    External {
        #[command(subcommand)]
        cmd: ExternalCmd,
    },
    /// Run a declared gate (or ad hoc command) and record the evidence. Gate
    /// evidence only counts at the change's own head, so the command runs in
    /// the change's recorded worktree whichever checkout it was typed in;
    /// `--attest` records evidence arc did not run
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
        /// Record reuse of passing evidence instead of rerunning the gate that
        /// produced it: with --all, evidence green at the current head's tree;
        /// with --against, evidence green at the merged tree. Evidence counts
        /// for its tree whichever commit it ran on, as it does for readiness.
        /// Reuse records that tree and requires evidence carrying the same
        /// content key; evidence with neither tree nor tested_tree is rerun
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
        /// Environment identity the attested evidence applies to, as the
        /// gate's declared environment probe yields it.
        ///
        /// Required with --attest for a gate whose declaration has an
        /// environment probe, and rejected for a declaration that has none:
        /// an identity is only meaningful against the probe that produces it.
        /// Omit it for a run arc observes itself; arc reads the environment
        /// from the declared probe
        #[arg(long, value_name = "IDENTITY")]
        environment: Option<String>,
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
        ///
        /// Only this declaration makes a gate `discriminating`. Separately, a
        /// pass that follows a failure of the same gate (or command, when
        /// unnamed) on this change records the newest such failure as
        /// `falsification_inferred`, whether or not this flag is given; that
        /// inference decides nothing. Acceptance-probe evidence neither
        /// records one nor serves as the failure one names.
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
        /// scratch checkout is removed whatever the gates do, and is not
        /// created at all when no gate is left to run. The evidence is spent
        /// as soon as the target moves again, because that is a different
        /// merge. With --skip-green, a gate already green at that merged tree
        /// records reuse rather than running a second time.
        ///
        /// A textual conflict, including a modify/delete conflict, refuses
        /// before any gate runs or evidence is recorded; rebase first.
        #[arg(long, value_name = "BRANCH")]
        against: Option<String>,
    },
    /// Finish implementation: snapshot, verify all gates, then print check
    /// state. A profile with no declared gate records no evidence and prints
    /// that no gate is declared instead of a pass.
    Done {
        /// Change to act on. Omitted, it is inferred from the current branch,
        /// then from the worktree the command runs in
        change: Option<String>,
        #[command(flatten)]
        attribution: AttributionOpts,
        #[command(flatten)]
        links: LinkOpts,
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
    /// one. A harness exports its session id into the processes it starts, so
    /// when several harnesses' variables are present it is the nearest
    /// ancestor that exported one that owns this process: a pi run inside a
    /// Claude Code tool shell reports pi, not the shell's claude. Where the
    /// ancestry names no single owner the ambiguity is reported and no
    /// harness, session, or model is set, rather than choosing by variable
    /// order. The store root is the harness's own override —
    /// `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_SESSION_DIR`, or
    /// `PI_CODING_AGENT_DIR` — before its default under `$HOME`. The store's
    /// answer for the exact canonical session id is reported with the exports:
    /// an id the store does not hold is uncorroborated; an ambiguous or
    /// unreadable lookup is unresolved. Events carry that verdict. Every
    /// established harness, session, and session link is exported; absent
    /// fields are explicitly unset. The resolved model is a comment and
    /// ARC_MODEL is always unset, so each write resolves its own model. Pi re-sets
    /// `PI_SESSION_FILE`, `PI_MODEL`, and `PI_REASONING_LEVEL` for every tool
    /// call, so those answer in preference to the recording while
    /// `PI_SESSION_ID` is the acting session, and a Claude subagent shares
    /// its parent's session id, so while the session has an unfinished
    /// subagent recording no model is named and the line says why. Not every
    /// harness exports a session variable, and a harness that
    /// does may not in every mode. OpenCode v2 exports none and is recognized
    /// by `OPENCODE_TERMINAL` or its process ancestry, printing the harness
    /// export, unsetting the session and model, and leaving the session as a
    /// comment to set by hand.
    ///
    /// With nothing to detect at all, or several harnesses and no owner the
    /// ancestry can name, it prints the export template as a comment, unsets
    /// every identity field, and
    /// exits non-zero, which is a report that identity must be
    /// set by hand rather than a failure. Every value it emits can be set
    /// directly: explicit identity always wins over a detected one.
    ///
    /// The model comment includes the newest selection's timestamp and native
    /// id, and whether it is inside or before the recording's newest operator
    /// turn. An earlier observation may predate an effort change. Leave
    /// ARC_MODEL is always unset for write-time resolution; declare a model
    /// by hand with ARC_MODEL or --model. Missing boundaries and incomplete
    /// head coverage are reported.
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
        /// Include an exact claimed session's sensitive transcript, or a
        /// cause and reason when its recording cannot be read; file reads take
        /// the newest 4 MiB
        #[arg(long)]
        transcript: bool,
        /// Maximum operator turns to include from the operator projection
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
    /// Guarded merge of one change, or of a dependency-ordered queue named
    /// by several changes or by --tag.
    ///
    /// The merge runs where the target is checked out. That checkout's tracked
    /// modifications refuse, staged or unstaged. Untracked or ignored paths
    /// the merge does not write are left in place and named in the report; a
    /// path the merge would write that is already there without being tracked
    /// is refused by name. A head the target already contains closes at the
    /// target revision that holds it, without a merge commit. When no checkout
    /// holds the target, the merge takes over the change's own checkout if it
    /// still holds the change branch and leaves it on the target.
    #[command(
        after_help = "Arc exits 17 when a paired replica does not hold integration authority; the refusal names the holder or an offer in flight."
    )]
    Integrate {
        /// Changes to integrate. Several run as a queue, in dependency order,
        /// stopping at the first that needs a person. Omit only when
        /// selecting with --tag
        change: Vec<String>,
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
        /// With --dry-run, print the plan as `arc-integration-plan/1` JSON:
        /// the authorization basis the integration event would record, the
        /// target revision it was decided at, the approved head, and the tree
        /// the merge would ship. It is decided exactly as the merge is; a
        /// refusal prints the blocker on stderr and nothing on stdout. One
        /// change only
        #[arg(long, requires = "dry_run")]
        json: bool,
        /// Compare the fresh decision with a plan an earlier
        /// `--dry-run --json` printed. The fresh readiness evaluation alone
        /// decides; the file never changes the decision. When the approved
        /// head, the target revision, the gate or policy declarations, or the
        /// approval moved since, a refusal adds one line naming what moved
        /// and a permitted integration proceeds with a warning naming it. An
        /// unchanged basis adds nothing. One change only
        #[arg(long = "expect-basis", value_name = "FILE", conflicts_with = "dry_run")]
        expect_basis: Option<PathBuf>,
        /// Integrate without an independent verdict, recording the review this
        /// change still owes. One change only: the reason binds to one
        /// patchset, so it has nothing to say about a queue. It stands in
        /// for a verdict nobody recorded — and
        /// for a self-approval policy would reject — in the same invocation.
        /// It never overrules a reviewer who read this patchset and asked for
        /// changes: that is a verdict, not a missing one. The obligation
        /// survives closure and `arc query --debt` finds it; discharge it
        /// with `arc audit`. A refused integration keeps the debt it
        /// declared, and a retry with the same reason reuses it.
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
        /// What review is owed, and why it could not run. The debt already
        /// in force, declared again unchanged, is reused rather than recorded
        /// twice
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
        /// Findings batch: a path to a JSON array, or '-' for stdin, in the
        /// shape `arc review --help` states for `--findings-json`, without an
        /// `anchor`: an audited revision has no patchset diff to anchor to
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
        /// Opaque upstream reference for an external approval at this patchset
        #[arg(long = "external-reference")]
        external_reference: Option<String>,
    },
    /// Record or withdraw rewrite maps, and resolve recorded revisions
    History {
        #[command(subcommand)]
        cmd: HistoryCmd,
    },
    /// Rewrite this repository's history, carrying every recorded revision
    /// forward
    Rewrite {
        #[command(subcommand)]
        cmd: RewriteCmd,
    },
    /// Register, show, list, judge, and retire candidates: alternative
    /// answers to a brief, recorded without opening a change
    Candidate {
        #[command(subcommand)]
        cmd: CandidateCmd,
    },
    /// Record what a change or a candidate read, declared, and captured:
    /// read records from a tool's record, attributed declarations, and
    /// capture reports
    Context {
        #[command(subcommand)]
        cmd: ContextCmd,
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
    /// Read or write policy kept with this repository's local Arc ledger
    Policy {
        #[command(subcommand)]
        cmd: PolicyCmd,
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
    /// Make, compare, and remove a disposable copy of this project under a
    /// sandbox prefix
    Sandbox {
        #[command(subcommand)]
        cmd: SandboxCmd,
    },
    /// Check the append-only ledger for malformed or stale state (read-only)
    ///
    /// Exits 0 when the ledger is clean or carries only advice, and 1 when it
    /// reports a problem. A ledger that cannot be read at all also exits 1,
    /// with the error on stderr and no report; a usage error exits 2.
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
    /// Print a portable convention specification, independent of arc state
    Instructions {
        #[command(subcommand)]
        cmd: InstructionsCmd,
    },
    /// Aggregate changes, inboxes, or backlog across every known project
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    /// Advise (never execute) rebases for open dependents of a change
    /// Aggregate reads name unreadable changes with their errors on stderr
    /// and retain readable changes; a selected unreadable change fails
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
    /// Unreadable changes are named with their errors on stderr; readable
    /// changes remain in the output
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
    /// Cross-harness project journal mechanics (plain Markdown stays the
    /// contract); read-only commands also resolve from the journal directory
    /// itself, while a write names the checkout it needs
    Journal {
        #[command(subcommand)]
        cmd: journal::JournalCmd,
    },
}

#[derive(Subcommand)]
enum ReplicaCmd {
    /// Print this store's repository ID for an explicit pairing record
    Id {
        /// Emit the machine-readable JSON view
        #[arg(long)]
        json: bool,
    },
    /// Start a logical project and hold its initial integration authority
    Init {
        /// Unique replica name within the logical project
        name: String,
    },
    /// Record a peer identity that can adopt the project by importing an export
    Pair {
        /// Unique name for the paired replica
        name: String,
        /// That replica's local repository ID from `arc replica id`
        #[arg(long = "repository-id")]
        repository_id: String,
    },
    /// Export this replica's known identity and authority events
    Export {
        /// Output file ('-' for stdout)
        #[arg(long)]
        output: String,
    },
    /// Import a pairing record or authority exchange from another replica.
    /// Accepts arc-replica-bundle/3 and /2; absent model provenance stays absent
    Import {
        /// Input file ('-' for stdin)
        input: String,
        /// Validate and report without writing events or an import receipt
        #[arg(long)]
        dry_run: bool,
    },
    /// Show this store's replica, peers, and current authority
    Status {
        /// Emit the machine-readable JSON view
        #[arg(long)]
        json: bool,
    },
    /// Offer authority or request and confirm its return
    Authority {
        #[command(subcommand)]
        cmd: ReplicaAuthorityCmd,
    },
}

#[derive(Subcommand)]
enum ReplicaAuthorityCmd {
    /// Relinquish authority and create an offer for a paired recipient
    Offer {
        /// Paired recipient's replica name
        #[arg(long)]
        to: String,
    },
    /// Request return of an active offer; authority stays blocked here
    Reclaim {
        /// Reason for requesting the recipient's confirmation
        #[arg(long)]
        because: String,
    },
    /// Relinquish an offered grant after importing its reclaim request
    ConfirmReturn,
}

#[derive(Subcommand)]
enum ForkCmd {
    /// Create the fork worktree and journal its marker
    Begin {
        /// Kebab-case slug naming the fork (and the fork/<slug> branch)
        slug: String,
        /// Branch to fork from; omitted, the current branch, or the
        /// repository's integration branch when standing on a fork
        #[arg(long)]
        from: Option<String>,
    },
    /// Journal a marker for a hand-made fork worktree
    Adopt {
        /// The fork slug: the name the fork is recorded and retired under
        slug: String,
        /// The branch this fork is; defaults to fork/<slug>, and an adopted
        /// fork keeps whatever name it has. An open change's branch is
        /// refused: a marker over it would make the change unintegrable
        #[arg(long)]
        branch: Option<String>,
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
    /// Unreadable change promotions are named with their errors on stderr;
    /// readable promotions remain in the output
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
enum InstructionsCmd {
    /// Git contribution trailers and commit provenance, with no arc state
    Git {
        /// Report malformed role values and keys outside the convention in a
        /// commit message file ('-' for stdin); never rewrites it
        #[arg(long, value_name = "FILE")]
        check: Option<String>,
    },
}

#[derive(Subcommand)]
enum PolicyCmd {
    /// Show effective policy, gate environment probes, and each declaring file
    Show,
    /// Print the operator policy file path for this repository
    Path,
    /// Replace the operator policy file with TOML from a file or stdin
    Write {
        /// Read TOML from a file ('-' for stdin)
        #[arg(long, required = true, value_name = "FILE")]
        body_file: String,
    },
}

#[derive(Subcommand)]
enum ExternalCmd {
    /// Record an external verdict at the revision its decision covered. A
    /// rejection of the latest patchset closes the change as abandoned.
    /// An external approval never supersedes a local refusal
    Verdict {
        /// Change that received the external decision
        change: String,
        /// Decision made by the external reviewer or receiver
        #[arg(long, value_enum)]
        verdict: ExternalVerdict,
        /// Name supplied for who made the external decision
        #[arg(long = "decided-by", required = true)]
        decided_by: String,
        /// Opaque source reference such as a review URL
        #[arg(long, required = true)]
        reference: String,
        /// Commit revision the external decision covered
        #[arg(long, required = true)]
        revision: String,
        /// Findings batch for a changes-requested verdict: a path to a JSON
        /// array, or '-' for stdin, in the shape `arc review --help` states
        /// for `--findings-json`. A non-empty batch is refused with any other
        /// verdict
        #[arg(long = "findings-json")]
        findings_json: Option<String>,
    },
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// Per-repo open-change rows across the data_root
    /// Unreadable changes are named with their errors on stderr; readable
    /// changes remain in the output
    List {
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// The inbox rollup for every registered project
    /// Unreadable changes are named with their errors on stderr; readable
    /// changes remain in the output
    Inbox {
        /// Report only projects whose canonical anchor is beneath this path
        #[arg(long, value_name = "PATH", conflicts_with_all = ["here", "global"])]
        under: Option<PathBuf>,
        /// Report only projects beneath the current directory
        #[arg(long, conflicts_with_all = ["under", "global"])]
        here: bool,
        /// Report every registered project, the default when no scope is set
        #[arg(long, conflicts_with_all = ["under", "here"])]
        global: bool,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// Every artifact across the selected stores, each with why it is where
    /// it is: present, terminal, archived, or superseded. It accepts the same
    /// `--under`/`--here`/`--global` scope, reports the same collection
    /// manifest, exits 16 on a partial collection, and is versioned
    /// `arc-workspace-inventory/2`.
    Inventory {
        /// Read the hot journal directory, the cold archive, or both
        #[arg(long, value_enum, default_value_t = commands::StorageSelection::Hot)]
        storage: commands::StorageSelection,
        /// Report only projects whose canonical anchor is beneath this path
        #[arg(long, value_name = "PATH", conflicts_with_all = ["here", "global"])]
        under: Option<PathBuf>,
        /// Report only projects beneath the current directory
        #[arg(long, conflicts_with_all = ["under", "global"])]
        here: bool,
        /// Report every registered project, the default when no scope is set
        #[arg(long, conflicts_with_all = ["under", "here"])]
        global: bool,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// Ledger and journal backlog across every project, ranked by what is
    /// blocked on a decision rather than on work. A partial collection keeps
    /// the projects it did read, names each failure, and exits 16
    /// Change observations name unreadable changes with their errors on
    /// stderr and retain readable changes
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
        /// Rank projects by this fact, highest first. Every row carries all
        /// three facts unchanged, so the choice only sets the order
        #[arg(long, value_enum, default_value_t = commands::RankBasis::Blocking)]
        rank_by: commands::RankBasis,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
    /// The backlog classified by named rules: each artifact's status, the
    /// section it belongs to, and the facts that deserve attention, versioned
    /// `arc-workspace-report/3`. With `--previous`, tallies carry their
    /// earlier values when both collections are complete, new rows are marked
    /// when their project was observed both times, and every artifact that left
    /// the backlog is listed with its recorded reason. The baseline must have
    /// the same schema and scope. Every failed component read, including the
    /// report's ledger read, enters the collection manifest and makes the
    /// command exit 16. Attention facts, listed below by rule, never change
    /// the exit code. The report writes nothing.
    #[command(after_help = commands::report_rules_help())]
    /// Change observations name unreadable changes with their errors on
    /// stderr and retain readable changes
    Report {
        /// Report only projects whose canonical anchor is beneath this path
        #[arg(long, value_name = "PATH", conflicts_with_all = ["here", "global"])]
        under: Option<PathBuf>,
        /// Report only projects beneath the current directory
        #[arg(long, conflicts_with_all = ["under", "global"])]
        here: bool,
        /// Report every registered project, the default when no scope is set
        #[arg(long, conflicts_with_all = ["under", "here"])]
        global: bool,
        /// An earlier `arc workspace report --json` to compare against
        #[arg(long, value_name = "FILE")]
        previous: Option<PathBuf>,
        /// Emit the machine-readable JSON view instead of text
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SandboxCmd {
    /// Copy this project — repository with its ledger, journal, registry
    /// entry, and configuration — into the prefix, so the copy answers
    /// `arc catchup` the way the source does
    Clone {
        /// Absolute prefix to build the sandbox under
        prefix: String,
        /// Emit the report as structured JSON
        #[arg(long)]
        json: bool,
    },
    /// Report what a sandbox's ledger events, journal events, and Git refs
    /// differ by from the project it was cloned from
    Diff {
        /// Prefix holding the sandbox
        prefix: String,
        /// Emit the report as structured JSON
        #[arg(long)]
        json: bool,
    },
    /// Remove a sandbox arc made, refusing any prefix that is not one
    Discard {
        /// Prefix holding the sandbox
        prefix: String,
    },
}

#[derive(Subcommand)]
enum HooksCmd {
    /// Install the arc hook scripts into this repository's hooks dir
    ///
    /// Both hooks are advisory and always exit 0. `post-commit` says when the
    /// commit staled an approval or landed on a closed change's branch;
    /// `prepare-commit-msg` appends an `Arc-Change: <change-id>` trailer on an
    /// open change's branch when the message lacks one. The hooks directory
    /// honours `core.hooksPath`
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
    /// Withdraw a recorded history-rewritten map by event ID; other event
    /// types are refused. Ref moves are not undone; output names recorded
    /// moves, or says when the map has no ref-move information. Withdrawals
    /// travel with maps in bundles and are honoured by importing replicas
    Withdraw {
        /// Exact repository event ID of the history-rewritten map
        event_id: String,
        /// Why this map must not participate in revision resolution
        #[arg(long)]
        reason: String,
    },
    /// Record a rewrite performed elsewhere, with its commit map. Active
    /// maps must agree; a withdrawn map does not constrain its replacement
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
    /// Show where a recorded revision ended up through active maps.
    /// Withdrawn maps are ignored; exits 2 when no recorded rewrite moved it
    Resolve {
        /// A revision a rewrite may have moved; the surviving one is printed
        revision: String,
    },
}

#[derive(Subcommand)]
enum RewriteCmd {
    /// Recreate every commit from --from to the branch head so it is signed by
    /// one key, moving the refs and recording the map
    Sign {
        /// The key to sign with; Git's configured signing key by default
        #[arg(long)]
        key: Option<String>,
        /// Oldest commit to recreate, inclusive. Defaults to the oldest not
        /// signed by the key in target..head on a tracked change branch, or in
        /// the whole history outside a change
        #[arg(long)]
        from: Option<String>,
        /// Permit recreating commits reachable from the target or another
        /// local branch, and default to the whole history. Without this flag
        /// shared commits are refused before any moves, including --dry-run
        #[arg(long)]
        include_shared: bool,
        /// Print the map the rewrite would record and stop
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Recreate the commits without signing them, for a repository with no
        /// signing key
        #[arg(long = "no-sign", conflicts_with = "key")]
        no_sign: bool,
        /// Recreate each annotated tag whose target was rewritten on the
        /// commit that replaced it, carrying its message, tagger and date, and
        /// signing it with the key the commits are signed with; --no-sign
        /// leaves it unsigned
        #[arg(long)]
        retag: bool,
    },
    /// Edit the trailers of every commit message from --from to the branch
    /// head, recreating only the commits that change and what sits on them
    Trailers {
        /// Remove every trailer with this key, matched without case; repeat
        /// for more than one
        #[arg(long = "drop", value_name = "KEY", required_unless_present = "append")]
        drop: Vec<String>,
        /// Add this `Key: value` line to the trailer block where it is not
        /// already there; repeat for more than one
        #[arg(long, value_name = "LINE", required_unless_present = "drop")]
        append: Vec<String>,
        /// Oldest commit whose trailers are edited; required, since a trailer
        /// edit has nothing to infer a range from
        #[arg(long, required = true)]
        from: String,
        /// The key to sign a recreated commit with; Git's configured signing
        /// key by default
        #[arg(long)]
        key: Option<String>,
        /// Print the map the rewrite would record and stop
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Recreate the edited commits without signing them, for a repository
        /// with no signing key
        #[arg(long = "no-sign", conflicts_with = "key")]
        no_sign: bool,
        /// Recreate each annotated tag whose target was rewritten on the
        /// commit that replaced it, carrying its message, tagger and date, and
        /// signing it with the key the commits are signed with; --no-sign
        /// leaves it unsigned
        #[arg(long)]
        retag: bool,
    },
}

#[derive(Subcommand)]
enum CandidateCmd {
    /// Register a candidate: a tree answering one version of a change's
    /// brief, with who produced it and where it came from.
    ///
    /// Records `candidate-registered` as a repository event and pins the tree
    /// at `refs/arc/candidate/<id>`. Registering opens no change and creates
    /// no patchset. Prints `candidate: <id>`. Two registrations of one tree
    /// are two candidates that share storage and nothing else.
    ///
    /// Refused, naming the rule and writing nothing: `duplicate-candidate`
    /// for an id already registered; `no-producers` with no --producer;
    /// `unknown-tree` for a tree or commit the object store does not hold;
    /// `unknown-brief` for a reference that resolves to no recorded brief;
    /// `unknown-parent` or `unknown-adopted` for an id not registered;
    /// `parent-other-contract` for a parent answering another change or
    /// brief version; `adoption-drops-producer`, naming them, when the
    /// producers omit anyone along the adopted registration's parent chain;
    /// `unknown-episode` for a claim not recorded on the brief's change.
    Register {
        /// The content: a tree, or a commit, which is recorded as its tree
        #[arg(long)]
        tree: String,
        /// The contract answered, `<change>[@<brief-event>]`; the latest
        /// brief version when no event is named. Recorded with a `sha256:`
        /// digest of the brief body
        #[arg(long)]
        brief: String,
        /// Who produced the content, repeated for each; at least one
        #[arg(long = "producer")]
        producers: Vec<String>,
        /// A registration this one continues, such as the candidate a repair
        /// repairs; it must answer the same brief version. Repeatable
        #[arg(long = "parent")]
        parents: Vec<String>,
        /// A registration whose content this one carries into its own
        /// contract; the producers must include every producer along its
        /// parent chain. Repeatable
        #[arg(long = "adopts")]
        adopts: Vec<String>,
        /// A claim id recorded on the brief's change that the work ran
        /// under. Repeatable
        #[arg(long = "episode")]
        episodes: Vec<String>,
        /// The candidate id; generated from the change's slug when omitted
        #[arg(long)]
        id: Option<String>,
    },
    /// Show one candidate: tree, contract, producers, parents, adoptions,
    /// episodes, judgements, retirement, whether its pin holds the tree,
    /// evaluations, selections naming it (promoted, superseded, or not
    /// promoted), promotion refs no `candidate-promoted` event records, and
    /// every registration sharing its tree. `--json` emits `arc-candidate/1`
    Show {
        /// Candidate id
        id: String,
        /// Emit `arc-candidate/1` instead of text
        #[arg(long)]
        json: bool,
    },
    /// List registrations as `show` renders them, grouping every tree more
    /// than one registration names under `shared_trees`. `--json` emits
    /// `arc-candidate/1`
    List {
        /// Only candidates answering this change's brief
        #[arg(long)]
        brief: Option<String>,
        /// Emit `arc-candidate/1` instead of text
        #[arg(long)]
        json: bool,
    },
    /// Record a judgement of a candidate by its declarant: rejected, or
    /// superseded by another registration. A judgement changes no
    /// registration and selects nothing; `unknown-candidate` refuses an id
    /// not registered
    #[command(group(clap::ArgGroup::new("judgement").required(true).args(["rejected", "superseded_by"])))]
    Judge {
        /// Candidate id
        id: String,
        /// A considered alternative not taken
        #[arg(long)]
        rejected: bool,
        /// The registration that answers in this one's place
        #[arg(long = "superseded-by")]
        superseded_by: Option<String>,
        /// Why
        #[arg(long, required = true)]
        reason: String,
    },
    /// Delete `refs/arc/candidate/<id>` and record `candidate-retired`, only
    /// when no root reaches the candidate. Roots are selections and
    /// promotions, and a root reaches what its candidate's parents and
    /// adoptions carry; when one does, exit 1 naming it, writing nothing.
    /// The registration stands. Retiring a retired candidate says so and
    /// exits 0. arc never retires a candidate on its own
    Retire {
        /// Candidate id
        id: String,
    },
    /// Evaluate a candidate: run the required gates of its brief's change,
    /// as that change's target declares them, against the candidate's tree.
    ///
    /// The tree is checked out in a scratch worktree, removed afterwards,
    /// and each gate's environment probe runs there first. Each gate records
    /// `candidate-verified` as a repository event keyed by the tree, with
    /// the gate, its command, timeout, and environment probe as declared
    /// when it ran, the environment identity the probe yielded, and the
    /// result; `evaluation: <event>` names it for `select --evaluation`.
    /// Exits 0 when every gate run passes and 1 otherwise. Refused:
    /// `unknown-candidate`; `unknown-gate` for a --gate the change is not
    /// required to pass; `target-unreadable` when the target cannot be read
    Verify {
        /// Candidate id
        id: String,
        /// Run only this required gate
        #[arg(long)]
        gate: Option<String>,
    },
    /// Validate a named selection and, when every ground holds, record it
    /// and promote the chosen candidate into its brief's change.
    ///
    /// arc never chooses: the proposal names the registration, the
    /// destination, the target head decided against, and the evaluations
    /// relied on. Every failing ground is reported, one line each led by its
    /// code, exit 1 and nothing written: `reuse-policy-undeclared` without
    /// `[candidates] evaluation_reuse`; `unknown-candidate`, and
    /// `candidate-retired` for a registration whose pin was released;
    /// `destination-other-contract` when the registration answers another
    /// change's brief; `destination-closed`;
    /// `target-moved` when --target is not the target head now;
    /// `environment-unobserved` for a gate whose probe yields no identity at
    /// the shipped tree. Each required gate needs a named, passing evaluation
    /// at the shipped tree (the chosen registration's), under the declaration
    /// in force and in the environment observed now, from the chosen
    /// registration or, under `matching-coordinates`, from any registration
    /// whose tree, declaration, and environment match; otherwise, per named
    /// evaluation, `unrecorded`, `other-registration` (under `never`),
    /// `other-tree`, `other-declaration`, `environment-unrecorded`,
    /// `environment-other`, or `failed`, and `no-evaluation` for a gate none
    /// is named for. Each `--must-read` of the registration's brief version
    /// needs a tool read (`arc context read`) on the registration or one
    /// along its parent chain, by an episode one of them cites, at the
    /// required version (an artifact's body digest; a file's blob, inferred
    /// or by the digest of its bytes over the range read), covering the
    /// required extent; otherwise `partial`, `unknown-coverage`,
    /// `only-declared`, `only-supplied` (the contract's own plan or opening
    /// artifact), or `not-read`. Reads through an adoption do not count.
    ///
    /// A permitted selection records `candidate-selected` with its basis:
    /// the chosen registration, destination head, target, the evaluation
    /// and read meeting each requirement, the reuse policy, the
    /// contributors (the producers along the parent chain), the selector,
    /// and the rationale. A selection replacing an unpromoted one for the
    /// same destination names it as superseded. The promotion then runs as
    /// `arc candidate promote` describes; its refusal leaves the selection
    /// standing without one
    Select {
        /// The registration chosen
        #[arg(long)]
        chosen: String,
        /// The destination change: the change whose brief it answers
        #[arg(long)]
        into: String,
        /// The destination's target head the choice was made against
        #[arg(long)]
        target: String,
        /// A `candidate-verified` event relied on for a required gate
        /// (repeatable)
        #[arg(long = "evaluation")]
        evaluations: Vec<String>,
        /// Why this candidate: text, or `@<file>`
        #[arg(long)]
        rationale: String,
    },
    /// Promote a recorded selection, or finish one an interrupted run left.
    ///
    /// The promotion holds the destination's transition lock and the
    /// repository-events lock. It re-reads the destination head and target
    /// and refuses `basis-moved` when either differs from the selection's
    /// basis, recording nothing: a basis is never reused, so select again.
    /// It refuses a checkout of the destination branch with tracked
    /// modifications, or one holding untracked or ignored paths the update
    /// would write. It commits the shipped tree onto the destination head
    /// with the selector as committer, keeps it at
    /// `refs/arc/candidate-promotion/<candidate>/<selection>`, moves the
    /// branch from the head it read (a concurrent move stops the promotion
    /// there), updates the checkout, records a patchset with the selection's
    /// contributors and a candidate link, then `candidate-promoted`.
    ///
    /// Given a promotion ref with no `candidate-promoted` event, it records
    /// the patchset and the promotion when the branch already points at the
    /// ref's commit, and otherwise deletes the ref and says so. A promoted
    /// selection is a no-op that says so; a superseded one is refused
    Promote {
        /// The `candidate-selected` event
        selection: String,
    },
}

#[derive(Subcommand)]
enum ContextCmd {
    /// Record a read record: a tool's record that a call succeeded and
    /// returned bytes for a path and range, within one episode of work on a
    /// subject.
    ///
    /// The read's version is the `sha256:` of the bytes the tool returned.
    /// Coverage is the range the tool recorded: `--lines`, `--whole`, or,
    /// with neither, `unknown`, which never counts as whole. A failed call is
    /// never a read. With `--at`, the returned bytes are compared with the
    /// path's blob at that revision over the same range (lines with their
    /// terminators; the whole blob for `whole` or `unknown`): when they are
    /// equal, the blob is recorded as an inferred locator labelled
    /// `content-matches-revision`; when they differ, as under uncommitted
    /// edits or a tool that decorates what it returns, or when the revision
    /// holds no such path, no blob is recorded and the read stands on its
    /// digest alone. A path directly inside a journal directory or its cold
    /// archive is also recorded as that journal artifact, with the artifact
    /// body's digest at recording, which is compared with the read's and
    /// never assumed equal: by file name in this project's journal, and as
    /// the qualified `<journal-dir>::<file>` in another project's.
    ///
    /// `--from-tapes <file>` reads a tool record document instead:
    /// `tapes-events/9` (`tapes events <session> --json`) or
    /// `tapes-session/14` (the `.json` of a `tapes export` bundle, which
    /// carries the same events). Every tool call with a `read` member becomes
    /// a read record: its `event_id` is the record id, `read.path` the path,
    /// `read.lines` a line range or `read.whole` whole coverage (neither is
    /// `unknown`), and `read.sha256` the digest. A call with no stable event
    /// id, no path, or no digest, one that failed (`read.succeeded` false, or
    /// its own or its paired result's `status` `error` or `failed`), and a
    /// record id already recorded on the subject are each skipped with one
    /// printed line naming the event and the reason. arc reads only the file
    /// it is handed; it opens no harness store and runs no program.
    /// `read.sha256` is the digest of the text the tool returned, recorded as
    /// tapes reports it and never normalized: a tool that returns decorated
    /// text, such as Claude Code's line-numbered `Read`, has a digest that
    /// equals no file's bytes, so `content-matches-revision` does not fire
    /// for it.
    ///
    /// A change subject's reads are events on that change; a candidate's are
    /// repository events, so every bundle carries them. Refused, writing
    /// nothing: `unknown-subject`; `unknown-episode` for a claim not recorded
    /// on the subject's change (a candidate's brief change);
    /// `duplicate-record` for a record id already recorded on the subject;
    /// `malformed-digest`; `unknown-revision` for an `--at` naming no
    /// commit.
    #[command(group(clap::ArgGroup::new("source").required(true).args(["record", "from_tapes"])))]
    Read {
        /// The change or candidate id the read belongs to
        #[arg(long)]
        subject: String,
        /// The claim id the read happened under
        #[arg(long)]
        episode: String,
        /// The tool record's stable identifier
        #[arg(long, requires = "path", requires = "digest")]
        record: Option<String>,
        /// The path the tool recorded
        #[arg(long, conflicts_with = "from_tapes")]
        path: Option<String>,
        /// `sha256:` over the bytes the tool returned
        #[arg(long, conflicts_with = "from_tapes")]
        digest: Option<String>,
        /// The line range the tool recorded, `<from>-<to>`, one-based and
        /// inclusive
        #[arg(long, value_parser = parse_line_range, conflicts_with_all = ["whole", "from_tapes"])]
        lines: Option<(u64, u64)>,
        /// The tool recorded the whole file
        #[arg(long, conflicts_with = "from_tapes")]
        whole: bool,
        /// Compare the returned bytes with the path's blob at this revision
        #[arg(long)]
        at: Option<String>,
        /// A `tapes-events/9` or `tapes-session/14` document to take the
        /// reads from
        #[arg(long = "from-tapes", value_name = "FILE")]
        from_tapes: Option<std::path::PathBuf>,
    },
    /// Record a declaration: the declarant's claim that a subject cites,
    /// relies on, or considers a path (optionally at a revision) or a
    /// journal artifact.
    ///
    /// A declaration is attributed to its declarant, never checked against
    /// what was read, and never becomes a read. `--citation` points it at a
    /// read record on the same subject; one that resolves to no recorded
    /// read is refused as `unknown-citation`, writing nothing.
    #[command(group(clap::ArgGroup::new("relation").required(true).args(["cites", "relies_on", "considers"])))]
    #[command(group(clap::ArgGroup::new("target").required(true).args(["path", "artifact"])))]
    Declare {
        /// The change or candidate id the declaration is about
        #[arg(long)]
        subject: String,
        /// The subject cites the target
        #[arg(long)]
        cites: bool,
        /// The subject relies on the target
        #[arg(long = "relies-on")]
        relies_on: bool,
        /// The subject considered the target
        #[arg(long)]
        considers: bool,
        /// A path
        #[arg(long)]
        path: Option<String>,
        /// The revision the path is meant at
        #[arg(long, requires = "path")]
        at: Option<String>,
        /// A journal artifact: a file name in this project's journal, or
        /// `<journal-dir>::<file>` in another project's
        #[arg(long)]
        artifact: Option<String>,
        /// A tool record id recorded as a read on the same subject
        #[arg(long)]
        citation: Option<String>,
    },
    /// Record what the provider reported for the retention of a read
    /// record's recording (`tapes capture <session> --json` is that report).
    ///
    /// A capture report is a claim about retention, attributed to its
    /// declarant; the latest report for a record stands. `arc explain`
    /// reads a read whose recording has no standing `pinned` report as `at
    /// risk`. The report joins the ledger of the one subject holding the
    /// record; `--subject` names it when several do. Refused, writing
    /// nothing: `unknown-record` for a record not recorded as a read;
    /// `ambiguous-record` when several subjects hold it and none is named.
    #[command(group(clap::ArgGroup::new("state").required(true).args(["pinned", "unpinned"])))]
    Capture {
        /// The tool record id of a recorded read
        #[arg(long)]
        record: String,
        /// The subject holding the read, when more than one does
        #[arg(long)]
        subject: Option<String>,
        /// The provider reported the recording pinned
        #[arg(long)]
        pinned: bool,
        /// The provider reported the recording not pinned
        #[arg(long)]
        unpinned: bool,
    },
}

fn parse_line_range(raw: &str) -> Result<(u64, u64), String> {
    let (from, to) = raw
        .split_once('-')
        .ok_or_else(|| format!("{raw:?} is not <from>-<to>"))?;
    let from: u64 = from
        .parse()
        .map_err(|_| format!("{from:?} is not a line number"))?;
    let to: u64 = to
        .parse()
        .map_err(|_| format!("{to:?} is not a line number"))?;
    if from == 0 || to < from {
        return Err(format!("{raw:?} is not a one-based, ascending range"));
    }
    Ok((from, to))
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
            Cmd::External { .. } => Some(("external verdict", "reviewer or lead")),
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
        Some("suggest") => Some("journal suggest"),
        Some("position") => Some("journal position"),
        Some("latest") => Some("journal latest"),
        Some("source") => Some("journal source"),
        Some("source-attach") => Some("journal source-attach"),
        Some("install") => Some("hooks install"),
        Some("uninstall") => Some("hooks uninstall"),
        _ => None,
    }
}

/// Identity flags such as `--actor` and `--harness` are global and read the
/// environment, so every command records who is acting without being told. A
/// command that also filters on one of those names gets a single argument for
/// both meanings, because clap allows one argument per name. Filters are
/// explicit: a read narrows only on a value the caller typed, never on the
/// ambient identity.
fn parse_cli() -> Result<Cli, clap::Error> {
    let matches = Cli::command().try_get_matches()?;
    let mut cli = Cli::from_arg_matches(&matches)?;
    if let Some(Cmd::Query { actor, harness, .. }) = cli.cmd.as_mut() {
        let query = matches.subcommand_matches("query");
        for (id, filter) in [("actor", actor), ("harness", harness)] {
            if !typed_on_command_line(query, id) {
                *filter = None;
            }
        }
    }
    Ok(cli)
}

/// Name the attached form when the unknown argument was meant as the value of
/// the long option typed before it.
///
/// A value that starts with `-` reads as the next option, so `--evidence
/// "--at …"` leaves `--at …` unknown, and clap's tips — a similarly spelled
/// option, or `-- --at …` — pass it as something else. Attached with `=`, it
/// is the option's value.
fn attach_hyphenated_value_tip(error: &mut clap::Error) {
    use clap::error::{ContextKind, ContextValue};
    let Some(ContextValue::String(invalid)) = error.get(ContextKind::InvalidArg) else {
        return;
    };
    let invalid = invalid.clone();
    let args = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let Some(position) = args.iter().position(|arg| *arg == invalid) else {
        return;
    };
    let Some((option, before)) = args[..position].split_last() else {
        return;
    };
    let Some(long) = option
        .strip_prefix("--")
        .filter(|name| !name.is_empty() && !name.contains('='))
    else {
        return;
    };
    let mut command = Cli::command();
    command.build();
    for arg in before {
        if let Some(subcommand) = command.find_subcommand(arg).cloned() {
            command = subcommand;
        }
    }
    let takes_value = command
        .get_arguments()
        .any(|arg| arg.get_long() == Some(long) && arg.get_action().takes_values());
    if !takes_value {
        return;
    }
    error.remove(ContextKind::SuggestedArg);
    error.insert(
        ContextKind::Suggested,
        ContextValue::StyledStrs(vec![format!(
            concat!(
                "'{0}' takes a value; to pass one that starts with '-', ",
                "attach it: '{0}={1}'"
            ),
            option, invalid
        )
        .into()]),
    );
    error.insert(
        ContextKind::Usage,
        ContextValue::StyledStr(command.render_usage()),
    );
}

fn typed_on_command_line(matches: Option<&ArgMatches>, id: &str) -> bool {
    matches.and_then(|matches| matches.value_source(id)) == Some(ValueSource::CommandLine)
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

    let cli = match parse_cli() {
        Ok(cli) => cli,
        Err(mut error) => {
            let kind = error.kind();
            if kind == clap::error::ErrorKind::UnknownArgument {
                attach_hyphenated_value_tip(&mut error);
            }
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

/// Resolve the shared workspace scope flags into the one selection the scoped
/// workspace projections read, so `--under` and `--here` mean the same thing
/// in `inbox` and `backlog`.
fn workspace_scope(under: Option<PathBuf>, here: bool) -> Result<commands::WorkspaceScope> {
    match (under, here) {
        (Some(path), false) => Ok(commands::WorkspaceScope::Under(path)),
        (None, true) => Ok(commands::WorkspaceScope::Under(std::env::current_dir()?)),
        (None, false) => Ok(commands::WorkspaceScope::Global),
        (Some(_), true) => unreachable!("clap rejects conflicting scopes"),
    }
}

fn run(cli: Cli) -> Result<i32> {
    // Export the prefix before anything resolves a path, so the flag and the
    // variable are one input and every command arc runs inherits the sandbox.
    if let Some(prefix) = cli.sandbox.as_deref().filter(|value| !value.is_empty()) {
        std::env::set_var(config::SANDBOX_VAR, prefix);
    }
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
    let git_user = || {
        gitio::git(&cwd, &["config", "user.name"])
            .ok()
            .map(|name| name.trim().to_string())
    };
    let (mut actor, mut actor_source) =
        match (cli.actor.filter(|value| !value.trim().is_empty()), from_env) {
            (Some(declared), _) => (declared, ActorSource::Flag),
            (None, Some(declared)) => (declared, ActorSource::Env),
            (None, None) => (
                git_user().unwrap_or_else(|| "unknown".into()),
                ActorSource::GitFallback,
            ),
        };
    let mut harness = cli.harness;
    let mut session = cli.session;
    let mut session_resolution = None;
    // An empty --model is the same as absent.
    let from_env = std::env::var("ARC_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let (model, model_source) = match (cli.model.filter(|value| !value.trim().is_empty()), from_env)
    {
        (Some(model), _) => (Some(model), Some(model::ModelSource::Flag)),
        (None, Some(model)) => (Some(model), Some(model::ModelSource::Env)),
        _ => (None, None),
    };
    let model_attribution = std::rc::Rc::new(std::cell::OnceCell::new());
    if (harness.is_none() || session.is_none())
        && config::load()
            .map(|config| config.identity_detect)
            .unwrap_or(false)
    {
        // A process carrying several harnesses' session variables and no
        // ancestry that names the owner records no identity at all: picking
        // one by list position is how work gets attributed to a thread that
        // was only supervising.
        if let context::Detection::Resolved(detected) = context::detect_identity() {
            if harness
                .as_deref()
                .is_none_or(|explicit| explicit == detected.harness)
            {
                harness.get_or_insert(detected.harness.clone());
                // A harness recognized without its cooperation carries no
                // session id; recording the harness alone is the honest half
                // of the detection, not a partial failure.
                if let Some(detected_session) = &detected.session {
                    if session.is_none() {
                        // The store's answer is about the session detection
                        // supplied; a session the caller declared was never
                        // asked about, so it carries no report.
                        session_resolution = Some(detected_session.resolution);
                        session = Some(detected_session.id.clone());
                    }
                    if session.as_deref() == Some(detected_session.id.as_str()) {
                        let _ = model_attribution.set(context::model_attribution(
                            model.as_deref(),
                            model_source,
                            Some(detected),
                        ));
                    }
                }
            }
        }
    }
    // The session a command runs in names the acting agent better than the
    // checkout's Git identity does, which is kept as the operator instead.
    let mut operator = None;
    if actor_source == ActorSource::GitFallback {
        let known = |value: &Option<String>| value.clone().filter(|value| !value.trim().is_empty());
        if let (Some(harness), Some(session)) = (known(&harness), known(&session)) {
            actor = format!("{harness}:{session}");
            actor_source = ActorSource::Derived;
            operator = git_user();
        }
    }
    let ctx = Ctx {
        cwd,
        actor,
        actor_source,
        operator,
        fallback_announced: std::cell::Cell::new(false),
        harness,
        session,
        session_link: cli.session_link.filter(|value| !value.trim().is_empty()),
        session_resolution,
        model,
        model_source,
        model_attribution,
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
            from_fork,
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
                from_fork,
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
        Cmd::Explain { change, at, json } => {
            let change = infer(change.as_deref())?;
            explain::explain(&ctx, &change, at.as_deref(), json)?;
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
            provenance,
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
            let view = match (by_model, provenance) {
                (true, _) => commands::StatsView::ByModel,
                (_, true) => commands::StatsView::Provenance,
                _ => commands::StatsView::Changes,
            };
            commands::stats(&ctx, selection, json, view)?;
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
            must_read,
            json,
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
                must_read,
                json,
            )
        }
        Cmd::Changelog {
            change,
            category,
            body_file,
            none: _,
            reason,
            json,
            provenance,
            since,
            write,
            keep_unrecorded,
        } => commands::changelog(
            &ctx,
            role,
            select(change)?.as_deref(),
            category,
            body_file,
            reason,
            json,
            provenance,
            since,
            write,
            keep_unrecorded,
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
        Cmd::Export {
            change,
            output,
            since,
        } => {
            commands::export_bundle(&ctx, &change, &output, since.as_deref())?;
            Ok(0)
        }
        Cmd::Import { input, dry_run } => commands::import_bundle(&ctx, &input, dry_run),
        Cmd::Replica { cmd } => match cmd {
            ReplicaCmd::Id { json } => {
                commands::replica_id(&ctx, json)?;
                Ok(0)
            }
            ReplicaCmd::Init { name } => {
                commands::replica_init(&ctx, &name)?;
                Ok(0)
            }
            ReplicaCmd::Pair {
                name,
                repository_id,
            } => {
                commands::replica_pair(&ctx, &name, &repository_id)?;
                Ok(0)
            }
            ReplicaCmd::Export { output } => {
                commands::replica_export(&ctx, &output)?;
                Ok(0)
            }
            ReplicaCmd::Import { input, dry_run } => {
                commands::replica_import(&ctx, &input, dry_run)
            }
            ReplicaCmd::Status { json } => {
                commands::replica_status(&ctx, json)?;
                Ok(0)
            }
            ReplicaCmd::Authority { cmd } => match cmd {
                ReplicaAuthorityCmd::Offer { to } => {
                    commands::replica_offer(&ctx, &to)?;
                    Ok(0)
                }
                ReplicaAuthorityCmd::Reclaim { because } => {
                    commands::replica_reclaim(&ctx, &because)?;
                    Ok(0)
                }
                ReplicaAuthorityCmd::ConfirmReturn => {
                    commands::replica_confirm_return(&ctx)?;
                    Ok(0)
                }
            },
        },
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
            because,
        } => {
            if because.is_some() && !takeover {
                bail!(
                    "--because records why a takeover cut a lease short; it applies \
                     only with --takeover"
                );
            }
            match artifact(&ctx, change.as_deref()) {
                Some(file) => {
                    if !stage_budget.is_empty() {
                        bail!(
                            "--stage-budget applies to a change; an artifact's lease is the \
                             whole of what expires"
                        );
                    }
                    journal::claim_artifact(
                        &ctx,
                        &file,
                        ttl.as_deref(),
                        takeover,
                        because.as_deref(),
                    )
                }
                None => {
                    let change = infer(change.as_deref())?;
                    commands::claim(&ctx, &change, ttl, stage_budget, takeover, because)
                }
            }
        }
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
        Cmd::Squash {
            change,
            message,
            attribution,
        } => {
            let change = infer(change.as_deref())?;
            commands::squash(
                &ctx,
                &change,
                &message,
                attribution.contributors,
                attribution.solo,
            )?;
            Ok(0)
        }
        Cmd::Snapshot {
            change,
            base,
            brief_version,
            verify,
            gate,
            all,
            attribution: AttributionOpts { contributors, solo },
            amend,
            links,
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
                    links.journal_ref,
                    links.thread,
                )
            }
        }
        Cmd::Keep {
            kind,
            change,
            body,
            evidence,
            cites,
        } => {
            let change = infer(change.as_deref())?;
            let text = commands::read_body(body.body, body.body_file)?;
            commands::keep(&ctx, &change, kind.into(), text, evidence, cites)?;
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
            attribution,
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
                        contributors: attribution.contributors,
                        solo: attribution.solo,
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
        Cmd::External { cmd } => match cmd {
            ExternalCmd::Verdict {
                change,
                verdict,
                decided_by,
                reference,
                revision,
                findings_json,
            } => {
                commands::record_external_verdict(
                    &ctx,
                    &change,
                    commands::ExternalVerdictArgs {
                        verdict,
                        decided_by,
                        reference,
                        revision,
                        findings_json,
                    },
                )?;
                Ok(0)
            }
        },
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
            environment,
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
                    environment,
                    note,
                    waive_dirty,
                    falsified_by,
                    predicted,
                    against,
                },
            )
        }
        Cmd::Done {
            change,
            attribution,
            links,
        } => {
            let change = infer(change.as_deref())?;
            commands::done(
                &ctx,
                &change,
                attribution.contributors,
                attribution.solo,
                links.journal_ref,
                links.thread,
            )
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
                        "--transcript reads the session recorded against a change; \
                         an artifact's record of what happened is its checkpoints"
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
            ForkCmd::Adopt {
                slug,
                branch,
                intent,
            } => fork::adopt(&ctx, &slug, branch.as_deref(), intent.as_deref()),
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
            json,
            expect_basis,
            debt,
            debt_kind,
        } => {
            // A list names its own members; `--change` names one, and mixing
            // the two spellings leaves no reading of what the run was asked
            // to land.
            let changes = match change.as_slice() {
                [] => select(None)?.into_iter().collect::<Vec<_>>(),
                [one] => select(Some(one.clone()))?.into_iter().collect(),
                many if flag_change.is_some() => {
                    bail!(
                        "{} changes were named as arguments and one more as --change; \
                         name them all the same way",
                        many.len()
                    )
                }
                many => many.to_vec(),
            };
            commands::integrate(
                &ctx,
                &changes,
                commands::IntegrateArgs {
                    tags: tag,
                    into,
                    message,
                    cleanup,
                    dry_run,
                    json,
                    expect_basis,
                    debt: debt.map(|reason| commands::DebtDeclaration {
                        reason,
                        kind: debt_kind,
                    }),
                },
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
            external_reference,
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
                    external_reference,
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
            HistoryCmd::Withdraw { event_id, reason } => {
                commands::withdraw_rewrite(&ctx, &event_id, reason)?;
                Ok(0)
            }
            HistoryCmd::Rewrite { map, reason, tool } => {
                commands::record_rewrite(&ctx, &map, reason, tool)?;
                Ok(0)
            }
            HistoryCmd::Resolve { revision } => commands::resolve_rewritten(&ctx, &revision),
        },
        Cmd::Rewrite { cmd } => match cmd {
            RewriteCmd::Sign {
                key,
                from,
                include_shared,
                dry_run,
                no_sign,
                retag,
            } => commands::rewrite_sign(
                &ctx,
                commands::SignArgs {
                    key,
                    from,
                    include_shared,
                    dry_run,
                    no_sign,
                    retag,
                },
            ),
            RewriteCmd::Trailers {
                drop,
                append,
                from,
                key,
                dry_run,
                no_sign,
                retag,
            } => commands::rewrite_trailers(
                &ctx,
                commands::TrailerArgs {
                    key,
                    from,
                    drop,
                    append,
                    dry_run,
                    no_sign,
                    retag,
                },
            ),
        },
        Cmd::Candidate { cmd } => match cmd {
            CandidateCmd::Register {
                tree,
                brief,
                producers,
                parents,
                adopts,
                episodes,
                id,
            } => commands::candidate::register(
                &ctx,
                commands::candidate::RegisterArgs {
                    tree,
                    brief,
                    producers,
                    parents,
                    adopts,
                    episodes,
                    id,
                },
            )
            .map(|_| 0),
            CandidateCmd::Show { id, json } => {
                commands::candidate::show(&ctx, &id, json).map(|_| 0)
            }
            CandidateCmd::List { brief, json } => {
                commands::candidate::list(&ctx, brief.as_deref(), json).map(|_| 0)
            }
            CandidateCmd::Judge {
                id,
                rejected: _,
                superseded_by,
                reason,
            } => commands::candidate::judge(&ctx, id, superseded_by, reason).map(|_| 0),
            CandidateCmd::Retire { id } => commands::candidate::retire(&ctx, id).map(|_| 0),
            CandidateCmd::Verify { id, gate } => {
                commands::selection::verify(&ctx, &id, gate.as_deref())
            }
            CandidateCmd::Select {
                chosen,
                into,
                target,
                evaluations,
                rationale,
            } => commands::selection::select(
                &ctx,
                commands::selection::SelectArgs {
                    chosen,
                    into,
                    target,
                    evaluations,
                    rationale,
                },
            ),
            CandidateCmd::Promote { selection } => commands::selection::promote(&ctx, &selection),
        },
        Cmd::Context { cmd } => match cmd {
            ContextCmd::Read {
                subject,
                episode,
                record,
                path,
                digest,
                lines,
                whole,
                at,
                from_tapes,
            } => match (from_tapes, record, path, digest) {
                (Some(file), _, _, _) => commands::relations::read_from_tapes(
                    &ctx,
                    commands::relations::TapesArgs {
                        subject,
                        episode,
                        file,
                        at,
                    },
                ),
                (None, Some(record), Some(path), Some(digest)) => commands::relations::read(
                    &ctx,
                    commands::relations::ReadArgs {
                        subject,
                        episode,
                        record,
                        path,
                        digest,
                        coverage: match (lines, whole) {
                            (Some((from, to)), _) => model::ReadCoverage::Lines { from, to },
                            (None, true) => model::ReadCoverage::Whole,
                            (None, false) => model::ReadCoverage::Unknown,
                        },
                        at,
                    },
                ),
                _ => Err(anyhow::anyhow!(
                    "--record needs --path and --digest; or read from --from-tapes"
                )),
            }
            .map(|_| 0),
            ContextCmd::Declare {
                subject,
                cites: _,
                relies_on,
                considers,
                path,
                at,
                artifact,
                citation,
            } => commands::relations::declare(
                &ctx,
                commands::relations::DeclareArgs {
                    subject,
                    relation: if relies_on {
                        model::DeclaredRelation::ReliesOn
                    } else if considers {
                        model::DeclaredRelation::Considers
                    } else {
                        model::DeclaredRelation::Cites
                    },
                    path,
                    at,
                    artifact,
                    citation,
                },
            )
            .map(|_| 0),
            ContextCmd::Capture {
                record,
                subject,
                pinned,
                unpinned: _,
            } => commands::relations::capture(
                &ctx,
                record,
                subject,
                if pinned {
                    model::CaptureState::Pinned
                } else {
                    model::CaptureState::Unpinned
                },
            )
            .map(|_| 0),
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
        Cmd::Policy { cmd } => match cmd {
            PolicyCmd::Show => commands::policy_show(&ctx),
            PolicyCmd::Path => commands::policy_path(&ctx),
            PolicyCmd::Write { body_file } => {
                let text = commands::read_body(None, Some(body_file))?;
                commands::policy_write(&ctx, &text)
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
            // The same resolution `arc journal dir` answers, reported where a
            // caller already collects paths. Unresolved is a null plus the
            // resolver's diagnostic, not a failure: config stays usable in a
            // directory no journal anchors, and answers without creating one.
            let journal_resolution = journal::resolve_dir(&ctx.cwd);
            let (journal_dir, journal_error) = match journal_resolution {
                Ok(dir) => (Some(dir.display().to_string()), None),
                Err(error) => (None, Some(format!("{error:#}"))),
            };
            let mut resolved = serde_json::json!({
                "sandbox": cfg.sandbox.as_ref().map(|p| p.display().to_string()),
                "ai_home": cfg.ai_home.display().to_string(),
                "config_file": cfg.config_path.display().to_string(),
                "config_file_exists": cfg.config_path.is_file(),
                "worktrees_dir": cfg.worktrees_dir.display().to_string(),
                "data_root": cfg.data_root.map(|p| p.display().to_string()),
                "store_root_for_cwd": store_root,
                "journal_dir_for_cwd": journal_dir,
            });
            if let Some(error) = journal_error {
                resolved["journal_resolution_error"] = serde_json::json!(error);
            }
            println!("{}", serde_json::to_string_pretty(&resolved)?);
            Ok(0)
        }
        Cmd::Sandbox { cmd } => match cmd {
            SandboxCmd::Clone { prefix, json } => {
                commands::sandbox::clone(&ctx, Path::new(&prefix), json)
            }
            SandboxCmd::Diff { prefix, json } => {
                commands::sandbox::diff(&ctx, Path::new(&prefix), json)
            }
            SandboxCmd::Discard { prefix } => commands::sandbox::discard(&ctx, Path::new(&prefix)),
        },
        Cmd::Doctor { json, verbose } => commands::run_doctor(&ctx, json, verbose),
        Cmd::Instructions { cmd } => {
            let InstructionsCmd::Git { check } = cmd;
            commands::instructions_git(check.as_deref())
        }
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
                WorkspaceCmd::Inbox {
                    under,
                    here,
                    global: _,
                    json,
                } => (
                    commands::WorkspaceView::Inbox {
                        scope: workspace_scope(under, here)?,
                    },
                    json,
                ),
                WorkspaceCmd::Inventory {
                    storage,
                    under,
                    here,
                    global: _,
                    json,
                } => (
                    commands::WorkspaceView::Inventory {
                        scope: workspace_scope(under, here)?,
                        storage,
                    },
                    json,
                ),
                WorkspaceCmd::Report {
                    under,
                    here,
                    global: _,
                    previous,
                    json,
                } => (
                    commands::WorkspaceView::Report {
                        scope: workspace_scope(under, here)?,
                        previous,
                    },
                    json,
                ),
                WorkspaceCmd::Backlog {
                    since,
                    items,
                    under,
                    here,
                    global: _,
                    unreachable,
                    rank_by,
                    json,
                } => (
                    commands::WorkspaceView::Backlog {
                        since,
                        items,
                        scope: workspace_scope(under, here)?,
                        show_unreachable: unreachable,
                        rank_by,
                    },
                    json,
                ),
            };
            let code = commands::workspace(&ctx, view, json)?;
            Ok(code)
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
