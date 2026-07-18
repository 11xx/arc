//! Mechanics for the cross-harness `/thread` archive.
//!
//! The content layer stays freeform Markdown and plain files remain the
//! contract: anything written here is readable and writable by a tool-less
//! agent. `arc thread` only encodes the invariants that drift in practice —
//! archive-directory resolution, timestamped filenames, and append-only
//! journal lines. It is a convenience and correctness layer, never a
//! gatekeeper, and it is intentionally decoupled from the change ledger.
//!
//! Lanes are advisory occupancy announced through journal markers. Their
//! liveness follows the owner's latest journal activity; they are never locks.

use crate::commands::Ctx;
use crate::config;
use crate::gitio;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use clap::{Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Closed set of artifact kinds. Malformed kinds are rejected by clap at
/// parse time, before anything is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ThreadKind {
    Note,
    Plan,
    Handoff,
    Done,
    Review,
    Conclusion,
    Inbox,
    Spec,
    Todo,
    Later,
}

impl ThreadKind {
    fn as_str(self) -> &'static str {
        match self {
            ThreadKind::Note => "note",
            ThreadKind::Plan => "plan",
            ThreadKind::Handoff => "handoff",
            ThreadKind::Done => "done",
            ThreadKind::Review => "review",
            ThreadKind::Conclusion => "conclusion",
            ThreadKind::Inbox => "inbox",
            ThreadKind::Spec => "spec",
            ThreadKind::Todo => "todo",
            ThreadKind::Later => "later",
        }
    }
}

/// Primary kinds that represent work waiting for a future session: they stay
/// in the main `thread open` queue until an explicit `thread consume`.
const PRIMARY_ACTIONABLE_KINDS: [&str; 4] = ["todo", "handoff", "inbox", "plan"];
const LATER_KIND: &str = "later";

