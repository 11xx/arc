//! Wave-aligned matrix projection for `arc milestone`.
//!
//! Composes existing `arc progress` series through the shared read-only
//! [`crate::commands::progress::reader`] layer so each referenced project's
//! journal event log and ledger open at most once per report. Unreadable
//! foreign tracks degrade to `unavailable` without failing the milestone view.

use crate::commands::progress::reader::{
    Blocker, ItemRow, Ledger, PersonQuestions, Reader, Registry,
};
use crate::commands::Ctx;
use crate::journal::{
    self,
    milestone::{self, Closure, Milestone, MilestoneCheckpoint, Track, TrackRole},
    series, JournalEvent,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The schema `arc milestone --json` prints.
const MILESTONE_SCHEMA: &str = "arc-milestone/1";

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct MatrixCounts {
    pub(crate) items: usize,
    pub(crate) complete: usize,
    pub(crate) integrated: usize,
    pub(crate) done: usize,
    pub(crate) in_flight: usize,
    pub(crate) open: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) ready: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) blocked: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) claimed: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) closed: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) waiting: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) percent: Option<usize>,
}

impl MatrixCounts {
    fn add_row(&mut self, row: &ItemRow) {
        if matches!(row.stage, "dropped" | "split" | "transferred") {
            return;
        }
        self.items += 1;
        if row.waiting.is_some() {
            self.waiting += 1;
        }
        if row.claimed.is_some() {
            self.claimed += 1;
        }
        if row.readiness == "ready" {
            self.ready += 1;
        } else if row.readiness == "blocked" {
            self.blocked += 1;
        }
        match row.stage {
            "integrated" => self.integrated += 1,
            "done" => self.done += 1,
            "closed" => self.closed += 1,
            "open" => self.open += 1,
            _ if row.in_flight() => self.in_flight += 1,
            _ => self.open += 1,
        }
    }

    fn finish(&mut self) {
        self.complete = self.integrated + self.done;
        self.percent = (self.items > 0).then(|| self.complete * 100 / self.items);
    }

    fn merge(&mut self, other: &Self) {
        self.items += other.items;
        self.integrated += other.integrated;
        self.done += other.done;
        self.in_flight += other.in_flight;
        self.open += other.open;
        self.ready += other.ready;
        self.blocked += other.blocked;
        self.claimed += other.claimed;
        self.closed += other.closed;
        self.waiting += other.waiting;
        self.finish();
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct MilestoneCounts {
    /// Counts across `critical` tracks only; `critical.percent` is milestone completion.
    pub(crate) critical: MatrixCounts,
    /// Counts across `sideband` tracks only; never included in milestone completion.
    pub(crate) sideband: MatrixCounts,
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) unavailable_tracks: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct WaveReport {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    /// Per-wave gating override recorded on the milestone, if any.
    pub(crate) gated: Option<bool>,
    /// Effective barrier state after applying milestone default and `--ignore-waves`.
    pub(crate) effective_gated: bool,
    pub(crate) critical: MatrixCounts,
    pub(crate) sideband: MatrixCounts,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct WaveCell {
    /// Wave identifier, or `None` (`null` in JSON) for items with no wave
    /// assignment, kept distinct from a declared wave named `"unassigned"`.
    pub(crate) wave: Option<String>,
    /// Whether this wave is declared on the milestone.
    pub(crate) declared: bool,
    pub(crate) counts: MatrixCounts,
    pub(crate) items: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct TrackReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project_label: Option<String>,
    pub(crate) file: String,
    pub(crate) role: TrackRole,
    pub(crate) label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) retired: Option<series::Retirement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unavailable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) counts: Option<MatrixCounts>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) waves: Vec<WaveCell>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) items: Vec<ItemRow>,
}

impl TrackReport {
    pub(crate) fn display_name(&self) -> String {
        if let Some(label) = &self.label {
            return label.clone();
        }
        let topic = journal::parse_artifact_name(&self.file)
            .map(|(_, topic, _)| topic)
            .unwrap_or_else(|| self.file.clone());
        match &self.project_label {
            Some(project) => format!("{project}::{topic}"),
            None => match &self.project {
                Some(slug) => format!("{slug}::{topic}"),
                None => topic,
            },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ReadyItem {
    pub(crate) track: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) track_label: Option<String>,
    pub(crate) role: TrackRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) wave: Option<String>,
    pub(crate) item: String,
    pub(crate) title: String,
}

#[derive(Serialize)]
pub(crate) struct MilestoneReport {
    pub(crate) file: String,
    pub(crate) title: String,
    pub(crate) follows: Option<String>,
    pub(crate) advisory_waves: bool,
    pub(crate) waves: Vec<WaveReport>,
    pub(crate) tracks: Vec<TrackReport>,
    pub(crate) counts: MilestoneCounts,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) ready: Vec<ReadyItem>,
    pub(crate) checkpoints: Vec<MilestoneCheckpoint>,
    pub(crate) closure: Option<Closure>,
}

