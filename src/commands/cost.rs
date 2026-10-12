//! The work cost view: what the executor sessions bound to a change's or a
//! journal artifact's dispatched runs consumed, measured through tapes, and
//! the retained facts that let the arithmetic replay after a source rotates.
//!
//! Measurement and assignment are separate facts. A session is measured by
//! the pinned tapes reader, or by facts retained from an earlier read; it is
//! assigned to work only through the dispatches bound to it. Consumption no
//! dispatch names is outside assignment coverage and never counted as zero.
//! A total names its quantity kind and is withheld, refused, or absent rather
//! than understated.

use super::cost_read::CostRead;
use super::run::{rounds, RunSubject};
use super::*;
use crate::model::{
    plain_session_id, portable_cost_metadata, portable_cost_reason, portable_lineage_reason,
    portable_quantity_reason, portable_selection, CostCacheWrites, CostConvention,
    CostCounterResidual, CostCounters, CostDisjoint, CostEventCoverage, CostEventIdentity,
    CostEvents, CostFacts, CostLineage, CostPricing, CostRelative, CostResidual,
    CostResidualSource, Event, Payload,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const COST_SCHEMA: &str = "arc-cost/1";

/// The reader a fresh measurement names: tapes' core library at the release
/// and revision Cargo.toml pins, which the build script reads.
pub const TAPES_READER: &str = env!("ARC_TAPES_READER");

/// How arc normalizes a reader's usage answer into retained facts. It moves
/// when that normalization does, so facts from an older one stay identifiable.
pub const COST_CALCULATION: &str = "usage-normalization 3";

const USD_KIND: &str = "harness-calculated-estimate";
const TOKEN_KIND: &str = "resource-quantity";
const CATEGORIES: [&str; 5] = ["input", "output", "reasoning", "cache_read", "cache_write"];
/// tapes' disjoint categories, cache writes also by TTL, and the sum of the
/// four categories.
const DISJOINT: [&str; 9] = [
    "uncached_input",
    "cache_read",
    "cache_write",
    "cache_write_5m",
    "cache_write_1h",
    "cache_write_ttl_unrecorded",
    "output",
    "reasoning",
    "total",
];
const ZERO_PRICE: &str = "recorded zero price accompanies nonzero or unknown usage";
const RESIDUAL_CONFLICT: &str = "recorded total is below its identified requests";

/// Work a dispatched run is assigned to, through the subject it names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum WorkKey {
    Change(String),
    /// Canonical journal directory and newest filename in the transition chain.
    Journal(ArtifactIdentity),
    /// Work this view does not select: a fork, a range, an unselected
    /// artifact, or a dispatch with no subject.
    Other(String),
}

impl WorkKey {
    fn label(&self) -> String {
        match self {
            WorkKey::Change(id) => format!("change:{id}"),
            WorkKey::Journal((_, file)) => format!("journal:{file}"),
            WorkKey::Other(label) => format!("other:{label}"),
        }
    }
}

/// One selected work item: its key, the names a journal artifact carried,
/// and the journal artifact a change was begun from when the view selects it.
struct WorkItem {
    key: WorkKey,
    lineage: Vec<String>,
    journal_members: BTreeSet<ArtifactIdentity>,
    source: Option<WorkKey>,
    complete: bool,
}

type ArtifactIdentity = (std::path::PathBuf, String);

fn artifact_identity(ctx: &Ctx, reference: &str) -> Result<ArtifactIdentity> {
    let location = crate::journal::locate_artifact(ctx, reference)
        .map_err(|_| anyhow::anyhow!("journal membership read failed"))?;
    let directory = std::fs::canonicalize(location.hot)
        .map_err(|_| anyhow::anyhow!("journal membership read failed"))?;
    Ok((directory, location.file))
}

fn journal_item(ctx: &Ctx, reference: &str) -> Result<WorkItem> {
    let lineage = crate::journal::transition_lineage(ctx, reference)
        .map_err(|_| anyhow::anyhow!("journal lineage read failed"))?;
    let journal_members = lineage
        .iter()
        .map(|name| artifact_identity(ctx, name))
        .collect::<Result<_>>()?;
    let latest = lineage.last().context("journal lineage is empty")?;
    Ok(WorkItem {
        key: WorkKey::Journal(artifact_identity(ctx, latest)?),
        lineage,
        journal_members,
        source: None,
        complete: true,
    })
}

/// What the view reports on and the work it selects.
struct Selection {
    target: Value,
    items: Vec<WorkItem>,
    /// Ledger changes among the items, whose own events name the sessions
    /// that acted on them.
    ledger_changes: Vec<String>,
    gaps: Vec<Value>,
    ledger_gaps: Vec<Value>,
    readable: bool,
}

/// One dispatch bound to an executor session.
struct Span {
    dispatch_event_id: String,
    dispatched_at: chrono::DateTime<chrono::Utc>,
    forked_from: Option<String>,
    round: Option<usize>,
    work: WorkKey,
}

/// Where a session's measurement came from.
enum Source {
    Fresh,
    Retained(String, String),
    Unavailable(String),
}

struct Measured {
    source: Source,
    facts: Option<CostFacts>,
    conflict: Option<Value>,
}

struct SessionRow {
    harness: String,
    session: String,
    spans: Vec<Span>,
    membership: BTreeSet<WorkKey>,
    measured: Measured,
    overlap: BTreeSet<String>,
}

impl SessionRow {
    fn key(&self) -> String {
        format!("{}:{}", self.harness, self.session)
    }
}

/// Print the read-only cost view of a change or a journal artifact.
pub fn show_cost(ctx: &Ctx, target: &str, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let mut read = CostRead::new(&store);
    let events = read.events(Store::REPOSITORY_SCOPE);
    let mut selection = select(ctx, &mut read, target, &events)?;
    qualify_ledger(&mut selection, &read.gaps);
    let (runs, sessions) = assign(ctx, &mut selection, &events);
    if !selection.readable && runs.is_empty() && sessions.is_empty() {
        bail!("cost target has no readable metadata");
    }
    let mut sessions: Vec<SessionRow> = sessions
        .into_iter()
        .map(|((harness, session), (spans, membership))| {
            let measured = measure(&events, &harness, &session);
            SessionRow {
                harness,
                session,
                spans,
                membership,
                measured,
                overlap: BTreeSet::new(),
            }
        })
        .collect();
    let groups = lineage_groups(&sessions);
    for row in &mut sessions {
        row.overlap = groups
            .iter()
            .find(|group| group.contains(&row.key()))
            .cloned()
            .unwrap_or_default();
    }
    let lead = outside_sessions(&mut read, &selection, &sessions);
    selection.ledger_gaps = read.gaps;
    let report = render_report(&selection, &runs, &sessions, &lead);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    Ok(())
}

/// Retain the normalized facts a fresh read of each bound session gives.
/// Facts identical to ones already retained for the session record nothing.
pub fn retain_cost(ctx: &Ctx, target: &str) -> Result<()> {
    let store = ctx.store()?;
    let mut read = CostRead::new(&store);
    let events = read.events(Store::REPOSITORY_SCOPE);
    let mut selection = select(ctx, &mut read, target, &events)?;
    qualify_ledger(&mut selection, &read.gaps);
    let (runs, sessions) = assign(ctx, &mut selection, &events);
    if !selection.readable && runs.is_empty() && sessions.is_empty() {
        bail!("cost target has no readable metadata");
    }
    print_read_gaps(&selection.gaps, &read.gaps);
    if sessions.is_empty() {
        println!("no bound sessions");
        return Ok(());
    }
    // Read every source before taking the ledger lock: a recording can be
    // large, and other writers wait on the lock.
    let reads: Vec<(String, String, std::result::Result<CostFacts, String>)> = sessions
        .into_keys()
        .map(|(harness, session)| {
            let read = read_fresh(&harness, &session);
            (harness, session, read)
        })
        .collect();
    let _repository_events = store.lock_repository_events()?;
    let mut held = CostRead::new(&store);
    let events = held.events(Store::REPOSITORY_SCOPE);
    print_read_gaps(&[], &held.gaps);
    for (harness, session, read) in reads {
        let key = format!("{harness}:{session}");
        let facts = match read {
            Ok(facts) => facts,
            Err(reason) => {
                println!("not retained: {key}: {reason}");
                continue;
            }
        };
        if let Some((event, _)) = retained_for(&events, &harness, &session)
            .into_iter()
            .find(|(_, held)| **held == facts)
        {
            println!("already retained: {key} frontier={}", facts.frontier);
            println!("event: {}", event.event_id);
            continue;
        }
        let frontier = facts.frontier;
        let mut event = ctx.event(
            &store,
            Store::REPOSITORY_SCOPE,
            Payload::CostFactsRetained {
                executor_harness: harness,
                executor_session: session,
                facts,
            },
        )?;
        store.append_local_repository_event(&mut event)?;
        println!("retained: {key} frontier={frontier}");
        println!("event: {}", event.event_id);
    }
    Ok(())
}

fn print_read_gaps(selection: &[Value], ledger: &[Value]) {
    for gap in selection.iter().chain(ledger) {
        println!(
            "coverage incomplete: {}: {}",
            gap["record"].as_str().unwrap_or("record"),
            gap["reason"].as_str().unwrap_or("record unavailable")
        );
    }
}

fn looks_like_journal(target: &str) -> bool {
    target.ends_with(".md") || crate::journal::split_qualified(target).is_some()
}

fn reference_file(reference: &str) -> &str {
    crate::journal::split_qualified(reference).map_or(reference, |(_, file)| file)
}

fn selection_gap(gaps: &mut Vec<Value>, record: &str, reason: &str, keys: &[WorkKey]) {
    gaps.push(json!({"record": record, "reason": reason, "work": keys.iter().map(WorkKey::label).collect::<Vec<_>>()}));
}

fn qualify_ledger(selection: &mut Selection, gaps: &[Value]) {
    let keys: Vec<_> = selection
        .items
        .iter()
        .map(|item| item.key.clone())
        .collect();
    for gap in gaps.iter().filter(|gap| {
        gap["assignment"] != false
            && gap["record"].as_str().is_some_and(|record| {
                record == Store::REPOSITORY_SCOPE
                    || record.starts_with(&format!("{}:", Store::REPOSITORY_SCOPE))
                    || record == "import"
                    || record.starts_with("import:")
            })
    }) {
        selection_gap(
            &mut selection.gaps,
            gap["record"].as_str().unwrap_or("ledger"),
            "repository assignment records unavailable",
            &keys,
        );
    }
    let journals: Vec<_> = keys
        .into_iter()
        .filter(|key| matches!(key, WorkKey::Journal(_)))
        .collect();
    for gap in gaps.iter().filter(|gap| gap["record"] == "changes") {
        selection_gap(
            &mut selection.gaps,
            gap["record"].as_str().unwrap_or("changes"),
            "change journal membership unavailable",
            &journals,
        );
    }
}

