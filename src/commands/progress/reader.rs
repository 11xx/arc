//! Shared read-only projection layer for `arc progress`, `arc milestone`, and
//! `arc workspace` views.
//!
//! Each project's journal event log and ledger open at most once per report,
//! lazily when an item, relation, or track references that project. Nothing
//! here writes to a ledger or journal, runs live gate checks, or asks a runner
//! whether a dispatched session is alive.

use super::Ctx;
use crate::journal::series::{self, ItemState, ItemWait, RelatedSeries, Series, SinceStart};
use crate::journal::JournalEvent;
use crate::model::{Closure, Verdict};
use crate::state::ChangeState;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The schema `arc progress --json` prints.
const PROGRESS_SCHEMA: &str = "arc-progress/1";

pub(crate) fn same_dir(left: &Path, right: &Path) -> bool {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical(left) == canonical(right)
}

/// This journal's registry slug: the name another project's relation
/// records it by.
pub(crate) fn own_slug(cfg: &crate::config::Config, dir: &Path) -> Option<String> {
    crate::registry::known_journals(cfg)
        .ok()?
        .into_iter()
        .find(|(_, journal)| same_dir(journal, dir))
        .map(|(slug, _)| slug)
}

/// One item as a report shows it.
#[derive(Serialize)]
pub(crate) struct ItemRow {
    pub(crate) id: String,
    pub(crate) title: String,
    /// One word. Unlinked: `open`, `done`, `dropped` or `split`. Linked,
    /// as the change's events record it: `opened`, `snapshotted`,
    /// `verified`, `reviewed`, `integrated`, or `closed` for a change closed
    /// without integrating; `unknown` when the ledger holding it has no such
    /// change, or cannot be read for a change of another project.
    pub(crate) stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) change: Option<String>,
    /// For a change of another project's ledger, that project's label, as
    /// `arc workspace` names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
    /// For a change of another project's ledger, that project's registry
    /// slug, which the link records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project_slug: Option<String>,
    /// For a `reviewed` item, the verdict on the newest patchset:
    /// `approved`, `changes-requested` or `comment-only`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) verdict: Option<&'static str>,
    /// Who holds a live claim on the change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) held_by: Option<String>,
    /// Review rounds on the change that asked for changes.
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) changes_requested_rounds: usize,
    /// When the change was integrated, and the revision it integrated as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) integrated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) revision: Option<String>,
    /// For a change in flight, the newest run the ledger records dispatched
    /// against it with no recorded end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dispatch: Option<Dispatch>,
    /// The journal question the item waits on, while it is open and waits
    /// on a person.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) waiting: Option<Waiting>,
    /// The originating journal artifact (`--from` / `source`), with its
    /// derived status in the owning journal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<SourceStatus>,
    /// The durable artifact recorded as resolving an unlinked `done` item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolved_by: Option<series::ResolvedBy>,
    /// Live advisory ownership on an unlinked open item, derived from a live
    /// journal lane targeting `<series>:<item>` or an active `arc claim` on
    /// its `--from` source artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) claimed: Option<PreChangeClaim>,
    /// The reason or note recorded with a done, dropped, split or reopened item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) split_into: Vec<String>,
}

/// Originating journal artifact linked to an item, with derived open/consumed
/// status and drift detection.
#[derive(Serialize, Clone)]
pub(crate) struct SourceStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project_slug: Option<String>,
    pub(crate) file: String,
    /// `open`, `consumed`, or `unknown` when a foreign journal cannot be read.
    pub(crate) status: &'static str,
    /// True when the item is `integrated` or `done` while its non-plan
    /// source filing remains `open` in its journal.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) drift: bool,
}

/// Advisory pre-change ownership surfaced on an unlinked open item before
/// `arc begin` runs.
#[derive(Serialize, Clone)]
pub(crate) struct PreChangeClaim {
    /// `lane` (a live `arc journal lane open --item`) or `artifact-claim` (an
    /// active `arc claim` on the item's `--from` source artifact).
    pub(crate) via: &'static str,
    pub(crate) owner: String,
    pub(crate) at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) lane: Option<String>,
}

/// A run dispatched against a linked change, as the ledger records it: no
/// runner is asked whether it is alive.
#[derive(Serialize, Clone)]
pub(crate) struct Dispatch {
    /// The subject the dispatch was recorded for, else the route it went
    /// through.
    pub(crate) to: String,
    pub(crate) route: String,
    pub(crate) at: String,
    /// Whether a backend session is recorded as executing it.
    pub(crate) bound: bool,
}

/// An open question an item waits on.
#[derive(Serialize)]
pub(crate) struct Waiting {
    pub(crate) file: String,
    pub(crate) question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) heading: Option<String>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl ItemRow {
    /// The item as its series leaves it, before any ledger is read.
    pub(crate) fn of(item: &series::Item) -> Self {
        Self {
            id: item.id.clone(),
            title: item.title.clone(),
            stage: match item.state {
                ItemState::Open => "open",
                ItemState::Done => "done",
                ItemState::Dropped => "dropped",
                ItemState::Split => "split",
            },
            change: item.change.clone(),
            project: None,
            project_slug: None,
            verdict: None,
            held_by: None,
            changes_requested_rounds: 0,
            integrated_at: None,
            revision: None,
            dispatch: None,
            waiting: None,
            source: None,
            resolved_by: item.resolved_by.clone(),
            claimed: None,
            note: item.note.clone(),
            split_into: item.split_into.clone(),
        }
    }

    pub(crate) fn in_flight(&self) -> bool {
        self.change.is_some()
            && !matches!(
                self.stage,
                "integrated" | "closed" | "dropped" | "split" | "unknown"
            )
    }
}

