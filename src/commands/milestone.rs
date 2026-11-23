use super::progress::Form;
use super::Ctx;
use crate::commands::progress::reader::Registry;
use crate::journal::{
    self, milestone,
    milestone::{Placement, Track, TrackRole},
    series,
};
use anyhow::{bail, ensure, Result};
use clap::{ArgGroup, Args, Subcommand};
use serde::Serialize;
use std::path::Path;

pub(crate) mod matrix;
mod view;
pub(crate) use matrix::{milestones_link_into, project_milestones};
use matrix::{MatrixCounts, MilestoneReport};

/// The schema `arc workspace milestones --json` prints.
const WORKSPACE_MILESTONES_SCHEMA: &str = "arc-workspace-milestones/1";

#[derive(Serialize)]
pub(crate) struct ProjectMilestones {
    pub(crate) project: String,
    pub(crate) slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) anchor: Option<String>,
    pub(crate) milestones: Vec<MilestoneReport>,
}

#[derive(Serialize)]
struct MilestoneFailure {
    project: String,
    reason: String,
}

/// Read a milestone by filename or topic. With no name, list open milestones.
#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct MilestoneArgs {
    milestone: Option<String>,
    /// Print line-oriented text
    #[arg(long, conflicts_with = "json")]
    plain: bool,
    /// Print the milestone matrix as structured JSON (`arc-milestone/1`)
    #[arg(long)]
    json: bool,
    /// Show only currently ready items across tracks (respects cross-track wave barriers)
    #[arg(long)]
    ready: bool,
    /// Ignore wave barriers when deriving readiness (explicit prerequisites still apply)
    #[arg(long)]
    ignore_waves: bool,
    #[command(subcommand)]
    command: Option<MilestoneCmd>,
}

#[derive(Subcommand)]
enum MilestoneCmd {
    /// Start a milestone with an optional plan and initial waves
    Start {
        topic: String,
        #[arg(long)]
        title: String,
        /// Bare filename of a plan in this journal
        #[arg(long)]
        follows: Option<String>,
        /// Declare a wave as ID or ID=TITLE. Repeat in wave order.
        #[arg(long = "wave", value_name = "ID[=TITLE]")]
        waves: Vec<String>,
        /// Make waves advisory by default. The default is gated for critical tracks.
        #[arg(long)]
        advisory_waves: bool,
    },
    /// Edit the milestone's ordered waves
    #[command(subcommand)]
    Wave(WaveCmd),
    /// Edit the milestone's ordered tracks
    ///
    /// A track names a series by filename or topic. Use PROJECT::SERIES
    /// for a series in another registered project.
    #[command(subcommand)]
    Track(TrackCmd),
    /// Defer a slice from one track of a milestone to a track of another milestone
    Defer {
        /// Source milestone filename or topic
        milestone: String,
        /// Slice to defer as `<track>:<item>` (track by label, series topic, or filename)
        slice: String,
        /// Target milestone, optionally with track: `<target-milestone>` or `<target-milestone>:<track>`
        #[arg(long)]
        to: String,
        /// Wave in the destination track (defaults to the source wave when declared there)
        #[arg(long)]
        wave: Option<String>,
        /// Why the slice is being deferred, one line
        #[arg(long)]
        reason: String,
    },
    /// Bind an existing repository checkpoint to an open milestone
    Checkpoint { milestone: String, name: String },
    /// Close a milestone. It remains readable by name.
    #[command(group(ArgGroup::new("closure").args(["checkpoint", "by", "note"]).multiple(true).required(true)))]
    Close {
        milestone: String,
        /// Bind this recorded repository checkpoint at closure
        #[arg(long)]
        checkpoint: Option<String>,
        /// Supersede the milestone with this local conclusion's bare filename
        #[arg(long)]
        by: Option<String>,
        /// Record a one-line closing note
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Subcommand)]
enum WaveCmd {
    /// Add a wave at the end or next to an existing wave
    Add {
        milestone: String,
        /// Lowercase ASCII letters and digits, with single hyphens or underscores
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, conflicts_with = "after")]
        before: Option<String>,
        #[arg(long)]
        after: Option<String>,
        /// Make this wave advisory
        #[arg(long, conflicts_with = "gated")]
        soft: bool,
        /// Gate critical tracks in this wave
        #[arg(long)]
        gated: bool,
    },
    /// Change a wave's title
    Rename {
        milestone: String,
        id: String,
        title: String,
    },
    /// Move a wave next to another wave
    Move {
        milestone: String,
        id: String,
        #[arg(long, conflicts_with = "after", required_unless_present = "after")]
        before: Option<String>,
        #[arg(long)]
        after: Option<String>,
    },
    /// Remove a wave
    Remove { milestone: String, id: String },
}