fn is_actionable_kind(kind: &str) -> bool {
    PRIMARY_ACTIONABLE_KINDS.contains(&kind) || kind == LATER_KIND
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
    },
    /// Renew a lane owned by this session
    Renew {
        topic: String,
        #[arg(long)]
        ttl: Option<String>,
        #[arg(long)]
        status: Option<String>,
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
pub enum ThreadCmd {
    /// Print the resolved archive directory (creates nothing)
    Dir {
        /// Print the cold sibling archive directory
        #[arg(long)]
        archive: bool,
    },
    /// Write a timestamped artifact and append its journal line
    Note {
        /// Kebab-case topic slug
        topic: String,
        /// Artifact kind (closed set)
        #[arg(long, value_enum)]
        kind: ThreadKind,
        /// Body source: a file path, or '-' for stdin (written verbatim)
        #[arg(long)]
        body_file: String,
        /// Optional title; when set, a `# <title>` heading is prepended
        #[arg(long)]
        title: Option<String>,
    },
    /// Append a journal-only line (no artifact file is created)
    Journal {
        /// Kebab-case topic slug
        topic: String,
        /// Free-text journal message
        message: String,
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
    /// List actionable artifacts (todo/handoff/inbox/plan, then later) not yet consumed
    Open {
        /// Restrict to one actionable kind; later is shown separately
        #[arg(long, value_enum)]
        kind: Option<ThreadKind>,
        /// Emit structured JSON instead of text
        #[arg(long)]
        json: bool,
    },
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
    },
    /// Move artifacts to the cold sibling archive without deleting history
    Archive {
        /// Artifact filename inside the hot dir (a name, not a path)
        #[arg(required_unless_present = "consumed", conflicts_with = "consumed")]
        filename: Option<String>,
        /// Archive every consumed actionable artifact, including later items
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

pub fn run(ctx: &Ctx, cmd: ThreadCmd) -> Result<i32> {
    match cmd {
        ThreadCmd::Dir { archive } => {
            let hot = resolve_dir(&ctx.cwd)?;
            println!(
                "{}",
                if archive { archive_dir(&hot) } else { hot }.display()
            );
            Ok(0)
        }
        ThreadCmd::Note {
            topic,
            kind,
            body_file,
            title,
        } => note(ctx, &topic, kind, &body_file, title.as_deref()),
        ThreadCmd::Journal { topic, message } => journal(ctx, &topic, &message),
        ThreadCmd::Catchup {
            limit,
            json,
            archived,
        } => catchup(ctx, limit.unwrap_or(20), json, archived),
        ThreadCmd::Open { kind, json } => open(ctx, kind, json),
        ThreadCmd::Lane { command } => lane(ctx, command),
        ThreadCmd::Consume {
            filename,
            outcome,
            note,
        } => consume(ctx, &filename, outcome, note.as_deref()),
        ThreadCmd::Archive {
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

/// Resolve the archive directory, override precedence: `ARC_THREAD_DIR`
/// env, then a `[threads] dirs` config entry keyed by the repository-root
/// path, then the default `<ai_home>/threads/<repo-root-slug>`.
pub fn resolve_dir(cwd: &Path) -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("ARC_THREAD_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let cfg = config::load()?;
    let root = repo_root(cwd)?;
    let key = root.to_string_lossy();
    if let Some(dir) = cfg.thread_dirs.get(key.as_ref()) {
        return config::expand_tilde(dir);
    }
    Ok(cfg.ai_home.join("threads").join(config::path_slug(&root)))
}

/// The main repository root, shared by every worktree. Keying the archive
/// off this (never a worktree path) means two worktrees of one repo always
/// resolve to the same directory.
fn repo_root(cwd: &Path) -> Result<PathBuf> {
    let common = gitio::common_dir(cwd)
        .context("not inside a Git repository (set ARC_THREAD_DIR to override)")?;
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
    kind: ThreadKind,
    body_file: &str,
    title: Option<&str>,
) -> Result<i32> {
    if !valid_topic(topic) {
        bail!("topic {topic:?} is not kebab-case-safe (use lowercase a-z, 0-9, single hyphens)");
    }
    // Read the body before touching the filesystem so a bad source path
    // leaves nothing written.
    let body = read_body_verbatim(body_file)?;

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

    // The note command takes no free-text message, so the journal line is
    // auto-derived: the title when given, otherwise "wrote <kind>". Callers
    // append richer context with `thread journal`.
    let message = match title {
        Some(t) => t.to_string(),
        None => format!("wrote {}", kind.as_str()),
    };
    append_journal(&dir, ctx, now, topic, &message, Some(&filename))?;
    println!("{}", path.display());
    Ok(0)
}

fn journal(ctx: &Ctx, topic: &str, message: &str) -> Result<i32> {
    if !valid_topic(topic) {
        bail!("topic {topic:?} is not kebab-case-safe (use lowercase a-z, 0-9, single hyphens)");
    }
    let dir = resolve_dir(&ctx.cwd)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create archive dir {}", dir.display()))?;
    let now = Utc::now();
    append_journal(&dir, ctx, now, topic, message, None)?;
    Ok(0)
}

/// Append one journal line in the archive's exact convention:
/// `- <ISO8601 UTC> <harness> <session> <topic>: <message> (<filename>)`.
/// The file is opened append-only; existing lines are never rewritten.
fn append_journal(
    dir: &Path,
    ctx: &Ctx,
    now: chrono::DateTime<Utc>,
    topic: &str,
    message: &str,
    filename: Option<&str>,
) -> Result<()> {
    use std::io::Write;
    let (harness, session) = identity(ctx);
    let ts = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut line = format!("- {ts} {harness} {session} {topic}: {message}");
    if let Some(name) = filename {
        line.push_str(&format!(" ({name})"));
    }
    line.push('\n');
    let journal_path = dir.join("journal.md");
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

struct JournalLine<'a> {
    timestamp: DateTime<Utc>,
    harness: &'a str,
    session: &'a str,
    topic: &'a str,
    message: &'a str,
}

fn parse_journal_line(line: &str) -> Option<JournalLine<'_>> {
    let (prefix, message) = line.split_once(": ")?;
    let mut fields = prefix.split_whitespace();
    if fields.next()? != "-" {
        return None;
    }
    let timestamp = DateTime::parse_from_rfc3339(fields.next()?)
        .ok()?
        .with_timezone(&Utc);
    let harness = fields.next()?;
    let session = fields.next()?;
    let topic = fields.next()?;
    if fields.next().is_some() || !valid_topic(topic) {
        return None;
    }
    Some(JournalLine {
        timestamp,
        harness,
        session,
        topic,
        message,
    })
}

#[derive(Clone, Serialize)]
struct LaneEntry {
    topic: String,
    owner_harness: String,
    owner_session: String,
    state: String,
    opened_at: String,
    last_activity: String,
    ttl_seconds: u64,
    scope: Vec<String>,
    status: Option<String>,
    #[serde(skip)]
    opened_time: DateTime<Utc>,
    #[serde(skip)]
    last_activity_time: DateTime<Utc>,
}

fn lanes_from_journal(journal: &str, now: DateTime<Utc>) -> Vec<LaneEntry> {
    struct ActiveLane {
        topic: String,
        owner_harness: String,
        owner_session: String,
        opened_time: DateTime<Utc>,
        ttl_seconds: u64,
        scope: Vec<String>,
        status: Option<String>,
    }

    let lines: Vec<_> = journal.lines().filter_map(parse_journal_line).collect();
    let mut last_activity = HashMap::new();
    for line in &lines {
        last_activity.insert(line.session.to_string(), line.timestamp);
    }
    let mut active: HashMap<String, ActiveLane> = HashMap::new();
    for line in lines {
        let Some(marker) = parse_lane_marker(line.message) else {
            continue;
        };
        match marker {
            LaneMarker::Opened { ttl, scope, status } => {
                active.retain(|_, lane| lane.owner_session != line.session);
                active.insert(
                    line.topic.to_string(),
                    ActiveLane {
                        topic: line.topic.to_string(),
                        owner_harness: line.harness.to_string(),
                        owner_session: line.session.to_string(),
                        opened_time: line.timestamp,
                        ttl_seconds: ttl,
                        scope,
                        status,
                    },
                );
            }
            LaneMarker::Renewed { ttl, status } => {
                if let Some(lane) = active.get_mut(line.topic) {
                    if let Some(ttl) = ttl {
                        lane.ttl_seconds = ttl;
                    }
                    if status.is_some() {
                        lane.status = status;
                    }
                }
            }
            LaneMarker::Closed { .. } => {
                active.remove(line.topic);
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
        bail!("thread lane requires a session identity (--session or ARC_SESSION)");
    }
    Ok(session)
}

fn lane(ctx: &Ctx, command: LaneCmd) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let now = Utc::now();
    let journal_text = read_journal(&dir)?;
    let lanes = lanes_from_journal(&journal_text, now);
    match command {
        LaneCmd::Open {
            topic,
            scope,
            ttl,
            status,
        } => {
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
        LaneCmd::Renew { topic, ttl, status } => {
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
    kind: String,
    heading: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lane: Option<ArtifactLane>,
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
    files: Vec<ArtifactEntry>,
    journal_tail: Vec<String>,
}

/// Split `<ts>-<topic>-<kind>.md` into its parts. Timestamps carry no
/// hyphen and kinds are single words, so the first and last segments are
/// unambiguous and the topic is whatever lies between.
fn parse_artifact_name(name: &str) -> Option<(String, String, String)> {
    let stem = name.strip_suffix(".md")?;
    let first = stem.find('-')?;
    let last = stem.rfind('-')?;
    if last <= first {
        return None;
    }
    let ts = &stem[..first];
    let topic = &stem[first + 1..last];
    let kind = &stem[last + 1..];
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

fn catchup(ctx: &Ctx, limit: usize, json: bool, archived: bool) -> Result<i32> {
    let hot_dir = resolve_dir(&ctx.cwd)?;
    let dir = if archived {
        archive_dir(&hot_dir)
    } else {
        hot_dir.clone()
    };
    let mut files: Vec<ArtifactEntry> = Vec::new();
    if dir.is_dir() {
        let mut names: Vec<String> = Vec::new();
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "journal.md" {
                continue;
            }
            if parse_artifact_name(&name).is_some() {
                names.push(name);
            }
        }
        // Filenames lead with a lexically sortable UTC stamp: descending
        // string order is newest-first.
        names.sort();
        names.reverse();
        for name in names.into_iter().take(limit) {
            if let Some((ts, topic, kind)) = parse_artifact_name(&name) {
                let heading = first_heading(&dir.join(&name));
                files.push(ArtifactEntry {
                    file: name,
                    timestamp: ts,
                    topic,
                    kind,
                    heading,
                    lane: None,
                });
            }
        }
    }

    let journal_tail = journal_tail(&hot_dir, limit)?;
    let now = Utc::now();
    let lanes = lanes_from_journal(&read_journal(&hot_dir)?, now);

    if json {
        let out = Catchup {
            dir: dir.display().to_string(),
            lanes,
            files,
            journal_tail,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        render_lanes(&lanes, now);
        println!("dir: {}", dir.display());
        println!("artifacts (newest first):");
        if files.is_empty() {
            println!("  (none)");
        }
        for f in &files {
            let heading = f.heading.as_deref().unwrap_or("");
            println!("  {}  {}  {}  {}", f.timestamp, f.topic, f.kind, heading);
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

/// The machine shape `consume` writes and `open` scans for:
/// `consumed <filename> [<outcome>]` with a known outcome, anywhere in a
/// journal line. Tool-less agents can retire an item by hand-writing the
/// same shape; prose that merely mentions the filename does not match.
fn consumed_markers(filename: &str) -> [String; 3] {
    [
        format!("consumed {filename} [done]"),
        format!("consumed {filename} [superseded]"),
        format!("consumed {filename} [discarded]"),
    ]
}

fn is_consumed(journal: &str, filename: &str) -> bool {
    let markers = consumed_markers(filename);
    journal.lines().any(|line| {
        // Journal lines are `- <ts> <harness> <session> <topic>: <message>`;
        // the marker must open the message field itself, so prose that quotes
        // the shape mid-sentence does not retire the item. (Timestamps carry
        // `:` but never `: `, so the first `: ` ends the topic.)
        let message = line.split_once(": ").map_or(line, |(_, message)| message);
        markers
            .iter()
            .any(|marker| message.starts_with(marker.as_str()))
    })
}

fn read_journal(dir: &Path) -> Result<String> {
    let path = dir.join("journal.md");
    if !path.is_file() {
        return Ok(String::new());
    }
    std::fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))
}

#[derive(Serialize)]
struct OpenItems {
    dir: String,
    open: Vec<ArtifactEntry>,
    later: Vec<ArtifactEntry>,
}

fn open(ctx: &Ctx, kind: Option<ThreadKind>, json: bool) -> Result<i32> {
    if let Some(kind) = kind {
        if !is_actionable_kind(kind.as_str()) {
            bail!(
                "--kind {} is not actionable; the open queue tracks {}",
                kind.as_str(),
                PRIMARY_ACTIONABLE_KINDS
                    .iter()
                    .copied()
                    .chain(std::iter::once(LATER_KIND))
                    .collect::<Vec<_>>()
                    .join("|")
            );
        }
    }
    let dir = resolve_dir(&ctx.cwd)?;
    let mut open: Vec<ArtifactEntry> = Vec::new();
    let mut later: Vec<ArtifactEntry> = Vec::new();
    let now = Utc::now();
    let journal = read_journal(&dir)?;
    let lanes = lanes_from_journal(&journal, now);
    let (_, caller_session) = identity(ctx);
    if dir.is_dir() {
        let mut open_names: Vec<String> = Vec::new();
        let mut later_names: Vec<String> = Vec::new();
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))?
        {
            let name = entry?.file_name().to_string_lossy().to_string();
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
                } else {
                    open_names.push(name);
                }
            }
        }
        open_names.sort();
        open_names.reverse();
        later_names.sort();
        later_names.reverse();
        for name in open_names {
            if let Some((ts, topic, file_kind)) = parse_artifact_name(&name) {
                let heading = first_heading(&dir.join(&name));
                open.push(ArtifactEntry {
                    file: name,
                    timestamp: ts,
                    lane: lane_for_topic(&lanes, &topic, &caller_session),
                    topic,
                    kind: file_kind,
                    heading,
                });
            }
        }
        for name in later_names {
            if let Some((ts, topic, file_kind)) = parse_artifact_name(&name) {
                let heading = first_heading(&dir.join(&name));
                later.push(ArtifactEntry {
                    file: name,
                    timestamp: ts,
                    lane: lane_for_topic(&lanes, &topic, &caller_session),
                    topic,
                    kind: file_kind,
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
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("dir: {}", dir.display());
        println!("open items (newest first):");
        if open.is_empty() {
            println!("  (none)");
        }
        for f in &open {
            let heading = f.heading.as_deref().unwrap_or("");
            println!(
                "  {}  {}  {}  {}{}",
                f.timestamp,
                f.topic,
                f.kind,
                heading,
                render_artifact_lane(f.lane.as_ref())
            );
        }
        println!("later items (newest first):");
        if later.is_empty() {
            println!("  (none)");
        }
        for f in &later {
            let heading = f.heading.as_deref().unwrap_or("");
            println!(
                "  {}  {}  {}  {}{}",
                f.timestamp,
                f.topic,
                f.kind,
                heading,
                render_artifact_lane(f.lane.as_ref())
            );
        }
    }
    Ok(0)
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

fn consume(ctx: &Ctx, filename: &str, outcome: ConsumeOutcome, note: Option<&str>) -> Result<i32> {
    if filename.contains(['/', '\\']) {
        bail!("consume takes an artifact filename inside the archive dir, not a path");
    }
    let Some((_, topic, _)) = parse_artifact_name(filename) else {
        bail!("{filename:?} is not a thread artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    let dir = resolve_dir(&ctx.cwd)?;
    if !dir.join(filename).is_file() {
        bail!("no such artifact {} in {}", filename, dir.display());
    }
    if is_consumed(&read_journal(&dir)?, filename) {
        bail!("{filename} is already consumed (see the journal)");
    }
    let mut message = format!("consumed {filename} [{}]", outcome.as_str());
    if let Some(note) = note {
        message.push_str(&format!(": {note}"));
    }
    append_journal(&dir, ctx, Utc::now(), &topic, &message, None)?;
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
        let journal = read_journal(&hot)?;
        let mut names = Vec::new();
        if hot.is_dir() {
            for entry in
                std::fs::read_dir(&hot).with_context(|| format!("cannot read {}", hot.display()))?
            {
                let name = entry?.file_name().to_string_lossy().to_string();
                let Some((timestamp, _, kind)) = parse_artifact_name(&name) else {
                    continue;
                };
                if is_actionable_kind(&kind)
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
        bail!("{filename:?} is not a thread artifact name (<timestamp>-<topic>-<kind>.md)");
    };
    let source = hot.join(filename);
    if !source.is_file() {
        bail!("no such artifact {} in {}", filename, hot.display());
    }
    if is_actionable_kind(&kind) && !is_consumed(&read_journal(hot)?, filename) {
        bail!("{filename} is actionable and must be consumed before it can be archived");
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
    let path = dir.join("journal.md");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect();
    let start = lines.len().saturating_sub(limit);
    Ok(lines[start..].to_vec())
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
            parse_artifact_name("20260717T062830Z-delegation-blocker-ux-note.md"),
            Some((
                "20260717T062830Z".to_string(),
                "delegation-blocker-ux".to_string(),
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