/// The list counted as it stands.
#[derive(Serialize, Default)]
pub(crate) struct Counts {
    /// Items that stand: every item but the dropped and the split.
    pub(crate) items: usize,
    /// Integrated and done items, the share's numerator.
    pub(crate) complete: usize,
    pub(crate) integrated: usize,
    pub(crate) done: usize,
    pub(crate) in_flight: usize,
    /// Standing items no change carries and nobody marked done.
    pub(crate) open: usize,
    /// Standing unlinked open items held by a live pre-change lane or source
    /// artifact claim, also counted under `open`.
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) claimed: usize,
    /// Linked items whose change closed without integrating.
    pub(crate) closed: usize,
    /// Standing items waiting on an open question, each also counted by its
    /// stage.
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) waiting: usize,
    /// `complete` over `items`, rounded down; absent with no items.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) percent: Option<usize>,
    pub(crate) since_start: SinceStart,
}

#[derive(Serialize, Clone)]
pub(crate) struct Integration {
    pub(crate) item: String,
    pub(crate) at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) revision: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Relation {
    /// The other project's label, as `arc workspace` names it.
    pub(crate) project: String,
    /// The other project's registry slug, which the relation records.
    pub(crate) slug: String,
    pub(crate) file: String,
    /// Whether the other series relates back; absent when this command did
    /// not read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mutual: Option<bool>,
}

#[derive(Serialize)]
pub(crate) struct SeriesReport {
    pub(crate) file: String,
    pub(crate) title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) follows: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) retired: Option<series::Retirement>,
    pub(crate) related: Vec<Relation>,
    pub(crate) items: Vec<ItemRow>,
    pub(crate) counts: Counts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_integration: Option<Integration>,
}

/// A related series from another project, in compact form.
#[derive(Serialize)]
pub(crate) struct RelatedReport {
    pub(crate) project: String,
    pub(crate) slug: String,
    pub(crate) file: String,
    /// This project's series that relate to it.
    pub(crate) related_from: Vec<String>,
    /// Why the other journal could not be read; the remaining fields are
    /// then absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unavailable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) retired: Option<series::Retirement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) counts: Option<Counts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_integration: Option<Integration>,
    pub(crate) in_flight: Vec<ItemRow>,
}

#[derive(Serialize)]
pub(crate) struct WaitingQuestion {
    pub(crate) file: String,
    pub(crate) question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) heading: Option<String>,
    pub(crate) asked_at: String,
}

#[derive(Serialize)]
pub(crate) struct ProgressReport {
    pub(crate) schema: &'static str,
    pub(crate) journal_dir: String,
    pub(crate) series: Vec<SeriesReport>,
    pub(crate) related: Vec<RelatedReport>,
    pub(crate) waiting_on_person: Vec<WaitingQuestion>,
}

#[derive(Serialize)]
pub(crate) struct ProjectProgress {
    pub(crate) project: String,
    pub(crate) slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) anchor: Option<String>,
    pub(crate) series: Vec<SeriesReport>,
}

#[derive(Serialize)]
pub(crate) struct ProgressFailure {
    pub(crate) project: String,
    pub(crate) reason: String,
}

/// Opens one project's ledger without creating anything; `None` when the
/// project has none.
type Opener = Box<dyn Fn() -> Result<Option<Store>>>;

/// One project's ledger as a report reads it: opened only when an item is
/// linked into it, and then read one linked change at a time. A stage is what
/// the ledger last recorded for the change; nothing here judges the change
/// live, runs Git, or reads gate declarations.
pub(crate) struct Ledger {
    label: String,
    opener: Opener,
    store: OnceCell<std::result::Result<Option<Store>, String>>,
    /// Each change's newest dispatch with no recorded end, gathered once
    /// from the repository events the change's reduction already read.
    dispatches: OnceCell<BTreeMap<String, Dispatch>>,
}

impl Ledger {
    /// The ledger of the checkout `ctx` stands in.
    pub(crate) fn local(ctx: &Ctx) -> Self {
        let cwd = ctx.cwd.clone();
        Self::new(&ctx.cwd, move || {
            if crate::gitio::toplevel(&cwd).is_err() {
                return Ok(None);
            }
            Store::discover_existing_for_reading(&cwd)
        })
    }

    /// The ledger of the project at `anchor`, wherever the configuration
    /// keeps it.
    pub(crate) fn at(anchor: &Path) -> Self {
        let path = anchor.to_path_buf();
        Self::new(anchor, move || {
            if !path.is_dir() {
                return Ok(None);
            }
            let cfg = crate::config::load()?;
            Ok(crate::registry::ledger_at(&cfg, &path)?
                .map(|root| Store::open_at(&root))
                .transpose()?
                .flatten())
        })
    }

