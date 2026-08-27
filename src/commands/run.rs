//! Repository-scoped records for delegated run dispatch and terminal outcomes.

use super::*;
use crate::model::{Event, Payload, RunOutcome};
use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct RunRow {
    dispatch_event_id: String,
    route: String,
    worktree: String,
    change: Option<String>,
    brief_event_id: Option<String>,
    note: Option<String>,
    dispatched_at: DateTime<Utc>,
    ending_event_id: Option<String>,
    ended_at: Option<DateTime<Utc>>,
    outcome: Option<RunOutcome>,
    ending_note: Option<String>,
}

/// Record that a caller dispatched a run through a resolved route.
pub fn dispatch_run(
    ctx: &Ctx,
    route: String,
    worktree: String,
    change: Option<String>,
    brief_event_id: Option<String>,
    note: Option<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    let event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::RunDispatched {
            route,
            worktree,
            change,
            brief_event_id,
            note,
        },
    );
    store.append_repository_event(&event)?;
    println!("run dispatched");
    println!("event: {}", event.event_id);
    Ok(())
}

/// Record a terminal outcome for a previously recorded dispatch.
pub fn end_run(
    ctx: &Ctx,
    dispatch_event_id: &str,
    outcome: RunOutcome,
    note: Option<String>,
) -> Result<()> {
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
    if let Some(ending) = ending_for(&events, dispatch_event_id) {
        bail!(
            "run end refused: dispatch {dispatch_event_id} already ended with {} in event {}",
            ending.outcome.as_str(),
            ending.event.event_id
        );
    }

    let event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::RunEnded {
            dispatch_event_id: dispatch_event_id.to_string(),
            outcome,
            note,
        },
    );
    store.append_repository_event(&event)?;
    println!("run ended: {}", outcome.as_str());
    println!("event: {}", event.event_id);
    Ok(())
}

/// List every recorded dispatch, newest first, with its terminal outcome when
/// one has been recorded. An absent ending remains visible as `open`.
pub fn list_runs(ctx: &Ctx, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let events = store.load_repository_events()?;
    let rows = events
        .iter()
        .rev()
        .filter_map(|event| {
            let Payload::RunDispatched {
                route,
                worktree,
                change,
                brief_event_id,
                note,
            } = &event.payload
            else {
                return None;
            };
            Some(run_row(
                event,
                route,
                worktree,
                change,
                brief_event_id,
                note,
                ending_for(&events, &event.event_id),
            ))
        })
        .collect::<Vec<_>>();

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no runs");
        return Ok(());
    }
    for row in rows {
        let outcome = row.outcome.map(RunOutcome::as_str).unwrap_or("open");
        let mut line = format!(
            "{outcome}  route={}  worktree={}  dispatched={}  dispatch={}",
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
    }
    Ok(())
}

fn run_row(
    dispatch: &Event,
    route: &str,
    worktree: &str,
    change: &Option<String>,
    brief_event_id: &Option<String>,
    note: &Option<String>,
    ending: Option<RunEnding<'_>>,
) -> RunRow {
    let (ending_event_id, ended_at, outcome, ending_note) =
        ending.map_or((None, None, None, None), |ending| {
            (
                Some(ending.event.event_id.clone()),
                Some(ending.event.created_at),
                Some(*ending.outcome),
                ending.note.clone(),
            )
        });
    RunRow {
        dispatch_event_id: dispatch.event_id.clone(),
        route: route.to_string(),
        worktree: worktree.to_string(),
        change: change.clone(),
        brief_event_id: brief_event_id.clone(),
        note: note.clone(),
        dispatched_at: dispatch.created_at,
        ending_event_id,
        ended_at,
        outcome,
        ending_note,
    }
}

/// The ending recorded against one dispatch, with the payload already read.
/// Returning the fields rather than the event keeps every caller off a
/// re-match that could only fail by construction.
struct RunEnding<'a> {
    event: &'a Event,
    outcome: &'a RunOutcome,
    note: &'a Option<String>,
}

fn ending_for<'a>(events: &'a [Event], dispatch_event_id: &str) -> Option<RunEnding<'a>> {
    events.iter().rev().find_map(|event| match &event.payload {
        Payload::RunEnded {
            dispatch_event_id: id,
            outcome,
            note,
        } if id == dispatch_event_id => Some(RunEnding {
            event,
            outcome,
            note,
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
