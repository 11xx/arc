use super::{locked_state, Ctx};
use crate::gitio;
use crate::model::{ChangelogSection, Closure, Payload};
use crate::state::{ChangeState, ChangelogEntry};
use crate::ExecutionRole;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Serialize)]
struct ProjectedEntry<'a> {
    change: &'a str,
    section: ChangelogSection,
    body: &'a str,
}

#[allow(clippy::too_many_arguments)]
pub fn changelog(
    ctx: &Ctx,
    role: ExecutionRole,
    reference: Option<&str>,
    section: Option<ChangelogSection>,
    body_file: Option<String>,
    json: bool,
    since: Option<String>,
    write: bool,
) -> Result<i32> {
    if section.is_some() || body_file.is_some() {
        let reference = reference.context("recording a changelog entry requires CHANGE")?;
        let section = section.context("--body-file requires --section")?;
        let body_file = body_file.context("--section requires --body-file")?;
        if json || since.is_some() || write {
            bail!("--json, --since, and --write cannot be used when recording an entry");
        }
        if role == ExecutionRole::Reviewer {
            eprintln!("role refusal: reviewer may not changelog (requires implementer or lead)");
            return Ok(9);
        }
        let body = super::read_body_file_verbatim(&body_file)?;
        let store = ctx.store()?;
        let (change_id, _transition, state) = locked_state(&store, reference)?;
        if state.is_closed() {
            bail!("change {change_id} is closed");
        }
        let event = ctx.event(
            &store,
            &change_id,
            Payload::ChangelogRecorded { section, body },
        );
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
            println!("{}", serde_json::to_string_pretty(&state.changelog)?);
        } else if let Some(entry) = state.changelog {
            print_entry(&entry);
        }
        return Ok(0);
    }

    let store = ctx.store()?;
    let boundary = match since {
        Some(revision) => Some(gitio::rev_parse(&ctx.cwd, &revision)?),
        None => gitio::latest_tag(&ctx.cwd)?,
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
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(0);
    }

    let rendered = render_unreleased(&entries);
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
            ProjectedEntry {
                change: &state.slug,
                section: entry.section,
                body: &entry.body,
            },
        ))
    })
}

fn render_unreleased(entries: &[ProjectedEntry<'_>]) -> String {
    let mut rendered = String::from("## [Unreleased]\n");
    for section in ChangelogSection::ALL {
        let bodies = entries
            .iter()
            .filter(|entry| entry.section == section)
            .map(|entry| entry.body.trim_end())
            .collect::<Vec<_>>();
        if bodies.is_empty() {
            continue;
        }
        rendered.push_str("\n### ");
        rendered.push_str(section.as_str());
        rendered.push_str("\n\n");
        rendered.push_str(&bodies.join("\n"));
        rendered.push('\n');
    }
    rendered
}

fn print_entry(entry: &ChangelogEntry) {
    println!("### {}\n", entry.section.as_str());
    print!("{}", entry.body);
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
