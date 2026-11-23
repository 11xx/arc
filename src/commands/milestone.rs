use super::Ctx;
use crate::journal::{
    self, milestone,
    milestone::{Placement, TrackRole},
};
use anyhow::Result;
use clap::{ArgGroup, Args, Subcommand};

/// Read a milestone by filename or topic. With no name, list open milestones.
#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct MilestoneArgs {
    milestone: Option<String>,
    /// Print line-oriented text
    #[arg(long, conflicts_with = "json")]
    plain: bool,
    /// Print the milestone summaries as JSON
    #[arg(long)]
    json: bool,
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
        #[arg(long, conflicts_with = "after")]
        before: Option<String>,
        #[arg(long)]
        after: Option<String>,
    },
    /// Set a track's role
    Role {
        milestone: String,
        track: String,
        #[arg(id = "track_role", value_name = "ROLE", value_enum)]
        role: TrackRole,
    },
    /// Move a track next to another track
    Move {
        milestone: String,
        track: String,
        #[arg(long, conflicts_with = "after", required_unless_present = "after")]
        before: Option<String>,
        #[arg(long)]
        after: Option<String>,
    },
    /// Detach a track
    Remove { milestone: String, track: String },
}

pub fn run(ctx: &Ctx, args: MilestoneArgs) -> Result<i32> {
    match args.command {
        None => report(ctx, args.milestone.as_deref(), args.json)?,
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

fn report(ctx: &Ctx, name: Option<&str>, json: bool) -> Result<()> {
    let dir = journal::resolve_dir(&ctx.cwd)?;
    let summaries = match name {
        Some(name) => vec![milestone::named_milestone(&dir, name)?],
        None => milestone::active_in(&journal::read_events(&dir)?),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "milestones": summaries }))?
        );
    } else if summaries.is_empty() {
        println!("No open milestones.");
    } else {
        for milestone in &summaries {
            print_summary(milestone);
        }
    }
    Ok(())
}

fn print_summary(milestone: &milestone::Milestone) {
    println!("{}  {}", milestone.file, milestone.title);
    if let Some(plan) = &milestone.follows {
        println!("  Follows {plan}");
    }
    for wave in &milestone.waves {
        let gated = wave.gated.unwrap_or(!milestone.advisory_waves);
        let policy = if gated { "gated" } else { "advisory" };
        println!(
            "  Wave {} [{policy}] {}",
            wave.id,
            wave.title.as_deref().unwrap_or_default()
        );
    }
    for track in &milestone.tracks {
        println!(
            "  Track {} [{}] {}",
            track.reference,
            track.role.as_str(),
            track.label.as_deref().unwrap_or_default()
        );
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
