use super::{ensure_append_allowed, locked_state, parse_duration, write_atomically, Ctx, FileMode};
use crate::changelog_render;
use crate::gitio;
use crate::model::{Closure, Payload};
use crate::state::{ChangeState, ChangelogEntry};
use crate::ExecutionRole;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const CHANGELOG_SCHEMA: &str = "arc-changelog/1";
const RENDER_REQUEST_SCHEMA: &str = "arc-changelog-render-request/1";
const CHANGELOG_CONFIG: &str = ".arc/changelog.toml";
const DEFAULT_CHANGELOG_TARGET: &str = "CHANGELOG.md";
const CHANGELOG_RENDERER: &str = "keep-a-changelog";
const COMMAND_RENDERER: &str = "command";
const DEFAULT_RENDERER_TIMEOUT: &str = "60s";
/// Introduces the lines a release block holds that no recorded entry
/// produced, so a later projection can tell them from its own output and
/// carry them forward unchanged.
const UNRECORDED_MARKER: &str = "<!-- unrecorded -->";

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ChangelogConfig {
    target: String,
    renderer: String,
    renderer_command: Vec<String>,
    renderer_timeout: Option<String>,
}

impl Default for ChangelogConfig {
    fn default() -> Self {
        Self {
            target: DEFAULT_CHANGELOG_TARGET.into(),
            renderer: CHANGELOG_RENDERER.into(),
            renderer_command: Vec::new(),
            renderer_timeout: None,
        }
    }
}

/// A validated `.arc/changelog.toml`: the target, the renderer's name as the
/// projection reports it, and the command when the project selected one.
struct Changelog {
    target: String,
    renderer: String,
    command: Option<CommandRenderer>,
}

struct CommandRenderer {
    argv: Vec<String>,
    timeout: Duration,
}

/// What a command renderer reads on stdin.
#[derive(Serialize)]
struct RenderRequest<'a> {
    schema: &'static str,
    operation: &'static str,
    target: &'a str,
    target_content: Option<&'a str>,
    include_provenance: bool,
    projection: &'a ChangelogProjection<'a>,
}

#[derive(Serialize)]
struct ProjectedEntry<'a> {
    change_id: &'a str,
    change: &'a str,
    category: &'a str,
    body: &'a str,
    integrated_commit: Option<&'a str>,
    integrated_at: Option<&'a chrono::DateTime<chrono::Utc>>,
    recorded: RecordedProvenance<'a>,
}

#[derive(Serialize)]
struct RecordedProvenance<'a> {
    event_id: &'a str,
    actor: &'a str,
    on_behalf_of: Option<&'a str>,
    effective_author: &'a str,
    harness: Option<&'a str>,
    session: Option<&'a str>,
    created_at: &'a chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
struct ChangelogProjection<'a> {
    schema: &'static str,
    boundary: Option<&'a str>,
    target: &'a str,
    renderer: &'a str,
    entries: Vec<ProjectedEntry<'a>>,
}