#[derive(Subcommand)]
enum TrackCmd {
    /// Attach a series as a track
    ///
    /// Here `--role` selects the track role. Set the execution role with
    /// `ARC_ROLE` or with `arc --role ROLE milestone track add ...`.
    Add {
        milestone: String,
        #[arg(allow_hyphen_values = true)]
        series: String,
        #[arg(
            long = "role",
            id = "track_role",
            value_enum,
            default_value = "critical"
        )]
        role: TrackRole,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, conflicts_with = "after", allow_hyphen_values = true)]
        before: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        after: Option<String>,
    },
    /// Set a track's role
    Role {
        milestone: String,
        #[arg(allow_hyphen_values = true)]
        track: String,
        #[arg(id = "track_role", value_name = "ROLE", value_enum)]
        role: TrackRole,
    },
    /// Move a track next to another track
    Move {
        milestone: String,
        #[arg(allow_hyphen_values = true)]
        track: String,
        #[arg(
            long,
            conflicts_with = "after",
            required_unless_present = "after",
            allow_hyphen_values = true
        )]
        before: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        after: Option<String>,
    },
    /// Detach a track
    Remove {
        milestone: String,
        #[arg(allow_hyphen_values = true)]
        track: String,
    },
}

pub fn run(ctx: &Ctx, args: MilestoneArgs) -> Result<i32> {
    match args.command {
        None => report(
            ctx,
            args.milestone.as_deref(),
            args.plain,
            args.json,
            args.ready,
            args.ignore_waves,
        )?,
        Some(MilestoneCmd::Start {
            topic,
            title,
            follows,
            waves,
            advisory_waves,
        }) => {
            println!(
                "{}",
                milestone::start(
                    ctx,
                    &topic,
                    &title,
                    follows.as_deref(),
                    &waves,
                    advisory_waves
                )?
            );
        }
        Some(MilestoneCmd::Wave(command)) => match command {
            WaveCmd::Add {
                milestone: name,
                id,
                title,
                before,
                after,
                soft,
                gated,
            } => {
                let override_gating = (soft || gated).then_some(gated);
                milestone::wave_add(
                    ctx,
                    &name,
                    &id,
                    title.as_deref(),
                    Placement::from_fields(before.as_deref(), after.as_deref()),
                    override_gating,
                )?;
                println!("added wave {id}");
            }
            WaveCmd::Rename {
                milestone: name,
                id,
                title,
            } => {
                milestone::wave_rename(ctx, &name, &id, &title)?;
                println!("renamed wave {id}");
            }
            WaveCmd::Move {
                milestone: name,
                id,
                before,
                after,
            } => {
                milestone::wave_move(
                    ctx,
                    &name,
                    &id,
                    Placement::from_fields(before.as_deref(), after.as_deref()),
                )?;
                println!("moved wave {id}");
            }
            WaveCmd::Remove {
                milestone: name,
                id,
            } => {
                milestone::wave_remove(ctx, &name, &id)?;
                println!("removed wave {id}");
            }
        },
        Some(MilestoneCmd::Track(command)) => match command {
            TrackCmd::Add {
                milestone: name,
                series,
                role,
                label,
                before,
                after,
            } => {
                milestone::track_add(
                    ctx,
                    &name,
                    &series,
                    role,
                    label.as_deref(),
                    Placement::from_fields(before.as_deref(), after.as_deref()),
                )?;
                println!("attached track {series}");
            }
            TrackCmd::Role {
                milestone: name,
                track,
                role,
            } => {
                milestone::track_set_role(ctx, &name, &track, role)?;
                println!("set track {track} to {}", role.as_str());
            }
            TrackCmd::Move {
                milestone: name,
                track,
                before,
                after,
            } => {
                milestone::track_move(
                    ctx,
                    &name,
                    &track,
                    Placement::from_fields(before.as_deref(), after.as_deref()),
                )?;
                println!("moved track {track}");
            }
            TrackCmd::Remove {
                milestone: name,
                track,
            } => {
                milestone::track_remove(ctx, &name, &track)?;
                println!("detached track {track}");
            }
        },
        Some(MilestoneCmd::Defer {
            milestone: name,
            slice,
            to,
            wave,
            reason,
        }) => defer(ctx, &name, &slice, &to, wave.as_deref(), &reason)?,
        Some(MilestoneCmd::Checkpoint {
            milestone: name,
            name: checkpoint,
        }) => {
            milestone::bind_checkpoint(ctx, &name, &checkpoint)?;
            println!("bound checkpoint {checkpoint}");
        }
        Some(MilestoneCmd::Close {
            milestone: name,
            checkpoint,
            by,
            note,
        }) => {
            let file = milestone::close(
                ctx,
                &name,
                checkpoint.as_deref(),
                by.as_deref(),
                note.as_deref(),
            )?;
            println!("closed {file}");
        }
    }
    Ok(0)
}