fn selected_journal(ctx: &Ctx, reference: &str, gaps: &mut Vec<Value>) -> WorkItem {
    match journal_item(ctx, reference) {
        Ok(item) => item,
        Err(_) => {
            let identity = artifact_identity(ctx, reference).unwrap_or_else(|_| {
                (
                    std::path::PathBuf::new(),
                    reference_file(reference).to_string(),
                )
            });
            let key = WorkKey::Journal(identity);
            selection_gap(
                gaps,
                &key.label(),
                "journal lineage or identity unavailable",
                std::slice::from_ref(&key),
            );
            WorkItem {
                key,
                lineage: vec![reference.to_string()],
                journal_members: BTreeSet::new(),
                source: None,
                complete: false,
            }
        }
    }
}

fn opened_reference(events: &[Event]) -> Option<Option<String>> {
    events.iter().find_map(|event| match &event.payload {
        Payload::ChangeOpened { journal_ref, .. } => Some(journal_ref.clone()),
        _ => None,
    })
}

fn possible_journal(item: &WorkItem, reference: &str) -> bool {
    !item.complete
        || item
            .lineage
            .iter()
            .any(|name| reference_file(name) == reference_file(reference))
}

/// A change selects itself and its source artifact; an artifact selects its
/// transition lineage and changes begun from it. A citation selects nothing.
fn select(ctx: &Ctx, read: &mut CostRead<'_>, target: &str, events: &[Event]) -> Result<Selection> {
    let mut gaps = Vec::new();
    if looks_like_journal(target) {
        let item = selected_journal(ctx, target, &mut gaps);
        let journal = item.key.clone();
        let lineage = item.lineage.clone();
        let mut items = vec![item];
        let mut ledger_changes = Vec::new();
        for change_id in read.change_ids() {
            let change_events = read.events(&change_id);
            match opened_reference(&change_events) {
                Some(Some(reference)) if possible_journal(&items[0], &reference) => {
                    match artifact_identity(ctx, &reference) {
                        Ok(identity) if items[0].journal_members.contains(&identity) => {
                            items.push(WorkItem {
                                key: WorkKey::Change(change_id.clone()),
                                lineage: Vec::new(),
                                journal_members: BTreeSet::new(),
                                source: Some(journal.clone()),
                                complete: true,
                            });
                            ledger_changes.push(change_id);
                        }
                        Ok(_) if items[0].complete => {}
                        _ => selection_gap(
                            &mut gaps,
                            &change_id,
                            "change journal membership unavailable",
                            std::slice::from_ref(&journal),
                        ),
                    }
                }
                None => selection_gap(
                    &mut gaps,
                    &change_id,
                    "change opening unreadable",
                    std::slice::from_ref(&journal),
                ),
                _ => {}
            }
        }
        return Ok(Selection {
            target: json!({"kind": "journal", "file": target, "lineage": lineage}),
            items,
            ledger_changes,
            gaps,
            ledger_gaps: Vec::new(),
            readable: crate::journal::artifact_digest(ctx, target).is_ok(),
        });
    }
    let ids = read.change_ids();
    let matches: Vec<&String> = ids
        .iter()
        .filter(|id| id.as_str() == target || id.starts_with(target))
        .collect();
    let change_id = if ids.iter().any(|id| id == target) {
        target.to_string()
    } else if matches.len() == 1 {
        matches[0].clone()
    } else if matches.len() > 1 {
        bail!("cost target matches multiple changes");
    } else if rounds(events)
        .iter()
        .any(|round| matches!(&round.subject, Some(RunSubject::Change { id }) if id == target))
    {
        target.to_string()
    } else {
        bail!("cost target has no readable change or dispatch");
    };
    let held = ids.contains(&change_id);
    let reference = held
        .then(|| opened_reference(&read.events(&change_id)))
        .flatten();
    let mut items = vec![WorkItem {
        key: WorkKey::Change(change_id.clone()),
        lineage: Vec::new(),
        journal_members: BTreeSet::new(),
        source: None,
        complete: true,
    }];
    if held && reference.is_none() {
        gaps.push(
            json!({"record": change_id, "reason": "change opening unreadable",
            "work": [items[0].key.label()], "scopes": ["inclusive", "union"]}),
        );
    }
    let readable = reference.is_some();
    if let Some(Some(reference)) = reference {
        let item = selected_journal(ctx, &reference, &mut gaps);
        items[0].source = Some(item.key.clone());
        items.push(item);
    }
    Ok(Selection {
        target: json!({"kind": "change", "id": change_id.clone()}),
        items,
        ledger_changes: if held { vec![change_id] } else { Vec::new() },
        gaps,
        ledger_gaps: Vec::new(),
        readable,
    })
}

struct AssignedRun {
    dispatch_event_id: String,
    round: Option<usize>,
    candidates: Vec<String>,
    conflicting: bool,
    invalid: bool,
}

type Runs = BTreeMap<WorkKey, Vec<AssignedRun>>;
type Sessions = BTreeMap<(String, String), (Vec<Span>, BTreeSet<WorkKey>)>;

/// The selected runs per work item, and every bound session with at least
/// one span in the selection, with all of its spans across the repository:
/// a span on unselected work makes the session shared beyond the view. A run
/// is a change's when its dispatch names that change's exact id, so what a
/// run is assigned to never moves as other changes are opened.
fn assign(ctx: &Ctx, selection: &mut Selection, events: &[Event]) -> (Runs, Sessions) {
    let selected: BTreeSet<WorkKey> = selection
        .items
        .iter()
        .map(|item| item.key.clone())
        .collect();
    let mut runs: Runs = BTreeMap::new();
    let mut sessions: Sessions = BTreeMap::new();
    let mut possible_sessions = BTreeSet::new();
    for round in rounds(events) {
        let mut uncertain = Vec::new();
        let work = match &round.subject {
            Some(RunSubject::Change { id }) => {
                for gap in &selection.gaps {
                    if gap["record"] == *id && gap["reason"] == "change opening unreadable" {
                        uncertain.extend(
                            selection
                                .items
                                .iter()
                                .filter(|item| {
                                    matches!(item.key, WorkKey::Journal(_))
                                        && gap["work"].as_array().is_some_and(|keys| {
                                            keys.contains(&json!(item.key.label()))
                                        })
                                })
                                .map(|item| item.key.clone()),
                        );
                    }
                }
                WorkKey::Change(id.clone())
            }
            Some(RunSubject::Journal { file }) => {
                let candidates: Vec<_> = selection
                    .items
                    .iter()
                    .filter(|item| {
                        matches!(item.key, WorkKey::Journal(_)) && possible_journal(item, file)
                    })
                    .collect();
                match if candidates.is_empty() {
                    None
                } else {
                    Some(artifact_identity(ctx, file))
                } {
                    Some(Ok(identity)) => candidates
                        .iter()
                        .find(|item| item.journal_members.contains(&identity))
                        .map(|item| item.key.clone())
                        .unwrap_or_else(|| {
                            uncertain.extend(
                                candidates
                                    .iter()
                                    .filter(|item| !item.complete)
                                    .map(|item| item.key.clone()),
                            );
                            WorkKey::Other(format!("journal dispatch {}", round.dispatch.event_id))
                        }),
                    Some(Err(_)) => {
                        uncertain.extend(candidates.iter().map(|item| item.key.clone()));
                        WorkKey::Other(format!(
                            "unresolved journal dispatch {}",
                            round.dispatch.event_id
                        ))
                    }
                    None => WorkKey::Other(format!(
                        "unselected journal dispatch {}",
                        round.dispatch.event_id
                    )),
                }
            }
            Some(subject) => WorkKey::Other(subject.label()),
            None => WorkKey::Other("unattributed".to_string()),
        };
        if !uncertain.is_empty() {
            selection_gap(
                &mut selection.gaps,
                &round.dispatch.event_id,
                "dispatch journal membership unavailable",
                &uncertain,
            );
        }
        let bindings: Vec<_> = round
            .binding
            .iter()
            .flat_map(|binding| std::iter::once(binding).chain(&binding.conflicts))
            .collect();
        let invalid = bindings.iter().any(|binding| {
            !crate::model::portable_binding(
                binding.executor_harness,
                binding.executor_session,
                binding.handle,
                binding.forked_from,
            )
        });
        let readable: Vec<_> = bindings
            .iter()
            .filter(|binding| {
                plain_session_id(binding.executor_harness)
                    && plain_session_id(binding.executor_session)
            })
            .collect();
        let candidates: BTreeSet<String> = readable
            .iter()
            .map(|binding| format!("{}:{}", binding.executor_harness, binding.executor_session))
            .collect();
        if selected.contains(&work) {
            runs.entry(work.clone()).or_default().push(AssignedRun {
                dispatch_event_id: round.dispatch.event_id.clone(),
                round: round.round,
                candidates: candidates.into_iter().collect(),
                conflicting: bindings.len() > 1,
                invalid,
            });
        }
        if invalid && (selected.contains(&work) || !uncertain.is_empty()) {
            selection_gap(
                &mut selection.gaps,
                &round.dispatch.event_id,
                "binding metadata nonportable",
                if uncertain.is_empty() {
                    std::slice::from_ref(&work)
                } else {
                    &uncertain
                },
            );
        }
        for binding in readable {
            let key = (
                binding.executor_harness.clone(),
                binding.executor_session.clone(),
            );
            if !uncertain.is_empty() {
                possible_sessions.insert(key.clone());
            }
            let (spans, membership) = sessions.entry(key).or_default();
            membership.insert(work.clone());
            spans.push(Span {
                dispatch_event_id: round.dispatch.event_id.clone(),
                dispatched_at: round.dispatch.created_at,
                forked_from: binding
                    .forked_from
                    .as_ref()
                    .filter(|id| plain_session_id(id))
                    .cloned(),
                round: round.round,
                work: work.clone(),
            });
        }
    }
    sessions.retain(|key, (_, membership)| {
        possible_sessions.contains(key) || membership.iter().any(|work| selected.contains(work))
    });
    (runs, sessions)
}

