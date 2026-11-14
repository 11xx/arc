//! Pins: artifacts kept as standing references.
//!
//! A pin is a fact recorded about an artifact, never a kind of its own, so a
//! note, a plan or a discussion can each be one. A pinned artifact is listed
//! first by `catchup` and stays out of the work queues until it is unpinned:
//! being a reference and being work are different states of the same file.

use super::{
    append_event, archive_dir, check_artifact_name, ensure_storage_settled, first_heading,
    format_age, is_consumed, lock_journal_transition, missing_write_target, read_events,
    relocation, resolve_dir, Amendments, JournalEvent, RecordedIdentity,
};
use crate::commands::Ctx;
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The pin each pinned artifact holds now: its latest `pinned` event with no
/// `unpinned` after it.
pub(super) fn pins(events: &[JournalEvent]) -> BTreeMap<&str, &JournalEvent> {
    let mut pins = BTreeMap::new();
    for event in events {
        let Some(file) = event.file.as_deref() else {
            continue;
        };
        match event.event.as_str() {
            "pinned" => {
                pins.insert(file, event);
            }
            "unpinned" => {
                pins.remove(file);
            }
            _ => {}
        }
    }
    pins
}

/// The files pinned now, for views that filter many artifacts at once.
pub(super) fn pinned_files(events: &[JournalEvent]) -> BTreeSet<String> {
    pins(events).into_keys().map(str::to_string).collect()
}

pub(super) fn is_pinned(events: &[JournalEvent], file: &str) -> bool {
    pins(events).contains_key(file)
}

/// The pin `file` holds now, when it is pinned.
pub(super) fn current_pin<'a>(events: &'a [JournalEvent], file: &str) -> Option<&'a JournalEvent> {
    pins(events).remove(file)
}

/// The refusal for a storage move that would take a pinned artifact out of
/// reach. A pin promises the artifact stays live and appendable, so the move
/// waits for an explicit unpin.
pub(super) fn refuse_pinned_move(events: &[JournalEvent], file: &str, verb: &str) -> Result<()> {
    if is_pinned(events, file) {
        bail!("{file} is pinned; `arc journal unpin {file}` before you {verb} it");
    }
    Ok(())
}

fn pin_reason(reason: Option<&str>) -> Result<Option<String>> {
    let Some(reason) = reason else {
        return Ok(None);
    };
    if reason.contains(['\n', '\r']) {
        bail!("--reason must be a single line");
    }
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        bail!("--reason must say why the artifact stays visible, or be omitted");
    }
    Ok(Some(trimmed.to_string()))
}

/// Pin an artifact in hot storage. Consumption does not matter: a finished
/// plan is still a reference. Pinning again with the reason already recorded
/// writes nothing; another reason records a new pin that replaces it.
pub(super) fn pin(
    ctx: &Ctx,
    filename: &str,
    reason: Option<&str>,
    guide: Option<&str>,
) -> Result<i32> {
    let (topic, _) = check_artifact_name(ctx, filename)?;
    let reason = pin_reason(reason)?;
    if let Some(name) = guide {
        super::guide_topics::validate_name(name)?;
    }
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let events = read_events(&dir)?;
    ensure_storage_settled(&dir, &events, filename)?;
    if !dir.join(filename).is_file() {
        if relocation(&events, filename).is_none() && archive_dir(&dir).join(filename).is_file() {
            bail!("{filename} is archived; `arc journal unarchive {filename}` before pinning it");
        }
        return Err(missing_write_target(&dir, filename));
    }
    let pins = pins(&events);
    let guide = guide.or_else(|| pins.get(filename).and_then(|pin| pin.guide.as_deref()));
    if let Some(name) = guide {
        if let Some((other, _)) = pins
            .iter()
            .find(|(file, pin)| **file != filename && pin.guide.as_deref() == Some(name))
        {
            bail!("guide topic {name:?} is already pinned to {other}; unpin it first");
        }
    }
    if let Some(current) = pins.get(filename) {
        if current.note == reason && current.guide.as_deref() == guide {
            println!("already pinned: {filename}");
            return Ok(0);
        }
    }
    let mut event = JournalEvent::base(ctx, Utc::now(), &topic, "pinned");
    event.file = Some(filename.to_string());
    event.note = reason;
    event.guide = guide.map(str::to_string);
    append_event(ctx, &dir, &event)?;
    println!("pinned: {filename}");
    Ok(0)
}

/// Remove an artifact's pin. The artifact itself need not still be present:
/// a pin left on a body moved by hand is still the journal's to retract.
pub(super) fn unpin(ctx: &Ctx, filename: &str) -> Result<i32> {
    let (topic, _) = check_artifact_name(ctx, filename)?;
    let dir = resolve_dir(&ctx.cwd)?;
    let _transition = lock_journal_transition(&dir)?;
    let events = read_events(&dir)?;
    if !is_pinned(&events, filename) {
        bail!("{filename} is not pinned");
    }
    let mut event = JournalEvent::base(ctx, Utc::now(), &topic, "unpinned");
    event.file = Some(filename.to_string());
    append_event(ctx, &dir, &event)?;
    println!("unpinned: {filename}");
    Ok(0)
}

