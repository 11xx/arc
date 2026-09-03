//! Repository-scoped records for delegated run dispatch and terminal outcomes.

use super::*;
use crate::model::{
    CommitRange, DeferredFinding, Event, Payload, RunFinding, RunOutcome, Severity,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// What a delegated run was dispatched against.
///
/// Rounds are ordinals within a subject, so the subject is what makes a
/// sequence of dispatches one conversation rather than a list. A ledger change
/// is one shape it takes; a fork and a bare commit range are the others, and
/// the loop of bounded rounds runs on all three.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RunSubject {
    Change { id: String },
    Fork { slug: String },
    Range { base: String, head: String },
}

impl RunSubject {
    /// The subject a dispatch payload names, or nothing when it names none.
    fn of(payload: &Payload) -> Option<Self> {
        let Payload::RunDispatched {
            change,
            fork,
            range,
            ..
        } = payload
        else {
            return None;
        };
        match (change, fork, range) {
            (Some(id), _, _) => Some(Self::Change { id: id.clone() }),
            (_, Some(slug), _) => Some(Self::Fork { slug: slug.clone() }),
            (_, _, Some(range)) => Some(Self::Range {
                base: range.base.clone(),
                head: range.head.clone(),
            }),
            _ => None,
        }
    }

    /// How the subject reads in a listing, as the flag that named it.
    pub fn label(&self) -> String {
        match self {
            Self::Change { id } => format!("change {id}"),
            Self::Fork { slug } => format!("fork {slug}"),
            Self::Range { base, head } => format!("range {base}..{head}"),
        }
    }
}

/// One dispatch, its ordinal within its subject, and its ending.
pub struct Round<'a> {
    /// Absent on a dispatch recorded without a subject. Such a run belongs to
    /// no sequence, so it is numbered in none.
    pub subject: Option<RunSubject>,
    pub round: Option<usize>,
    pub dispatch: &'a Event,
    pub ending: Option<RunEnding<'a>>,
}

/// One deferral no later round on its subject has collected.
pub struct OpenDeferral {
    pub subject: Option<RunSubject>,
    pub dispatch_event_id: String,
    pub finding: DeferredFinding,
    pub deferred_at: DateTime<Utc>,
}

/// Every dispatch in the repository, oldest first, numbered within its subject.
///
/// The ordinal is derived here and nowhere else: a round is not a fact anyone
/// records, and a second derivation of it would disagree the first time a
/// dispatch was written out of order.
pub fn rounds(events: &[Event]) -> Vec<Round<'_>> {
    let mut counts: HashMap<RunSubject, usize> = HashMap::new();
    let mut rounds = Vec::new();
    for event in events {
        if !matches!(event.payload, Payload::RunDispatched { .. }) {
            continue;
        }
        let subject = RunSubject::of(&event.payload);
        let round = subject.as_ref().map(|subject| {
            let count = counts.entry(subject.clone()).or_insert(0);
            *count += 1;
            *count
        });
        rounds.push(Round {
            subject,
            round,
            dispatch: event,
            ending: ending_for(events, &event.event_id),
        });
    }
    rounds
}

/// Deferrals still waiting, newest first.
///
/// A deferral is open until a later round on the same subject collects it.
/// Collection is scoped to the subject because an ID is only meaningful in the
/// sequence that minted it, and a round on other work cannot discharge an
/// obligation it never read.
pub fn open_deferrals(events: &[Event]) -> Vec<OpenDeferral> {
    let rounds = rounds(events);
    let mut open = Vec::new();
    for round in &rounds {
        let Some(ending) = &round.ending else {
            continue;
        };
        for finding in ending.deferred {
            let collected = rounds.iter().any(|later| {
                later.subject == round.subject
                    && later.ending.as_ref().is_some_and(|later_ending| {
                        later_ending.event.created_at > ending.event.created_at
                            && later_ending.collects.iter().any(|id| id == &finding.id)
                    })
            });
            if collected {
                continue;
            }
            open.push(OpenDeferral {
                subject: round.subject.clone(),
                dispatch_event_id: round.dispatch.event_id.clone(),
                finding: finding.clone(),
                deferred_at: ending.event.created_at,
            });
        }
    }
    open.sort_by_key(|deferral| std::cmp::Reverse(deferral.deferred_at));
    open
}