#[derive(Serialize)]
pub(crate) struct MilestonesDocument {
    pub(crate) schema: &'static str,
    pub(crate) journal_dir: String,
    pub(crate) milestones: Vec<MilestoneReport>,
}

/// Build the derived matrix report for one named milestone or every open
/// milestone in this project's journal.
pub(crate) fn build(
    ctx: &Ctx,
    name: Option<&str>,
    ready_only: bool,
    ignore_waves: bool,
) -> Result<MilestonesDocument> {
    let now = Utc::now();
    let dir = journal::resolve_dir(&ctx.cwd)?;
    let raw_events = journal::read_events(&dir)?;
    let chosen = match name {
        Some(name) => vec![milestone::named_in(&raw_events, &dir, name)?],
        None => milestone::active_in(&raw_events),
    };
    let ledger = Ledger::local(ctx);
    let milestones = build_in(
        &dir,
        raw_events,
        &ledger,
        &chosen,
        ready_only,
        ignore_waves,
        now,
    )?;
    Ok(MilestonesDocument {
        schema: MILESTONE_SCHEMA,
        journal_dir: dir.display().to_string(),
        milestones,
    })
}

/// Build milestone reports using a caller-supplied shared [`Registry`],
/// already-read local journal events, and local [`Ledger`] so callers such as
/// `arc checkpoint record` share caches across milestone tracks and explicit
/// `--series` bindings without reopening foreign journals or ledgers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_with_registry(
    dir: &Path,
    raw_events: &[JournalEvent],
    local_ledger: &Ledger,
    registry: &Registry,
    name: Option<&str>,
    ready_only: bool,
    ignore_waves: bool,
    now: DateTime<Utc>,
) -> Result<Vec<MilestoneReport>> {
    let chosen = match name {
        Some(name) => vec![milestone::named_in(raw_events, dir, name)?],
        None => milestone::active_in(raw_events),
    };
    let local_questions = PersonQuestions::new(dir, raw_events);
    let local_reader = Reader::new(
        local_ledger,
        dir,
        None,
        raw_events,
        registry,
        &local_questions,
        now,
    );
    let mut reports = Vec::with_capacity(chosen.len());
    for ms in &chosen {
        reports.push(build_one(
            ms,
            dir,
            raw_events,
            &local_reader,
            registry,
            None,
            ready_only,
            ignore_waves,
            now,
        ));
    }
    Ok(reports)
}

fn build_in(
    dir: &Path,
    raw_events: Vec<JournalEvent>,
    local_ledger: &Ledger,
    chosen: &[Milestone],
    ready_only: bool,
    ignore_waves: bool,
    now: DateTime<Utc>,
) -> Result<Vec<MilestoneReport>> {
    let registry = Registry::lazy();
    registry.seed_events(dir, raw_events.clone());
    let local_questions = PersonQuestions::new(dir, &raw_events);
    let local_reader = Reader::new(
        local_ledger,
        dir,
        None,
        &raw_events,
        &registry,
        &local_questions,
        now,
    );

    let mut reports = Vec::with_capacity(chosen.len());
    for ms in chosen {
        reports.push(build_one(
            ms,
            dir,
            &raw_events,
            &local_reader,
            &registry,
            None,
            ready_only,
            ignore_waves,
            now,
        ));
    }
    Ok(reports)
}

struct ReadTrack {
    meta: Track,
    project_label: Option<String>,
    title: Option<String>,
    retired: Option<series::Retirement>,
    unavailable: Option<String>,
    rows: Vec<ItemRow>,
}

