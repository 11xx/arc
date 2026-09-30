use super::*;
use crate::session_store::{self, Turn};
use crate::state::{Brief, ClaimIdentity};
use crate::status::{self, BriefBaseDrift, FindingSummary, GateStatus};
use std::path::PathBuf;

const RESCUE_SCHEMA: &str = "arc-rescue/5";

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
    #[serde(skip)]
    base_drift: Option<BriefBaseDrift>,
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
    /// The reader that supplied the turns, when any did.
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'static str>,
    /// The window the answer rests on, when a reader stopped before the start
    /// of the recording.
    #[serde(skip_serializing_if = "Option::is_none")]
    bound: Option<TranscriptBound>,
    /// Why the transcript is empty, when `count` and the readers do not say.
    /// A `count: 0` consumer separates an absent recording, an unknown
    /// identity, and text outside the read window from this alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    cause: Option<TranscriptCause>,
    /// The reader's diagnostic when lookup or reading could not finish.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// The human rendering's lines, composed where the readers are known.
    #[serde(skip)]
    lines: Vec<String>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum TranscriptBound {
    /// A recording file read from its end: the newest `window_bytes`, with
    /// `skipped_bytes` of older recording before it when the read measured
    /// the file.
    FileTail {
        window_bytes: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        skipped_bytes: Option<u64>,
    },
    /// A store read that stopped at the newest `records` records of one kind.
    RecordPage { records: usize, of: String },
    /// A returned turn's text was cut at a fixed length by its source.
    TextCut { turns: usize, chars: usize },
    /// A reader reported an omission it could not name.
    Unnamed,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum TranscriptCause {
    /// The claim names no session, or one whose harness no reader can read.
    UnknownIdentity,
    /// No reader found a recording for the session.
    NoRecording,
    /// More than one recording matched the claimed id.
    Ambiguous,
    /// The reader could not establish or read the claimed recording.
    Unreadable,
    /// A recording exists, and the read window holds no operator turn because
    /// older text lies outside it.
    OutsideReadBound,
}

impl From<session_store::ReadBound> for TranscriptBound {
    fn from(bound: session_store::ReadBound) -> Self {
        match bound {
            session_store::ReadBound::FileTail { bytes, skipped } => TranscriptBound::FileTail {
                window_bytes: bytes,
                skipped_bytes: (skipped > 0).then_some(skipped),
            },
            session_store::ReadBound::RecordPage { records, of } => {
                TranscriptBound::RecordPage { records, of }
            }
            session_store::ReadBound::TurnText { turns, chars } => {
                TranscriptBound::TextCut { turns, chars }
            }
            session_store::ReadBound::Other => TranscriptBound::Unnamed,
        }
    }
}

impl TranscriptBound {
    /// What a human reads: what was read, what was not, and how to reach the
    /// rest with the `tapes` command, which reads a whole recording.
    fn line(&self, session: &str) -> String {
        match self {
            TranscriptBound::FileTail {
                window_bytes,
                skipped_bytes: Some(skipped),
            } => format!(
                "the newest {window_bytes} bytes of a {}-byte recording; {skipped} earlier bytes were not read",
                window_bytes + skipped
            ),
            TranscriptBound::FileTail {
                window_bytes,
                skipped_bytes: None,
            } => format!(
                "the newest {window_bytes} bytes; older text was not read (`tapes show {session} --full` reads the whole recording)"
            ),
            TranscriptBound::RecordPage { records, of } => format!(
                "the newest {records} {of} records; older records were not read (`tapes show {session} --full` reads the whole recording)"
            ),
            TranscriptBound::TextCut { turns, chars } => format!(
                "{turns} returned turns carry text the store cut at {chars} characters"
            ),
            TranscriptBound::Unnamed => format!(
                "the reader stopped before the start of the recording (`tapes show {session} --full` reads the whole recording)"
            ),
        }
    }
}

impl TranscriptCause {
    fn line(self) -> String {
        match self {
            TranscriptCause::UnknownIdentity => {
                "Unavailable: claim harness/session is unknown".to_string()
            }
            TranscriptCause::NoRecording => {
                "Unavailable: no recording of the claimed session in its harness's store"
                    .to_string()
            }
            TranscriptCause::Ambiguous => {
                "Unavailable: the claimed session lookup is ambiguous".to_string()
            }
            TranscriptCause::Unreadable => {
                "Unreadable: the claimed session lookup could not complete".to_string()
            }
            TranscriptCause::OutsideReadBound => {
                "Outside read bound: the read window holds no operator turn".to_string()
            }
        }
    }
}

/// Read the claimed session's transcript through the tapes library, which
/// holds every harness's store knowledge. A bound the read rested on is
/// reported rather than hidden, and the cause of an empty answer is carried
/// into the machine view.
fn read_transcript(owner: Option<&ClaimIdentity>, tail: usize) -> Result<RescueTranscript> {
    let readable = owner.is_some_and(|owner| {
        !owner.session.trim().is_empty()
            && matches!(
                owner.harness.as_str(),
                "claude" | "codex" | "opencode" | "pi"
            )
    });
    let Some(owner) = owner.filter(|_| readable) else {
        return Ok(RescueTranscript {
            path: None,
            count: 0,
            turns: Vec::new(),
            source: None,
            bound: None,
            cause: Some(TranscriptCause::UnknownIdentity),
            reason: None,
            lines: vec![
                "Source: none (the claim names no session a reader can read)".to_string(),
                TranscriptCause::UnknownIdentity.line(),
            ],
        });
    };

    let (read, lookup_cause, reason) =
        match session_store::read_session(&owner.harness, &owner.session) {
            session_store::SessionAnswer::Read(read) => (Some(read), None, None),
            session_store::SessionAnswer::NoRecording => {
                (None, Some(TranscriptCause::NoRecording), None)
            }
            session_store::SessionAnswer::Ambiguous(reason) => {
                (None, Some(TranscriptCause::Ambiguous), Some(reason))
            }
            session_store::SessionAnswer::Unreadable(reason) => {
                (None, Some(TranscriptCause::Unreadable), Some(reason))
            }
        };
    let (turns, bound, path) = match read {
        Some(read) => (read.turns, read.bound, read.path),
        None => (Vec::new(), None, None),
    };
    let withheld = bound.as_ref().is_some_and(|bound| bound.withholds_turns());
    let bound: Option<TranscriptBound> = bound.map(Into::into);
    let source = (!turns.is_empty()).then_some("tapes");
    let cause = lookup_cause
        .or_else(|| (turns.is_empty() && withheld).then_some(TranscriptCause::OutsideReadBound));
    let turns = session_store::operator_view(turns, tail);
    let mut lines = vec![match (source, &reason) {
        (Some(source), _) => format!("Source: {source}"),
        (None, Some(reason)) => format!("Source: none (tapes could not read it: {reason})"),
        (None, None) => "Source: none".to_string(),
    }];
    if let Some(path) = &path {
        lines.push(format!("Path: `{}`", path.display()));
    }
    if let Some(bound) = &bound {
        lines.push(format!("Bound: {}", bound.line(&owner.session)));
    }
    match cause {
        Some(cause) => lines.push(cause.line()),
        None => lines.push(format!("Turns: {}", turns.len())),
    }
    Ok(RescueTranscript {
        path,
        count: turns.len(),
        turns,
        source,
        bound,
        cause,
        reason,
        lines,
    })
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
        .then(|| read_transcript(transcript_owner, tail))
        .transpose()?;
    let base_drift = state.latest_brief().and_then(|brief| {
        status::brief_base_drift(
            &ctx.cwd,
            brief.base_revision.as_deref(),
            report.current_head.as_deref(),
        )
    });
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
        base_drift,
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
            let drift = output
                .base_drift
                .as_ref()
                .and_then(BriefBaseDrift::annotation)
                .unwrap_or_default();
            println!("- Base revision: `{base_revision}`{drift}\n");
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
        for line in &transcript.lines {
            println!("- {line}");
        }
        for turn in &transcript.turns {
            match &turn.ts {
                Some(ts) => println!("\n### {} ({ts})\n\n{}", turn.role, turn.text),
                None => println!("\n### {}\n\n{}", turn.role, turn.text),
            }
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
