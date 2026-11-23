//! Progress series: the list of items a series of work is made of.
//!
//! A series is a journal artifact of kind `progress`. Its body is written
//! once, when the series starts, and arc never reads it back. Everything a
//! report shows is either a typed event on the artifact (`item-*`,
//! `series-related`) or derived from the ledger by the caller: an item's
//! stage follows the change it is linked to and is never written here.
//!
//! The list is replayed in append order. Every edit is one event and nothing
//! is rewritten, so the list as it stands and what changed since the series
//! started are both read from the same log. A series is retired when its
//! conclusion supersedes it: a `consumed` event with outcome `superseded`
//! whose `decision` names the conclusion.

use super::{
    append_event, create_artifact, heading_from_topic, lock_journal_transition,
    parse_artifact_name, resolve_dir, valid_topic, JournalEvent,
};
use crate::commands::Ctx;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// The artifact kind a series is filed under.
pub(crate) const KIND: &str = "progress";

/// Artifact kinds an item may record as its originating source (`--from`).
pub(crate) const SOURCE_KINDS: [&str; 4] = ["todo", "feature-request", "plan", "handoff"];

/// Fields only progress-series events carry, flattened into the journal
/// event. Every one is optional, so every event written without them remains
/// valid input.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct SeriesFields {
    /// On the `note` event that started a series: the plan it follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    follows: Option<String>,
    /// The item an `item-*` event acts on, by its id within the series.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item: Option<String>,
    /// Where an added or moved item goes: directly before this item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item_before: Option<String>,
    /// Where an added or moved item goes: directly after this item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item_after: Option<String>,
    /// Set on an item declared by the call that started its series, which
    /// is what "since the start" counts from.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    initial: bool,
    /// The items an `item-split` puts in place of its item, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    parts: Vec<SplitPart>,
    /// The change an `item-linked` event names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    change: Option<String>,
    /// On an `item-linked` event, the registry slug of the project whose
    /// ledger holds the change; absent for this project's ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    change_project: Option<String>,
    /// The journal question an `item-waiting` event says the item waits on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    waits_on: Option<ItemWait>,
    /// The journal artifact an item originated from (`--from` / `source`).
    /// Named `item_source` rather than `source` because `JournalEvent` already
    /// carries `source` for filing provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item_source: Option<ItemSource>,
    /// The durable artifact recorded as resolving an unlinked `done` item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved_by: Option<ResolvedBy>,
    /// The series in another project a `series-related` event names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    related: Option<RelatedSeries>,
}

/// One item a split produced.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SplitPart {
    pub(crate) item: String,
    pub(crate) title: String,
}

/// A question in the series' own journal: the discussion that holds it and
/// its id there.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ItemWait {
    pub(crate) file: String,
    pub(crate) question: String,
}

/// A journal artifact an item originated from: a bare filename in this
/// journal, or `<project>::<file>` in another registered project's journal.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ItemSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
    pub(crate) file: String,
}

impl std::fmt::Display for ItemSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.project {
            Some(project) => write!(f, "{project}::{}", self.file),
            None => f.write_str(&self.file),
        }
    }
}

/// A durable artifact cited as resolving an unlinked `done` item: a bare
/// `conclusion` or `decision` filename of this journal, or a recorded
/// repository `checkpoint` name.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ResolvedBy {
    pub(crate) kind: String,
    pub(crate) target: String,
}

impl std::fmt::Display for ResolvedBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.kind, self.target)
    }
}

/// The change an item follows: one of this project's ledger, or of the
/// registered project `project` names by its slug.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ItemLink {
    pub(crate) project: Option<String>,
    pub(crate) change: String,
}

impl std::fmt::Display for ItemLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.project {
            Some(project) => write!(f, "{project}::{}", self.change),
            None => f.write_str(&self.change),
        }
    }
}

/// A series in another project, named by that project's registry slug and
/// the series' filename in its journal.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct RelatedSeries {
    pub(crate) project: String,
    pub(crate) file: String,
}

fn single_line(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains(['\n', '\r'])
}