    /// A project with no ledger to read.
    pub(crate) fn none(place: &Path) -> Self {
        Self::new(place, || Ok(None))
    }

    fn new(place: &Path, opener: impl Fn() -> Result<Option<Store>> + 'static) -> Self {
        Self {
            label: place.display().to_string(),
            opener: Box::new(opener),
            store: OnceCell::new(),
            dispatches: OnceCell::new(),
        }
    }

    /// The store itself, for a caller that writes nothing through this read.
    pub(crate) fn opened(self) -> Result<Option<Store>> {
        (self.opener)()
    }

    fn store(&self) -> Result<Option<&Store>> {
        match self
            .store
            .get_or_init(|| (self.opener)().map_err(|error| format!("{error:#}")))
        {
            Ok(store) => Ok(store.as_ref()),
            Err(error) => bail!("cannot read the ledger of {}: {error}", self.label),
        }
    }

    /// One change's state, from that change's events and the records every
    /// reduction reads. `None` when the ledger holds no such change; a ledger
    /// that cannot be opened, or a change that cannot be reduced, is an error.
    pub(crate) fn state(&self, change: &str) -> Result<Option<ChangeState>> {
        if crate::ids::validate_id_component(change).is_err() {
            return Ok(None);
        }
        let Some(store) = self.store()? else {
            return Ok(None);
        };
        if !store.holds_change(change)? {
            return Ok(None);
        }
        store
            .state(change)
            .map(Some)
            .with_context(|| format!("cannot read change {change} in {}", self.label))
    }

    /// The newest run dispatched against `change` that has no recorded end.
    fn open_dispatch(&self, change: &str) -> Result<Option<Dispatch>> {
        let Some(store) = self.store()? else {
            return Ok(None);
        };
        if self.dispatches.get().is_none() {
            let events = store.repository_events()?;
            let _ = self.dispatches.set(open_dispatches(&events));
        }
        Ok(self
            .dispatches
            .get()
            .and_then(|open| open.get(change))
            .cloned())
    }

    /// The item as this ledger leaves it. An unlinked item's stage is what
    /// the series recorded; a linked one's is what the change last recorded.
    pub(crate) fn row(&self, item: &series::Item, now: DateTime<Utc>) -> Result<ItemRow> {
        let mut row = ItemRow::of(item);
        let Some(change) = item.change.as_deref() else {
            return Ok(row);
        };
        if !item.stands() {
            return Ok(row);
        }
        let Some(state) = self.state(change)? else {
            row.stage = "unknown";
            return Ok(row);
        };
        row.changes_requested_rounds = state
            .verdicts
            .iter()
            .filter(|verdict| verdict.verdict == Verdict::ChangesRequested)
            .count();
        row.held_by = state.claim.as_ref().and_then(|claim| {
            crate::state::claim_timing_at(claim, now)
                .active
                .then(|| format!("{} via {}", claim.owner.actor, claim.owner.harness))
        });
        row.stage = recorded_stage(&state, &mut row);
        if row.in_flight() {
            row.dispatch = self.open_dispatch(change)?;
        }
        Ok(row)
    }
}

/// Each change's newest dispatch that no recorded end closes, in the order
/// the repository events record them.
fn open_dispatches(events: &[crate::model::Event]) -> BTreeMap<String, Dispatch> {
    let mut open = BTreeMap::new();
    for round in crate::commands::run::rounds(events) {
        let Some(crate::commands::run::RunSubject::Change { id }) = &round.subject else {
            continue;
        };
        let crate::model::Payload::RunDispatched { route, .. } = &round.dispatch.payload else {
            continue;
        };
        if round.ending.is_some() {
            continue;
        }
        open.insert(
            id.clone(),
            Dispatch {
                to: round
                    .dispatch
                    .on_behalf_of
                    .clone()
                    .unwrap_or_else(|| route.clone()),
                route: route.clone(),
                at: round
                    .dispatch
                    .created_at
                    .to_rfc3339_opts(SecondsFormat::Secs, true),
                bound: round.binding.is_some(),
            },
        );
    }
    open
}

/// A registered project as a report reads it: both its journal events and its
/// ledger open lazily at most once per report.
pub(crate) struct Registered {
    pub(crate) slug: String,
    pub(crate) label: String,
    pub(crate) journal_dir: PathBuf,
    /// Opened only when an item links into the project or a related series
    /// of it is read.
    pub(crate) ledger: Ledger,
    /// Journal events, loaded once on first access (or seeded when the caller
    /// already read this journal).
    events: OnceCell<std::result::Result<Vec<JournalEvent>, String>>,
}

impl Registered {
    /// Read this project's journal events, caching the result for the lifetime
    /// of the report.
    pub(crate) fn events(&self) -> std::result::Result<&[JournalEvent], &str> {
        match self.events.get_or_init(|| {
            crate::journal::read_events(&self.journal_dir)
                .map_err(|error| format!("{error:#}").replace(['\n', '\r'], " "))
        }) {
            Ok(events) => Ok(events.as_slice()),
            Err(reason) => Err(reason.as_str()),
        }
    }

    /// Seed already-read journal events so a second reference back into this
    /// project does not re-read `events.jsonl`.
    pub(crate) fn seed_events(&self, events: Vec<JournalEvent>) {
        let _ = self.events.set(Ok(events));
    }
}