fn defer(
    ctx: &Ctx,
    source_ms_name: &str,
    slice: &str,
    to: &str,
    wave: Option<&str>,
    reason: &str,
) -> Result<()> {
    let Some((src_track_spec, item_id)) = slice.rsplit_once(':') else {
        bail!("defer takes <track>:<item>, such as core:i3; got {slice:?}");
    };
    let dir = journal::resolve_dir(&ctx.cwd)?;
    let events = journal::read_events(&dir)?;
    let src_ms = milestone::named_in(&events, &dir, source_ms_name)?;
    ensure!(
        src_ms.closure.is_none(),
        "{} is closed and read-only",
        src_ms.file
    );

    let (dst_ms_name, dst_track_spec) = match to.split_once(':') {
        Some((ms_name, track_spec)) if !ms_name.is_empty() && !track_spec.is_empty() => {
            (ms_name, Some(track_spec))
        }
        _ => (to, None),
    };
    let dst_ms = milestone::named_in(&events, &dir, dst_ms_name)?;
    ensure!(
        dst_ms.closure.is_none(),
        "{} is closed and read-only",
        dst_ms.file
    );

    let src_track = match_track(&src_ms.tracks, src_track_spec, &src_ms.file)?;
    let dst_track = match dst_track_spec {
        Some(spec) => match_track(&dst_ms.tracks, spec, &dst_ms.file)?,
        None => infer_destination_track(&dst_ms.tracks, src_track, &dst_ms.file)?,
    };

    let (src_journal_dir, src_anchor) = match &src_track.reference.project {
        None => (dir.clone(), Some(ctx.cwd.clone())),
        Some(slug) => {
            let project = journal::registered_project(slug)?;
            (project.journal_dir, project.anchor)
        }
    };

    let dst_spec = match &dst_track.reference.project {
        Some(slug) => format!("{slug}::{}", dst_track.reference.file),
        None => match &src_track.reference.project {
            None => dst_track.reference.file.clone(),
            Some(_) => {
                // Crossing back into the milestone's own journal from a foreign track.
                let cfg = crate::config::load()?;
                let known = crate::registry::known_journals(&cfg)?;
                let c = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
                let here_canon = c(&dir);
                let here_slug = known
                    .iter()
                    .find(|(_, journal)| c(journal) == here_canon)
                    .map(|(slug, _)| slug.clone());
                match here_slug {
                    Some(slug) => {
                        let count = known.iter().filter(|(s, _)| *s == slug).count();
                        if count > 1 {
                            bail!(
                                "this project's journal slug {slug:?} is ambiguous across {count} \
                                 registered journals; give each journal a unique directory name \
                                 before deferring from a cross-project track into a local track"
                            );
                        }
                        format!("{slug}::{}", dst_track.reference.file)
                    }
                    None => bail!(
                        "this project's journal is not registered; bind or record a journal entry \
                         before deferring from a cross-project track into a local track"
                    ),
                }
            }
        },
    };

    let outcome = series::transfer_from_journal(
        ctx,
        &src_journal_dir,
        src_anchor.as_deref(),
        &src_track.reference.file,
        item_id,
        &dst_spec,
        wave,
        reason,
    )?;
    for warning in &outcome.warnings {
        eprintln!("warning: {warning}");
    }
    let src_label = track_short_name(src_track);
    let dst_label = track_short_name(dst_track);
    let wave_suffix = outcome
        .wave
        .as_deref()
        .map(|w| format!(" (wave {w})"))
        .unwrap_or_default();
    println!(
        "deferred {} {src_label}:{item_id} -> {} {dst_label}:{}{wave_suffix}",
        src_ms.file, dst_ms.file, outcome.target_item
    );
    Ok(())
}