#[derive(Debug, Serialize)]
struct RunRow {
    dispatch_event_id: String,
    route: String,
    worktree: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<RunSubject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    round: Option<usize>,
    change: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fork: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<CommitRange>,
    brief_event_id: Option<String>,
    note: Option<String>,
    dispatched_at: DateTime<Utc>,
    ending_event_id: Option<String>,
    ended_at: Option<DateTime<Utc>>,
    outcome: Option<RunOutcome>,
    ending_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reviewed_head: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    raised: Vec<RunFinding>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    deferred: Vec<DeferredFinding>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    collects: Vec<String>,
    /// The subset of `deferred` no later round on this subject collected.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    open_deferrals: Vec<DeferredFinding>,
}

/// Everything one dispatch records.
pub struct DispatchInput {
    pub route: String,
    pub worktree: String,
    pub change: Option<String>,
    pub fork: Option<String>,
    pub range: Option<String>,
    pub brief_event_id: Option<String>,
    pub note: Option<String>,
}

/// Record that a caller dispatched a run through a resolved route.
pub fn dispatch_run(ctx: &Ctx, dispatch: DispatchInput) -> Result<()> {
    let DispatchInput {
        route,
        worktree,
        change,
        fork,
        range,
        brief_event_id,
        note,
    } = dispatch;
    let named: Vec<&str> = [
        change.as_ref().map(|_| "--change"),
        fork.as_ref().map(|_| "--fork"),
        range.as_ref().map(|_| "--range"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if named.is_empty() {
        bail!(
            "run dispatch refused: name what the run is against with --change <id>, --fork <slug>, or --range <base>..<head>"
        );
    }
    if named.len() > 1 {
        bail!(
            "run dispatch refused: a run has one subject, and {} were given",
            named.join(" and ")
        );
    }
    let range = range
        .as_deref()
        .map(CommitRange::parse)
        .transpose()
        .map_err(|error| anyhow::anyhow!("run dispatch refused: {error}"))?;

    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    let earlier = store.load_repository_events()?;
    let event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::RunDispatched {
            route,
            worktree,
            change,
            fork,
            range,
            brief_event_id,
            note,
        },
    );
    let subject = RunSubject::of(&event.payload);
    let round = rounds(&earlier)
        .iter()
        .filter(|round| round.subject == subject)
        .count()
        + 1;
    store.append_repository_event(&event)?;
    println!("run dispatched");
    if let Some(subject) = &subject {
        println!("round: {round} of {}", subject.label());
    }
    println!("event: {}", event.event_id);
    Ok(())
}

/// Everything `run end` records beyond the dispatch it ends.
pub struct EndingInput {
    pub outcome: RunOutcome,
    pub reviewed_head: Option<String>,
    pub raised_json: Option<String>,
    pub deferred_json: Option<String>,
    pub collects: Vec<String>,
    pub note: Option<String>,
}

/// Record a terminal outcome for a previously recorded dispatch.
pub fn end_run(ctx: &Ctx, dispatch_event_id: &str, ending: EndingInput) -> Result<()> {
    // Read the findings before touching the ledger so a malformed file leaves
    // no half-recorded ending behind.
    let raised = read_raised(ending.raised_json.as_deref())?;
    let deferred = read_deferred(ending.deferred_json.as_deref())?;

    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    let events = store.load_repository_events()?;
    let Some(dispatch) = events
        .iter()
        .find(|event| event.event_id == dispatch_event_id)
    else {
        if let Some(event) = find_change_event(&store, dispatch_event_id)? {
            let kind = crate::render::event_kind_summary(&event.payload).0;
            bail!(
                "run end refused: event {dispatch_event_id} is {kind}, not a repository-scoped run-dispatched event"
            );
        }
        bail!("run end refused: no event has id {dispatch_event_id}");
    };
    if !matches!(dispatch.payload, Payload::RunDispatched { .. }) {
        let kind = crate::render::event_kind_summary(&dispatch.payload).0;
        bail!("run end refused: event {dispatch_event_id} is {kind}, not a run-dispatched event");
    }
    if let Some(existing) = ending_for(&events, dispatch_event_id) {
        bail!(
            "run end refused: dispatch {dispatch_event_id} already ended with {} in event {}",
            existing.outcome.as_str(),
            existing.event.event_id
        );
    }
    // A collected ID has to name a deferral actually waiting on this subject.
    // Accepting an unknown one would let a round report an obligation
    // discharged that no round ever took on.
    let subject = RunSubject::of(&dispatch.payload);
    let open = open_deferrals(&events);
    for id in &ending.collects {
        if !open
            .iter()
            .any(|deferral| deferral.subject == subject && &deferral.finding.id == id)
        {
            bail!(
                "run end refused: no open deferral {id} on {}",
                subject
                    .as_ref()
                    .map(RunSubject::label)
                    .unwrap_or_else(|| "this run's subject".to_string())
            );
        }
    }

    let outcome = ending.outcome;
    let event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::RunEnded {
            dispatch_event_id: dispatch_event_id.to_string(),
            outcome,
            reviewed_head: ending.reviewed_head,
            raised,
            deferred: deferred.clone(),
            collects: ending.collects,
            note: ending.note,
        },
    );
    store.append_repository_event(&event)?;
    println!("run ended: {}", outcome.as_str());
    for finding in &deferred {
        println!(
            "deferred: {} {}",
            finding.id,
            crate::render::one_line(&finding.summary)
        );
    }
    println!("event: {}", event.event_id);
    Ok(())
}

