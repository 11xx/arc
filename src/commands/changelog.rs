use super::{ensure_append_allowed, locked_state, Ctx};
use crate::gitio;
use crate::model::{ChangelogSection, Closure, Payload};
use crate::state::{ChangeState, ChangelogEntry};
use crate::ExecutionRole;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::fs;
use std::path::Path;

const CHANGELOG_SCHEMA: &str = "arc-changelog/1";
const CHANGELOG_TARGET: &str = "CHANGELOG.md";
const CHANGELOG_RENDERER: &str = "keep-a-changelog";

#[derive(Serialize)]
struct ProjectedEntry<'a> {
    change_id: &'a str,
    change: &'a str,
    category: &'static str,
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
    target: &'static str,
    renderer: &'static str,
    entries: Vec<ProjectedEntry<'a>>,
}

#[allow(clippy::too_many_arguments)]
pub fn changelog(
    ctx: &Ctx,
    role: ExecutionRole,
    reference: Option<&str>,
    section: Option<ChangelogSection>,
    body_file: Option<String>,
    json: bool,
    provenance: bool,
    since: Option<String>,
    write: bool,
) -> Result<i32> {
    if section.is_some() || body_file.is_some() {
        let reference = reference.context("recording a changelog entry requires CHANGE")?;
        let section = section.context("--body-file requires --section")?;
        let body_file = body_file.context("--section requires --body-file")?;
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
        let store = ctx.store()?;
        let (change_id, _transition, state) = locked_state(&store, reference)?;
        let payload = Payload::ChangelogRecorded { section, body };
        ensure_append_allowed(&state, &payload)?;
        let event = ctx.event(&store, &change_id, payload);
        store.append_event(&event)?;
        println!("changelog: {}", section.as_str());
        println!("event: {}", event.event_id);
        return Ok(0);
    }

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
                target: CHANGELOG_TARGET,
                renderer: CHANGELOG_RENDERER,
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
            target: CHANGELOG_TARGET,
            renderer: CHANGELOG_RENDERER,
            entries,
        };
        println!("{}", serde_json::to_string_pretty(&projection)?);
        return Ok(0);
    }

    let rendered = render_unreleased(&entries, provenance);
    if write {
        write_changelog(ctx, &rendered)?;
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
        category: entry.section.as_str(),
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
    for section in ChangelogSection::ALL {
        let section_entries = entries
            .iter()
            .filter(|entry| entry.category == section.as_str())
            .collect::<Vec<_>>();
        if section_entries.is_empty() {
            continue;
        }
        rendered.push_str("\n### ");
        rendered.push_str(section.as_str());
        rendered.push_str("\n\n");
        for entry in section_entries {
            rendered.push_str(entry.body.trim_end());
            rendered.push('\n');
            if provenance {
                rendered.push_str(&provenance_line(entry));
                rendered.push('\n');
            }
        }
    }
    rendered
}

fn print_entry(change: &str, entry: &ChangelogEntry, provenance: bool) {
    println!("### {}\n", entry.section.as_str());
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

fn write_changelog(ctx: &Ctx, rendered: &str) -> Result<()> {
    let root = gitio::toplevel(&ctx.cwd)?;
    let path = root.join("CHANGELOG.md");
    let original = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let heading = "## [Unreleased]";
    let heading_start = original
        .match_indices(heading)
        .find_map(|(offset, _)| {
            let line_start = offset == 0 || original.as_bytes()[offset - 1] == b'\n';
            let line_end = original
                .as_bytes()
                .get(offset + heading.len())
                .is_none_or(|byte| *byte == b'\n' || *byte == b'\r');
            (line_start && line_end).then_some(offset)
        })
        .context("CHANGELOG.md has no ## [Unreleased] heading")?;
    let after_heading = original[heading_start..]
        .find('\n')
        .map(|offset| heading_start + offset + 1)
        .unwrap_or(original.len());
    let next_release = original[after_heading..]
        .match_indices("## [")
        .find_map(|(offset, _)| {
            let absolute = after_heading + offset;
            (absolute == 0 || original.as_bytes()[absolute - 1] == b'\n').then_some(absolute)
        })
        .context("CHANGELOG.md has no released section after [Unreleased]")?;
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
    fs::write(&path, updated).with_context(|| format!("write {}", path.display()))
}
