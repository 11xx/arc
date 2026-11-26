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
use std::collections::{BTreeMap, BTreeSet};
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
    /// On the `note` event that started a series: whether declared waves act
    /// as readiness barriers by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    gated_waves: bool,
    /// Wave identifier on `wave-*`, `item-added`, `item-waved`, or `item-split`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wave: Option<String>,
    /// Optional human title of a wave on `wave-declared` or `wave-renamed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wave_title: Option<String>,
    /// Wave placement anchor on `wave-declared` or `wave-moved`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wave_before: Option<String>,
    /// Wave placement anchor on `wave-declared` or `wave-moved`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wave_after: Option<String>,
    /// Explicit barrier setting on `wave-declared`; `None` inherits `gated_waves`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wave_gated: Option<bool>,
    /// Prerequisite item on `item-depended` or `item-undepended`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depends_on: Option<ItemPrereq>,
    /// Destination item on `item-transferred`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transferred_to: Option<ItemRef>,
    /// Origin item on an `item-added` created by a transfer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transferred_from: Option<ItemRef>,
    /// Durable transfer key persisted on `item-transfer-pending` in the source
    /// journal and echoed on the destination `item-added` so cross-project
    /// transfer retries bind to the exact originating item across journal
    /// relocations and slug renames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transfer_key: Option<String>,
    /// Intended cross-project destination recorded on `item-transfer-pending`
    /// so retargeting fails closed if that destination is no longer registered
    /// or inspectable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transfer_target: Option<RelatedSeries>,
    /// Durable digest of the intended destination journal directory and series
    /// start event recorded on `item-transfer-pending` so slug rebinding onto
    /// another journal with the same series filename cannot duplicate an
    /// interrupted transfer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transfer_target_digest: Option<String>,
    /// The series in another project a `series-related` event names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    related: Option<RelatedSeries>,
}

/// Another series' item referenced by a transfer (`transferred_to` or
/// `transferred_from`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ItemRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
    pub(crate) series: String,
    pub(crate) item: String,
}

impl std::fmt::Display for ItemRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.project {
            Some(project) => write!(f, "{project}::{}:{}", self.series, self.item),
            None => write!(f, "{}:{}", self.series, self.item),
        }
    }
}

/// One declared wave on a progress series.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Wave {
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) gated: bool,
}

/// An explicit prerequisite edge from an item to another item: same series
/// (`series: None, project: None`), another local series (`series: Some(_)`),
/// or another registered project's series (`project: Some(_), series: Some(_)`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ItemPrereq {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) series: Option<String>,
    pub(crate) item: String,
}

impl std::fmt::Display for ItemPrereq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.project, &self.series) {
            (Some(project), Some(series)) => write!(f, "{project}::{series}:{}", self.item),
            (None, Some(series)) => write!(f, "{series}:{}", self.item),
            (Some(project), None) => write!(f, "{project}::{}", self.item),
            (None, None) => f.write_str(&self.item),
        }
    }
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

pub(super) fn single_line(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains(['\n', '\r'])
}