/// The registered projects one report reads through: the registry, read once
/// and only when an item or a relation names another project. Each project's
/// journal events and ledger open at most once, when something read needs them.
#[derive(Default)]
pub(crate) struct Registry {
    projects: Vec<Registered>,
}

impl Registry {
    pub(crate) fn of(projects: Vec<crate::registry::Project>) -> Self {
        Self {
            projects: projects
                .into_iter()
                .map(|project| Registered {
                    label: project.label(),
                    ledger: match project.anchor.as_deref() {
                        Some(anchor) => Ledger::at(anchor),
                        None => Ledger::none(&project.journal_dir),
                    },
                    slug: project.slug,
                    journal_dir: project.journal_dir,
                    events: OnceCell::new(),
                })
                .collect(),
        }
    }

    /// The registry as the journals record it. One that cannot be read
    /// leaves every other project unread rather than failing the report.
    pub(crate) fn read() -> Result<Self> {
        let cfg = crate::config::load()?;
        Ok(Self::of(
            crate::registry::journal_projects(&cfg).unwrap_or_default(),
        ))
    }

    /// Look up one registered project by slug only when that slug uniquely
    /// identifies a single registration. Two configured journals sharing a
    /// basename-derived slug return `None` rather than silently reading the
    /// wrong project.
    pub(crate) fn get(&self, slug: &str) -> Option<&Registered> {
        let mut matches = self.projects.iter().filter(|project| project.slug == slug);
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    /// Look up the exact registered project owning `dir`, by canonical path.
    pub(crate) fn by_dir(&self, dir: &Path) -> Option<&Registered> {
        self.projects
            .iter()
            .find(|project| same_dir(&project.journal_dir, dir))
    }

    /// The slug the registry gives the journal at `dir`, when that slug
    /// uniquely identifies it among all registered projects.
    pub(crate) fn slug_of(&self, dir: &Path) -> Option<&str> {
        let slug = self.by_dir(dir)?.slug.as_str();
        self.get(slug).map(|_| slug)
    }

    /// Seed already-read journal events for `dir` when it is registered here.
    pub(crate) fn seed_events(&self, dir: &Path, events: Vec<JournalEvent>) {
        if let Some(project) = self.by_dir(dir) {
            project.seed_events(events);
        }
    }

    pub(crate) fn label(&self, slug: &str) -> String {
        self.get(slug)
            .map_or_else(|| slug.to_string(), |project| project.label.clone())
    }
}

/// Whether reading `series` needs the registry: an item linked into another
/// project, a cross-project source artifact, or a relation to another project.
pub(crate) fn needs_registry(series: &[Series]) -> bool {
    series.iter().any(|series| {
        !series.related.is_empty()
            || series.items.iter().any(|item| {
                item.project.is_some()
                    || item
                        .source
                        .as_ref()
                        .is_some_and(|source| source.project.is_some())
            })
    })
}

/// The questions of one journal that wait on a person, found from events
/// already read and only when something asks for them.
pub(crate) struct PersonQuestions<'a> {
    dir: &'a Path,
    events: &'a [JournalEvent],
    found: OnceCell<Vec<crate::journal::OpenQuestion>>,
}

impl<'a> PersonQuestions<'a> {
    pub(crate) fn new(dir: &'a Path, events: &'a [JournalEvent]) -> Self {
        Self {
            dir,
            events,
            found: OnceCell::new(),
        }
    }

    pub(crate) fn all(&self) -> Result<&[crate::journal::OpenQuestion]> {
        if self.found.get().is_none() {
            let found = series::person_questions_in(self.dir, self.events)?;
            let _ = self.found.set(found);
        }
        Ok(self.found.get().map(Vec::as_slice).unwrap_or_default())
    }

    /// The wait as it stands: the question, while it is open and waits on a
    /// person.
    pub(crate) fn waiting(&self, wait: &ItemWait) -> Result<Option<Waiting>> {
        Ok(self
            .all()?
            .iter()
            .find(|open| open.file == wait.file && open.question == wait.question)
            .map(|open| Waiting {
                file: open.file.clone(),
                question: open.question.clone(),
                heading: open.heading.clone(),
            }))
    }
}