fn track_short_name(track: &Track) -> String {
    if let Some(label) = &track.label {
        return label.clone();
    }
    let topic = journal::parse_artifact_name(&track.reference.file)
        .map(|(_, topic, _)| topic)
        .unwrap_or_else(|| track.reference.file.clone());
    match &track.reference.project {
        Some(slug) => format!("{slug}::{topic}"),
        None => topic,
    }
}

fn match_track<'a>(tracks: &'a [Track], spec: &str, ms_file: &str) -> Result<&'a Track> {
    let (spec_project, spec_series) = match spec.split_once("::") {
        Some((proj, rest)) => (Some(proj), rest),
        None => (None, spec),
    };
    let resolved_slug = spec_project.and_then(|proj| {
        journal::registered_project(proj)
            .ok()
            .map(|p| p.slug)
            .or_else(|| Some(proj.to_string()))
    });

    let matches: Vec<&Track> = tracks
        .iter()
        .filter(|track| {
            if spec_project.is_none() && track.label.as_deref() == Some(spec) {
                return true;
            }
            let project_ok = match (&resolved_slug, &track.reference.project) {
                (Some(wanted), Some(actual)) => wanted == actual,
                (Some(_), None) => false,
                (None, _) => true,
            };
            if !project_ok {
                return false;
            }
            track.reference.file == spec_series
                || journal::parse_artifact_name(&track.reference.file)
                    .is_some_and(|(_, topic, _)| topic == spec_series)
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => bail!("{ms_file} has no track matching {spec:?}"),
        _ => bail!(
            "{spec:?} matches several tracks of {ms_file}; name one by label or filename: {}",
            matches
                .iter()
                .map(|t| t.reference.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn infer_destination_track<'a>(
    dst_tracks: &'a [Track],
    src_track: &Track,
    dst_file: &str,
) -> Result<&'a Track> {
    if dst_tracks.len() == 1 {
        return Ok(&dst_tracks[0]);
    }
    if let Some(label) = src_track.label.as_deref() {
        let by_label: Vec<&Track> = dst_tracks
            .iter()
            .filter(|t| t.label.as_deref() == Some(label))
            .collect();
        if by_label.len() == 1 {
            return Ok(by_label[0]);
        }
    }
    let by_proj_role: Vec<&Track> = dst_tracks
        .iter()
        .filter(|t| t.reference.project == src_track.reference.project && t.role == src_track.role)
        .collect();
    if by_proj_role.len() == 1 {
        return Ok(by_proj_role[0]);
    }
    bail!(
        "{dst_file} has {} track(s); name the destination track with --to <milestone>:<track>",
        dst_tracks.len()
    );
}

fn report(
    ctx: &Ctx,
    name: Option<&str>,
    plain: bool,
    json: bool,
    ready_only: bool,
    ignore_waves: bool,
) -> Result<()> {
    let doc = matrix::build(ctx, name, ready_only, ignore_waves)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&doc)?);
    } else if plain {
        if doc.milestones.is_empty() {
            println!("No open milestones.");
        } else {
            for (idx, milestone) in doc.milestones.iter().enumerate() {
                if idx > 0 {
                    println!();
                }
                print_plain(milestone, ready_only);
            }
        }
    } else {
        let mut view = view::View::new(crate::terminal::Look::of_stdout(), chrono::Utc::now());
        view.milestones(&doc.milestones, ready_only);
        println!("{}", view.finish());
    }
    Ok(())
}

fn format_counts(counts: &MatrixCounts) -> String {
    let mut parts = vec![format!("{} of {} complete", counts.complete, counts.items)];
    if let Some(pct) = counts.percent {
        parts[0].push_str(&format!(" ({pct}%)"));
    }
    if counts.in_flight > 0 {
        parts.push(format!("{} in flight", counts.in_flight));
    }
    if counts.ready > 0 {
        parts.push(format!("{} ready", counts.ready));
    }
    if counts.blocked > 0 {
        parts.push(format!("{} blocked", counts.blocked));
    }
    if counts.claimed > 0 {
        parts.push(format!("{} claimed", counts.claimed));
    }
    if counts.waiting > 0 {
        parts.push(format!("{} waiting", counts.waiting));
    }
    if counts.closed > 0 {
        parts.push(format!("{} closed", counts.closed));
    }
    parts.join("; ")
}