#[allow(clippy::too_many_arguments)]
pub fn changelog(
    ctx: &Ctx,
    role: ExecutionRole,
    reference: Option<&str>,
    category: Option<String>,
    body_file: Option<String>,
    no_entry_reason: Option<String>,
    json: bool,
    provenance: bool,
    since: Option<String>,
    write: bool,
    keep_unrecorded: bool,
) -> Result<i32> {
    if keep_unrecorded && !write {
        bail!("--keep-unrecorded applies only to --write");
    }
    if category.is_some() || body_file.is_some() || no_entry_reason.is_some() {
        if json || provenance || since.is_some() || write {
            bail!(
                "--json, --provenance, --since, and --write cannot be used when recording an entry"
            );
        }
        let no_entry_reason = no_entry_reason
            .as_deref()
            .map(validate_reason)
            .transpose()?;
        let entry = match no_entry_reason {
            Some(_) if category.is_some() || body_file.is_some() => {
                bail!("--none records no entry; it cannot be given --category or --body-file")
            }
            Some(_) => None,
            None => Some((
                category.context("--body-file requires --category")?,
                body_file.context("--category requires --body-file")?,
            )),
        };
        if role == ExecutionRole::Reviewer {
            eprintln!("role refusal: reviewer may not changelog (requires implementer or lead)");
            return Ok(9);
        }
        let (category, body) = match entry {
            Some((category, body_file)) => (
                validate_category(&category)?,
                super::read_body_file_verbatim(&body_file)?,
            ),
            None => (String::new(), String::new()),
        };
        let store = ctx.store()?;
        let change = crate::context::resolve_change_or_infer(&store, &ctx.cwd, reference)?;
        let (change_id, _transition, state) = locked_state(&store, &change)?;
        // Recording a second entry replaced the first with no event saying
        // it did, an identical success message both times, and no way to see
        // what was lost. The edge is what makes the replacement inspectable;
        // the projection still emits one entry, which is a granularity
        // question this deliberately leaves where it is.
        let superseded = state.changelog.as_ref().map(|entry| entry.event_id.clone());
        let recorded = if no_entry_reason.is_some() {
            "no entry".to_owned()
        } else {
            category.clone()
        };
        let payload = Payload::ChangelogRecorded {
            category,
            body,
            supersedes: superseded.clone(),
            no_entry_reason,
        };
        ensure_append_allowed(&state, &payload)?;
        let event = ctx.event(&store, &change_id, payload);
        store.append_event(&event)?;
        match superseded {
            Some(superseded) => println!("changelog: {recorded} (supersedes {superseded})"),
            None => println!("changelog: {recorded}"),
        }
        println!("event: {}", event.event_id);
        return Ok(0);
    }

    let config = load_changelog_config(ctx)?;

    if let Some(reference) = reference {
        if write {
            bail!("--write cannot be used with CHANGE");
        }
        let store = ctx.store()?;
        let (_, state) = ctx.load_state(&store, reference)?;
        if json {
            let entries = state
                .changelog_entry()
                .map(|entry| projected_state_entry(&state, entry))
                .into_iter()
                .collect();
            let projection = ChangelogProjection {
                schema: CHANGELOG_SCHEMA,
                boundary: None,
                target: &config.target,
                renderer: &config.renderer,
                entries,
            };
            println!("{}", serde_json::to_string_pretty(&projection)?);
        } else if let Some(entry) = state.changelog {
            print_entry(&state.slug, &entry, provenance);
        }
        return Ok(0);
    }

    if provenance && write {
        bail!("--provenance cannot be used with --write");
    }
    let store = ctx.store()?;
    let boundary = match since {
        Some(revision) => Some(gitio::rev_parse(&ctx.cwd, &revision)?),
        None => gitio::latest_tag(&ctx.cwd)?
            .map(|tag| gitio::rev_parse(&ctx.cwd, &tag))
            .transpose()?,
    };
    let states = if write {
        ctx.load_all_states(&store)?
    } else {
        store.readable_states()?
    };
    let mut entries = states
        .values()
        .filter_map(|state| projected_entry(&ctx.cwd, state, boundary.as_deref()))
        .collect::<Result<Vec<_>>>()?;
    entries.sort_by(|(left_event, _), (right_event, _)| right_event.cmp(left_event));
    let entries = entries
        .into_iter()
        .map(|(_, entry)| entry)
        .collect::<Vec<_>>();

    let projection = ChangelogProjection {
        schema: CHANGELOG_SCHEMA,
        boundary: boundary.as_deref(),
        target: &config.target,
        renderer: &config.renderer,
        entries,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&projection)?);
        return Ok(0);
    }
    if let Some(command) = &config.command {
        if keep_unrecorded {
            bail!("--keep-unrecorded applies only to the {CHANGELOG_RENDERER} renderer");
        }
        return render_with_command(ctx, &config, command, &projection, provenance, write);
    }

    let rendered = render_unreleased(&projection.entries, provenance);
    if !write {
        print!("{rendered}");
        return Ok(0);
    }
    match write_changelog(ctx, &config, &rendered, keep_unrecorded)? {
        WriteOutcome::Written => Ok(0),
        WriteOutcome::Unwritable(reason) => {
            eprintln!("{} {reason}; nothing was written", config.target);
            eprintln!(
                "a project whose file follows another convention can select renderer = \
                 \"{COMMAND_RENDERER}\" with a renderer_command in {CHANGELOG_CONFIG}"
            );
            Ok(1)
        }
        WriteOutcome::Unrecorded(paragraphs) => {
            eprintln!(
                "{} holds prose no recorded changelog entry produced; nothing was written:",
                config.target
            );
            for paragraph in &paragraphs {
                eprintln!("  {paragraph}");
            }
            eprintln!(
                "record each on the change that made it with arc changelog CHANGE --category \
                 CATEGORY --body-file FILE, or keep them with --keep-unrecorded"
            );
            Ok(1)
        }
    }
}

/// What one `--write` did to the target file.
enum WriteOutcome {
    Written,
    /// The target holds nothing this renderer can replace: the file is
    /// missing, or it has no `## [Unreleased]` heading. The reason completes
    /// a sentence whose subject is the target.
    Unwritable(&'static str),
    /// The block holds prose the ledger cannot account for, and replacing it
    /// would destroy the only copy. Each unaccounted paragraph is named by
    /// enough of itself to find it in the file.
    Unrecorded(Vec<String>),
}

fn projected_entry<'a>(
    cwd: &Path,
    state: &'a ChangeState,
    boundary: Option<&str>,
) -> Option<Result<(String, ProjectedEntry<'a>)>> {
    let closure = state.closure.as_ref()?;
    if closure.outcome != Closure::Integrated {
        return None;
    }
    let integrated_commit = closure.integrated_commit.as_deref()?;
    if let Some(boundary) = boundary {
        match gitio::is_ancestor(cwd, integrated_commit, boundary) {
            Ok(true) => return None,
            Ok(false) => {}
            Err(error) => return Some(Err(error)),
        }
    }
    state.changelog_entry().map(|entry| {
        Ok((
            closure.event_id.clone(),
            projected_state_entry(state, entry),
        ))
    })
}

fn projected_state_entry<'a>(
    state: &'a ChangeState,
    entry: &'a ChangelogEntry,
) -> ProjectedEntry<'a> {
    let integrated = state
        .closure
        .as_ref()
        .filter(|closure| closure.outcome == Closure::Integrated);
    ProjectedEntry {
        change_id: &state.change_id,
        change: &state.slug,
        category: &entry.category,
        body: &entry.body,
        integrated_commit: integrated.and_then(|closure| closure.integrated_commit.as_deref()),
        integrated_at: integrated.map(|closure| &closure.created_at),
        recorded: RecordedProvenance {
            event_id: &entry.event_id,
            actor: &entry.actor,
            on_behalf_of: entry.on_behalf_of.as_deref(),
            effective_author: entry.effective_author(),
            harness: entry.harness.as_deref(),
            session: entry.session.as_deref(),
            created_at: &entry.created_at,
        },
    }
}