pub(crate) fn valid_item_id(value: &str) -> bool {
    value
        .strip_prefix('i')
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

fn valid_item_source(source: &ItemSource) -> bool {
    source.project.as_deref().is_none_or(single_line)
        && bare_artifact_kind(&source.file)
            .is_some_and(|kind| SOURCE_KINDS.contains(&kind.as_str()))
}

fn valid_resolved_by(resolved: &ResolvedBy) -> bool {
    match resolved.kind.as_str() {
        "conclusion" | "decision" => {
            bare_artifact_kind(&resolved.target).as_deref() == Some(resolved.kind.as_str())
        }
        "checkpoint" => {
            single_line(&resolved.target)
                && !resolved.target.contains(['/', '\\', '\0'])
                && !resolved.target.contains(super::REFERENCE_SEPARATOR)
                && !resolved.target.starts_with('.')
        }
        _ => false,
    }
}

/// Whether `file` names a series artifact.
pub(crate) fn is_series_file(file: &str) -> bool {
    parse_artifact_name(file).is_some_and(|(_, _, kind)| kind == KIND)
}

/// Whether a series event says enough to be replayed. An event that fails
/// is not read at all, so a hand-written line can never place an item
/// nowhere or drop one without a reason.
pub(super) fn well_formed(event: &JournalEvent) -> bool {
    let fields = &event.series;
    let line = |value: &Option<String>| value.as_deref().is_some_and(single_line);
    let on_series = event.file.as_deref().is_some_and(is_series_file);
    let names_item = fields.item.as_deref().is_some_and(valid_item_id);
    let placements = [&fields.item_before, &fields.item_after]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let placed = placements.iter().all(|anchor| valid_item_id(anchor));
    let no_link_or_wait = fields.parts.is_empty()
        && fields.change.is_none()
        && fields.change_project.is_none()
        && fields.waits_on.is_none()
        && fields.related.is_none();
    let bare = no_link_or_wait && fields.item_source.is_none() && fields.resolved_by.is_none();
    if !on_series {
        return false;
    }
    match event.event.as_str() {
        "item-added" => {
            names_item
                && line(&event.title)
                && placements.len() <= 1
                && placed
                && no_link_or_wait
                && fields.resolved_by.is_none()
                && fields.item_source.as_ref().is_none_or(valid_item_source)
        }
        "item-renamed" => names_item && line(&event.title) && placements.is_empty() && bare,
        "item-moved" => names_item && placements.len() == 1 && placed && bare,
        "item-dropped" => names_item && line(&event.note) && placements.is_empty() && bare,
        "item-done" => {
            names_item
                && event.note.as_deref().is_none_or(single_line)
                && placements.is_empty()
                && no_link_or_wait
                && fields.item_source.is_none()
                && fields.resolved_by.as_ref().is_none_or(valid_resolved_by)
        }
        "item-reopened" | "item-unlinked" | "item-unwaited" | "item-unsourced" => {
            names_item
                && event.note.as_deref().is_none_or(single_line)
                && placements.is_empty()
                && bare
        }
        "item-sourced" => {
            names_item
                && event.note.is_none()
                && placements.is_empty()
                && no_link_or_wait
                && fields.resolved_by.is_none()
                && fields.item_source.as_ref().is_some_and(valid_item_source)
        }
        "item-linked" => {
            names_item
                && fields.change.as_deref().is_some_and(single_line)
                && fields.change_project.as_deref().is_none_or(single_line)
                && placements.is_empty()
                && fields.parts.is_empty()
                && fields.waits_on.is_none()
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.related.is_none()
        }
        "item-waiting" => {
            names_item
                && event.note.is_none()
                && placements.is_empty()
                && fields.parts.is_empty()
                && fields.change.is_none()
                && fields.change_project.is_none()
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.related.is_none()
                && fields.waits_on.as_ref().is_some_and(|wait| {
                    bare_artifact_kind(&wait.file).is_some() && single_line(&wait.question)
                })
        }
        "item-split" => {
            names_item
                && line(&event.note)
                && placements.is_empty()
                && fields.parts.len() >= 2
                && fields
                    .parts
                    .iter()
                    .all(|part| valid_item_id(&part.item) && single_line(&part.title))
                && fields.change.is_none()
                && fields.change_project.is_none()
                && fields.waits_on.is_none()
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.related.is_none()
        }
        "series-related" | "series-unrelated" => {
            fields.item.is_none()
                && placements.is_empty()
                && fields.parts.is_empty()
                && fields.change.is_none()
                && fields.change_project.is_none()
                && fields.waits_on.is_none()
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.related.as_ref().is_some_and(|related| {
                    single_line(&related.project) && is_series_file(&related.file)
                })
        }
        _ => false,
    }
}

/// The line the journal's own event views print for a series event.
pub(super) fn event_message(event: &JournalEvent) -> String {
    let fields = &event.series;
    let file = event.file.as_deref().unwrap_or_default();
    let item = fields.item.as_deref().unwrap_or_default();
    let title = event.title.as_deref().unwrap_or_default();
    let note = event.note.as_deref().unwrap_or_default();
    match event.event.as_str() {
        "item-added" => format!("added item {item} to {file}: {title}"),
        "item-renamed" => format!("renamed item {item} of {file}: {title}"),
        "item-moved" => format!("moved item {item} of {file}"),
        "item-dropped" => format!("dropped item {item} of {file}: {note}"),
        "item-done" => match &fields.resolved_by {
            Some(by) => format!("marked item {item} of {file} done by {by}"),
            None => format!("marked item {item} of {file} done"),
        },
        "item-reopened" => format!("reopened item {item} of {file}"),
        "item-sourced" => format!(
            "linked item {item} of {file} to source {}",
            fields
                .item_source
                .as_ref()
                .map_or_else(String::new, ToString::to_string)
        ),
        "item-unsourced" => format!("removed source link from item {item} of {file}"),
        "item-linked" => format!(
            "linked item {item} of {file} to change {}{}",
            fields
                .change_project
                .as_deref()
                .map(|project| format!("{project}::"))
                .unwrap_or_default(),
            fields.change.as_deref().unwrap_or_default()
        ),
        "item-unlinked" => format!("unlinked item {item} of {file}"),
        "item-waiting" => format!(
            "item {item} of {file} waits on question {} of {}",
            fields
                .waits_on
                .as_ref()
                .map_or("", |wait| wait.question.as_str()),
            fields
                .waits_on
                .as_ref()
                .map_or("", |wait| wait.file.as_str())
        ),
        "item-unwaited" => format!("item {item} of {file} waits on no question"),
        "item-split" => format!(
            "split item {item} of {file} into {}: {note}",
            fields
                .parts
                .iter()
                .map(|part| part.item.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "series-related" | "series-unrelated" => {
            let related = fields.related.as_ref();
            format!(
                "{} {file} {} {}::{}",
                if event.event == "series-related" {
                    "related"
                } else {
                    "unrelated"
                },
                if event.event == "series-related" {
                    "to"
                } else {
                    "from"
                },
                related.map_or("", |r| r.project.as_str()),
                related.map_or("", |r| r.file.as_str())
            )
        }
        _ => String::new(),
    }
}

/// Where an item stands, as the series records it. A linked item's stage is
/// the ledger's to say; this is only what the series itself was told.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ItemState {
    Open,
    Done,
    Dropped,
    Split,
}

/// One item of a series as its events leave it.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) state: ItemState,
    /// The change the item is linked to, when it is.
    pub(crate) change: Option<String>,
    /// The registry slug of the project whose ledger holds `change`; absent
    /// for this project's ledger.
    pub(crate) project: Option<String>,
    /// The journal question the item was last said to wait on. Whether it
    /// still waits is the question's to say: only while it is open.
    pub(crate) waits_on: Option<ItemWait>,
    /// The journal artifact the item originated from, when recorded.
    pub(crate) source: Option<ItemSource>,
    /// The durable artifact recorded as resolving an unlinked `done` item.
    pub(crate) resolved_by: Option<ResolvedBy>,
    /// The reason or note recorded with the item's latest state.
    pub(crate) note: Option<String>,
    /// When the item reached its latest state.
    pub(crate) state_at: Option<String>,
    /// For a split item, the items that replaced it.
    pub(crate) split_into: Vec<String>,
    initial: bool,
}

impl Item {
    /// The change the item follows, when it is linked.
    pub(crate) fn link(&self) -> Option<ItemLink> {
        self.change.as_ref().map(|change| ItemLink {
            project: self.project.clone(),
            change: change.clone(),
        })
    }

    /// Whether the item counts in the list as it stands. A dropped or split
    /// item stays visible and leaves the count.
    pub(crate) fn stands(&self) -> bool {
        !matches!(self.state, ItemState::Dropped | ItemState::Split)
    }
}

/// A series' retirement: when, and by which conclusion.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Retirement {
    pub(crate) at: String,
    pub(crate) by: Option<String>,
}

