//! The work cost view: what the sessions that act on a change's or a journal
//! artifact's work consumed, measured through tapes, and the retained facts
//! that let the arithmetic replay after a source rotates.
//!
//! Measurement and assignment are separate facts. A session is measured by
//! the pinned tapes reader, or by facts retained from an earlier read. An
//! executor session is assigned to work through the dispatches bound to it. A
//! participant is a session with no binding whose ledger events act on the
//! work: it is measured whole and never split, so it counts as direct only
//! for work that holds every change its events name, and is shared otherwise.
//! A session whose recording cannot be measured qualifies every total it would
//! join and is never counted as zero. A total names its quantity kind and is
//! withheld, refused, or absent rather than understated.

use super::cost_liveness::{Liveness, Snapshot};
use super::cost_read::CostRead;
use super::run::{rounds, RunSubject};
use super::*;
use crate::model::{
    plain_session_id, portable_cost_metadata, portable_cost_reason, portable_lineage_reason,
    portable_liveness, portable_quantity_reason, portable_selection, CostCacheWrites,
    CostConvention, CostCounterResidual, CostCounters, CostDisjoint, CostEventCoverage,
    CostEventIdentity, CostEvents, CostFacts, CostLineage, CostPricing, CostRelative, CostResidual,
    CostResidualSource, Event, Payload, DISJOINT_CATEGORIES,
};
use agent_tapes_core::measurement::{MeasurementCompleteness, MeasurementReason};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Append one formatted line to a text report.
macro_rules! emit {
    ($out:expr, $($arg:tt)*) => {{
        $out.push_str(&format!($($arg)*));
        $out.push('\n');
    }};
}
pub(super) use emit;

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
    /// The liveness retained with facts at the measured frontier: those the
    /// view uses, or for a fresh read, any retained at the same frontier.
    retained_liveness: Option<String>,
}

/// What a participant's events show it did. The variants are the fixed role
/// names, in the order the view lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    Integrator,
    Orchestrator,
    Reviewer,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Integrator => "integrator",
            Role::Orchestrator => "orchestrator",
            Role::Reviewer => "reviewer",
        }
    }
}

/// The one place an event kind names a role: a snapshot or a verdict is a
/// reviewer's, an integration, a debt or a closure an integrator's, and a
/// dispatch an orchestrator's. Any other kind makes its session no
/// participant, and nothing else, prose or trailer, supplies a role.
fn role_of(payload: &Payload) -> Option<Role> {
    match payload {
        Payload::PatchsetAdded { .. }
        | Payload::VerdictRecorded { .. }
        | Payload::ExternalVerdictRecorded { .. }
        | Payload::AuditVerdictRecorded { .. } => Some(Role::Reviewer),
        Payload::ChangeIntegrated { .. }
        | Payload::ChangeProvisionallyIntegrated { .. }
        | Payload::ProvisionalIntegrationResolved { .. }
        | Payload::IntegrationAsserted { .. }
        | Payload::DebtDeclared { .. }
        | Payload::AuditDebtDeclared { .. }
        | Payload::ChangeClosed { .. } => Some(Role::Integrator),
        Payload::RunDispatched { .. } => Some(Role::Orchestrator),
        _ => None,
    }
}

/// Why a participant's figure is incomplete while agent-status lists it.
const PARTICIPANT_LIVE: &str = "participant session is live: measured as of its read frontier";
/// Why a figure from facts retained before the session was known to end is
/// incomplete: the session may have gone on past the retained frontier.
const RETAINED_BEFORE_END: &str =
    "participant facts were retained before its session was known to end: measured as of their retained frontier";

/// What liveness a participant's figure is read under, and why it is not
/// final when it is not. A fresh read of an ended session is final; facts
/// retained after the session ended are final unless it is listed again.
struct ParticipantLiveness {
    fresh: Liveness,
    retained: Option<String>,
    incomplete: Option<String>,
}

impl ParticipantLiveness {
    fn of(measured: &Measured, fresh: Liveness) -> Self {
        let retained_ended = measured.retained_liveness.as_deref() == Some("ended");
        let incomplete = match (fresh, &measured.source) {
            (Liveness::Live, _) => Some(PARTICIPANT_LIVE.to_string()),
            (Liveness::Ended, Source::Fresh) => None,
            _ if retained_ended => None,
            (Liveness::Ended, _) => Some(RETAINED_BEFORE_END.to_string()),
            (Liveness::Unknown(reason), _) => Some(format!(
                "participant session liveness is unknown ({reason}): measured as of its read frontier"
            )),
        };
        Self {
            fresh,
            retained: measured.retained_liveness.clone(),
            incomplete,
        }
    }

    fn json(&self) -> Value {
        json!({"answer": self.fresh.answer(), "reason": self.fresh.reason(),
            "source": "agent-status", "retained": self.retained,
            "final": self.incomplete.is_none()})
    }
}

/// A session no dispatch binds, measured because its ledger events act on
/// the selected work.
struct Participation {
    roles: BTreeSet<Role>,
    liveness: ParticipantLiveness,
    /// The ledger read had no gap, so the membership names every change the
    /// session acted on. Otherwise a record that could name another one is
    /// unreadable and the session is never counted as the work's alone.
    membership_complete: bool,
}