fn render_unreleased(entries: &[ProjectedEntry<'_>], provenance: bool) -> String {
    let mut rendered = String::from("## [Unreleased]\n");
    for (comparison, heading) in CANONICAL_CATEGORIES {
        let category_entries = entries
            .iter()
            .filter(|entry| entry.category.eq_ignore_ascii_case(comparison));
        render_category(&mut rendered, heading, category_entries, provenance);
    }

    let mut custom = BTreeMap::<&str, Vec<&ProjectedEntry<'_>>>::new();
    for entry in entries {
        if canonical_category(entry.category).is_none() {
            custom.entry(entry.category).or_default().push(entry);
        }
    }
    for (heading, category_entries) in custom {
        render_category(
            &mut rendered,
            heading,
            category_entries.into_iter(),
            provenance,
        );
    }
    rendered
}

const CANONICAL_CATEGORIES: [(&str, &str); 6] = [
    ("added", "Added"),
    ("changed", "Changed"),
    ("deprecated", "Deprecated"),
    ("removed", "Removed"),
    ("fixed", "Fixed"),
    ("security", "Security"),
];
const CHANGELOG_LINE_WIDTH: usize = 75;

fn canonical_category(category: &str) -> Option<&'static str> {
    CANONICAL_CATEGORIES
        .iter()
        .find_map(|(comparison, heading)| {
            category
                .eq_ignore_ascii_case(comparison)
                .then_some(*heading)
        })
}

fn wrap_words(line: &str, width: usize) -> Vec<String> {
    let mut wrapped = Vec::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        let word_width = word.chars().count();
        let candidate_width =
            current.chars().count() + usize::from(!current.is_empty()) + word_width;
        if !current.is_empty() && candidate_width > width {
            wrapped.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        wrapped.push(current);
    }
    wrapped
}

/// The marker a line begins with: its indent, then a `-`, `*`, or `+`
/// bullet, or a number of one to nine digits closed by `.` or `)`, then a
/// space. An author who wrote their own list chose the markers, the numbers,
/// and the nesting; they did not choose the column the file wraps at, so the
/// prefix survives and the text after it is still wrapped.
fn line_marker(line: &str) -> Option<&str> {
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent..];
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let token = if ["- ", "* ", "+ "]
        .iter()
        .any(|bullet| rest.starts_with(bullet))
    {
        1
    } else if (1..=9).contains(&digits)
        && [". ", ") "]
            .iter()
            .any(|close| rest[digits..].starts_with(close))
    {
        digits + 1
    } else {
        return None;
    };
    Some(&line[..indent + token + 1])
}

/// Whether a line beginning with `marker` opens an item instead of joining
/// the paragraph or item above it, whose own marker is `above`. A bullet
/// always opens one. A number opens one only when it is 1 or the unit above
/// is a numbered item; elsewhere, as CommonMark reads it, the line is a
/// wrapped sentence that happens to begin with a number.
fn interrupts(marker: &str, above: Option<&str>) -> bool {
    match ordinal(marker) {
        None => true,
        Some(number) => number.trim_start_matches('0') == "1" || above.and_then(ordinal).is_some(),
    }
}

/// The number of a numbered marker; a bullet has none.
fn ordinal(marker: &str) -> Option<&str> {
    let token = marker.trim();
    let number = &token[..token.len() - 1];
    (!number.is_empty()).then_some(number)
}

/// Recorded bodies are free text and predate any convention about list
/// markers, so a release block would otherwise mix bulleted and bare entries.
/// Normalise at render time rather than at write time: the event keeps exactly
/// what its author recorded, and the projection decides how a release reads.
/// A body that already leads with a marker keeps the markers and nesting its
/// author chose; only the bullet arc would otherwise have added is withheld.
///
/// Each paragraph and list item is refilled to the width as one unit, so an
/// entry renders the same whatever column its author wrapped it at. A fenced
/// block keeps its lines exactly, trailing whitespace included; the only
/// change is the item's indentation in front of each non-empty line.
fn as_list_item(body: &str) -> String {
    let blocks = body_blocks(body.trim_end());
    let Some(first) = blocks.first() else {
        return String::new();
    };
    let authored = matches!(
        first,
        Block::Prose {
            marker: Some(_),
            ..
        }
    );
    // A bare body becomes one item: everything sits under the text of the
    // bullet that opens it.
    let base = if authored { "" } else { "  " };

    let mut lines = Vec::new();
    for block in &blocks {
        match block {
            Block::Blank => lines.push(String::new()),
            Block::Fenced(fenced) => lines.extend(fenced.iter().map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("{base}{line}")
                }
            })),
            Block::Prose {
                marker,
                indent,
                words,
            } => {
                let opener = match marker {
                    Some(marker) => format!("{base}{marker}"),
                    None if authored => (*indent).to_string(),
                    None => base.to_string(),
                };
                let continuation = " ".repeat(opener.chars().count());
                let width = CHANGELOG_LINE_WIDTH.saturating_sub(opener.chars().count());
                for (index, wrapped) in wrap_words(&words.join(" "), width).into_iter().enumerate()
                {
                    let prefix = if index == 0 { &opener } else { &continuation };
                    lines.push(format!("{prefix}{wrapped}"));
                }
            }
        }
    }
    if !authored {
        lines[0].replace_range(..base.len(), "- ");
    }
    lines.join("\n")
}

