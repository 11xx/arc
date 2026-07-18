mod bundle;
mod commands;
mod config;
mod forge;
mod gates;
mod gitio;
mod ids;
mod inbox;
mod journal;
mod model;
mod render;
mod state;
mod status;
mod store;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use commands::{AnchorArgs, Ctx, ListFormat, QueryArgs};
use model::{
    DispositionStatus, MessageSeverity, MessageType, Severity, Side, Verdict, VerifyResult,
};

/// Change, review, and integration state over plain Git for agentic
/// coding arcs. Git owns content and history; arc owns the collaboration
/// objects Git lacks: changes, patchsets, findings, verdicts, gates,
/// holds, and a guarded merge.
#[derive(Parser)]
#[command(name = "arc", version, about)]
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
    /// Execution boundary: implementer | reviewer | lead
    #[arg(long, global = true, env = "ARC_ROLE")]
    role: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
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
    },
    /// Record or read a change-scoped implementation contract
    Brief {
        change: String,
        /// Read a new brief body from a file ('-' for stdin)
        #[arg(long)]
        body_file: Option<String>,
        /// Optional title for a newly recorded brief
        #[arg(long)]
        title: Option<String>,
        /// Read one derived brief version instead of the latest
        #[arg(long)]
        version: Option<usize>,
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
    /// Lead-facing queue rollup across open changes, including active claim work (arc-inbox/1 schema)
    Inbox {
        /// Restrict to changes assigned to this harness
        #[arg(long = "assigned-to")]
        assigned_to: Option<String>,
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
    },
    /// Machine-readable status report (the versioned arc-status/5 schema)
    Status {
        change: String,
        /// Accepted for compatibility; status output is always JSON
        #[arg(long)]
        json: bool,
    },
    /// Report whether declared prerequisite changes have integrated
    BlockerStatus { change: String },
    /// Dependency probe: exit 0 ready, 1 blocked, 2 on lookup/ledger errors
    IsBlocked { change: String },
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
    },
    /// Wait for a change to reach a selected ledger-derived condition
    Watch {
        change: String,
        #[arg(long, value_enum)]
        until: commands::WatchUntil,
        /// Fail with exit 2 after this many seconds
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: Option<u64>,
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
    },
    /// Acquire or renew an advisory executor claim
    Claim {
        change: String,
        /// Lease duration (positive integer with s, m, or h suffix; default 2h)
        #[arg(long)]
        ttl: Option<String>,
        /// Override one stage budget as <name>=<duration> (repeatable)
        #[arg(long = "stage-budget")]
        stage_budget: Vec<String>,
    },
    /// Release the current advisory executor claim
    ReleaseClaim { change: String },
    /// Record typed executor progress (requires an owned live claim)
    Stage {
        change: String,
        #[arg(value_enum)]
        stage: commands::StageArg,
        #[arg(long)]
        note: Option<String>,
    },
    /// Record the current branch head as a new patchset
    Snapshot {
        change: String,
        /// Override the recorded base revision
        #[arg(long)]
        base: Option<String>,
    },
    /// Add a discussion comment
    Comment {
        change: String,
        #[command(flatten)]
        body: BodyOpts,
        #[arg(long)]
        patchset: Option<String>,
        #[command(flatten)]
        anchor: AnchorOpts,
    },
    /// Record a standalone review finding
    Finding {
        change: String,
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
    Reply {
        change: String,
        event_id: String,
        #[command(flatten)]
        body: BodyOpts,
    },
    /// Record a finding disposition (supersedes current tips automatically)
    Resolve {
        change: String,
        finding: String,
        #[arg(long, value_enum)]
        status: DispositionStatus,
        /// Fixing commit, when one exists
        #[arg(long)]
        commit: Option<String>,
        #[arg(long)]
        evidence: Option<String>,
    },
    /// Record a verdict, optionally with a findings batch, in one atomic event
    Review {
        change: String,
        #[arg(long, value_enum)]
        verdict: Verdict,
        /// Patchset under review (defaults to the latest)
        #[arg(long)]
        patchset: Option<String>,
        /// JSON array of findings ('-' for stdin); IDs are assigned by arc
        #[arg(long)]
        findings_json: Option<String>,
    },
    /// Run a declared gate (or ad hoc command) and record the evidence
    Verify {
        change: String,
        /// Run every gate declared for the change profile
        #[arg(long)]
        all: bool,
        /// Gate name from .arc/gates.toml
        #[arg(long)]
        gate: Option<String>,
        /// Ad hoc command (recorded, but not a declared gate)
        #[arg(long)]
        command: Option<String>,
        /// Record externally observed evidence without running the command
        /// (e.g. a sandboxed executor or another host ran the gate)
        #[arg(long)]
        attest: bool,
        /// The attested result; required with --attest, rejected without it
        #[arg(long, value_enum)]
        result: Option<VerifyResult>,
        /// Optional note recorded alongside the evidence
        #[arg(long)]
        note: Option<String>,
    },
    /// Set an integration hold
    Hold {
        change: String,
        #[arg(long)]
        reason: String,
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
    Config,
    /// Cross-harness project journal mechanics (plain Markdown stays the contract)
    Journal {
        #[command(subcommand)]
        cmd: journal::JournalCmd,
    },
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

fn role_refusal(role: ExecutionRole, command: &Cmd) -> Option<&'static str> {
    // Brief reads are open to every role, while writes are lead-only; the
    // handler must inspect --body-file, so Brief cannot live in this deny-list.
    match role {
        ExecutionRole::Lead => None,
        ExecutionRole::Reviewer => match command {
            Cmd::Integrate { .. } => Some("integrate"),
            Cmd::Close { .. } => Some("close"),
            _ => None,
        },
        ExecutionRole::Implementer => match command {
            Cmd::Review { .. } => Some("review"),
            Cmd::Resolve { .. } => Some("resolve"),
            Cmd::Hold { .. } => Some("hold"),
            Cmd::ReleaseHold { .. } => Some("release-hold"),
            Cmd::Close { .. } => Some("close"),
            Cmd::Integrate { .. } => Some("integrate"),
            _ => None,
        },
    }
}

fn main() {
    let cli = Cli::parse();
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
    if let Some(command) = role_refusal(role, &cli.cmd) {
        eprintln!("role refusal: {} may not {command}", role.as_str());
        return Ok(9);
    }

    let cwd = std::env::current_dir()?;
    let actor = match cli.actor {
        Some(a) => a,
        None => gitio::git(&cwd, &["config", "user.name"])
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".into()),
    };
    let ctx = Ctx {
        cwd,
        actor,
        harness: cli.harness,
        session: cli.session,
    };

    match cli.cmd {
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
            json,
        } => {
            commands::query(
                &ctx,
                QueryArgs {
                    status,
                    target,
                    tags: tag,
                    verdict,
                    actor,
                    harness,
                    json,
                },
            )?;
            Ok(0)
        }
        Cmd::Show { change, tag, json } => {
            commands::show_selection(&ctx, change.as_deref(), tag, json)?;
            Ok(0)
        }
        Cmd::Brief {
            change,
            body_file,
            title,
            version,
        } => commands::brief(&ctx, role, &change, body_file, title, version),
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
        Cmd::Metadata {
            change,
            blocked_by,
            remove_blocked_by,
            tag,
            remove_tag,
            assign,
        } => {
            commands::metadata(
                &ctx,
                &change,
                blocked_by,
                remove_blocked_by,
                tag,
                remove_tag,
                assign,
            )?;
            Ok(0)
        }
        Cmd::Status { change, json: _ } => {
            commands::status_cmd(&ctx, &change)?;
            Ok(0)
        }
        Cmd::BlockerStatus { change } => {
            commands::blocker_status_cmd(&ctx, &change)?;
            Ok(0)
        }
        Cmd::IsBlocked { change } => match commands::is_blocked(&ctx, &change) {
            Ok(code) => Ok(code),
            Err(error) => {
                eprintln!("error: {error:#}");
                Ok(2)
            }
        },
        Cmd::Events {
            follow,
            change,
            event_type,
        } => {
            commands::events(&ctx, follow, change.as_deref(), event_type.as_deref())?;
            Ok(0)
        }
        Cmd::Watch {
            change,
            until,
            timeout,
        } => commands::watch(&ctx, &change, until, timeout),
        Cmd::Export { change, output } => {
            commands::export_bundle(&ctx, &change, &output)?;
            Ok(0)
        }
        Cmd::Import { input, dry_run } => commands::import_bundle(&ctx, &input, dry_run),
        Cmd::Check { change, tag } => commands::check_selection(&ctx, change.as_deref(), tag),
        Cmd::Claim {
            change,
            ttl,
            stage_budget,
        } => commands::claim(&ctx, &change, ttl, stage_budget),
        Cmd::ReleaseClaim { change } => commands::release_claim(&ctx, &change),
        Cmd::Stage {
            change,
            stage,
            note,
        } => commands::stage(&ctx, &change, stage, note),
        Cmd::Snapshot { change, base } => {
            commands::snapshot(&ctx, &change, base)?;
            Ok(0)
        }
        Cmd::Comment {
            change,
            body,
            patchset,
            anchor,
        } => {
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
            commands::resolve(&ctx, &change, finding, status, commit, evidence)?;
            Ok(0)
        }
        Cmd::Review {
            change,
            verdict,
            patchset,
            findings_json,
        } => {
            commands::review(&ctx, &change, verdict, patchset, findings_json)?;
            Ok(0)
        }
        Cmd::Verify {
            change,
            all,
            gate,
            command,
            attest,
            result,
            note,
        } => commands::verify(
            &ctx,
            &change,
            commands::VerifyArgs {
                all,
                gate,
                command,
                attest,
                result,
                note,
            },
        ),
        Cmd::Hold { change, reason } => {
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
        } => commands::integrate(&ctx, change.as_deref(), tag, into, message, cleanup),
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
        Cmd::Config => {
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
        Cmd::Journal { cmd } => journal::run(&ctx, cmd),
    }
}
