//! Mechanics for the cross-harness `/thread` archive.
//!
//! The content layer stays freeform Markdown and plain files remain the
//! contract: anything written here is readable and writable by a tool-less
//! agent. `arc thread` only encodes the invariants that drift in practice —
//! archive-directory resolution, timestamped filenames, and append-only
//! journal lines. It is a convenience and correctness layer, never a
//! gatekeeper, and it is intentionally decoupled from the change ledger.

use crate::commands::Ctx;
use crate::config;
use crate::gitio;
use anyhow::{bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use clap::{Subcommand, ValueEnum};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Closed set of artifact kinds. Malformed kinds are rejected by clap at
/// parse time, before anything is written.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ThreadKind {
    Note,
    Plan,
    Handoff,
    Done,
    Review,
    Conclusion,
    Inbox,
    Spec,
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
        }
    }
}

#[derive(Subcommand)]
pub enum ThreadCmd {
    /// Print the resolved archive directory (creates nothing)
    Dir,
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
    },
}

pub fn run(ctx: &Ctx, cmd: ThreadCmd) -> Result<i32> {
    match cmd {
        ThreadCmd::Dir => {
            println!("{}", resolve_dir(&ctx.cwd)?.display());
            Ok(0)
        }
        ThreadCmd::Note {
            topic,
            kind,
            body_file,
            title,
        } => note(ctx, &topic, kind, &body_file, title.as_deref()),
        ThreadCmd::Journal { topic, message } => journal(ctx, &topic, &message),
        ThreadCmd::Catchup { limit, json } => catchup(ctx, limit.unwrap_or(20), json),
    }
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
/// off this (never a worktree path) is exactly the drift fix: two worktrees
/// of one repo resolve to the same directory.
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
    std::fs::write(&path, contents).with_context(|| format!("cannot write {}", path.display()))?;

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

#[derive(Serialize)]
struct ArtifactEntry {
    file: String,
    timestamp: String,
    topic: String,
    kind: String,
    heading: Option<String>,
}

#[derive(Serialize)]
struct Catchup {
    dir: String,
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

fn catchup(ctx: &Ctx, limit: usize, json: bool) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
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
                });
            }
        }
    }

    let journal_tail = journal_tail(&dir, limit)?;

    if json {
        let out = Catchup {
            dir: dir.display().to_string(),
            files,
            journal_tail,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
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
}