/// Every retained observation of one session, oldest first.
fn retained_for<'a>(
    events: &'a [Event],
    harness: &str,
    session: &str,
) -> Vec<(&'a Event, &'a CostFacts)> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::CostFactsRetained {
                executor_harness,
                executor_session,
                facts,
            } if executor_harness == harness && executor_session == session => Some((event, facts)),
            _ => None,
        })
        .collect()
}

/// A fresh read when the source answers, else the retained facts at the
/// furthest frontier. A fresh read that disagrees with facts retained at the
/// same frontier carries the conflict; neither is added to the other.
fn measure(events: &[Event], harness: &str, session: &str) -> Measured {
    // The furthest frontier, and among equal frontiers the latest retention.
    let retained = retained_for(events, harness, session)
        .into_iter()
        .max_by_key(|(_, facts)| facts.frontier)
        .map(|(event, facts)| (event, sanitized_facts(facts)));
    match read_fresh(harness, session) {
        Ok(fresh) => {
            let conflict = retained
                .as_ref()
                .filter(|(_, held)| {
                    held.frontier == fresh.frontier
                        && held.calculation == fresh.calculation
                        && !same_measurement(held, &fresh)
                })
                .map(|(event, held)| {
                    json!({
                        "retained_event_id": event.event_id,
                        "frontier": held.frontier,
                        "retained": measurement_facts(held),
                        "fresh": measurement_facts(&fresh),
                    })
                });
            Measured {
                source: Source::Fresh,
                facts: Some(fresh),
                conflict,
            }
        }
        Err(reason) => match retained {
            Some((event, held)) => Measured {
                source: Source::Retained(event.event_id.clone(), reason),
                facts: Some(held),
                conflict: None,
            },
            None => Measured {
                source: Source::Unavailable(reason),
                facts: None,
                conflict: None,
            },
        },
    }
}

/// Two reads measured the same thing, whichever reader made them.
fn same_measurement(left: &CostFacts, right: &CostFacts) -> bool {
    let mut left = left.clone();
    left.reader.clone_from(&right.reader);
    left == *right
}

/// The facts a conflict compares, without the reader and calculation names.
fn measurement_facts(facts: &CostFacts) -> Value {
    let mut value = serde_json::to_value(facts).unwrap_or_default();
    if let Some(object) = value.as_object_mut() {
        for name in ["reader", "calculation", "frontier"] {
            object.remove(name);
        }
    }
    value
}

/// Measure one session through the pinned tapes reader, over its whole
/// recording.
fn read_fresh(harness: &str, session: &str) -> std::result::Result<CostFacts, String> {
    let backends: Vec<Box<dyn agent_tapes_core::backend::Backend>> =
        agent_tapes_core::backend::backends()
            .into_iter()
            .filter(|backend| backend.harness() == harness)
            .collect();
    if backends.is_empty() {
        return Err("session store unsupported".to_string());
    }
    let resolved = agent_tapes_core::resolve_session(&backends, session)
        .map_err(|_| "usage read failed".to_string())?;
    if resolved.session.id != session {
        return Err("usage identity mismatch".to_string());
    }
    let backend = &backends[resolved.backend_index];
    let mut tally = agent_tapes_core::usage::TurnTally::default();
    let mut earliest = None;
    let mut read = backend
        .stream_transcript(&resolved.session, None, &mut |turn| {
            earliest = earliest.into_iter().chain(turn.ts).min();
            tally.add(&turn);
            Ok(())
        })
        .map_err(|_| "usage read failed".to_string())?;
    let stream_notes = read.notes.len();
    read.notes
        .extend(resolved.diagnostics.latest_warnings(&resolved.session));
    let whole = backend
        .stream_session(&resolved.session, &read)
        .map_err(|_| "usage read failed".to_string())?;
    let view = agent_tapes_core::usage::streamed(&whole, tally, &read);
    let accounting_notes = whole
        .usage_detail
        .as_ref()
        .map_or(0, |detail| detail.accounting_notes.len());
    let mut facts = normalize(&view, accounting_notes + stream_notes, harness);
    if harness == "claude" {
        facts.residual = Some(residual(
            backend.as_ref(),
            &resolved.session,
            facts.frontier,
        ));
        qualify_residual(&mut facts);
    }
    facts.source_started_at = earliest.into_iter().chain(whole.started_at).min();
    facts.source_start_uncertain = whole.start_uncertain
        || view.truncated
        || !view.truncation.is_empty()
        || view.read.as_ref().is_none_or(|read| !read.gaps.is_empty());
    let fork_parent_unestablished = view
        .consumption
        .as_ref()
        .is_some_and(|consumption| consumption.identity.fork_parent_unestablished);
    facts.lineage = Some(
        match agent_tapes_core::lineage_with_backends(
            &backends,
            agent_tapes_core::Selection::IdInHarness {
                id: session,
                harness,
            },
        ) {
            Ok(view) if view.session.id == session => {
                let mut unknown = Vec::new();
                if !view.notes.is_empty() {
                    unknown.push(format!("{} lineage notes", view.notes.len()));
                }
                if view.truncated {
                    unknown.push("lineage read bounded".to_string());
                }
                let parent = view.lineage.parent.and_then(|parent| {
                    if !parent.resolved && plain_session_id(&parent.native_id) {
                        unknown.push("lineage relative unresolved".to_string());
                    }
                    safe_relative(parent.native_id, &mut unknown)
                });
                let children = view
                    .lineage
                    .children
                    .into_iter()
                    .filter_map(|child| {
                        if !child.resolved {
                            unknown.push("lineage relative unresolved".to_string());
                        }
                        let Some(session) = child.session_id else {
                            unknown.push("lineage relative unresolved".to_string());
                            return None;
                        };
                        if !plain_session_id(&child.harness) {
                            unknown.push("lineage relative is not a plain session id".to_string());
                            return None;
                        }
                        safe_relative(session, &mut unknown).map(|session| CostRelative {
                            harness: child.harness,
                            session,
                        })
                    })
                    .collect();
                let forked_from = view
                    .lineage
                    .forked_from
                    .and_then(|id| safe_relative(id, &mut unknown));
                if fork_parent_unestablished {
                    unknown.push("lineage relative unresolved".to_string());
                }
                unknown.sort();
                unknown.dedup();
                CostLineage {
                    parent,
                    children,
                    forked_from,
                    unknown,
                }
            }
            Ok(_) => CostLineage {
                unknown: vec!["lineage identity mismatch".to_string()],
                ..CostLineage::default()
            },
            Err(_) => CostLineage {
                unknown: vec!["lineage read failed".to_string()],
                ..CostLineage::default()
            },
        },
    );
    Ok(facts)
}

fn safe_relative(id: String, unknown: &mut Vec<String>) -> Option<String> {
    if plain_session_id(&id) {
        Some(id)
    } else {
        unknown.push("lineage relative is not a plain session id".to_string());
        None
    }
}

fn sanitized_facts(held: &CostFacts) -> CostFacts {
    let mut facts = held.clone();
    for value in [&mut facts.reader, &mut facts.calculation] {
        if !portable_cost_metadata(value) {
            *value = "retained metadata unavailable".to_string();
            facts
                .incomplete
                .push("retained measurement gap".to_string());
        }
    }
    if facts
        .basis
        .as_deref()
        .is_some_and(|basis| !matches!(basis, "recorded-total" | "summed-requests"))
    {
        facts.basis = None;
        facts
            .incomplete
            .push("retained measurement gap".to_string());
    }
    if facts
        .coverage
        .as_deref()
        .is_some_and(|coverage| !matches!(coverage, "session" | "read-window" | "since-reset"))
    {
        facts.coverage = None;
        facts
            .incomplete
            .push("retained measurement gap".to_string());
    }
    if facts
        .digest
        .as_ref()
        .is_some_and(|digest| !portable_cost_metadata(digest))
    {
        facts.digest = None;
        facts
            .incomplete
            .push("retained measurement gap".to_string());
    }
    if facts
        .selection
        .as_deref()
        .is_some_and(|selection| !portable_selection(selection))
    {
        facts.selection = None;
        facts
            .incomplete
            .push("retained measurement gap".to_string());
    }
    if facts
        .disjoint
        .as_ref()
        .is_some_and(|disjoint| !disjoint.portable())
    {
        facts.disjoint = None;
        facts
            .incomplete
            .push("retained measurement gap".to_string());
    }
    if facts
        .events
        .as_ref()
        .is_some_and(|events| !events.portable())
    {
        facts.events = None;
        facts
            .incomplete
            .push("retained measurement gap".to_string());
    }
    if let Some(residual) = &mut facts.residual {
        sanitize_residual(residual);
    }
    if let Some(gaps) = &mut facts.quantity_gaps {
        let unknown_category = gaps
            .keys()
            .any(|key| key != "usd" && !CATEGORIES.contains(&key.as_str()));
        gaps.retain(|key, _| key == "usd" || CATEGORIES.contains(&key.as_str()));
        if unknown_category {
            for category in std::iter::once("usd").chain(CATEGORIES) {
                gaps.entry(category.to_string())
                    .or_default()
                    .push("quantity coverage was not retained".to_string());
            }
        }
        for reason in gaps.values_mut().flatten() {
            if !portable_quantity_reason(reason) {
                *reason = "quantity coverage was not retained".to_string();
            }
        }
    }
    qualify_price(&mut facts);
    for reason in &mut facts.incomplete {
        if !portable_cost_reason(reason) {
            *reason = "retained measurement gap".to_string();
        }
    }
    if let Some(lineage) = &mut facts.lineage {
        let unclassified = lineage
            .unknown
            .iter()
            .filter(|reason| !portable_lineage_reason(reason))
            .count();
        lineage
            .unknown
            .retain(|reason| portable_lineage_reason(reason));
        if unclassified > 0 {
            lineage
                .unknown
                .push(format!("{unclassified} lineage notes"));
        }
        lineage.parent = lineage
            .parent
            .take()
            .and_then(|id| safe_relative(id, &mut lineage.unknown));
        lineage.forked_from = lineage
            .forked_from
            .take()
            .and_then(|id| safe_relative(id, &mut lineage.unknown));
        lineage.children.retain(|child| {
            let safe = plain_session_id(&child.harness) && plain_session_id(&child.session);
            if !safe {
                lineage
                    .unknown
                    .push("lineage relative is not a plain session id".to_string());
            }
            safe
        });
        lineage.unknown.sort();
        lineage.unknown.dedup();
    }
    facts
}