/// A series replayed from its events.
#[derive(Clone, Debug)]
pub(crate) struct Series {
    pub(crate) file: String,
    pub(crate) title: String,
    pub(crate) follows: Option<String>,
    pub(crate) started_at: Option<String>,
    pub(crate) retired: Option<Retirement>,
    pub(crate) items: Vec<Item>,
    pub(crate) related: Vec<RelatedSeries>,
    /// Items added after the start, counted from the events rather than from
    /// the items left, so an added item later dropped still counts as added.
    added: usize,
}

/// What changed in the list since the call that started the series.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct SinceStart {
    /// Items declared when the series started.
    pub(crate) declared: usize,
    pub(crate) added: usize,
    pub(crate) dropped: usize,
    pub(crate) split: usize,
}

impl Series {
    pub(crate) fn since_start(&self) -> SinceStart {
        let mut since = SinceStart::default();
        for item in &self.items {
            if item.initial {
                since.declared += 1;
            }
            match item.state {
                ItemState::Dropped => since.dropped += 1,
                ItemState::Split => since.split += 1,
                _ => {}
            }
        }
        since.added = self.added;
        since
    }

    fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }

    fn require_item(&self, id: &str) -> Result<&Item> {
        self.item(id)
            .with_context(|| format!("{} has no item {id}", self.file))
    }

    fn next_id(&self) -> u64 {
        self.items
            .iter()
            .filter_map(|item| item.id.strip_prefix('i')?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1
    }
}

fn place(items: &mut Vec<Item>, item: Item, before: Option<&str>, after: Option<&str>) {
    let anchor = before
        .map(|id| (id, 0))
        .or_else(|| after.map(|id| (id, 1)))
        .and_then(|(id, offset)| {
            items
                .iter()
                .position(|candidate| candidate.id == id)
                .map(|at| at + offset)
        });
    match anchor {
        Some(at) => items.insert(at, item),
        None => items.push(item),
    }
}

