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
use std::path::{Path, PathBuf};

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
        /// Artifact kind (closed set)
        #[arg(long, value_enum)]
        kind: JournalKind,
        /// Body source: a file path, or '-' for stdin (written verbatim)
        #[arg(long, required_unless_present = "scaffold")]
        body_file: Option<String>,
        /// Optional title; when set, a `# <title>` heading is prepended
        #[arg(long)]
        title: Option<String>,
        /// Scaffold template prepended to the body (records the template alone with no --body-file)
        #[arg(long)]
        scaffold: Option<String>,
    },
    /// Append a log-only journal line (no artifact file is created)
    Log {
        /// Kebab-case topic slug
        topic: String,
        /// Free-text journal message
        message: String,
    },
    /// Append a position block to an artifact and emit a typed `position` event
    Append {
        /// Artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// Position or item this answers: a position ID, legacy timestamp, or item slug
        #[arg(long = "ref")]
        reference: Option<String>,
        /// Body source: a file path, or '-' for stdin (the position argument,
        /// written verbatim below a tool-computed `### Position` heading)
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
        #[arg(long, value_enum)]
        kind: Option<JournalKind>,
        /// Emit structured JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// List live artifacts (hot journal dir) newest first, optionally filtered by kind (read-only)
    List {
        /// Restrict to one kind
        #[arg(long, value_enum)]
        kind: Option<JournalKind>,
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
    /// age, and resolution with a resolver-participation flag (read-only)
    Discussion {
        /// Discussion artifact filename inside the journal dir (a name, not a path)
        filename: String,
        /// Emit structured JSON instead of text
        #[arg(long)]
        json: bool,
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
        JournalCmd::Dir { archive } => {
            let hot = resolve_dir(&ctx.cwd)?;
            println!(
                "{}",
                if archive { archive_dir(&hot) } else { hot }.display()
            );
            Ok(0)
        }
        JournalCmd::Doctor { json } => doctor(ctx, json),
        JournalCmd::Note {
            topic,
            kind,
            body_file,
            title,
            scaffold,
        } => note(
            ctx,
            &topic,
            kind,
            body_file.as_deref(),
            title.as_deref(),
            scaffold.as_deref(),
        ),
        JournalCmd::Log { topic, message } => log_line(ctx, &topic, &message),
        JournalCmd::Append {
            filename,
            reference,
            body_file,
        } => append(ctx, &filename, reference.as_deref(), &body_file),
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

fn doctor(ctx: &Ctx, json: bool) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let cold = archive_dir(&dir);
    let mut problems = Vec::new();
    let mut advice = Vec::new();

    let jsonl = dir.join("events.jsonl");
    if jsonl.is_file() {
        let text = std::fs::read_to_string(&jsonl)
            .with_context(|| format!("cannot read {}", jsonl.display()))?;
        for (index, line) in text.lines().enumerate() {
            match serde_json::from_str::<JournalEvent>(line) {
                Ok(event) if event.known() => {}
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
            if name == "events.jsonl" {
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

/// Resolve the journal directory, override precedence: `ARC_JOURNAL_DIR`
/// env, then a `[journals] dirs` config entry keyed by the repository-root
/// path, then the default `<ai_home>/journals/<repo-root-slug>`.
pub fn resolve_dir(cwd: &Path) -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("ARC_JOURNAL_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let cfg = config::load()?;
    let root = repo_root(cwd)?;
    let key = root.to_string_lossy();
    if let Some(dir) = cfg.journal_dirs.get(key.as_ref()) {
        return config::expand_tilde(dir);
    }
    Ok(cfg.ai_home.join("journals").join(config::path_slug(&root)))
}

/// The main repository root, shared by every worktree. Keying the archive
/// off this (never a worktree path) means two worktrees of one repo always
/// resolve to the same directory.
fn repo_root(cwd: &Path) -> Result<PathBuf> {
    let common = gitio::common_dir(cwd)
        .context("not inside a Git repository (set ARC_JOURNAL_DIR to override)")?;
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

fn note(
    ctx: &Ctx,
    topic: &str,
    kind: JournalKind,
    body_file: Option<&str>,
    title: Option<&str>,
    scaffold: Option<&str>,
) -> Result<i32> {
    if !valid_topic(topic) {
        bail!("topic {topic:?} is not kebab-case-safe (use lowercase a-z, 0-9, single hyphens)");
    }
    // Read the body before touching the filesystem so a bad source path or
    // scaffold name leaves nothing written. A scaffold template is prepended
    // to the body; --scaffold with no --body-file records the template alone.
    let template = match scaffold {
        Some(name) => crate::commands::scaffold::resolve(ctx, name)?,
        None => String::new(),
    };
    let content = match body_file {
        Some(source) => read_body_verbatim(source)?,
        None => String::new(),
    };
    let body = crate::commands::scaffold::prepended(&template, &content);

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
    append_event(&dir, &event)?;
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
    append_event(&dir, &event)?;
    Ok(0)
}

/// Append a position to a live artifact and emit a typed `position` event.
/// The Markdown block and the event are the two halves of the design: the
/// block is for people and structural stance parsing; the event supplies the
/// stable position ID, activity time, identity, and reply edge. Advisory and
/// fail-open like every journal write — the block is appended even if the
/// identity is only partially known, and the file stays hand-writable.
fn append(ctx: &Ctx, filename: &str, reference: Option<&str>, body_file: &str) -> Result<i32> {
    if filename.contains(['/', '\\']) {
        bail!("journal append takes an artifact filename inside the journal dir, not a path");
    }
    let Some((_, topic, _)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a journal artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    // Read the body before touching the filesystem so a bad source path leaves
    // the artifact untouched.
    let body = read_body_verbatim(body_file)?;

    // Positions ride an open discussion in the hot directory; a cold archived
    // artifact is a closed record, not an append target.
    let dir = resolve_dir(&ctx.cwd)?;
    let path = dir.join(filename);
    if !path.is_file() {
        bail!("no such artifact {} in {}", filename, dir.display());
    }
    if is_consumed(&read_events(&dir)?, filename) {
        bail!("cannot append to consumed artifact {filename}; open a successor discussion");
    }

    let now = Utc::now();
    let ts = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let position_id = format!("pos-{}", ulid::Ulid::new().to_string().to_ascii_lowercase());
    let (harness, _) = identity(ctx);
    // The heading is tool-computed so the position timestamp is never authored
    // by hand. The model, when known, is the primary attribution — the whole
    // reason positions carry `### Position <id> (<model> via <harness>, <ts>)`.
    let heading = match ctx.model.as_deref().filter(|value| !value.is_empty()) {
        Some(model) => format!("### Position {position_id} ({model} via {harness}, {ts})"),
        None => format!("### Position {position_id} ({harness}, {ts})"),
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
    append_event(&dir, &event)?;
    println!("{}", path.display());
    Ok(0)
}

fn events(ctx: &Ctx, limit: Option<usize>) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let jsonl_path = dir.join("events.jsonl");
    let mut events = Vec::new();
    if jsonl_path.is_file() {
        let text = std::fs::read_to_string(&jsonl_path)
            .with_context(|| format!("cannot read {}", jsonl_path.display()))?;
        events.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<JournalEvent>(line).ok())
                .filter(JournalEvent::known),
        );
    }
    events.sort_by_key(JournalEvent::timestamp);
    for event in events.into_iter().take(limit.unwrap_or(usize::MAX)) {
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
            "position" => self.file.is_some(),
            "consumed" => {
                self.file.is_some()
                    && self
                        .outcome
                        .as_deref()
                        .is_some_and(|value| ["done", "superseded", "discarded"].contains(&value))
            }
            "archived" => self.file.is_some(),
            "lane-opened" => self.ttl_seconds.is_some() && self.scope.is_some(),
            "lane-renewed" => true,
            "lane-closed" => self
                .outcome
                .as_deref()
                .is_some_and(|value| ["done", "handoff", "abandoned", "expired"].contains(&value)),
            _ => false,
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
    append_event(dir, &event)
}

fn append_event(dir: &Path, event: &JournalEvent) -> Result<()> {
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
            event.timestamp().is_some().then_some(event)
        }));
    }
    events.sort_by_key(JournalEvent::timestamp);
    Ok(events)
}

fn event_message(event: &JournalEvent) -> String {
    match event.event.as_str() {
        "log" => event.message.clone().unwrap_or_default(),
        "note" => event
            .title
            .clone()
            .unwrap_or_else(|| "wrote artifact".into()),
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
struct OpenItems {
    dir: String,
    open: Vec<ArtifactEntry>,
    later: Vec<ArtifactEntry>,
    feature_requests: Vec<ArtifactEntry>,
}

fn open(ctx: &Ctx, kind: Option<JournalKind>, json: bool) -> Result<i32> {
    if let Some(kind) = kind {
        if !is_actionable_kind(kind.as_str()) {
            bail!(
                "--kind {} is not actionable; the open queue tracks {}",
                kind.as_str(),
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
    let dir = resolve_dir(&ctx.cwd)?;
    let mut open: Vec<ArtifactEntry> = Vec::new();
    let mut later: Vec<ArtifactEntry> = Vec::new();
    let mut feature_requests: Vec<ArtifactEntry> = Vec::new();
    let now = Utc::now();
    let journal = read_events(&dir)?;
    let lanes = lanes_from_journal(&journal, now);
    let changes = open_changes_for_annotation(&ctx.cwd);
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
                Some(kind) => file_kind == kind.as_str(),
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

    if json {
        let out = OpenItems {
            dir: dir.display().to_string(),
            open,
            later,
            feature_requests,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("dir: {}", dir.display());
        println!("open items (newest first):");
        if open.is_empty() {
            println!("  (none)");
        }
        for f in &open {
            render_open_entry(f);
        }
        println!("later items (newest first):");
        if later.is_empty() {
            println!("  (none)");
        }
        for f in &later {
            render_open_entry(f);
        }
        println!("feature requests (newest first):");
        if feature_requests.is_empty() {
            println!("  (none)");
        }
        for f in &feature_requests {
            render_open_entry(f);
        }
    }
    Ok(0)
}

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
fn list(ctx: &Ctx, kind: Option<JournalKind>, json: bool) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let events = read_events(&dir)?;
    let mut artifacts: Vec<ListEntry> = Vec::new();
    for name in sorted_artifact_names(&dir)? {
        let Some((ts, topic, file_kind)) = parse_artifact_name(&name) else {
            continue;
        };
        if let Some(kind) = kind {
            if file_kind != kind.as_str() {
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

#[derive(Serialize)]
struct DiscussionSummary {
    file: String,
    topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    age_seconds: Option<u64>,
    /// `### Position` headings in the file — every position, however added.
    positions: usize,
    stances: StanceTally,
    /// Distinct `<model via harness>` identities from typed `position` events.
    /// Hand-written positions that never ran `journal append` are not counted
    /// here (they still count toward `positions` and `stances`).
    participants: Vec<String>,
    /// Typed `position` events that named a `--ref`.
    reply_refs: usize,
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
            return 1;
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

fn is_position_heading(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix("### Position") else {
        return false;
    };
    rest.is_empty() || rest.starts_with(char::is_whitespace)
}

/// Count actual position blocks and at most one stated stance within each.
/// A `Position:` example or explanation elsewhere in the document is prose,
/// not a vote; a second stance-looking line inside one block is prose too.
fn position_structure(body: &str) -> (usize, StanceTally) {
    let mut positions = 0;
    let mut tally = StanceTally::default();
    let mut in_position = false;
    let mut saw_stance = false;
    for line in body.lines() {
        if is_position_heading(line) {
            positions += 1;
            in_position = true;
            saw_stance = false;
            continue;
        }
        if line.trim_start().starts_with('#') {
            in_position = false;
        }
        if !in_position || saw_stance {
            continue;
        }
        let Some(rest) = line.trim().strip_prefix("Position:") else {
            continue;
        };
        saw_stance = true;
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
            None => {}
        }
    }
    (positions, tally)
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

    let (positions, stances) = position_structure(&body);

    // Typed position events for this file, in ledger order.
    let position_events: Vec<&JournalEvent> = events
        .iter()
        .filter(|event| event.event == "position" && event.file.as_deref() == Some(filename))
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

    // Resolution: the newest consumed event for this file, if any. The resolver
    // participated when a position event shares its harness-native session.
    let resolution = events
        .iter()
        .rev()
        .find(|event| event.event == "consumed" && event.file.as_deref() == Some(filename))
        .map(|event| Resolution {
            outcome: event.outcome.clone().unwrap_or_default(),
            resolver: event_identity_label(event),
            decision: event.decision.clone(),
            resolver_participated: position_events.iter().any(|position| {
                position.harness == event.harness && position.session == event.session
            }),
        });

    let summary = DiscussionSummary {
        age_seconds: discussion_age_seconds(Utc::now(), &ts, filename, &events),
        file: filename.to_string(),
        topic,
        positions,
        stances,
        participants,
        reply_refs,
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
    let participants = if summary.participants.is_empty() {
        "(none via journal append)".to_string()
    } else {
        summary.participants.join(", ")
    };
    println!(
        "participants: {participants} ({} reply-ref{})",
        summary.reply_refs,
        if summary.reply_refs == 1 { "" } else { "s" }
    );
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
    append_event(&dir, &event)?;
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
