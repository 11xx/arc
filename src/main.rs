mod commands;
mod gates;
mod gitio;
mod ids;
mod model;
mod render;
mod state;
mod status;
mod store;

use anyhow::Result;
use clap::{Parser, Subcommand};
use commands::{AnchorArgs, Ctx};
use model::{DispositionStatus, Severity, Side, Verdict};

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
    #[command(subcommand)]
    cmd: Cmd,
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
    },
    /// List changes
    List {
        #[arg(long)]
        open: bool,
        #[arg(long)]
        json: bool,
    },
    /// Render one change (Markdown, or full state with --json)
    Show {
        change: String,
        #[arg(long)]
        json: bool,
    },
    /// Machine-readable status report (the versioned arc-status/1 schema)
    Status { change: String },
    /// Integration preflight; exit code identifies the first blocker
    Check { change: String },
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
        /// Gate name from .arc/gates.toml
        #[arg(long)]
        gate: Option<String>,
        /// Ad hoc command (recorded, but not a declared gate)
        #[arg(long)]
        command: Option<String>,
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
    /// Guarded --no-ff merge of the approved head into the target branch
    Integrate {
        change: String,
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
            )?;
            Ok(0)
        }
        Cmd::List { open, json } => {
            commands::list(&ctx, open, json)?;
            Ok(0)
        }
        Cmd::Show { change, json } => {
            commands::show(&ctx, &change, json)?;
            Ok(0)
        }
        Cmd::Status { change } => {
            commands::status_cmd(&ctx, &change)?;
            Ok(0)
        }
        Cmd::Check { change } => commands::check(&ctx, &change),
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
            gate,
            command,
        } => commands::verify(&ctx, &change, gate, command),
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
            into,
            message,
            cleanup,
        } => commands::integrate(&ctx, &change, into, message, cleanup),
        Cmd::Close {
            change,
            integrated,
            abandoned,
            superseded,
        } => {
            commands::close(&ctx, &change, integrated, abandoned, superseded)?;
            Ok(0)
        }
    }
}