/// What one series' items are read through: the ledger of the project whose
/// series it is, its own journal events (for source status and live lanes),
/// the registry for an item linked into another project, and the questions of
/// its own journal.
pub(crate) struct Reader<'a> {
    pub(crate) home: &'a Ledger,
    pub(crate) home_dir: &'a Path,
    pub(crate) home_slug: Option<&'a str>,
    pub(crate) home_events: &'a [JournalEvent],
    pub(crate) registry: &'a Registry,
    pub(crate) questions: &'a PersonQuestions<'a>,
    pub(crate) now: DateTime<Utc>,
    live_lanes: OnceCell<Vec<crate::journal::LaneEntry>>,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(
        home: &'a Ledger,
        home_dir: &'a Path,
        home_slug: Option<&'a str>,
        home_events: &'a [JournalEvent],
        registry: &'a Registry,
        questions: &'a PersonQuestions<'a>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            home,
            home_dir,
            home_slug,
            home_events,
            registry,
            questions,
            now,
            live_lanes: OnceCell::new(),
        }
    }

    fn live_lanes(&self) -> &[crate::journal::LaneEntry] {
        self.live_lanes
            .get_or_init(|| crate::journal::live_lanes_in(self.home_events, self.now))
    }

    fn resolve_source(
        &self,
        source: &series::ItemSource,
        stage: &str,
    ) -> (SourceStatus, Option<PreChangeClaim>) {
        let foreign = source
            .project
            .as_deref()
            .filter(|slug| Some(*slug) != self.home_slug);
        let resolved: Option<(&[JournalEvent], &Path)> = match foreign {
            None => Some((self.home_events, self.home_dir)),
            Some(slug) => self.registry.get(slug).and_then(|project| {
                project
                    .events()
                    .ok()
                    .map(|events| (events, project.journal_dir.as_path()))
            }),
        };
        let (status, claim) = match resolved {
            None => ("unknown", None),
            Some((events, journal_dir)) => {
                if crate::journal::is_consumed(events, &source.file) {
                    ("consumed", None)
                } else if std::fs::symlink_metadata(journal_dir.join(&source.file))
                    .is_ok_and(|meta| meta.is_file())
                {
                    let claim =
                        crate::journal::active_artifact_claim(events, &source.file, self.now).map(
                            |active| PreChangeClaim {
                                via: "artifact-claim",
                                owner: active.owner,
                                at: active.at,
                                lane: None,
                            },
                        );
                    ("open", claim)
                } else if std::fs::symlink_metadata(
                    crate::journal::archive_dir(journal_dir).join(&source.file),
                )
                .is_ok_and(|meta| meta.is_file())
                {
                    ("archived", None)
                } else {
                    ("absent", None)
                }
            }
        };
        let is_plan = series::bare_artifact_kind(&source.file).as_deref() == Some("plan");
        let drift = !is_plan && status == "open" && matches!(stage, "integrated" | "done");
        let status_row = SourceStatus {
            project: source
                .project
                .as_deref()
                .map(|slug| self.registry.label(slug)),
            project_slug: source.project.clone(),
            file: source.file.clone(),
            status,
            drift,
        };
        (status_row, claim)
    }

    /// One item, read from the ledger that holds its change. A change of
    /// another project whose ledger is not registered or cannot be read is
    /// `unknown` and fails nothing.
    pub(crate) fn row(&self, series_file: &str, item: &series::Item) -> Result<ItemRow> {
        let foreign = item
            .project
            .as_deref()
            .filter(|slug| Some(*slug) != self.home_slug);
        let mut row = match foreign {
            None => self.home.row(item, self.now)?,
            Some(slug) => match self
                .registry
                .get(slug)
                .map(|project| project.ledger.row(item, self.now))
            {
                Some(Ok(row)) => row,
                _ => {
                    let mut row = ItemRow::of(item);
                    if item.stands() {
                        row.stage = "unknown";
                    }
                    row
                }
            },
        };
        if let Some(slug) = &item.project {
            row.project = Some(self.registry.label(slug));
            row.project_slug = Some(slug.clone());
        }
        let mut source_claim = None;
        if let Some(source) = &item.source {
            let (status, claim) = self.resolve_source(source, row.stage);
            row.source = Some(status);
            source_claim = claim;
        }
        if item.stands() {
            if let Some(wait) = &item.waits_on {
                row.waiting = self.questions.waiting(wait)?;
            }
            if row.stage == "open" {
                let lane_claim = self
                    .live_lanes()
                    .iter()
                    .find(|lane| {
                        lane.item.as_ref().is_some_and(|target| {
                            target.file == series_file && target.item == item.id
                        })
                    })
                    .map(|lane| PreChangeClaim {
                        via: "lane",
                        owner: lane.owner.to_string(),
                        at: lane.opened_at.clone(),
                        lane: Some(lane.topic.clone()),
                    });
                row.claimed = lane_claim.or(source_claim);
            }
        }
        Ok(row)
    }

    pub(crate) fn rows(&self, series: &Series) -> Result<Vec<ItemRow>> {
        series
            .items
            .iter()
            .map(|item| self.row(&series.file, item))
            .collect()
    }
}

/// The furthest stage the change's own events record. Integration and
/// closure come first; then a verdict on the newest patchset; then a
/// recorded pass at it; then the patchset itself.
pub(crate) fn recorded_stage(state: &ChangeState, row: &mut ItemRow) -> &'static str {
    if let Some(closure) = &state.closure {
        if closure.outcome != Closure::Integrated {
            return "closed";
        }
        row.integrated_at = Some(
            closure
                .created_at
                .to_rfc3339_opts(SecondsFormat::Secs, true),
        );
        row.revision = closure
            .landing
            .as_ref()
            .map(|landing| landing.target_after.clone())
            .or_else(|| closure.integrated_commit.clone());
        return "integrated";
    }
    let Some(patchset) = state.latest_patchset() else {
        return "opened";
    };
    if let Some(verdict) = state
        .latest_verdict()
        .filter(|verdict| verdict.patchset_id == patchset.id)
    {
        row.verdict = Some(match verdict.verdict {
            Verdict::Approved => "approved",
            Verdict::ChangesRequested => "changes-requested",
            Verdict::CommentOnly => "comment-only",
        });
        return "reviewed";
    }
    if verified_at(state, &patchset.head) {
        "verified"
    } else {
        "snapshotted"
    }
}