pub(crate) fn valid_item_id(value: &str) -> bool {
    value
        .strip_prefix('i')
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

/// A wave identifier: lowercase ASCII `[a-z0-9]` with optional internal `-`
/// or `_`, 1..=32 characters, never starting or ending with a separator.
pub(crate) fn valid_wave_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes[0].is_ascii_lowercase() | bytes[0].is_ascii_digit()
        && bytes[bytes.len() - 1].is_ascii_lowercase() | bytes[bytes.len() - 1].is_ascii_digit()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

fn valid_prereq(prereq: &ItemPrereq) -> bool {
    valid_item_id(&prereq.item)
        && prereq.project.as_deref().is_none_or(single_line)
        && match (&prereq.project, &prereq.series) {
            (Some(_), None) => false,
            (_, Some(file)) => is_series_file(file),
            (None, None) => true,
        }
}

fn valid_item_ref(reference: &ItemRef) -> bool {
    valid_item_id(&reference.item)
        && is_series_file(&reference.series)
        && reference.project.as_deref().is_none_or(single_line)
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
    let no_wave_or_dep = fields.wave.is_none()
        && fields.wave_title.is_none()
        && fields.wave_before.is_none()
        && fields.wave_after.is_none()
        && fields.wave_gated.is_none()
        && fields.depends_on.is_none();
    let no_link_or_wait = fields.parts.is_empty()
        && fields.change.is_none()
        && fields.change_project.is_none()
        && fields.waits_on.is_none()
        && fields.related.is_none();
    let no_transfer = fields.transferred_to.is_none()
        && fields.transferred_from.is_none()
        && fields.transfer_key.is_none()
        && fields.transfer_target.is_none()
        && fields.transfer_target_digest.is_none();
    let bare = no_link_or_wait
        && no_wave_or_dep
        && no_transfer
        && fields.item_source.is_none()
        && fields.resolved_by.is_none();
    if !on_series {
        return false;
    }
    let wave_placements = [&fields.wave_before, &fields.wave_after]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let wave_placed = wave_placements.iter().all(|anchor| valid_wave_id(anchor));
    match event.event.as_str() {
        "item-added" => {
            names_item
                && line(&event.title)
                && placements.len() <= 1
                && placed
                && no_link_or_wait
                && fields.resolved_by.is_none()
                && fields.wave_title.is_none()
                && fields.wave_before.is_none()
                && fields.wave_after.is_none()
                && fields.wave_gated.is_none()
                && fields.depends_on.is_none()
                && fields.wave.as_deref().is_none_or(valid_wave_id)
                && fields.item_source.as_ref().is_none_or(valid_item_source)
                && fields.transferred_to.is_none()
                && fields.transferred_from.as_ref().is_none_or(valid_item_ref)
                && fields
                    .transfer_key
                    .as_deref()
                    .is_none_or(|key| fields.transferred_from.is_some() && single_line(key))
                && fields.transfer_target.is_none()
                && fields.transfer_target_digest.is_none()
        }
        "item-renamed" => names_item && line(&event.title) && placements.is_empty() && bare,
        "item-moved" => names_item && placements.len() == 1 && placed && bare,
        "item-dropped" => names_item && line(&event.note) && placements.is_empty() && bare,
        "item-transfer-pending" => {
            names_item
                && event.note.is_none()
                && placements.is_empty()
                && no_link_or_wait
                && no_wave_or_dep
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.transferred_to.is_none()
                && fields.transferred_from.is_none()
                && fields.transfer_key.as_deref().is_some_and(single_line)
                && fields
                    .transfer_target
                    .as_ref()
                    .is_none_or(|t| single_line(&t.project) && is_series_file(&t.file))
                && fields
                    .transfer_target_digest
                    .as_deref()
                    .is_none_or(single_line)
        }
        "item-transferred" => {
            names_item
                && line(&event.note)
                && placements.is_empty()
                && no_link_or_wait
                && no_wave_or_dep
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.transferred_from.is_none()
                && fields.transfer_key.is_none()
                && fields.transfer_target.is_none()
                && fields.transfer_target_digest.is_none()
                && fields.transferred_to.as_ref().is_some_and(valid_item_ref)
        }
        "item-done" => {
            names_item
                && event.note.as_deref().is_none_or(single_line)
                && placements.is_empty()
                && no_link_or_wait
                && no_wave_or_dep
                && no_transfer
                && fields.item_source.is_none()
                && fields.resolved_by.as_ref().is_none_or(valid_resolved_by)
        }
        "item-reopened" | "item-unlinked" | "item-unwaited" | "item-unsourced" | "item-unwaved" => {
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
                && no_wave_or_dep
                && no_transfer
                && fields.resolved_by.is_none()
                && fields.item_source.as_ref().is_some_and(valid_item_source)
        }
        "item-waved" => {
            names_item
                && event.note.is_none()
                && placements.is_empty()
                && no_link_or_wait
                && no_transfer
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.wave_title.is_none()
                && fields.wave_before.is_none()
                && fields.wave_after.is_none()
                && fields.wave_gated.is_none()
                && fields.depends_on.is_none()
                && fields.wave.as_deref().is_some_and(valid_wave_id)
        }
        "item-depended" | "item-undepended" => {
            names_item
                && event.note.is_none()
                && placements.is_empty()
                && no_link_or_wait
                && no_transfer
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.wave.is_none()
                && fields.wave_title.is_none()
                && fields.wave_before.is_none()
                && fields.wave_after.is_none()
                && fields.wave_gated.is_none()
                && fields.depends_on.as_ref().is_some_and(valid_prereq)
        }
        "wave-declared" => {
            fields.item.is_none()
                && placements.is_empty()
                && no_link_or_wait
                && no_transfer
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.depends_on.is_none()
                && fields.wave.as_deref().is_some_and(valid_wave_id)
                && fields.wave_title.as_deref().is_none_or(single_line)
                && wave_placements.len() <= 1
                && wave_placed
        }
        "wave-renamed" => {
            fields.item.is_none()
                && placements.is_empty()
                && no_link_or_wait
                && no_transfer
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.depends_on.is_none()
                && fields.wave.as_deref().is_some_and(valid_wave_id)
                && line(&fields.wave_title)
                && wave_placements.is_empty()
                && fields.wave_gated.is_none()
        }
        "wave-moved" => {
            fields.item.is_none()
                && placements.is_empty()
                && no_link_or_wait
                && no_transfer
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.depends_on.is_none()
                && fields.wave.as_deref().is_some_and(valid_wave_id)
                && fields.wave_title.is_none()
                && wave_placements.len() == 1
                && wave_placed
                && fields.wave_gated.is_none()
        }
        "wave-removed" => {
            fields.item.is_none()
                && placements.is_empty()
                && no_link_or_wait
                && no_transfer
                && fields.item_source.is_none()
                && fields.resolved_by.is_none()
                && fields.depends_on.is_none()
                && fields.wave.as_deref().is_some_and(valid_wave_id)
                && fields.wave_title.is_none()
                && wave_placements.is_empty()
                && fields.wave_gated.is_none()
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
                && no_wave_or_dep
                && no_transfer
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
                && no_wave_or_dep
                && no_transfer
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
                && fields.wave_title.is_none()
                && fields.wave_before.is_none()
                && fields.wave_after.is_none()
                && fields.wave_gated.is_none()
                && fields.depends_on.is_none()
                && no_transfer
                && fields.wave.as_deref().is_none_or(valid_wave_id)
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
                && no_wave_or_dep
                && no_transfer
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
        "item-waved" => format!(
            "assigned item {item} of {file} to wave {}",
            fields.wave.as_deref().unwrap_or_default()
        ),
        "item-unwaved" => format!("removed wave assignment from item {item} of {file}"),
        "item-depended" => format!(
            "item {item} of {file} depends on {}",
            fields
                .depends_on
                .as_ref()
                .map_or_else(String::new, ToString::to_string)
        ),
        "item-undepended" => format!(
            "removed prerequisite {} from item {item} of {file}",
            fields
                .depends_on
                .as_ref()
                .map_or_else(String::new, ToString::to_string)
        ),
        "wave-declared" => format!(
            "declared wave {} on {file}{}",
            fields.wave.as_deref().unwrap_or_default(),
            fields
                .wave_title
                .as_deref()
                .map(|title| format!(": {title}"))
                .unwrap_or_default()
        ),
        "wave-renamed" => format!(
            "renamed wave {} of {file}: {}",
            fields.wave.as_deref().unwrap_or_default(),
            fields.wave_title.as_deref().unwrap_or_default()
        ),
        "wave-moved" => format!(
            "moved wave {} of {file}",
            fields.wave.as_deref().unwrap_or_default()
        ),
        "wave-removed" => format!(
            "removed wave {} from {file}",
            fields.wave.as_deref().unwrap_or_default()
        ),
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
        "item-transfer-pending" => format!("pending transfer of item {item} of {file}"),
        "item-transferred" => format!(
            "transferred item {item} of {file} to {}: {note}",
            fields
                .transferred_to
                .as_ref()
                .map_or_else(String::new, ToString::to_string)
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
    Transferred,
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
    /// Optional wave id this item is assigned to.
    pub(crate) wave: Option<String>,
    /// Explicit prerequisite items that must complete before this item is ready.
    pub(crate) depends_on: Vec<ItemPrereq>,
    /// The reason or note recorded with the item's latest state.
    pub(crate) note: Option<String>,
    /// When the item reached its latest state.
    pub(crate) state_at: Option<String>,
    /// For a split item, the items that replaced it.
    pub(crate) split_into: Vec<String>,
    /// For a transferred item, the destination series item it moved to.
    pub(crate) transferred_to: Option<ItemRef>,
    /// For an item created by transfer, the source series item it came from.
    pub(crate) transferred_from: Option<ItemRef>,
    /// Durable transfer key recorded on transferred items for retry matching.
    pub(crate) transfer_key: Option<String>,
    /// Durable transfer key recorded on an in-flight cross-project transfer
    /// before destination creation so retries match across journal relocations.
    pub(crate) pending_transfer_key: Option<String>,
    /// Intended cross-project destination of an in-flight transfer.
    pub(crate) pending_transfer_target: Option<RelatedSeries>,
    /// Durable digest of the intended destination journal directory and series
    /// creation event for an in-flight transfer.
    pub(crate) pending_transfer_target_digest: Option<String>,
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

    /// Whether the item counts in the list as it stands. A dropped, split, or
    /// transferred item stays visible and leaves the count.
    pub(crate) fn stands(&self) -> bool {
        !matches!(
            self.state,
            ItemState::Dropped | ItemState::Split | ItemState::Transferred
        )
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
    /// Whether waves declared on this series act as barriers by default.
    pub(crate) gated_waves: bool,
    /// Ordered waves declared on this series.
    pub(crate) waves: Vec<Wave>,
    pub(crate) items: Vec<Item>,
    pub(crate) related: Vec<RelatedSeries>,
    /// Items added after the start, counted from the events rather than from
    /// the items left, so an added item later dropped still counts as added.
    added: usize,
}

fn is_zero_count(value: &usize) -> bool {
    *value == 0
}

/// What changed in the list since the call that started the series.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct SinceStart {
    /// Items declared when the series started.
    pub(crate) declared: usize,
    pub(crate) added: usize,
    pub(crate) dropped: usize,
    pub(crate) split: usize,
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub(crate) transferred: usize,
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
                ItemState::Transferred => since.transferred += 1,
                _ => {}
            }
        }
        since.added = self.added;
        since
    }

    pub(crate) fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }

    pub(crate) fn wave(&self, id: &str) -> Option<&Wave> {
        self.waves.iter().find(|wave| wave.id == id)
    }

    fn require_wave(&self, id: &str) -> Result<&Wave> {
        self.wave(id).with_context(|| {
            format!(
                "{} has no declared wave {id:?}; declare one with `arc progress waves add {} {id}`",
                self.file, self.file
            )
        })
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

fn place_wave(waves: &mut Vec<Wave>, wave: Wave, before: Option<&str>, after: Option<&str>) {
    let anchor = before
        .map(|id| (id, 0))
        .or_else(|| after.map(|id| (id, 1)))
        .and_then(|(id, offset)| {
            waves
                .iter()
                .position(|candidate| candidate.id == id)
                .map(|at| at + offset)
        });
    match anchor {
        Some(at) => waves.insert(at, wave),
        None => waves.push(wave),
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
                gated_waves: event.series.gated_waves,
                waves: Vec::new(),
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
            "wave-declared" => {
                let Some(wave_id) = fields.wave.as_deref() else {
                    continue;
                };
                if current.wave(wave_id).is_some() {
                    continue;
                }
                let wave = Wave {
                    id: wave_id.to_string(),
                    title: fields.wave_title.clone(),
                    gated: fields.wave_gated.unwrap_or(current.gated_waves),
                };
                place_wave(
                    &mut current.waves,
                    wave,
                    fields.wave_before.as_deref(),
                    fields.wave_after.as_deref(),
                );
            }
            "wave-renamed" => {
                if let Some(wave_id) = fields.wave.as_deref() {
                    if let Some(wave) = current.waves.iter_mut().find(|w| w.id == wave_id) {
                        wave.title = fields.wave_title.clone();
                    }
                }
            }
            "wave-moved" => {
                let Some(wave_id) = fields.wave.as_deref() else {
                    continue;
                };
                let anchor = fields
                    .wave_before
                    .as_deref()
                    .or(fields.wave_after.as_deref());
                if anchor == Some(wave_id) || anchor.is_none_or(|a| current.wave(a).is_none()) {
                    continue;
                }
                if let Some(index) = current.waves.iter().position(|w| w.id == wave_id) {
                    let wave = current.waves.remove(index);
                    place_wave(
                        &mut current.waves,
                        wave,
                        fields.wave_before.as_deref(),
                        fields.wave_after.as_deref(),
                    );
                }
            }
            "wave-removed" => {
                if let Some(wave_id) = fields.wave.as_deref() {
                    current.waves.retain(|w| w.id != wave_id);
                }
            }
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
                    wave: fields.wave.clone(),
                    depends_on: Vec::new(),
                    note: None,
                    state_at: at,
                    split_into: Vec::new(),
                    transferred_to: None,
                    transferred_from: fields.transferred_from.clone(),
                    transfer_key: fields.transfer_key.clone(),
                    pending_transfer_key: None,
                    pending_transfer_target: None,
                    pending_transfer_target_digest: None,
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
                let inherited_wave = fields.wave.clone().or_else(|| original.wave.clone());
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
                            wave: inherited_wave.clone(),
                            depends_on: Vec::new(),
                            note: None,
                            state_at: at.clone(),
                            split_into: Vec::new(),
                            transferred_to: None,
                            transferred_from: None,
                            transfer_key: None,
                            pending_transfer_key: None,
                            pending_transfer_target: None,
                            pending_transfer_target_digest: None,
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
            "reopened" => {
                current.retired = None;
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
                    "item-transfer-pending" => {
                        item.pending_transfer_key = fields.transfer_key.clone();
                        item.pending_transfer_target = fields.transfer_target.clone();
                        item.pending_transfer_target_digest = fields.transfer_target_digest.clone();
                    }
                    "item-transferred" => {
                        item.state = ItemState::Transferred;
                        item.transferred_to = fields.transferred_to.clone();
                        item.pending_transfer_key = None;
                        item.pending_transfer_target = None;
                        item.pending_transfer_target_digest = None;
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
                    "item-waved" => item.wave = fields.wave.clone(),
                    "item-unwaved" => item.wave = None,
                    "item-depended" => {
                        if let Some(prereq) = &fields.depends_on {
                            if !item.depends_on.contains(prereq) {
                                item.depends_on.push(prereq.clone());
                            }
                        }
                    }
                    "item-undepended" => {
                        if let Some(prereq) = &fields.depends_on {
                            item.depends_on.retain(|known| known != prereq);
                        }
                    }
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
    open_under_lock(dir, lock, name)
}

fn open_under_lock(
    dir: std::path::PathBuf,
    lock: super::JournalTransitionLock,
    name: &str,
) -> Result<Open> {
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

    fn append_batch(&self, ctx: &Ctx, events: &[JournalEvent]) -> Result<()> {
        super::append_events(ctx, &self.dir, events)
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
        ItemState::Transferred => "transferred",
    }
}

pub(super) fn one_line(what: &str, value: &str) -> Result<String> {
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
pub(super) fn named_artifact(
    dir: &Path,
    name: &str,
    kind: &str,
    flag: &str,
) -> Result<std::path::PathBuf> {
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

/// A wave declaration passed to `start`.
pub(crate) struct WaveSpec {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) gated: Option<bool>,
}

/// Start a series: write its artifact and declare its first items.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start(
    ctx: &Ctx,
    topic: &str,
    title: &str,
    follows: Option<&str>,
    items: &[String],
    sources: &BTreeMap<usize, String>,
    gated_waves: bool,
    waves: &[WaveSpec],
    item_waves: &BTreeMap<usize, String>,
) -> Result<String> {
    if !valid_topic(topic) {
        bail!("topic {topic:?} is not kebab-case-safe (use lowercase a-z, 0-9, single hyphens)");
    }
    let title = one_line("--title", title)?;
    let items = items
        .iter()
        .map(|item| one_line("--item", item))
        .collect::<Result<Vec<_>>>()?;
    let mut seen_waves = BTreeSet::new();
    let mut validated_waves = Vec::with_capacity(waves.len());
    for spec in waves {
        if !valid_wave_id(&spec.id) {
            bail!(
                "wave id {:?} must be 1..=32 lowercase ASCII letters/digits with internal `-` or `_`",
                spec.id
            );
        }
        if !seen_waves.insert(spec.id.clone()) {
            bail!("wave {:?} declared more than once", spec.id);
        }
        let wave_title = spec
            .title
            .as_deref()
            .map(|t| one_line("--wave title", t))
            .transpose()?;
        validated_waves.push((spec.id.clone(), wave_title, spec.gated));
    }
    for (&index, wave_id) in item_waves {
        if index >= items.len() {
            bail!(
                "--item-wave names item i{}, but start declared only {} item(s)",
                index + 1,
                items.len()
            );
        }
        if !seen_waves.contains(wave_id) {
            bail!(
                "--item-wave names undeclared wave {wave_id:?}; declare it with --wave {wave_id}"
            );
        }
    }
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
    note.series.gated_waves = gated_waves;
    append_event(ctx, &dir, &note)?;
    for (wave_id, wave_title, wave_gated) in validated_waves {
        let mut event = JournalEvent::base(ctx, now, topic, "wave-declared");
        event.file = Some(filename.clone());
        event.series.wave = Some(wave_id);
        event.series.wave_title = wave_title;
        event.series.wave_gated = wave_gated;
        append_event(ctx, &dir, &event)?;
    }
    for (index, item) in items.iter().enumerate() {
        let mut event = JournalEvent::base(ctx, now, topic, "item-added");
        event.file = Some(filename.clone());
        event.title = Some(item.clone());
        event.series.item = Some(format!("i{}", index + 1));
        event.series.initial = true;
        event.series.item_source = resolved_sources.get(&index).cloned();
        event.series.wave = item_waves.get(&index).cloned();
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
    wave: Option<&str>,
    depends_on: &[String],
) -> Result<String> {
    let open = open_for_write(ctx, name)?;
    let source = from
        .map(|spec| resolve_source(&open.dir, spec, "--from"))
        .transpose()?;
    if let Some(wave_id) = wave {
        open.series.require_wave(wave_id)?;
    }
    let mut prereqs = Vec::new();
    for spec in depends_on {
        let prereq = resolve_prereq(&open.dir, &open.events, &open.series, spec)?;
        if !prereqs.contains(&prereq) {
            prereqs.push(prereq);
        }
    }
    let id = format!("i{}", open.series.next_id());
    let mut event = open.event(ctx, "item-added", Some(&id));
    event.title = Some(one_line("the title", title)?);
    event.series.item_source = source;
    event.series.wave = wave.map(str::to_string);
    placement.apply(&open.series, &mut event, None)?;
    let mut records = Vec::with_capacity(1 + prereqs.len());
    records.push(event);
    for prereq in prereqs {
        let mut dep = open.event(ctx, "item-depended", Some(&id));
        dep.series.depends_on = Some(prereq);
        records.push(dep);
    }
    open.append_batch(ctx, &records)?;
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
    link_in_open(
        ctx,
        &open.dir,
        &open.series,
        &open.topic,
        id,
        link,
        expected,
        expected_source,
        replaceable,
    )
}

/// Link an item to a change while the caller already holds the item journal's
/// transition lock (used by `arc begin --item` so validation, source promotion,
/// and attachment execute under one continuous lock).
#[allow(clippy::too_many_arguments)]
pub(crate) fn link_with_held_lock(
    ctx: &Ctx,
    name: &str,
    id: &str,
    link: &ItemLink,
    expected: Option<&ItemLink>,
    expected_source: Option<Option<&ItemSource>>,
    replaceable: bool,
    _held: &super::JournalTransitionLock,
) -> Result<()> {
    let dir = resolve_dir(&ctx.cwd)?;
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
    link_in_open(
        ctx,
        &dir,
        &series,
        &topic,
        id,
        link,
        expected,
        expected_source,
        replaceable,
    )
}

#[allow(clippy::too_many_arguments)]
fn link_in_open(
    ctx: &Ctx,
    dir: &Path,
    series: &Series,
    topic: &str,
    id: &str,
    link: &ItemLink,
    expected: Option<&ItemLink>,
    expected_source: Option<Option<&ItemSource>>,
    replaceable: bool,
) -> Result<()> {
    let item = series.require_item(id)?;
    if !item.stands() {
        bail!(
            "item {id} of {} is {}; it no longer stands in the list",
            series.file,
            state_word(item.state)
        );
    }
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
    let mut event = JournalEvent::base(ctx, Utc::now(), topic, "item-linked");
    event.file = Some(series.file.clone());
    event.series.item = Some(id.to_string());
    event.series.change = Some(link.change.clone());
    event.series.change_project = link.project.clone();
    append_event(ctx, dir, &event)
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
    wave: Option<&str>,
) -> Result<Vec<String>> {
    let open = open_for_write(ctx, name)?;
    let item = open.standing(id)?;
    if let Some(change) = &item.change {
        bail!("item {id} follows change {change}; unlink it before splitting it");
    }
    if let Some(wave_id) = wave {
        open.series.require_wave(wave_id)?;
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
    event.series.wave = wave.map(str::to_string);
    event.series.parts = parts.clone();
    open.append(ctx, &event)?;
    Ok(parts.into_iter().map(|part| part.item).collect())
}

/// Declare a wave on a series.
pub(crate) fn wave_declare(
    ctx: &Ctx,
    name: &str,
    id: &str,
    title: Option<&str>,
    before: Option<&str>,
    after: Option<&str>,
    gated: Option<bool>,
) -> Result<()> {
    if !valid_wave_id(id) {
        bail!(
            "wave id {id:?} must be 1..=32 lowercase ASCII letters/digits with internal `-` or `_`"
        );
    }
    let open = open_for_write(ctx, name)?;
    if open.series.wave(id).is_some() {
        bail!("{} already declares wave {id:?}", open.series.file);
    }
    if let Some(anchor) = before.or(after) {
        open.series.require_wave(anchor)?;
    }
    let mut event = open.event(ctx, "wave-declared", None);
    event.series.wave = Some(id.to_string());
    event.series.wave_title = title.map(|t| one_line("--title", t)).transpose()?;
    event.series.wave_before = before.map(str::to_string);
    event.series.wave_after = after.map(str::to_string);
    event.series.wave_gated = gated;
    open.append(ctx, &event)
}

/// Rename a declared wave's title.
pub(crate) fn wave_rename(ctx: &Ctx, name: &str, id: &str, title: &str) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    let wave = open.series.require_wave(id)?;
    let title = one_line("the title", title)?;
    if wave.title.as_deref() == Some(title.as_str()) {
        bail!("wave {id:?} is already titled {title:?}");
    }
    let mut event = open.event(ctx, "wave-renamed", None);
    event.series.wave = Some(id.to_string());
    event.series.wave_title = Some(title);
    open.append(ctx, &event)
}

/// Move a declared wave before or after another wave.
pub(crate) fn wave_move(
    ctx: &Ctx,
    name: &str,
    id: &str,
    before: Option<&str>,
    after: Option<&str>,
) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    open.series.require_wave(id)?;
    let Some(anchor) = before.or(after) else {
        bail!("say where the wave goes: --before or --after another wave");
    };
    open.series.require_wave(anchor)?;
    if anchor == id {
        bail!("a wave cannot be placed relative to itself");
    }
    let mut event = open.event(ctx, "wave-moved", None);
    event.series.wave = Some(id.to_string());
    event.series.wave_before = before.map(str::to_string);
    event.series.wave_after = after.map(str::to_string);
    open.append(ctx, &event)
}

/// Remove a declared wave; refused while any standing item is assigned to it.
pub(crate) fn wave_remove(ctx: &Ctx, name: &str, id: &str) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    open.series.require_wave(id)?;
    let assigned: Vec<&str> = open
        .series
        .items
        .iter()
        .filter(|item| item.stands() && item.wave.as_deref() == Some(id))
        .map(|item| item.id.as_str())
        .collect();
    if !assigned.is_empty() {
        bail!(
            "wave {id:?} still holds standing item(s) {}; move or unwave them first",
            assigned.join(", ")
        );
    }
    let mut event = open.event(ctx, "wave-removed", None);
    event.series.wave = Some(id.to_string());
    open.append(ctx, &event)
}

/// Assign a standing item to a declared wave.
pub(crate) fn set_wave(ctx: &Ctx, name: &str, id: &str, wave_id: &str) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    let item = open.standing(id)?;
    open.series.require_wave(wave_id)?;
    if item.wave.as_deref() == Some(wave_id) {
        bail!("item {id} is already in wave {wave_id:?}");
    }
    let mut event = open.event(ctx, "item-waved", Some(id));
    event.series.wave = Some(wave_id.to_string());
    open.append(ctx, &event)
}

/// Remove an item's wave assignment; returns the removed wave id.
pub(crate) fn clear_wave(ctx: &Ctx, name: &str, id: &str) -> Result<String> {
    let open = open_for_write(ctx, name)?;
    let Some(wave_id) = open.series.require_item(id)?.wave.clone() else {
        bail!("item {id} is not assigned to a wave");
    };
    open.append(ctx, &open.event(ctx, "item-unwaved", Some(id)))?;
    Ok(wave_id)
}

/// Resolve a prerequisite reference: `i1` (same series), `<series>:i2` (same
/// journal), or `<project>::<series>:i3` (another registered project).
fn resolve_prereq(
    dir: &Path,
    events: &[JournalEvent],
    current: &Series,
    spec: &str,
) -> Result<ItemPrereq> {
    if let Some((prefix, rest)) = spec.split_once(super::REFERENCE_SEPARATOR) {
        let Some((series_name, item_id)) = rest.rsplit_once(':') else {
            bail!("cross-project prerequisite takes <project>::<series>:<item>; got {spec:?}");
        };
        let project = super::registered_project(prefix)?;
        let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if canonical(&project.journal_dir) == canonical(dir) {
            bail!("{prefix} is this project; name {series_name}:{item_id} without a project");
        }
        let remote = named_series(&project.journal_dir, series_name).with_context(|| {
            format!("cannot read series {series_name:?} in {}", project.label())
        })?;
        remote.require_item(item_id)?;
        return Ok(ItemPrereq {
            project: Some(project.slug),
            series: Some(remote.file),
            item: item_id.to_string(),
        });
    }
    if let Some((series_name, item_id)) = spec.rsplit_once(':') {
        let target = named_in(events, dir, series_name)?;
        target.require_item(item_id)?;
        if target.file == current.file {
            return Ok(ItemPrereq {
                project: None,
                series: None,
                item: item_id.to_string(),
            });
        }
        return Ok(ItemPrereq {
            project: None,
            series: Some(target.file),
            item: item_id.to_string(),
        });
    }
    if !valid_item_id(spec) {
        bail!(
            "prerequisite takes <item>, <series>:<item>, or <project>::<series>:<item>; got {spec:?}"
        );
    }
    current.require_item(spec)?;
    Ok(ItemPrereq {
        project: None,
        series: None,
        item: spec.to_string(),
    })
}

/// Look up a unique journal directory in the slug namespace alone so a stored
/// project slug never collides with another project's label.
fn unique_journal_by_slug<'a>(
    known: &'a [(String, std::path::PathBuf)],
    slug: &str,
) -> Option<&'a Path> {
    let mut matches = known
        .iter()
        .filter(|(entry_slug, _)| entry_slug == slug)
        .map(|(_, path)| path.as_path());
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Check whether adding `source -> target` creates a cycle in the reachable
/// prerequisite graph across this journal and any readable registered project.
/// Split items expand to their children just as readiness derivation does, so
/// depending on an ancestor item after a split is rejected as a cycle.
#[allow(clippy::too_many_arguments)]
fn check_no_cycle(
    dir: &Path,
    local_events: &[JournalEvent],
    local_slug: Option<&str>,
    known: &[(String, std::path::PathBuf)],
    source_series: &str,
    source_item: &str,
    target: &ItemPrereq,
) -> Result<()> {
    type Node = (Option<String>, String, String);
    let normalize_project = |project: Option<&str>| -> Option<String> {
        project
            .filter(|slug| Some(*slug) != local_slug)
            .map(str::to_string)
    };
    let origin: Node = (None, source_series.to_string(), source_item.to_string());
    let start: Node = (
        normalize_project(target.project.as_deref()),
        target
            .series
            .clone()
            .unwrap_or_else(|| source_series.to_string()),
        target.item.clone(),
    );
    if start == origin {
        bail!("item {source_item} cannot depend on itself");
    }
    let mut cache: BTreeMap<Option<String>, Option<Vec<JournalEvent>>> = BTreeMap::new();
    let mut visited: BTreeSet<Node> = BTreeSet::new();
    let mut stack: Vec<Node> = vec![start];
    while let Some(node) = stack.pop() {
        if node == origin {
            bail!(
                "adding prerequisite {target} on {source_item} would introduce a dependency cycle"
            );
        }
        if !visited.insert(node.clone()) {
            continue;
        }
        let (proj, series_file, item_id) = &node;
        let events_slice: Option<&[JournalEvent]> = match proj {
            None => Some(local_events),
            Some(slug) => cache
                .entry(Some(slug.clone()))
                .or_insert_with(|| {
                    unique_journal_by_slug(known, slug)
                        .and_then(|journal_dir| super::read_events(journal_dir).ok())
                })
                .as_deref(),
        };
        let Some(events_slice) = events_slice else {
            continue;
        };
        let Some(series) = replay(events_slice, series_file) else {
            continue;
        };
        let Some(item) = series.item(item_id) else {
            continue;
        };
        if item.state == ItemState::Split {
            for part_id in &item.split_into {
                stack.push((proj.clone(), series_file.clone(), part_id.clone()));
            }
        }
        for dep in &item.depends_on {
            let next_proj = match &dep.project {
                Some(slug) => normalize_project(Some(slug.as_str())),
                None => proj.clone(),
            };
            let next_series = dep.series.clone().unwrap_or_else(|| series_file.clone());
            stack.push((next_proj, next_series, dep.item.clone()));
        }
        if item.state == ItemState::Transferred {
            if let Some(dest) = &item.transferred_to {
                let next_proj = match &dest.project {
                    Some(slug) => normalize_project(Some(slug.as_str())),
                    None => proj.clone(),
                };
                stack.push((next_proj, dest.series.clone(), dest.item.clone()));
            }
        }
    }
    let _ = dir;
    Ok(())
}

/// Record a prerequisite edge on a standing item, refusing cycles.
///
/// Acquires the workspace-wide prerequisite graph lock before taking this
/// journal's transition lock so concurrent `progress depend` writes across any
/// number of journals serialize and observe each prior committed edge before
/// cycle checking.
pub(crate) fn depend(ctx: &Ctx, name: &str, id: &str, on: &str) -> Result<ItemPrereq> {
    let local_dir = resolve_dir(&ctx.cwd)?;
    let _graph_lock = super::lock_depend_graph(&local_dir)?;
    let open = open_for_write(ctx, name)?;
    let item = open.standing(id)?;
    let prereq = resolve_prereq(&open.dir, &open.events, &open.series, on)?;
    if item.depends_on.contains(&prereq) {
        bail!("item {id} already depends on {prereq}");
    }
    let known = crate::config::load()
        .ok()
        .and_then(|cfg| crate::registry::known_journals(&cfg).ok())
        .unwrap_or_default();
    let c = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let here_canon = c(&open.dir);
    let local_slug = known
        .iter()
        .find(|(_, journal)| c(journal) == here_canon)
        .map(|(slug, _)| slug.as_str())
        .filter(|slug| unique_journal_by_slug(&known, slug).is_some());
    check_no_cycle(
        &open.dir,
        &open.events,
        local_slug,
        &known,
        &open.series.file,
        id,
        &prereq,
    )?;
    let mut event = open.event(ctx, "item-depended", Some(id));
    event.series.depends_on = Some(prereq.clone());
    open.append(ctx, &event)?;
    Ok(prereq)
}

/// Remove a recorded prerequisite edge from an item.
pub(crate) fn undepend(ctx: &Ctx, name: &str, id: &str, on: &str) -> Result<ItemPrereq> {
    let open = open_for_write(ctx, name)?;
    let item = open.series.require_item(id)?;
    // Match against recorded prereqs directly first (so a deleted foreign
    // project or series can still be unlinked), then resolve by name.
    let matched = if let Some(found) = item
        .depends_on
        .iter()
        .find(|dep| dep.to_string() == on)
        .cloned()
    {
        found
    } else if let Ok(resolved) = resolve_prereq(&open.dir, &open.events, &open.series, on) {
        if item.depends_on.contains(&resolved) {
            resolved
        } else {
            bail!("item {id} does not depend on {on}");
        }
    } else {
        // Also allow matching by `<series-topic>:<item>` or `<project-label>::<series-topic>:<item>`.
        let mut candidates: Vec<&ItemPrereq> = item
            .depends_on
            .iter()
            .filter(|dep| {
                let series_matches = |target: &str| {
                    dep.series.as_deref().is_some_and(|file| {
                        file == target
                            || parse_artifact_name(file)
                                .is_some_and(|(_, topic, _)| topic == target)
                    })
                };
                if let Some((proj, rest)) = on.split_once(super::REFERENCE_SEPARATOR) {
                    if let Some((s, it)) = rest.rsplit_once(':') {
                        let proj_ok = dep.project.as_deref() == Some(proj)
                            || super::registered_project(proj)
                                .ok()
                                .is_some_and(|p| dep.project.as_deref() == Some(p.slug.as_str()));
                        return proj_ok && series_matches(s) && dep.item == it;
                    }
                } else if let Some((s, it)) = on.rsplit_once(':') {
                    return dep.project.is_none() && series_matches(s) && dep.item == it;
                }
                false
            })
            .collect();
        match candidates.len() {
            1 => candidates.remove(0).clone(),
            0 => bail!("item {id} does not depend on {on}"),
            _ => bail!("{on} matches several prerequisites of {id}; name the series by file"),
        }
    };
    let mut event = open.event(ctx, "item-undepended", Some(id));
    event.series.depends_on = Some(matched.clone());
    open.append(ctx, &event)?;
    Ok(matched)
}

/// The result of transferring an item to another series.
pub(crate) struct TransferOutcome {
    pub(crate) source_item: String,
    pub(crate) target_project_label: Option<String>,
    pub(crate) target_series: String,
    pub(crate) target_item: String,
    pub(crate) wave: Option<String>,
    pub(crate) warnings: Vec<String>,
}

/// Transfer or defer a standing open item to another series (in this journal
/// or `<project>::<series>` in another registered project).
///
/// Refuses completed (`done`), dropped, split, already transferred, or
/// integrated-change items. Preserves title, `--from` source, active change
/// link (skipping a closed-without-integrating change), and matching wave
/// unless overridden by `--wave`.
///
/// Same-journal transfers execute under one journal transition lock.
/// Cross-project transfers lock both journals in canonical path order before
/// reading or writing, and append the destination item before marking the
/// source item transferred.
pub(crate) fn transfer(
    ctx: &Ctx,
    source_name: &str,
    item_id: &str,
    target_spec: &str,
    wave: Option<&str>,
    reason: &str,
) -> Result<TransferOutcome> {
    let here = resolve_dir(&ctx.cwd)?;
    transfer_from_journal(
        ctx,
        &here,
        Some(&ctx.cwd),
        source_name,
        item_id,
        target_spec,
        wave,
        reason,
    )
}

/// Transfer an item starting from an explicit source journal directory and
/// optional repository anchor.
#[allow(clippy::too_many_arguments)]
pub(crate) fn transfer_from_journal(
    ctx: &Ctx,
    source_dir: &Path,
    here_anchor: Option<&Path>,
    source_name: &str,
    item_id: &str,
    target_spec: &str,
    wave: Option<&str>,
    reason: &str,
) -> Result<TransferOutcome> {
    use crate::commands::progress::reader::Ledger;
    use crate::model::Closure;
    use crate::store::Store;

    let reason = one_line("--reason", reason)?;
    let here = source_dir.to_path_buf();
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let (target_project, target_series_name) =
        if let Some((prefix, series_name)) = target_spec.split_once(super::REFERENCE_SEPARATOR) {
            let project = super::registered_project(prefix)?;
            if canonical(&project.journal_dir) == canonical(&here) {
                (None, series_name)
            } else {
                (Some(project), series_name)
            }
        } else {
            (None, target_spec)
        };

    // Acquire journal transition lock(s) in canonical path order to prevent deadlock.
    let (_lock_a, _lock_b) = match &target_project {
        None => (lock_journal_transition(&here)?, None),
        Some(project) => {
            let here_canon = canonical(&here);
            let there_canon = canonical(&project.journal_dir);
            if here_canon <= there_canon {
                let first = lock_journal_transition(&here)?;
                let second = lock_journal_transition(&project.journal_dir)?;
                (first, Some(second))
            } else {
                let first = lock_journal_transition(&project.journal_dir)?;
                let second = lock_journal_transition(&here)?;
                (first, Some(second))
            }
        }
    };

    let source_events = super::read_events(&here)?;
    let source_file = resolve_name(&source_events, source_name, &here)?;
    let source_series = replay(&source_events, &source_file)
        .with_context(|| format!("{source_file} was never started"))?;
    if source_series.retired.is_some() {
        bail!("{source_file} was retired; it is read-only");
    }
    let source_item = source_series.require_item(item_id)?;
    if !source_item.stands() {
        bail!(
            "item {item_id} of {source_file} is {}; it no longer stands in the list",
            state_word(source_item.state)
        );
    }
    if source_item.state == ItemState::Done {
        bail!("item {item_id} of {source_file} is done; completed work cannot be transferred");
    }

    // Check any linked change: refuse integrated changes, drop closed-without-integrating
    // links so the destination starts clean, and preserve open in-flight links.
    let active_link: Option<ItemLink> = match source_item.link() {
        None => None,
        Some(link) => {
            let state = match &link.project {
                None => here_anchor
                    .and_then(|anchor| Store::discover_existing_for_reading(anchor).ok().flatten())
                    .and_then(|store| store.state(&link.change).ok()),
                Some(slug) => super::registered_project(slug).ok().and_then(|project| {
                    project
                        .anchor
                        .as_deref()
                        .and_then(|anchor| Ledger::at(anchor).state(&link.change).ok().flatten())
                }),
            };
            match state.and_then(|s| s.closure) {
                Some(closure) if closure.outcome == Closure::Integrated => {
                    bail!(
                        "item {item_id} of {source_file} is linked to integrated change {}; \
                         completed work cannot be transferred",
                        link
                    );
                }
                Some(_) => None,
                None => Some(link),
            }
        }
    };

    let (target_dir, target_series, target_events) = match &target_project {
        None => {
            let target_file = resolve_name(&source_events, target_series_name, &here)?;
            if target_file == source_file {
                bail!("cannot transfer item {item_id} to its own series {source_file}");
            }
            let series = replay(&source_events, &target_file)
                .with_context(|| format!("{target_file} was never started"))?;
            if series.retired.is_some() {
                bail!("{target_file} was retired; it is read-only");
            }
            (here.clone(), series, None)
        }
        Some(project) => {
            let target_events = super::read_events(&project.journal_dir)?;
            let target_file =
                resolve_name(&target_events, target_series_name, &project.journal_dir)?;
            let series = replay(&target_events, &target_file)
                .with_context(|| format!("{target_file} was never started"))?;
            if series.retired.is_some() {
                bail!(
                    "{}::{target_file} was retired; it is read-only",
                    project.label()
                );
            }
            (project.journal_dir.clone(), series, Some(target_events))
        }
    };

    let dest_wave = match wave {
        Some(wave_id) => {
            target_series.require_wave(wave_id)?;
            Some(wave_id.to_string())
        }
        None => source_item
            .wave
            .as_ref()
            .filter(|wave_id| target_series.wave(wave_id).is_some())
            .cloned(),
    };

    // For cross-project transfers, establish this journal's unique registered
    // slug so the destination can record unambiguous provenance and re-scope
    // local references without colliding with another same-basename journal.
    let (here_slug, known_journals): (Option<String>, Vec<(String, std::path::PathBuf)>) =
        if target_project.is_some() {
            let cfg = crate::config::load()?;
            let known = crate::registry::known_journals(&cfg)?;
            let here_canon = canonical(&here);
            let found = known
                .iter()
                .find(|(_, journal)| canonical(journal) == here_canon)
                .map(|(slug, _)| slug.clone());
            let Some(slug) = found else {
                bail!(
                    "this project's journal is not registered; bind or record a journal entry \
                     before transferring items to another project"
                );
            };
            let same_slug_count = known
                .iter()
                .filter(|(other_slug, _)| *other_slug == slug)
                .count();
            if same_slug_count > 1 {
                bail!(
                    "this project's journal slug {slug:?} is ambiguous across {same_slug_count} \
                     registered journals; give each journal a unique directory name before \
                     transferring items across projects"
                );
            }
            (Some(slug), known)
        } else {
            (None, Vec::new())
        };

    let carried_source = source_item
        .source
        .as_ref()
        .map(|src| match &target_project {
            None => src.clone(),
            Some(target_proj) => {
                let project = match &src.project {
                    None => here_slug.clone(),
                    Some(slug) if slug == &target_proj.slug => None,
                    Some(slug) => Some(slug.clone()),
                };
                ItemSource {
                    project,
                    file: src.file.clone(),
                }
            }
        });

    let carried_link = active_link.map(|link| match &target_project {
        None => link,
        Some(target_proj) => {
            let project = match &link.project {
                None => here_slug.clone(),
                Some(slug) if slug == &target_proj.slug => None,
                Some(slug) => Some(slug.clone()),
            };
            ItemLink {
                project,
                change: link.change,
            }
        }
    });

    // Warn when a local standing item depends on the transferred item and the
    // transfer shifts it into a later wave than its dependent.
    let mut warnings = Vec::new();
    if let Some(dest_wave_id) = dest_wave.as_deref() {
        for file in series_files(&source_events) {
            let Some(other_series) = replay(&source_events, &file) else {
                continue;
            };
            if other_series.retired.is_some() {
                continue;
            }
            let Some(dest_idx) = other_series.waves.iter().position(|w| w.id == dest_wave_id)
            else {
                continue;
            };
            for dep_item in &other_series.items {
                if !dep_item.stands() {
                    continue;
                }
                let depends = dep_item.depends_on.iter().any(|dep| {
                    dep.project.is_none()
                        && dep.item == item_id
                        && dep.series.as_deref().unwrap_or(&other_series.file) == source_file
                });
                if !depends {
                    continue;
                }
                if let Some(dep_wave) = dep_item.wave.as_deref() {
                    if let Some(dep_idx) = other_series.waves.iter().position(|w| w.id == dep_wave)
                    {
                        if dest_idx > dep_idx {
                            warnings.push(format!(
                                "item {}:{} in wave {dep_wave} depends on {item_id}, which is \
                                 moving to later wave {dest_wave_id}",
                                other_series.file, dep_item.id
                            ));
                        }
                    }
                }
            }
        }
    }

    let now = Utc::now();
    let target_topic = parse_artifact_name(&target_series.file)
        .map(|(_, topic, _)| topic)
        .unwrap_or_default();
    let source_topic = parse_artifact_name(&source_file)
        .map(|(_, topic, _)| topic)
        .unwrap_or_default();
    let provenance = ItemRef {
        project: here_slug,
        series: source_file.clone(),
        item: item_id.to_string(),
    };

    let new_id = match &target_project {
        None => {
            // Never silently retarget an in-doubt cross-project transfer onto a
            // local series: complete the pending cross-project transfer first.
            if let Some(pending_key) = source_item.pending_transfer_key.as_deref() {
                match source_item.pending_transfer_target.as_ref() {
                    Some(target) => bail!(
                        "item {item_id} has a pending cross-project transfer to {}::{}; finish \
                         that transfer before moving {item_id} locally",
                        target.project,
                        target.file
                    ),
                    None => bail!(
                        "item {item_id} has a pending cross-project transfer ({pending_key}); \
                         finish that transfer before moving {item_id} locally"
                    ),
                }
            }
            // Publish destination creation, optional active change link, and source
            // retirement in one recoverable write.
            let new_id = format!("i{}", target_series.next_id());
            let mut records = Vec::with_capacity(3);
            let mut add_event = JournalEvent::base(ctx, now, &target_topic, "item-added");
            add_event.file = Some(target_series.file.clone());
            add_event.title = Some(source_item.title.clone());
            add_event.series.item = Some(new_id.clone());
            add_event.series.wave = dest_wave.clone();
            add_event.series.item_source = carried_source;
            add_event.series.transferred_from = Some(provenance);
            records.push(add_event);

            if let Some(link) = carried_link {
                let mut link_event = JournalEvent::base(ctx, now, &target_topic, "item-linked");
                link_event.file = Some(target_series.file.clone());
                link_event.series.item = Some(new_id.clone());
                link_event.series.change = Some(link.change);
                link_event.series.change_project = link.project;
                records.push(link_event);
            }

            let mut xfer_event = JournalEvent::base(ctx, now, &source_topic, "item-transferred");
            xfer_event.file = Some(source_file.clone());
            xfer_event.note = Some(reason);
            xfer_event.series.item = Some(item_id.to_string());
            xfer_event.series.transferred_to = Some(ItemRef {
                project: None,
                series: target_series.file.clone(),
                item: new_id.clone(),
            });
            records.push(xfer_event);

            super::append_events(ctx, &here, &records)?;
            new_id
        }
        Some(target_proj) => {
            // Cross-project transfer: persist a unique `transfer_key` on the source
            // journal via `item-transfer-pending` before touching the destination so
            // retries match the committed half-transfer even if the source journal is
            // relocated on disk or a slug is later reused by another producer.
            let target_proj_slug = target_proj.slug.as_str();
            let current_target = RelatedSeries {
                project: target_proj_slug.to_string(),
                file: target_series.file.clone(),
            };
            let current_target_digest = destination_series_digest(
                &target_dir,
                target_events.as_deref().unwrap_or(&[]),
                &target_series.file,
            );
            let pending_key = source_item.pending_transfer_key.as_deref();
            let exact_retry = pending_key.and_then(|key| {
                target_series
                    .items
                    .iter()
                    .find(|item| item.transfer_key.as_deref() == Some(key))
            });
            let new_id = if let Some(existing) = exact_retry {
                if let Some(link) = carried_link {
                    if existing.link().as_ref() != Some(&link) {
                        let mut link_event =
                            JournalEvent::base(ctx, now, &target_topic, "item-linked");
                        link_event.file = Some(target_series.file.clone());
                        link_event.series.item = Some(existing.id.clone());
                        link_event.series.change = Some(link.change);
                        link_event.series.change_project = link.project;
                        append_event(ctx, &target_dir, &link_event)?;
                    }
                }
                existing.id.clone()
            } else {
                if let Some(key) = pending_key {
                    // An in-doubt pending transfer may only complete against the
                    // destination that already holds `key` (matched above) or retry
                    // the exact originally targeted `(project, series)` instance
                    // verified by its journal directory + series start digest.
                    // Silently retargeting to another destination or to a rebound
                    // journal carrying a same-named series is always refused.
                    let target_matches = source_item.pending_transfer_target.as_ref()
                        == Some(&current_target)
                        && source_item.pending_transfer_target_digest.as_deref()
                            == Some(current_target_digest.as_str());
                    if !target_matches {
                        match source_item.pending_transfer_target.as_ref() {
                            Some(target) => bail!(
                                "item {item_id} has a pending cross-project transfer to {}::{} \
                                 that does not match the resolved destination journal/series for \
                                 {}::{}; retry the original destination to finish it",
                                target.project,
                                target.file,
                                current_target.project,
                                current_target.file
                            ),
                            None => bail!(
                                "item {item_id} has a pending cross-project transfer ({key}) whose \
                                 destination does not match {}::{}; finish that transfer at its \
                                 original destination first",
                                current_target.project,
                                current_target.file
                            ),
                        }
                    }
                    if let Some((other_slug, other_file, other_item)) =
                        find_transfer_key_in_known(&known_journals, key)?
                    {
                        bail!(
                            "item {item_id} already landed as {other_item} in \
                             {other_slug}::{other_file} under a pending transfer; finish that \
                             transfer before moving {item_id} elsewhere"
                        );
                    }
                }
                if let Some(legacy) = target_series.items.iter().find(|item| {
                    item.transferred_from.as_ref() == Some(&provenance)
                        && item.transfer_key.is_none()
                }) {
                    bail!(
                        "destination {} already holds item {} with matching transferred_from {} \
                         but no durable transfer_key; refusing to assume it originated from this \
                         source item before retiring {item_id}",
                        target_series.file,
                        legacy.id,
                        provenance
                    );
                }
                let expected_key = match pending_key {
                    Some(key) => key.to_string(),
                    None => {
                        let key = crate::ids::new_identifier();
                        let mut pending_event =
                            JournalEvent::base(ctx, now, &source_topic, "item-transfer-pending");
                        pending_event.file = Some(source_file.clone());
                        pending_event.series.item = Some(item_id.to_string());
                        pending_event.series.transfer_key = Some(key.clone());
                        pending_event.series.transfer_target = Some(current_target);
                        pending_event.series.transfer_target_digest = Some(current_target_digest);
                        append_event(ctx, &here, &pending_event)?;
                        key
                    }
                };
                let new_id = format!("i{}", target_series.next_id());
                let mut dest_records = Vec::with_capacity(2);
                let mut add_event = JournalEvent::base(ctx, now, &target_topic, "item-added");
                add_event.file = Some(target_series.file.clone());
                add_event.title = Some(source_item.title.clone());
                add_event.series.item = Some(new_id.clone());
                add_event.series.wave = dest_wave.clone();
                add_event.series.item_source = carried_source;
                add_event.series.transferred_from = Some(provenance);
                add_event.series.transfer_key = Some(expected_key);
                dest_records.push(add_event);

                if let Some(link) = carried_link {
                    let mut link_event = JournalEvent::base(ctx, now, &target_topic, "item-linked");
                    link_event.file = Some(target_series.file.clone());
                    link_event.series.item = Some(new_id.clone());
                    link_event.series.change = Some(link.change);
                    link_event.series.change_project = link.project;
                    dest_records.push(link_event);
                }
                super::append_events(ctx, &target_dir, &dest_records)?;
                new_id
            };

            let mut xfer_event = JournalEvent::base(ctx, now, &source_topic, "item-transferred");
            xfer_event.file = Some(source_file.clone());
            xfer_event.note = Some(reason);
            xfer_event.series.item = Some(item_id.to_string());
            xfer_event.series.transferred_to = Some(ItemRef {
                project: Some(target_proj.slug.clone()),
                series: target_series.file.clone(),
                item: new_id.clone(),
            });
            append_event(ctx, &here, &xfer_event)?;
            new_id
        }
    };

    Ok(TransferOutcome {
        source_item: item_id.to_string(),
        target_project_label: target_project.as_ref().map(|p| p.label()),
        target_series: target_series.file,
        target_item: new_id,
        wave: dest_wave,
        warnings,
    })
}

/// Compute a durable digest identifying one destination series instance
/// (canonical destination journal directory native bytes and Unix `(dev, ino)`,
/// series filename, and immutable `note` start event) so rebinding or swapping
/// the destination onto another journal cannot satisfy an uncommitted pending
/// transfer retry.
fn destination_series_digest(
    target_dir: &Path,
    target_events: &[JournalEvent],
    target_file: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut feed = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    let canon = target_dir
        .canonicalize()
        .unwrap_or_else(|_| target_dir.to_path_buf());
    feed(canon.as_os_str().as_encoded_bytes());
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(&canon) {
        use std::os::unix::fs::MetadataExt;
        feed(&meta.dev().to_le_bytes());
        feed(&meta.ino().to_le_bytes());
    }
    feed(target_file.as_bytes());
    if let Some(start_note) = target_events
        .iter()
        .find(|e| e.event == "note" && e.file.as_deref() == Some(target_file))
    {
        if let Ok(json) = serde_json::to_vec(start_note) {
            feed(&json);
        }
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Search registered journals for any series item already carrying `key`.
/// Unreadable registered journals return an error rather than being skipped so
/// retries fail closed instead of duplicating a committed half-transfer.
fn find_transfer_key_in_known(
    known: &[(String, std::path::PathBuf)],
    key: &str,
) -> Result<Option<(String, String, String)>> {
    let mut seen_dirs = std::collections::HashSet::new();
    for (slug, dir) in known {
        let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        if !seen_dirs.insert(canon) {
            continue;
        }
        let jsonl = dir.join("events.jsonl");
        match std::fs::metadata(&jsonl) {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "cannot inspect registered journal {slug:?} ({}) while verifying pending transfer key",
                        jsonl.display()
                    )
                });
            }
        }
        let events = super::read_events(dir).with_context(|| {
            format!(
                "cannot inspect registered journal {slug:?} ({}) while verifying pending transfer key",
                jsonl.display()
            )
        })?;
        for file in series_files(&events) {
            let Some(series) = replay(&events, &file) else {
                continue;
            };
            if let Some(item) = series
                .items
                .iter()
                .find(|item| item.transfer_key.as_deref() == Some(key))
            {
                return Ok(Some((slug.clone(), file, item.id.clone())));
            }
        }
    }
    Ok(None)
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

#[cfg(all(test, unix))]
mod tests {
    use super::destination_series_digest;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn destination_series_digest_distinguishes_raw_non_utf8_from_replacement_char() {
        let tmp = tempfile::tempdir().unwrap();
        let raw_parent = tmp.path().join(OsStr::from_bytes(b"dir-\xe9"));
        let ufffd_parent = tmp.path().join("dir-\u{FFFD}");
        std::fs::create_dir_all(&raw_parent).unwrap();
        std::fs::create_dir_all(&ufffd_parent).unwrap();
        let file = "20260103T000000Z-work-progress.md";
        let d1 = destination_series_digest(&raw_parent, &[], file);
        let d2 = destination_series_digest(&ufffd_parent, &[], file);
        assert_ne!(d1, d2);
        // Same canonical directory produces identical digest.
        assert_eq!(d1, destination_series_digest(&raw_parent, &[], file));
    }
}