struct SessionRow {
    harness: String,
    session: String,
    spans: Vec<Span>,
    membership: BTreeSet<WorkKey>,
    measured: Measured,
    overlap: BTreeSet<String>,
    participation: Option<Participation>,
}

impl SessionRow {
    fn key(&self) -> String {
        format!("{}:{}", self.harness, self.session)
    }
}

/// A selected run: the work it is for, the executor model its route names,
/// and the sessions its binding names.
pub(super) struct ExecutorRun {
    pub work: String,
    pub model: Option<String>,
    pub sessions: Vec<String>,
}

/// A session whose ledger events act on the selected work: the models it
/// declared in each role, and why it has no figure when it has none.
pub(super) struct Participant {
    pub key: String,
    pub models: BTreeMap<&'static str, BTreeSet<String>>,
    pub unmeasured: Option<String>,
}

/// What one read of a target yields: the `arc-cost/1` document, and the
/// facts that map a role to the sessions behind it.
pub(super) struct CostView {
    pub report: Value,
    pub runs: Vec<ExecutorRun>,
    pub participants: Vec<Participant>,
}

/// Print the read-only cost view of a change or a journal artifact.
pub fn show_cost(ctx: &Ctx, target: &str, json: bool) -> Result<()> {
    let view = read_cost(ctx, target)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&view.report)?);
    } else {
        print!("{}", report_text(&view.report));
    }
    Ok(())
}

/// Read the cost view of a change or a journal artifact.
pub(super) fn read_cost(ctx: &Ctx, target: &str) -> Result<CostView> {
    let store = ctx.store()?;
    let mut read = CostRead::new(&store);
    let events = read.events(Store::REPOSITORY_SCOPE);
    let mut selection = select(ctx, &mut read, target, &events)?;
    qualify_ledger(&mut selection, &read.gaps);
    let assigned = assign(ctx, &mut selection, &events);
    if !selection.readable && assigned.runs.is_empty() && assigned.sessions.is_empty() {
        bail!("cost target has no readable metadata");
    }
    let ledger = read_ledger(&mut read, &selection, &events, &assigned);
    let Assigned { runs, sessions, .. } = assigned;
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
                participation: None,
            }
        })
        .collect();
    // A participant whose recording cannot be read is still a row: its
    // missing figure qualifies every total it would have joined.
    let mut unmeasured = BTreeMap::new();
    let snapshot = std::cell::OnceCell::new();
    for ((harness, session), person) in ledger.participants() {
        let mut measured = measure(&events, harness, session);
        let liveness = ParticipantLiveness::of(
            &measured,
            snapshot.get_or_init(Snapshot::read).of(harness, session),
        );
        match (measured.facts.as_mut(), &measured.source) {
            (Some(facts), _) => facts.incomplete.extend(liveness.incomplete.clone()),
            (None, Source::Unavailable(reason)) => {
                unmeasured.insert((harness.clone(), session.clone()), reason.clone());
            }
            (None, _) => {
                unmeasured.insert(
                    (harness.clone(), session.clone()),
                    "usage read failed".to_string(),
                );
            }
        }
        sessions.push(SessionRow {
            harness: harness.clone(),
            session: session.clone(),
            spans: Vec::new(),
            membership: person.membership.clone(),
            measured,
            overlap: BTreeSet::new(),
            participation: Some(Participation {
                roles: person.roles.clone(),
                liveness,
                membership_complete: ledger.complete,
            }),
        });
    }
    let groups = lineage_groups(&sessions);
    for row in &mut sessions {
        row.overlap = groups
            .iter()
            .find(|group| group.contains(&row.key()))
            .cloned()
            .unwrap_or_default();
    }
    let outside = outside_sessions(&ledger, &sessions, &unmeasured);
    selection.ledger_gaps = read.gaps;
    let report = render_report(&selection, &runs, &sessions, &outside, &ledger);
    let executor_runs = runs
        .iter()
        .flat_map(|(work, runs)| {
            runs.iter().map(|run| ExecutorRun {
                work: work.label(),
                model: run.model.clone(),
                sessions: if run.conflicting {
                    Vec::new()
                } else {
                    run.candidates.clone()
                },
            })
        })
        .collect();
    let participants = ledger
        .participants()
        .map(|((harness, session), person)| {
            let key = format!("{harness}:{session}");
            Participant {
                models: person
                    .models
                    .iter()
                    .map(|(role, models)| (role.name(), models.clone()))
                    .collect(),
                unmeasured: unmeasured.get(&(harness.clone(), session.clone())).cloned(),
                key,
            }
        })
        .collect();
    Ok(CostView {
        report,
        runs: executor_runs,
        participants,
    })
}

