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
    append_event, archive_dir, create_artifact, heading_from_topic, lock_journal_transition,
    parse_artifact_name, resolve_dir, valid_topic, JournalEvent,
};
use crate::commands::Ctx;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// The artifact kind a series is filed under.
pub(crate) const KIND: &str = "progress";

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
    /// The change of this project's ledger an `item-linked` event names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    change: Option<String>,
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

fn valid_item_id(value: &str) -> bool {
    value
        .strip_prefix('i')
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
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
    let bare = fields.parts.is_empty() && fields.change.is_none() && fields.related.is_none();
    if !on_series {
        return false;
    }
    match event.event.as_str() {
        "item-added" => names_item && line(&event.title) && placements.len() <= 1 && placed && bare,
        "item-renamed" => names_item && line(&event.title) && placements.is_empty() && bare,
        "item-moved" => names_item && placements.len() == 1 && placed && bare,
        "item-dropped" => names_item && line(&event.note) && placements.is_empty() && bare,
        "item-done" | "item-reopened" | "item-unlinked" => {
            names_item
                && event.note.as_deref().is_none_or(single_line)
                && placements.is_empty()
                && bare
        }
        "item-linked" => {
            names_item
                && fields.change.as_deref().is_some_and(single_line)
                && placements.is_empty()
                && fields.parts.is_empty()
                && fields.related.is_none()
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
                && fields.related.is_none()
        }
        "series-related" => {
            fields.item.is_none()
                && placements.is_empty()
                && fields.parts.is_empty()
                && fields.change.is_none()
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
        "item-done" => format!("marked item {item} of {file} done"),
        "item-reopened" => format!("reopened item {item} of {file}"),
        "item-linked" => format!(
            "linked item {item} of {file} to change {}",
            fields.change.as_deref().unwrap_or_default()
        ),
        "item-unlinked" => format!("unlinked item {item} of {file}"),
        "item-split" => format!(
            "split item {item} of {file} into {}: {note}",
            fields
                .parts
                .iter()
                .map(|part| part.item.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "series-related" => {
            let related = fields.related.as_ref();
            format!(
                "related {file} to {}::{}",
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
    /// The reason or note recorded with the item's latest state.
    pub(crate) note: Option<String>,
    /// When the item reached its latest state.
    pub(crate) state_at: Option<String>,
    /// For a split item, the items that replaced it.
    pub(crate) split_into: Vec<String>,
    initial: bool,
}

impl Item {
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
                        item.note = event.note.clone();
                        item.state_at = at;
                    }
                    "item-linked" => item.change = fields.change.clone(),
                    "item-unlinked" => item.change = None,
                    _ => {}
                }
            }
        }
    }
    if let Some(series) = series.as_mut() {
        series.added = added;
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

/// Every series in the journal at `dir` that is not retired.
pub(crate) fn active_series(dir: &Path) -> Result<Vec<Series>> {
    let events = super::read_events(dir)?;
    Ok(series_files(&events)
        .iter()
        .filter_map(|file| replay(&events, file))
        .filter(|series| series.retired.is_none())
        .collect())
}

/// One series in the journal at `dir`, by filename or by topic. A topic
/// several series share resolves to the one not yet retired, and is refused
/// when that does not narrow it to one.
pub(crate) fn named_series(dir: &Path, name: &str) -> Result<Series> {
    let events = super::read_events(dir)?;
    let file = resolve_name(&events, name, dir)?;
    replay(&events, &file).with_context(|| format!("{file} was never started as a series"))
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

/// Start a series: write its artifact and declare its first items.
pub(crate) fn start(
    ctx: &Ctx,
    topic: &str,
    title: &str,
    follows: Option<&str>,
    items: &[String],
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
        match parse_artifact_name(plan) {
            Some((_, _, kind)) if kind == "plan" => {}
            _ => bail!("--follows takes a plan artifact's filename, got {plan:?}"),
        }
        if !dir.join(plan).is_file() && !archive_dir(&dir).join(plan).is_file() {
            bail!("no plan {plan} in {} or its cold archive", dir.display());
        }
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
        append_event(ctx, &dir, &event)?;
    }
    Ok(filename)
}

/// Add an item; returns its id.
pub(crate) fn add(ctx: &Ctx, name: &str, title: &str, placement: Placement) -> Result<String> {
    let open = open_for_write(ctx, name)?;
    let id = format!("i{}", open.series.next_id());
    let mut event = open.event(ctx, "item-added", Some(&id));
    event.title = Some(one_line("the title", title)?);
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

pub(crate) fn done(ctx: &Ctx, name: &str, id: &str, note: Option<&str>) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    let item = open.standing(id)?;
    if let Some(change) = &item.change {
        bail!("item {id} follows change {change}; its stage is the ledger's, so unlink it first");
    }
    if item.state == ItemState::Done {
        bail!("item {id} is already done");
    }
    let mut event = open.event(ctx, "item-done", Some(id));
    event.note = note.map(|note| one_line("--note", note)).transpose()?;
    open.append(ctx, &event)
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
pub(crate) fn linkable(ctx: &Ctx, name: &str, id: &str) -> Result<(String, Option<String>)> {
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
    Ok((file, item.change.clone()))
}

/// Link an item to a change. `replaceable` says whether the link the item
/// already carries may be replaced, which only the ledger can answer.
pub(crate) fn link(ctx: &Ctx, name: &str, id: &str, change: &str, replaceable: bool) -> Result<()> {
    let open = open_for_write(ctx, name)?;
    let item = open.standing(id)?;
    if item.state == ItemState::Done {
        bail!("item {id} is done; reopen it before linking a change");
    }
    match item.change.as_deref() {
        Some(current) if current == change => bail!("item {id} is already linked to {change}"),
        Some(current) if !replaceable => bail!(
            "item {id} is linked to change {current}, which is still open or integrated; \
             unlink it first"
        ),
        _ => {}
    }
    let mut event = open.event(ctx, "item-linked", Some(id));
    event.series.change = Some(change.to_string());
    open.append(ctx, &event)
}

pub(crate) fn unlink(ctx: &Ctx, name: &str, id: &str) -> Result<String> {
    let open = open_for_write(ctx, name)?;
    let Some(change) = open.series.require_item(id)?.change.clone() else {
        bail!("item {id} is not linked to a change");
    };
    open.append(ctx, &open.event(ctx, "item-unlinked", Some(id)))?;
    Ok(change)
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

/// Retire a series as superseded by its conclusion.
pub(crate) fn retire(ctx: &Ctx, name: &str, conclusion: &str) -> Result<String> {
    let open = open_for_write(ctx, name)?;
    match parse_artifact_name(conclusion) {
        Some((_, _, kind)) if kind == "conclusion" => {}
        _ => bail!("--by takes a conclusion artifact's filename, got {conclusion:?}"),
    }
    let path = [
        open.dir.join(conclusion),
        archive_dir(&open.dir).join(conclusion),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .with_context(|| {
        format!(
            "no conclusion {conclusion} in {} or its cold archive",
            open.dir.display()
        )
    })?;
    let bytes = std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut event = open.event(ctx, "consumed", None);
    event.outcome = Some("superseded".to_string());
    event.note = Some(format!("retired by {conclusion}"));
    event.decision = Some(conclusion.to_string());
    event.decision_kind = Some("conclusion".to_string());
    event.decision_digest = Some(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))));
    open.append(ctx, &event)?;
    Ok(open.series.file.clone())
}

/// Open questions in the journal at `dir` that a person must settle.
pub(crate) fn person_questions(dir: &Path) -> Result<Vec<super::OpenQuestion>> {
    Ok(super::open_questions(dir)?
        .into_iter()
        .filter(|question| {
            question
                .settle_by
                .as_deref()
                .is_none_or(|by| by == "person")
        })
        .collect())
}
