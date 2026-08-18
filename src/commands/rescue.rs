use super::*;
use crate::session_store::{self, Turn};
use crate::state::{Brief, ClaimIdentity};
use crate::status::{FindingSummary, GateStatus};
use std::path::PathBuf;

const RESCUE_SCHEMA: &str = "arc-rescue/1";

#[derive(Serialize)]
struct RescueOutput<'a> {
    schema: &'static str,
    change_id: &'a str,
    title: &'a str,
    brief: Option<&'a Brief>,
    stage: Option<String>,
    open_findings: Vec<&'a FindingSummary>,
    gates: &'a [GateStatus],
    next_action: &'a str,
    worktree_dirty: Option<bool>,
    head_state: &'static str,
    claim: Option<RescueClaim<'a>>,
    abandoned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript: Option<RescueTranscript>,
}

#[derive(Serialize)]
struct RescueClaim<'a> {
    owner: &'a ClaimIdentity,
    stage: String,
    active: bool,
    stale: bool,
    expired: bool,
    age_seconds: u64,
}

#[derive(Serialize)]
struct RescueTranscript {
    path: Option<PathBuf>,
    count: usize,
    turns: Vec<Turn>,
    #[serde(skip)]
    unavailable: Option<&'static str>,
}

pub fn rescue(
    ctx: &Ctx,
    reference: &str,
    json: bool,
    take: bool,
    include_transcript: bool,
    tail: usize,
) -> Result<i32> {
    let rescued_owner = if take {
        let (code, previous_owner) = super::claims::takeover_abandoned(ctx, reference)?;
        if code != 0 {
            return Ok(code);
        }
        let previous_owner =
            previous_owner.context("abandoned takeover did not capture the previous owner")?;
        let store = ctx.store()?;
        let change_id = store.resolve_change(reference)?;
        let (_, state) = ctx.load_state(&store, &change_id)?;
        crate::journal::auto_log(
            ctx,
            &state.slug,
            &format!(
                "rescued change {change_id} from {} via {}/{}",
                previous_owner.actor, previous_owner.harness, previous_owner.session
            ),
        );
        Some(previous_owner)
    } else {
        None
    };

    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let (_, state) = ctx.load_state(&store, &change_id)?;
    let report = ctx.report(&store, &state)?;
    let now = chrono::Utc::now();
    let claim = state.claim.as_ref().map(|claim| {
        let timing = state::claim_timing_at(claim, now);
        RescueClaim {
            owner: &claim.owner,
            stage: timing.stage,
            active: timing.active,
            stale: timing.stale,
            expired: timing.expired,
            age_seconds: timing.age_seconds,
        }
    });
    let abandoned = state.claim.as_ref().is_some_and(|held| {
        let timing = state::claim_timing_at(held, now);
        let caller = (
            ctx.actor.as_str(),
            ctx.harness.as_deref(),
            ctx.session.as_deref(),
        );
        let owner = (
            held.owner.actor.as_str(),
            Some(held.owner.harness.as_str()),
            Some(held.owner.session.as_str()),
        );
        caller != owner && (timing.stale || timing.expired)
    });
    let open_findings = report
        .findings
        .iter()
        .filter(|finding| {
            !matches!(
                finding.status.as_str(),
                "resolved" | "acceptedrisk" | "obsolete"
            )
        })
        .collect::<Vec<_>>();
    let head_state = match (
        report.latest_patchset.as_ref(),
        report.head_matches_latest_patchset,
    ) {
        (None, _) => "no-patchset",
        (Some(_), true) => "matches",
        (Some(_), false) => "moved-past",
    };
    let transcript_owner = rescued_owner
        .as_ref()
        .or_else(|| state.claim.as_ref().map(|claim| &claim.owner));
    let transcript = include_transcript
        .then(|| {
            let identity_known = transcript_owner.is_some_and(|owner| {
                !owner.session.trim().is_empty()
                    && matches!(
                        owner.harness.as_str(),
                        "claude" | "codex" | "opencode" | "pi"
                    )
            });
            let path = transcript_owner
                .filter(|_| identity_known)
                .and_then(|owner| session_store::transcript_path(&owner.harness, &owner.session));
            let turns = path
                .as_deref()
                .map(|path| session_store::operator_turns(path, tail))
                .transpose()?
                .unwrap_or_default();
            let unavailable = if !identity_known {
                Some("claim harness/session is unknown")
            } else if path.is_none() {
                Some("no transcript file exists for the claimed session")
            } else {
                None
            };
            Ok::<_, anyhow::Error>(RescueTranscript {
                path,
                count: turns.len(),
                turns,
                unavailable,
            })
        })
        .transpose()?;
    let output = RescueOutput {
        schema: RESCUE_SCHEMA,
        change_id: &state.change_id,
        title: &state.title,
        brief: state.latest_brief(),
        stage: claim.as_ref().map(|claim| claim.stage.clone()),
        open_findings,
        gates: &report.gates,
        next_action: &report.next_action,
        worktree_dirty: report.worktree_dirty,
        head_state,
        claim,
        abandoned,
        transcript,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        render(&output);
    }
    Ok(0)
}