/// One unit of a recorded body as the changelog renders it.
enum Block<'a> {
    /// A paragraph break; runs of blank lines collapse to one.
    Blank,
    /// A fenced code block, fences included, kept line for line.
    Fenced(Vec<&'a str>),
    /// A paragraph or list item: the marker that opens it, if any, the
    /// indentation of its first line, and every word of its lines in order.
    Prose {
        marker: Option<&'a str>,
        indent: &'a str,
        words: Vec<&'a str>,
    },
}

/// An open code fence, as CommonMark delimits one: the character its run is
/// drawn with, the run's length, and the run's indentation.
struct Fence {
    mark: char,
    run: usize,
    indent: usize,
}

impl Fence {
    /// The fence `line` opens: a run of three or more backticks or tildes
    /// after its indentation. A backtick run followed by another backtick on
    /// the line is inline code and opens nothing.
    fn opened_by(line: &str) -> Option<Self> {
        let (indent, mark, run, after) = fence_run(line)?;
        (run >= 3 && !(mark == '`' && after.contains('`'))).then_some(Self { mark, run, indent })
    }

    /// Whether `line` closes this fence: a run of the same character at
    /// least as long, nothing after it but whitespace, and indented less than
    /// four columns past the opener; deeper, the line is content.
    fn closed_by(&self, line: &str) -> bool {
        fence_run(line).is_some_and(|(indent, mark, run, after)| {
            mark == self.mark
                && run >= self.run
                && after.trim().is_empty()
                && indent < self.indent + 4
        })
    }
}

/// A line's indentation, the backtick or tilde that begins its text, the
/// length of that character's run, and the text after the run.
fn fence_run(line: &str) -> Option<(usize, char, usize, &str)> {
    let text = line.trim_start();
    let mark = text
        .chars()
        .next()
        .filter(|mark| matches!(mark, '`' | '~'))?;
    let after = text.trim_start_matches(mark);
    Some((
        line.len() - text.len(),
        mark,
        text.len() - after.len(),
        after,
    ))
}

/// Split a body into paragraphs, list items, and fenced blocks. A line joins
/// the paragraph or item above it unless a blank line, a fence, or a list
/// marker of its own separates them.
fn body_blocks(body: &str) -> Vec<Block<'_>> {
    let mut blocks: Vec<Block<'_>> = Vec::new();
    let mut fence: Option<Fence> = None;
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(open) = &fence {
            if let Some(Block::Fenced(fenced)) = blocks.last_mut() {
                fenced.push(line);
            }
            if open.closed_by(line) {
                fence = None;
            }
            continue;
        }
        if trimmed.is_empty() {
            if !matches!(blocks.last(), None | Some(Block::Blank)) {
                blocks.push(Block::Blank);
            }
            continue;
        }
        if let Some(open) = Fence::opened_by(line) {
            fence = Some(open);
            blocks.push(Block::Fenced(vec![line]));
            continue;
        }
        let marker = line_marker(line).filter(|marker| match blocks.last() {
            Some(Block::Prose { marker: above, .. }) => interrupts(marker, *above),
            _ => true,
        });
        if marker.is_none() {
            if let Some(Block::Prose { words, .. }) = blocks.last_mut() {
                words.extend(trimmed.split_whitespace());
                continue;
            }
        }
        let text = marker.map_or(trimmed, |marker| &line[marker.len()..]);
        blocks.push(Block::Prose {
            marker,
            indent: &line[..line.len() - line.trim_start().len()],
            words: text.split_whitespace().collect(),
        });
    }
    blocks
}

fn render_category<'a>(
    rendered: &mut String,
    heading: &str,
    entries: impl Iterator<Item = &'a ProjectedEntry<'a>>,
    provenance: bool,
) {
    let entries = entries.collect::<Vec<_>>();
    if entries.is_empty() {
        return;
    }
    rendered.push_str("\n### ");
    rendered.push_str(heading);
    rendered.push_str("\n\n");
    for (index, entry) in entries.iter().enumerate() {
        rendered.push_str(&as_list_item(entry.body));
        rendered.push('\n');
        if provenance {
            rendered.push_str(&provenance_line(entry));
            rendered.push('\n');
        }
        if index + 1 < entries.len() {
            rendered.push('\n');
        }
    }
}

fn print_entry(change: &str, entry: &ChangelogEntry, provenance: bool) {
    match &entry.no_entry_reason {
        Some(reason) => println!("no entry: {reason}"),
        None => {
            println!("### {}\n", entry.category);
            print!("{}", entry.body);
        }
    }
    if provenance {
        if !entry.body.ends_with('\n') && entry.is_entry() {
            println!();
        }
        println!(
            "> arc provenance: change={} event={} actor={} on_behalf_of={} harness={} session={} created_at={}",
            change,
            entry.event_id,
            entry.actor,
            entry.on_behalf_of.as_deref().unwrap_or("-"),
            entry.harness.as_deref().unwrap_or("-"),
            entry.session.as_deref().unwrap_or("-"),
            entry.created_at,
        );
    }
}

fn validate_reason(reason: &str) -> Result<String> {
    let reason = reason.trim();
    if reason.is_empty() {
        bail!("--reason must say why the change needs no changelog entry");
    }
    Ok(reason.to_owned())
}