/// Replay one series from the journal's events. `None` when no event ever
/// started it.
pub(crate) fn replay(events: &[JournalEvent], file: &str) -> Option<Series> {
    let mut series: Option<Series> = None;
    let mut added = 0;
    for event in events.iter().filter(|e| e.file.as_deref() == Some(file)) {
        if event.event == "note" {
            let topic = parse_artifact_name(file).map(|(_, topic, _)| topic)?;
            series.get_or_insert(Series {
                file: file.to_string(),
                title: event
                    .title
                    .clone()
                    .unwrap_or_else(|| heading_from_topic(&topic)),
                follows: event.series.follows.clone(),
                started_at: Some(event.ts.clone()),
                retired: None,
                items: Vec::new(),
                related: Vec::new(),
                added: 0,
            });
            continue;
        }
        let Some(current) = series.as_mut() else {
            continue;
        };
        let fields = &event.series;
        let id = fields.item.as_deref().unwrap_or_default();
        let at = Some(event.ts.clone());
        match event.event.as_str() {
            "item-added" if current.item(id).is_none() => {
                if !fields.initial {
                    added += 1;
                }
                let item = Item {
                    id: id.to_string(),
                    title: event.title.clone().unwrap_or_default(),
                    state: ItemState::Open,
                    change: None,
                    project: None,
                    waits_on: None,
                    source: fields.item_source.clone(),
                    resolved_by: None,
                    note: None,
                    state_at: at,
                    split_into: Vec::new(),
                    initial: fields.initial,
                };
                place(
                    &mut current.items,
                    item,
                    fields.item_before.as_deref(),
                    fields.item_after.as_deref(),
                );
            }
            "item-moved" => {
                let anchor = fields
                    .item_before
                    .as_deref()
                    .or(fields.item_after.as_deref());
                if anchor == Some(id) || anchor.is_none_or(|a| current.item(a).is_none()) {
                    continue;
                }
                if let Some(index) = current.items.iter().position(|item| item.id == id) {
                    let item = current.items.remove(index);
                    place(
                        &mut current.items,
                        item,
                        fields.item_before.as_deref(),
                        fields.item_after.as_deref(),
                    );
                }
            }
            "item-split" => {
                let Some(index) = current.items.iter().position(|item| item.id == id) else {
                    continue;
                };
                let fresh: Vec<&SplitPart> = fields
                    .parts
                    .iter()
                    .filter(|part| current.item(&part.item).is_none())
                    .collect();
                let original = &mut current.items[index];
                original.state = ItemState::Split;
                original.note = event.note.clone();
                original.state_at = at.clone();
                original.split_into = fields.parts.iter().map(|p| p.item.clone()).collect();
                for (offset, part) in fresh.into_iter().enumerate() {
                    current.items.insert(
                        index + 1 + offset,
                        Item {
                            id: part.item.clone(),
                            title: part.title.clone(),
                            state: ItemState::Open,
                            change: None,
                            project: None,
                            waits_on: None,
                            source: None,
                            resolved_by: None,
                            note: None,
                            state_at: at.clone(),
                            split_into: Vec::new(),
                            initial: false,
                        },
                    );
                }
            }
            "series-related" => {
                if let Some(related) = &fields.related {
                    if !current.related.contains(related) {
                        current.related.push(related.clone());
                    }
                }
            }
            "series-unrelated" => {
                if let Some(related) = &fields.related {
                    current.related.retain(|known| known != related);
                }
            }
            "consumed" => {
                current.retired = Some(Retirement {
                    at: event.ts.clone(),
                    by: event.decision.clone(),
                });
            }
            kind => {
                let Some(item) = current.items.iter_mut().find(|item| item.id == id) else {
                    continue;
                };
                match kind {
                    "item-renamed" => item.title = event.title.clone().unwrap_or_default(),
                    "item-dropped" | "item-done" | "item-reopened" => {
                        item.state = match kind {
                            "item-dropped" => ItemState::Dropped,
                            "item-done" => ItemState::Done,
                            _ => ItemState::Open,
                        };
                        item.resolved_by = if kind == "item-done" {
                            fields.resolved_by.clone()
                        } else {
                            None
                        };
                        item.note = event.note.clone();
                        item.state_at = at;
                    }
                    "item-linked" => {
                        item.change = fields.change.clone();
                        item.project = fields.change_project.clone();
                    }
                    "item-unlinked" => {
                        item.change = None;
                        item.project = None;
                    }
                    "item-sourced" => item.source = fields.item_source.clone(),
                    "item-unsourced" => item.source = None,
                    "item-waiting" => item.waits_on = fields.waits_on.clone(),
                    "item-unwaited" => item.waits_on = None,
                    _ => {}
                }
            }
        }
    }
    if let Some(series) = series.as_mut() {
        series.added = added;
        if let Some(title) = super::Amendments::collect(events, file).title() {
            series.title = title.to_string();
        }
    }
    series
}

/// Every series this journal's events started, in the order they started.
pub(crate) fn series_files(events: &[JournalEvent]) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for event in events {
        if event.event == "note" {
            if let Some(file) = event.file.as_deref().filter(|f| is_series_file(f)) {
                if !files.iter().any(|known| known == file) {
                    files.push(file.to_string());
                }
            }
        }
    }
    files
}

/// Every series `events` started that is not retired.
pub(crate) fn active_in(events: &[JournalEvent]) -> Vec<Series> {
    series_files(events)
        .iter()
        .filter_map(|file| replay(events, file))
        .filter(|series| series.retired.is_none())
        .collect()
}

/// One series in the journal at `dir`, by filename or by topic. A topic
/// several series share resolves to the one not yet retired, and is refused
/// when that does not narrow it to one.
pub(crate) fn named_series(dir: &Path, name: &str) -> Result<Series> {
    named_in(&super::read_events(dir)?, dir, name)
}

/// [`named_series`] among the events of the journal at `dir`, already read.
pub(crate) fn named_in(events: &[JournalEvent], dir: &Path, name: &str) -> Result<Series> {
    let file = resolve_name(events, name, dir)?;
    replay(events, &file).with_context(|| format!("{file} was never started as a series"))
}