/// Retain the normalized facts a fresh read of each bound or participating
/// session gives. Facts identical to ones already retained for the session
/// record nothing.
pub fn retain_cost(ctx: &Ctx, target: &str) -> Result<()> {
    let store = ctx.store()?;
    let mut read = CostRead::new(&store);
    let events = read.events(Store::REPOSITORY_SCOPE);
    let mut selection = select(ctx, &mut read, target, &events)?;
    qualify_ledger(&mut selection, &read.gaps);
    let assigned = assign(ctx, &mut selection, &events);
    if !selection.readable && assigned.runs.is_empty() && assigned.sessions.is_empty() {
        bail!("cost target has no readable metadata");
    }
    let ledger = read_ledger(&mut read, &selection, &events, &assigned);
    print_read_gaps(&selection.gaps, &read.gaps);
    let mut keys: BTreeSet<(String, String)> = assigned.sessions.into_keys().collect();
    keys.extend(ledger.participants().map(|(key, _)| key.clone()));
    if keys.is_empty() {
        println!("no bound or participating sessions");
        return Ok(());
    }
    // Read every source before taking the ledger lock: a recording can be
    // large, and other writers wait on the lock.
    let snapshot = Snapshot::read();
    let reads: Vec<(String, String, std::result::Result<CostFacts, String>)> = keys
        .into_iter()
        .map(|(harness, session)| {
            let read = read_fresh(&harness, &session).map(|mut facts| {
                facts.liveness = Some(snapshot.of(&harness, &session).answer().to_string());
                facts
            });
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
        let liveness = facts.liveness.clone().unwrap_or_default();
        if let Some((event, _)) = retained_for(&events, &harness, &session)
            .into_iter()
            .find(|(_, held)| **held == facts)
        {
            println!(
                "already retained: {key} frontier={} liveness={liveness}",
                facts.frontier
            );
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
        println!("retained: {key} frontier={frontier} liveness={liveness}");
        println!("event: {}", event.event_id);
    }
    Ok(())
}

fn print_read_gaps(selection: &[Value], ledger: &[Value]) {
    let mut out = String::new();
    write_read_gaps(&mut out, selection, ledger);
    print!("{out}");
}

fn write_read_gaps(out: &mut String, selection: &[Value], ledger: &[Value]) {
    for gap in selection.iter().chain(ledger) {
        emit!(
            out,
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
        gaps,
        ledger_gaps: Vec::new(),
        readable,
    })
}

struct AssignedRun {
    dispatch_event_id: String,
    /// The executor model the dispatch's route names, effort dropped.
    model: Option<String>,
    round: Option<usize>,
    candidates: Vec<String>,
    conflicting: bool,
    invalid: bool,
}

type Runs = BTreeMap<WorkKey, Vec<AssignedRun>>;
type Sessions = BTreeMap<(String, String), (Vec<Span>, BTreeSet<WorkKey>)>;

/// What the dispatches assign.
struct Assigned {
    runs: Runs,
    /// The bound sessions with a span in the selection.
    sessions: Sessions,
    /// The work each dispatch is for, selected or not.
    dispatches: BTreeMap<String, WorkKey>,
    /// Every session any dispatch binds, selected or not.
    bound: BTreeSet<(String, String)>,
}

/// The selected runs per work item, every bound session with at least
/// one span in the selection, with all of its spans across the repository:
/// a span on unselected work makes the session shared beyond the view. A run
/// is a change's when its dispatch names that change's exact id, so what a
/// run is assigned to never moves as other changes are opened.
fn assign(ctx: &Ctx, selection: &mut Selection, events: &[Event]) -> Assigned {
    let selected: BTreeSet<WorkKey> = selection
        .items
        .iter()
        .map(|item| item.key.clone())
        .collect();
    let mut runs: Runs = BTreeMap::new();
    let mut sessions: Sessions = BTreeMap::new();
    let mut dispatches = BTreeMap::new();
    let mut bound = BTreeSet::new();
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
        dispatches.insert(round.dispatch.event_id.clone(), work.clone());
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
            let model = match &round.dispatch.payload {
                Payload::RunDispatched { route, .. } => model_of(route),
                _ => None,
            };
            runs.entry(work.clone()).or_default().push(AssignedRun {
                dispatch_event_id: round.dispatch.event_id.clone(),
                model,
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
            bound.insert(key.clone());
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
    Assigned {
        runs,
        sessions,
        dispatches,
        bound,
    }
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
            let retained_liveness = retained
                .as_ref()
                .filter(|(_, held)| held.frontier == fresh.frontier)
                .and_then(|(_, held)| held.liveness.clone());
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
                retained_liveness,
            }
        }
        Err(reason) => match retained {
            Some((event, held)) => Measured {
                source: Source::Retained(event.event_id.clone(), reason),
                retained_liveness: held.liveness.clone(),
                facts: Some(held),
                conflict: None,
            },
            None => Measured {
                source: Source::Unavailable(reason),
                facts: None,
                conflict: None,
                retained_liveness: None,
            },
        },
    }
}

/// Two reads measured the same thing, whichever reader made them and
/// whatever liveness each was taken under.
fn same_measurement(left: &CostFacts, right: &CostFacts) -> bool {
    let mut left = left.clone();
    left.reader.clone_from(&right.reader);
    left.liveness.clone_from(&right.liveness);
    left == *right
}

/// The facts a conflict compares, without the reader and calculation names
/// or the liveness they were taken under.
fn measurement_facts(facts: &CostFacts) -> Value {
    let mut value = serde_json::to_value(facts).unwrap_or_default();
    if let Some(object) = value.as_object_mut() {
        for name in ["reader", "calculation", "frontier", "liveness"] {
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
    let read = backend
        .stream_transcript(&resolved.session, None, &mut |turn| {
            earliest = earliest.into_iter().chain(turn.ts).min();
            tally.add(&turn);
            Ok(())
        })
        .map_err(|_| "usage read failed".to_string())?;
    let selection_warnings = resolved
        .diagnostics
        .latest_warnings(&resolved.session)
        .len();
    let whole = backend
        .stream_session(&resolved.session, &read)
        .map_err(|_| "usage read failed".to_string())?;
    let view = agent_tapes_core::usage::streamed(&whole, tally, &read);
    let mut facts = normalize(&view, selection_warnings, harness);
    if harness == "claude" {
        facts.residual = Some(residual(
            backend.as_ref(),
            &resolved.session,
            facts.frontier,
        ));
        qualify_residual(&mut facts, harness);
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
    // An unreadable liveness answer reads as unknown; the measurement stands.
    if facts
        .liveness
        .as_deref()
        .is_some_and(|liveness| !portable_liveness(liveness))
    {
        facts.liveness = None;
    }
    if let Some(gaps) = facts
        .disjoint
        .as_mut()
        .and_then(|disjoint| disjoint.gaps.as_mut())
    {
        let unknown_category = gaps
            .keys()
            .any(|key| !DISJOINT_CATEGORIES.contains(&key.as_str()));
        gaps.retain(|key, _| DISJOINT_CATEGORIES.contains(&key.as_str()));
        if unknown_category {
            for category in DISJOINT_CATEGORIES {
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

/// The fixed reason arc reports for each reason tapes names. No reason
/// carries a recording path, an error text, or a count of notes.
fn arc_reason(reason: MeasurementReason) -> &'static str {
    match reason {
        MeasurementReason::MissingCounter => "some counted events did not record this category",
        MeasurementReason::MissingPrice => "some counted events recorded no price",
        MeasurementReason::Unpriced => ZERO_PRICE,
        MeasurementReason::StoppedAttempt => "a request stopped before its usage was complete",
        MeasurementReason::UnreadableRecord => "some source records could not be read",
        MeasurementReason::ReadWindow => {
            "the figure covers only the part of the recording the read reached"
        }
        MeasurementReason::SinceReset => "the counters restarted inside the recording",
        MeasurementReason::UnestablishedScope => "no accounting establishes what the figure covers",
        MeasurementReason::IncompleteAttribution => {
            "some requests could not be attributed to a model"
        }
        MeasurementReason::Undetermined => "the recorded counters do not determine this category",
        MeasurementReason::UnrecognizedUsage => "some records carrying usage were not read",
    }
}

fn arc_reasons<'a>(reasons: impl IntoIterator<Item = &'a MeasurementReason>) -> Vec<String> {
    let mut reasons: Vec<String> = reasons
        .into_iter()
        .map(|reason| arc_reason(*reason).to_string())
        .collect();
    reasons.sort();
    reasons.dedup();
    reasons
}

/// Reasons that say only that the figure holds no amount, which arc reports
/// as its own absence of a recorded category.
fn is_absence(reason: MeasurementReason) -> bool {
    matches!(
        reason,
        MeasurementReason::MissingCounter | MeasurementReason::MissingPrice
    )
}

/// The facts arc keeps from one usage answer. The counters and the dollar
/// figure are the reader's, unchanged; what keeps them from covering the
/// session is named rather than corrected. Where the reader sums consumption
/// over every branch a recording retains, that sum is the measurement, not
/// the active branch's. Why a counter is not completely measured is tapes'
/// typed answer for the figure measured; the only prose arc still counts is
/// the reader's `--latest` selection warnings, which no typed field carries.
fn normalize(
    view: &agent_tapes_core::usage::UsageView,
    selection_warnings: usize,
    harness: &str,
) -> CostFacts {
    let consumption = view.consumption.as_ref();
    let completeness = consumption.map_or(&view.measurement_completeness, |consumption| {
        &consumption.measurement_completeness
    });
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
    if selection_warnings > 0 {
        incomplete.push(format!("{selection_warnings} selection warnings"));
    }
    if let Some(read) = view.read.as_ref().filter(|read| !read.gaps.is_empty()) {
        incomplete.push(format!("{} source read gaps", read.gaps.len()));
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
                    .with_completeness(completeness)
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
        liveness: None,
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
                gaps: Some(
                    disjoint
                        .measurement_completeness
                        .iter()
                        .map(|(category, reasons)| (category.to_string(), arc_reasons(reasons)))
                        .collect(),
                ),
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
    let mut gaps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let typed = |counter: &str, absent: bool| -> Vec<String> {
        completeness
            .iter()
            .filter(|(name, _)| *name == counter)
            .flat_map(|(_, reasons)| reasons)
            .filter(|reason| !(absent && is_absence(**reason)))
            .map(|reason| arc_reason(*reason).to_string())
            .collect()
    };
    for category in CATEGORIES {
        let absent = counter(&facts.counters, category).is_none();
        let mut reasons = Vec::new();
        if absent {
            reasons.push("recorded category is absent".to_string());
        }
        if facts.basis.is_none() {
            reasons.push("accounting basis is unknown".to_string());
        }
        reasons.extend(typed(category, absent));
        if !reasons.is_empty() {
            reasons.sort();
            reasons.dedup();
            gaps.insert(category.to_string(), reasons);
        }
    }
    let zero_written = facts.cost_coverage.as_ref().is_some_and(|coverage| {
        coverage.zero_written_requests > 0 || coverage.zero_written_observations > 0
    });
    let mut usd = Vec::new();
    match facts.harness_estimate_usd {
        None if kind_supported == Some(false) => {
            usd.push("recorded cost kind is unsupported".to_string())
        }
        None if zero_written => usd.push(ZERO_PRICE.to_string()),
        None => usd.push("recorded category is absent".to_string()),
        Some(_) if facts.basis.is_none() => usd.push("accounting basis is unknown".to_string()),
        Some(_) => {}
    }
    usd.extend(typed("cost", facts.harness_estimate_usd.is_none()));
    if !usd.is_empty() {
        usd.sort();
        usd.dedup();
        gaps.insert("usd".to_string(), usd);
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
fn qualify_residual(facts: &mut CostFacts, harness: &str) {
    let conflicts: Vec<String> = facts
        .residual
        .iter()
        .flat_map(|residual| &residual.counters)
        .filter(|row| row.status == "conflict")
        .map(|row| row.counter.clone())
        .collect();
    let convention = agent_tapes_core::model::TokenConvention::of(harness);
    for counter in conflicts {
        // The categories restating the counter are the ones tapes derives
        // from it.
        let mut native = MeasurementCompleteness::default();
        native.qualify(&counter, MeasurementReason::Undetermined);
        let restated: Vec<String> = convention
            .as_ref()
            .map(|convention| native.disjoint(convention))
            .iter()
            .flat_map(|disjoint| disjoint.iter().map(|(category, _)| category.to_string()))
            .collect();
        if let Some(disjoint) = &mut facts.disjoint {
            for category in restated {
                push_reason(
                    disjoint
                        .gaps
                        .get_or_insert_with(BTreeMap::new)
                        .entry(category)
                        .or_default(),
                );
            }
        }
        push_reason(
            facts
                .quantity_gaps
                .get_or_insert_with(BTreeMap::new)
                .entry(counter)
                .or_default(),
        );
    }
}

fn push_reason(reasons: &mut Vec<String>) {
    if !reasons.iter().any(|reason| reason == RESIDUAL_CONFLICT) {
        reasons.push(RESIDUAL_CONFLICT.to_string());
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

/// The model of a `model[#effort]` coordinate, without its effort.
fn model_of(coordinate: &str) -> Option<String> {
    let model = coordinate.split('#').next().unwrap_or(coordinate).trim();
    (!model.is_empty()).then(|| model.to_string())
}

/// A session the ledger's events name.
#[derive(Default)]
struct Person {
    /// Every work item in this ledger whose events carry the session.
    membership: BTreeSet<WorkKey>,
    /// What its events on the selected work show it did.
    roles: BTreeSet<Role>,
    /// The models its events declared in each role on the selected work,
    /// effort dropped.
    models: BTreeMap<Role, BTreeSet<String>>,
    /// Its events on the selected work, of any kind.
    selected_events: usize,
    /// It has an event of a role-bearing kind on the selected work.
    acts: bool,
    /// A dispatch binds it, so it is an executor and no participant.
    bound: bool,
}

/// The sessions every event in this ledger names, read once, and the acting
/// events that name none.
struct Ledger {
    people: BTreeMap<(String, String), Person>,
    /// Acting events on the selected work without a usable harness and
    /// session: whoever wrote them cannot be named or measured.
    unattributed_events: usize,
    /// No ledger record went unread.
    complete: bool,
}

impl Ledger {
    /// The sessions measured as participants: unbound, and acting on the
    /// selected work.
    fn participants(&self) -> impl Iterator<Item = (&(String, String), &Person)> {
        self.people
            .iter()
            .filter(|(_, person)| person.acts && !person.bound)
    }
}

/// One pass over the repository's dispatches and every change's events, so a
/// session's membership names each change it acted on, not only the selected
/// ones.
fn read_ledger(
    read: &mut CostRead<'_>,
    selection: &Selection,
    repository: &[Event],
    assigned: &Assigned,
) -> Ledger {
    let selected: BTreeSet<&WorkKey> = selection.items.iter().map(|item| &item.key).collect();
    let mut ledger = Ledger {
        people: BTreeMap::new(),
        unattributed_events: 0,
        complete: false,
    };
    let mut note = |event: &Event, work: WorkKey| {
        let role = role_of(&event.payload);
        let on_selection = selected.contains(&work);
        match (&event.harness, &event.session) {
            (Some(harness), Some(session))
                if plain_session_id(harness) && plain_session_id(session) =>
            {
                let person = ledger
                    .people
                    .entry((harness.clone(), session.clone()))
                    .or_default();
                person.membership.insert(work);
                if on_selection {
                    person.selected_events += 1;
                    if let Some(role) = role {
                        person.acts = true;
                        person.roles.insert(role);
                        if let Some(model) = event.model.as_deref().and_then(model_of) {
                            person.models.entry(role).or_default().insert(model);
                        }
                    }
                }
            }
            _ => {
                if on_selection && role.is_some() {
                    ledger.unattributed_events += 1;
                }
            }
        }
    };
    for event in repository {
        if let Some(work) = assigned.dispatches.get(&event.event_id) {
            note(event, work.clone());
        }
    }
    for change_id in read.change_ids() {
        for event in read.events(&change_id) {
            note(&event, WorkKey::Change(change_id.clone()));
        }
    }
    for (key, person) in &mut ledger.people {
        person.bound = assigned.bound.contains(key);
    }
    ledger.complete = read.gaps.is_empty();
    ledger
}

/// Sessions that acted on the selected work and are not among the measured
/// rows: a session that holds no acting event kind, one bound to a dispatch
/// elsewhere, and a participant whose recording the store cannot resolve,
/// with the reason. Their consumption is outside assignment coverage, and
/// the view names them without a figure.
fn outside_sessions(
    ledger: &Ledger,
    sessions: &[SessionRow],
    unmeasured: &BTreeMap<(String, String), String>,
) -> Vec<Value> {
    ledger
        .people
        .iter()
        .filter(|(key, person)| {
            person.selected_events > 0
                && (unmeasured.contains_key(key)
                    || !sessions
                        .iter()
                        .any(|row| row.harness == key.0 && row.session == key.1))
        })
        .map(|(key, person)| {
            let ((harness, session), events) = (key, person.selected_events);
            let mut row = json!({"harness": harness, "session": session, "events": events});
            if let Some(reason) = unmeasured.get(key) {
                row["roles"] = json!(person
                    .roles
                    .iter()
                    .map(|role| role.name())
                    .collect::<Vec<_>>());
                row["reason"] = json!(reason);
            }
            row
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

/// The members of each accounting domain of one figure. A session with no
/// measurement has no basis of its own, so it joins every domain of its
/// harness, or every domain when the figure adds across harnesses, and
/// qualifies each one it could belong to.
type Domain<'a> = (Option<&'a str>, Option<&'a str>);

fn partitions<'a>(
    members: &[&'a SessionRow],
    figure: Figure,
) -> BTreeMap<Domain<'a>, Vec<&'a SessionRow>> {
    let (measured, unmeasured): (Vec<&SessionRow>, Vec<&SessionRow>) =
        members.iter().partition(|row| row.measured.facts.is_some());
    let mut domains: BTreeMap<Domain<'a>, Vec<&'a SessionRow>> = BTreeMap::new();
    for row in measured {
        domains.entry(domain(row, figure)).or_default().push(row);
    }
    for row in unmeasured {
        let (harness, _) = domain(row, figure);
        let mut joined = false;
        for (key, partition) in &mut domains {
            if key.0 == harness {
                partition.push(row);
                joined = true;
            }
        }
        if !joined {
            domains.entry(domain(row, figure)).or_default().push(row);
        }
    }
    domains
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
        let domains = partitions(members, figure);
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
            let categories: &[&str] = match category {
                "total" => &["uncached_input", "cache_read", "cache_write", "output"],
                "cache_write_5m" | "cache_write_1h" | "cache_write_ttl_unrecorded" => {
                    &["cache_write"]
                }
                _ => std::slice::from_ref(&category),
            };
            let Some(gaps) = &disjoint.gaps else {
                return vec!["quantity coverage was not retained".to_string()];
            };
            let mut reasons: BTreeSet<String> = categories
                .iter()
                .flat_map(|category| gaps.get(*category).into_iter().flatten().cloned())
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
    let domains: Vec<Domain<'_>> = partitions(members, figure).into_keys().collect();
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
    outside: &[Value],
    ledger: &Ledger,
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
        // A participant has no dispatch to start from: its whole recording is
        // its figure.
        for row in sessions.iter().filter(|row| {
            row.participation.is_none() && row.membership.iter().any(|work| keys.contains(&work))
        }) {
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
    // A session counts for these items alone when they hold every work item
    // it acted on, and for a participant when that membership is complete.
    let counted = |row: &SessionRow, keys: &[&WorkKey]| -> bool {
        row.participation
            .as_ref()
            .is_none_or(|participation| participation.membership_complete)
            && row.membership.iter().all(|work| keys.contains(&work))
    };
    let within = |keys: &[&WorkKey]| -> (Vec<&SessionRow>, Vec<String>) {
        let mut inside = Vec::new();
        let mut shared = Vec::new();
        for row in sessions {
            if counted(row, keys) {
                inside.push(row);
            } else if row.membership.iter().any(|work| keys.contains(&work)) {
                shared.push(row.key());
            }
        }
        (inside, shared)
    };
    // Every participant that acted on these items, with its whole figure. It
    // is `counted` where the items hold every change its events name.
    let participants_in = |keys: &[&WorkKey]| -> Vec<Value> {
        sessions
            .iter()
            .filter_map(|row| {
                let participation = row.participation.as_ref()?;
                if !row.membership.iter().any(|work| keys.contains(&work)) {
                    return None;
                }
                Some(json!({
                    "key": row.key(),
                    "roles": participation.roles.iter().map(|role| role.name()).collect::<Vec<_>>(),
                    "spans": row.membership.len(),
                    "counted": counted(row, keys),
                    "totals": totals(&[row], &[], &format!("session {}", row.key())),
                }))
            })
            .collect()
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
            "participants": participants_in(&[&item.key]),
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
            totals["participants"] = json!(participants_in(&keys));
            entry["inclusive"] = totals;
        }
        work.push(entry);
    }

    let all: Vec<&WorkKey> = selection.items.iter().map(|item| &item.key).collect();
    let (union_members, union_shared) = within(&all);
    let mut union = totals(&union_members, &withheld_for(&all, "union"), "union");
    union["members"] = json!(all.iter().map(|key| key.label()).collect::<Vec<_>>());
    union["shared"] = json!(union_shared);
    union["participants"] = json!(participants_in(&all));

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
    if !ledger.complete && sessions.iter().any(|row| row.participation.is_some()) {
        assignment_gaps.push(
            "ledger records are unreadable, so a participant's membership may be incomplete and it is never counted as the work's alone".to_string(),
        );
    }
    for session in outside
        .iter()
        .filter(|session| session["reason"].is_string())
    {
        assignment_gaps.push(format!(
            "participant {}:{} acted on the selected work and is not measured ({}), so its consumption is outside assignment coverage",
            session["harness"].as_str().unwrap_or_default(),
            session["session"].as_str().unwrap_or_default(),
            session["reason"].as_str().unwrap_or_default(),
        ));
    }
    if ledger.unattributed_events > 0 {
        assignment_gaps.push(format!(
            "{} acting events carry no session identity, so their sessions are not measured here",
            ledger.unattributed_events
        ));
    }
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
                "participants": {
                    "statement": "a participant is a session no dispatch binds whose ledger events act on the selected work; it is measured whole and never split, and it may also act in other repositories' ledgers, which this ledger cannot see",
                    "sessions": sessions
                        .iter()
                        .filter(|row| row.participation.is_some())
                        .map(SessionRow::key)
                        .collect::<Vec<_>>(),
                },
                "outside": {
                    "statement": "a session that acted on the selected work without a measured recording, such as one the store cannot resolve, is outside assignment coverage: it is not measured here and is never zero",
                    "sessions": outside,
                    "unattributed_events": ledger.unattributed_events,
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
    let mut value = json!({
        "key": row.key(),
        "harness": row.harness,
        "session": row.session,
        "kind": if row.participation.is_some() { "participant" } else { "executor" },
        "spans": row.spans.iter().map(|span| json!({
            "dispatch_event_id": span.dispatch_event_id,
            "dispatched_at": span.dispatched_at,
            "forked_from": span.forked_from,
            "round": span.round,
            "work": span.work.label(),
        })).collect::<Vec<_>>(),
        "membership": row.membership.iter().map(WorkKey::label).collect::<Vec<_>>(),
        "per_run": if row.participation.is_some() { "not-a-run" } else if row.spans.len() > 1 { "shared-among-spans" } else { "whole-session" },
        "measurement": measurement,
        "totals": totals(&[row], &[], &scope),
    });
    if let Some(participation) = &row.participation {
        value["participant"] = json!({
            "roles": participation.roles.iter().map(|role| role.name()).collect::<Vec<_>>(),
            "spans": row.membership.len(),
        });
        value["measurement"]["liveness"] = participation.liveness.json();
    }
    value
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

pub(super) fn write_totals(out: &mut String, indent: &str, heading: &str, totals: &Value) {
    let quantities = totals["quantities"].as_array().cloned().unwrap_or_default();
    let parts: Vec<String> = quantities.iter().map(describe_quantity).collect();
    emit!(out, "{indent}{heading}: {}", parts.join("; "));
    emit!(
        out,
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
        emit!(out, "{indent}  {}", crate::render::one_line(&reason));
    }
    if let Some(unknown) = totals["identity"]["unknown"].as_object() {
        for (session, reasons) in unknown {
            for reason in reasons.as_array().into_iter().flatten() {
                emit!(
                    out,
                    "{indent}  {session}: {}",
                    reason.as_str().unwrap_or_default()
                );
            }
        }
    }
}

/// The text form of a cost report.
pub(super) fn report_text(report: &Value) -> String {
    let mut out = String::new();
    write_report(&mut out, report);
    out
}

fn write_report(out: &mut String, report: &Value) {
    let target = &report["target"];
    match target["kind"].as_str() {
        Some("journal") => {
            let lineage: Vec<&str> = target["lineage"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            emit!(
                out,
                "cost: journal {}  lineage: {}",
                target["file"].as_str().unwrap_or_default(),
                lineage.join(" -> ")
            );
        }
        _ => emit!(
            out,
            "cost: change {}",
            target["id"].as_str().unwrap_or_default()
        ),
    }
    for item in report["work"].as_array().into_iter().flatten() {
        let runs = item["runs"].as_array().map_or(0, Vec::len);
        emit!(
            out,
            "{}  runs={runs}",
            item["key"].as_str().unwrap_or_default()
        );
        for unbound in item["unbound"].as_array().into_iter().flatten() {
            emit!(
                out,
                "  unbound dispatch {}",
                unbound.as_str().unwrap_or_default()
            );
        }
        write_totals(out, "  ", "direct", &item["direct"]);
        let shared: Vec<&str> = item["shared"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !shared.is_empty() {
            emit!(out, "  shared: {}", shared.join(", "));
        }
        write_participants(out, "  ", &item["participants"]);
        if item["inclusive"].is_object() {
            write_totals(out, "  ", "inclusive", &item["inclusive"]);
        }
    }
    emit!(out, "union");
    write_totals(out, "  ", "total", &report["union"]);
    write_participants(out, "  ", &report["union"]["participants"]);
    emit!(out, "sessions");
    for session in report["sessions"].as_array().into_iter().flatten() {
        let measurement = &session["measurement"];
        emit!(
            out,
            "  {}  {} {}  frontier={}  spans={}  {}",
            session["key"].as_str().unwrap_or_default(),
            measurement["source"].as_str().unwrap_or_default(),
            measurement["status"].as_str().unwrap_or_default(),
            measurement["frontier"],
            if session["kind"] == "participant" {
                session["participant"]["spans"].clone()
            } else {
                json!(session["spans"].as_array().map_or(0, Vec::len))
            },
            if session["kind"] == "participant" {
                format!(
                    "participant ({})",
                    role_names(&session["participant"]["roles"])
                )
            } else {
                session["per_run"].as_str().unwrap_or_default().to_string()
            },
        );
        let liveness = &measurement["liveness"];
        if let Some(answer) = liveness["answer"].as_str() {
            emit!(
                out,
                "    liveness: {answer}{}{}",
                liveness["reason"]
                    .as_str()
                    .map_or(String::new(), |reason| format!(" ({reason})")),
                liveness["retained"]
                    .as_str()
                    .map_or(String::new(), |retained| format!("; retained {retained}"))
            );
        }
        if measurement["conflict"].is_object() {
            emit!(
                out,
                "    conflict: a fresh read disagrees with retained facts at the same frontier"
            );
        }
        if measurement["fresh_read"]["status"] == "unavailable" {
            emit!(
                out,
                "    fresh read unavailable: {}",
                measurement["fresh_read"]["reason"]
                    .as_str()
                    .unwrap_or_default()
            );
        }
        if let Some(selection) = measurement["selection"].as_str() {
            emit!(
                out,
                "    counts {selection}: {} events, {} beyond the active branch",
                measurement["events"]["counted"],
                measurement["events"]["other_branches"]
            );
        }
        for reason in measurement["reasons"].as_array().into_iter().flatten() {
            emit!(out, "    {}", reason.as_str().unwrap_or_default());
        }
        write_totals(out, "    ", "measured", &session["totals"]);
    }
    let coverage = &report["coverage"];
    emit!(out, "coverage");
    write_read_gaps(
        out,
        report["coverage"]["selection"]["records"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default(),
        report["coverage"]["ledger"]["records"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default(),
    );
    emit!(
        out,
        "  measurement: {}",
        coverage["measurement"]["status"]
            .as_str()
            .unwrap_or_default()
    );
    let residual = &coverage["measurement"]["residual"];
    emit!(
        out,
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
        emit!(
            out,
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
    emit!(
        out,
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
        emit!(out, "    {}", reason.as_str().unwrap_or_default());
    }
    emit!(
        out,
        "  identity: {}",
        coverage["identity"]["status"].as_str().unwrap_or_default()
    );
    let outside = &coverage["assignment"]["outside"];
    emit!(
        out,
        "  {}",
        outside["statement"].as_str().unwrap_or_default()
    );
    for session in outside["sessions"].as_array().into_iter().flatten() {
        emit!(
            out,
            "    {}:{} ({} events){}",
            session["harness"].as_str().unwrap_or_default(),
            session["session"].as_str().unwrap_or_default(),
            session["events"],
            session["reason"]
                .as_str()
                .map_or(String::new(), |reason| format!(": {reason}"))
        );
    }
    if outside["unattributed_events"].as_u64().unwrap_or(0) > 0 {
        emit!(
            out,
            "    {} acting events name no session",
            outside["unattributed_events"]
        );
    }
    emit!(
        out,
        "  {}",
        coverage["assignment"]["participants"]["statement"]
            .as_str()
            .unwrap_or_default()
    );
}

fn role_names(roles: &Value) -> String {
    roles
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn write_participants(out: &mut String, indent: &str, participants: &Value) {
    for participant in participants.as_array().into_iter().flatten() {
        emit!(
            out,
            "{indent}participant {} ({}) spans={} {}",
            participant["key"].as_str().unwrap_or_default(),
            role_names(&participant["roles"]),
            participant["spans"],
            if participant["counted"] == true {
                "counted"
            } else {
                "shared"
            }
        );
        write_totals(out, &format!("{indent}  "), "whole", &participant["totals"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each reason tapes names has its own fixed reason, which travels.
    #[test]
    fn every_tapes_reason_maps_to_a_distinct_portable_reason() {
        use MeasurementReason::*;
        let reasons = [
            MissingCounter,
            MissingPrice,
            Unpriced,
            StoppedAttempt,
            UnreadableRecord,
            ReadWindow,
            SinceReset,
            UnestablishedScope,
            IncompleteAttribution,
            Undetermined,
            UnrecognizedUsage,
        ];
        let mapped: BTreeSet<&str> = reasons.into_iter().map(arc_reason).collect();
        assert_eq!(mapped.len(), reasons.len());
        assert!(mapped.iter().all(|reason| portable_quantity_reason(reason)));
    }

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