fn validate_category(category: &str) -> Result<String> {
    let category = category.trim();
    if category.is_empty() {
        bail!("changelog category must not be empty");
    }
    if category.contains(['\n', '\r']) {
        bail!("changelog category must be a single line");
    }
    Ok(category.to_owned())
}

fn provenance_line(entry: &ProjectedEntry<'_>) -> String {
    format!(
        "> arc provenance: change={} event={} actor={} on_behalf_of={} harness={} session={} created_at={}",
        entry.change,
        entry.recorded.event_id,
        entry.recorded.actor,
        entry.recorded.on_behalf_of.unwrap_or("-"),
        entry.recorded.harness.unwrap_or("-"),
        entry.recorded.session.unwrap_or("-"),
        entry.recorded.created_at,
    )
}

fn load_changelog_config(ctx: &Ctx) -> Result<Changelog> {
    let root = gitio::toplevel(&ctx.cwd)?;
    let path = root.join(CHANGELOG_CONFIG);
    let config = match fs::read_to_string(&path) {
        Ok(contents) => toml::from_str::<ChangelogConfig>(&contents)
            .with_context(|| format!("parse {}", path.display()))?,
        Err(error) if error.kind() == ErrorKind::NotFound => ChangelogConfig::default(),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let command = match config.renderer.as_str() {
        CHANGELOG_RENDERER => {
            if !config.renderer_command.is_empty() {
                bail!("{CHANGELOG_CONFIG}: renderer_command applies only to renderer `{COMMAND_RENDERER}`");
            }
            if config.renderer_timeout.is_some() {
                bail!("{CHANGELOG_CONFIG}: renderer_timeout applies only to renderer `{COMMAND_RENDERER}`");
            }
            None
        }
        COMMAND_RENDERER => {
            if config.renderer_command.first().is_none_or(String::is_empty) {
                bail!(
                    "{CHANGELOG_CONFIG}: renderer `{COMMAND_RENDERER}` requires a non-empty \
                     renderer_command, the argv of the program that renders the target"
                );
            }
            let timeout = config
                .renderer_timeout
                .as_deref()
                .unwrap_or(DEFAULT_RENDERER_TIMEOUT);
            let timeout = parse_duration(timeout)
                .with_context(|| format!("{CHANGELOG_CONFIG}: renderer_timeout"))?;
            Some(CommandRenderer {
                argv: config.renderer_command,
                timeout: Duration::from_secs(timeout),
            })
        }
        other => bail!(
            "{CHANGELOG_CONFIG}: unsupported changelog renderer `{other}`; expected \
             `{CHANGELOG_RENDERER}` or `{COMMAND_RENDERER}`"
        ),
    };
    normalize_target(&config.target)?;
    Ok(Changelog {
        target: config.target,
        renderer: config.renderer,
        command,
    })
}

/// Hand the projection to the project's renderer. A read prints its answer;
/// a write replaces the target with it. Anything short of an answer leaves
/// the target byte-identical, says why on stderr, and exits 1.
fn render_with_command(
    ctx: &Ctx,
    config: &Changelog,
    command: &CommandRenderer,
    projection: &ChangelogProjection<'_>,
    provenance: bool,
    write: bool,
) -> Result<i32> {
    let root = gitio::toplevel(&ctx.cwd)?;
    let path = target_path(&root, &config.target)?;
    let target_content = if write {
        match fs::read(&path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(content) => Some(content),
                Err(_) => {
                    eprintln!(
                        "{} is not UTF-8, so the renderer cannot be given it; nothing was written",
                        config.target
                    );
                    return Ok(1);
                }
            },
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        }
    } else {
        None
    };
    let request = RenderRequest {
        schema: RENDER_REQUEST_SCHEMA,
        operation: if write { "write" } else { "render" },
        target: &config.target,
        target_content: target_content.as_deref(),
        include_provenance: provenance,
        projection,
    };
    let request = serde_json::to_vec(&request)?;
    let outcome = changelog_render::run(&command.argv, &root, &request, command.timeout)?;
    let consequence = if write {
        format!("{} was not written", config.target)
    } else {
        "nothing was rendered".to_owned()
    };
    let rendered = match outcome {
        Ok(rendered) if write && rendered.is_empty() => {
            eprintln!(
                "changelog renderer `{}` printed nothing; {consequence}",
                command.argv.join(" ")
            );
            return Ok(1);
        }
        Ok(rendered) => rendered,
        Err(refusal) => {
            eprintln!(
                "changelog renderer `{}` {}; {consequence}",
                command.argv.join(" "),
                refusal.cause
            );
            if !refusal.stderr_tail.trim().is_empty() {
                eprintln!("renderer stderr:");
                eprint!("{}", refusal.stderr_tail);
                if !refusal.stderr_tail.ends_with('\n') {
                    eprintln!();
                }
            }
            return Ok(1);
        }
    };
    if write {
        replace_target(&path, rendered.as_bytes())?;
    } else {
        print!("{rendered}");
    }
    Ok(0)
}

/// Replace the target atomically, through a symlink to the file it names, and
/// keep the permission bits it had. A new file gets the umask's.
fn replace_target(path: &Path, contents: &[u8]) -> Result<()> {
    let (path, mode) = match fs::canonicalize(path) {
        Ok(resolved) => {
            let bits = fs::metadata(&resolved)
                .with_context(|| format!("read {}", resolved.display()))?
                .permissions()
                .mode();
            (resolved, FileMode::Keep(bits & 0o777))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            (path.to_path_buf(), FileMode::Create(0o666))
        }
        Err(error) => return Err(error).with_context(|| format!("resolve {}", path.display())),
    };
    write_atomically(&path, contents, mode)
}