fn resolve_name(events: &[JournalEvent], name: &str, dir: &Path) -> Result<String> {
    let files = series_files(events);
    if parse_artifact_name(name).is_some() {
        if files.iter().any(|file| file == name) {
            return Ok(name.to_string());
        }
        bail!("no series {name} in {}", dir.display());
    }
    let matching: Vec<&String> = files
        .iter()
        .filter(|file| parse_artifact_name(file).is_some_and(|(_, topic, _)| topic == name))
        .collect();
    let active: Vec<&&String> = matching
        .iter()
        .filter(|file| replay(events, file).is_some_and(|series| series.retired.is_none()))
        .collect();
    match (active.as_slice(), matching.as_slice()) {
        ([one], _) => Ok((**one).clone()),
        ([], [one]) => Ok((*one).clone()),
        ([], []) => bail!("no series named {name:?} in {}", dir.display()),
        _ => bail!(
            "{name:?} names several series; name one by its file: {}",
            matching
                .iter()
                .map(|file| file.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The series a write acts on, read under the journal's transition lock. A
/// retired series is a closed record and refuses every write.
struct Open {
    dir: std::path::PathBuf,
    events: Vec<JournalEvent>,
    series: Series,
    topic: String,
    _lock: super::JournalTransitionLock,
}

fn open_for_write(ctx: &Ctx, name: &str) -> Result<Open> {
    let dir = resolve_dir(&ctx.cwd)?;
    let lock = lock_journal_transition(&dir)?;
    let events = super::read_events(&dir)?;
    let file = resolve_name(&events, name, &dir)?;
    let series = replay(&events, &file).with_context(|| format!("{file} was never started"))?;
    if let Some(retired) = &series.retired {
        bail!(
            "{file} was retired{}; it is read-only",
            retired
                .by
                .as_deref()
                .map(|by| format!(" by {by}"))
                .unwrap_or_default()
        );
    }
    let topic = parse_artifact_name(&file)
        .map(|(_, topic, _)| topic)
        .unwrap_or_default();
    Ok(Open {
        dir,
        events,
        series,
        topic,
        _lock: lock,
    })
}

impl Open {
    fn event(&self, ctx: &Ctx, kind: &str, item: Option<&str>) -> JournalEvent {
        let mut event = JournalEvent::base(ctx, Utc::now(), &self.topic, kind);
        event.file = Some(self.series.file.clone());
        event.series.item = item.map(str::to_string);
        event
    }

    fn append(&self, ctx: &Ctx, event: &JournalEvent) -> Result<()> {
        append_event(ctx, &self.dir, event)
    }

    /// An item that still stands, for a write that only makes sense on one.
    fn standing(&self, id: &str) -> Result<&Item> {
        let item = self.series.require_item(id)?;
        if !item.stands() {
            bail!(
                "item {id} of {} is {}; it no longer stands in the list",
                self.series.file,
                state_word(item.state)
            );
        }
        Ok(item)
    }
}

fn state_word(state: ItemState) -> &'static str {
    match state {
        ItemState::Open => "open",
        ItemState::Done => "done",
        ItemState::Dropped => "dropped",
        ItemState::Split => "split",
    }
}

fn one_line(what: &str, value: &str) -> Result<String> {
    if !single_line(value) {
        bail!("{what} must be one non-empty line");
    }
    Ok(value.trim().to_string())
}

/// The kind of `name` when it is a bare artifact filename, judged from its
/// characters alone: no path separator, no `<journal-dir>::<file>`
/// qualifier, no leading dot, and the `<timestamp>-<topic>-<kind>.md` shape.
/// Nothing is read to decide it, so a name that is refused is never looked
/// up anywhere first.
pub(crate) fn bare_artifact_kind(name: &str) -> Option<String> {
    if name.contains(['/', '\\', '\0'])
        || name.contains(super::REFERENCE_SEPARATOR)
        || name.starts_with('.')
    {
        return None;
    }
    parse_artifact_name(name).map(|(_, _, kind)| kind)
}

/// An artifact a write names on the command line, as a path inside this
/// journal. Only a bare artifact filename of `kind` is accepted, decided from
/// the name's characters before anything is read; the name must then be a
/// regular file in the hot journal or its cold archive. A symbolic link is
/// refused rather than followed, so a name never leads out of the journal's
/// directories.
fn named_artifact(dir: &Path, name: &str, kind: &str, flag: &str) -> Result<std::path::PathBuf> {
    let Some(found) = bare_artifact_kind(name) else {
        bail!(
            "{flag} takes a bare {kind} artifact filename of this journal \
             (<timestamp>-<topic>-{kind}.md), not a path or another journal's reference"
        );
    };
    if found != kind {
        bail!("{flag} takes a {kind} artifact's filename; {name} is a {found}");
    }
    super::artifact_body_path(dir, name).with_context(|| {
        format!(
            "no {kind} {name} as a regular file in {} or its cold archive",
            dir.display()
        )
    })
}

/// Where an added or moved item goes, checked against the list.
#[derive(Clone, Copy)]
pub(crate) enum Placement<'a> {
    End,
    Before(&'a str),
    After(&'a str),
}

impl Placement<'_> {
    fn apply(self, series: &Series, event: &mut JournalEvent, moving: Option<&str>) -> Result<()> {
        let anchor = match self {
            Placement::End => return Ok(()),
            Placement::Before(anchor) | Placement::After(anchor) => anchor,
        };
        series.require_item(anchor)?;
        if moving == Some(anchor) {
            bail!("an item cannot be placed relative to itself");
        }
        match self {
            Placement::Before(anchor) => event.series.item_before = Some(anchor.to_string()),
            Placement::After(anchor) => event.series.item_after = Some(anchor.to_string()),
            Placement::End => {}
        }
        Ok(())
    }
}

/// Validate a source artifact reference (`<file>` in `dir`, or
/// `<project>::<file>` in another registered project's journal) without
/// following symlinks or paths outside the journal.
pub(crate) fn resolve_source(dir: &Path, spec: &str, flag: &str) -> Result<ItemSource> {
    let accepted = SOURCE_KINDS.join("|");
    let check_in = |target_dir: &Path, file: &str| -> Result<()> {
        let Some(kind) = bare_artifact_kind(file) else {
            bail!(
                "{flag} takes a bare artifact filename (<timestamp>-<topic>-<{accepted}>.md) \
                 or <project>::<file>, not a path"
            );
        };
        if !SOURCE_KINDS.contains(&kind.as_str()) {
            bail!("{flag} takes a {accepted} artifact; {file} is a {kind}");
        }
        super::artifact_body_path(target_dir, file).with_context(|| {
            format!(
                "no {kind} {file} as a regular file in {} or its cold archive",
                target_dir.display()
            )
        })?;
        Ok(())
    };
    let Some((prefix, file)) = spec.split_once(super::REFERENCE_SEPARATOR) else {
        check_in(dir, spec)?;
        return Ok(ItemSource {
            project: None,
            file: spec.to_string(),
        });
    };
    let project = super::registered_project(prefix)?;
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if canonical(&project.journal_dir) == canonical(dir) {
        bail!("{prefix} is this project; name {file} without a project");
    }
    check_in(&project.journal_dir, file)?;
    Ok(ItemSource {
        project: Some(project.slug),
        file: file.to_string(),
    })
}

/// Validate a `--by` journal artifact (`conclusion` or `decision`) in this
/// journal. Returns `Ok(None)` when `target` is not a bare journal filename
/// (so the caller can resolve it as a checkpoint name), refusing paths,
/// foreign qualifiers, or disallowed journal kinds by spelling.
pub(crate) fn resolve_journal_resolution(
    dir: &Path,
    target: &str,
    flag: &str,
) -> Result<Option<ResolvedBy>> {
    if target.contains(['/', '\\', '\0'])
        || target.contains(super::REFERENCE_SEPARATOR)
        || target.starts_with('.')
    {
        bail!(
            "{flag} takes a bare conclusion or decision filename of this journal, \
             or a checkpoint name, not a path or another journal's reference"
        );
    }
    let Some(kind) = bare_artifact_kind(target) else {
        return Ok(None);
    };
    if !matches!(kind.as_str(), "conclusion" | "decision") {
        bail!("{flag} takes a conclusion, decision, or checkpoint; {target} is a {kind}");
    }
    super::artifact_body_path(dir, target).with_context(|| {
        format!(
            "no {kind} {target} as a regular file in {} or its cold archive",
            dir.display()
        )
    })?;
    Ok(Some(ResolvedBy {
        kind,
        target: target.to_string(),
    }))
}

/// Resolve `<series>:<item>` against `events` of the journal at `dir` for a
/// live lane claim.
pub(crate) fn resolve_lane_item(
    dir: &Path,
    events: &[JournalEvent],
    spec: &str,
) -> Result<super::LaneItem> {
    let Some((series_name, item_id)) = spec.rsplit_once(':') else {
        bail!("--item takes <series>:<item>, such as my-series:i3; got {spec:?}");
    };
    let series = named_in(events, dir, series_name)?;
    if series.retired.is_some() {
        bail!("{} was retired; cannot target it with a lane", series.file);
    }
    let item = series.require_item(item_id)?;
    if !item.stands() {
        bail!(
            "item {item_id} of {} is {}; it no longer stands in the list",
            series.file,
            state_word(item.state)
        );
    }
    Ok(super::LaneItem {
        file: series.file,
        item: item_id.to_string(),
    })
}

/// Start a series: write its artifact and declare its first items.
pub(crate) fn start(
    ctx: &Ctx,
    topic: &str,
    title: &str,
    follows: Option<&str>,
    items: &[String],
    sources: &BTreeMap<usize, String>,
) -> Result<String> {
    if !valid_topic(topic) {
        bail!("topic {topic:?} is not kebab-case-safe (use lowercase a-z, 0-9, single hyphens)");
    }
    let title = one_line("--title", title)?;
    let items = items
        .iter()
        .map(|item| one_line("--item", item))
        .collect::<Result<Vec<_>>>()?;
    let dir = resolve_dir(&ctx.cwd)?;
    if let Some(plan) = follows {
        named_artifact(&dir, plan, "plan", "--follows")?;
    }
    let mut resolved_sources: BTreeMap<usize, ItemSource> = BTreeMap::new();
    for (&index, spec) in sources {
        if index >= items.len() {
            bail!(
                "--from names item i{}, but start declared only {} item(s)",
                index + 1,
                items.len()
            );
        }
        resolved_sources.insert(index, resolve_source(&dir, spec, "--from")?);
    }
    let now = Utc::now();
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let _lock = lock_journal_transition(&dir)?;
    let filename = super::allocate_artifact_name(&dir, &format!("{stamp}-{topic}-{KIND}.md"))?;
    let mut body = format!("# {title}\n\n");
    if let Some(plan) = follows {
        body.push_str(&format!("Follows `{plan}`.\n\n"));
    }
    body.push_str(&format!(
        "A progress series. Its items are journal events; read them with `arc progress {filename}`.\n"
    ));
    create_artifact(&dir, &dir.join(&filename), &body)?;
    let mut note = JournalEvent::base(ctx, now, topic, "note");
    note.file = Some(filename.clone());
    note.title = Some(title);
    note.series.follows = follows.map(str::to_string);
    append_event(ctx, &dir, &note)?;
    for (index, item) in items.iter().enumerate() {
        let mut event = JournalEvent::base(ctx, now, topic, "item-added");
        event.file = Some(filename.clone());
        event.title = Some(item.clone());
        event.series.item = Some(format!("i{}", index + 1));
        event.series.initial = true;
        event.series.item_source = resolved_sources.get(&index).cloned();
        append_event(ctx, &dir, &event)?;
    }
    Ok(filename)
}

/// Add an item; returns its id.
pub(crate) fn add(
    ctx: &Ctx,
    name: &str,
    title: &str,
    placement: Placement,
    from: Option<&str>,
) -> Result<String> {
    let open = open_for_write(ctx, name)?;
    let source = from
        .map(|spec| resolve_source(&open.dir, spec, "--from"))
        .transpose()?;
    let id = format!("i{}", open.series.next_id());
    let mut event = open.event(ctx, "item-added", Some(&id));
    event.title = Some(one_line("the title", title)?);
    event.series.item_source = source;
    placement.apply(&open.series, &mut event, None)?;
    open.append(ctx, &event)?;
    Ok(id)
}

pub(crate) fn rename(ctx: &Ctx, name: &str, id: &str, title: &str) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    let title = one_line("the title", title)?;
    if open.series.require_item(id)?.title == title {
        bail!("item {id} is already titled {title:?}");
    }
    let mut event = open.event(ctx, "item-renamed", Some(id));
    event.title = Some(title);
    open.append(ctx, &event)
}

pub(crate) fn move_item(ctx: &Ctx, name: &str, id: &str, placement: Placement) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    open.series.require_item(id)?;
    if matches!(placement, Placement::End) {
        bail!("say where the item goes: --before or --after another item");
    }
    let mut event = open.event(ctx, "item-moved", Some(id));
    placement.apply(&open.series, &mut event, Some(id))?;
    open.append(ctx, &event)
}

pub(crate) fn drop_item(ctx: &Ctx, name: &str, id: &str, reason: &str) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    open.standing(id)?;
    let mut event = open.event(ctx, "item-dropped", Some(id));
    event.note = Some(one_line("--reason", reason)?);
    open.append(ctx, &event)
}