fn kebab<T: serde::Serialize>(value: T) -> Option<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
}

/// The facts arc keeps from one usage answer. The counters and the dollar
/// figure are the reader's, unchanged; what keeps them from covering the
/// session is named rather than corrected. Where the reader sums consumption
/// over every branch a recording retains, that sum is the measurement, not
/// the active branch's.
fn normalize(
    view: &agent_tapes_core::usage::UsageView,
    superseded_notes: usize,
    harness: &str,
) -> CostFacts {
    let consumption = view.consumption.as_ref();
    let mut incomplete = Vec::new();
    let accounting = consumption
        .map(|consumption| &consumption.accounting)
        .or(view.accounting.as_ref());
    let (basis, coverage, selection) = match accounting {
        Some(accounting) => (
            kebab(accounting.basis),
            kebab(accounting.coverage),
            accounting.selection.and_then(kebab),
        ),
        None => {
            incomplete.push("the source recorded no usage counters".to_string());
            (None, None, None)
        }
    };
    if let Some(coverage) = coverage
        .as_deref()
        .filter(|coverage| *coverage != "session")
    {
        incomplete.push(format!(
            "the counters cover the {coverage}, not the whole session"
        ));
    }
    if view.truncated || !view.truncation.is_empty() {
        incomplete.push("the read was bounded and withheld part of the source".to_string());
    }
    // The recording's accounting notes and the backend's own read notes
    // qualify the active-branch figure a consumption sum replaces; notes from
    // diagnostics and skipped records qualify either.
    let notes = match consumption {
        Some(_) => view.notes.len().saturating_sub(superseded_notes),
        None => view.notes.len(),
    };
    if notes > 0 {
        incomplete.push(format!("{notes} usage notes"));
    }
    if let Some(read) = view.read.as_ref().filter(|read| !read.gaps.is_empty()) {
        incomplete.push(format!("{} source read gaps", read.gaps.len()));
    }
    if let Some(consumption) = consumption {
        // Each counter with missing or unpriced events has its own note,
        // which the per-category gaps below carry.
        let per_counter = consumption
            .measurement_coverage
            .values()
            .filter(|coverage| coverage.missing > 0 || coverage.unpriced > 0)
            .count();
        let other = consumption.incomplete.len().saturating_sub(per_counter);
        if other > 0 {
            incomplete.push(format!("{other} consumption notes"));
        }
    }
    let read = view.read.as_ref();
    if read.is_none() {
        incomplete.push("the reader established no source frontier".to_string());
    }
    let (tokens, cost, cost_coverage) = match consumption {
        Some(consumption) => (
            consumption.tokens.as_ref(),
            consumption.cost.as_ref(),
            consumption.cost_coverage.as_ref(),
        ),
        None => (
            view.tokens.as_ref(),
            view.cost.as_ref(),
            view.cost_coverage.as_ref(),
        ),
    };
    let disjoint = match consumption {
        Some(_) => tokens
            .zip(agent_tapes_core::model::TokenConvention::of(harness))
            .map(|(tokens, convention)| {
                agent_tapes_core::model::DisjointTokens::from_native(tokens, convention, None)
            }),
        None => view.disjoint.clone(),
    };
    let events = consumption.map(|consumption| CostEvents {
        counted: consumption.events.counted as u64,
        active_branch: consumption.events.active_branch as u64,
        other_branches: consumption.events.other_branches as u64,
        repeats: consumption.events.repeats as u64,
        coverage: consumption
            .measurement_coverage
            .iter()
            .filter(|(name, _)| *name == "cost" || CATEGORIES.contains(&name.as_str()))
            .map(|(name, coverage)| {
                (
                    name.clone(),
                    CostEventCoverage {
                        measured: coverage.measured as u64,
                        missing: coverage.missing as u64,
                        unpriced: coverage.unpriced as u64,
                    },
                )
            })
            .collect(),
        identity: CostEventIdentity {
            provider_response: consumption.identity.provider_response as u64,
            recording_entry: consumption.identity.recording_entry as u64,
            unidentified: consumption.identity.unidentified as u64,
            account_namespace: consumption
                .identity
                .provider_account_namespace
                .as_deref()
                .map(|namespace| {
                    if namespace == "unestablished" {
                        "unestablished"
                    } else {
                        "established"
                    }
                    .to_string()
                }),
        },
    });
    let kind_supported =
        cost.map(|cost| serde_json::to_value(cost).is_ok_and(|record| record["kind"] == USD_KIND));
    let mut facts = CostFacts {
        reader: TAPES_READER.to_string(),
        calculation: COST_CALCULATION.to_string(),
        frontier: read.map_or(0, |read| read.source_length),
        digest: read
            .and_then(|read| read.source_prefix_digest.as_ref())
            .map(|digest| format!("sha256:{}", digest.value)),
        basis,
        coverage,
        source_started_at: view.session.started_at,
        source_start_uncertain: false,
        quantity_gaps: Some(BTreeMap::new()),
        lineage: None,
        counters: CostCounters {
            input: tokens.and_then(|tokens| tokens.input),
            output: tokens.and_then(|tokens| tokens.output),
            reasoning: tokens.and_then(|tokens| tokens.reasoning),
            cache_read: tokens.and_then(|tokens| tokens.cache_read),
            cache_write: tokens.and_then(|tokens| tokens.cache_write),
        },
        harness_estimate_usd: cost
            .filter(|_| kind_supported == Some(true))
            .map(|cost| cost.usd),
        incomplete,
        selection,
        disjoint: disjoint.as_ref().and_then(|disjoint| {
            Some(CostDisjoint {
                convention: CostConvention {
                    input: kebab(disjoint.convention.input)?,
                    output: kebab(disjoint.convention.output)?,
                },
                uncached_input: disjoint.uncached_input,
                cache_read: disjoint.cache_read,
                // The split sums to the writes, so an unrecorded bucket is empty.
                cache_write: disjoint.cache_write.map(|writes| CostCacheWrites {
                    ephemeral_5m: writes.ephemeral_5m.unwrap_or(0),
                    ephemeral_1h: writes.ephemeral_1h.unwrap_or(0),
                    ttl_unrecorded: writes.ttl_unrecorded.unwrap_or(0),
                }),
                output: disjoint.output,
                reasoning: disjoint.reasoning,
            })
        }),
        cost_coverage: cost_coverage.map(|coverage| CostPricing {
            complete: coverage.complete,
            zero_written_requests: coverage.zero_written_requests as u64,
            zero_written_observations: coverage.zero_written_observations as u64,
        }),
        events,
        residual: None,
    };
    let summed = facts.basis.as_deref() == Some("summed-requests");
    let mut gaps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for category in CATEGORIES {
        let reason = if counter(&facts.counters, category).is_none() {
            Some("recorded category is absent")
        } else if facts.basis.is_none() {
            Some("accounting basis is unknown")
        } else if let Some(events) = &facts.events {
            match events.coverage.get(category) {
                Some(coverage) if coverage.missing > 0 || coverage.unpriced > 0 => {
                    Some("some counted events did not record this category")
                }
                Some(_) => None,
                None => Some("per-event counter coverage is unknown"),
            }
        } else if summed {
            Some("per-event counter coverage is unknown")
        } else if harness != "codex" {
            Some("per-model counter coverage is unknown")
        } else {
            None
        };
        if let Some(reason) = reason {
            gaps.insert(category.to_string(), vec![reason.to_string()]);
        }
    }
    let zero_written = facts.cost_coverage.as_ref().is_some_and(|coverage| {
        coverage.zero_written_requests > 0 || coverage.zero_written_observations > 0
    });
    let mut usd = Vec::new();
    match facts.harness_estimate_usd {
        None if kind_supported == Some(false) => usd.push("recorded cost kind is unsupported"),
        None if zero_written => usd.push(ZERO_PRICE),
        None => usd.push("recorded category is absent"),
        Some(_) => {
            if facts.basis.is_none() {
                usd.push("accounting basis is unknown");
            }
            match (&facts.events, &facts.cost_coverage) {
                (Some(events), _) => match events.coverage.get("cost") {
                    Some(coverage) => {
                        if coverage.unpriced > 0 {
                            usd.push(ZERO_PRICE);
                        }
                        if coverage.missing > 0 {
                            usd.push("some counted events recorded no price");
                        }
                    }
                    None => usd.push("per-event price coverage is unknown"),
                },
                (None, Some(coverage)) if !coverage.complete => usd.push(if zero_written {
                    ZERO_PRICE
                } else {
                    "some counted events recorded no price"
                }),
                (None, None) if summed => usd.push("per-event price coverage is unknown"),
                _ => {}
            }
        }
    }
    if !usd.is_empty() {
        gaps.insert(
            "usd".to_string(),
            usd.into_iter().map(str::to_string).collect(),
        );
    }
    facts.quantity_gaps = Some(gaps);
    qualify_price(&mut facts);
    facts
}

fn unavailable_residual(reason: &str) -> CostResidual {
    CostResidual {
        unavailable: Some(reason.to_string()),
        rules: None,
        scope_established: false,
        identity_coverage: None,
        counters: Vec::new(),
        missing: Vec::new(),
        sources: Vec::new(),
    }
}