/// A finding as a caller writes it. A deferral may name its own ID; every
/// other field is the recorded one, so the file and the record read alike.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FindingInput {
    summary: String,
    #[serde(default)]
    severity: Option<Severity>,
    #[serde(default)]
    why: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

fn read_findings(source: Option<&str>) -> Result<Vec<FindingInput>> {
    let Some(source) = source else {
        return Ok(Vec::new());
    };
    let text = if source == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("cannot read findings from stdin")?;
        buf
    } else {
        std::fs::read_to_string(source)
            .with_context(|| format!("cannot read findings file {source}"))?
    };
    serde_json::from_str(&text).with_context(|| {
        format!("malformed findings JSON from {source}: expected an array of objects, each with a summary")
    })
}

fn read_raised(source: Option<&str>) -> Result<Vec<RunFinding>> {
    read_findings(source)?
        .into_iter()
        .map(|finding| {
            if finding.why.is_some() || finding.id.is_some() {
                bail!("a raised finding takes a summary and an optional severity; 'why' and 'id' belong to a deferral");
            }
            Ok(RunFinding {
                summary: finding.summary,
                severity: finding.severity,
            })
        })
        .collect()
}

fn read_deferred(source: Option<&str>) -> Result<Vec<DeferredFinding>> {
    read_findings(source)?
        .into_iter()
        .map(|finding| {
            let Some(why) = finding.why.filter(|why| !why.trim().is_empty()) else {
                bail!(
                    "deferred finding {:?} needs a why: a deferral without a reason cannot be told from a finding that was missed",
                    crate::render::one_line(&finding.summary)
                );
            };
            Ok(DeferredFinding {
                id: finding.id.unwrap_or_else(ids::new_deferral_id),
                summary: finding.summary,
                severity: finding.severity,
                why,
            })
        })
        .collect()
}

/// List every recorded dispatch grouped by the subject it ran against, with
/// the rounds numbered within each. An absent ending remains visible as
/// `open`.
pub fn list_runs(ctx: &Ctx, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let events = store.load_repository_events()?;
    let open = open_deferrals(&events);
    let rows = rounds(&events)
        .iter()
        .rev()
        .map(|round| run_row(round, &open))
        .collect::<Vec<_>>();

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no runs");
        return Ok(());
    }
    // Subjects in most-recent-dispatch order, rounds ascending within each:
    // the sequence only reads as a sequence forwards.
    let mut order: Vec<Option<RunSubject>> = Vec::new();
    for row in &rows {
        if !order.contains(&row.subject) {
            order.push(row.subject.clone());
        }
    }
    for subject in order {
        let heading = subject
            .as_ref()
            .map(RunSubject::label)
            .unwrap_or_else(|| "unattributed".to_string());
        let group = rows
            .iter()
            .rev()
            .filter(|row| row.subject == subject)
            .collect::<Vec<_>>();
        println!("{heading}  {} round(s)", group.len());
        for row in group {
            print_round(row);
        }
    }
    Ok(())
}