pub(crate) fn done(
    ctx: &Ctx,
    name: &str,
    id: &str,
    resolved_by: Option<ResolvedBy>,
    note: Option<&str>,
) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    let item = open.standing(id)?;
    if let Some(change) = &item.change {
        bail!("item {id} follows change {change}; its stage is the ledger's, so unlink it first");
    }
    if item.state == ItemState::Done {
        bail!("item {id} is already done");
    }
    let mut event = open.event(ctx, "item-done", Some(id));
    event.series.resolved_by = resolved_by;
    event.note = note.map(|note| one_line("--note", note)).transpose()?;
    open.append(ctx, &event)
}

/// Bind an item to its originating journal artifact (`todo`, `feature-request`,
/// `plan`, or `handoff`), replacing any earlier source link.
pub(crate) fn set_source(ctx: &Ctx, name: &str, id: &str, spec: &str) -> Result<ItemSource> {
    let open = open_for_write(ctx, name)?;
    let item = open.standing(id)?;
    let source = resolve_source(&open.dir, spec, "source")?;
    if item.source.as_ref() == Some(&source) {
        bail!("item {id} already links source {source}");
    }
    let mut event = open.event(ctx, "item-sourced", Some(id));
    event.series.item_source = Some(source.clone());
    open.append(ctx, &event)?;
    Ok(source)
}