/// The recording family's reconciliation, as tapes answers it. A failed read
/// leaves only the residual unavailable; the session's measurement stands.
/// Recording locators, notes and errors are not kept.
fn residual(
    backend: &dyn agent_tapes_core::backend::Backend,
    session: &agent_tapes_core::model::Session,
    frontier: u64,
) -> CostResidual {
    let Ok(answer) = backend.usage_reconciliation(session) else {
        return unavailable_residual("reconciliation read failed");
    };
    let parent = answer
        .sources
        .iter()
        .find(|source| source.role == agent_tapes_core::reconcile::SourceRole::Parent);
    if parent.and_then(|source| source.length) != Some(frontier) {
        return unavailable_residual("reconciliation frontier differs from the measurement");
    }
    let mut residual = CostResidual {
        unavailable: None,
        rules: Some(answer.rules.to_string()),
        scope_established: answer.scope_established,
        identity_coverage: Some(answer.identified.identity_coverage.to_string()),
        counters: answer
            .counters
            .iter()
            .map(|row| {
                let difference = row.difference.map(i64::try_from);
                let mut reasons: Vec<String> =
                    row.reasons.iter().map(ToString::to_string).collect();
                if matches!(difference, Some(Err(_))) {
                    reasons.push("difference-out-of-range".to_string());
                }
                CostCounterResidual {
                    counter: row.counter.to_string(),
                    recorded: row.recorded,
                    identified: row.identified,
                    difference: difference.and_then(Result::ok),
                    status: kebab(row.status).unwrap_or_else(|| "withheld".to_string()),
                    reasons,
                }
            })
            .collect(),
        missing: answer.missing.iter().map(ToString::to_string).collect(),
        sources: answer
            .sources
            .iter()
            .map(|source| CostResidualSource {
                role: kebab(source.role).unwrap_or_default(),
                frontier: source.length,
                digest: source
                    .sha256
                    .as_ref()
                    .map(|digest| format!("sha256:{digest}")),
                complete: source.complete,
            })
            .collect(),
    };
    sanitize_residual(&mut residual);
    residual
}

/// A residual field arc cannot classify is withheld rather than kept.
fn sanitize_residual(residual: &mut CostResidual) {
    if !residual.portable() {
        *residual = unavailable_residual("retained reconciliation gap");
    }
}

/// A recorded total below the requests it should contain understates that
/// counter.
fn qualify_residual(facts: &mut CostFacts) {
    let conflicts: Vec<String> = facts
        .residual
        .iter()
        .flat_map(|residual| &residual.counters)
        .filter(|row| row.status == "conflict")
        .map(|row| row.counter.clone())
        .collect();
    for counter in conflicts {
        let gaps = facts
            .quantity_gaps
            .get_or_insert_with(BTreeMap::new)
            .entry(counter)
            .or_default();
        if !gaps.iter().any(|reason| reason == RESIDUAL_CONFLICT) {
            gaps.push(RESIDUAL_CONFLICT.to_string());
        }
    }
}

fn qualify_price(facts: &mut CostFacts) {
    let reason = facts.harness_estimate_usd.and_then(|amount| {
        if !amount.is_finite() || amount < 0.0 {
            Some("recorded price is invalid")
        } else if amount == 0.0
            && CATEGORIES
                .into_iter()
                .any(|category| counter(&facts.counters, category) != Some(0))
        {
            Some("recorded zero price accompanies nonzero or unknown usage")
        } else {
            None
        }
    });
    if let Some(reason) = reason {
        facts.harness_estimate_usd = None;
        facts
            .quantity_gaps
            .get_or_insert_with(|| {
                std::iter::once("usd")
                    .chain(CATEGORIES)
                    .map(|category| {
                        (
                            category.to_string(),
                            vec!["quantity coverage was not retained".to_string()],
                        )
                    })
                    .collect()
            })
            .entry("usd".to_string())
            .or_default()
            .push(reason.to_string());
    }
}