/// Whether the ledger records a passing verification of `head`: at least one
/// gate has a run recorded at it, and every gate's most recent run there is a
/// pass. A run that was not admitted ran nothing and is passed over; a
/// failure, or a bypass, recorded after a pass takes it back.
fn verified_at(state: &ChangeState, head: &str) -> bool {
    let mut latest: BTreeMap<&str, crate::model::VerifyResult> = BTreeMap::new();
    for entry in &state.verifications {
        let Some(gate) = entry.gate.as_deref() else {
            continue;
        };
        // The revision the run was recorded at: a pass carried to `head` by
        // a recorded rewrite is not a run at `head`.
        if entry.recorded_revision != head
            || entry.probe.is_some()
            || entry.result == crate::model::VerifyResult::NotAdmitted
        {
            continue;
        }
        latest.insert(gate, entry.result);
    }
    !latest.is_empty()
        && latest
            .values()
            .all(|result| *result == crate::model::VerifyResult::Pass)
}

pub(crate) fn counts(series: &Series, rows: &[ItemRow]) -> Counts {
    let mut counts = Counts {
        since_start: series.since_start(),
        ..Counts::default()
    };
    for (item, row) in series.items.iter().zip(rows) {
        if !item.stands() {
            continue;
        }
        counts.items += 1;
        if row.waiting.is_some() {
            counts.waiting += 1;
        }
        if row.claimed.is_some() {
            counts.claimed += 1;
        }
        match row.stage {
            "integrated" => counts.integrated += 1,
            "done" => counts.done += 1,
            "closed" => counts.closed += 1,
            "open" => counts.open += 1,
            _ if row.in_flight() => counts.in_flight += 1,
            _ => counts.open += 1,
        }
    }
    counts.complete = counts.integrated + counts.done;
    counts.percent = (counts.items > 0).then(|| counts.complete * 100 / counts.items);
    counts
}

pub(crate) fn last_integration(rows: &[ItemRow]) -> Option<Integration> {
    rows.iter()
        .filter_map(|row| {
            row.integrated_at.as_ref().map(|at| Integration {
                item: row.id.clone(),
                at: at.clone(),
                revision: row.revision.clone(),
            })
        })
        .max_by(|left, right| left.at.cmp(&right.at))
}

pub(crate) fn series_report(
    series: &Series,
    reader: &Reader,
    relations: Vec<Relation>,
) -> Result<SeriesReport> {
    let rows = reader.rows(series)?;
    Ok(SeriesReport {
        file: series.file.clone(),
        title: series.title.clone(),
        follows: series.follows.clone(),
        started_at: series.started_at.clone(),
        retired: series.retired.clone(),
        related: relations,
        counts: counts(series, &rows),
        last_integration: last_integration(&rows),
        items: rows,
    })
}

/// One related series, resolved against its project's cached journal events.
struct RelatedRead {
    slug: String,
    label: String,
    result: std::result::Result<(Series, PathBuf), String>,
}

fn read_related(registry: &Registry, relation: &RelatedSeries) -> RelatedRead {
    let project = registry.get(&relation.project);
    let result = match project {
        None => Err(format!("no journal is registered as {}", relation.project)),
        Some(project) => match project.events() {
            Err(reason) => Err(reason.to_string()),
            Ok(events) => match series::replay(events, &relation.file) {
                Some(series) => Ok((series, project.journal_dir.clone())),
                None => Err(format!(
                    "{} holds no series {}",
                    relation.project, relation.file
                )),
            },
        },
    };
    RelatedRead {
        slug: relation.project.clone(),
        label: registry.label(&relation.project),
        result,
    }
}

fn related_report(
    registry: &Registry,
    read: RelatedRead,
    file: &str,
    from: Vec<String>,
    now: DateTime<Utc>,
) -> RelatedReport {
    let mut report = RelatedReport {
        project: read.label,
        slug: read.slug,
        file: file.to_string(),
        related_from: from,
        unavailable: None,
        title: None,
        retired: None,
        counts: None,
        last_integration: None,
        in_flight: Vec::new(),
    };
    match read.result {
        Err(reason) => report.unavailable = Some(reason),
        Ok((series, dir)) => {
            let none = Ledger::none(&dir);
            let project = registry.get(&report.slug);
            let home = project.map_or(&none, |project| &project.ledger);
            let events = project
                .and_then(|project| project.events().ok())
                .unwrap_or(&[]);
            let questions = PersonQuestions::new(&dir, events);
            let reader = Reader::new(
                home,
                &dir,
                Some(&report.slug),
                events,
                registry,
                &questions,
                now,
            );
            // A related series is read on the way, never relied on: a ledger
            // that cannot be read makes it unavailable, not the report.
            let rows = match reader.rows(&series) {
                Ok(rows) => rows,
                Err(error) => {
                    report.unavailable = Some(format!("{error:#}").replace(['\n', '\r'], " "));
                    return report;
                }
            };
            report.title = Some(series.title.clone());
            report.retired = series.retired.clone();
            report.counts = Some(counts(&series, &rows));
            report.last_integration = last_integration(&rows);
            report.in_flight = rows.into_iter().filter(ItemRow::in_flight).collect();
        }
    }
    report
}