fn normalize_target(target: &str) -> Result<PathBuf> {
    let path = Path::new(target);
    if path.is_absolute() {
        bail!("changelog target must stay inside the repository");
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir if normalized.pop() => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("changelog target must stay inside the repository");
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        bail!("changelog target must name a repository-relative file");
    }
    Ok(normalized)
}

fn target_path(root: &Path, target: &str) -> Result<PathBuf> {
    let path = root.join(normalize_target(target)?);
    let canonical_root =
        fs::canonicalize(root).with_context(|| format!("resolve {}", root.display()))?;
    let containment_probe = if path.exists() {
        fs::canonicalize(&path).with_context(|| format!("resolve {}", path.display()))?
    } else {
        let parent = path
            .parent()
            .context("changelog target has no parent directory")?;
        fs::canonicalize(parent).with_context(|| format!("resolve {}", parent.display()))?
    };
    if !containment_probe.starts_with(&canonical_root) {
        bail!("changelog target must stay inside the repository");
    }
    Ok(path)
}

/// One prose unit of a release block: a heading, a bullet with the lines
/// wrapped under it, or one paragraph of a body that holds several. A column
/// is a rendering choice rather than prose, so a paragraph is judged by its
/// words alone.
struct Paragraph {
    /// The lines the file holds, without trailing whitespace.
    lines: Vec<String>,
    /// The words of those lines, without the bullet marker, indentation, or
    /// the columns they happen to be wrapped at.
    prose: String,
}

impl Paragraph {
    /// Enough of the paragraph to find it in the file.
    fn summary(&self) -> String {
        match self.lines.split_first() {
            Some((first, [])) => first.clone(),
            Some((first, _)) => format!("{first} ..."),
            None => String::new(),
        }
    }
}

/// Split prose into paragraphs. A blank line ends one; a heading, or a list
/// marker that opens an item where the renderer would open one, starts one,
/// so an author's own list stays as many units as it has items. The
/// unrecorded marker carries no prose and delimits rather than joins.
fn paragraphs(text: &str) -> Vec<Paragraph> {
    let mut paragraphs = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut flush = |lines: &mut Vec<String>| {
        if !lines.is_empty() {
            let taken = std::mem::take(lines);
            paragraphs.push(Paragraph {
                prose: prose_of(&taken),
                lines: taken,
            });
        }
    };
    for line in text.lines() {
        let line = line.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed == UNRECORDED_MARKER {
            flush(&mut lines);
            continue;
        }
        let opens_item = line_marker(line).is_some_and(|marker| {
            lines
                .first()
                .is_none_or(|first| interrupts(marker, line_marker(first)))
        });
        if trimmed.starts_with('#') || opens_item {
            flush(&mut lines);
        }
        lines.push(line.to_owned());
    }
    flush(&mut lines);
    paragraphs
}

/// The words of one paragraph, each run of whitespace collapsed to a single
/// space and the leading bullet marker dropped.
fn prose_of(lines: &[String]) -> String {
    let mut prose = String::new();
    for (index, line) in lines.iter().enumerate() {
        let text = match (index, line_marker(line)) {
            (0, Some(marker)) => &line[marker.len()..],
            _ => line.as_str(),
        };
        for word in text.split_whitespace() {
            if !prose.is_empty() {
                prose.push(' ');
            }
            prose.push_str(word);
        }
    }
    prose
}

/// The paragraphs of a release block whose prose the projection does not
/// produce, in the order the file holds them.
fn unrecorded_paragraphs(block: &str, projected: &str) -> Vec<Paragraph> {
    let recorded = paragraphs(projected)
        .into_iter()
        .map(|paragraph| paragraph.prose)
        .collect::<HashSet<_>>();
    paragraphs(block)
        .into_iter()
        .filter(|paragraph| !recorded.contains(&paragraph.prose))
        .collect()
}