/// Remove an item's originating source link; returns it.
pub(crate) fn clear_source(ctx: &Ctx, name: &str, id: &str) -> Result<ItemSource> {
    let open = open_for_write(ctx, name)?;
    let Some(source) = open.series.require_item(id)?.source.clone() else {
        bail!("item {id} has no source artifact link");
    };
    open.append(ctx, &open.event(ctx, "item-unsourced", Some(id)))?;
    Ok(source)
}

pub(crate) fn reopen(ctx: &Ctx, name: &str, id: &str, note: Option<&str>) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    let item = open.series.require_item(id)?;
    if !matches!(item.state, ItemState::Done | ItemState::Dropped) {
        bail!(
            "item {id} is {}; only a done or dropped item reopens",
            state_word(item.state)
        );
    }
    let mut event = open.event(ctx, "item-reopened", Some(id));
    event.note = note.map(|note| one_line("--note", note)).transpose()?;
    open.append(ctx, &event)
}

/// The item a link would act on, for a caller that checks the ledger first.
pub(crate) fn linkable(
    ctx: &Ctx,
    name: &str,
    id: &str,
) -> Result<(String, Option<ItemLink>, Option<ItemSource>)> {
    let dir = resolve_dir(&ctx.cwd)?;
    let events = super::read_events(&dir)?;
    let file = resolve_name(&events, name, &dir)?;
    let series = replay(&events, &file).with_context(|| format!("{file} was never started"))?;
    if series.retired.is_some() {
        bail!("{file} was retired; it is read-only");
    }
    let item = series.require_item(id)?;
    if !item.stands() || item.state == ItemState::Done {
        bail!("item {id} of {file} is {}", state_word(item.state));
    }
    Ok((file, item.link(), item.source.clone()))
}

/// Link an item to a change. `expected` is the link the caller read before
/// taking the journal's lock, and `replaceable` whether the ledger lets that
/// link be replaced. A link that changed in between is refused, so a
/// concurrent link is never replaced on a stale reading.
pub(crate) fn link(
    ctx: &Ctx,
    name: &str,
    id: &str,
    link: &ItemLink,
    expected: Option<&ItemLink>,
    expected_source: Option<Option<&ItemSource>>,
    replaceable: bool,
) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    let item = open.standing(id)?;
    let current = item.link();
    if current.as_ref() != expected {
        bail!(
            "item {id} is linked to {} since this command read it; read the series again",
            current
                .as_ref()
                .map_or_else(|| "nothing".to_string(), ToString::to_string)
        );
    }
    if let Some(expected_source) = expected_source {
        if item.source.as_ref() != expected_source {
            bail!(
                "item {id} changed its source to {} since this command read it; read the series again",
                item.source
                    .as_ref()
                    .map_or_else(|| "nothing".to_string(), ToString::to_string)
            );
        }
    }
    if item.state == ItemState::Done {
        bail!("item {id} is done; reopen it before linking a change");
    }
    match current {
        Some(current) if &current == link => bail!("item {id} is already linked to {link}"),
        Some(current) if !replaceable => bail!(
            "item {id} is linked to change {current}, which is still open or integrated; \
             unlink it first"
        ),
        _ => {}
    }
    let mut event = open.event(ctx, "item-linked", Some(id));
    event.series.change = Some(link.change.clone());
    event.series.change_project = link.project.clone();
    open.append(ctx, &event)
}