fn print_round(row: &RunRow) {
    let outcome = row.outcome.map(RunOutcome::as_str).unwrap_or("open");
    let round = row
        .round
        .map_or_else(|| "round -".to_string(), |round| format!("round {round}"));
    let mut line = format!(
        "  {round}  {outcome}  route={}  worktree={}  dispatched={}  dispatch={}",
        crate::render::one_line(&row.route),
        crate::render::one_line(&row.worktree),
        row.dispatched_at.format("%Y-%m-%dT%H:%M:%SZ"),
        row.dispatch_event_id
    );
    if let Some(change) = &row.change {
        line.push_str(&format!("  change={}", crate::render::one_line(change)));
    }
    if let Some(note) = &row.note {
        line.push_str(&format!("  note={}", crate::render::one_line(note)));
    }
    if let Some(note) = &row.ending_note {
        line.push_str(&format!("  ending-note={}", crate::render::one_line(note)));
    }
    println!("{line}");

    let mut ending = Vec::new();
    if let Some(head) = &row.reviewed_head {
        ending.push(format!("reviewed={}", crate::render::short_sha(head)));
    }
    if !row.raised.is_empty() {
        ending.push(format!("raised={}", row.raised.len()));
    }
    if !row.deferred.is_empty() {
        ending.push(format!(
            "deferred={}  open-deferrals={}",
            row.deferred.len(),
            row.open_deferrals.len()
        ));
    }
    if !row.collects.is_empty() {
        ending.push(format!("collects={}", row.collects.join(",")));
    }
    if !ending.is_empty() {
        println!("    {}", ending.join("  "));
    }
    for finding in &row.open_deferrals {
        println!(
            "    open {}: {} — {}",
            finding.id,
            crate::render::one_line(&finding.summary),
            crate::render::one_line(&finding.why)
        );
    }
}

fn run_row(round: &Round<'_>, open: &[OpenDeferral]) -> RunRow {
    let Payload::RunDispatched {
        route,
        worktree,
        change,
        fork,
        range,
        brief_event_id,
        note,
    } = &round.dispatch.payload
    else {
        unreachable!("rounds() yields dispatch events only");
    };
    let ending = round.ending.as_ref();
    let deferred: Vec<DeferredFinding> = ending
        .map(|ending| ending.deferred.clone())
        .unwrap_or_default();
    let open_deferrals = deferred
        .iter()
        .filter(|finding| {
            open.iter().any(|deferral| {
                deferral.dispatch_event_id == round.dispatch.event_id
                    && deferral.finding.id == finding.id
            })
        })
        .cloned()
        .collect();
    RunRow {
        dispatch_event_id: round.dispatch.event_id.clone(),
        route: route.clone(),
        worktree: worktree.clone(),
        subject: round.subject.clone(),
        round: round.round,
        change: change.clone(),
        fork: fork.clone(),
        range: range.clone(),
        brief_event_id: brief_event_id.clone(),
        note: note.clone(),
        dispatched_at: round.dispatch.created_at,
        ending_event_id: ending.map(|ending| ending.event.event_id.clone()),
        ended_at: ending.map(|ending| ending.event.created_at),
        outcome: ending.map(|ending| *ending.outcome),
        ending_note: ending.and_then(|ending| ending.note.clone()),
        reviewed_head: ending.and_then(|ending| ending.reviewed_head.clone()),
        raised: ending
            .map(|ending| ending.raised.clone())
            .unwrap_or_default(),
        deferred,
        collects: ending
            .map(|ending| ending.collects.clone())
            .unwrap_or_default(),
        open_deferrals,
    }
}

/// The ending recorded against one dispatch, with the payload already read.
/// Returning the fields rather than the event keeps every caller off a
/// re-match that could only fail by construction.
pub struct RunEnding<'a> {
    pub event: &'a Event,
    pub outcome: &'a RunOutcome,
    pub note: &'a Option<String>,
    pub reviewed_head: &'a Option<String>,
    pub raised: &'a Vec<RunFinding>,
    pub deferred: &'a Vec<DeferredFinding>,
    pub collects: &'a Vec<String>,
}

fn ending_for<'a>(events: &'a [Event], dispatch_event_id: &str) -> Option<RunEnding<'a>> {
    events.iter().rev().find_map(|event| match &event.payload {
        Payload::RunEnded {
            dispatch_event_id: id,
            outcome,
            note,
            reviewed_head,
            raised,
            deferred,
            collects,
        } if id == dispatch_event_id => Some(RunEnding {
            event,
            outcome,
            note,
            reviewed_head,
            raised,
            deferred,
            collects,
        }),
        _ => None,
    })
}

fn find_change_event(store: &Store, event_id: &str) -> Result<Option<Event>> {
    for change_id in store.list_change_ids()? {
        if let Some(event) = store
            .load_events(&change_id)?
            .into_iter()
            .find(|event| event.event_id == event_id)
        {
            return Ok(Some(event));
        }
    }
    Ok(None)
}