fn write_changelog(
    ctx: &Ctx,
    config: &Changelog,
    rendered: &str,
    keep_unrecorded: bool,
) -> Result<WriteOutcome> {
    let root = gitio::toplevel(&ctx.cwd)?;
    let path = target_path(&root, &config.target)?;
    let original = match fs::read_to_string(&path) {
        Ok(original) => original,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(WriteOutcome::Unwritable(
                "does not exist, and the keep-a-changelog renderer does not create it",
            ))
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let heading = "## [Unreleased]";
    let Some(heading_start) = original.match_indices(heading).find_map(|(offset, _)| {
        let line_start = offset == 0 || original.as_bytes()[offset - 1] == b'\n';
        let line_end = original
            .as_bytes()
            .get(offset + heading.len())
            .is_none_or(|byte| *byte == b'\n' || *byte == b'\r');
        (line_start && line_end).then_some(offset)
    }) else {
        return Ok(WriteOutcome::Unwritable(
            "holds no `## [Unreleased]` heading for the keep-a-changelog renderer to replace",
        ));
    };
    let after_heading = original[heading_start..]
        .find('\n')
        .map(|offset| heading_start + offset + 1)
        .unwrap_or(original.len());
    // The block runs to the next release heading, or to the end of a file
    // that has never been released.
    let next_release = original[after_heading..]
        .match_indices("## [")
        .find_map(|(offset, _)| {
            let absolute = after_heading + offset;
            (original.as_bytes()[absolute - 1] == b'\n').then_some(absolute)
        });
    let block_end = next_release.unwrap_or(original.len());
    let replacement = rendered
        .strip_prefix("## [Unreleased]\n")
        .expect("renderer always emits the unreleased heading");
    // The projection is authoritative only over the entries it can produce.
    // Prose the ledger never saw exists in the file and nowhere else, so the
    // write either keeps it under the marker or declines to run at all.
    let unrecorded = unrecorded_paragraphs(&original[after_heading..block_end], replacement);
    if !unrecorded.is_empty() && !keep_unrecorded {
        return Ok(WriteOutcome::Unrecorded(
            unrecorded.iter().map(Paragraph::summary).collect(),
        ));
    }
    let mut block = String::new();
    if !unrecorded.is_empty() {
        block.push('\n');
        block.push_str(UNRECORDED_MARKER);
        block.push_str("\n\n");
        for (index, paragraph) in unrecorded.iter().enumerate() {
            if index > 0 {
                block.push('\n');
            }
            for line in &paragraph.lines {
                block.push_str(line);
                block.push('\n');
            }
        }
    }
    block.push_str(replacement);
    let mut updated = String::with_capacity(original.len() + block.len());
    updated.push_str(&original[..after_heading]);
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&block);
    if let Some(next_release) = next_release {
        if !block.ends_with("\n\n") {
            updated.push('\n');
        }
        updated.push_str(&original[next_release..]);
    }
    replace_target(&path, updated.as_bytes())?;
    Ok(WriteOutcome::Written)
}

#[cfg(test)]
mod tests {
    use super::{as_list_item, paragraphs, render_category, ProjectedEntry, RecordedProvenance};

    #[test]
    fn bare_bodies_become_list_items_and_authored_markers_survive() {
        assert_eq!(as_list_item("Did a thing.\n"), "- Did a thing.");
        // An author who already formatted a list keeps their exact markers.
        assert_eq!(as_list_item("- Did a thing.\n"), "- Did a thing.");
        assert_eq!(as_list_item("* Did a thing."), "* Did a thing.");
        // A body its author wrapped is one item, refilled as one paragraph.
        assert_eq!(
            as_list_item("Did a thing,\nacross lines.\n"),
            "- Did a thing, across lines."
        );
        assert_eq!(as_list_item("   "), "");
    }

    #[test]
    fn a_body_renders_the_same_whatever_column_its_author_wrapped_it_at() {
        let words = concat!(
            "A change keeps its own branch from the integration target with the ",
            "fork's commits replayed onto it, and a recorded link naming the fork ",
            "and the source base, head, and tree. The link grants no review credit.",
        );
        let wrapped_at = |width: usize| {
            words
                .split_whitespace()
                .fold(vec![String::new()], |mut lines, word| {
                    let line = lines.last_mut().unwrap();
                    if !line.is_empty() && line.len() + 1 + word.len() > width {
                        lines.push(word.to_string());
                    } else {
                        if !line.is_empty() {
                            line.push(' ');
                        }
                        line.push_str(word);
                    }
                    lines
                })
                .join("\n")
        };
        let unwrapped = as_list_item(words);
        for width in [60, 78] {
            let body = format!("{}\n\nA second paragraph.\n", wrapped_at(width));
            assert_eq!(
                as_list_item(&body),
                format!("{unwrapped}\n\n  A second paragraph."),
                "wrapped at {width}"
            );
        }
        let lines = unwrapped.lines().collect::<Vec<_>>();
        for pair in lines.windows(2) {
            let next_word = pair[1].split_whitespace().next().unwrap();
            assert!(
                pair[0].chars().count() + 1 + next_word.chars().count() > 75,
                "{unwrapped}"
            );
        }
    }

    #[test]
    fn authored_items_refill_their_continuations_and_keep_their_nesting() {
        assert_eq!(
            as_list_item(concat!(
                "- a top-level item\n  continued on a second line\n",
                "  - a nested item\n    continued too\n- a second item",
            )),
            concat!(
                "- a top-level item continued on a second line\n",
                "  - a nested item continued too\n- a second item",
            )
        );
        assert_eq!(
            as_list_item("Did a thing:\n- first\n- second\n\n\n\nAfter the list."),
            "- Did a thing:\n  - first\n  - second\n\n  After the list."
        );
    }

    #[test]
    fn fenced_blocks_keep_their_lines() {
        assert_eq!(
            as_list_item("Run it:\n```\narc changelog --write\n  indented\n```\nthen look."),
            "- Run it:\n  ```\n  arc changelog --write\n    indented\n  ```\n  then look."
        );
    }

    #[test]
    fn a_fence_closes_only_on_its_own_character_and_length() {
        assert_eq!(
            as_list_item("Example:\n````md\n```\nline one\n  indented\n```\n````\nAfter."),
            "- Example:\n  ````md\n  ```\n  line one\n    indented\n  ```\n  ````\n  After."
        );
        assert_eq!(
            as_list_item("~~~\n```\n~~\n~~~~ \nthen prose"),
            "- ~~~\n  ```\n  ~~\n  ~~~~ \n  then prose"
        );
        // Text after the run, or an indent four columns past the opener,
        // makes the line content rather than a closing fence.
        assert_eq!(
            as_list_item("```\n``` not a close\n    ```\n```\nthen prose"),
            "- ```\n  ``` not a close\n      ```\n  ```\n  then prose"
        );
        // A backtick run followed by another backtick is inline code.
        assert_eq!(
            as_list_item("``` x ``` opens nothing\nso this joins it"),
            "- ``` x ``` opens nothing so this joins it"
        );
    }

