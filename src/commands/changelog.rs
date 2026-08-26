use super::{ensure_append_allowed, locked_state, Ctx};
use crate::gitio;
use crate::model::{Closure, Payload};
use crate::state::{ChangeState, ChangelogEntry};
use crate::ExecutionRole;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

const CHANGELOG_SCHEMA: &str = "arc-changelog/1";
const DEFAULT_CHANGELOG_TARGET: &str = "CHANGELOG.md";
const CHANGELOG_RENDERER: &str = "keep-a-changelog";

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ChangelogConfig {
    target: String,
    renderer: String,
}

impl Default for ChangelogConfig {
    fn default() -> Self {
        Self {
            target: DEFAULT_CHANGELOG_TARGET.into(),
            renderer: CHANGELOG_RENDERER.into(),
        }
    }
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
    json: bool,
    provenance: bool,
    since: Option<String>,
    write: bool,
) -> Result<i32> {
    if category.is_some() || body_file.is_some() {
        let reference = reference.context("recording a changelog entry requires CHANGE")?;
        let category = category.context("--body-file requires --category")?;
        let body_file = body_file.context("--category requires --body-file")?;
        if json || provenance || since.is_some() || write {
            bail!(
                "--json, --provenance, --since, and --write cannot be used when recording an entry"
            );
        }
        if role == ExecutionRole::Reviewer {
            eprintln!("role refusal: reviewer may not changelog (requires implementer or lead)");
            return Ok(9);
        }
        let body = super::read_body_file_verbatim(&body_file)?;
        let category = validate_category(&category)?;
        let store = ctx.store()?;
        let (change_id, _transition, state) = locked_state(&store, reference)?;
        let payload = Payload::ChangelogRecorded {
            category: category.clone(),
            body,
        };
        ensure_append_allowed(&state, &payload)?;
        let event = ctx.event(&store, &change_id, payload);
        store.append_event(&event)?;
        println!("changelog: {category}");
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
                .changelog
                .as_ref()
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
    let states = ctx.load_all_states(&store)?;
    let mut entries = states
        .values()
        .filter_map(|state| projected_entry(&ctx.cwd, state, boundary.as_deref()))
        .collect::<Result<Vec<_>>>()?;
    entries.sort_by(|(left_event, _), (right_event, _)| right_event.cmp(left_event));
    let entries = entries
        .into_iter()
        .map(|(_, entry)| entry)
        .collect::<Vec<_>>();

    if json {
        let projection = ChangelogProjection {
            schema: CHANGELOG_SCHEMA,
            boundary: boundary.as_deref(),
            target: &config.target,
            renderer: &config.renderer,
            entries,
        };
        println!("{}", serde_json::to_string_pretty(&projection)?);
        return Ok(0);
    }

    let rendered = render_unreleased(&entries, provenance);
    if write {
        if !write_changelog(ctx, &config, &rendered)? {
            print!("{rendered}");
        }
    } else {
        print!("{rendered}");
    }
    Ok(0)
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
    state.changelog.as_ref().map(|entry| {
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

/// The marker a line already carries: its indent plus a `-`, `*`, or `+`
/// bullet. An author who wrote their own list chose the markers and the
/// nesting; they did not choose the column the file wraps at, so the prefix
/// survives and the text after it is still wrapped.
fn line_marker(line: &str) -> Option<&str> {
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent..];
    ["- ", "* ", "+ "]
        .iter()
        .any(|marker| rest.starts_with(marker))
        .then(|| &line[..indent + 2])
}

/// Recorded bodies are free text and predate any convention about list
/// markers, so a release block would otherwise mix bulleted and bare entries.
/// Normalise at render time rather than at write time: the event keeps exactly
/// what its author recorded, and the projection decides how a release reads.
/// A body that already leads with a marker keeps the markers and nesting its
/// author chose; only the bullet arc would otherwise have added is withheld.
fn as_list_item(body: &str) -> String {
    let body = body.trim_end();
    let Some(first) = body.lines().next() else {
        return String::new();
    };
    if line_marker(first).is_some() {
        return wrap_authored_list(body);
    }

    let content_width = CHANGELOG_LINE_WIDTH - 2;
    let mut out = String::new();
    let mut first_line = true;
    for line in body.lines() {
        if line.trim().is_empty() {
            out.push('\n');
            continue;
        }
        for wrapped in wrap_words(line.trim(), content_width) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(if first_line { "- " } else { "  " });
            out.push_str(&wrapped);
            first_line = false;
        }
    }
    out
}

/// Wrap a body whose author already formatted it as a list, keeping each
/// line's own marker and indent and aligning continuations under the text
/// the marker introduces.
fn wrap_authored_list(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        if !out.is_empty() {
            out.push('\n');
        }
        if line.trim().is_empty() {
            continue;
        }
        let (prefix, text) = match line_marker(line) {
            Some(marker) => (marker.to_string(), line[marker.len()..].trim()),
            None => {
                let indent = line.len() - line.trim_start().len();
                (" ".repeat(indent), line.trim())
            }
        };
        let continuation = " ".repeat(prefix.chars().count());
        let width = CHANGELOG_LINE_WIDTH.saturating_sub(prefix.chars().count());
        for (index, wrapped) in wrap_words(text, width).into_iter().enumerate() {
            if index > 0 {
                out.push('\n');
                out.push_str(&continuation);
            } else {
                out.push_str(&prefix);
            }
            out.push_str(&wrapped);
        }
    }
    out
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
    println!("### {}\n", entry.category);
    print!("{}", entry.body);
    if provenance {
        if !entry.body.ends_with('\n') {
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

fn load_changelog_config(ctx: &Ctx) -> Result<ChangelogConfig> {
    let root = gitio::toplevel(&ctx.cwd)?;
    let path = root.join(".arc/changelog.toml");
    let config = match fs::read_to_string(&path) {
        Ok(contents) => toml::from_str::<ChangelogConfig>(&contents)
            .with_context(|| format!("parse {}", path.display()))?,
        Err(error) if error.kind() == ErrorKind::NotFound => ChangelogConfig::default(),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    if config.renderer != CHANGELOG_RENDERER {
        bail!(
            "unsupported changelog renderer `{}`; only `{CHANGELOG_RENDERER}` is available",
            config.renderer
        );
    }
    normalize_target(&config.target)?;
    Ok(config)
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

fn write_changelog(ctx: &Ctx, config: &ChangelogConfig, rendered: &str) -> Result<bool> {
    let root = gitio::toplevel(&ctx.cwd)?;
    let path = target_path(&root, &config.target)?;
    let original = match fs::read_to_string(&path) {
        Ok(original) => original,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
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
        return Ok(false);
    };
    let after_heading = original[heading_start..]
        .find('\n')
        .map(|offset| heading_start + offset + 1)
        .unwrap_or(original.len());
    let Some(next_release) =
        original[after_heading..]
            .match_indices("## [")
            .find_map(|(offset, _)| {
                let absolute = after_heading + offset;
                (absolute == 0 || original.as_bytes()[absolute - 1] == b'\n').then_some(absolute)
            })
    else {
        return Ok(false);
    };
    let replacement = rendered
        .strip_prefix("## [Unreleased]\n")
        .expect("renderer always emits the unreleased heading");
    let mut updated = String::with_capacity(original.len() + replacement.len());
    updated.push_str(&original[..after_heading]);
    updated.push_str(replacement);
    if !replacement.ends_with("\n\n") {
        updated.push('\n');
    }
    updated.push_str(&original[next_release..]);
    fs::write(&path, updated).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{as_list_item, render_category, ProjectedEntry, RecordedProvenance};

    #[test]
    fn bare_bodies_become_list_items_and_authored_markers_survive() {
        assert_eq!(as_list_item("Did a thing.\n"), "- Did a thing.");
        // An author who already formatted a list keeps their exact markers.
        assert_eq!(as_list_item("- Did a thing.\n"), "- Did a thing.");
        assert_eq!(as_list_item("* Did a thing."), "* Did a thing.");
        // An explicitly multi-line body stays one item with indented continuations.
        assert_eq!(
            as_list_item("Did a thing,\nacross lines.\n"),
            "- Did a thing,\n  across lines."
        );
        assert_eq!(as_list_item("   "), "");
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