/// Build the report for this project: one series by name, or every active
/// one, with the related series they name.
pub(crate) fn build(ctx: &Ctx, name: Option<&str>) -> Result<ProgressReport> {
    let now = Utc::now();
    let dir = crate::journal::resolve_dir(&ctx.cwd)?;
    let events = crate::journal::read_events(&dir)?;
    let chosen = match name {
        Some(name) => vec![series::named_in(&events, &dir, name)?],
        None => series::active_in(&events),
    };
    let ledger = Ledger::local(ctx);
    let registry = if needs_registry(&chosen) {
        Registry::read()?
    } else {
        Registry::default()
    };
    let own = registry.slug_of(&dir).map(str::to_string);
    let local_events;
    let events: &[JournalEvent] = if registry.by_dir(&dir).is_some() {
        registry.seed_events(&dir, events);
        registry
            .by_dir(&dir)
            .and_then(|project| project.events().ok())
            .unwrap_or(&[])
    } else {
        local_events = events;
        &local_events
    };
    let questions = PersonQuestions::new(&dir, events);
    let reader = Reader::new(
        &ledger,
        &dir,
        own.as_deref(),
        events,
        &registry,
        &questions,
        now,
    );
    let mut reads: Vec<(RelatedSeries, RelatedRead, Vec<String>)> = Vec::new();
    let mut reports = Vec::new();
    for series in &chosen {
        let mut relations = Vec::new();
        for relation in &series.related {
            let index = match reads.iter().position(|(known, _, _)| known == relation) {
                Some(index) => index,
                None => {
                    reads.push((
                        relation.clone(),
                        read_related(&registry, relation),
                        Vec::new(),
                    ));
                    reads.len() - 1
                }
            };
            let (_, read, from) = &mut reads[index];
            from.push(series.file.clone());
            let mutual = read.result.as_ref().ok().map(|(other, _)| {
                own.as_deref().is_some_and(|slug| {
                    other
                        .related
                        .iter()
                        .any(|back| back.project == slug && back.file == series.file)
                })
            });
            relations.push(Relation {
                project: read.label.clone(),
                slug: relation.project.clone(),
                file: relation.file.clone(),
                mutual,
            });
        }
        reports.push(series_report(series, &reader, relations)?);
    }
    let related = reads
        .into_iter()
        .map(|(relation, read, from)| related_report(&registry, read, &relation.file, from, now))
        .collect();
    let waiting_on_person = questions
        .all()?
        .iter()
        .map(|question| WaitingQuestion {
            file: question.file.clone(),
            question: question.question.clone(),
            heading: question.heading.clone(),
            asked_at: question.asked_at.clone(),
        })
        .collect();
    Ok(ProgressReport {
        schema: PROGRESS_SCHEMA,
        journal_dir: dir.display().to_string(),
        series: reports,
        related,
        waiting_on_person,
    })
}

/// One project's active series as the workspace reports them: each in full
/// from the project's own journal and ledger, its relations named without
/// reading the related series, so whether one is mutual is left absent.
pub(crate) fn project_series(
    journal_dir: &Path,
    anchor: Option<&Path>,
    now: DateTime<Utc>,
) -> Result<Vec<SeriesReport>> {
    series_of(journal_dir, anchor, None, now)
}

/// [`project_series`] through `shared`, the registry the caller already
/// read with this project's slug in it. Without it the registry is read, and
/// only when an item or a relation names another project.
pub(crate) fn series_of(
    journal_dir: &Path,
    anchor: Option<&Path>,
    shared: Option<(&Registry, &str)>,
    now: DateTime<Utc>,
) -> Result<Vec<SeriesReport>> {
    let owned_events;
    let events: &[JournalEvent] =
        match shared.and_then(|(registry, _)| registry.by_dir(journal_dir)) {
            Some(project) => project
                .events()
                .map_err(|reason| anyhow::anyhow!("{reason}"))?,
            None => {
                owned_events = crate::journal::read_events(journal_dir)?;
                &owned_events
            }
        };
    let active = series::active_in(events);
    if active.is_empty() {
        return Ok(Vec::new());
    }
    let read;
    let (registry, home_slug) = match shared {
        Some((registry, slug)) => (registry, registry.get(slug).map(|_| slug.to_string())),
        None => {
            read = if needs_registry(&active) {
                Registry::read()?
            } else {
                Registry::default()
            };
            let slug = read.slug_of(journal_dir).map(str::to_string);
            (&read, slug)
        }
    };
    let own;
    let home = match registry.by_dir(journal_dir) {
        Some(project) => &project.ledger,
        None => {
            own = match anchor {
                Some(anchor) => Ledger::at(anchor),
                None => Ledger::none(journal_dir),
            };
            &own
        }
    };
    let questions = PersonQuestions::new(journal_dir, events);
    let reader = Reader::new(
        home,
        journal_dir,
        home_slug.as_deref(),
        events,
        registry,
        &questions,
        now,
    );
    active
        .iter()
        .map(|series| {
            let relations = series
                .related
                .iter()
                .map(|relation| Relation {
                    project: registry.label(&relation.project),
                    slug: relation.project.clone(),
                    file: relation.file.clone(),
                    mutual: None,
                })
                .collect();
            series_report(series, &reader, relations)
        })
        .collect()
}