fn print_plain(milestone: &MilestoneReport, ready_only: bool) {
    println!("{}  {}", milestone.file, milestone.title);
    if let Some(plan) = &milestone.follows {
        println!("  Follows {plan}");
    }
    println!("  Critical: {}", format_counts(&milestone.counts.critical));
    if milestone.counts.sideband.items > 0 {
        println!("  Sideband: {}", format_counts(&milestone.counts.sideband));
    }
    for wave in &milestone.waves {
        let policy = if wave.effective_gated {
            "gated"
        } else {
            "advisory"
        };
        println!(
            "  Wave {} [{policy}] {}",
            wave.id,
            wave.title.as_deref().unwrap_or_default()
        );
    }
    for track in &milestone.tracks {
        let reference = match &track.project {
            Some(slug) => format!("{slug}::{}", track.file),
            None => track.file.clone(),
        };
        let label = track.label.as_deref().unwrap_or_default();
        if let Some(reason) = &track.unavailable {
            println!(
                "  Track {reference} [{}] {label} (unavailable: {reason})",
                track.role.as_str()
            );
        } else {
            let summary = track.counts.as_ref().map(format_counts).unwrap_or_default();
            println!(
                "  Track {reference} [{}] {label}  {summary}",
                track.role.as_str()
            );
            for item in &track.items {
                if ready_only && item.readiness != "ready" {
                    continue;
                }
                let wave = item
                    .wave
                    .as_deref()
                    .map(|w| format!(" wave={w}"))
                    .unwrap_or_default();
                println!(
                    "    {} {} ({}){}  {}",
                    item.id, item.stage, item.readiness, wave, item.title
                );
            }
        }
    }
    for checkpoint in &milestone.checkpoints {
        println!(
            "  Checkpoint {} at {} (event {})",
            checkpoint.name, checkpoint.revision, checkpoint.event_id
        );
    }
    if let Some(closure) = &milestone.closure {
        println!("  Closed {} [{}]", closure.at, closure.outcome);
        if let Some(by) = &closure.by {
            println!("    By {by}");
        }
        if let Some(checkpoint) = &closure.checkpoint {
            println!("    At checkpoint {}", checkpoint.name);
        }
        if let Some(note) = &closure.note {
            println!("    {note}");
        }
    }
}

/// `arc workspace milestones`: every registered project's active milestones.
pub(crate) fn workspace(
    includes: impl Fn(Option<&Path>) -> bool,
    scope: impl Serialize,
    form: Form,
) -> Result<i32> {
    let now = chrono::Utc::now();
    let cfg = crate::config::load()?;
    let mut projects = Vec::new();
    let mut failures = Vec::new();
    let registered = crate::registry::journal_projects(&cfg)?;
    let registry = Registry::of(registered.clone());
    for project in registered {
        if project.retired.is_some() || !includes(project.anchor.as_deref()) {
            continue;
        }
        let read = project_milestones(
            &project.journal_dir,
            project.anchor.as_deref(),
            Some((&registry, &project.slug)),
            now,
        );
        match read {
            Ok(milestones) if milestones.is_empty() => {}
            Ok(milestones) => projects.push(ProjectMilestones {
                project: project.label(),
                slug: project.slug.clone(),
                anchor: project
                    .anchor
                    .as_ref()
                    .map(|path| path.display().to_string()),
                milestones,
            }),
            Err(error) => failures.push(MilestoneFailure {
                project: project.label(),
                reason: format!("{error:#}"),
            }),
        }
    }
    match form {
        Form::Json => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": WORKSPACE_MILESTONES_SCHEMA,
                "scope": scope,
                "projects": projects,
                "failures": failures,
            }))?
        ),
        Form::Plain => {
            if projects.is_empty() {
                println!("No active milestones.");
            }
            for (index, project) in projects.iter().enumerate() {
                if index > 0 {
                    println!();
                }
                println!("{} ({}):", project.project, project.milestones.len());
                for ms in &project.milestones {
                    print_plain(ms, false);
                }
            }
        }
        Form::Terminal => {
            let mut view = view::View::new(crate::terminal::Look::of_stdout(), now);
            view.workspace(&projects);
            println!("{}", view.finish());
        }
    }
    for failure in &failures {
        eprintln!(
            "{}: unavailable ({})",
            failure.project,
            failure.reason.replace(['\n', '\r'], " ")
        );
    }
    Ok(if failures.is_empty() { 0 } else { 16 })
}
