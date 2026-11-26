//! Milestone waves, tracks, and checkpoint bindings replay in journal append order.

use super::{
    append_event, artifact_body_path, heading_from_topic, lock_journal_transition,
    parse_artifact_name, resolve_dir, series, valid_topic, JournalEvent,
};
use crate::commands::{checkpoint, Ctx};
use crate::store::Store;
use anyhow::{bail, ensure, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) const KIND: &str = "milestone";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct MilestoneFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_follows: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    milestone_advisory_waves: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_wave: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_wave_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_wave_before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_wave_after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_wave_gated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_track: Option<TrackRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_track_role: Option<TrackRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_track_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_track_before: Option<TrackRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    milestone_track_after: Option<TrackRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) milestone_checkpoint: Option<MilestoneCheckpoint>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct TrackRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) project: Option<String>,
    pub(crate) file: String,
}

impl std::fmt::Display for TrackRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(project) = &self.project {
            write!(f, "{project}::")?;
        }
        f.write_str(&self.file)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TrackRole {
    #[default]
    Critical,
    Sideband,
}

impl TrackRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Sideband => "sideband",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct MilestoneCheckpoint {
    pub(crate) name: String,
    pub(crate) revision: String,
    pub(crate) event_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Wave {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    /// A per-wave override of the milestone's default.
    pub(crate) gated: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Track {
    #[serde(flatten)]
    pub(crate) reference: TrackRef,
    pub(crate) role: TrackRole,
    pub(crate) label: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Closure {
    pub(crate) at: String,
    pub(crate) outcome: String,
    pub(crate) by: Option<String>,
    pub(crate) note: Option<String>,
    pub(crate) checkpoint: Option<MilestoneCheckpoint>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Milestone {
    pub(crate) file: String,
    pub(crate) title: String,
    pub(crate) follows: Option<String>,
    pub(crate) advisory_waves: bool,
    pub(crate) waves: Vec<Wave>,
    pub(crate) tracks: Vec<Track>,
    pub(crate) checkpoints: Vec<MilestoneCheckpoint>,
    pub(crate) closure: Option<Closure>,
}

pub(crate) fn is_milestone_file(file: &str) -> bool {
    series::bare_artifact_kind(file).as_deref() == Some(KIND)
}

fn valid_wave_id(id: &str) -> bool {
    id.len() <= 32
        && id.split(['-', '_']).all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn valid_track(reference: &TrackRef) -> bool {
    series::bare_artifact_kind(&reference.file).as_deref() == Some(series::KIND)
        && reference.project.as_deref().is_none_or(|project| {
            series::single_line(project) && !project.contains(['/', '\\', ':', '\0'])
        })
}

fn valid_placement<T: ?Sized>(
    before: Option<&T>,
    after: Option<&T>,
    required: bool,
    valid: impl Fn(&T) -> bool,
) -> bool {
    match (before, after) {
        (Some(anchor), None) | (None, Some(anchor)) => valid(anchor),
        (None, None) => !required,
        (Some(_), Some(_)) => false,
    }
}

pub(super) fn well_formed(event: &JournalEvent) -> bool {
    let f = &event.milestone;
    if !event.file.as_deref().is_some_and(is_milestone_file) {
        return false;
    }
    let wave = f.milestone_wave.as_deref().is_some_and(valid_wave_id);
    let track = f.milestone_track.as_ref().is_some_and(valid_track);
    let wave_placed = |required| {
        valid_placement(
            f.milestone_wave_before.as_deref(),
            f.milestone_wave_after.as_deref(),
            required,
            valid_wave_id,
        )
    };
    let track_placed = |required| {
        valid_placement(
            f.milestone_track_before.as_ref(),
            f.milestone_track_after.as_ref(),
            required,
            valid_track,
        )
    };
    let checkpoint = |binding: &MilestoneCheckpoint| {
        [&binding.name, &binding.revision, &binding.event_id]
            .into_iter()
            .all(|value| series::single_line(value))
    };
    match event.event.as_str() {
        "note" => {
            event.title.as_deref().is_none_or(series::single_line)
                && f.milestone_follows
                    .as_deref()
                    .is_none_or(|file| series::bare_artifact_kind(file).as_deref() == Some("plan"))
        }
        "milestone-wave-added" => {
            wave && wave_placed(false)
                && f.milestone_wave_title
                    .as_deref()
                    .is_none_or(series::single_line)
        }
        "milestone-wave-renamed" => {
            wave && f
                .milestone_wave_title
                .as_deref()
                .is_some_and(series::single_line)
        }
        "milestone-wave-moved" => wave && wave_placed(true),
        "milestone-wave-removed" => wave,
        "milestone-track-added" => {
            track
                && track_placed(false)
                && f.milestone_track_label
                    .as_deref()
                    .is_none_or(series::single_line)
        }
        "milestone-track-role-set" => track && f.milestone_track_role.is_some(),
        "milestone-track-moved" => track && track_placed(true),
        "milestone-track-removed" => track,
        "milestone-checkpoint-bound" => f.milestone_checkpoint.as_ref().is_some_and(checkpoint),
        "consumed" => f.milestone_checkpoint.as_ref().is_none_or(checkpoint),
        _ => false,
    }
}

pub(super) fn event_message(event: &JournalEvent) -> String {
    let fields = &event.milestone;
    let file = event.file.as_deref().unwrap_or_default();
    let subject = if let Some(track) = &fields.milestone_track {
        track.to_string()
    } else if let Some(binding) = &fields.milestone_checkpoint {
        binding.name.clone()
    } else {
        fields.milestone_wave.clone().unwrap_or_default()
    };
    format!(
        "{} {subject} on {file}",
        event
            .event
            .trim_start_matches("milestone-")
            .replace('-', " ")
    )
}

#[derive(Clone, Copy)]
pub(crate) enum Placement<T> {
    End,
    Before(T),
    After(T),
}

impl<T> Placement<T> {
    pub(crate) fn from_fields(before: Option<T>, after: Option<T>) -> Self {
        match (before, after) {
            (Some(anchor), _) => Self::Before(anchor),
            (None, Some(anchor)) => Self::After(anchor),
            (None, None) => Self::End,
        }
    }

    fn map<U>(self, resolve: impl FnOnce(T) -> Result<U>) -> Result<Placement<U>> {
        Ok(match self {
            Self::End => Placement::End,
            Self::Before(anchor) => Placement::Before(resolve(anchor)?),
            Self::After(anchor) => Placement::After(resolve(anchor)?),
        })
    }

    fn fields(self) -> (Option<T>, Option<T>) {
        match self {
            Self::End => (None, None),
            Self::Before(anchor) => (Some(anchor), None),
            Self::After(anchor) => (None, Some(anchor)),
        }
    }
}

fn insert_at<T: Eq + std::fmt::Display>(
    mut keys: impl ExactSizeIterator<Item = T>,
    subject: T,
    placement: Placement<T>,
    moving: Option<usize>,
) -> Result<usize> {
    let (anchor, offset) = match placement {
        Placement::End => return Ok(keys.len() - usize::from(moving.is_some())),
        Placement::Before(anchor) => (anchor, 0),
        Placement::After(anchor) => (anchor, 1),
    };
    ensure!(
        subject != anchor,
        "cannot place {subject} relative to itself"
    );
    let index = keys
        .position(|key| key == anchor)
        .with_context(|| format!("no placement anchor {anchor}"))?;
    Ok(index + offset - usize::from(moving.is_some_and(|from| from < index)))
}

impl Milestone {
    fn apply(&mut self, event: &JournalEvent) -> Result<()> {
        if event.event == "reopened" {
            self.closure = None;
            return Ok(());
        }
        if self.closure.is_some() {
            bail!("{} is closed and read-only", self.file);
        }
        let f = &event.milestone;
        match event.event.as_str() {
            kind if kind.starts_with("milestone-wave-") => self.apply_wave(kind, f),
            kind if kind.starts_with("milestone-track-") => self.apply_track(kind, f),
            "milestone-checkpoint-bound" | "consumed" => {
                if let Some(binding) = &f.milestone_checkpoint {
                    if !self.checkpoints.contains(binding) {
                        self.checkpoints.push(binding.clone());
                    }
                }
                if event.event == "consumed" {
                    self.closure = Some(Closure {
                        at: event.ts.clone(),
                        outcome: event.outcome.clone().unwrap_or_default(),
                        by: event.decision.clone(),
                        note: event.note.clone(),
                        checkpoint: f.milestone_checkpoint.clone(),
                    });
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn apply_wave(&mut self, kind: &str, f: &MilestoneFields) -> Result<()> {
        let id = f.milestone_wave.as_deref().context("missing wave id")?;
        let keys = || self.waves.iter().map(|wave| wave.id.as_str());
        let index = keys().position(|key| key == id);
        let placement = Placement::from_fields(
            f.milestone_wave_before.as_deref(),
            f.milestone_wave_after.as_deref(),
        );
        if kind == "milestone-wave-added" {
            ensure!(index.is_none(), "wave {id} already exists");
            let at = insert_at(keys(), id, placement, None)?;
            self.waves.insert(
                at,
                Wave {
                    id: id.to_string(),
                    title: f.milestone_wave_title.clone(),
                    gated: f.milestone_wave_gated,
                },
            );
        } else {
            let index = index.with_context(|| format!("no wave {id}"))?;
            match kind {
                "milestone-wave-renamed" => {
                    self.waves[index].title = f.milestone_wave_title.clone()
                }
                "milestone-wave-moved" => {
                    let at = insert_at(keys(), id, placement, Some(index))?;
                    let wave = self.waves.remove(index);
                    self.waves.insert(at, wave);
                }
                "milestone-wave-removed" => {
                    self.waves.remove(index);
                }
                _ => unreachable!("known wave event"),
            }
        }
        Ok(())
    }

    fn apply_track(&mut self, kind: &str, f: &MilestoneFields) -> Result<()> {
        let reference = f
            .milestone_track
            .as_ref()
            .context("missing track reference")?;
        let keys = || self.tracks.iter().map(|track| &track.reference);
        let index = keys().position(|key| key == reference);
        let placement = Placement::from_fields(
            f.milestone_track_before.as_ref(),
            f.milestone_track_after.as_ref(),
        );
        if kind == "milestone-track-added" {
            ensure!(index.is_none(), "track {reference} already exists");
            let at = insert_at(keys(), reference, placement, None)?;
            self.tracks.insert(
                at,
                Track {
                    reference: reference.clone(),
                    role: f.milestone_track_role.unwrap_or_default(),
                    label: f.milestone_track_label.clone(),
                },
            );
        } else {
            let index = index.with_context(|| format!("no track {reference}"))?;
            match kind {
                "milestone-track-role-set" => {
                    self.tracks[index].role =
                        f.milestone_track_role.context("missing track role")?
                }
                "milestone-track-moved" => {
                    let at = insert_at(keys(), reference, placement, Some(index))?;
                    let track = self.tracks.remove(index);
                    self.tracks.insert(at, track);
                }
                "milestone-track-removed" => {
                    self.tracks.remove(index);
                }
                _ => unreachable!("known track event"),
            }
        }
        Ok(())
    }
}

pub(crate) fn replay(events: &[JournalEvent], file: &str) -> Option<Milestone> {
    if !is_milestone_file(file) {
        return None;
    }
    let mut events_on_file = events
        .iter()
        .filter(|event| event.file.as_deref() == Some(file));
    let start = events_on_file.find(|event| event.event == "note" && event.known())?;
    let (_, topic, _) = parse_artifact_name(file)?;
    let mut milestone = Milestone {
        file: file.to_string(),
        title: super::Amendments::collect(events, file)
            .title()
            .map(str::to_string)
            .or_else(|| start.title.clone())
            .unwrap_or_else(|| heading_from_topic(&topic)),
        follows: start.milestone.milestone_follows.clone(),
        advisory_waves: start.milestone.milestone_advisory_waves,
        waves: Vec::new(),
        tracks: Vec::new(),
        checkpoints: Vec::new(),
        closure: None,
    };
    for event in events_on_file.filter(|event| event.known()) {
        // Two replicas can independently record the same track or edit a
        // wave the other has removed. Keep the first applicable transition.
        let _ = milestone.apply(event);
    }
    Some(milestone)
}

pub(crate) fn milestone_files(events: &[JournalEvent]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    events
        .iter()
        .filter(|event| event.event == "note" && event.known())
        .filter_map(|event| event.file.as_deref())
        .filter(|file| is_milestone_file(file) && seen.insert(*file))
        .map(str::to_string)
        .collect()
}

pub(crate) fn active_in(events: &[JournalEvent]) -> Vec<Milestone> {
    milestone_files(events)
        .iter()
        .filter_map(|file| replay(events, file))
        .filter(|milestone| milestone.closure.is_none())
        .collect()
}

pub(crate) fn named_in(events: &[JournalEvent], dir: &Path, name: &str) -> Result<Milestone> {
    let found = if is_milestone_file(name) {
        replay(events, name)
    } else {
        let mut matching = milestone_files(events)
            .iter()
            .filter(|file| parse_artifact_name(file).is_some_and(|(_, topic, _)| topic == name))
            .filter_map(|file| replay(events, file))
            .collect::<Vec<_>>();
        let mut active = matching
            .iter()
            .filter(|milestone| milestone.closure.is_none());
        match (active.next(), active.next(), matching.len()) {
            (Some(one), None, _) => Some(one.clone()),
            (None, None, 1) => matching.pop(),
            (_, _, 0) => None,
            _ => bail!(
                "{name:?} names several milestones; name one by its file: {}",
                matching
                    .iter()
                    .map(|milestone| milestone.file.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    .with_context(|| format!("no milestone named {name:?} in {}", dir.display()))?;
    require_body(dir, &found.file)?;
    Ok(found)
}

fn require_body(dir: &Path, file: &str) -> Result<()> {
    artifact_body_path(dir, file).with_context(|| {
        format!(
            "no regular artifact {file} in {} or its cold archive",
            dir.display()
        )
    })?;
    Ok(())
}

pub(crate) fn start(
    ctx: &Ctx,
    topic: &str,
    title: &str,
    follows: Option<&str>,
    waves: &[String],
    advisory_waves: bool,
) -> Result<String> {
    ensure!(valid_topic(topic), "topic {topic:?} must be kebab-case");
    let title = series::one_line("--title", title)?;
    let mut ids = std::collections::HashSet::new();
    let waves = waves
        .iter()
        .map(|spec| {
            let (id, title) = spec
                .split_once('=')
                .map_or((spec.as_str(), None), |(id, title)| (id, Some(title)));
            check_wave_id(id)?;
            ensure!(ids.insert(id), "wave {id} already exists");
            Ok((
                id,
                title
                    .map(|title| series::one_line("wave title", title))
                    .transpose()?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let dir = resolve_dir(&ctx.cwd)?;
    let _lock = lock_journal_transition(&dir)?;
    if let Some(plan) = follows {
        series::named_artifact(&dir, plan, "plan", "--follows")?;
    }
    if let Err(error) = super::ensure_bound(ctx, &dir) {
        eprintln!("warning: could not record this journal's project binding: {error:#}");
    }
    let _events = super::lock_journal_events(&dir)?;
    let now = Utc::now();
    let stamp = now.format("%Y%m%dT%H%M%SZ");
    let file = super::uniquified(&format!("{stamp}-{topic}-{KIND}.md"), |candidate| {
        [&dir, &super::archive_dir(&dir)]
            .into_iter()
            .any(|path| std::fs::symlink_metadata(path.join(candidate)).is_ok())
    })?;
    let new_event = |kind| {
        let mut event = JournalEvent::base(ctx, now, topic, kind);
        event.file = Some(file.clone());
        event
    };
    let body = format!("# {title}\n\nRead the milestone with `arc milestone {file}`.\n");
    let mut note = new_event("note");
    note.title = Some(title);
    note.milestone.milestone_follows = follows.map(str::to_string);
    note.milestone.milestone_advisory_waves = advisory_waves;
    let mut records = vec![note];
    for (id, title) in waves {
        let mut event = new_event("milestone-wave-added");
        event.milestone.milestone_wave = Some(id.to_string());
        event.milestone.milestone_wave_title = title;
        records.push(event);
    }
    publish_start(&dir, &file, body, &records)?;
    Ok(file)
}

/// Publish the body and all starting waves as one recoverable journal write.
fn publish_start(dir: &Path, file: &str, body: String, records: &[JournalEvent]) -> Result<()> {
    use crate::journal_exchange::{BundledArtifact, HOT_STORAGE};
    let basis = super::event_log_bytes(dir)?;
    super::check_event_tail(dir, basis.as_bytes())?;
    let mut events = basis.clone();
    let mut raw_records = Vec::new();
    for record in records {
        let raw = serde_json::value::to_raw_value(record)?;
        events.push_str(raw.get());
        events.push('\n');
        raw_records.push(raw);
    }
    let pending = super::ExchangeTransaction {
        bodies: vec![BundledArtifact {
            file: file.to_string(),
            storage: HOT_STORAGE.to_string(),
            digest: crate::journal_exchange::body_digest(&body),
            body,
            events: raw_records,
        }],
        body_bases: Some(vec![None]),
        events,
        events_basis: Some(basis),
        receipt: None,
        promotion: None,
    };
    super::publish_exchange_file(
        &super::exchange_pending_path(dir),
        &serde_json::to_vec(&pending)?,
    )?;
    super::finish_exchange_transaction(dir)
}

struct Open {
    dir: PathBuf,
    events: Vec<JournalEvent>,
    milestone: Milestone,
    _lock: super::JournalTransitionLock,
}

impl Open {
    fn read(ctx: &Ctx, name: &str) -> Result<Self> {
        let dir = resolve_dir(&ctx.cwd)?;
        let lock = lock_journal_transition(&dir)?;
        let events = super::read_events(&dir)?;
        let milestone = named_in(&events, &dir, name)?;
        ensure!(
            milestone.closure.is_none(),
            "{} is closed and read-only",
            milestone.file
        );
        Ok(Self {
            dir,
            events,
            milestone,
            _lock: lock,
        })
    }

    fn append(mut self, ctx: &Ctx, kind: &str, fields: MilestoneFields) -> Result<()> {
        let mut event = self.event(ctx, kind);
        event.milestone = fields;
        ensure!(well_formed(&event), "invalid {kind} event");
        self.milestone.apply(&event)?;
        append_event(ctx, &self.dir, &event)
    }

    fn event(&self, ctx: &Ctx, kind: &str) -> JournalEvent {
        let topic = parse_artifact_name(&self.milestone.file)
            .map(|(_, topic, _)| topic)
            .unwrap_or_default();
        let mut event = JournalEvent::base(ctx, Utc::now(), &topic, kind);
        event.file = Some(self.milestone.file.clone());
        event
    }

    fn resolve_track(&self, name: &str) -> Result<TrackRef> {
        if let Some((prefix, name)) = name.split_once("::") {
            let project = super::registered_project(prefix)?;
            ensure!(
                super::canonical_dir(&project.journal_dir) != super::canonical_dir(&self.dir),
                "{prefix} is this project; use an unqualified series name"
            );
            let series = series::named_series(&project.journal_dir, name)?;
            require_body(&project.journal_dir, &series.file)?;
            return Ok(TrackRef {
                project: Some(project.slug),
                file: series.file,
            });
        }
        let series = series::named_in(&self.events, &self.dir, name)?;
        require_body(&self.dir, &series.file)?;
        Ok(TrackRef {
            project: None,
            file: series.file,
        })
    }

    /// Resolve a track already attached to this milestone purely from recorded
    /// membership, without opening any foreign journal or reading bodies, so
    /// an unavailable critical track can still be demoted, moved, or removed.
    fn resolve_attached_track(&self, spec: &str) -> Result<TrackRef> {
        // 1. Exact stored reference match (`<slug>::<file>` or `<file>`).
        if let Some(track) = self
            .milestone
            .tracks
            .iter()
            .find(|t| t.reference.to_string() == spec)
        {
            return Ok(track.reference.clone());
        }

        let matches: Vec<&Track> = if let Some((prefix, rest)) = spec.split_once("::") {
            let resolved_slug = super::registered_project(prefix).ok().map(|p| p.slug);
            self.milestone
                .tracks
                .iter()
                .filter(|t| {
                    let project_ok = t.reference.project.as_deref() == Some(prefix)
                        || (resolved_slug.is_some() && t.reference.project == resolved_slug);
                    if !project_ok {
                        return false;
                    }
                    let topic = parse_artifact_name(&t.reference.file)
                        .map(|(_, topic, _)| topic)
                        .unwrap_or_default();
                    t.reference.file == rest || topic == rest
                })
                .collect()
        } else {
            let by_label: Vec<&Track> = self
                .milestone
                .tracks
                .iter()
                .filter(|t| t.label.as_deref() == Some(spec))
                .collect();
            if !by_label.is_empty() {
                by_label
            } else {
                let local: Vec<&Track> = self
                    .milestone
                    .tracks
                    .iter()
                    .filter(|t| {
                        t.reference.project.is_none()
                            && (t.reference.file == spec
                                || parse_artifact_name(&t.reference.file)
                                    .is_some_and(|(_, topic, _)| topic == spec))
                    })
                    .collect();
                if !local.is_empty() {
                    local
                } else {
                    self.milestone
                        .tracks
                        .iter()
                        .filter(|t| {
                            t.reference.file == spec
                                || parse_artifact_name(&t.reference.file)
                                    .is_some_and(|(_, topic, _)| topic == spec)
                        })
                        .collect()
                }
            }
        };

        match matches.as_slice() {
            [one] => Ok(one.reference.clone()),
            [] => bail!(
                "{} has no attached track matching {spec:?}",
                self.milestone.file
            ),
            many => {
                let names: Vec<String> = many.iter().map(|t| t.reference.to_string()).collect();
                bail!(
                    "{spec:?} matches several tracks on {}: {}; name one by its full reference",
                    self.milestone.file,
                    names.join(", ")
                );
            }
        }
    }
}

fn check_wave_id(id: &str) -> Result<()> {
    ensure!(valid_wave_id(id),
        "wave id {id:?} must have 1 to 32 lowercase ASCII letters or digits separated by single hyphens or underscores");
    Ok(())
}

pub(crate) fn wave_add(
    ctx: &Ctx,
    name: &str,
    id: &str,
    title: Option<&str>,
    placement: Placement<&str>,
    gated: Option<bool>,
) -> Result<()> {
    let open = Open::read(ctx, name)?;
    check_wave_id(id)?;
    let (before, after) = placement
        .map(|anchor| {
            check_wave_id(anchor)?;
            Ok(anchor.to_string())
        })?
        .fields();
    open.append(
        ctx,
        "milestone-wave-added",
        MilestoneFields {
            milestone_wave: Some(id.to_string()),
            milestone_wave_title: title
                .map(|title| series::one_line("wave title", title))
                .transpose()?,
            milestone_wave_before: before,
            milestone_wave_after: after,
            milestone_wave_gated: gated,
            ..Default::default()
        },
    )
}

pub(crate) fn wave_rename(ctx: &Ctx, name: &str, id: &str, title: &str) -> Result<()> {
    Open::read(ctx, name)?.append(
        ctx,
        "milestone-wave-renamed",
        MilestoneFields {
            milestone_wave: Some(id.to_string()),
            milestone_wave_title: Some(series::one_line("wave title", title)?),
            ..Default::default()
        },
    )
}

pub(crate) fn wave_move(ctx: &Ctx, name: &str, id: &str, placement: Placement<&str>) -> Result<()> {
    let (before, after) = placement.map(|anchor| Ok(anchor.to_string()))?.fields();
    Open::read(ctx, name)?.append(
        ctx,
        "milestone-wave-moved",
        MilestoneFields {
            milestone_wave: Some(id.to_string()),
            milestone_wave_before: before,
            milestone_wave_after: after,
            ..Default::default()
        },
    )
}

pub(crate) fn wave_remove(ctx: &Ctx, name: &str, id: &str) -> Result<()> {
    Open::read(ctx, name)?.append(
        ctx,
        "milestone-wave-removed",
        MilestoneFields {
            milestone_wave: Some(id.to_string()),
            ..Default::default()
        },
    )
}

pub(crate) fn track_add(
    ctx: &Ctx,
    name: &str,
    series: &str,
    role: TrackRole,
    label: Option<&str>,
    placement: Placement<&str>,
) -> Result<()> {
    let open = Open::read(ctx, name)?;
    let track = open.resolve_track(series)?;
    let (before, after) = placement
        .map(|anchor| open.resolve_attached_track(anchor))?
        .fields();
    open.append(
        ctx,
        "milestone-track-added",
        MilestoneFields {
            milestone_track: Some(track),
            milestone_track_role: Some(role),
            milestone_track_label: label
                .map(|label| series::one_line("track label", label))
                .transpose()?,
            milestone_track_before: before,
            milestone_track_after: after,
            ..Default::default()
        },
    )
}

pub(crate) fn track_set_role(ctx: &Ctx, name: &str, track: &str, role: TrackRole) -> Result<()> {
    let open = Open::read(ctx, name)?;
    let track = open.resolve_attached_track(track)?;
    open.append(
        ctx,
        "milestone-track-role-set",
        MilestoneFields {
            milestone_track: Some(track),
            milestone_track_role: Some(role),
            ..Default::default()
        },
    )
}

pub(crate) fn track_move(
    ctx: &Ctx,
    name: &str,
    track: &str,
    placement: Placement<&str>,
) -> Result<()> {
    let open = Open::read(ctx, name)?;
    let track = open.resolve_attached_track(track)?;
    let (before, after) = placement
        .map(|anchor| open.resolve_attached_track(anchor))?
        .fields();
    open.append(
        ctx,
        "milestone-track-moved",
        MilestoneFields {
            milestone_track: Some(track),
            milestone_track_before: before,
            milestone_track_after: after,
            ..Default::default()
        },
    )
}

pub(crate) fn track_remove(ctx: &Ctx, name: &str, track: &str) -> Result<()> {
    let open = Open::read(ctx, name)?;
    let track = open.resolve_attached_track(track)?;
    open.append(
        ctx,
        "milestone-track-removed",
        MilestoneFields {
            milestone_track: Some(track),
            ..Default::default()
        },
    )
}

fn recorded_checkpoint(ctx: &Ctx, name: &str) -> Result<MilestoneCheckpoint> {
    let store = Store::discover_existing_for_reading(&ctx.cwd)?
        .with_context(|| format!("no checkpoint named {name:?}; this repository has no ledger"))?;
    let recorded = checkpoint::find(&store, name)?;
    Ok(MilestoneCheckpoint {
        name: recorded.name,
        revision: recorded.revision,
        event_id: recorded.event.event_id,
    })
}

pub(crate) fn bind_checkpoint(ctx: &Ctx, name: &str, checkpoint: &str) -> Result<()> {
    let open = Open::read(ctx, name)?;
    let binding = recorded_checkpoint(ctx, checkpoint)?;
    open.append(
        ctx,
        "milestone-checkpoint-bound",
        MilestoneFields {
            milestone_checkpoint: Some(binding),
            ..Default::default()
        },
    )
}

pub(crate) fn close(
    ctx: &Ctx,
    name: &str,
    checkpoint: Option<&str>,
    by: Option<&str>,
    note: Option<&str>,
) -> Result<String> {
    let open = Open::read(ctx, name)?;
    ensure!(
        checkpoint.is_some() || by.is_some() || note.is_some(),
        "close requires --checkpoint, --by, or --note"
    );
    let mut event = open.event(ctx, "consumed");
    event.note = note
        .map(|note| series::one_line("--note", note))
        .transpose()?;
    event.milestone.milestone_checkpoint = checkpoint
        .map(|name| recorded_checkpoint(ctx, name))
        .transpose()?;
    event.outcome = Some(if by.is_some() { "superseded" } else { "done" }.to_string());
    if let Some(by) = by {
        let digest = super::artifact_body_digest_nofollow(&open.dir, by, "conclusion", "--by")?;
        event.decision = Some(by.to_string());
        event.decision_kind = Some("conclusion".to_string());
        event.decision_digest = Some(digest);
    }
    append_event(ctx, &open.dir, &event)?;
    Ok(open.milestone.file)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replay preserves the last valid order when the log contains conflicting
    /// placements and stops at closure. Rejects replay that silently appends a
    /// wave with a missing anchor or applies edits after consumption.
    #[test]
    fn conflicting_placements_and_post_closure_events_do_not_change_replay() {
        let file = "20261123T120000Z-release-milestone.md";
        let record = |event: &str, fields: serde_json::Value| {
            let mut value = serde_json::json!({
                "schema": super::super::JOURNAL_SCHEMA,
                "ts": "2026-11-23T12:00:00Z",
                "topic": "release",
                "event": event,
                "file": file,
            });
            value
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let event: JournalEvent = serde_json::from_value(value).unwrap();
            assert!(event.known(), "{event:?}");
            event
        };
        let events = [
            record("note", serde_json::json!({ "title": "Release" })),
            record(
                "milestone-wave-added",
                serde_json::json!({ "milestone_wave": "w0" }),
            ),
            record(
                "milestone-wave-added",
                serde_json::json!({
                    "milestone_wave": "w1", "milestone_wave_before": "w0",
                }),
            ),
            record(
                "milestone-wave-added",
                serde_json::json!({
                    "milestone_wave": "w2", "milestone_wave_after": "missing",
                }),
            ),
            record("consumed", serde_json::json!({ "outcome": "done" })),
            record(
                "milestone-wave-removed",
                serde_json::json!({ "milestone_wave": "w1" }),
            ),
        ];
        let milestone = replay(&events, file).unwrap();
        assert_eq!(
            milestone
                .waves
                .iter()
                .map(|wave| wave.id.as_str())
                .collect::<Vec<_>>(),
            ["w1", "w0"]
        );
        assert_eq!(milestone.closure.unwrap().outcome, "done");
        assert!(active_in(&events).is_empty());
    }
}