/// Fill each relation's `mutual` from the series this command read: `true`
/// when the related series relates back, `false` when it was read and does
/// not, and absent when it was not read here.
pub(crate) fn settle_mutual(projects: &mut [ProjectProgress]) {
    let read: BTreeSet<(String, String)> = projects
        .iter()
        .flat_map(|project| {
            project
                .series
                .iter()
                .map(|series| (project.slug.clone(), series.file.clone()))
        })
        .collect();
    let relates: BTreeSet<(String, String, String, String)> = projects
        .iter()
        .flat_map(|project| {
            project.series.iter().flat_map(move |series| {
                series.related.iter().map(move |relation| {
                    (
                        project.slug.clone(),
                        series.file.clone(),
                        relation.slug.clone(),
                        relation.file.clone(),
                    )
                })
            })
        })
        .collect();
    for project in projects.iter_mut() {
        for series in &mut project.series {
            for relation in &mut series.related {
                relation.mutual = read
                    .contains(&(relation.slug.clone(), relation.file.clone()))
                    .then(|| {
                        relates.contains(&(
                            relation.slug.clone(),
                            relation.file.clone(),
                            project.slug.clone(),
                            series.file.clone(),
                        ))
                    });
            }
        }
    }
}

/// Whether any item of `series`, as [`project_series`] reports them, follows
/// a change of one of `projects`, named by registry slug.
pub(crate) fn links_into(series: &serde_json::Value, projects: &BTreeSet<&str>) -> bool {
    !projects.is_empty()
        && series.as_array().into_iter().flatten().any(|series| {
            series["items"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|item| {
                    item["project_slug"]
                        .as_str()
                        .is_some_and(|slug| projects.contains(slug))
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered_for(slug: &str, dir: PathBuf) -> Registered {
        Registered {
            slug: slug.to_string(),
            label: slug.to_string(),
            ledger: Ledger::none(&dir),
            journal_dir: dir,
            events: OnceCell::new(),
        }
    }

    /// A registered project caches its journal event read once per report:
    /// removing `events.jsonl` after the first read still returns the cached
    /// events, and seeding events avoids touching disk at all. Rejects a
    /// reader that re-opens `events.jsonl` on every series or item reference.
    #[test]
    fn registered_project_caches_journal_events_once_per_report() {
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");
        let line = serde_json::json!({
            "schema": "journal-events/2",
            "ts": "2026-01-02T00:00:00Z",
            "topic": "alpha",
            "event": "note",
            "file": "20260102T000000Z-alpha-progress.md",
            "title": "Alpha"
        });
        std::fs::write(&events_path, format!("{line}\n")).unwrap();

        let project = registered_for("alpha", dir.path().to_path_buf());
        let first = project.events().unwrap();
        assert_eq!(first.len(), 1);

        // Removing the file proves a second access uses the cached events
        // rather than re-reading disk.
        std::fs::remove_file(&events_path).unwrap();
        let second = project.events().unwrap();
        assert_eq!(second.len(), 1);

        // Seeding events for a nonexistent directory likewise avoids disk I/O.
        let seeded = Registry {
            projects: vec![registered_for("beta", dir.path().join("absent"))],
        };
        seeded.seed_events(&dir.path().join("absent"), first.to_vec());
        assert_eq!(seeded.get("beta").unwrap().events().unwrap().len(), 1);
    }

    /// Claim: two configured journals sharing a basename-derived slug each read
    /// their own journal events via canonical directory lookup, never colliding
    /// on the first registration's cache.
    #[test]
    fn duplicate_basename_slugs_never_alias_each_others_events() {
        let root = tempfile::tempdir().unwrap();
        let alpha_dir = root.path().join("a").join("shared");
        let beta_dir = root.path().join("b").join("shared");
        std::fs::create_dir_all(&alpha_dir).unwrap();
        std::fs::create_dir_all(&beta_dir).unwrap();
        let write_series = |dir: &Path, topic: &str, title: &str| {
            let line = serde_json::json!({
                "schema": "journal-events/2",
                "ts": "2026-01-02T00:00:00Z",
                "topic": topic,
                "event": "note",
                "file": format!("20260102T000000Z-{topic}-progress.md"),
                "title": title,
            });
            std::fs::write(dir.join("events.jsonl"), format!("{line}\n")).unwrap();
        };
        write_series(&alpha_dir, "alpha", "Alpha");
        write_series(&beta_dir, "beta", "Beta");

        let registry = Registry {
            projects: vec![
                registered_for("shared", alpha_dir.clone()),
                registered_for("shared", beta_dir.clone()),
            ],
        };
        // Ambiguous slug lookup returns None rather than aliasing.
        assert!(registry.get("shared").is_none());
        assert!(registry.slug_of(&alpha_dir).is_none());
        assert!(registry.slug_of(&beta_dir).is_none());

        let now = Utc::now();
        let alpha = series_of(&alpha_dir, None, Some((&registry, "shared")), now).unwrap();
        let beta = series_of(&beta_dir, None, Some((&registry, "shared")), now).unwrap();
        assert_eq!(alpha[0].title, "Alpha");
        assert_eq!(beta[0].title, "Beta");
    }
}