pub(crate) fn unlink(ctx: &Ctx, name: &str, id: &str) -> Result<ItemLink> {
    let open = open_for_write(ctx, name)?;
    let Some(link) = open.series.require_item(id)?.link() else {
        bail!("item {id} is not linked to a change");
    };
    open.append(ctx, &open.event(ctx, "item-unlinked", Some(id)))?;
    Ok(link)
}

/// Record that a standing item waits on a question of this journal. The
/// discussion is named by its bare filename and must hold the question,
/// asked and still open, for a person to settle; both are checked before
/// anything is written. A wait replaces the item's earlier one.
pub(crate) fn wait(ctx: &Ctx, name: &str, id: &str, file: &str, question: &str) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    let item = open.standing(id)?;
    named_artifact(&open.dir, file, "discussion", "wait")?;
    let target = ItemWait {
        file: file.to_string(),
        question: question.to_string(),
    };
    if item.waits_on.as_ref() == Some(&target) {
        bail!("item {id} already waits on {question} of {file}");
    }
    let asked = open.events.iter().any(|event| {
        event.event == "question"
            && event.known()
            && event.file.as_deref() == Some(file)
            && event.question_id.as_deref() == Some(question)
    });
    if !asked {
        bail!("{file} holds no question {question}");
    }
    let waiting = super::questions_from(&open.dir, &open.events, false)?
        .into_iter()
        .find(|open| open.file == file && open.question == question);
    match waiting {
        None => bail!("question {question} of {file} is answered; nothing waits on it"),
        Some(open) if !waits_on_person(&open) => bail!(
            "question {question} of {file} is settled by {}, not by a person",
            open.settle_by.as_deref().unwrap_or_default()
        ),
        Some(_) => {}
    }
    let mut event = open.event(ctx, "item-waiting", Some(id));
    event.series.waits_on = Some(target);
    open.append(ctx, &event)
}

/// Remove the question an item was said to wait on; returns it.
pub(crate) fn unwait(ctx: &Ctx, name: &str, id: &str) -> Result<ItemWait> {
    let open = open_for_write(ctx, name)?;
    let Some(wait) = open.series.require_item(id)?.waits_on.clone() else {
        bail!("item {id} waits on no question");
    };
    open.append(ctx, &open.event(ctx, "item-unwaited", Some(id)))?;
    Ok(wait)
}

/// Split one item into several; returns the new items' ids.
pub(crate) fn split(
    ctx: &Ctx,
    name: &str,
    id: &str,
    titles: &[String],
    reason: &str,
) -> Result<Vec<String>> {
    let open = open_for_write(ctx, name)?;
    let item = open.standing(id)?;
    if let Some(change) = &item.change {
        bail!("item {id} follows change {change}; unlink it before splitting it");
    }
    if titles.len() < 2 {
        bail!("a split names at least two items with --into");
    }
    let first = open.series.next_id();
    let parts = titles
        .iter()
        .enumerate()
        .map(|(offset, title)| {
            Ok(SplitPart {
                item: format!("i{}", first + offset as u64),
                title: one_line("--into", title)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut event = open.event(ctx, "item-split", Some(id));
    event.note = Some(one_line("--reason", reason)?);
    event.series.parts = parts.clone();
    open.append(ctx, &event)?;
    Ok(parts.into_iter().map(|part| part.item).collect())
}

/// Record that a series relates to one in another project.
pub(crate) fn relate(ctx: &Ctx, name: &str, related: RelatedSeries) -> Result<bool> {
    let open = open_for_write(ctx, name)?;
    if open.series.related.contains(&related) {
        return Ok(false);
    }
    let mut event = open.event(ctx, "series-related", None);
    event.series.related = Some(related);
    open.append(ctx, &event)?;
    Ok(true)
}

/// Remove a relation the series records.
pub(crate) fn unrelate(ctx: &Ctx, name: &str, related: RelatedSeries) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    if !open.series.related.contains(&related) {
        bail!(
            "{} does not relate to {}::{}",
            open.series.file,
            related.project,
            related.file
        );
    }
    let mut event = open.event(ctx, "series-unrelated", None);
    event.series.related = Some(related);
    open.append(ctx, &event)
}

/// Retire a series as superseded by its conclusion.
pub(crate) fn retire(ctx: &Ctx, name: &str, conclusion: &str) -> Result<String> {
    let open = open_for_write(ctx, name)?;
    let path = named_artifact(&open.dir, conclusion, "conclusion", "--by")?;
    let digest = super::body_digest(&path)?;
    let mut event = open.event(ctx, "consumed", None);
    event.outcome = Some("superseded".to_string());
    event.note = Some(format!("retired by {conclusion}"));
    event.decision = Some(conclusion.to_string());
    event.decision_kind = Some("conclusion".to_string());
    event.decision_digest = Some(digest);
    open.append(ctx, &event)?;
    Ok(open.series.file.clone())
}

/// Open questions among the events of the journal at `dir` that a person
/// must settle.
pub(crate) fn person_questions_in(
    dir: &Path,
    events: &[JournalEvent],
) -> Result<Vec<super::OpenQuestion>> {
    Ok(super::questions_from(dir, events, false)?
        .into_iter()
        .filter(waits_on_person)
        .collect())
}

fn waits_on_person(question: &super::OpenQuestion) -> bool {
    question
        .settle_by
        .as_deref()
        .is_none_or(|by| by == "person")
}
