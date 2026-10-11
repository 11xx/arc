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

use super::run::{rounds, RunSubject};
use super::*;
use crate::model::{
    plain_session_id, portable_cost_metadata, portable_cost_reason, portable_lineage_reason,
    portable_quantity_reason, CostCounters, CostFacts, CostLineage, CostRelative, Event, Payload,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const COST_SCHEMA: &str = "arc-cost/1";

/// The reader a fresh measurement names: tapes' core library at the release
/// and revision Cargo.toml pins. A test holds the two together.
pub const TAPES_READER: &str =
    "agent-tapes-core 2026.9.29 3f6f757d40b7ef8e15fe7ebcf81ba59edb4e6ba7";

/// How arc normalizes a reader's usage answer into retained facts. It moves
/// when that normalization does, so facts from an older one stay identifiable.
pub const COST_CALCULATION: &str = "usage-normalization 2";

const USD_KIND: &str = "harness-calculated-estimate";
const TOKEN_KIND: &str = "resource-quantity";
const CATEGORIES: [&str; 5] = ["input", "output", "reasoning", "cache_read", "cache_write"];

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
    })
}

/// What the view reports on and the work it selects.
struct Selection {
    target: Value,
    items: Vec<WorkItem>,
    /// Ledger changes among the items, whose own events name the sessions
    /// that acted on them.
    ledger_changes: Vec<String>,
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
    let events = store
        .load_repository_events()
        .map_err(|_| anyhow::anyhow!("run metadata read failed"))?;
    super::run::ensure_portable_bindings(&events)?;
    let selection = select(ctx, &store, target, &events)?;
    let (runs, sessions) = assign(ctx, &selection, &events)?;
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
    let lead = outside_sessions(&store, &selection, &sessions)?;
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
    let events = store
        .load_repository_events()
        .map_err(|_| anyhow::anyhow!("run metadata read failed"))?;
    super::run::ensure_portable_bindings(&events)?;
    let selection = select(ctx, &store, target, &events)?;
    let (_, sessions) = assign(ctx, &selection, &events)?;
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
    let events = store
        .load_repository_events()
        .map_err(|_| anyhow::anyhow!("run metadata read failed"))?;
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

fn looks_like_journal(target: &str) -> bool {
    target.ends_with(".md") || crate::journal::split_qualified(target).is_some()
}

/// The work a target selects. A change selects itself and the journal
/// artifact it was begun from; an artifact selects its transition lineage and
/// every change begun from any name in it. A citation selects nothing.
fn select(ctx: &Ctx, store: &Store, target: &str, events: &[Event]) -> Result<Selection> {
    let read = store.read()?;
    if looks_like_journal(target) {
        crate::journal::artifact_digest(ctx, target)
            .map_err(|_| anyhow::anyhow!("journal source read failed"))?;
        let item = journal_item(ctx, target)?;
        let journal = item.key.clone();
        let lineage = item.lineage.clone();
        let mut items = vec![item];
        let mut ledger_changes = Vec::new();
        for change_id in read.change_ids()? {
            let state = read.project(&read.events(&change_id)?)?;
            if state
                .journal_ref
                .as_ref()
                .map(|reference| artifact_identity(ctx, reference))
                .transpose()?
                .as_ref()
                .is_some_and(|identity| items[0].journal_members.contains(identity))
            {
                items.push(WorkItem {
                    key: WorkKey::Change(change_id.clone()),
                    lineage: Vec::new(),
                    journal_members: BTreeSet::new(),
                    source: Some(journal.clone()),
                });
                ledger_changes.push(change_id);
            }
        }
        return Ok(Selection {
            target: json!({"kind": "journal", "file": target, "lineage": lineage}),
            items,
            ledger_changes,
        });
    }
    let (change_id, journal_ref) = match store.resolve_change(target) {
        Ok(change_id) => {
            let state = read.project(&read.events(&change_id)?)?;
            (change_id, state.journal_ref)
        }
        // A dispatch may name a change this ledger does not hold; the runs
        // that name it exactly are still that work.
        Err(error) => {
            let named = rounds(events).iter().any(
                |round| matches!(&round.subject, Some(RunSubject::Change { id }) if id == target),
            );
            if !named {
                return Err(error);
            }
            (target.to_string(), None)
        }
    };
    let ledger_changes = store
        .resolve_change(&change_id)
        .ok()
        .into_iter()
        .collect::<Vec<_>>();
    let mut items = vec![WorkItem {
        key: WorkKey::Change(change_id.clone()),
        lineage: Vec::new(),
        journal_members: BTreeSet::new(),
        source: None,
    }];
    if let Some(reference) = journal_ref {
        let item = journal_item(ctx, &reference)?;
        items[0].source = Some(item.key.clone());
        items.push(item);
    }
    Ok(Selection {
        target: json!({"kind": "change", "id": change_id}),
        items,
        ledger_changes,
    })
}

struct AssignedRun {
    dispatch_event_id: String,
    round: Option<usize>,
    candidates: Vec<String>,
    conflicting: bool,
}

type Runs = BTreeMap<WorkKey, Vec<AssignedRun>>;
type Sessions = BTreeMap<(String, String), (Vec<Span>, BTreeSet<WorkKey>)>;

/// The selected runs per work item, and every bound session with at least
/// one span in the selection, with all of its spans across the repository:
/// a span on unselected work makes the session shared beyond the view. A run
/// is a change's when its dispatch names that change's exact id, so what a
/// run is assigned to never moves as other changes are opened.
fn assign(ctx: &Ctx, selection: &Selection, events: &[Event]) -> Result<(Runs, Sessions)> {
    let work_of = |subject: &Option<RunSubject>| -> Result<WorkKey> {
        Ok(match subject {
            Some(RunSubject::Change { id }) => WorkKey::Change(id.clone()),
            Some(RunSubject::Journal { file }) => {
                let identity = if selection
                    .items
                    .iter()
                    .any(|item| !item.journal_members.is_empty())
                {
                    Some(artifact_identity(ctx, file)?)
                } else {
                    None
                };
                selection
                    .items
                    .iter()
                    .find(|item| {
                        identity
                            .as_ref()
                            .is_some_and(|id| item.journal_members.contains(id))
                    })
                    .map(|item| item.key.clone())
                    .unwrap_or_else(|| WorkKey::Other(format!("journal {file}")))
            }
            Some(subject) => WorkKey::Other(subject.label()),
            None => WorkKey::Other("unattributed".to_string()),
        })
    };
    let selected: BTreeSet<&WorkKey> = selection.items.iter().map(|item| &item.key).collect();
    let mut runs: Runs = BTreeMap::new();
    let mut sessions: Sessions = BTreeMap::new();
    for round in rounds(events) {
        let work = work_of(&round.subject)?;
        let bindings: Vec<_> = round
            .binding
            .iter()
            .flat_map(|binding| std::iter::once(binding).chain(&binding.conflicts))
            .collect();
        let candidates: BTreeSet<String> = bindings
            .iter()
            .map(|binding| format!("{}:{}", binding.executor_harness, binding.executor_session))
            .collect();
        if selected.contains(&work) {
            runs.entry(work.clone()).or_default().push(AssignedRun {
                dispatch_event_id: round.dispatch.event_id.clone(),
                round: round.round,
                candidates: candidates.into_iter().collect(),
                conflicting: bindings.len() > 1,
            });
        }
        for binding in bindings {
            let key = (
                binding.executor_harness.clone(),
                binding.executor_session.clone(),
            );
            let (spans, membership) = sessions.entry(key).or_default();
            membership.insert(work.clone());
            spans.push(Span {
                dispatch_event_id: round.dispatch.event_id.clone(),
                dispatched_at: round.dispatch.created_at,
                forked_from: binding.forked_from.clone(),
                round: round.round,
                work: work.clone(),
            });
        }
    }
    sessions.retain(|_, (_, membership)| membership.iter().any(|work| selected.contains(work)));
    Ok((runs, sessions))
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
                .filter(|(_, held)| held.frontier == fresh.frontier && !same_measurement(held, &fresh))
                .map(|(event, held)| {
                    json!({
                        "retained_event_id": event.event_id,
                        "frontier": held.frontier,
                        "retained": {"digest": held.digest, "counters": held.counters, "harness_estimate_usd": held.harness_estimate_usd, "lineage": held.lineage, "accounting_basis": held.basis, "coverage": held.coverage, "source_started_at": held.source_started_at, "source_start_uncertain": held.source_start_uncertain, "quantity_gaps": held.quantity_gaps, "incomplete": held.incomplete},
                        "fresh": {"digest": fresh.digest, "counters": fresh.counters, "harness_estimate_usd": fresh.harness_estimate_usd, "lineage": fresh.lineage, "accounting_basis": fresh.basis, "coverage": fresh.coverage, "source_started_at": fresh.source_started_at, "source_start_uncertain": fresh.source_start_uncertain, "quantity_gaps": fresh.quantity_gaps, "incomplete": fresh.incomplete},
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

fn same_measurement(left: &CostFacts, right: &CostFacts) -> bool {
    left.digest == right.digest
        && left.counters == right.counters
        && left.harness_estimate_usd == right.harness_estimate_usd
        && left.lineage == right.lineage
        && left.basis == right.basis
        && left.coverage == right.coverage
        && left.source_started_at == right.source_started_at
        && left.source_start_uncertain == right.source_start_uncertain
        && left.quantity_gaps == right.quantity_gaps
        && left.incomplete == right.incomplete
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
    read.notes
        .extend(resolved.diagnostics.latest_warnings(&resolved.session));
    let whole = backend
        .stream_session(&resolved.session, &read)
        .map_err(|_| "usage read failed".to_string())?;
    let view = agent_tapes_core::usage::streamed(&whole, tally, &read);
    let mut facts = normalize(&view, harness);
    facts.source_started_at = earliest.into_iter().chain(whole.started_at).min();
    facts.source_start_uncertain = whole.start_uncertain
        || view.truncated
        || !view.truncation.is_empty()
        || view.read.as_ref().is_none_or(|read| !read.gaps.is_empty());
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
/// session is named rather than corrected.
fn normalize(view: &agent_tapes_core::usage::UsageView, harness: &str) -> CostFacts {
    let mut incomplete = Vec::new();
    let (basis, coverage) = match &view.accounting {
        Some(accounting) => (kebab(accounting.basis), kebab(accounting.coverage)),
        None => {
            incomplete.push("the source recorded no usage counters".to_string());
            (None, None)
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
    if !view.notes.is_empty() {
        incomplete.push(format!("{} usage notes", view.notes.len()));
    }
    if let Some(read) = view.read.as_ref().filter(|read| !read.gaps.is_empty()) {
        incomplete.push(format!("{} source read gaps", read.gaps.len()));
    }
    for note in &view.notes {
        if note.contains("abandoned branch") {
            incomplete.push(
                "abandoned branch consumption is outside the pinned reader's active branch"
                    .to_string(),
            );
        }
    }
    let read = view.read.as_ref();
    if read.is_none() {
        incomplete.push("the reader established no source frontier".to_string());
    }
    let tokens = view.tokens.as_ref();
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
        harness_estimate_usd: view.cost.as_ref().map(|cost| cost.usd),
        incomplete,
    };
    let mut gaps = BTreeMap::new();
    for category in CATEGORIES {
        let reason = if counter(&facts.counters, category).is_none() {
            Some("recorded category is absent")
        } else if facts.basis.is_none() {
            Some("accounting basis is unknown")
        } else if facts.basis.as_deref() == Some("summed-requests") {
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
    if facts.harness_estimate_usd.is_none() {
        gaps.insert(
            "usd".to_string(),
            vec!["recorded category is absent".to_string()],
        );
    } else if facts.basis.is_none() {
        gaps.insert(
            "usd".to_string(),
            vec!["accounting basis is unknown".to_string()],
        );
    } else if facts.basis.as_deref() == Some("summed-requests") {
        gaps.insert(
            "usd".to_string(),
            vec!["per-event price coverage is unknown".to_string()],
        );
    }
    facts.quantity_gaps = Some(gaps);
    qualify_price(&mut facts);
    facts
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
    store: &Store,
    selection: &Selection,
    sessions: &[SessionRow],
) -> Result<Vec<Value>> {
    let read = store.read()?;
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for change_id in &selection.ledger_changes {
        for event in read.events(change_id)? {
            if let (Some(harness), Some(session)) = (&event.harness, &event.session) {
                *counts
                    .entry((harness.clone(), session.clone()))
                    .or_default() += 1;
            }
        }
    }
    Ok(counts
        .into_iter()
        .filter(|((harness, session), _)| {
            !sessions
                .iter()
                .any(|row| &row.harness == harness && &row.session == session)
        })
        .map(|((harness, session), events)| {
            json!({"harness": harness, "session": session, "events": events})
        })
        .collect())
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

/// Each quantity retains its measurement and assignment gaps independently.
fn totals(members: &[&SessionRow], withheld: &[String], scope: &str) -> Value {
    let keys: Vec<String> = members.iter().map(|row| row.key()).collect();
    let mut quantities = Vec::new();

    let harnesses: BTreeSet<&str> = members.iter().map(|row| row.harness.as_str()).collect();
    let mut domains: BTreeMap<(&str, Option<&str>), Vec<&SessionRow>> = BTreeMap::new();
    for row in members {
        let basis = row
            .measured
            .facts
            .as_ref()
            .and_then(|facts| facts.basis.as_deref());
        domains.entry((&row.harness, basis)).or_default().push(row);
    }
    let bases: BTreeSet<Option<&str>> = domains.keys().map(|(_, basis)| *basis).collect();
    let common_basis = (bases.len() == 1)
        .then(|| bases.first().copied().flatten())
        .flatten();
    for category in std::iter::once("usd").chain(CATEGORIES) {
        let base = |harness: Option<&str>, basis: Option<&str>| {
            let mut base = if category == "usd" {
                json!({"kind": USD_KIND, "unit": "USD", "pricing_basis": "unknown",
                    "source": "harness recording", "precision": "currency sums rounded to ten fractional places; smaller nonzero values preserved", "scope": scope})
            } else {
                base_tokens(category, scope)
            };
            base["accounting_basis"] = json!(basis);
            if let Some(harness) = harness {
                base["harness"] = json!(harness);
            }
            base
        };
        let mut combined = base(None, common_basis);
        if category != "usd" && harnesses.len() > 1 && withheld.is_empty() {
            let named = harnesses.iter().copied().collect::<Vec<_>>().join(", ");
            let (status, reason) = if category == "input" {
                ("refused", format!("a raw input sum across harnesses ({named}) is refused: their input conventions differ"))
            } else {
                ("withheld", format!("summing {category} across harnesses ({named}) waits for tapes' disjoint-token-categories, which the pinned reader does not carry"))
            };
            combined["status"] = json!(status);
            combined["reasons"] = json!([reason]);
            quantities.push(combined);
        } else {
            quantities.push(quantity(combined, aggregate(members, withheld, category)));
        }
        if domains.len() > 1 {
            for ((harness, basis), partition) in &domains {
                let mut partition_base = base(Some(harness), *basis);
                partition_base["partition"] = json!(true);
                quantities.push(quantity(
                    partition_base,
                    aggregate(partition, withheld, category),
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

fn base_tokens(category: &str, scope: &str) -> Value {
    json!({"kind": TOKEN_KIND, "unit": "tokens", "category": category,
        "convention": "tapes category; reasoning is a subset of output", "scope": scope})
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

fn recorded_amount(facts: &CostFacts, category: &str) -> Option<Amount> {
    if category == "usd" {
        facts.harness_estimate_usd.map(Amount::Usd)
    } else {
        counter(&facts.counters, category).map(Amount::Tokens)
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

fn aggregate(members: &[&SessionRow], withheld: &[String], category: &str) -> Aggregate {
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
    let domains: BTreeSet<(&str, Option<&str>)> = members
        .iter()
        .map(|row| {
            (
                row.harness.as_str(),
                row.measured
                    .facts
                    .as_ref()
                    .and_then(|facts| facts.basis.as_deref()),
            )
        })
        .collect();
    if domains.len() > 1 {
        let names = domains
            .iter()
            .map(|(harness, basis)| format!("{harness}/{}", basis.unwrap_or("unknown")))
            .collect::<Vec<_>>();
        return Aggregate::withheld(vec![format!("incompatible accounting domains: {}; quantities are partitioned by harness and accounting basis", names.join(", "))]);
    }
    let mut amounts = Vec::new();
    let mut missing = Vec::new();
    let mut gaps = BTreeSet::new();
    for row in members {
        match row.measured.facts.as_ref() {
            Some(facts) => {
                match recorded_amount(facts, category) {
                    Some(amount) => amounts.push(amount),
                    None => missing.push(row.key()),
                }
                for reason in &facts.incomplete {
                    gaps.insert(format!("{}: {reason}", row.key()));
                }
                match &facts.quantity_gaps {
                    Some(categories) => {
                        for reason in categories.get(category).into_iter().flatten() {
                            gaps.insert(format!("{}: {reason}", row.key()));
                        }
                    }
                    None => {
                        gaps.insert(format!("{}: quantity coverage was not retained", row.key()));
                    }
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
                        .and_then(|facts| recorded_amount(facts, category))
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
                        .and_then(|facts| recorded_amount(facts, category))
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
    let withheld_for = |keys: &[&WorkKey]| -> Vec<String> {
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
            "direct": totals(&direct, &withheld_for(&[&item.key]), "direct"),
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
            let mut totals = totals(&inclusive, &withheld_for(&keys), "inclusive");
            totals["members"] = json!([item.key.label(), source.label()]);
            totals["shared"] = json!(beyond);
            entry["inclusive"] = totals;
        }
        work.push(entry);
    }

    let all: Vec<&WorkKey> = selection.items.iter().map(|item| &item.key).collect();
    let (union_members, union_shared) = within(&all);
    let mut union = totals(&union_members, &withheld_for(&all), "union");
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
    let assignment_gaps = withheld_for(&all);
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
            "measurement": {
                "status": if unmeasured.is_empty() && unavailable_sources.is_empty() { "complete" } else { "incomplete" },
                "sessions": unmeasured,
                "current_sources": {"status": if unavailable_sources.is_empty() { "complete" } else { "incomplete" }, "unavailable": unavailable_sources},
                "residual": {
                    "status": "withheld",
                    "reason": "the residual between a recorded total and its requests waits for tapes' usage-total-request-reconciliation, which the pinned reader does not carry",
                },
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
        _ => quantity["category"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
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
        for reason in measurement["reasons"].as_array().into_iter().flatten() {
            println!("    {}", reason.as_str().unwrap_or_default());
        }
        print_totals("    ", "measured", &session["totals"]);
    }
    let coverage = &report["coverage"];
    println!("coverage");
    println!(
        "  measurement: {}",
        coverage["measurement"]["status"]
            .as_str()
            .unwrap_or_default()
    );
    println!(
        "  residual: withheld — {}",
        coverage["measurement"]["residual"]["reason"]
            .as_str()
            .unwrap_or_default()
    );
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

    /// The reader a measurement names is the one Cargo.toml pins.
    #[test]
    fn the_recorded_reader_names_the_pinned_tapes_revision() {
        let manifest = include_str!("../../Cargo.toml");
        let line = manifest
            .lines()
            .find(|line| line.starts_with("agent-tapes-core"))
            .expect("Cargo.toml pins agent-tapes-core");
        let field = |name: &str| {
            line.split(&format!("{name} = \""))
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_else(|| panic!("the pin names a {name}: {line}"))
        };
        assert_eq!(
            TAPES_READER,
            format!("agent-tapes-core {} {}", field("version"), field("rev"))
        );
    }
}