#[allow(clippy::too_many_arguments)]
fn build_one(
    ms: &Milestone,
    home_dir: &Path,
    local_events: &[JournalEvent],
    local_reader: &Reader,
    registry: &Registry,
    own_slug: Option<&str>,
    ready_only: bool,
    ignore_waves: bool,
    now: DateTime<Utc>,
) -> MilestoneReport {
    // 1. Read every attached track once, degrading unreadable tracks gracefully.
    let mut tracks: Vec<ReadTrack> = Vec::with_capacity(ms.tracks.len());
    for track in &ms.tracks {
        tracks.push(read_track(
            track,
            home_dir,
            local_events,
            local_reader,
            registry,
            own_slug,
            now,
        ));
    }

    // 2. When `--ignore-waves` is passed, drop series-level wave barriers too
    //    (keeping explicit `prereq` and `cycle` blockers intact).
    if ignore_waves {
        for track in &mut tracks {
            for row in &mut track.rows {
                if row.blocked_by.iter().any(|b| b.kind == "wave") {
                    row.blocked_by.retain(|b| b.kind != "wave");
                    recompute_readiness(row);
                }
            }
        }
    }

    // 3. Apply cross-track critical wave barriers across milestone waves.
    let wave_index: BTreeMap<&str, usize> = ms
        .waves
        .iter()
        .enumerate()
        .map(|(idx, wave)| (wave.id.as_str(), idx))
        .collect();

    if !ignore_waves && !ms.waves.is_empty() {
        // Snapshot incomplete critical items per declared wave index and
        // unavailable critical tracks before mutating rows.
        let mut incomplete_by_wave: Vec<Vec<Blocker>> = vec![Vec::new(); ms.waves.len()];
        let mut unavailable_critical: Vec<Blocker> = Vec::new();
        for track in &tracks {
            if track.meta.role != TrackRole::Critical {
                continue;
            }
            if let Some(reason) = &track.unavailable {
                unavailable_critical.push(Blocker {
                    kind: "wave",
                    project: track.project_label.clone(),
                    project_slug: track.meta.reference.project.clone(),
                    series: Some(track.meta.reference.file.clone()),
                    item: None,
                    wave: None,
                    stage: "unknown",
                    reason: Some(format!("critical track unavailable: {reason}")),
                });
                continue;
            }
            for row in &track.rows {
                if matches!(
                    row.stage,
                    "integrated" | "done" | "dropped" | "split" | "transferred"
                ) {
                    continue;
                }
                let Some(wave_id) = row.wave.as_deref() else {
                    continue;
                };
                let Some(&idx) = wave_index.get(wave_id) else {
                    continue;
                };
                incomplete_by_wave[idx].push(Blocker {
                    kind: "wave",
                    project: track.project_label.clone(),
                    project_slug: track.meta.reference.project.clone(),
                    series: Some(track.meta.reference.file.clone()),
                    item: Some(row.id.clone()),
                    wave: Some(wave_id.to_string()),
                    stage: row.stage,
                    reason: None,
                });
            }
        }

        for track in &mut tracks {
            if track.meta.role != TrackRole::Critical || track.unavailable.is_some() {
                continue;
            }
            let track_proj = track.meta.reference.project.as_deref();
            let track_file = track.meta.reference.file.as_str();
            for row in &mut track.rows {
                if matches!(
                    row.stage,
                    "integrated" | "done" | "dropped" | "split" | "transferred"
                ) || row.in_flight()
                {
                    continue;
                }
                let Some(wave_id) = row.wave.as_deref() else {
                    continue;
                };
                let Some(&idx) = wave_index.get(wave_id) else {
                    continue;
                };
                let effective_gated = ms.waves[idx].gated.unwrap_or(!ms.advisory_waves);
                if !effective_gated || idx == 0 {
                    continue;
                }
                let mut changed = false;
                for earlier_blockers in &incomplete_by_wave[..idx] {
                    for candidate in earlier_blockers {
                        let same_series = candidate.project_slug.as_deref() == track_proj
                            && candidate.series.as_deref() == Some(track_file);
                        let duplicate = row.blocked_by.iter().any(|existing| {
                            existing.kind == "wave"
                                && existing.wave == candidate.wave
                                && existing.item == candidate.item
                                && (existing.series == candidate.series
                                    || (same_series && existing.series.is_none()))
                        });
                        if !duplicate {
                            let mut blocker = candidate.clone();
                            if same_series {
                                blocker.project = None;
                                blocker.project_slug = None;
                                blocker.series = None;
                            }
                            row.blocked_by.push(blocker);
                            changed = true;
                        }
                    }
                }
                for unavail in &unavailable_critical {
                    if !row.blocked_by.contains(unavail) {
                        row.blocked_by.push(unavail.clone());
                        changed = true;
                    }
                }
                if changed {
                    recompute_readiness(row);
                }
            }
        }
    }

    // 4. Compute wave-level, track-level, and milestone-level critical/sideband counts.
    let declared_wave_ids: BTreeSet<&str> = ms.waves.iter().map(|w| w.id.as_str()).collect();
    let mut wave_reports: Vec<WaveReport> = ms
        .waves
        .iter()
        .map(|w| WaveReport {
            id: w.id.clone(),
            title: w.title.clone(),
            gated: w.gated,
            effective_gated: !ignore_waves && w.gated.unwrap_or(!ms.advisory_waves),
            critical: MatrixCounts::default(),
            sideband: MatrixCounts::default(),
        })
        .collect();

    let mut ms_counts = MilestoneCounts::default();
    let mut ready_items = Vec::new();
    let mut track_reports = Vec::with_capacity(tracks.len());

    for track in tracks {
        if let Some(reason) = track.unavailable {
            ms_counts.unavailable_tracks += 1;
            track_reports.push(TrackReport {
                project: track.meta.reference.project,
                project_label: track.project_label,
                file: track.meta.reference.file,
                role: track.meta.role,
                label: track.meta.label,
                title: track.title,
                retired: track.retired,
                unavailable: Some(reason),
                counts: None,
                waves: Vec::new(),
                items: Vec::new(),
            });
            continue;
        }

        let mut track_counts = MatrixCounts::default();
        let mut cells_by_wave: BTreeMap<Option<String>, (bool, MatrixCounts, Vec<String>)> =
            BTreeMap::new();
        for w in &ms.waves {
            cells_by_wave.insert(
                Some(w.id.clone()),
                (true, MatrixCounts::default(), Vec::new()),
            );
        }

        for row in &track.rows {
            track_counts.add_row(row);
            let wave_key = row.wave.clone();
            let declared = wave_key
                .as_deref()
                .is_some_and(|w| declared_wave_ids.contains(w));
            let entry = cells_by_wave
                .entry(wave_key.clone())
                .or_insert_with(|| (declared, MatrixCounts::default(), Vec::new()));
            entry.1.add_row(row);
            if !matches!(row.stage, "dropped" | "split" | "transferred") {
                entry.2.push(row.id.clone());
            }
            if let Some(wave_id) = wave_key.as_deref() {
                if let Some(&idx) = wave_index.get(wave_id) {
                    match track.meta.role {
                        TrackRole::Critical => wave_reports[idx].critical.add_row(row),
                        TrackRole::Sideband => wave_reports[idx].sideband.add_row(row),
                    }
                }
            }
            if row.readiness == "ready" {
                ready_items.push(ReadyItem {
                    track: track.meta.reference.file.clone(),
                    track_label: track.meta.label.clone(),
                    role: track.meta.role,
                    project: track.project_label.clone(),
                    wave: row.wave.clone(),
                    item: row.id.clone(),
                    title: row.title.clone(),
                });
            }
        }
        track_counts.finish();
        match track.meta.role {
            TrackRole::Critical => ms_counts.critical.merge(&track_counts),
            TrackRole::Sideband => ms_counts.sideband.merge(&track_counts),
        }

        // Emit wave cells in milestone wave declaration order, followed by any
        // undeclared track waves and unassigned (`None`) items (only when non-empty).
        let mut cells = Vec::new();
        for w in &ms.waves {
            if let Some((declared, mut c, items)) = cells_by_wave.remove(&Some(w.id.clone())) {
                c.finish();
                cells.push(WaveCell {
                    wave: Some(w.id.clone()),
                    declared,
                    counts: c,
                    items,
                });
            }
        }
        let unassigned = cells_by_wave.remove(&None);
        for (wave_key, (declared, mut c, items)) in cells_by_wave {
            if c.items > 0 {
                c.finish();
                cells.push(WaveCell {
                    wave: wave_key,
                    declared,
                    counts: c,
                    items,
                });
            }
        }
        if let Some((declared, mut c, items)) = unassigned {
            if c.items > 0 {
                c.finish();
                cells.push(WaveCell {
                    wave: None,
                    declared,
                    counts: c,
                    items,
                });
            }
        }

        let mut items = track.rows;
        if ready_only {
            items.retain(|row| row.readiness == "ready");
        }
        track_reports.push(TrackReport {
            project: track.meta.reference.project,
            project_label: track.project_label,
            file: track.meta.reference.file,
            role: track.meta.role,
            label: track.meta.label,
            title: track.title,
            retired: track.retired,
            unavailable: None,
            counts: Some(track_counts),
            waves: cells,
            items,
        });
    }

    for wave in &mut wave_reports {
        wave.critical.finish();
        wave.sideband.finish();
    }

    MilestoneReport {
        file: ms.file.clone(),
        title: ms.title.clone(),
        follows: ms.follows.clone(),
        advisory_waves: ms.advisory_waves,
        waves: wave_reports,
        tracks: track_reports,
        counts: ms_counts,
        ready: ready_items,
        checkpoints: ms.checkpoints.clone(),
        closure: ms.closure.clone(),
    }
}