/// One pinned artifact as the pin views show it.
#[derive(Serialize)]
pub(crate) struct PinRow {
    pub(crate) file: String,
    kind: String,
    /// The artifact's title as corrected, else its first heading.
    title: Option<String>,
    reason: Option<String>,
    pinned_at: String,
    pinned_by: RecordedIdentity,
    /// `hot`, `archived`, `relocated`, or `missing`: where the body is now.
    storage: &'static str,
    consumed: bool,
    /// Sections appended to the body: positions, evidence, questions,
    /// answers, and checkpoints. Corrections and retractions amend those
    /// and are not counted.
    blocks: usize,
    last_block: Option<LastBlock>,
    /// Where `journal published` last said a rendered copy lives.
    published: Option<String>,
    /// When the artifact last changed: its last appended block, else its pin.
    #[serde(skip)]
    active_at: Option<DateTime<Utc>>,
}

/// The newest section appended to a pinned artifact.
#[derive(Serialize)]
struct LastBlock {
    kind: &'static str,
    id: String,
    title: Option<String>,
    at: String,
}

/// Every pin in the journal at `dir`, most recently active first.
pub(crate) fn pin_rows(dir: &Path) -> Result<Vec<PinRow>> {
    let events = read_events(dir)?;
    let mut rows: Vec<PinRow> = pins(&events)
        .into_iter()
        .filter_map(|(file, pin)| pin_row(dir, &events, file, pin))
        .collect();
    rows.sort_by(|left, right| {
        right
            .active_at
            .cmp(&left.active_at)
            .then_with(|| right.file.cmp(&left.file))
    });
    Ok(rows)
}

fn pin_row(dir: &Path, events: &[JournalEvent], file: &str, pin: &JournalEvent) -> Option<PinRow> {
    let (_, _, kind) = super::parse_artifact_name(file)?;
    let amendments = Amendments::collect(events, file);
    let storage = if relocation(events, file).is_some() {
        "relocated"
    } else if dir.join(file).is_file() {
        "hot"
    } else if archive_dir(dir).join(file).is_file() {
        "archived"
    } else {
        "missing"
    };
    let title = amendments.title().map(str::to_string).or_else(|| {
        super::artifact_body_path(dir, file)
            .and_then(|path| first_heading(&path))
            .map(|heading| heading.trim_start_matches('#').trim().to_string())
    });
    let appended: Vec<(&'static str, &str, &JournalEvent)> = events
        .iter()
        .filter(|event| {
            event.file.as_deref() == Some(file)
                && !matches!(event.event.as_str(), "correction" | "retraction")
        })
        .filter_map(|event| {
            let (label, id) = event.entry_identity()?;
            Some((label, id, event))
        })
        .collect();
    let last_block = appended.last().map(|(label, id, event)| LastBlock {
        kind: label,
        id: id.to_string(),
        title: (event.event == "evidence")
            .then(|| amendments.field(id, "title", event.title.as_deref()))
            .flatten()
            .map(str::to_string),
        at: event.ts.clone(),
    });
    let active_at = appended
        .last()
        .map(|(_, _, event)| *event)
        .unwrap_or(pin)
        .timestamp();
    Some(PinRow {
        file: file.to_string(),
        kind,
        title,
        reason: pin.note.clone(),
        pinned_at: pin.ts.clone(),
        pinned_by: RecordedIdentity::from(pin),
        storage,
        consumed: is_consumed(events, file),
        blocks: appended.len(),
        last_block,
        published: super::page::published_url(events, file).map(str::to_string),
        active_at,
    })
}

/// The pin rows as `catchup` and `journal pins` print them: the file and
/// title, then the reason and the appended blocks beneath.
pub(crate) fn render_pins(rows: &[PinRow], now: DateTime<Utc>) {
    println!("pinned ({}):", rows.len());
    if rows.is_empty() {
        println!("  (none)");
    }
    for row in rows {
        render_pin_row(row, now);
    }
}

/// One pin row: the file and title, then the reason, the appended blocks,
/// storage when not hot, and the published copy beneath.
pub(crate) fn render_pin_row(row: &PinRow, now: DateTime<Utc>) {
    println!("  {}  {}", row.file, row.title.as_deref().unwrap_or(""));
    let mut facts = Vec::new();
    if let Some(reason) = &row.reason {
        facts.push(reason.clone());
    }
    facts.push(match (&row.last_block, row.blocks) {
        (Some(last), count) => {
            let age = DateTime::parse_from_rfc3339(&last.at)
                .ok()
                .map(|at| {
                    let seconds = now
                        .signed_duration_since(at.with_timezone(&Utc))
                        .num_seconds()
                        .max(0) as u64;
                    format!(" {} ago", format_age(seconds))
                })
                .unwrap_or_default();
            let noun = if count == 1 { "block" } else { "blocks" };
            let title = last
                .title
                .as_deref()
                .map(|title| format!(" \"{title}\""))
                .unwrap_or_default();
            format!("{count} {noun}, last {}{title}{age}", last.id)
        }
        (None, _) => "no appended blocks".to_string(),
    });
    if row.storage != "hot" {
        facts.push(row.storage.to_string());
    }
    if row.consumed {
        facts.push("consumed".to_string());
    }
    if let Some(url) = &row.published {
        facts.push(url.clone());
    }
    println!("    {}", facts.join(" · "));
}

/// `arc journal pins`: the pinned artifacts of this journal, or of the
/// operator journal under `--global`.
pub(super) fn list(ctx: &Ctx, json: bool) -> Result<i32> {
    let dir = resolve_dir(&ctx.cwd)?;
    let rows = pin_rows(&dir)?;
    if json {
        #[derive(Serialize)]
        struct Pins<'a> {
            schema: &'static str,
            dir: String,
            pins: &'a [PinRow],
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Pins {
                schema: "arc-journal-pins/1",
                dir: dir.display().to_string(),
                pins: &rows,
            })?
        );
    } else {
        super::print_dir_header(&dir);
        render_pins(&rows, Utc::now());
    }
    Ok(0)
}