    #[test]
    fn fenced_lines_keep_their_trailing_whitespace() {
        assert_eq!(
            as_list_item("Diff:\n```\nkeep  \n\n   \nthis\t\n```"),
            "- Diff:\n  ```\n  keep  \n\n     \n  this\t\n  ```"
        );
    }

    #[test]
    fn ordered_items_refill_one_per_number() {
        assert_eq!(
            as_list_item("Steps:\n1. First\n   continued\n2. Second"),
            "- Steps:\n  1. First continued\n  2. Second"
        );
        // An authored ordered list keeps its numbers and gains no bullet.
        assert_eq!(
            as_list_item("1. First\n2. Second\n\n3) Third\n10) Tenth"),
            "1. First\n2. Second\n\n3) Third\n10) Tenth"
        );
        let rendered = as_list_item(concat!(
            "10. An ordered item whose text is long enough that the renderer ",
            "has to wrap it somewhere."
        ));
        assert_eq!(
            rendered,
            concat!(
                "10. An ordered item whose text is long enough that the renderer has to wrap\n",
                "    it somewhere."
            )
        );
        assert!(rendered.lines().all(|line| line.chars().count() <= 75));
    }

    #[test]
    fn a_wrapped_line_that_begins_with_a_number_stays_in_its_paragraph() {
        assert_eq!(
            as_list_item("Shipped in\n2024. Then more.\nAnd 1.5 too."),
            "- Shipped in 2024. Then more. And 1.5 too."
        );
        assert_eq!(
            as_list_item("- an item\n  14. still the item"),
            "- an item 14. still the item"
        );
    }

    #[test]
    fn paragraphs_split_where_the_renderer_opens_items() {
        let prose = |text: &str| {
            paragraphs(text)
                .into_iter()
                .map(|paragraph| paragraph.prose)
                .collect::<Vec<_>>()
        };
        assert_eq!(prose("1. First\n2. Second"), ["First", "Second"]);
        assert_eq!(
            prose("- Steps:\n  1. First\n  2. Second"),
            ["Steps:", "First", "Second"]
        );
        assert_eq!(
            prose("- Shipped in\n  2024. Then more."),
            ["Shipped in 2024. Then more."]
        );
    }

    #[test]
    fn authored_markers_keep_their_nesting_and_still_wrap() {
        let rendered = as_list_item(
            "- A top-level item whose text is long enough that the renderer has to wrap it somewhere.\n  - A nested item, also long enough that it cannot fit on one line of the file.",
        );
        assert_eq!(
            rendered,
            "- A top-level item whose text is long enough that the renderer has to wrap\n  it somewhere.\n  - A nested item, also long enough that it cannot fit on one line of the\n    file."
        );
        assert!(rendered.lines().all(|line| line.chars().count() <= 75));
    }

    #[test]
    fn long_bare_bodies_wrap_with_two_space_continuations() {
        let rendered = as_list_item(
            "This release entry contains enough words to prove that the renderer wraps a long line at the configured width.",
        );
        assert_eq!(
            rendered,
            "- This release entry contains enough words to prove that the renderer wraps\n  a long line at the configured width."
        );
        assert_eq!(rendered.lines().next().unwrap().chars().count(), 75);
        assert!(rendered.lines().nth(1).unwrap().starts_with("  "));
    }

    #[test]
    fn overlong_tokens_are_not_split() {
        let token = format!("https://example.com/{}", "x".repeat(70));
        let rendered = as_list_item(&format!("See {token} now."));
        assert_eq!(rendered, format!("- See\n  {token}\n  now."));
        assert!(rendered.lines().any(|line| line.chars().count() > 75));
    }

    #[test]
    fn blank_lines_remain_paragraph_breaks_within_an_item() {
        assert_eq!(
            as_list_item("First paragraph.\n\nSecond paragraph."),
            "- First paragraph.\n\n  Second paragraph."
        );
    }

    #[test]
    fn category_entries_are_separated_by_blank_lines() {
        let created_at = chrono::Utc::now();
        let entries = [
            ProjectedEntry {
                change_id: "first",
                change: "first",
                category: "added",
                body: "first entry",
                integrated_commit: None,
                integrated_at: None,
                recorded: RecordedProvenance {
                    event_id: "event-first",
                    actor: "actor",
                    on_behalf_of: None,
                    effective_author: "actor",
                    harness: None,
                    session: None,
                    created_at: &created_at,
                },
            },
            ProjectedEntry {
                change_id: "second",
                change: "second",
                category: "added",
                body: "second entry",
                integrated_commit: None,
                integrated_at: None,
                recorded: RecordedProvenance {
                    event_id: "event-second",
                    actor: "actor",
                    on_behalf_of: None,
                    effective_author: "actor",
                    harness: None,
                    session: None,
                    created_at: &created_at,
                },
            },
        ];
        let mut rendered = String::new();
        render_category(&mut rendered, "Added", entries.iter(), false);
        assert_eq!(rendered, "\n### Added\n\n- first entry\n\n- second entry\n");
    }
}