/// Sessions that acted on the selected changes without being bound to any
/// dispatch: a lead, a reviewer, anyone who wrote the change's events. Their
/// consumption is outside assignment coverage, and the view names them
/// without a figure.
fn outside_sessions(
    read: &mut CostRead<'_>,
    selection: &Selection,
    sessions: &[SessionRow],
) -> Vec<Value> {
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for change_id in &selection.ledger_changes {
        for event in read.events(change_id) {
            if let (Some(harness), Some(session)) = (&event.harness, &event.session) {
                *counts
                    .entry((harness.clone(), session.clone()))
                    .or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .filter(|((harness, session), _)| {
            !sessions
                .iter()
                .any(|row| &row.harness == harness && &row.session == session)
        })
        .map(|((harness, session), events)| {
            json!({"harness": harness, "session": session, "events": events})
        })
        .collect()
}

fn round_amount(value: f64) -> f64 {
    let scaled = value * 1e10;
    if (value != 0.0 && value.abs() < 1e-10) || !scaled.is_finite() {
        value
    } else {
        scaled.round() / 1e10
    }
}

/// Connected components include named relatives outside the selected figure.
fn lineage_groups(sessions: &[SessionRow]) -> Vec<BTreeSet<String>> {
    let mut groups: Vec<BTreeSet<String>> = Vec::new();
    for row in sessions {
        let mut group = BTreeSet::from([row.key()]);
        for origin in row
            .spans
            .iter()
            .filter_map(|span| span.forked_from.as_ref())
        {
            group.insert(format!("{}:{origin}", row.harness));
        }
        if let Some(lineage) = row
            .measured
            .facts
            .as_ref()
            .and_then(|facts| facts.lineage.as_ref())
        {
            for relative in [&lineage.parent, &lineage.forked_from]
                .into_iter()
                .flatten()
            {
                group.insert(format!("{}:{relative}", row.harness));
            }
            for child in &lineage.children {
                group.insert(format!("{}:{}", child.harness, child.session));
            }
        }
        let mut index = 0;
        while index < groups.len() {
            if !group.is_disjoint(&groups[index]) {
                group.extend(groups.remove(index));
                index = 0;
            } else {
                index += 1;
            }
        }
        groups.push(group);
    }
    groups
}

fn identity_groups(members: &[&SessionRow]) -> Vec<Vec<String>> {
    let keys: BTreeSet<String> = members.iter().map(|row| row.key()).collect();
    if members.len() > 1 && members.iter().any(|row| lineage_unknown(row)) {
        return vec![keys.into_iter().collect()];
    }
    members
        .iter()
        .map(|row| row.overlap.intersection(&keys).cloned().collect::<Vec<_>>())
        .filter(|group| group.len() > 1)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn lineage_unknown(row: &SessionRow) -> bool {
    row.measured
        .facts
        .as_ref()
        .and_then(|facts| facts.lineage.as_ref())
        .is_none_or(|lineage| !lineage.unknown.is_empty())
}

/// One reported quantity: the harness-written dollar figure, a native
/// counter in its harness's own convention, or a disjoint token category.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Figure {
    Usd,
    Native(&'static str),
    Disjoint(&'static str),
}

fn figures() -> impl Iterator<Item = Figure> {
    std::iter::once(Figure::Usd)
        .chain(CATEGORIES.into_iter().map(Figure::Native))
        .chain(DISJOINT.into_iter().map(Figure::Disjoint))
}

/// The accounting domain a figure adds within. Native counters and dollar
/// figures add within one harness and basis; disjoint categories add across
/// harnesses within one basis.
fn domain(row: &SessionRow, figure: Figure) -> (Option<&str>, Option<&str>) {
    let basis = row
        .measured
        .facts
        .as_ref()
        .and_then(|facts| facts.basis.as_deref());
    match figure {
        Figure::Disjoint(_) => (None, basis),
        _ => (Some(row.harness.as_str()), basis),
    }
}

/// Each harness's native counter convention, as tapes names it.
fn conventions(members: &[&SessionRow]) -> BTreeMap<String, Value> {
    members
        .iter()
        .filter_map(|row| {
            let convention = &row.measured.facts.as_ref()?.disjoint.as_ref()?.convention;
            Some((row.harness.clone(), json!(convention)))
        })
        .collect()
}

/// Each quantity retains its measurement and assignment gaps independently.
fn totals(members: &[&SessionRow], withheld: &[String], scope: &str) -> Value {
    let keys: Vec<String> = members.iter().map(|row| row.key()).collect();
    let mut quantities = Vec::new();
    let harnesses: BTreeSet<&str> = members.iter().map(|row| row.harness.as_str()).collect();
    for figure in figures() {
        let mut domains: BTreeMap<(Option<&str>, Option<&str>), Vec<&SessionRow>> = BTreeMap::new();
        for row in members {
            domains.entry(domain(row, figure)).or_default().push(row);
        }
        let bases: BTreeSet<Option<&str>> = domains.keys().map(|(_, basis)| *basis).collect();
        let common_basis = (bases.len() == 1)
            .then(|| bases.first().copied().flatten())
            .flatten();
        let mut combined = base(figure, None, common_basis, scope, members);
        match figure {
            Figure::Native(category) if harnesses.len() > 1 && withheld.is_empty() => {
                let named = harnesses.iter().copied().collect::<Vec<_>>().join(", ");
                let (status, reason) = if category == "input" {
                    ("refused", format!("a raw input sum across harnesses ({named}) is refused: their input conventions differ"))
                } else {
                    ("withheld", format!("native {category} counters follow each harness's own convention ({named}); the disjoint categories add across harnesses"))
                };
                combined["status"] = json!(status);
                combined["reasons"] = json!([reason]);
                quantities.push(combined);
            }
            _ => quantities.push(quantity(combined, aggregate(members, withheld, figure))),
        }
        if domains.len() > 1 {
            for ((harness, basis), partition) in &domains {
                let mut partition_base = base(figure, *harness, *basis, scope, partition);
                partition_base["partition"] = json!(true);
                quantities.push(quantity(
                    partition_base,
                    aggregate(partition, withheld, figure),
                ));
            }
        }
    }
    let groups = identity_groups(members);
    let unknown: BTreeMap<String, Vec<String>> = members
        .iter()
        .filter_map(|row| {
            let reasons = row
                .measured
                .facts
                .as_ref()
                .and_then(|facts| facts.lineage.as_ref())
                .map(|lineage| lineage.unknown.clone())
                .unwrap_or_else(|| vec!["lineage is unavailable".to_string()]);
            (!reasons.is_empty()).then(|| (row.key(), reasons))
        })
        .collect();
    json!({"sessions": keys, "quantities": quantities, "identity": {
        "status": if groups.is_empty() && unknown.is_empty() { "complete" } else { "incomplete" },
        "groups": groups, "unknown": unknown,
    }})
}

fn base(
    figure: Figure,
    harness: Option<&str>,
    basis: Option<&str>,
    scope: &str,
    members: &[&SessionRow],
) -> Value {
    let mut base = match figure {
        Figure::Usd => {
            json!({"kind": USD_KIND, "unit": "USD", "pricing_basis": "unknown",
                "source": "harness recording", "precision": "currency sums rounded to ten fractional places; smaller nonzero values preserved", "scope": scope})
        }
        Figure::Native(category) => json!({"kind": TOKEN_KIND, "unit": "tokens",
            "categories": "native", "category": category,
            "convention": "the harness's own counters; they add only within one harness",
            "conventions": conventions(members), "scope": scope}),
        Figure::Disjoint(category) => json!({"kind": TOKEN_KIND, "unit": "tokens",
            "categories": "disjoint", "category": category,
            "convention": "tapes' disjoint categories: uncached input, cache read, cache write by TTL, and output; reasoning is a subset of output; total sums the four",
            "conventions": conventions(members), "scope": scope}),
    };
    base["accounting_basis"] = json!(basis);
    if let Some(harness) = harness {
        base["harness"] = json!(harness);
    }
    base
}

fn counter(counters: &CostCounters, category: &str) -> Option<u64> {
    match category {
        "input" => counters.input,
        "output" => counters.output,
        "reasoning" => counters.reasoning,
        "cache_read" => counters.cache_read,
        _ => counters.cache_write,
    }
}

fn disjoint_amount(disjoint: &CostDisjoint, category: &str) -> Option<u64> {
    let writes = disjoint.cache_write.as_ref();
    match category {
        "uncached_input" => disjoint.uncached_input,
        "cache_read" => disjoint.cache_read,
        "cache_write" => writes.and_then(CostCacheWrites::total),
        "cache_write_5m" => writes.map(|writes| writes.ephemeral_5m),
        "cache_write_1h" => writes.map(|writes| writes.ephemeral_1h),
        "cache_write_ttl_unrecorded" => writes.map(|writes| writes.ttl_unrecorded),
        "output" => disjoint.output,
        "reasoning" => disjoint.reasoning,
        _ => [
            disjoint.uncached_input,
            disjoint.cache_read,
            writes.and_then(CostCacheWrites::total),
            disjoint.output,
        ]
        .into_iter()
        .try_fold(0u64, |sum, count| sum.checked_add(count?)),
    }
}

/// The native counters a disjoint category is derived from under the
/// harness's convention; their gaps are its gaps.
fn disjoint_sources(convention: &CostConvention, category: &str) -> Vec<&'static str> {
    let input: &[&'static str] = if convention.input == "includes-cache-read" {
        &["input", "cache_read", "cache_write"]
    } else {
        &["input"]
    };
    let output: &[&'static str] = if convention.output == "excludes-reasoning" {
        &["output", "reasoning"]
    } else {
        &["output"]
    };
    match category {
        "uncached_input" => input.to_vec(),
        "cache_read" => vec!["cache_read"],
        "output" => output.to_vec(),
        "reasoning" => vec!["reasoning", "output"],
        "total" => input
            .iter()
            .chain(&["cache_read", "cache_write"])
            .chain(output)
            .copied()
            .collect(),
        _ => vec!["cache_write"],
    }
}

/// Why a session's figure is not a complete measurement of one quantity.
fn figure_gaps(facts: &CostFacts, figure: Figure) -> Vec<String> {
    let Some(gaps) = &facts.quantity_gaps else {
        return vec!["quantity coverage was not retained".to_string()];
    };
    let of = |category: &str| gaps.get(category).into_iter().flatten().cloned();
    match figure {
        Figure::Usd => of("usd").collect(),
        Figure::Native(category) => of(category).collect(),
        Figure::Disjoint(category) => {
            let Some(disjoint) = &facts.disjoint else {
                return Vec::new();
            };
            let mut reasons: BTreeSet<String> = disjoint_sources(&disjoint.convention, category)
                .into_iter()
                .flat_map(of)
                .collect();
            if matches!(category, "cache_write_5m" | "cache_write_1h")
                && disjoint
                    .cache_write
                    .is_some_and(|writes| writes.ttl_unrecorded > 0)
            {
                reasons.insert("some cache writes recorded no TTL".to_string());
            }
            reasons.into_iter().collect()
        }
    }
}

#[derive(Clone, Copy)]
enum Amount {
    Tokens(u64),
    Usd(f64),
}

impl Amount {
    fn add(self, other: Self) -> Option<Self> {
        match (self, other) {
            (Self::Tokens(left), Self::Tokens(right)) => left.checked_add(right).map(Self::Tokens),
            (Self::Usd(left), Self::Usd(right)) => {
                let sum = left + right;
                sum.is_finite().then_some(Self::Usd(sum))
            }
            _ => None,
        }
    }

    fn max(self, other: Self) -> Self {
        match (self, other) {
            (Self::Tokens(left), Self::Tokens(right)) => Self::Tokens(left.max(right)),
            (Self::Usd(left), Self::Usd(right)) => Self::Usd(left.max(right)),
            _ => unreachable!("one quantity has one unit"),
        }
    }

    fn json(self) -> Value {
        match self {
            Self::Tokens(count) => json!(count),
            Self::Usd(amount) => json!(round_amount(amount)),
        }
    }

    fn bound_json(self, upper: bool) -> Value {
        match self {
            Self::Usd(value) if value.abs() >= 1e-10 && (value * 1e10).is_finite() => {
                let scaled = value * 1e10;
                json!(if upper {
                    scaled.ceil() / 1e10
                } else {
                    scaled.floor() / 1e10
                })
            }
            _ => self.json(),
        }
    }
}

fn sum_amounts(
    amounts: impl IntoIterator<Item = Amount>,
) -> std::result::Result<Option<Amount>, &'static str> {
    amounts
        .into_iter()
        .try_fold(None, |sum: Option<Amount>, amount| match sum {
            Some(sum) => sum
                .add(amount)
                .map(Some)
                .ok_or("quantity arithmetic overflowed"),
            None => Ok(Some(amount)),
        })
}

fn recorded_amount(facts: &CostFacts, figure: Figure) -> Option<Amount> {
    match figure {
        Figure::Usd => facts.harness_estimate_usd.map(Amount::Usd),
        Figure::Native(category) => counter(&facts.counters, category).map(Amount::Tokens),
        Figure::Disjoint(category) => facts
            .disjoint
            .as_ref()
            .and_then(|disjoint| disjoint_amount(disjoint, category))
            .map(Amount::Tokens),
    }
}

/// The status, amount, known subtotal, and reasons of one quantity.
struct Aggregate {
    status: &'static str,
    amount: Option<Amount>,
    known: Option<Amount>,
    reasons: Vec<String>,
    bounds: Option<(Amount, Amount, bool)>,
}

impl Aggregate {
    fn withheld(reasons: Vec<String>) -> Self {
        Self {
            status: "withheld",
            amount: None,
            known: None,
            reasons,
            bounds: None,
        }
    }
}

fn aggregate(members: &[&SessionRow], withheld: &[String], figure: Figure) -> Aggregate {
    if !withheld.is_empty() {
        return Aggregate::withheld(withheld.to_vec());
    }
    if members.is_empty() {
        return Aggregate {
            bounds: None,
            status: "absent",
            amount: None,
            known: None,
            reasons: vec!["no bound session is assigned here".to_string()],
        };
    }
    let domains: BTreeSet<(Option<&str>, Option<&str>)> =
        members.iter().map(|row| domain(row, figure)).collect();
    if domains.len() > 1 {
        let names = domains
            .iter()
            .map(|(harness, basis)| {
                let basis = basis.unwrap_or("unknown");
                harness.map_or(basis.to_string(), |harness| format!("{harness}/{basis}"))
            })
            .collect::<Vec<_>>();
        return Aggregate::withheld(vec![match figure {
            Figure::Disjoint(_) => format!("incompatible accounting bases: {}; quantities are partitioned by accounting basis", names.join(", ")),
            _ => format!("incompatible accounting domains: {}; quantities are partitioned by harness and accounting basis", names.join(", ")),
        }]);
    }
    let mut amounts = Vec::new();
    let mut missing = Vec::new();
    let mut gaps = BTreeSet::new();
    for row in members {
        match row.measured.facts.as_ref() {
            Some(facts) => {
                match recorded_amount(facts, figure) {
                    Some(amount) => amounts.push(amount),
                    None => missing.push(row.key()),
                }
                for reason in &facts.incomplete {
                    gaps.insert(format!("{}: {reason}", row.key()));
                }
                for reason in figure_gaps(facts, figure) {
                    gaps.insert(format!("{}: {reason}", row.key()));
                }
                if facts.basis.is_none() {
                    gaps.insert(format!("{}: accounting basis is unknown", row.key()));
                }
            }
            None => missing.push(row.key()),
        }
    }
    let known = match sum_amounts(amounts) {
        Ok(known) => known,
        Err(reason) => return Aggregate::withheld(vec![reason.to_string()]),
    };
    let groups = identity_groups(members);
    if !groups.is_empty() {
        let mut lower_terms = Vec::new();
        let mut grouped = BTreeSet::new();
        for group in &groups {
            if let Some(largest) = members
                .iter()
                .filter(|row| group.contains(&row.key()))
                .filter_map(|row| {
                    row.measured
                        .facts
                        .as_ref()
                        .and_then(|facts| recorded_amount(facts, figure))
                })
                .reduce(Amount::max)
            {
                lower_terms.push(largest);
            }
            grouped.extend(group.iter().cloned());
        }
        lower_terms.extend(
            members
                .iter()
                .filter(|row| !grouped.contains(&row.key()))
                .filter_map(|row| {
                    row.measured
                        .facts
                        .as_ref()
                        .and_then(|facts| recorded_amount(facts, figure))
                }),
        );
        let lower = match sum_amounts(lower_terms) {
            Ok(lower) => lower,
            Err(reason) => return Aggregate::withheld(vec![reason.to_string()]),
        };
        let mut reasons = vec![format!(
            "incomplete identity: sessions may overlap in groups {}",
            serde_json::to_string(&groups).unwrap()
        )];
        reasons.extend(gaps.iter().cloned());
        if !missing.is_empty() {
            reasons.push(format!("no recorded amount for {}", missing.join(", ")));
        }
        return Aggregate {
            status: "withheld",
            amount: None,
            known: None,
            bounds: lower
                .zip(known)
                .map(|(lower, upper)| (lower, upper, !missing.is_empty() || !gaps.is_empty())),
            reasons,
        };
    }
    if !missing.is_empty() {
        let mut reasons = vec![format!("no recorded amount for {}", missing.join(", "))];
        reasons.extend(gaps);
        return Aggregate {
            bounds: None,
            status: "absent",
            amount: None,
            known,
            reasons,
        };
    }
    if !gaps.is_empty() {
        return Aggregate {
            bounds: None,
            status: "incomplete",
            amount: None,
            known,
            reasons: gaps.into_iter().collect(),
        };
    }
    Aggregate {
        bounds: None,
        status: "complete",
        amount: known,
        known: None,
        reasons: Vec::new(),
    }
}

fn quantity(mut base: Value, aggregate: Aggregate) -> Value {
    if let Some((lower, upper, known_only)) = aggregate.bounds {
        let field = if known_only { "known_bound" } else { "bound" };
        base[field] = json!({"scope": if known_only { "known recorded amounts" } else { "whole figure" },
            "at_least": lower.bound_json(false), "at_most": upper.bound_json(true),
            "basis": "sum of disjoint groups; each overlapping group is at least its largest member and at most its sum"});
    }
    base["status"] = json!(aggregate.status);
    if let Some(amount) = aggregate.amount {
        base["amount"] = amount.json();
    }
    if let Some(known) = aggregate.known {
        base["known_subtotal"] = known.json();
    }
    base["reasons"] = json!(aggregate.reasons);
    base
}

fn measurement_reasons(facts: &CostFacts) -> Vec<String> {
    let mut reasons: BTreeSet<String> = facts.incomplete.iter().cloned().collect();
    match &facts.quantity_gaps {
        Some(gaps) => reasons.extend(gaps.values().flatten().cloned()),
        None => {
            reasons.insert("quantity coverage was not retained".to_string());
        }
    }
    reasons.into_iter().collect()
}

fn render_report(
    selection: &Selection,
    runs: &Runs,
    sessions: &[SessionRow],
    lead: &[Value],
) -> Value {
    let unbound_in = |key: &WorkKey| -> Vec<String> {
        runs.get(key)
            .into_iter()
            .flatten()
            .filter(|run| run.candidates.is_empty())
            .map(|run| run.dispatch_event_id.clone())
            .collect()
    };
    let withheld_for = |keys: &[&WorkKey], scope: &str| -> Vec<String> {
        let mut reasons: Vec<String> = keys
            .iter()
            .flat_map(|key| unbound_in(key))
            .map(|id| format!("dispatch {id} is unbound, so its consumption is unassigned"))
            .collect();
        for key in keys {
            for run in runs
                .get(key)
                .into_iter()
                .flatten()
                .filter(|run| run.conflicting)
            {
                reasons.push(format!(
                    "dispatch {} has a binding conflict, so its consumption is unassigned",
                    run.dispatch_event_id
                ));
            }
        }
        for row in sessions
            .iter()
            .filter(|row| row.membership.iter().any(|work| keys.contains(&work)))
        {
            let first = row.spans.iter().map(|span| span.dispatched_at).min();
            match row.measured.facts.as_ref() {
                Some(facts)
                    if !facts.source_start_uncertain && facts.source_started_at.is_some() =>
                {
                    if facts.source_started_at < first {
                        reasons.push(format!("session {} has pre-dispatch consumption: its recording begins before its first dispatch", row.key()));
                    }
                }
                _ => reasons.push(format!(
                    "session {} has an unknown source start, so its consumption is unassigned",
                    row.key()
                )),
            }
        }
        for gap in &selection.gaps {
            if gap["work"]
                .as_array()
                .is_some_and(|work| keys.iter().any(|key| work.contains(&json!(key.label()))))
                && gap["scopes"]
                    .as_array()
                    .is_none_or(|scopes| scopes.contains(&json!(scope)))
            {
                reasons.push(format!(
                    "{}: {}",
                    gap["record"].as_str().unwrap_or("record"),
                    gap["reason"].as_str().unwrap_or("membership unavailable")
                ));
            }
        }
        reasons
    };
    let within = |keys: &[&WorkKey]| -> (Vec<&SessionRow>, Vec<String>) {
        let mut inside = Vec::new();
        let mut shared = Vec::new();
        for row in sessions {
            if row.membership.iter().all(|work| keys.contains(&work)) {
                inside.push(row);
            } else if row.membership.iter().any(|work| keys.contains(&work)) {
                shared.push(row.key());
            }
        }
        (inside, shared)
    };
    let span_count = |key: &str| {
        sessions
            .iter()
            .find(|row| row.key() == key)
            .map_or(0, |row| row.spans.len())
    };

    let mut work = Vec::new();
    for item in &selection.items {
        let (direct, shared) = within(&[&item.key]);
        let item_runs: Vec<Value> = runs
            .get(&item.key)
            .into_iter()
            .flatten()
            .map(|run| {
                let session = (!run.conflicting).then(|| run.candidates.first()).flatten();
                let per_run = match &session {
                    _ if run.invalid => "invalid-binding",
                    _ if run.conflicting => "conflicting-bindings",
                    None => "unbound",
                    Some(key) if span_count(key) > 1 => "shared-among-spans",
                    Some(_) => "whole-session",
                };
                json!({"dispatch_event_id": run.dispatch_event_id, "round": run.round,
                    "session": session, "binding_candidates": run.candidates, "per_run": per_run})
            })
            .collect();
        let mut entry = json!({
            "key": item.key.label(),
            "runs": item_runs,
            "unbound": unbound_in(&item.key),
            "direct": totals(&direct, &withheld_for(&[&item.key], "direct"), "direct"),
            "shared": shared,
        });
        match &item.key {
            WorkKey::Change(id) => {
                entry["kind"] = json!("change");
                entry["id"] = json!(id);
            }
            WorkKey::Journal((_, file)) => {
                entry["kind"] = json!("journal");
                entry["file"] = json!(item.lineage.last().unwrap_or(file));
                entry["lineage"] = json!(item.lineage);
            }
            WorkKey::Other(_) => {}
        }
        if let Some(source) = &item.source {
            let keys = [&item.key, source];
            let (inclusive, beyond) = within(&keys);
            let mut totals = totals(&inclusive, &withheld_for(&keys, "inclusive"), "inclusive");
            totals["members"] = json!([item.key.label(), source.label()]);
            totals["shared"] = json!(beyond);
            entry["inclusive"] = totals;
        }
        work.push(entry);
    }

    let all: Vec<&WorkKey> = selection.items.iter().map(|item| &item.key).collect();
    let (union_members, union_shared) = within(&all);
    let mut union = totals(&union_members, &withheld_for(&all, "union"), "union");
    union["members"] = json!(all.iter().map(|key| key.label()).collect::<Vec<_>>());
    union["shared"] = json!(union_shared);

    let session_rows: Vec<Value> = sessions.iter().map(session_json).collect();
    let unmeasured: BTreeMap<String, Vec<String>> = sessions
        .iter()
        .filter_map(|row| match (&row.measured.facts, &row.measured.source) {
            (None, Source::Unavailable(reason)) => Some((row.key(), vec![reason.clone()])),
            (Some(facts), _) if !measurement_reasons(facts).is_empty() => {
                Some((row.key(), measurement_reasons(facts)))
            }
            _ => None,
        })
        .collect();
    let unbound: Vec<String> = all.iter().flat_map(|key| unbound_in(key)).collect();
    let mut assignment_gaps = withheld_for(&all, "union");
    assignment_gaps.extend(selection.gaps.iter().map(|gap| {
        format!(
            "{}: {}",
            gap["record"].as_str().unwrap_or("record"),
            gap["reason"].as_str().unwrap_or("membership unavailable")
        )
    }));
    let unavailable_sources: BTreeMap<String, &str> = sessions
        .iter()
        .filter_map(|row| match &row.measured.source {
            Source::Fresh => None,
            Source::Retained(_, reason) | Source::Unavailable(reason) => {
                Some((row.key(), reason.as_str()))
            }
        })
        .collect();
    let mut identities = vec![json!({"scope": "union", "identity": union["identity"]})];
    for item in &work {
        for scope in ["direct", "inclusive"] {
            if item[scope].is_object() {
                identities.push(json!({"work": item["key"], "scope": scope, "identity": item[scope]["identity"]}));
            }
        }
    }
    let identity_complete = identities
        .iter()
        .all(|figure| figure["identity"]["status"] == "complete");
    json!({
        "schema": COST_SCHEMA,
        "target": selection.target,
        "work": work,
        "union": union,
        "sessions": session_rows,
        "coverage": {
            "selection": {"status": if selection.gaps.is_empty() { "complete" } else { "incomplete" }, "records": selection.gaps},
            "ledger": {"status": if selection.ledger_gaps.is_empty() { "complete" } else { "incomplete" }, "records": selection.ledger_gaps},
            "measurement": {
                "status": if unmeasured.is_empty() && unavailable_sources.is_empty() { "complete" } else { "incomplete" },
                "sessions": unmeasured,
                "current_sources": {"status": if unavailable_sources.is_empty() { "complete" } else { "incomplete" }, "unavailable": unavailable_sources},
                "residual": residual_coverage(sessions),
            },
            "assignment": {
                "status": if assignment_gaps.is_empty() { "complete" } else { "incomplete" },
                "unbound": unbound,
                "reasons": assignment_gaps,
                "outside": {
                    "statement": "consumption no dispatch names, such as a lead session, is outside assignment coverage: it is not measured here and is never zero",
                    "sessions": lead,
                },
            },
            "identity": {
                "status": if identity_complete { "complete" } else { "incomplete" },
                "key": "executor harness and native session",
                "figures": identities,
            },
        },
    })
}

/// Where each Claude session's recorded total stands against the requests its
/// recording family identifies. A counter that is not `reconciled` keeps its
/// tapes status; a withheld one is never read as zero.
fn residual_coverage(sessions: &[SessionRow]) -> Value {
    let mut rows = BTreeMap::new();
    let mut complete = true;
    for row in sessions {
        let Some(residual) = row
            .measured
            .facts
            .as_ref()
            .and_then(|facts| facts.residual.as_ref())
        else {
            continue;
        };
        let counters: BTreeMap<&str, &str> = residual
            .counters
            .iter()
            .map(|counter| (counter.counter.as_str(), counter.status.as_str()))
            .collect();
        let reconciled = residual.unavailable.is_none()
            && !counters.is_empty()
            && counters.values().all(|status| *status == "reconciled");
        complete &= reconciled;
        rows.insert(
            row.key(),
            json!({"status": if reconciled { "reconciled" } else if residual.unavailable.is_some() { "unavailable" } else { "unreconciled" },
                "unavailable": residual.unavailable, "counters": counters}),
        );
    }
    json!({
        "status": if rows.is_empty() { "not-applicable" } else if complete { "complete" } else { "incomplete" },
        "statement": "a recorded total is reconciled per counter with the requests its recording family identifies; the difference is signed and never clamped, and a withheld counter has no residual",
        "sessions": rows,
    })
}

fn session_json(row: &SessionRow) -> Value {
    let measured = &row.measured;
    let mut measurement = json!({});
    match &measured.source {
        Source::Fresh => measurement["source"] = json!("fresh"),
        Source::Retained(event_id, reason) => {
            measurement["source"] = json!("retained");
            measurement["retained_event_id"] = json!(event_id);
            measurement["fresh_read"] = json!({"status": "unavailable", "reason": reason});
        }
        Source::Unavailable(reason) => {
            measurement["source"] = json!("unavailable");
            measurement["reasons"] = json!([reason]);
        }
    }
    match &measured.facts {
        Some(facts) => {
            let reasons = measurement_reasons(facts);
            measurement["status"] = json!(if reasons.is_empty() {
                "complete"
            } else {
                "incomplete"
            });
            measurement["reasons"] = json!(reasons);
            measurement["quantity_gaps"] = json!(facts.quantity_gaps);
            measurement["reader"] = json!(facts.reader);
            measurement["calculation"] = json!(facts.calculation);
            measurement["frontier"] = json!(facts.frontier);
            measurement["digest"] = json!(facts.digest);
            measurement["lineage"] = json!(facts.lineage);
            measurement["basis"] = json!(facts.basis);
            measurement["coverage"] = json!(facts.coverage);
            measurement["source_started_at"] = json!(facts.source_started_at);
            measurement["source_start_uncertain"] = json!(facts.source_start_uncertain);
            measurement["selection"] = json!(facts.selection);
            measurement["disjoint"] = json!(facts.disjoint);
            measurement["cost_coverage"] = json!(facts.cost_coverage);
            measurement["events"] = json!(facts.events);
            measurement["residual"] = json!(facts.residual);
        }
        None => measurement["status"] = json!("unmeasured"),
    }
    if let Some(conflict) = &measured.conflict {
        measurement["conflict"] = conflict.clone();
    }
    let scope = format!("session {}", row.key());
    json!({
        "key": row.key(),
        "harness": row.harness,
        "session": row.session,
        "spans": row.spans.iter().map(|span| json!({
            "dispatch_event_id": span.dispatch_event_id,
            "dispatched_at": span.dispatched_at,
            "forked_from": span.forked_from,
            "round": span.round,
            "work": span.work.label(),
        })).collect::<Vec<_>>(),
        "membership": row.membership.iter().map(WorkKey::label).collect::<Vec<_>>(),
        "per_run": if row.spans.len() > 1 { "shared-among-spans" } else { "whole-session" },
        "measurement": measurement,
        "totals": totals(&[row], &[], &scope),
    })
}

fn describe_quantity(quantity: &Value) -> String {
    let unit = quantity["unit"].as_str().unwrap_or_default();
    let label = match quantity["kind"].as_str() {
        Some(USD_KIND) => "harness-calculated estimate".to_string(),
        _ => match quantity["categories"].as_str() {
            Some("disjoint") => format!(
                "disjoint {}",
                quantity["category"].as_str().unwrap_or_default()
            ),
            _ => quantity["category"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        },
    };
    let label = match quantity["harness"].as_str() {
        Some(harness) => format!("{label} ({harness})"),
        None => label,
    };
    let label = match quantity["accounting_basis"].as_str() {
        Some(basis) => format!("{label} [{basis}]"),
        None => format!("{label} [accounting basis unknown]"),
    };
    let status = quantity["status"].as_str().unwrap_or_default();
    match (&quantity["amount"], &quantity["known_subtotal"]) {
        (Value::Number(amount), _) => format!("{label} {amount} {unit}"),
        (_, Value::Number(known)) => {
            format!("{label} {status} (known subtotal {known} {unit})")
        }
        _ if quantity["bound"].is_object() || quantity["known_bound"].is_object() => {
            let (bound, qualifier) = if quantity["bound"].is_object() {
                (&quantity["bound"], "")
            } else {
                (&quantity["known_bound"], "known recorded amounts: ")
            };
            format!(
                "{label} {status} ({qualifier}at least {} {unit}, at most {} {unit})",
                bound["at_least"], bound["at_most"]
            )
        }
        _ => format!("{label} {status}"),
    }
}

fn print_totals(indent: &str, heading: &str, totals: &Value) {
    let quantities = totals["quantities"].as_array().cloned().unwrap_or_default();
    let parts: Vec<String> = quantities.iter().map(describe_quantity).collect();
    println!("{indent}{heading}: {}", parts.join("; "));
    println!(
        "{indent}  identity: {}",
        totals["identity"]["status"].as_str().unwrap_or_default()
    );
    let reasons: BTreeSet<String> = quantities
        .iter()
        .flat_map(|quantity| {
            quantity["reasons"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|reason| reason.as_str().map(str::to_string))
        })
        .collect();
    for reason in reasons {
        println!("{indent}  {}", crate::render::one_line(&reason));
    }
    if let Some(unknown) = totals["identity"]["unknown"].as_object() {
        for (session, reasons) in unknown {
            for reason in reasons.as_array().into_iter().flatten() {
                println!(
                    "{indent}  {session}: {}",
                    reason.as_str().unwrap_or_default()
                );
            }
        }
    }
}

fn print_report(report: &Value) {
    let target = &report["target"];
    match target["kind"].as_str() {
        Some("journal") => {
            let lineage: Vec<&str> = target["lineage"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            println!(
                "cost: journal {}  lineage: {}",
                target["file"].as_str().unwrap_or_default(),
                lineage.join(" -> ")
            );
        }
        _ => println!("cost: change {}", target["id"].as_str().unwrap_or_default()),
    }
    for item in report["work"].as_array().into_iter().flatten() {
        let runs = item["runs"].as_array().map_or(0, Vec::len);
        println!("{}  runs={runs}", item["key"].as_str().unwrap_or_default());
        for unbound in item["unbound"].as_array().into_iter().flatten() {
            println!(
                "  unbound dispatch {}",
                unbound.as_str().unwrap_or_default()
            );
        }
        print_totals("  ", "direct", &item["direct"]);
        let shared: Vec<&str> = item["shared"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !shared.is_empty() {
            println!("  shared: {}", shared.join(", "));
        }
        if item["inclusive"].is_object() {
            print_totals("  ", "inclusive", &item["inclusive"]);
        }
    }
    println!("union");
    print_totals("  ", "total", &report["union"]);
    println!("sessions");
    for session in report["sessions"].as_array().into_iter().flatten() {
        let measurement = &session["measurement"];
        println!(
            "  {}  {} {}  frontier={}  spans={}  {}",
            session["key"].as_str().unwrap_or_default(),
            measurement["source"].as_str().unwrap_or_default(),
            measurement["status"].as_str().unwrap_or_default(),
            measurement["frontier"],
            session["spans"].as_array().map_or(0, Vec::len),
            session["per_run"].as_str().unwrap_or_default(),
        );
        if measurement["conflict"].is_object() {
            println!(
                "    conflict: a fresh read disagrees with retained facts at the same frontier"
            );
        }
        if measurement["fresh_read"]["status"] == "unavailable" {
            println!(
                "    fresh read unavailable: {}",
                measurement["fresh_read"]["reason"]
                    .as_str()
                    .unwrap_or_default()
            );
        }
        if let Some(selection) = measurement["selection"].as_str() {
            println!(
                "    counts {selection}: {} events, {} beyond the active branch",
                measurement["events"]["counted"], measurement["events"]["other_branches"]
            );
        }
        for reason in measurement["reasons"].as_array().into_iter().flatten() {
            println!("    {}", reason.as_str().unwrap_or_default());
        }
        print_totals("    ", "measured", &session["totals"]);
    }
    let coverage = &report["coverage"];
    println!("coverage");
    print_read_gaps(
        report["coverage"]["selection"]["records"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default(),
        report["coverage"]["ledger"]["records"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default(),
    );
    println!(
        "  measurement: {}",
        coverage["measurement"]["status"]
            .as_str()
            .unwrap_or_default()
    );
    let residual = &coverage["measurement"]["residual"];
    println!(
        "  residual: {}",
        residual["status"].as_str().unwrap_or_default()
    );
    for (session, row) in residual["sessions"].as_object().into_iter().flatten() {
        let counters: Vec<String> = row["counters"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, status)| *status != "reconciled")
            .map(|(counter, status)| format!("{counter} {}", status.as_str().unwrap_or_default()))
            .collect();
        println!(
            "    {session}: {}{}{}",
            row["status"].as_str().unwrap_or_default(),
            row["unavailable"]
                .as_str()
                .map_or(String::new(), |reason| format!(" ({reason})")),
            if counters.is_empty() {
                String::new()
            } else {
                format!(": {}", counters.join(", "))
            }
        );
    }
    println!(
        "  assignment: {}",
        coverage["assignment"]["status"]
            .as_str()
            .unwrap_or_default()
    );
    for reason in coverage["assignment"]["reasons"]
        .as_array()
        .into_iter()
        .flatten()
    {
        println!("    {}", reason.as_str().unwrap_or_default());
    }
    println!(
        "  identity: {}",
        coverage["identity"]["status"].as_str().unwrap_or_default()
    );
    let outside = &coverage["assignment"]["outside"];
    println!("  {}", outside["statement"].as_str().unwrap_or_default());
    for session in outside["sessions"].as_array().into_iter().flatten() {
        println!(
            "    {}:{} ({} events)",
            session["harness"].as_str().unwrap_or_default(),
            session["session"].as_str().unwrap_or_default(),
            session["events"]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reader a measurement names is the release and revision Cargo.lock
    /// resolved for the library this build links.
    #[test]
    fn the_recorded_reader_names_the_resolved_tapes_revision() {
        let lock = include_str!("../../Cargo.lock");
        let package = lock
            .split("[[package]]")
            .find(|package| package.contains("\nname = \"agent-tapes-core\"\n"))
            .expect("Cargo.lock resolves agent-tapes-core");
        let field = |name: &str| {
            package
                .split(&format!("\n{name} = \""))
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_else(|| panic!("the package names a {name}: {package}"))
        };
        let revision = field("source")
            .rsplit('#')
            .next()
            .expect("a git source names its commit");
        assert_eq!(
            TAPES_READER,
            format!("agent-tapes-core {} {revision}", field("version"))
        );
    }
}