fn read_track(
    track: &Track,
    home_dir: &Path,
    local_events: &[JournalEvent],
    local_reader: &Reader,
    registry: &Registry,
    own_slug: Option<&str>,
    now: DateTime<Utc>,
) -> ReadTrack {
    let foreign_slug = track.reference.project.as_deref().filter(|slug| {
        if own_slug == Some(*slug) {
            return false;
        }
        // Normalize an explicit project slug to home only when that slug
        // uniquely resolves to `home_dir`; an ambiguous duplicate-basename
        // slug stays foreign so `registry.get(slug)` reports it unavailable.
        registry.slug_of(home_dir) != Some(*slug)
    });

    match foreign_slug {
        None => match series::replay(local_events, &track.reference.file) {
            None => ReadTrack {
                meta: track.clone(),
                project_label: None,
                title: None,
                retired: None,
                unavailable: Some(format!("no series {}", track.reference.file)),
                rows: Vec::new(),
            },
            Some(series) => match local_reader.rows(&series) {
                Ok(rows) => ReadTrack {
                    meta: track.clone(),
                    project_label: None,
                    title: Some(series.title),
                    retired: series.retired,
                    unavailable: None,
                    rows,
                },
                Err(error) => ReadTrack {
                    meta: track.clone(),
                    project_label: None,
                    title: Some(series.title),
                    retired: series.retired,
                    unavailable: Some(format!("{error:#}").replace(['\n', '\r'], " ")),
                    rows: Vec::new(),
                },
            },
        },
        Some(slug) => {
            let label = Some(registry.label(slug));
            let Some(project) = registry.get(slug) else {
                return ReadTrack {
                    meta: track.clone(),
                    project_label: label,
                    title: None,
                    retired: None,
                    unavailable: Some(format!("no journal is registered as {slug}")),
                    rows: Vec::new(),
                };
            };
            let events = match project.events() {
                Ok(events) => events,
                Err(reason) => {
                    return ReadTrack {
                        meta: track.clone(),
                        project_label: label,
                        title: None,
                        retired: None,
                        unavailable: Some(reason.to_string()),
                        rows: Vec::new(),
                    };
                }
            };
            let Some(series) = series::replay(events, &track.reference.file) else {
                return ReadTrack {
                    meta: track.clone(),
                    project_label: label,
                    title: None,
                    retired: None,
                    unavailable: Some(format!("{slug} holds no series {}", track.reference.file)),
                    rows: Vec::new(),
                };
            };
            let questions = PersonQuestions::new(&project.journal_dir, events);
            let reader = Reader::new(
                &project.ledger,
                &project.journal_dir,
                Some(slug),
                events,
                registry,
                &questions,
                now,
            );
            match reader.rows(&series) {
                Ok(rows) => ReadTrack {
                    meta: track.clone(),
                    project_label: label,
                    title: Some(series.title),
                    retired: series.retired,
                    unavailable: None,
                    rows,
                },
                Err(error) => ReadTrack {
                    meta: track.clone(),
                    project_label: label,
                    title: Some(series.title),
                    retired: series.retired,
                    unavailable: Some(format!("{error:#}").replace(['\n', '\r'], " ")),
                    rows: Vec::new(),
                },
            }
        }
    }
}

fn recompute_readiness(row: &mut ItemRow) {
    if matches!(row.stage, "dropped" | "split" | "transferred") {
        row.readiness = "inactive";
    } else if matches!(row.stage, "integrated" | "done") {
        row.readiness = "complete";
    } else if row.in_flight() {
        row.readiness = "in-flight";
    } else if !row.blocked_by.is_empty() {
        row.readiness = "blocked";
    } else if row.waiting.is_some() {
        row.readiness = "waiting";
    } else if row.claimed.is_some() {
        row.readiness = "claimed";
    } else if row.stage == "open" && row.change.is_none() {
        row.readiness = "ready";
    } else {
        row.readiness = "blocked";
    }
}