fn render(output: &RescueOutput<'_>) {
    println!("# {} (`{}`)", output.title, output.change_id);
    if let Some(brief) = output.brief {
        println!("\n## Brief\n");
        if let Some(base_revision) = &brief.base_revision {
            println!("- Base revision: `{base_revision}`\n");
        }
        if !brief.acceptance_probes.is_empty() {
            println!("- Acceptance probes:");
            for probe in &brief.acceptance_probes {
                println!("  - `{}`: `{}`", probe.name, probe.command);
            }
            println!();
        }
        print!("{}", brief.body);
        if !brief.body.ends_with('\n') {
            println!();
        }
    }
    println!("\n## Claim / Stage\n");
    match &output.claim {
        Some(claim) => {
            println!(
                "- Owner: {} via {}/{}",
                claim.owner.actor, claim.owner.harness, claim.owner.session
            );
            println!("- Stage: `{}`", claim.stage);
            println!(
                "- State: {}",
                if claim.expired {
                    "expired"
                } else if claim.stale {
                    "stale"
                } else {
                    "active"
                }
            );
            println!("- Last activity: {}s ago", claim.age_seconds);
        }
        None => println!("- (unclaimed)"),
    }
    if let Some(transcript) = &output.transcript {
        println!("\n## Transcript\n");
        match &transcript.path {
            Some(path) => {
                println!("- Path: `{}`", path.display());
                println!("- Turns: {}", transcript.count);
                for turn in &transcript.turns {
                    match &turn.ts {
                        Some(ts) => println!("\n### {} ({ts})\n\n{}", turn.role, turn.text),
                        None => println!("\n### {}\n\n{}", turn.role, turn.text),
                    }
                }
            }
            None => println!(
                "- Unavailable: {}",
                transcript
                    .unavailable
                    .expect("missing transcript should carry an explanation")
            ),
        }
    }
    println!("\n## Worktree\n");
    println!(
        "- Branch head: {}",
        match output.head_state {
            "no-patchset" => "no patchset recorded",
            "matches" => "matches the newest approved/snapshotted head",
            _ => "has moved past the newest patchset",
        }
    );
    println!(
        "- Uncommitted edits: {}",
        match output.worktree_dirty {
            Some(true) => "present",
            Some(false) => "absent",
            None => "unknown",
        }
    );
    println!("\n## Open Findings\n");
    if output.open_findings.is_empty() {
        println!("- (none)");
    } else {
        for finding in &output.open_findings {
            println!(
                "- `{}` [{}] {}",
                finding.id, finding.status, finding.summary
            );
        }
    }
    println!("\n## Gates at Head\n");
    if output.gates.is_empty() {
        println!("- (none)");
    } else {
        for gate in output.gates {
            println!("- {}: {}", gate.name, crate::render::gate_line(gate));
        }
    }
    println!("\n## Assessment\n");
    println!(
        "- Abandoned: {}",
        if output.abandoned { "yes" } else { "no" }
    );
    println!("\nNext action: {}", output.next_action);
}
