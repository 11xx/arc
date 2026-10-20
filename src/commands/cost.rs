//! The work cost view: what the sessions that act on a change's or a journal
//! artifact's work consumed, measured through tapes, and the retained facts
//! that let the arithmetic replay after a source rotates.
//!
//! Measurement and assignment are separate facts. A session is measured by
//! the pinned tapes reader, or by facts retained from an earlier read. An
//! executor session is assigned to work through the dispatches bound to it. A
//! participant is a session with no binding whose ledger events act on the
//! work. A session the ledger gives several spans is read per segment of its
//! timeline, and each work item takes the segments it alone covers; work that
//! holds every item a session acted on counts it whole.
//! A session whose recording cannot be measured qualifies every total it would
//! join and is never counted as zero. A total names its quantity kind and is
//! withheld, refused, or absent rather than understated.

mod span;

use super::cost_liveness::{Liveness, Snapshot};
use super::cost_read::CostRead;
use super::run::{rounds, RunSubject};
use super::*;
use crate::model::{
    plain_session_id, portable_coordinate, portable_cost_metadata, portable_cost_reason,
    portable_lineage_reason, portable_liveness, portable_quantity_reason, portable_selection,
    retained_coverage_reason, CostCacheWrites, CostConvention, CostCounterResidual, CostCounters,
    CostDisjoint, CostEstimate, CostEstimatePart, CostEventCoverage, CostEventIdentity, CostEvents,
    CostFacts, CostLineage, CostListPrice, CostNativeChild, CostPricing, CostRelative,
    CostRequestTerms, CostResidual, CostResidualSource, CostSegment, CostSpans, Event, Payload,
    COUNTERS_READ_WINDOW, COUNTERS_SINCE_RESET, DISJOINT_CATEGORIES,
};
use agent_tapes_core::measurement::{MeasurementCompleteness, MeasurementReason};
use serde_json::{json, Value};
use span::{ItemSpan, Plan, Segment};
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
pub const COST_CALCULATION: &str = "usage-normalization 9";

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
const UNLISTED_MODEL: &str = "the price table has no row for the model that served the usage";
const TTL_UNRECORDED: &str = "some cache writes recorded no TTL";
const RESIDUAL_CONFLICT: &str = "recorded total is below its identified requests";
const LIST_KIND: &str = agent_tapes_core::pricing::ESTIMATE_KIND;
const NO_TABLE: &str = "no price table is loaded";
const NO_SERVED_MODEL: &str = "no single model is known to have served the figure";

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
#[derive(Clone)]
struct Span {
    dispatch_event_id: String,
    dispatched_at: chrono::DateTime<chrono::Utc>,
    /// When the run's recorded `run end` was written, if one was.
    ended_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The model coordinate the dispatch's route names, when it names one.
    route: Option<String>,
    forked_from: Option<String>,
    /// The session that spawned the executor as its native child.
    parent: Option<String>,
    /// The role the binding names; absent reads as implementer.
    role: Option<String>,
    round: Option<usize>,
    work: WorkKey,
}

/// Where a session's measurement came from.
#[derive(Clone)]
enum Source {
    Fresh,
    Retained(String, String),
    Unavailable(String),
}

#[derive(Clone)]
struct Measured {
    source: Source,
    facts: Option<CostFacts>,
    conflict: Option<Value>,
    /// The liveness retained with facts at the measured frontier: those the
    /// view uses, or for a fresh read, any retained at the same frontier.
    retained_liveness: Option<String>,
    /// When the facts the view uses were retained, for a retained source.
    retained_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// What a session's events show it did. The variants are the fixed role
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
        | Payload::ReviewStarted { .. }
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

/// The role an event gives a session it names: the role its kind names,
/// except that a patchset makes a reviewer only of a session no dispatch
/// binds. A bound session's patchsets are the work its binding gave it, and
/// its own verdicts, integrations and dispatches give it their roles beside
/// that one.
fn acting_role(payload: &Payload, bound: bool) -> Option<Role> {
    match payload {
        Payload::PatchsetAdded { .. } if bound => None,
        _ => role_of(payload),
    }
}

/// Why a participant's figure is incomplete while agent-status lists it.
const PARTICIPANT_LIVE: &str = "participant session is live: measured as of its read frontier";
/// Why a figure from facts retained before the session was known to end is
/// incomplete: the session may have gone on past the retained frontier.
const RETAINED_BEFORE_END: &str =
    "participant facts were retained before its session was known to end: measured as of their retained frontier";

/// Why an executor's figure is incomplete while agent-status lists it.
const EXECUTOR_LIVE: &str = "executor session is live: measured as of its read frontier";
/// Why an executor's figure from facts retained before its last run ended is
/// incomplete.
const RETAINED_BEFORE_RUN_END: &str =
    "executor facts were retained before its last run ended: measured as of their retained frontier";

/// What liveness a session's figure is read under, and why it is not final
/// when it is not.
#[derive(Clone)]
struct SessionLiveness {
    fresh: Liveness,
    retained: Option<String>,
    /// For an executor, how many of its bound dispatches have a recorded
    /// run end, of how many.
    runs: Option<(usize, usize)>,
    incomplete: Option<String>,
}

impl SessionLiveness {
    /// A participant has no run to end, so agent-status alone answers. A
    /// fresh read of an ended session is final; facts retained after the
    /// session ended are final unless it is listed again.
    fn participant(measured: &Measured, fresh: Liveness) -> Self {
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
            runs: None,
            incomplete,
        }
    }

    /// The ledger is the stronger evidence for an executor: a session whose
    /// every bound dispatch has a recorded run end has ended, and one whose
    /// dispatch has none is still running. An agent-status listing makes it
    /// live either way. When unreadable ledger records could hold the
    /// missing end, only a listing qualifies the figure.
    fn executor(measured: &Measured, fresh: Liveness, spans: &[Span], ledger: bool) -> Self {
        let open: Vec<&str> = spans
            .iter()
            .filter(|span| span.ended_at.is_none())
            .map(|span| span.dispatch_event_id.as_str())
            .collect();
        let last_end = spans.iter().filter_map(|span| span.ended_at).max();
        let retained_after_end = measured.retained_liveness.as_deref() == Some("ended")
            || measured
                .retained_at
                .zip(last_end)
                .is_some_and(|(at, end)| at >= end);
        let incomplete = match (fresh, &measured.source) {
            (Liveness::Live, _) => Some(EXECUTOR_LIVE.to_string()),
            _ if !open.is_empty() && ledger => Some(format!(
                "executor dispatch {} has no recorded run end: measured as of its read frontier",
                open.join(", ")
            )),
            _ if !open.is_empty() => None,
            (_, Source::Fresh) => None,
            _ if retained_after_end => None,
            _ => Some(RETAINED_BEFORE_RUN_END.to_string()),
        };
        Self {
            fresh,
            retained: measured.retained_liveness.clone(),
            runs: Some((spans.len() - open.len(), spans.len())),
            incomplete,
        }
    }

    /// The figure is incomplete because the session is still running: listed
    /// by agent-status, or an executor with a dispatch that has not ended.
    fn running(&self) -> bool {
        self.incomplete.is_some()
            && (self.fresh == Liveness::Live
                || self
                    .runs
                    .is_some_and(|(ended, dispatches)| ended < dispatches))
    }

    fn json(&self) -> Value {
        let mut value = json!({"answer": self.fresh.answer(), "reason": self.fresh.reason(),
            "source": "agent-status", "retained": self.retained,
            "final": self.incomplete.is_none()});
        if let Some((ended, dispatches)) = self.runs {
            value["runs"] = json!({"ended": ended, "dispatches": dispatches});
        }
        value
    }
}

/// A session no dispatch binds, measured because its ledger events act on
/// the selected work.
#[derive(Clone)]
struct Participation {
    roles: BTreeSet<Role>,
    /// The ledger read had no gap, so the membership names every change the
    /// session acted on. Otherwise a record that could name another one is
    /// unreadable and the session is never counted as the work's alone.
    membership_complete: bool,
}

/// Whether a session's figure is divided among the work its spans cover.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Split {
    /// The ledger gives the session one span, so its figure is not divided.
    Whole,
    /// Read per segment of its timeline.
    Divided,
    /// Several spans, but no segment figures to divide, for this reason.
    Unsplit(&'static str),
}

#[derive(Clone)]
struct SessionRow {
    harness: String,
    session: String,
    spans: Vec<Span>,
    membership: BTreeSet<WorkKey>,
    measured: Measured,
    split: Split,
    overlap: BTreeSet<String>,
    participation: Option<Participation>,
    /// The roles its own events and those recorded on its behalf give it on
    /// the selected work, beside the roles its bindings name.
    acted: BTreeSet<Role>,
    liveness: Option<SessionLiveness>,
    /// The model coordinate the row is attributed to, and where it came from.
    agent: Option<(String, &'static str)>,
    /// A coordinate a source gave the row that names another harness.
    agent_conflict: Option<(String, &'static str)>,
    /// The session that spawned this executor as its native child, as its
    /// bindings name it.
    parent: Option<String>,
}

impl SessionRow {
    /// The first source that names one coordinate: the bound dispatches'
    /// routes, the models the session's own events carry and its subjects
    /// declare, then the model its
    /// recording says served it. Several distinct coordinates in one source
    /// establish none there, and neither does a coordinate whose prefix names
    /// a harness other than the row's: the first such one is kept as the
    /// row's conflict.
    fn attribute(&mut self, person: Option<&Person>) {
        let one = |coordinates: BTreeSet<String>| {
            let mut coordinates = coordinates.into_iter();
            match (coordinates.next(), coordinates.next()) {
                (Some(coordinate), None) => Some(coordinate),
                _ => None,
            }
        };
        let routes = one(self
            .spans
            .iter()
            .filter_map(|span| span.route.clone())
            .collect())
        .filter(|_| self.spans.iter().all(|span| span.route.is_some()));
        let events = one(person
            .map(|person| person.coordinates.clone())
            .unwrap_or_default());
        let recording = self
            .measured
            .facts
            .as_ref()
            .and_then(|facts| facts.served_model.clone());
        // A native child is attributed by its own model evidence first: the
        // lead's route names what it asked for, the recording what served.
        let sources = if self.parent.is_some() {
            [
                (recording, "recording"),
                (routes, "route"),
                (events, "events"),
            ]
        } else {
            [
                (routes, "route"),
                (events, "events"),
                (recording, "recording"),
            ]
        };
        self.agent = None;
        self.agent_conflict = None;
        for (coordinate, source) in sources {
            let Some(coordinate) =
                coordinate.map(|coordinate| on_harness(&self.harness, coordinate))
            else {
                continue;
            };
            if names_harness(&coordinate) {
                self.agent_conflict.get_or_insert((coordinate, source));
                continue;
            }
            self.agent = Some((coordinate, source));
            break;
        }
    }

    /// `harness:model#effort`, or the harness alone when no model is
    /// established, then any coordinate naming another harness beside it:
    /// `claude (route codex:gpt-6.1-sol#high)`.
    fn agent_label(&self) -> String {
        let label = match &self.agent {
            Some((coordinate, _)) => format!("{}:{coordinate}", self.harness),
            None => self.harness.clone(),
        };
        match &self.agent_conflict {
            Some((coordinate, source)) => format!("{label} ({source} {coordinate})"),
            None => label,
        }
    }

    /// The roles its bindings name and the roles its events give it, by one
    /// rule for an executor and a participant alike.
    fn roles(&self) -> Vec<&'static str> {
        let mut roles: BTreeSet<&'static str> = self
            .spans
            .iter()
            .map(|span| executor_role(span.role.as_deref()))
            .collect();
        roles.extend(self.acted.iter().map(|role| role.name()));
        if roles.is_empty() {
            vec!["implementer"]
        } else {
            roles.into_iter().collect()
        }
    }
}

impl SessionRow {
    fn key(&self) -> String {
        format!("{}:{}", self.harness, self.session)
    }
}

/// The role a binding names, as the view spells it.
fn executor_role(role: Option<&str>) -> &'static str {
    match role {
        Some("reviewer") => "reviewer",
        _ => "implementer",
    }
}

/// A selected run: the work it is for, the executor model its route names,
/// the role its binding names, and the sessions its binding names.
pub(super) struct ExecutorRun {
    pub work: String,
    pub model: Option<String>,
    pub role: &'static str,
    pub sessions: Vec<String>,
}

/// A session whose ledger events act on the selected work, bound or not: its
/// kind, the roles its events give it, the models its own events, or the
/// subject recorded on its behalf, declared in each, the model its recording
/// says served it, and why it has no figure when it has none.
pub(super) struct Acting {
    pub key: String,
    /// `executor` when a dispatch binds it, else `participant`.
    pub kind: &'static str,
    pub roles: BTreeSet<&'static str>,
    pub models: BTreeMap<&'static str, BTreeSet<String>>,
    /// The served model, effort and a prefix naming its harness dropped.
    pub served: Option<String>,
    pub unmeasured: Option<String>,
}

/// What one read of a target yields: the `arc-cost/1` document, and the
/// facts that map a role to the sessions behind it.
pub(super) struct CostView {
    pub report: Value,
    pub runs: Vec<ExecutorRun>,
    pub acting: Vec<Acting>,
}

/// Print the read-only cost view of a change or a journal artifact.
pub fn show_cost(ctx: &Ctx, query: &CostQuery, json: bool, detail: bool) -> Result<()> {
    let view = read_query(ctx, query)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&view.report)?);
    } else if detail {
        print!("{}", report_text(&view.report));
    } else {
        print!("{}", super::cost_table::table_text(&view.report));
    }
    Ok(())
}

/// Read the cost view of a change or a journal artifact.
pub(super) fn read_cost(
    ctx: &Ctx,
    target: &str,
    prices: Option<&std::path::Path>,
) -> Result<CostView> {
    let mut query = CostQuery::target(target);
    query.prices = prices.map(std::path::Path::to_path_buf);
    read_query(ctx, &query)
}

/// Read the cost view of the work a query selects.
fn read_query(ctx: &Ctx, query: &CostQuery) -> Result<CostView> {
    let prices = agent_tapes_core::pricing::lookup(query.prices.as_deref())?;
    let store = ctx.store()?;
    let mut read = CostRead::new(&store);
    let events = read.events(Store::REPOSITORY_SCOPE);
    let backends = std::cell::OnceCell::new();
    let subjects = Subjects::read(&mut read, &events, &backends);
    let mut selection = select_query(ctx, &mut read, query, &events, &subjects)?;
    qualify_ledger(&mut selection, &read.gaps);
    let assigned = assign(ctx, &mut selection, &events);
    if !selection.readable && assigned.runs.is_empty() && assigned.sessions.is_empty() {
        bail!("cost target has no readable metadata");
    }
    let ledger = read_ledger(&mut read, &selection, &events, &assigned, &subjects);
    let Assigned {
        runs,
        sessions,
        bound,
        ..
    } = assigned;
    let plans = span_plans(&sessions, &ledger);
    let mut fresh = read_all(
        sessions
            .keys()
            .chain(ledger.participants().map(|(key, _)| key))
            .filter(|key| !subjects.unlocated.contains(*key))
            .cloned()
            .collect(),
        &plans,
        query.idle_ms(),
    );
    let empty = Plan::default();
    let plan_of = |harness: &str, session: &str| {
        plans
            .get(&(harness.to_string(), session.to_string()))
            .unwrap_or(&empty)
    };
    let mut fresh_read = |harness: &str, session: &str| {
        fresh
            .remove(&(harness.to_string(), session.to_string()))
            .unwrap_or_else(|| {
                read_fresh(
                    backends.get_or_init(Backends::new),
                    harness,
                    session,
                    plan_of(harness, session),
                    query.idle_ms(),
                )
            })
    };
    let snapshot = std::cell::OnceCell::new();
    let mut sessions: Vec<SessionRow> = sessions
        .into_iter()
        .map(|((harness, session), (spans, membership))| {
            let mut measured = measure(
                &events,
                &harness,
                &session,
                fresh_read(&harness, &session),
                query.idle_ms(),
            );
            let liveness = SessionLiveness::executor(
                &measured,
                snapshot.get_or_init(Snapshot::read).of(&harness, &session),
                &spans,
                ledger.complete,
            );
            if let Some(facts) = measured.facts.as_mut() {
                facts.incomplete.extend(liveness.incomplete.clone());
            }
            let split = split_of(&measured, plan_of(&harness, &session), true);
            let parent = spans.iter().find_map(|span| span.parent.clone());
            let acted = ledger
                .people
                .get(&(harness.clone(), session.clone()))
                .map(|person| person.roles.clone())
                .unwrap_or_default();
            SessionRow {
                harness,
                session,
                spans,
                membership,
                measured,
                split,
                overlap: BTreeSet::new(),
                participation: None,
                acted,
                liveness: Some(liveness),
                agent: None,
                agent_conflict: None,
                parent,
            }
        })
        .collect();
    // A participant whose recording cannot be read is still a row: its
    // missing figure qualifies every total it would have joined.
    let mut unmeasured = BTreeMap::new();
    for ((harness, session), person) in ledger.participants() {
        let fresh = if subjects
            .unlocated
            .contains(&(harness.clone(), session.clone()))
        {
            Err(NOT_LOCATED.to_string())
        } else {
            fresh_read(harness, session)
        };
        let mut measured = measure(&events, harness, session, fresh, query.idle_ms());
        let liveness = SessionLiveness::participant(
            &measured,
            snapshot.get_or_init(Snapshot::read).of(harness, session),
        );
        match (measured.facts.as_mut(), &measured.source) {
            (Some(facts), _) => facts.incomplete.extend(liveness.incomplete.clone()),
            (None, Source::Unavailable(reason)) => {
                unmeasured.insert((harness.clone(), session.clone()), reason.clone());
            }
            (None, _) => {
                unmeasured.insert((harness.clone(), session.clone()), READ_FAILED.to_string());
            }
        }
        let split = match split_of(&measured, plan_of(harness, session), ledger.complete) {
            Split::Whole if person.spanless() => Split::Unsplit(span::ON_BEHALF),
            split => split,
        };
        sessions.push(SessionRow {
            harness: harness.clone(),
            session: session.clone(),
            spans: Vec::new(),
            membership: person.membership.clone(),
            measured,
            split,
            overlap: BTreeSet::new(),
            participation: Some(Participation {
                roles: person.roles.clone(),
                membership_complete: ledger.complete,
            }),
            acted: person.roles.clone(),
            liveness: Some(liveness),
            agent: None,
            agent_conflict: None,
            parent: None,
        });
    }
    separate_family_parents(&mut sessions, &mut unmeasured);
    for row in &mut sessions {
        if let Some(facts) = row.measured.facts.as_mut() {
            price(facts, prices.table());
        }
    }
    let groups = lineage_groups(&sessions);
    for row in &mut sessions {
        row.overlap = groups
            .iter()
            .find(|group| group.contains(&row.key()))
            .cloned()
            .unwrap_or_default();
    }
    for row in &mut sessions {
        let person = ledger
            .people
            .get(&(row.harness.clone(), row.session.clone()));
        row.attribute(person);
    }
    let outside = outside_sessions(&ledger, &sessions, &unmeasured);
    selection.ledger_gaps = read.gaps;
    let mut children = read_all(
        unbound_spawned(&sessions, &bound),
        &BTreeMap::new(),
        query.idle_ms(),
    );
    for facts in children.values_mut().flatten() {
        price(facts, prices.table());
    }
    let view = View {
        by: &query.by,
        prices: &prices,
        allocation: query.allocation,
        idle_ms: query.idle_ms(),
        children: &children,
        bound: &bound,
    };
    let mut report = render_report(&selection, &runs, &sessions, &outside, &ledger, &view);
    if let Some(footprint) = &query.footprint {
        report["footprint"] = footprint_figure(&report, footprint);
    }
    let executor_runs = runs
        .iter()
        .flat_map(|(work, runs)| {
            runs.iter().map(|run| ExecutorRun {
                work: work.label(),
                model: run.model.clone(),
                role: run.role,
                sessions: if run.conflicting {
                    Vec::new()
                } else {
                    run.candidates.clone()
                },
            })
        })
        .collect();
    let acting = ledger
        .people
        .iter()
        .filter(|(_, person)| person.acts)
        .filter_map(|((harness, session), person)| {
            let key = format!("{harness}:{session}");
            let row = sessions.iter().find(|row| row.key() == key)?;
            let served = row
                .measured
                .facts
                .as_ref()
                .and_then(|facts| facts.served_model.as_deref())
                .and_then(model_of)
                .map(|model| on_harness(harness, model));
            Some(Acting {
                kind: if person.bound {
                    "executor"
                } else {
                    "participant"
                },
                roles: person.roles.iter().map(|role| role.name()).collect(),
                served,
                models: person
                    .models
                    .iter()
                    .map(|(role, models)| (role.name(), models.clone()))
                    .collect(),
                unmeasured: unmeasured.get(&(harness.clone(), session.clone())).cloned(),
                key,
            })
        })
        .collect();
    Ok(CostView {
        report,
        runs: executor_runs,
        acting,
    })
}

/// The spawned children each row's recording names under their own session
/// ids that no dispatch binds: they are listed under their parent, outside
/// every total, and read for that listing alone.
fn unbound_spawned(
    sessions: &[SessionRow],
    bound: &BTreeSet<(String, String)>,
) -> BTreeSet<(String, String)> {
    sessions
        .iter()
        .filter_map(|row| row.measured.facts.as_ref()?.lineage.as_ref())
        .flat_map(|lineage| &lineage.children)
        .map(|child| (child.harness.clone(), child.session.clone()))
        .filter(|key| !bound.contains(key))
        .collect()
}

/// A figure of `counters` alone, in the request basis of `base`, the facts
/// of the session whose recording family holds them.
fn counted_facts(
    base: &CostFacts,
    harness: &str,
    counters: &CostCounters,
    complete: bool,
) -> CostFacts {
    let mut facts = base.clone();
    facts.spans = None;
    facts.residual = None;
    facts.events = None;
    facts.lineage = Some(CostLineage::default());
    facts.list_price = None;
    // The child's requests and activity are not its parent's; only the
    // billing of the recording family is shared.
    facts.request_terms = CostRequestTerms {
        billing: base.request_terms.billing.clone(),
        ..CostRequestTerms::default()
    };
    facts.by_model = Vec::new();
    facts.served_model = None;
    facts.attribution_uncertain = false;
    facts.cost_coverage = None;
    facts.harness_estimate_usd = None;
    facts.basis = Some("summed-requests".to_string());
    facts.coverage = Some("session".to_string());
    facts.counters = counters.clone();
    let tokens = agent_tapes_core::model::Tokens {
        input: counters.input,
        output: counters.output,
        reasoning: counters.reasoning,
        cache_read: counters.cache_read,
        cache_write: counters.cache_write,
    };
    facts.disjoint = agent_tapes_core::model::TokenConvention::of(harness)
        .map(|convention| {
            agent_tapes_core::model::DisjointTokens::from_native(&tokens, convention, None)
        })
        .as_ref()
        .and_then(cost_disjoint);
    let mut gaps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for category in CATEGORIES {
        if counter(counters, category).is_none() {
            gaps.entry(category.to_string())
                .or_default()
                .push("recorded category is absent".to_string());
        }
    }
    gaps.entry("usd".to_string())
        .or_default()
        .push("recorded category is absent".to_string());
    facts.quantity_gaps = Some(gaps);
    facts.incomplete = if complete {
        Vec::new()
    } else {
        vec![CHILD_SOURCE_INCOMPLETE.to_string()]
    };
    facts
}

/// A child listed under `parent` with `facts` as its figure, under its own
/// identity so no total reads it as the parent counted again.
fn child_row(parent: &SessionRow, harness: &str, session: &str, facts: CostFacts) -> SessionRow {
    let mut child = parent.with_facts(facts);
    child.harness = harness.to_string();
    child.session = session.to_string();
    child.spans = Vec::new();
    child.membership = BTreeSet::new();
    child.split = Split::Whole;
    child.overlap = BTreeSet::new();
    child.participation = None;
    child.acted = BTreeSet::new();
    child.liveness = None;
    child.agent = None;
    child.agent_conflict = None;
    child.parent = Some(parent.session.clone());
    child
}

/// A child listed under `parent` that has no figure, for `reason`: a total
/// holding it has no amount beside its known subtotal.
fn unmeasured_child(parent: &SessionRow, harness: &str, session: &str, reason: &str) -> SessionRow {
    let mut child = parent.clone();
    child.harness = harness.to_string();
    child.session = session.to_string();
    child.spans = Vec::new();
    child.membership = BTreeSet::new();
    child.measured = Measured {
        source: Source::Unavailable(reason.to_string()),
        facts: None,
        conflict: None,
        retained_liveness: None,
        retained_at: None,
    };
    child.split = Split::Unsplit(span::UNMEASURED);
    child.overlap = BTreeSet::new();
    child.participation = None;
    child.acted = BTreeSet::new();
    child.liveness = None;
    child.agent = None;
    child.agent_conflict = None;
    child.parent = Some(parent.session.clone());
    child
}

/// Why a child recording's figure in its family is a known subtotal: tapes
/// read a record it could not count, or a request that failed.
const CHILD_SOURCE_INCOMPLETE: &str = "the child recording holds records its requests do not count";

/// A parent's native family: each child its recording names, bound to a
/// row or listed with the consumption no binding assigns, outside every
/// total; and for a Claude parent, the recorded total reconciled with the
/// requests of the whole family, and whether the parent's own requests and
/// each child's add up to that union.
fn family_json(row: &SessionRow, sessions: &[SessionRow], view: &View<'_>) -> Option<Value> {
    let table = view.prices.table();
    let facts = row.measured.facts.as_ref();
    let lineage = facts.and_then(|facts| facts.lineage.as_ref());
    let bound: Vec<&SessionRow> = sessions
        .iter()
        .filter(|child| {
            child.harness == row.harness && child.parent.as_deref() == Some(&row.session)
        })
        .collect();
    // A child any dispatch binds is assigned, to this view's work or to
    // work it does not select; only a child no binding names is unassigned.
    let assigned = |harness: &str, session: &str| {
        view.bound
            .contains(&(harness.to_string(), session.to_string()))
    };
    let natives: Vec<&CostNativeChild> = lineage
        .map(|lineage| {
            lineage
                .native_children
                .iter()
                .filter(|child| !child.mirror())
                .collect()
        })
        .unwrap_or_default();
    let spawned: Vec<&CostRelative> = lineage
        .map(|lineage| lineage.children.iter().collect())
        .unwrap_or_default();
    if bound.is_empty() && natives.is_empty() && spawned.is_empty() {
        return None;
    }
    let residual = facts
        .and_then(|facts| facts.residual.as_ref())
        .filter(|residual| residual.unavailable.is_none());
    let source_of = |agent: &str| {
        residual.and_then(|residual| {
            residual
                .sources
                .iter()
                .find(|source| source.role == "child" && source.agent.as_deref() == Some(agent))
        })
    };
    let scope = |what: &str| format!("session {} {what}", row.key());
    let mut children = Vec::new();
    let mut unassigned: Vec<SessionRow> = Vec::new();
    let mut unmeasured = 0usize;
    for child in &natives {
        let session = format!("{}::{}", row.session, child.agent);
        let key = format!("{}:{session}", row.harness);
        let held = assigned(&row.harness, &session);
        let figure = facts
            .zip(source_of(&child.agent))
            .and_then(|(facts, source)| {
                let counters = source.identified.as_ref()?;
                let mut figure = counted_facts(facts, &row.harness, counters, source.complete);
                price(&mut figure, table);
                Some((
                    child_row(row, &row.harness, &session, figure),
                    source.requests,
                ))
            });
        let recording = if child.resolved { "present" } else { "missing" };
        let mut entry = json!({"agent": child.agent, "key": key, "role": child.role,
            "recording": recording, "bound": held, "row": bound.iter().any(|row| row.key() == key)});
        let reason = if child.resolved {
            "the family reconciliation holds no figure for it"
        } else {
            CHILD_MISSING
        };
        match &figure {
            Some((figure, requests)) => {
                entry["requests"] = json!(requests);
                entry["totals"] = totals(&[figure], &[], &scope(&format!("child {}", child.agent)));
            }
            None => entry["reason"] = json!(reason),
        }
        if !held {
            match figure {
                Some((figure, _)) => unassigned.push(figure),
                None => {
                    unassigned.push(unmeasured_child(row, &row.harness, &session, reason));
                    unmeasured += 1;
                }
            }
        }
        children.push(entry);
    }
    for child in &spawned {
        let key = format!("{}:{}", child.harness, child.session);
        let held = assigned(&child.harness, &child.session);
        let mut entry = json!({"session": child.session, "key": key, "bound": held,
            "row": bound.iter().any(|row| row.key() == key)});
        if !held {
            match view
                .children
                .get(&(child.harness.clone(), child.session.clone()))
            {
                Some(Ok(facts)) => {
                    let figure = child_row(row, &child.harness, &child.session, facts.clone());
                    entry["totals"] =
                        totals(&[&figure], &[], &scope(&format!("child {}", child.session)));
                    unassigned.push(figure);
                }
                Some(Err(reason)) => {
                    entry["reason"] = json!(reason);
                    unassigned.push(unmeasured_child(
                        row,
                        &child.harness,
                        &child.session,
                        reason,
                    ));
                    unmeasured += 1;
                }
                None => {
                    entry["reason"] = json!(READ_FAILED);
                    unassigned.push(unmeasured_child(
                        row,
                        &child.harness,
                        &child.session,
                        READ_FAILED,
                    ));
                    unmeasured += 1;
                }
            }
        }
        children.push(entry);
    }
    let held: Vec<&SessionRow> = unassigned.iter().collect();
    let mut value = json!({
        "statement": "a parent's figure is its own requests; each child is measured from its own recording, as an executor row where a binding names it, and otherwise listed here outside every total",
        "bound": bound.iter().map(|child| child.key()).collect::<Vec<_>>(),
        "children": children,
        "unassigned": {"children": unassigned.len(), "unmeasured": unmeasured,
            "totals": totals(&held, &[], &scope("unassigned children"))},
    });
    if row.harness == "claude" {
        value["reconciliation"] =
            family_reconciliation(facts.and_then(|facts| facts.residual.as_ref()));
    }
    Some(value)
}

/// The recorded total against the family's identified requests, and whether
/// the parent's own requests and every child's add up to that union. The
/// residual, the recorded total less that union, is the parent's and is no
/// child's; it is shown, never added to a total.
fn family_reconciliation(residual: Option<&CostResidual>) -> Value {
    let Some(residual) = residual else {
        return json!({"status": "unavailable", "reason": "reconciliation was not read"});
    };
    if let Some(reason) = &residual.unavailable {
        return json!({"status": "unavailable", "reason": reason});
    }
    let mut differs = Vec::new();
    let mut unestablished = residual.identity_coverage.as_deref() != Some("validated-family");
    for row in &residual.counters {
        // A counter no recording in the family records has nothing to add.
        let recorded = residual.sources.iter().any(|source| {
            source
                .identified
                .as_ref()
                .is_some_and(|counters| counter(counters, &row.counter).is_some())
        });
        if !recorded && row.identified.is_none() {
            continue;
        }
        let parts: Option<u64> = residual.sources.iter().try_fold(0u64, |sum, source| {
            let counters = source.identified.as_ref();
            match counters.and_then(|counters| counter(counters, &row.counter)) {
                Some(amount) => sum.checked_add(amount),
                None if source.requests == 0 => Some(sum),
                None => None,
            }
        });
        match (parts, row.identified) {
            (Some(parts), Some(union)) if parts == union => {}
            (Some(_), Some(_)) => differs.push(row.counter.clone()),
            _ => unestablished = true,
        }
    }
    let status = if unestablished {
        "unestablished"
    } else if differs.is_empty() {
        "conserved"
    } else {
        "not-conserved"
    };
    json!({
        "status": status,
        "statement": "the parent's own requests and each child's add up to the family's identified union per counter unless `differs` names one, as a request two recordings both hold leaves it; the residual is the recorded total less that union, the parent's own, and joins no total",
        "differs": differs,
        "residual": residual.counters,
        "scope_established": residual.scope_established,
    })
}

/// Why a parent session has no figure in a view that measures its native
/// children apart: the only figure held is a recorded total, which covers
/// those children too, so counting it beside them would count them twice.
const PARENT_TOTAL_COVERS_CHILDREN: &str = "recorded total covers its native children";

/// A Claude parent whose native children the view measures apart, or whose
/// recording names children with consumption of their own, counts only its
/// own requests. Held facts in the recorded-total basis cannot be separated,
/// so the parent is unmeasured rather than counted with its children twice.
fn separate_family_parents(
    sessions: &mut [SessionRow],
    unmeasured: &mut BTreeMap<(String, String), String>,
) {
    let parents: BTreeSet<(String, String)> = sessions
        .iter()
        .filter_map(|row| Some((row.harness.clone(), row.parent.clone()?)))
        .collect();
    for row in sessions.iter_mut().filter(|row| row.harness == "claude") {
        let Some(facts) = row.measured.facts.as_ref() else {
            continue;
        };
        let spawned = facts
            .lineage
            .as_ref()
            .is_some_and(|lineage| lineage.native_children.iter().any(|child| !child.mirror()));
        let bound = parents.contains(&(row.harness.clone(), row.session.clone()));
        if (spawned || bound) && facts.basis.as_deref() == Some("recorded-total") {
            row.measured.facts = None;
            row.measured.source = Source::Unavailable(PARENT_TOTAL_COVERS_CHILDREN.to_string());
            row.split = Split::Unsplit(span::UNMEASURED);
            if row.participation.is_some() {
                unmeasured.insert(
                    (row.harness.clone(), row.session.clone()),
                    PARENT_TOTAL_COVERS_CHILDREN.to_string(),
                );
            }
        }
    }
}

/// Retain the normalized facts a fresh read of each bound or participating
/// session gives. Facts identical to ones already retained for the session
/// record nothing.
pub fn retain_cost(ctx: &Ctx, target: &str) -> Result<()> {
    let store = ctx.store()?;
    let mut read = CostRead::new(&store);
    let events = read.events(Store::REPOSITORY_SCOPE);
    let subjects = Subjects::read(&mut read, &events, &std::cell::OnceCell::new());
    let mut selection = select(ctx, &mut read, target, &events)?;
    qualify_ledger(&mut selection, &read.gaps);
    let assigned = assign(ctx, &mut selection, &events);
    if !selection.readable && assigned.runs.is_empty() && assigned.sessions.is_empty() {
        bail!("cost target has no readable metadata");
    }
    let ledger = read_ledger(&mut read, &selection, &events, &assigned, &subjects);
    print_read_gaps(&selection.gaps, &read.gaps);
    let plans = span_plans(&assigned.sessions, &ledger);
    let mut keys: BTreeSet<(String, String)> = assigned.sessions.into_keys().collect();
    keys.extend(ledger.participants().map(|(key, _)| key.clone()));
    if keys.is_empty() {
        println!("no bound or participating sessions");
        return Ok(());
    }
    // Read every source before taking the ledger lock: a recording can be
    // large, and other writers wait on the lock.
    let snapshot = Snapshot::read();
    let (unlocated, keys): (BTreeSet<_>, BTreeSet<_>) = keys
        .into_iter()
        .partition(|key| subjects.unlocated.contains(key));
    let mut reads = read_all(keys, &plans, DEFAULT_IDLE_MS);
    reads.extend(
        unlocated
            .into_iter()
            .map(|key| (key, Err(NOT_LOCATED.to_string()))),
    );
    let reads: Vec<(String, String, std::result::Result<CostFacts, String>)> = reads
        .into_iter()
        .map(|((harness, session), read)| {
            let read = read.map(|mut facts| {
                facts.liveness = Some(snapshot.of(&harness, &session).answer().to_string());
                facts
            });
            (harness, session, read)
        })
        .collect();
    // Under the lock, only records published since the first read are
    // opened, so a retention another writer just made is seen.
    let _repository_events = store.lock_repository_events()?;
    let known_gaps = read.gaps.len();
    let events = read.refreshed(Store::REPOSITORY_SCOPE);
    print_read_gaps(&[], &read.gaps[known_gaps..]);
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
                facts: Box::new(facts),
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

/// What a cost view reads: named targets and range selectors, whose union
/// is the selected work, and how its table groups rows.
#[derive(Default)]
pub struct CostQuery {
    /// Change ids or prefixes and journal artifacts.
    pub targets: Vec<String>,
    /// Every change carrying all of these tags.
    pub tags: Vec<String>,
    /// Every change and journal item this `harness:id` session was bound to
    /// or acted on.
    pub footprint: Option<String>,
    /// Every change with a ledger event at or after this instant.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// Every change with a ledger event at or before this instant.
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    /// Every change in this ledger.
    pub all: bool,
    /// Table groupings, in order: agent, role, model, harness, change or
    /// session.
    pub by: Vec<String>,
    /// How what no item holds is allocated beside the conserving figures.
    pub allocation: Option<Allocation>,
    /// The idle gap that ends a participant's working stretch, in
    /// milliseconds; [`DEFAULT_IDLE_MS`] when absent.
    pub idle: Option<u64>,
    /// The price table `--prices` names.
    pub prices: Option<std::path::PathBuf>,
}

impl CostQuery {
    fn idle_ms(&self) -> u64 {
        self.idle.unwrap_or(DEFAULT_IDLE_MS)
    }

    pub fn target(target: &str) -> Self {
        Self {
            targets: vec![target.to_string()],
            ..Self::default()
        }
    }

    fn ranged(&self) -> bool {
        self.targets.len() != 1
            || !self.tags.is_empty()
            || self.footprint.is_some()
            || self.since.is_some()
            || self.until.is_some()
            || self.all
    }

    fn selectors(&self) -> Value {
        json!({"targets": self.targets, "tags": self.tags, "footprint": self.footprint,
            "since": self.since, "until": self.until, "all": self.all})
    }
}

/// A `harness:id` footprint selector, split at its first colon. The id is a
/// session id, or a native child's `<parent>::<agent>` key.
fn footprint_key(raw: &str) -> Result<(String, String)> {
    let plain_or_child = |session: &str| match session.split_once("::") {
        Some((parent, agent)) => plain_session_id(parent) && plain_session_id(agent),
        None => plain_session_id(session),
    };
    match raw.split_once(':') {
        Some((harness, session)) if plain_session_id(harness) && plain_or_child(session) => {
            Ok((harness.to_string(), session.to_string()))
        }
        _ => bail!(
            "--footprint takes harness:session, such as claude:<session id>, or a native child's claude:<parent>::<agent>"
        ),
    }
}

/// The targets every range selector names, in a stable order and each once:
/// explicit targets first, then the changes and journal items the selectors
/// add. A change whose state cannot be read for a tag is a selection gap.
fn range_targets(
    read: &mut CostRead<'_>,
    query: &CostQuery,
    events: &[Event],
    subjects: &Subjects,
    gaps: &mut Vec<Value>,
) -> Result<Vec<String>> {
    let mut targets: Vec<String> = query.targets.clone();
    let ids = read.change_ids();
    let in_window = |at: &chrono::DateTime<chrono::Utc>| {
        query.since.is_none_or(|since| *at >= since) && query.until.is_none_or(|until| *at <= until)
    };
    let windowed = query.since.is_some() || query.until.is_some();
    let footprint = query.footprint.as_deref().map(footprint_key).transpose()?;
    let names = |event: &Event, key: &(String, String)| {
        named_sessions(event, subjects)
            .iter()
            .any(|named| &named.key == key)
    };
    let mut child_bound = false;
    for round in rounds(events) {
        let subject = match &round.subject {
            Some(RunSubject::Change { id }) => id.clone(),
            Some(RunSubject::Journal { file }) => file.clone(),
            _ => continue,
        };
        let windowed_in = windowed
            && std::iter::once(round.dispatch.created_at)
                .chain(round.ending.as_ref().map(|ending| ending.event.created_at))
                .any(|at| in_window(&at))
            && matches!(round.subject, Some(RunSubject::Change { .. }));
        let bound = footprint.as_ref().is_some_and(|key| {
            names(round.dispatch, key)
                || round.binding.iter().any(|binding| {
                    std::iter::once(binding)
                        .chain(&binding.conflicts)
                        .any(|binding| {
                            let session = match binding.parent_session {
                                Some(parent) => super::native_child::child_key(
                                    binding.executor_harness,
                                    parent,
                                    binding.executor_session,
                                ),
                                None => binding.executor_session.clone(),
                            };
                            binding.executor_harness == &key.0 && session == key.1
                        })
                })
        });
        child_bound |= bound;
        if windowed_in || bound {
            targets.push(subject);
        }
    }
    if let Some((harness, session)) = footprint
        .as_ref()
        .filter(|(_, session)| session.contains("::"))
    {
        if !child_bound {
            bail!("--footprint {harness}:{session} names a native child no binding records");
        }
    }
    for id in &ids {
        let change_events = read.events(id);
        let tagged = !query.tags.is_empty()
            && match crate::state::reduce(&change_events) {
                Ok(state) => query.tags.iter().all(|tag| state.tags.contains(tag)),
                Err(_) => {
                    gaps.push(json!({"record": id, "reason": "change state unreadable, so its tags are unknown", "work": []}));
                    false
                }
            };
        let windowed_in = windowed
            && change_events
                .iter()
                .any(|event| in_window(&event.created_at));
        let acted = footprint
            .as_ref()
            .is_some_and(|key| change_events.iter().any(|event| names(event, key)));
        if query.all || tagged || windowed_in || acted {
            targets.push(id.clone());
        }
    }
    let mut seen = BTreeSet::new();
    targets.retain(|target| seen.insert(target.clone()));
    Ok(targets)
}

/// The union of every target's selection: each work item once, every gap,
/// and the selectors that named them.
fn select_query(
    ctx: &Ctx,
    read: &mut CostRead<'_>,
    query: &CostQuery,
    events: &[Event],
    subjects: &Subjects,
) -> Result<Selection> {
    if !query.ranged() {
        return select(ctx, read, &query.targets[0], events);
    }
    let mut gaps = Vec::new();
    let targets = range_targets(read, query, events, subjects, &mut gaps)?;
    if targets.is_empty() {
        bail!("the cost range selects no work");
    }
    let mut items: Vec<WorkItem> = Vec::new();
    let mut named = Vec::new();
    let mut readable = false;
    for target in &targets {
        let selection = select(ctx, read, target, events)?;
        readable |= selection.readable;
        named.push(selection.target);
        for gap in selection.gaps {
            if !gaps.contains(&gap) {
                gaps.push(gap);
            }
        }
        for item in selection.items {
            match items.iter_mut().find(|held| held.key == item.key) {
                Some(held) => {
                    if held.source.is_none() {
                        held.source = item.source;
                    }
                }
                None => items.push(item),
            }
        }
    }
    let changes = items
        .iter()
        .filter(|item| matches!(item.key, WorkKey::Change(_)))
        .count();
    Ok(Selection {
        target: json!({"kind": "range", "selectors": query.selectors(), "targets": named,
            "changes": changes, "journals": items.len() - changes}),
        items,
        gaps,
        ledger_gaps: Vec::new(),
        readable,
    })
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
    role: &'static str,
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
/// a span on unselected work divides the session beyond the view. A run
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
                binding.parent_session,
            )
        });
        let readable: Vec<_> = bindings
            .iter()
            .filter(|binding| {
                plain_session_id(binding.executor_harness)
                    && plain_session_id(binding.executor_session)
                    && binding
                        .parent_session
                        .as_deref()
                        .is_none_or(plain_session_id)
            })
            .collect();
        // A native child is measured under the key its harness reads it by.
        let session_of = |binding: &&&super::run::RunBinding<'_>| match binding.parent_session {
            Some(parent) => super::native_child::child_key(
                binding.executor_harness,
                parent,
                binding.executor_session,
            ),
            None => binding.executor_session.clone(),
        };
        let candidates: BTreeSet<String> = readable
            .iter()
            .map(|binding| format!("{}:{}", binding.executor_harness, session_of(binding)))
            .collect();
        if selected.contains(&work) {
            let harnesses: BTreeSet<&String> = readable
                .iter()
                .map(|binding| binding.executor_harness)
                .collect();
            let model = match &round.dispatch.payload {
                Payload::RunDispatched { route, .. } => model_of(route),
                _ => None,
            }
            .map(|model| match harnesses.first() {
                Some(harness) if harnesses.len() == 1 => on_harness(harness, model),
                _ => model,
            });
            runs.entry(work.clone()).or_default().push(AssignedRun {
                dispatch_event_id: round.dispatch.event_id.clone(),
                model,
                role: executor_role(
                    round
                        .binding
                        .as_ref()
                        .and_then(|binding| binding.role.as_deref()),
                ),
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
        for binding in readable.iter() {
            let key = (binding.executor_harness.clone(), session_of(binding));
            bound.insert(key.clone());
            if !uncertain.is_empty() {
                possible_sessions.insert(key.clone());
            }
            let (spans, membership) = sessions.entry(key).or_default();
            membership.insert(work.clone());
            spans.push(Span {
                dispatch_event_id: round.dispatch.event_id.clone(),
                dispatched_at: round.dispatch.created_at,
                ended_at: round.ending.as_ref().map(|ending| ending.event.created_at),
                route: match &round.dispatch.payload {
                    Payload::RunDispatched { route, .. } => route_coordinate(route),
                    _ => None,
                },
                forked_from: binding
                    .forked_from
                    .as_ref()
                    .filter(|id| plain_session_id(id))
                    .cloned(),
                parent: binding.parent_session.clone(),
                role: binding.role.clone(),
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
            } if executor_harness == harness && executor_session == session => {
                Some((event, &**facts))
            }
            _ => None,
        })
        .collect()
}

/// A fresh read when the source answers, else the retained facts at the
/// furthest frontier. A fresh read that disagrees with facts retained at the
/// same frontier carries the conflict; neither is added to the other.
/// Read every session's source, several at once: each read is independent,
/// and a lineage read can scan a whole harness store.
fn read_all(
    keys: BTreeSet<(String, String)>,
    plans: &BTreeMap<(String, String), Plan>,
    idle_ms: u64,
) -> BTreeMap<(String, String), std::result::Result<CostFacts, String>> {
    let keys: Vec<(String, String)> = keys.into_iter().collect();
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .clamp(1, 8);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let reads = std::sync::Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..workers.min(keys.len()) {
            scope.spawn(|| {
                let backends = Backends::new();
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((harness, session)) = keys.get(index) else {
                        break;
                    };
                    let empty = Plan::default();
                    let plan = plans
                        .get(&(harness.clone(), session.clone()))
                        .unwrap_or(&empty);
                    let read = read_fresh(&backends, harness, session, plan, idle_ms);
                    if let Ok(mut reads) = reads.lock() {
                        reads.insert((harness.clone(), session.clone()), read);
                    }
                }
            });
        }
    });
    reads.into_inner().unwrap_or_default()
}

fn measure(
    events: &[Event],
    harness: &str,
    session: &str,
    fresh: std::result::Result<CostFacts, String>,
    idle_ms: u64,
) -> Measured {
    // The furthest frontier, and among equal frontiers the latest retention.
    let retained = retained_for(events, harness, session)
        .into_iter()
        .max_by_key(|(_, facts)| facts.frontier)
        .map(|(event, facts)| (event, sanitized_facts(facts)));
    match fresh {
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
                retained_at: None,
            }
        }
        Err(reason) => match retained {
            Some((event, mut held)) => {
                at_idle(&mut held, idle_ms);
                Measured {
                    source: Source::Retained(event.event_id.clone(), reason),
                    retained_liveness: held.liveness.clone(),
                    retained_at: Some(event.created_at),
                    facts: Some(held),
                    conflict: None,
                }
            }
            None => Measured {
                source: Source::Unavailable(reason),
                facts: None,
                conflict: None,
                retained_liveness: None,
                retained_at: None,
            },
        },
    }
}

/// Retained facts as a view at `idle_ms` reads them: an estimate made at
/// another idle gap answers another question, so it is withheld and the
/// measured segments stand.
fn at_idle(facts: &mut CostFacts, idle_ms: u64) {
    let Some(spans) = facts.spans.as_mut() else {
        return;
    };
    let Some(estimate) = spans
        .estimate
        .as_mut()
        .filter(|estimate| estimate.idle_ms != idle_ms)
    else {
        return;
    };
    estimate.idle_ms = idle_ms;
    estimate
        .withheld
        .get_or_insert_with(|| span::RETAINED_IDLE.to_string());
    for segment in &mut spans.segments {
        segment.estimates.clear();
    }
}

/// Whether the measured facts divide the session along `plan`. A fresh read
/// was made along it; retained facts divide the session only when their
/// segments are the ones the ledger gives it now. A participant's spans are
/// known only while every ledger record is readable.
fn split_of(measured: &Measured, plan: &[Segment], membership_complete: bool) -> Split {
    if plan.is_empty() {
        return Split::Whole;
    }
    let Some(facts) = measured.facts.as_ref() else {
        return Split::Unsplit(span::UNMEASURED);
    };
    if !membership_complete {
        return Split::Unsplit(span::INCOMPLETE_MEMBERSHIP);
    }
    match facts.spans.as_ref() {
        Some(spans) if spans.unsplit.is_some() => Split::Unsplit(span::UNSUPPORTED),
        Some(spans)
            if spans.segments.len() == plan.len()
                && plan
                    .iter()
                    .zip(&spans.segments)
                    .all(|(segment, retained)| segment.defines(retained)) =>
        {
            Split::Divided
        }
        _ => Split::Unsplit(span::RETAINED_MISMATCH),
    }
}

/// Two reads measured the same thing, whichever reader made them and
/// whatever liveness each was taken under.
/// Segment figures are compared only when both reads cut the session alike.
fn same_measurement(left: &CostFacts, right: &CostFacts) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.reader.clone_from(&right.reader);
    left.liveness.clone_from(&right.liveness);
    let definitions = |facts: &CostFacts| {
        facts.spans.as_ref().map(|spans| {
            spans
                .segments
                .iter()
                .map(|segment| {
                    let (from, to, work, runs) = segment.bounds();
                    (from, to, work.to_vec(), runs.to_vec())
                })
                .collect::<Vec<_>>()
        })
    };
    if definitions(&left) != definitions(&right) {
        left.spans = None;
        right.spans = None;
    }
    // Estimates read at another idle gap estimate something else.
    if let (Some(left), Some(right)) = (left.spans.as_mut(), right.spans.as_mut()) {
        if left.estimate != right.estimate {
            for spans in [left, right] {
                spans.estimate = None;
                for segment in &mut spans.segments {
                    segment.estimates.clear();
                }
            }
        }
    }
    left == right
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

static BACKEND_SETS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many sets of tapes backends this process built for cost reads.
pub(crate) fn backend_sets() -> usize {
    BACKEND_SETS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The tapes backends one reader reads every session through, by harness.
/// Built once per reader, so a store's walk, its catalog and its lineage
/// index serve every session the reader reads; the sessions a held
/// backend lists are those its walk found.
struct Backends(BTreeMap<String, Vec<Box<dyn agent_tapes_core::backend::Backend>>>);

impl Backends {
    fn new() -> Self {
        BACKEND_SETS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut by_harness: BTreeMap<String, Vec<_>> = BTreeMap::new();
        for backend in agent_tapes_core::backend::backends() {
            by_harness
                .entry(backend.harness().to_string())
                .or_default()
                .push(backend);
        }
        Self(by_harness)
    }

    fn of(&self, harness: &str) -> &[Box<dyn agent_tapes_core::backend::Backend>] {
        self.0.get(harness).map_or(&[], Vec::as_slice)
    }
}

/// Why a tapes read gave no figure.
const READ_FAILED: &str = "usage read failed";

/// Why a tapes read gave no figure when its store stayed busy or
/// unavailable through every attempt tapes makes: reading again may
/// measure the session.
const READ_TRANSIENT: &str = "usage read failed transiently; a later read may succeed";

fn read_failed(transient: bool) -> String {
    if transient {
        READ_TRANSIENT
    } else {
        READ_FAILED
    }
    .to_string()
}

/// Measure one session through the pinned tapes reader, over its whole
/// recording, and per segment of `plan` when the ledger gives it several
/// spans. A Claude session that spawned native children is measured by its
/// own requests: its recorded total covers its children too, and they are
/// measured from their own recordings.
fn read_fresh(
    backends: &Backends,
    harness: &str,
    session: &str,
    plan: &Plan,
    idle_ms: u64,
) -> std::result::Result<CostFacts, String> {
    let backends = backends.of(harness);
    if backends.is_empty() {
        return Err("session store unsupported".to_string());
    }
    if let Some((parent, _)) = session.split_once("::") {
        return read_child(backends, harness, parent, session, &plan.segments);
    }
    let resolved = agent_tapes_core::resolve_session(backends, session)
        .map_err(|error| read_failed(error.is_transient()))?;
    if resolved.session.id != session {
        return Err("usage identity mismatch".to_string());
    }
    let backend = &backends[resolved.backend_index];
    let mut tally = agent_tapes_core::usage::TurnTally::default();
    let mut earliest = None;
    // A participant's request timeline and working stretches come from the
    // same read as its usage.
    let options = agent_tapes_core::backend::StreamOptions {
        timeline: !plan.events.is_empty(),
    };
    let read = backend
        .stream_transcript_with(&resolved.session, None, options, &mut |turn| {
            earliest = earliest.into_iter().chain(turn.ts).min();
            tally.add(&turn);
            Ok(())
        })
        .map_err(|error| read_failed(agent_tapes_core::transient::is_transient(&error)))?;
    let estimating = (!plan.events.is_empty()).then(|| {
        let stretches = agent_tapes_core::usage::stretches(&read, idle_ms as i64);
        Estimating {
            events: &plan.events,
            idle_ms,
            withheld: stretches.is_none().then(|| {
                agent_tapes_core::usage::timeline_withheld(&read)
                    .unwrap_or(span::TIMELINE_UNAVAILABLE)
                    .to_string()
            }),
            starts: stretches
                .unwrap_or_default()
                .iter()
                .map(|stretch| stretch.first)
                .collect(),
        }
    });
    let selection_warnings = resolved
        .diagnostics
        .latest_warnings(&resolved.session)
        .len();
    let whole = backend
        .stream_session(&resolved.session, &read)
        .map_err(|error| read_failed(agent_tapes_core::transient::is_transient(&error)))?;
    let lineage = read_lineage(backends, harness, session);
    let family = harness == "claude" && lineage.native_children.iter().any(|child| !child.mirror());
    let view = agent_tapes_core::usage::streamed(&whole, tally.clone(), &read);
    let mut facts = if family {
        let spans = read_spans(
            backend.as_ref(),
            &resolved.session,
            &read,
            &tally,
            plan,
            estimating.as_ref(),
            selection_warnings,
            harness,
        );
        let mut own = *spans
            .whole
            .clone()
            .ok_or_else(|| OWN_REQUESTS_UNREAD.to_string())?;
        own.spans = (!plan.is_empty()).then_some(spans);
        own
    } else {
        let mut facts = normalize(&view, selection_warnings, harness);
        if !plan.is_empty() {
            facts.spans = Some(read_spans(
                backend.as_ref(),
                &resolved.session,
                &read,
                &tally,
                plan,
                estimating.as_ref(),
                selection_warnings,
                harness,
            ));
        }
        facts
    };
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
    let mut lineage = lineage;
    if view
        .consumption
        .as_ref()
        .is_some_and(|consumption| consumption.identity.fork_parent_unestablished)
    {
        lineage
            .unknown
            .push("lineage relative unresolved".to_string());
        lineage.unknown.sort();
        lineage.unknown.dedup();
    }
    facts.lineage = Some(lineage);
    Ok(facts)
}

/// Why a Claude parent with native children has no figure: its own requests
/// could not be read apart from the recorded total that covers its children.
const OWN_REQUESTS_UNREAD: &str = "parent requests unread apart from its children";

/// Measure a native child held under its parent's recording, by its own
/// requests. Its tie to the parent is the binding's, not an overlap.
fn read_child(
    backends: &[Box<dyn agent_tapes_core::backend::Backend>],
    harness: &str,
    parent: &str,
    key: &str,
    plan: &[Segment],
) -> std::result::Result<CostFacts, String> {
    let resolved = agent_tapes_core::resolve_session(backends, parent)
        .map_err(|error| read_failed(error.is_transient()))?;
    if resolved.session.id != parent {
        return Err(READ_FAILED.to_string());
    }
    let backend = &backends[resolved.backend_index];
    let (children, failures) = backend.window_children(&resolved.session);
    let Some(child) = children.into_iter().find(|child| child.id == key) else {
        let unreadable = failures.iter().any(|failure| failure.starts_with(key));
        return Err(if unreadable {
            READ_FAILED
        } else {
            CHILD_MISSING
        }
        .to_string());
    };
    let mut tally = agent_tapes_core::usage::TurnTally::default();
    let mut earliest = None;
    let read = backend
        .stream_transcript(&child, None, &mut |turn| {
            earliest = earliest.into_iter().chain(turn.ts).min();
            tally.add(&turn);
            Ok(())
        })
        .map_err(|error| read_failed(agent_tapes_core::transient::is_transient(&error)))?;
    let selection_warnings = resolved
        .diagnostics
        .latest_warnings(&resolved.session)
        .len();
    let spans = read_spans(
        backend.as_ref(),
        &child,
        &read,
        &tally,
        plan,
        None,
        selection_warnings,
        harness,
    );
    let mut facts = *spans.whole.clone().ok_or_else(|| READ_FAILED.to_string())?;
    facts.spans = (!plan.is_empty()).then_some(spans);
    facts.source_started_at = earliest.into_iter().chain(child.started_at).min();
    facts.source_start_uncertain = child.start_uncertain || !read.gaps.is_empty();
    facts.lineage = Some(CostLineage::default());
    Ok(facts)
}

/// Why a bound native child has no figure: its parent's store holds no
/// recording for it.
const CHILD_MISSING: &str = "child recording missing";

/// The session's recorded relatives. A child recording its parent's store
/// holds under it, named by agent id rather than a session id, is a native
/// child: it is measured from its own recording when a binding names it, and
/// is listed under its parent otherwise, never an unresolved relative.
fn read_lineage(
    backends: &[Box<dyn agent_tapes_core::backend::Backend>],
    harness: &str,
    session: &str,
) -> CostLineage {
    let view = match agent_tapes_core::lineage_full_with_backends(
        backends,
        agent_tapes_core::Selection::IdInHarness {
            id: session,
            harness,
        },
    ) {
        Ok(view) if view.session.id == session => view,
        Ok(_) => {
            return CostLineage {
                unknown: vec!["lineage identity mismatch".to_string()],
                ..CostLineage::default()
            }
        }
        Err(_) => {
            return CostLineage {
                unknown: vec!["lineage read failed".to_string()],
                ..CostLineage::default()
            }
        }
    };
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
    let mut native_children = Vec::new();
    let children = view
        .lineage
        .children
        .into_iter()
        .filter_map(|child| {
            let Some(session) = child.session_id else {
                if harness == "claude" && plain_session_id(&child.reference) {
                    native_children.push(CostNativeChild {
                        agent: child.reference,
                        role: child
                            .role
                            .filter(|role| portable_cost_metadata(role) && !role.is_empty()),
                        resolved: child.resolved,
                    });
                } else {
                    unknown.push("lineage relative unresolved".to_string());
                }
                return None;
            };
            if !child.resolved {
                unknown.push("lineage relative unresolved".to_string());
            }
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
    native_children.sort_by(|left, right| left.agent.cmp(&right.agent));
    native_children.dedup_by(|left, right| left.agent == right.agent);
    CostLineage {
        parent,
        children,
        forked_from,
        unknown,
        native_children,
    }
}

/// The session's figure in each segment of `plan`, read in one pass. The
/// segments and the remainder add up to the whole in the segments' basis.
/// A participant's working stretches and its own events, which estimate
/// the segments no item covers, or why they cannot.
struct Estimating<'a> {
    events: &'a [(span::Instant, WorkKey)],
    /// Each working stretch's first stamp, in order.
    starts: Vec<span::Instant>,
    idle_ms: u64,
    withheld: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn read_spans(
    backend: &dyn agent_tapes_core::backend::Backend,
    session: &agent_tapes_core::model::Session,
    read: &agent_tapes_core::backend::StreamedTranscript,
    tally: &agent_tapes_core::usage::TurnTally,
    plan: &[Segment],
    estimating: Option<&Estimating<'_>>,
    selection_warnings: usize,
    harness: &str,
) -> CostSpans {
    // The estimate parts are read in the same pass as the segments they cut:
    // each part is a span of its own inside its segment.
    let windows = estimating
        .filter(|estimating| estimating.withheld.is_none())
        .map(|estimating| span::estimate_windows(plan, &estimating.starts, estimating.events))
        .unwrap_or_default();
    let bounds: Vec<_> = plan
        .iter()
        .map(Segment::bounds)
        .chain(
            windows
                .iter()
                .map(|window| agent_tapes_core::span::SpanBounds::Time {
                    from: window.from,
                    to: window.to,
                }),
        )
        .collect();
    let Ok(figures) = backend.stream_spans(session, read, &bounds) else {
        return CostSpans {
            unsplit: Some(span::UNSUPPORTED.to_string()),
            segments: Vec::new(),
            remainder: None,
            remainder_requests: 0,
            whole: None,
            estimate: None,
        };
    };
    let usage = agent_tapes_core::usage::spans(&figures, tally, read);
    let requests = |view: &agent_tapes_core::usage::UsageView| {
        view.span
            .as_ref()
            .map_or(0, |placement| placement.requests as u64)
    };
    let mut segments: Vec<CostSegment> = plan
        .iter()
        .zip(&usage.spans)
        .map(|(segment, view)| CostSegment {
            from: segment.from,
            to: segment.to,
            work: segment.work_labels(),
            runs: segment.runs.iter().cloned().collect(),
            requests: requests(view),
            facts: Box::new(normalize(view, selection_warnings, harness)),
            estimates: Vec::new(),
        })
        .collect();
    for (window, view) in windows.iter().zip(usage.spans.iter().skip(plan.len())) {
        if let Some(segment) = segments.get_mut(window.segment) {
            segment.estimates.push(CostEstimatePart {
                from: window.from,
                to: window.to,
                work: window.work.as_ref().map(WorkKey::label),
                requests: requests(view),
                facts: Box::new(normalize(view, selection_warnings, harness)),
            });
        }
    }
    CostSpans {
        unsplit: None,
        segments,
        estimate: estimating.map(|estimating| CostEstimate {
            basis: span::ESTIMATE_BASIS.to_string(),
            idle_ms: estimating.idle_ms,
            withheld: estimating.withheld.clone(),
        }),
        remainder_requests: requests(&usage.remainder),
        remainder: Some(Box::new(normalize(
            &usage.remainder,
            selection_warnings,
            harness,
        ))),
        whole: Some(Box::new(normalize(
            &usage.whole,
            selection_warnings,
            harness,
        ))),
    }
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
        .served_model
        .as_deref()
        .is_some_and(|model| !portable_coordinate(model))
    {
        facts.served_model = None;
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
        if let Some(coverage) = retained_coverage_reason(reason) {
            *reason = partial_coverage(coverage).to_string();
        } else if !portable_cost_reason(reason) {
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
        lineage.native_children.retain(|child| {
            let safe = plain_session_id(&child.agent);
            if !safe {
                lineage
                    .unknown
                    .push("lineage relative is not a plain session id".to_string());
            }
            safe
        });
        for child in &mut lineage.native_children {
            if child
                .role
                .as_deref()
                .is_some_and(|role| !portable_cost_metadata(role))
            {
                child.role = None;
            }
        }
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
    if let Some(spans) = facts.spans.as_mut() {
        for segment in &mut spans.segments {
            *segment.facts = sanitized_facts(&segment.facts);
        }
        for figure in spans.remainder.iter_mut().chain(spans.whole.iter_mut()) {
            **figure = sanitized_facts(figure);
        }
    }
    // Segments that cannot be classified leave the session whole.
    if facts.spans.as_ref().is_some_and(|spans| !spans.portable()) {
        facts.spans = None;
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
        MeasurementReason::UnlistedModel => UNLISTED_MODEL,
        MeasurementReason::TtlUnrecorded => TTL_UNRECORDED,
        MeasurementReason::RequestTermsUnknown => {
            "a price term set by the request may apply to requests the figure does not hold one by one"
        }
        MeasurementReason::BillingUnestablished => {
            "the recording does not establish whether a long-context surcharge was billed"
        }
        MeasurementReason::OutsideValidity => "no price row was in effect at the request's time",
    }
}

/// Every reason `arc_reason` names.
const REASONS: [MeasurementReason; 16] = [
    MeasurementReason::MissingCounter,
    MeasurementReason::MissingPrice,
    MeasurementReason::Unpriced,
    MeasurementReason::StoppedAttempt,
    MeasurementReason::UnreadableRecord,
    MeasurementReason::ReadWindow,
    MeasurementReason::SinceReset,
    MeasurementReason::UnestablishedScope,
    MeasurementReason::IncompleteAttribution,
    MeasurementReason::Undetermined,
    MeasurementReason::UnrecognizedUsage,
    MeasurementReason::UnlistedModel,
    MeasurementReason::TtlUnrecorded,
    MeasurementReason::RequestTermsUnknown,
    MeasurementReason::BillingUnestablished,
    MeasurementReason::OutsideValidity,
];

/// The tapes reason arc names `text`, if it names one.
fn tapes_reason(text: &str) -> Option<MeasurementReason> {
    REASONS
        .into_iter()
        .find(|reason| arc_reason(*reason) == text)
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
/// The one model the recording says served the session, if it names one.
/// It names one in its per-model split, its effort unnamed when the split
/// names several, or, with no split, in the model selections it observed,
/// or in the session's model when it observed no mix of models.
fn served_model(view: &agent_tapes_core::usage::UsageView) -> Option<String> {
    let coordinate = |model: &str, variant: Option<&String>| match variant {
        Some(variant) => format!("{model}#{variant}"),
        None => model.to_string(),
    };
    let session = &view.session;
    let coordinate = match view.by_model.as_deref().unwrap_or_default() {
        [model] => coordinate(&model.model, model.variant.as_ref()),
        [first, rest @ ..] if rest.iter().all(|model| model.model == first.model) => {
            first.model.clone()
        }
        // One model selected at several efforts is that model, effort unnamed.
        [] if !session.model_selections.is_empty() => {
            let selections = &session.model_selections;
            let models: Vec<&String> = selections
                .iter()
                .map(|span| &span.model.id)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let [model] = models[..] else {
                return None;
            };
            let variants: BTreeSet<Option<&String>> = selections
                .iter()
                .map(|span| span.model.variant.as_ref())
                .collect();
            match &variants.into_iter().collect::<Vec<_>>()[..] {
                [variant] => coordinate(model, *variant),
                _ => model.clone(),
            }
        }
        [] if session
            .model_observation
            .as_ref()
            .is_none_or(|status| !status.mixed) =>
        {
            let model = session.model.as_ref()?;
            coordinate(&model.id, model.variant.as_ref())
        }
        _ => return None,
    };
    portable_coordinate(&coordinate).then_some(coordinate)
}

/// Why a figure whose counters cover less than the whole session is
/// incomplete, in words for each accounting coverage.
fn partial_coverage(coverage: &str) -> &'static str {
    match coverage {
        "read-window" => COUNTERS_READ_WINDOW,
        "since-reset" => COUNTERS_SINCE_RESET,
        _ => "the counters cover less than the whole session",
    }
}

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
        incomplete.push(partial_coverage(coverage).to_string());
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
        served_model: served_model(view),
        attribution_uncertain: view
            .session
            .model_observation
            .as_ref()
            .is_some_and(|status| status.attribution_uncertain),
        disjoint: disjoint.as_ref().and_then(cost_disjoint),
        by_model: priced_shares(view, harness, disjoint.as_ref()),
        cost_coverage: cost_coverage.map(|coverage| CostPricing {
            complete: coverage.complete,
            zero_written_requests: coverage.zero_written_requests as u64,
            zero_written_observations: coverage.zero_written_observations as u64,
        }),
        events,
        residual: None,
        spans: None,
        list_price: None,
        // Consumption keeps its own requests, those on other branches too,
        // never the active branch's alone.
        request_terms: CostRequestTerms {
            requests: match consumption {
                Some(consumption) => consumption.request_uses.clone(),
                None => view.request_uses.clone(),
            },
            billing: view.billing.clone(),
            from: view.session.started_at,
            to: view.session.last_activity_at,
        },
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

fn cost_disjoint(disjoint: &agent_tapes_core::model::DisjointTokens) -> Option<CostDisjoint> {
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
}

/// The model shares a figure is priced by one by one, each model's
/// disjoint categories with its efforts added together; none where the one
/// model `served_model` names prices it whole. A figure is priced share by
/// share where its recording names several models; where it counts events
/// on other branches than the active one, by the model each event names,
/// since the session's model evidence is the active branch's; and where its
/// attribution is uncertain and its shares leave part of it out, so the
/// part no model is named for stays unpriced. A share is priced by the
/// requests that model served, which a recording keeps by model but not
/// always by effort, so it holds every effort's tokens.
fn priced_shares(
    view: &agent_tapes_core::usage::UsageView,
    harness: &str,
    whole: Option<&agent_tapes_core::model::DisjointTokens>,
) -> Vec<crate::model::CostModelShare> {
    let Some(convention) = agent_tapes_core::model::TokenConvention::of(harness) else {
        return Vec::new();
    };
    if let Some(consumption) = view
        .consumption
        .as_ref()
        .filter(|consumption| consumption.events.other_branches > 0)
    {
        return model_shares(
            &consumption.served,
            &consumption.measurement_completeness,
            convention,
        );
    }
    let models = view.by_model.as_deref().unwrap_or_default();
    let shares = model_shares(models, &MeasurementCompleteness::default(), convention);
    let ids: BTreeSet<&str> = models.iter().map(|model| model.model.as_str()).collect();
    let uncertain = view
        .session
        .model_observation
        .as_ref()
        .is_some_and(|status| status.attribution_uncertain);
    let unattributed = uncertain
        && !models.is_empty()
        && whole.is_some_and(|whole| {
            agent_tapes_core::pricing::PRICED_CATEGORIES
                .iter()
                .any(|category| {
                    category_amount(whole, category).is_some()
                        && !shares_cover(whole, &shares, category)
                })
        });
    if ids.len() > 1 || unattributed {
        shares
    } else {
        Vec::new()
    }
}

/// Each model's disjoint categories among `models`, its efforts added
/// together, each qualified by `completeness` beside its own and its cache
/// writes split by the TTLs its recording names.
fn model_shares(
    models: &[agent_tapes_core::usage::ModelUsage],
    completeness: &MeasurementCompleteness,
    convention: agent_tapes_core::model::TokenConvention,
) -> Vec<crate::model::CostModelShare> {
    let mut efforts: BTreeMap<&str, Vec<CostDisjoint>> = BTreeMap::new();
    for model in models
        .iter()
        .filter(|model| portable_coordinate(&model.model))
    {
        let Some(tokens) = model.tokens.as_ref() else {
            continue;
        };
        let mut qualified = model.measurement_completeness.clone();
        qualified.merge(completeness);
        let disjoint = agent_tapes_core::model::DisjointTokens::from_native(
            tokens,
            convention,
            model.cache_write_by_ttl.as_ref(),
        )
        .with_completeness(&qualified);
        if let Some(disjoint) = cost_disjoint(&disjoint) {
            efforts.entry(&model.model).or_default().push(disjoint);
        }
    }
    efforts
        .into_iter()
        .map(|(model, efforts)| {
            let held: Vec<(Option<&CostDisjoint>, u64)> =
                efforts.iter().map(|effort| (Some(effort), 1)).collect();
            crate::model::CostModelShare {
                model: model.to_string(),
                disjoint: span::sum_disjoint(efforts[0].convention.clone(), &held),
            }
        })
        .collect()
}

/// A figure's amount of one priced category.
fn category_amount(
    disjoint: &agent_tapes_core::model::DisjointTokens,
    category: &str,
) -> Option<u64> {
    match category {
        "uncached_input" => disjoint.uncached_input,
        "cache_read" => disjoint.cache_read,
        "cache_write" => disjoint.cache_write.map(|writes| writes.total()),
        _ => disjoint.output,
    }
}

/// Whether the model shares account for all of the figure's `category`.
fn shares_cover(
    whole: &agent_tapes_core::model::DisjointTokens,
    shares: &[crate::model::CostModelShare],
    category: &str,
) -> bool {
    let Some(total) = category_amount(whole, category) else {
        return false;
    };
    shares
        .iter()
        .try_fold(0u64, |held, share| {
            let part = tapes_disjoint(&share.disjoint)?;
            held.checked_add(category_amount(&part, category)?)
        })
        .is_some_and(|covered| covered >= total)
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
                agent: source.agent_id.clone(),
                requests: source.requests as u64,
                identified: source.identified.as_ref().map(|tokens| CostCounters {
                    input: tokens.input,
                    output: tokens.output,
                    reasoning: tokens.reasoning,
                    cache_read: tokens.cache_read,
                    cache_write: tokens.cache_write,
                }),
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

/// Price a figure under the view's table, or leave it unpriced without one.
fn price(facts: &mut CostFacts, table: Option<&agent_tapes_core::pricing::PriceTable>) {
    facts.list_price = table.map(|table| list_price(facts, table));
}

/// tapes' disjoint categories of a figure, as the pricing reads them, each
/// with the tapes reasons its gaps name.
fn tapes_disjoint(disjoint: &CostDisjoint) -> Option<agent_tapes_core::model::DisjointTokens> {
    use agent_tapes_core::model::{CacheWrites, DisjointTokens, TokenConvention};
    let mut completeness = MeasurementCompleteness::default();
    for (category, reasons) in disjoint.gaps.iter().flatten() {
        for reason in reasons.iter().filter_map(|reason| tapes_reason(reason)) {
            completeness.qualify(category, reason);
        }
    }
    Some(DisjointTokens {
        convention: serde_json::from_value::<TokenConvention>(json!(disjoint.convention)).ok()?,
        uncached_input: disjoint.uncached_input,
        cache_read: disjoint.cache_read,
        cache_write: disjoint.cache_write.map(|writes| CacheWrites {
            ephemeral_5m: Some(writes.ephemeral_5m),
            ephemeral_1h: Some(writes.ephemeral_1h),
            ttl_unrecorded: Some(writes.ttl_unrecorded),
        }),
        output: disjoint.output,
        reasoning: disjoint.reasoning,
        total: None,
        measurement_completeness: completeness,
        missing: Vec::new(),
    })
}

/// One model's part of a figure priced through tapes' share pricing: by its
/// rows' flat rates, or, where a row carries terms set by the request,
/// request by request over the requests the figure holds one by one, under
/// the billing its recording establishes, and between every rate it could
/// pay for what it does not hold, each category keeping the reasons the
/// share gives it. `sole` says the model served the whole figure, so a
/// request no model is named for is its too.
fn price_model(
    disjoint: &agent_tapes_core::model::DisjointTokens,
    model: &str,
    sole: bool,
    table: &agent_tapes_core::pricing::PriceTable,
    terms: &CostRequestTerms,
) -> agent_tapes_core::pricing::EstimateSum {
    agent_tapes_core::pricing::PricedShare {
        disjoint,
        model,
        variant: None,
        sole,
        terms: agent_tapes_core::pricing::RequestTerms {
            requests: terms.requests.as_ref(),
            billing: terms.billing.as_ref(),
            from: terms.from,
            to: terms.to,
        },
    }
    .estimate(table)
}

/// A figure's list price: its disjoint categories priced by the table rows
/// of the one model that served it, not all of it certainly where its
/// attribution is uncertain, or share by share where its model shares
/// price it.
/// What the table cannot price is a fixed reason, a category the figure
/// measured incompletely qualifies the estimate, and so does any part of a
/// category the models' shares leave unattributed, a term set by the request
/// the figure's requests do not settle, and billing its recording does not
/// establish.
fn list_price(facts: &CostFacts, table: &agent_tapes_core::pricing::PriceTable) -> CostListPrice {
    use agent_tapes_core::pricing::{EstimateSum, PRICED_CATEGORIES};
    let unpriced = |gaps: Vec<String>| CostListPrice {
        currency: table.currency.clone(),
        schema: table.schema,
        amount: None,
        at_most: None,
        models: Vec::new(),
        gaps,
    };
    let Some(whole) = facts.disjoint.as_ref().and_then(tapes_disjoint) else {
        return unpriced(vec!["recorded category is absent".to_string()]);
    };
    let mut gaps: BTreeSet<String> = figure_gaps(facts, Figure::Disjoint("total"))
        .into_iter()
        .collect();
    let mut sum = EstimateSum::default();
    let mut unlisted = false;
    let mut price = |model: &str, disjoint: &agent_tapes_core::model::DisjointTokens, sole| {
        unlisted |= table.price(model).is_none();
        sum.merge(price_model(
            disjoint,
            model,
            sole,
            table,
            &facts.request_terms,
        ));
    };
    // Events on other branches than the active one are priced by the model
    // each names, never by the one model the session's evidence names.
    let other_branches = facts
        .events
        .as_ref()
        .is_some_and(|events| events.other_branches > 0);
    match facts.served_model.as_deref().and_then(model_of) {
        Some(model) if facts.by_model.is_empty() && !other_branches => {
            price(&model, &whole, true);
            if facts.attribution_uncertain {
                gaps.insert(arc_reason(MeasurementReason::IncompleteAttribution).to_string());
            }
        }
        _ if !facts.by_model.is_empty() => {
            for share in &facts.by_model {
                if let Some(disjoint) = tapes_disjoint(&share.disjoint) {
                    price(&share.model, &disjoint, false);
                }
            }
            for category in PRICED_CATEGORIES {
                if !shares_cover(&whole, &facts.by_model, category) {
                    gaps.insert(arc_reason(MeasurementReason::IncompleteAttribution).to_string());
                }
            }
        }
        _ if other_branches => {
            return unpriced(vec![
                arc_reason(MeasurementReason::IncompleteAttribution).to_string()
            ])
        }
        _ => return unpriced(vec![NO_SERVED_MODEL.to_string()]),
    }
    if unlisted {
        gaps.insert(UNLISTED_MODEL.to_string());
    }
    let Some(estimate) = sum.finish() else {
        if !unlisted {
            gaps.insert("recorded category is absent".to_string());
        }
        return unpriced(gaps.into_iter().collect());
    };
    gaps.extend(arc_reasons(
        estimate
            .measurement_completeness
            .iter()
            .flat_map(|(_, reasons)| reasons),
    ));
    CostListPrice {
        currency: estimate.currency,
        schema: estimate.schema,
        // A cache write with no recorded TTL, or a term the figure's requests
        // do not settle, leaves the least and the most it can cost.
        amount: estimate.amount.or(estimate.at_least),
        at_most: estimate.at_most.filter(|_| estimate.amount.is_none()),
        models: estimate.models.into_iter().map(|model| model.id).collect(),
        gaps: gaps.into_iter().collect(),
    }
}

/// The model coordinate a free-form route names: the whole route, or what
/// precedes ` via <harness>`, when that is one coordinate. A route that is a
/// profile name or prose names none.
fn route_coordinate(route: &str) -> Option<String> {
    let head = route
        .split_once(" via ")
        .map_or(route, |(head, _)| head)
        .trim();
    portable_coordinate(head).then(|| head.to_string())
}

/// A coordinate as read under the harness that ran it: a route or a declared
/// model may carry that harness as a prefix, `claude:claude-opus-5-5#medium`,
/// which a `harness:model#effort` label already names once.
fn on_harness(harness: &str, coordinate: String) -> String {
    match coordinate
        .strip_prefix(harness)
        .and_then(|rest| rest.strip_prefix(':'))
    {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ => coordinate,
    }
}

/// Whether a coordinate's prefix names a harness tapes reads recordings of,
/// as `codex` in `codex:gpt-6.1-sol#high` does.
fn names_harness(coordinate: &str) -> bool {
    coordinate.split_once(':').is_some_and(|(prefix, rest)| {
        !rest.is_empty() && agent_tapes_core::model::TokenConvention::of(prefix).is_some()
    })
}

/// The model of a `model[#effort]` coordinate, without its effort.
fn model_of(coordinate: &str) -> Option<String> {
    let model = coordinate.split('#').next().unwrap_or(coordinate).trim();
    (!model.is_empty()).then(|| model.to_string())
}

/// The `harness:id` an event was recorded on behalf of, before anything
/// says whether it names a session, with the model the subject declares for
/// it: `harness:id:model[#effort]`, either form followed by a parenthesized
/// label or not, names the session `id` served by `model`.
fn subject_of(event: &Event) -> Option<((String, String), Option<String>)> {
    let subject = event.on_behalf_of.as_deref()?.trim();
    let subject = match subject.split_once(" (") {
        Some((head, label)) if label.ends_with(')') => head,
        _ => subject,
    };
    let (harness, rest) = subject.split_once(':')?;
    let (id, model) = match rest.split_once(':') {
        Some((id, model)) => (id, Some(model.to_string())),
        None => (rest, None),
    };
    (plain_session_id(harness)
        && plain_session_id(id)
        && model.as_deref().is_none_or(portable_coordinate))
    .then(|| ((harness.to_string(), id.to_string()), model))
}

/// Why a subject session nothing on this machine holds has no figure.
const NOT_LOCATED: &str = "not located on this machine";

/// Whether `id` has the shape `harness` gives a session id, so it names no
/// model, person or label: a UUID, as Claude, Codex and Pi write one, a
/// Claude cloud session's `cse_…`, or an OpenCode `ses_…`.
fn session_shaped(harness: &str, id: &str) -> bool {
    let cloud =
        |rest: &str| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_alphanumeric());
    crate::coordinate::is_session_id(id, None)
        || (harness == "claude" && id.strip_prefix("cse_").is_some_and(cloud))
        || (harness == "opencode" && id.strip_prefix("ses_").is_some_and(|rest| !rest.is_empty()))
}

/// The `--on-behalf-of` subjects in this ledger that name a session: an id
/// the subject harness's tapes reader locates as one of its recordings, as
/// each harness shapes its ids (OpenCode's `ses_…`, a Pi id as its
/// recording names it, a Claude or Codex UUID), one whose store could not
/// answer, one the ledger retains facts for, or one shaped as its harness
/// shapes a session id that nothing on this machine locates. Any other
/// subject, a person, a label or a `harness:model`, names a party and no
/// session.
struct Subjects {
    sessions: BTreeSet<(String, String)>,
    /// The sessions among them that only their shape names: their recording
    /// is not on this machine.
    unlocated: BTreeSet<(String, String)>,
}

impl Subjects {
    fn read(
        read: &mut CostRead<'_>,
        repository: &[Event],
        backends: &std::cell::OnceCell<Backends>,
    ) -> Self {
        let mut candidates: BTreeSet<(String, String)> = repository
            .iter()
            .filter_map(subject_of)
            .map(|(key, _)| key)
            .collect();
        for change_id in read.change_ids() {
            candidates.extend(
                read.events(&change_id)
                    .iter()
                    .filter_map(subject_of)
                    .map(|(key, _)| key),
            );
        }
        let mut subjects = Self {
            sessions: BTreeSet::new(),
            unlocated: BTreeSet::new(),
        };
        for (harness, id) in candidates {
            let stores = backends.get_or_init(Backends::new).of(&harness);
            let located = !retained_for(repository, &harness, &id).is_empty()
                || stores.iter().any(|backend| {
                    // A join that expects misses: no store is walked twice.
                    backend.locate_relative(&id).map_or(true, |session| {
                        session.is_some_and(|session| session.id == id)
                    })
                });
            let shaped = !stores.is_empty() && session_shaped(&harness, &id);
            if !located && !shaped {
                continue;
            }
            if !located {
                subjects.unlocated.insert((harness.clone(), id.clone()));
            }
            subjects.sessions.insert((harness, id));
        }
        subjects
    }

    /// The session `event` was recorded on behalf of, if its subject names
    /// one, with the model the subject declares for it.
    fn of(&self, event: &Event) -> Option<((String, String), Option<String>)> {
        subject_of(event).filter(|(key, _)| self.sessions.contains(key))
    }
}

/// A session an event names.
struct Named {
    key: (String, String),
    /// It is the event's acting session, not the subject it was recorded on
    /// behalf of.
    own: bool,
    /// The model coordinate a subject declares for its session.
    declared: Option<String>,
}

/// The sessions an event names, each once: its acting session, marked own,
/// and the session it was recorded on behalf of. This one rule decides the
/// work a session is part of, for its membership and for `--footprint`.
fn named_sessions(event: &Event, subjects: &Subjects) -> Vec<Named> {
    let actor = match (&event.harness, &event.session) {
        (Some(harness), Some(session))
            if plain_session_id(harness) && plain_session_id(session) =>
        {
            Some((harness.clone(), session.clone()))
        }
        _ => None,
    };
    let subject = subjects
        .of(event)
        .filter(|(subject, _)| Some(subject) != actor.as_ref());
    actor
        .into_iter()
        .map(|key| Named {
            key,
            own: true,
            declared: None,
        })
        .chain(subject.map(|(key, declared)| Named {
            key,
            own: false,
            declared,
        }))
        .collect()
}

/// A session the ledger's events name.
#[derive(Default)]
struct Person {
    /// Every work item in this ledger whose events carry the session, as
    /// their acting session or the subject they were recorded on behalf of.
    membership: BTreeSet<WorkKey>,
    /// Its first and last own event on each item it recorded events on.
    bounds: BTreeMap<WorkKey, (span::Instant, span::Instant)>,
    /// Every event it recorded on those items, with the item.
    events: Vec<(span::Instant, WorkKey)>,
    /// What its events on the selected work show it did.
    roles: BTreeSet<Role>,
    /// The models its own events, or the subjects recorded on its behalf,
    /// declared in each role on the selected work, effort dropped.
    models: BTreeMap<Role, BTreeSet<String>>,
    /// The model coordinates its own events on the selected work declared or
    /// resolved, and those its subjects declared, effort kept.
    coordinates: BTreeSet<String>,
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

impl Person {
    /// It has several members, and one of them only events recorded on its
    /// behalf name, which give it no span there.
    fn spanless(&self) -> bool {
        self.membership.len() > 1 && self.membership.len() > self.bounds.len()
    }
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
    subjects: &Subjects,
) -> Ledger {
    let selected: BTreeSet<&WorkKey> = selection.items.iter().map(|item| &item.key).collect();
    let mut ledger = Ledger {
        people: BTreeMap::new(),
        unattributed_events: 0,
        complete: false,
    };
    let mut note = |event: &Event, work: WorkKey| {
        let on_selection = selected.contains(&work);
        let named = named_sessions(event, subjects);
        if !named.iter().any(|named| named.own) && on_selection && role_of(&event.payload).is_some()
        {
            ledger.unattributed_events += 1;
        }
        // The subject's event was recorded by the invoker: its instant, model
        // and session are the invoker's, so it opens no span of the subject
        // and declares only the model its subject names.
        for Named { key, own, declared } in named {
            let role = acting_role(&event.payload, assigned.bound.contains(&key));
            let harness = key.0.clone();
            let person = ledger.people.entry(key).or_default();
            if own {
                person
                    .bounds
                    .entry(work.clone())
                    .and_modify(|(first, last)| {
                        *first = (*first).min(event.created_at);
                        *last = (*last).max(event.created_at);
                    })
                    .or_insert((event.created_at, event.created_at));
                person.events.push((event.created_at, work.clone()));
            }
            person.membership.insert(work.clone());
            if !on_selection {
                continue;
            }
            person.selected_events += 1;
            let model = if own { event.model.clone() } else { declared };
            if let Some(model) = model.as_deref().filter(|model| portable_coordinate(model)) {
                person.coordinates.insert(model.to_string());
            }
            if let Some(role) = role {
                person.acts = true;
                person.roles.insert(role);
                if let Some(model) = model.as_deref().and_then(model_of) {
                    let model = on_harness(&harness, model);
                    person.models.entry(role).or_default().insert(model);
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

/// The segments each session with several spans is read in: an executor's
/// runs, and a participant's events on each work item.
fn span_plans(sessions: &Sessions, ledger: &Ledger) -> BTreeMap<(String, String), Plan> {
    let executors = sessions
        .iter()
        .filter(|(_, (spans, _))| spans.len() > 1)
        .map(|(key, (spans, _))| {
            let spans: Vec<ItemSpan> = spans
                .iter()
                .map(|span| ItemSpan {
                    work: span.work.clone(),
                    run: Some(span.dispatch_event_id.clone()),
                    from: span.dispatched_at,
                    to: span.ended_at,
                })
                .collect();
            let plan = Plan {
                segments: span::segments(&spans),
                events: Vec::new(),
            };
            (key.clone(), plan)
        });
    let participants = ledger
        .participants()
        .filter(|(_, person)| person.bounds.len() > 1 && !person.spanless())
        .map(|(key, person)| {
            let spans: Vec<ItemSpan> = person
                .bounds
                .iter()
                .map(|(work, (first, last))| ItemSpan::events(work.clone(), *first, *last))
                .collect();
            let plan = Plan {
                segments: span::segments(&spans),
                events: person.events.clone(),
            };
            (key.clone(), plan)
        });
    executors.chain(participants).collect()
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
    // A spawn a binding identified is a parent and a child with recordings of
    // their own, not one recording copying another, so it groups nothing.
    let spawns: BTreeSet<(String, String)> = sessions
        .iter()
        .filter_map(|row| {
            let parent = row.parent.as_ref()?;
            Some((format!("{}:{parent}", row.harness), row.key()))
        })
        .collect();
    // A Claude child recording can hold a request its parent's recording
    // holds too. Unless the family's reconciliation shows the parent's own
    // requests and each child's add up to its identified union, a measured
    // parent and its bound child may overlap.
    let shares_requests = |row: &SessionRow| -> Option<String> {
        let parent = row.parent.as_ref().filter(|_| row.harness == "claude")?;
        let parent = sessions
            .iter()
            .find(|candidate| candidate.harness == row.harness && &candidate.session == parent)?;
        let facts = parent.measured.facts.as_ref()?;
        (family_reconciliation(facts.residual.as_ref())["status"] != "conserved")
            .then(|| parent.key())
    };
    let spawned =
        |parent: &str, child: &str| spawns.contains(&(parent.to_string(), child.to_string()));
    let mut groups: Vec<BTreeSet<String>> = Vec::new();
    for row in sessions {
        let mut group = BTreeSet::from([row.key()]);
        group.extend(shares_requests(row));
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
            if let Some(parent) = &lineage.parent {
                let parent = format!("{}:{parent}", row.harness);
                if !spawned(&parent, &row.key()) {
                    group.insert(parent);
                }
            }
            if let Some(origin) = &lineage.forked_from {
                group.insert(format!("{}:{origin}", row.harness));
            }
            for child in &lineage.children {
                let child = format!("{}:{}", child.harness, child.session);
                if !spawned(&row.key(), &child) {
                    group.insert(child);
                }
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

/// Whether a row's recorded relatives are unknown, so it may overlap any
/// other. A native child's identity is its parent's recording naming it,
/// whether or not it was measured.
fn lineage_unknown(row: &SessionRow) -> bool {
    if row.parent.is_some() {
        return false;
    }
    row.measured
        .facts
        .as_ref()
        .and_then(|facts| facts.lineage.as_ref())
        .is_none_or(|lineage| !lineage.unknown.is_empty())
}

/// One reported quantity: the harness-written dollar figure, the list-price
/// estimate, a native counter in its harness's own convention, or a
/// disjoint token category.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Figure {
    Usd,
    ListPrice,
    Native(&'static str),
    Disjoint(&'static str),
}

fn figures() -> impl Iterator<Item = Figure> {
    [Figure::Usd, Figure::ListPrice]
        .into_iter()
        .chain(CATEGORIES.into_iter().map(Figure::Native))
        .chain(DISJOINT.into_iter().map(Figure::Disjoint))
}

/// The accounting domain a figure adds within. Native counters and dollar
/// figures add within one harness and basis; disjoint categories and the
/// list prices of them add across harnesses within one basis.
fn domain(row: &SessionRow, figure: Figure) -> (Option<&str>, Option<&str>) {
    let basis = row
        .measured
        .facts
        .as_ref()
        .and_then(|facts| facts.basis.as_deref());
    match figure {
        Figure::Disjoint(_) | Figure::ListPrice => (None, basis),
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
        Figure::ListPrice => {
            let currency = members.iter().find_map(|row| {
                Some(
                    row.measured
                        .facts
                        .as_ref()?
                        .list_price
                        .as_ref()?
                        .currency
                        .clone(),
                )
            });
            let schema = members
                .iter()
                .find_map(|row| Some(row.measured.facts.as_ref()?.list_price.as_ref()?.schema))
                .unwrap_or(agent_tapes_core::pricing::PRICE_TABLE_SCHEMA);
            json!({"kind": LIST_KIND, "unit": currency, "schema": schema,
                "pricing_basis": "the price table's list price per million tokens of each disjoint category, by the model that served them",
                "source": "price table", "precision": "currency sums rounded to ten fractional places; smaller nonzero values preserved", "scope": scope})
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
        Figure::ListPrice => match &facts.list_price {
            Some(price) => price.gaps.clone(),
            None => vec![NO_TABLE.to_string()],
        },
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
                reasons.insert(TTL_UNRECORDED.to_string());
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
        Figure::ListPrice => facts
            .list_price
            .as_ref()
            .and_then(|price| price.amount)
            .map(Amount::Usd),
        Figure::Native(category) => counter(&facts.counters, category).map(Amount::Tokens),
        Figure::Disjoint(category) => facts
            .disjoint
            .as_ref()
            .and_then(|disjoint| disjoint_amount(disjoint, category))
            .map(Amount::Tokens),
    }
}

/// The most a session's figure can be: the upper bound of a list price in
/// doubt, else its recorded amount.
fn recorded_most(facts: &CostFacts, figure: Figure) -> Option<Amount> {
    match (figure, &facts.list_price) {
        (Figure::ListPrice, Some(price)) => price.at_most.or(price.amount).map(Amount::Usd),
        _ => recorded_amount(facts, figure),
    }
}

/// A reason a list price is priced between the least and the most it can
/// cost, which its bound accounts for.
fn priced_in_doubt(reason: &str) -> bool {
    reason == TTL_UNRECORDED
        || reason == arc_reason(MeasurementReason::RequestTermsUnknown)
        || reason == arc_reason(MeasurementReason::BillingUnestablished)
}

const OVERLAP_BASIS: &str =
    "sum of disjoint groups; each overlapping group is at least its largest member and at most its sum";
const OVERLAP_PRICE_BASIS: &str = "sum of disjoint groups; each overlapping group is at least its largest member and at most its sum, each list price in doubt at the least and the most it can cost";
const PRICE_BASIS: &str =
    "sum of the members' list prices, each in doubt at the least and the most it can cost";

/// What a quantity with no established amount lies between.
struct Bounds {
    lower: Amount,
    upper: Amount,
    /// Whether they bound only the recorded amounts, not the whole figure.
    known_only: bool,
    basis: &'static str,
}

/// The status, amount, known subtotal, and reasons of one quantity.
struct Aggregate {
    status: &'static str,
    amount: Option<Amount>,
    known: Option<Amount>,
    reasons: Vec<String>,
    bounds: Option<Bounds>,
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
            Figure::Disjoint(_) | Figure::ListPrice => format!("incompatible accounting bases: {}; quantities are partitioned by accounting basis", names.join(", ")),
            _ => format!("incompatible accounting domains: {}; quantities are partitioned by harness and accounting basis", names.join(", ")),
        }]);
    }
    let mut amounts = Vec::new();
    let mut mosts = Vec::new();
    let mut missing = Vec::new();
    let mut gaps = BTreeSet::new();
    // Whether a member's list price is in doubt, and whether a reason
    // its range does not account for qualifies the figure.
    let mut ranged = false;
    let mut beyond_range = false;
    for row in members {
        match row.measured.facts.as_ref() {
            Some(facts) => {
                match recorded_amount(facts, figure) {
                    Some(amount) => amounts.push(amount),
                    None => missing.push(row.key()),
                }
                mosts.extend(recorded_most(facts, figure));
                ranged |= matches!(figure, Figure::ListPrice)
                    && facts
                        .list_price
                        .as_ref()
                        .is_some_and(|price| price.at_most.is_some());
                let figure_gaps = figure_gaps(facts, figure);
                beyond_range |= !facts.incomplete.is_empty()
                    || facts.basis.is_none()
                    || !figure_gaps.iter().all(|reason| priced_in_doubt(reason));
                for reason in &facts.incomplete {
                    gaps.insert(format!("{}: {reason}", row.key()));
                }
                for reason in figure_gaps {
                    gaps.insert(format!("{}: {reason}", row.key()));
                }
                if facts.basis.is_none() {
                    gaps.insert(format!("{}: accounting basis is unknown", row.key()));
                }
            }
            None => missing.push(row.key()),
        }
    }
    let (known, most) = match (sum_amounts(amounts), sum_amounts(mosts)) {
        (Ok(known), Ok(most)) => (known, most),
        (Err(reason), _) | (_, Err(reason)) => {
            return Aggregate::withheld(vec![reason.to_string()])
        }
    };
    let range = |known_only: bool| {
        known
            .zip(most)
            .filter(|_| ranged)
            .map(|(lower, upper)| Bounds {
                lower,
                upper,
                known_only,
                basis: PRICE_BASIS,
            })
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
            bounds: lower.zip(most).map(|(lower, upper)| Bounds {
                lower,
                upper,
                known_only: !missing.is_empty() || !gaps.is_empty(),
                basis: if ranged {
                    OVERLAP_PRICE_BASIS
                } else {
                    OVERLAP_BASIS
                },
            }),
            reasons,
        };
    }
    if !missing.is_empty() {
        let mut reasons = vec![format!("no recorded amount for {}", missing.join(", "))];
        reasons.extend(gaps);
        return Aggregate {
            bounds: range(true),
            status: "absent",
            amount: None,
            known,
            reasons,
        };
    }
    if !gaps.is_empty() {
        return Aggregate {
            bounds: range(beyond_range),
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
    if let Some(bounds) = aggregate.bounds {
        let known_only = bounds.known_only;
        let field = if known_only { "known_bound" } else { "bound" };
        base[field] = json!({"scope": if known_only { "known recorded amounts" } else { "whole figure" },
            "at_least": bounds.lower.bound_json(false), "at_most": bounds.upper.bound_json(true),
            "basis": bounds.basis});
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

/// A figure as a view gives it: conserving, allocated beside it when the
/// view allocates, and with a participant's estimated parts beside it.
struct Figures {
    conserving: Value,
    allocated: Option<Value>,
    estimated: Option<Value>,
}

/// How a figure with estimates was estimated: its basis and the idle gap
/// that ends a working stretch.
fn estimate_label(idle_ms: u64) -> Value {
    json!({"basis": span::ESTIMATE_BASIS, "idle_ms": idle_ms, "idle": idle_text(idle_ms),
        "statement": "a participant's requests no item's span holds, given to an item by its own events in the same working stretch: those up to an event serve that event's item, those after a stretch's last event serve the same item, and a stretch with no event serves none; measured, estimated and unattributed parts add up to the whole"})
}

/// An idle gap as a duration: whole hours, else whole minutes, else seconds.
fn idle_text(idle_ms: u64) -> String {
    let seconds = idle_ms / 1000;
    if seconds.is_multiple_of(3600) && seconds > 0 {
        format!("{}h", seconds / 3600)
    } else if seconds.is_multiple_of(60) && seconds > 0 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

/// The item the first segment after `index` that one item covers belongs to.
fn next_item(segments: &[CostSegment], index: usize) -> Option<&String> {
    segments[index + 1..]
        .iter()
        .find_map(|segment| match &segment.work[..] {
            [one] => Some(one),
            _ => None,
        })
}

/// The idle gap that ends a participant's working stretch unless a view
/// names another: 45 minutes, as `tapes stats` reads active time.
pub(super) const DEFAULT_IDLE_MS: u64 = 45 * 60 * 1000;

/// How what no single item holds is allocated beside the conserving figures:
/// a segment several items cover evenly among them, or what remains between
/// items to the next item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allocation {
    Even,
    Following,
}

impl Allocation {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "even" => Some(Self::Even),
            "following" => Some(Self::Following),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Even => "even",
            Self::Following => "following",
        }
    }
}

/// Which parts of a divided session a share takes: the segments its items
/// alone cover, those and the estimated parts given to them, or an
/// allocation beside them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Take {
    Measured,
    Estimated,
    Allocated(Allocation),
}

/// What a view shows beside the figures: its groupings, its price table,
/// and whether it allocates overlapped requests.
struct View<'a> {
    by: &'a [String],
    prices: &'a agent_tapes_core::pricing::PriceLookup,
    allocation: Option<Allocation>,
    /// The idle gap that ends a participant's working stretch.
    idle_ms: u64,
    /// The fresh reads of spawned children no row holds, by harness and id.
    children: &'a Children,
    /// Every session any dispatch binds, selected or not.
    bound: &'a BTreeSet<(String, String)>,
}

type Children = BTreeMap<(String, String), std::result::Result<CostFacts, String>>;

impl SessionRow {
    /// The session's segment figures, when its figure is divided.
    fn segments(&self) -> Option<&CostSpans> {
        if self.split != Split::Divided {
            return None;
        }
        self.measured.facts.as_ref()?.spans.as_ref()
    }

    /// This session carrying `facts`, part of its figure, as its measurement.
    fn with_facts(&self, facts: CostFacts) -> SessionRow {
        SessionRow {
            harness: self.harness.clone(),
            session: self.session.clone(),
            spans: self.spans.clone(),
            membership: self.membership.clone(),
            measured: Measured {
                source: self.measured.source.clone(),
                facts: Some(facts),
                conflict: None,
                retained_liveness: self.measured.retained_liveness.clone(),
                retained_at: self.measured.retained_at,
            },
            split: self.split,
            overlap: self.overlap.clone(),
            participation: self.participation.clone(),
            acted: self.acted.clone(),
            liveness: self.liveness.clone(),
            agent: self.agent.clone(),
            agent_conflict: self.agent_conflict.clone(),
            parent: self.parent.clone(),
        }
    }

    /// The figure of the segments `keep` holds, by index, in the segments'
    /// basis, with each even share `allocate` names of a segment several
    /// items cover, and with `remainder`, the requests no segment holds.
    fn part(
        &self,
        table: Option<&agent_tapes_core::pricing::PriceTable>,
        keep: impl Fn(usize, &CostSegment) -> bool,
        allocate: impl Fn(&CostSegment) -> Vec<usize>,
        remainder: bool,
    ) -> Option<SessionRow> {
        let facts = self.measured.facts.as_ref()?;
        let spans = self.segments()?;
        let allocated: Vec<(CostFacts, u64)> = spans
            .segments
            .iter()
            .flat_map(|segment| {
                allocate(segment).into_iter().map(|index| {
                    (
                        span::even_share(&segment.facts, segment.work.len(), index),
                        segment.requests,
                    )
                })
            })
            .collect();
        let parts: Vec<span::Part<'_>> = spans
            .segments
            .iter()
            .enumerate()
            .filter(|(index, segment)| keep(*index, segment))
            .map(|(_, segment)| (&*segment.facts, segment.requests))
            .chain(allocated.iter().map(|(facts, requests)| (facts, *requests)))
            .chain(
                spans
                    .remainder
                    .as_deref()
                    .filter(|_| remainder)
                    .map(|facts| (facts, spans.remainder_requests)),
            )
            .collect();
        let mut share = span::sum_facts(facts, spans.whole.as_deref()?, &parts);
        price(&mut share, table);
        Some(self.with_facts(share))
    }

    /// The share of the work items `keys` hold: the segments one of them
    /// alone covers, and as `take` asks, the estimate parts given to one of
    /// them, an even share of each segment several items cover for every
    /// one of them, or what remains between items for the next item.
    fn share(
        &self,
        keys: &[&WorkKey],
        take: Take,
        table: Option<&agent_tapes_core::pricing::PriceTable>,
    ) -> Option<SessionRow> {
        let labels: BTreeSet<String> = keys.iter().map(|key| key.label()).collect();
        if take == Take::Measured || take == Take::Allocated(Allocation::Even) {
            let even = take == Take::Allocated(Allocation::Even);
            return self.part(
                table,
                |_, segment| matches!(&segment.work[..], [one] if labels.contains(one)),
                |segment| {
                    if !even || segment.work.len() < 2 {
                        return Vec::new();
                    }
                    (0..segment.work.len())
                        .filter(|index| labels.contains(&segment.work[*index]))
                        .collect()
                },
                false,
            );
        }
        let facts = self.measured.facts.as_ref()?;
        let spans = self.segments()?;
        let following = take == Take::Allocated(Allocation::Following);
        let held = |label: Option<&String>| label.is_some_and(|label| labels.contains(label));
        let mut parts: Vec<span::Part<'_>> = Vec::new();
        for (index, segment) in spans.segments.iter().enumerate() {
            if let [one] = &segment.work[..] {
                if labels.contains(one) {
                    parts.push((&*segment.facts, segment.requests));
                }
                continue;
            }
            let next = (following
                && span::unattributed_reason(&spans.segments, index) == Some(span::BETWEEN))
            .then(|| next_item(&spans.segments, index))
            .flatten();
            let to_next = held(next);
            if segment.estimates.is_empty() {
                if to_next {
                    parts.push((&*segment.facts, segment.requests));
                }
                continue;
            }
            for part in &segment.estimates {
                if held(part.work.as_ref()) || (part.work.is_none() && to_next) {
                    parts.push((&*part.facts, part.requests));
                }
            }
        }
        let mut share = span::sum_facts(facts, spans.whole.as_deref()?, &parts);
        price(&mut share, table);
        Some(self.with_facts(share))
    }

    /// The estimate parts given to `work`; with none, what the estimate
    /// leaves unattributed: its parts given to no item, the segments no
    /// single item covers that it did not cut, and the requests no segment
    /// holds.
    fn estimate_part(
        &self,
        table: Option<&agent_tapes_core::pricing::PriceTable>,
        work: Option<&str>,
    ) -> Option<SessionRow> {
        let facts = self.measured.facts.as_ref()?;
        let spans = self.segments()?;
        let mut parts: Vec<span::Part<'_>> = Vec::new();
        for segment in &spans.segments {
            if segment.work.len() == 1 {
                continue;
            }
            if segment.estimates.is_empty() {
                if work.is_none() {
                    parts.push((&*segment.facts, segment.requests));
                }
                continue;
            }
            for part in &segment.estimates {
                if part.work.as_deref() == work {
                    parts.push((&*part.facts, part.requests));
                }
            }
        }
        if work.is_none() {
            parts.extend(
                spans
                    .remainder
                    .as_deref()
                    .map(|remainder| (remainder, spans.remainder_requests)),
            );
        } else if parts.is_empty() {
            return None;
        }
        let mut share = span::sum_facts(facts, spans.whole.as_deref()?, &parts);
        price(&mut share, table);
        Some(self.with_facts(share))
    }

    /// Whether the read estimated any of this session's unattributed part.
    fn estimated(&self) -> bool {
        self.segments().is_some_and(|spans| {
            spans
                .segments
                .iter()
                .any(|segment| segment.estimates.iter().any(|part| part.work.is_some()))
        })
    }

    /// One run's figure: the segments that run alone covers.
    fn run_share(
        &self,
        dispatch: &str,
        table: Option<&agent_tapes_core::pricing::PriceTable>,
    ) -> Option<SessionRow> {
        self.part(
            table,
            |_, segment| matches!(&segment.runs[..], [one] if one == dispatch),
            |_| Vec::new(),
            false,
        )
    }

    /// What no single work item holds, by fixed reason, with the requests
    /// each part holds: segments several items cover, segments none covers,
    /// and requests whose time the recording leaves unrecorded.
    fn unattributed(
        &self,
        table: Option<&agent_tapes_core::pricing::PriceTable>,
    ) -> Vec<(&'static str, u64, SessionRow)> {
        let Some(spans) = self.segments() else {
            return Vec::new();
        };
        let reason = |index: usize| span::unattributed_reason(&spans.segments, index);
        let mut held: BTreeMap<&'static str, u64> = BTreeMap::new();
        for (index, segment) in spans.segments.iter().enumerate() {
            if let Some(reason) = reason(index) {
                *held.entry(reason).or_default() += segment.requests;
            }
        }
        let mut parts: Vec<(&'static str, u64, SessionRow)> = held
            .into_iter()
            .filter_map(|(named, requests)| {
                let row = self.part(
                    table,
                    |index, _| reason(index) == Some(named),
                    |_| Vec::new(),
                    false,
                )?;
                Some((named, requests, row))
            })
            .collect();
        if spans.remainder_requests > 0 {
            if let Some(row) = self.part(table, |_, _| false, |_| Vec::new(), true) {
                parts.push((span::UNTIMED, spans.remainder_requests, row));
            }
        }
        parts
    }

    /// The figure outside the selection's total, with why: a divided
    /// session's unattributed part and its shares of the items `selected`
    /// does not hold, or an undivided one's whole figure.
    fn outside_figure(
        &self,
        table: Option<&agent_tapes_core::pricing::PriceTable>,
        selected: &BTreeSet<String>,
    ) -> Option<(SessionRow, Vec<&'static str>)> {
        let Some(spans) = self.segments() else {
            return self.unattributed_figure(table);
        };
        let facts = self.measured.facts.as_ref()?;
        fn alone(segment: &CostSegment) -> Option<&String> {
            match &segment.work[..] {
                [one] => Some(one),
                _ => None,
            }
        }
        let outside =
            |segment: &CostSegment| alone(segment).is_none_or(|one| !selected.contains(one));
        let mut reasons: Vec<&'static str> = self
            .unattributed(table)
            .iter()
            .filter(|(_, requests, _)| *requests > 0)
            .map(|(reason, _, _)| *reason)
            .collect();
        if spans.segments.iter().any(|segment| {
            segment.requests > 0 && alone(segment).is_some_and(|one| !selected.contains(one))
        }) {
            reasons.push(span::UNSELECTED);
        }
        if spans
            .whole
            .as_ref()
            .is_some_and(|whole| whole.basis != facts.basis)
        {
            reasons.push(span::NOT_SPLITTABLE);
        }
        if reasons.is_empty() {
            return None;
        }
        let row = self.part(table, |_, segment| outside(segment), |_| Vec::new(), true)?;
        Some((row, reasons))
    }

    /// The figure outside every work item's share, with why: a divided
    /// session's unattributed part, or an undivided one's whole figure.
    fn unattributed_figure(
        &self,
        table: Option<&agent_tapes_core::pricing::PriceTable>,
    ) -> Option<(SessionRow, Vec<&'static str>)> {
        let Some(spans) = self.segments() else {
            let reason = match self.split {
                Split::Unsplit(reason) => reason,
                _ => span::INCOMPLETE_MEMBERSHIP,
            };
            return Some((self.clone(), vec![reason]));
        };
        let facts = self.measured.facts.as_ref()?;
        let mut reasons: Vec<&'static str> = self
            .unattributed(table)
            .iter()
            .filter(|(_, requests, _)| *requests > 0)
            .map(|(reason, _, _)| *reason)
            .collect();
        if spans
            .whole
            .as_ref()
            .is_some_and(|whole| whole.basis != facts.basis)
        {
            reasons.push(span::NOT_SPLITTABLE);
        }
        if reasons.is_empty() {
            return None;
        }
        let row = self.part(
            table,
            |index, _| span::unattributed_reason(&spans.segments, index).is_some(),
            |_| Vec::new(),
            true,
        )?;
        Some((row, reasons))
    }
}

/// The sessions one figure holds: those it counts whole, the shares of
/// divided sessions it counts in part, and the sessions it holds neither way.
struct Members<'a> {
    whole: Vec<&'a SessionRow>,
    shares: Vec<SessionRow>,
    shared: Vec<String>,
}

impl Members<'_> {
    fn rows(&self) -> Vec<&SessionRow> {
        self.whole
            .iter()
            .copied()
            .chain(self.shares.iter())
            .collect()
    }

    fn share_keys(&self) -> Vec<String> {
        self.shares.iter().map(SessionRow::key).collect()
    }
}

fn render_report(
    selection: &Selection,
    runs: &Runs,
    sessions: &[SessionRow],
    outside: &[Value],
    ledger: &Ledger,
    view: &View<'_>,
) -> Value {
    let table = view.prices.table();
    // An undivided session counts whole for these items when they hold
    // every work item it acted on, and for a participant when that
    // membership is complete. A divided session always joins by its items'
    // shares: its consumption no item holds is unattributed, never charged.
    let counted = |row: &SessionRow, keys: &[&WorkKey]| -> bool {
        row.split != Split::Divided
            && row
                .participation
                .as_ref()
                .is_none_or(|participation| participation.membership_complete)
            && row.membership.iter().all(|work| keys.contains(&work))
    };
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
        // its figure. A divided executor's consumption before its first
        // dispatch is outside every share.
        for row in sessions.iter().filter(|row| {
            row.participation.is_none()
                && row.membership.iter().any(|work| keys.contains(&work))
                && !(row.split == Split::Divided && !counted(row, keys))
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
    // A divided session the items do not hold whole joins with its share.
    let within = |keys: &[&WorkKey], take: Take| -> Members<'_> {
        let mut members = Members {
            whole: Vec::new(),
            shares: Vec::new(),
            shared: Vec::new(),
        };
        for row in sessions {
            if counted(row, keys) {
                members.whole.push(row);
            } else if row.membership.iter().any(|work| keys.contains(&work)) {
                match row.share(keys, take, table) {
                    Some(share) => members.shares.push(share),
                    None => members.shared.push(row.key()),
                }
            }
        }
        members
    };
    // A session these items share with other work and cannot divide has an
    // unknown share in them, so every figure it would join is withheld.
    let shared_withheld = |members: &Members<'_>| -> Vec<String> {
        members
            .shared
            .iter()
            .map(|key| {
                let reason = sessions
                    .iter()
                    .find(|row| &row.key() == key)
                    .and_then(|row| match row.split {
                        Split::Unsplit(reason) => Some(reason),
                        _ => None,
                    })
                    .unwrap_or(span::INCOMPLETE_MEMBERSHIP);
                format!("session {key} also acted on other work and is not divided ({reason}), so its share here is unknown")
            })
            .collect()
    };
    // The figure `take` gives these items, each quantity labelled `label`
    // when it is not the conserving one.
    let taken = |keys: &[&WorkKey], scope: &str, take: Take, label: Option<&str>| -> Value {
        let members = within(keys, take);
        let mut withheld = withheld_for(keys, scope);
        withheld.extend(shared_withheld(&members));
        let mut figure = totals(&members.rows(), &withheld, scope);
        if let Some(label) = label {
            for quantity in figure["quantities"].as_array_mut().into_iter().flatten() {
                quantity[label] = json!(true);
            }
        }
        figure["shares"] = json!(members.share_keys());
        figure
    };
    // A figure; when the view allocates, the allocated figure beside it;
    // and when a participant's read estimated part of what no item holds,
    // the figure with those estimates beside it.
    let figure = |keys: &[&WorkKey], scope: &str| -> Figures {
        let allocated = view.allocation.map(|allocation| {
            let mut allocated = taken(keys, scope, Take::Allocated(allocation), Some("allocated"));
            allocated["allocation"] = json!(allocation.name());
            allocated
        });
        let estimates = sessions
            .iter()
            .any(|row| row.estimated() && row.membership.iter().any(|work| keys.contains(&work)));
        let estimated = estimates.then(|| {
            let mut estimated = taken(keys, scope, Take::Estimated, Some("estimated"));
            estimated["estimate"] = estimate_label(view.idle_ms);
            estimated
        });
        Figures {
            conserving: taken(keys, scope, Take::Measured, None),
            allocated,
            estimated,
        }
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
                    "share": !counted(row, keys) && row.split == Split::Divided,
                    "totals": totals(&[row], &[], &format!("session {}", row.key())),
                }))
            })
            .collect()
    };
    let row_of = |key: &str| sessions.iter().find(|row| row.key() == key);

    let mut work = Vec::new();
    for item in &selection.items {
        let shared = within(&[&item.key], Take::Measured).shared;
        let item_runs: Vec<Value> = runs
            .get(&item.key)
            .into_iter()
            .flatten()
            .map(|run| {
                let session = (!run.conflicting).then(|| run.candidates.first()).flatten();
                let row = session.and_then(|key| row_of(key));
                let divided = row.filter(|row| row.split == Split::Divided);
                let per_run = match &session {
                    _ if run.invalid => "invalid-binding",
                    _ if run.conflicting => "conflicting-bindings",
                    None => "unbound",
                    Some(_) if divided.is_some() => "split",
                    Some(_) if row.is_some_and(|row| row.spans.len() > 1) => "shared-among-spans",
                    Some(_) => "whole-session",
                };
                let mut entry = json!({"dispatch_event_id": run.dispatch_event_id, "round": run.round,
                    "session": session, "binding_candidates": run.candidates, "per_run": per_run});
                if let Some(share) = divided.and_then(|row| row.run_share(&run.dispatch_event_id, table)) {
                    entry["totals"] = totals(&[&share], &[], &format!("run {}", run.dispatch_event_id));
                }
                entry
            })
            .collect();
        let Figures {
            conserving: direct,
            allocated,
            estimated,
        } = figure(&[&item.key], "direct");
        let mut entry = json!({
            "key": item.key.label(),
            "runs": item_runs,
            "unbound": unbound_in(&item.key),
            "direct": direct,
            "shared": shared,
            "participants": participants_in(&[&item.key]),
        });
        if let Some(allocated) = allocated {
            entry["allocated"] = json!({"direct": allocated});
        }
        if let Some(estimated) = estimated {
            entry["estimated"] = json!({"direct": estimated});
        }
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
            let Figures {
                conserving: mut totals,
                allocated,
                estimated,
            } = figure(&keys, "inclusive");
            totals["members"] = json!([item.key.label(), source.label()]);
            totals["shared"] = json!(within(&keys, Take::Measured).shared);
            totals["participants"] = json!(participants_in(&keys));
            entry["inclusive"] = totals;
            if let Some(allocated) = allocated {
                entry["allocated"]["inclusive"] = allocated;
            }
            if let Some(estimated) = estimated {
                entry["estimated"]["inclusive"] = estimated;
            }
        }
        work.push(entry);
    }

    let all: Vec<&WorkKey> = selection.items.iter().map(|item| &item.key).collect();
    let union_members = within(&all, Take::Measured);
    let Figures {
        conserving: mut union,
        allocated: union_allocated,
        estimated: union_estimated,
    } = figure(&all, "union");
    union["members"] = json!(all.iter().map(|key| key.label()).collect::<Vec<_>>());
    union["shared"] = json!(union_members.shared);
    union["participants"] = json!(participants_in(&all));
    if let Some(allocated) = &union_allocated {
        union["allocated"] = allocated.clone();
    }
    if let Some(estimated) = union_estimated {
        union["estimated"] = estimated;
    }

    let session_rows: Vec<Value> = sessions
        .iter()
        .map(|row| {
            let mut value = session_json(row, table);
            if let Some(family) = family_json(row, sessions, view) {
                value["family"] = family;
            }
            value
        })
        .collect();
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
    let mut union_withheld = withheld_for(&all, "union");
    union_withheld.extend(shared_withheld(&union_members));
    let mut report = json!({
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
                    "statement": "a participant is a session no dispatch binds whose ledger events act on the selected work; its span on each work item runs from its first to its last event there, and it may also act in other repositories' ledgers, which this ledger cannot see",
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
    });
    report["prices"] = prices_json(view.prices);
    report["table"] = cost_table(&report, sessions, &union_members, &union_withheld, view);
    report
}

/// Which price table the view priced with, or why it has none.
fn prices_json(prices: &agent_tapes_core::pricing::PriceLookup) -> Value {
    use agent_tapes_core::pricing::PriceLookup;
    let schema = agent_tapes_core::pricing::PRICE_TABLE_SCHEMA;
    match prices {
        PriceLookup::Loaded(loaded) => json!({"status": "loaded", "schema": loaded.table.schema,
            "origin": kebab(loaded.origin), "currency": loaded.table.currency,
            "models": loaded.table.models.len()}),
        PriceLookup::Absent => json!({"status": "absent", "schema": schema}),
        PriceLookup::DefaultInvalid { .. } => {
            json!({"status": "invalid", "schema": schema, "origin": "default"})
        }
    }
}

/// The footprint session's whole figure beside the work it selects, or why
/// there is none.
fn footprint_figure(report: &Value, footprint: &str) -> Value {
    let row = report["table"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["key"] == footprint);
    let figure = row.map(|row| {
        let tokens = super::cost_table::cell_text(&row["tokens"], false);
        let list = super::cost_table::cell_text(&row["list"], true);
        let usd = super::cost_table::cell_text(&row["usd"], true);
        [
            tokens,
            if list.is_empty() {
                String::new()
            } else {
                format!("list {list}")
            },
            usd,
            row["status"].as_str().unwrap_or_default().to_string(),
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
    });
    json!({"session": footprint, "row": row, "figure": figure})
}

/// Each row's session id, cut to the shortest prefix of at least eight
/// characters no other row's different id shares; the agent column tells
/// apart one id under two harnesses. Time-ordered ids share long prefixes,
/// so the cut is per view.
fn short_ids(sessions: &[SessionRow]) -> Vec<String> {
    // A native child's key is its parent's id and its agent id: each part is
    // shortened among the ids in its own position.
    let parts = |row: &SessionRow| match row.session.split_once("::") {
        Some((parent, agent)) => (parent.to_string(), Some(agent.to_string())),
        None => (row.session.clone(), None),
    };
    let held: Vec<(String, Option<String>)> = sessions.iter().map(parts).collect();
    let shortest = |id: &str, among: &mut dyn Iterator<Item = &str>| {
        let others: Vec<&str> = among.filter(|other| *other != id).collect();
        let chars: Vec<char> = id.chars().collect();
        (8..chars.len())
            .map(|length| chars[..length].iter().collect::<String>())
            .find(|prefix| {
                others
                    .iter()
                    .all(|other| !other.starts_with(prefix.as_str()))
            })
            .unwrap_or_else(|| id.to_string())
    };
    held.iter()
        .map(|(session, agent)| {
            let head = shortest(session, &mut held.iter().map(|(other, _)| other.as_str()));
            match agent {
                Some(agent) => format!(
                    "{head}::{}",
                    shortest(
                        agent,
                        &mut held.iter().filter_map(|(_, other)| other.as_deref())
                    )
                ),
                None => head,
            }
        })
        .collect()
}

/// The groupings `--by` accepts.
pub const GROUPINGS: [&str; 6] = ["agent", "role", "model", "harness", "change", "session"];

/// The table's columns: the disjoint token total, the list-price estimate,
/// and the harness-written dollar estimate.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Tokens,
    List,
    Usd,
}

impl Column {
    const ALL: [Column; 3] = [Column::Tokens, Column::List, Column::Usd];

    fn name(self) -> &'static str {
        match self {
            Column::Tokens => "tokens",
            Column::List => "list",
            Column::Usd => "usd",
        }
    }
}

/// The combined figure of one table column.
fn column(totals: &Value, column: Column, partition: bool) -> Vec<&Value> {
    totals["quantities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|quantity| {
            quantity.get("partition").is_some() == partition
                && match column {
                    Column::Usd => quantity["kind"] == USD_KIND,
                    Column::List => quantity["kind"] == LIST_KIND,
                    Column::Tokens => {
                        quantity["categories"] == "disjoint" && quantity["category"] == "total"
                    }
                }
        })
        .collect()
}

/// One table cell: the figure's status and the amount, known subtotal, or
/// bound it shows.
fn cell(quantity: Option<&Value>) -> Value {
    let Some(quantity) = quantity else {
        return json!({"status": "absent"});
    };
    let mut cell = json!({"status": quantity["status"]});
    for field in ["amount", "known_subtotal", "bound", "known_bound"] {
        if !quantity[field].is_null() {
            cell[field] = quantity[field].clone();
        }
    }
    cell
}

/// The status word of a figure from its cells. A money column with nothing
/// recorded, as for a harness that writes no estimate or a figure no table
/// prices, qualifies nothing.
fn figure_status(tokens: &Value, usd: &Value, list: &Value) -> &'static str {
    let recorded = |cell: &Value| {
        ["amount", "known_subtotal", "bound", "known_bound"]
            .iter()
            .any(|field| !cell[field].is_null())
    };
    let word = |cell: &Value| match cell["status"].as_str() {
        Some("complete") => 0,
        Some("withheld" | "refused") => 2,
        _ if recorded(cell) => 1,
        _ => 3,
    };
    let money = |cell: &Value| {
        if cell["status"] == "absent" && !recorded(cell) {
            0
        } else {
            word(cell)
        }
    };
    match word(tokens).max(money(usd)).max(money(list)) {
        0 => "complete",
        1 => "subtotal",
        2 => "withheld",
        _ => "absent",
    }
}

/// The table's view of a figure: its columns, their status word, and each
/// accounting partition when the combined figure spans several.
fn table_figure(totals: &Value) -> Value {
    let tokens = cell(column(totals, Column::Tokens, false).first().copied());
    let list = cell(column(totals, Column::List, false).first().copied());
    let usd = cell(column(totals, Column::Usd, false).first().copied());
    let partitions: Vec<Value> = Column::ALL
        .into_iter()
        .flat_map(|named| {
            column(totals, named, true)
                .into_iter()
                .map(move |quantity| {
                    json!({"column": named.name(), "harness": quantity["harness"],
                    "accounting_basis": quantity["accounting_basis"], "cell": cell(Some(quantity))})
                })
        })
        .collect();
    let status = figure_status(&tokens, &usd, &list);
    json!({"status": status, "tokens": tokens, "list": list, "usd": usd, "partitions": partitions})
}

fn group_value(row: &SessionRow, grouping: &str) -> String {
    match grouping {
        "agent" => row.agent_label(),
        "role" => row.roles().join(", "),
        "model" => row
            .agent
            .as_ref()
            .and_then(|(coordinate, _)| model_of(coordinate))
            .unwrap_or_else(|| "unestablished".to_string()),
        "harness" => row.harness.clone(),
        _ => row.key(),
    }
}

fn membership_label(row: &SessionRow) -> String {
    row.membership
        .iter()
        .map(WorkKey::label)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The rows the text table shows, the total it ends with, what of each
/// session is outside that total and why, a count of the qualifications
/// `--detail` explains, and one subtotal per group. The total is the union
/// figure, with its status.
fn cost_table(
    report: &Value,
    sessions: &[SessionRow],
    union: &Members<'_>,
    union_withheld: &[String],
    view: &View<'_>,
) -> Value {
    let table = view.prices.table();
    let whole = |row: &SessionRow| union.whole.iter().any(|member| member.key() == row.key());
    let share = |row: &SessionRow| union.shares.iter().any(|member| member.key() == row.key());
    let short = short_ids(sessions);
    let rows: Vec<Value> = sessions
        .iter()
        .zip(&short)
        .map(|(row, short)| {
            let figure = table_figure(&totals(&[row], &[], &format!("session {}", row.key())));
            let running = row.liveness.as_ref().is_some_and(SessionLiveness::running);
            let status = if row.measured.facts.is_none() {
                "unmeasured".to_string()
            } else if share(row) {
                format!("split ×{}", row.membership.len())
            } else if !whole(row) {
                format!("shared ×{}", row.membership.len())
            } else if running {
                "live".to_string()
            } else {
                figure["status"].as_str().unwrap_or_default().to_string()
            };
            json!({"key": row.key(), "agent": row.agent_label(), "roles": row.roles(),
                "session": short,
                "runs": row.spans.len(), "spans": row.membership.len(),
                "counted": whole(row), "share": share(row), "status": status,
                "tokens": figure["tokens"], "list": figure["list"], "usd": figure["usd"]})
        })
        .collect();
    let selected: BTreeSet<String> = report["union"]["members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|label| label.as_str().map(str::to_string))
        .collect();
    let unattributed: Vec<(SessionRow, Vec<&'static str>, &String)> = sessions
        .iter()
        .zip(&short)
        .filter(|(row, _)| !whole(row))
        .filter_map(|(row, short)| {
            let (figure, reasons) = row.outside_figure(table, &selected)?;
            Some((figure, reasons, short))
        })
        .collect();
    let coverage = &report["coverage"];
    let count = |value: &Value| {
        value.as_array().map_or(0, Vec::len) + value.as_object().map_or(0, serde_json::Map::len)
    };
    let qualifications = json!({
        "measurement": count(&coverage["measurement"]["sessions"]),
        "assignment": count(&coverage["assignment"]["reasons"]),
        "identity": (coverage["identity"]["status"] != "complete") as usize,
        "selection": count(&coverage["selection"]["records"]),
        "ledger": count(&coverage["ledger"]["records"]),
    });
    let total_qualifications: u64 = qualifications
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, count)| count.as_u64())
        .sum();
    // Each union row with the change it is counted for. Under `change`, a
    // divided session's union share is its share of each selected change,
    // and a session counted whole is counted for its whole membership.
    let mut counted_for: Vec<(String, std::borrow::Cow<'_, SessionRow>)> = union
        .whole
        .iter()
        .map(|row| (membership_label(row), std::borrow::Cow::Borrowed(*row)))
        .collect();
    if view.by.iter().any(|grouping| grouping == "change") {
        for share in &union.shares {
            let Some(row) = sessions.iter().find(|row| row.key() == share.key()) else {
                continue;
            };
            for key in row
                .membership
                .iter()
                .filter(|key| selected.contains(&key.label()))
            {
                if let Some(part) = row.share(&[key], Take::Measured, table) {
                    counted_for.push((key.label(), std::borrow::Cow::Owned(part)));
                }
            }
        }
    } else {
        counted_for.extend(
            union
                .shares
                .iter()
                .map(|row| (membership_label(row), std::borrow::Cow::Borrowed(row))),
        );
    }
    let mut groups: BTreeMap<Vec<String>, Vec<&SessionRow>> = BTreeMap::new();
    if !view.by.is_empty() {
        for (change, row) in &counted_for {
            groups
                .entry(
                    view.by
                        .iter()
                        .map(|grouping| match grouping.as_str() {
                            "change" => change.clone(),
                            _ => group_value(row, grouping),
                        })
                        .collect(),
                )
                .or_default()
                .push(&**row);
        }
    }
    let groups: Vec<Value> = groups
        .into_iter()
        .map(|(values, members)| {
            let mut figure = table_figure(&totals(&members, union_withheld, "group"));
            figure["by"] = json!(view
                .by
                .iter()
                .zip(&values)
                .map(|(grouping, value)| (grouping.clone(), json!(value)))
                .collect::<serde_json::Map<_, _>>());
            figure["sessions"] = json!(members.iter().map(|row| row.key()).collect::<Vec<_>>());
            figure
        })
        .collect();
    let mut total = table_figure(&report["union"]);
    total["scope"] = json!("union");
    let held: Vec<&SessionRow> = unattributed.iter().map(|(row, _, _)| row).collect();
    let mut outside = table_figure(&totals(&held, &[], "unattributed shared"));
    outside["sessions"] = json!(unattributed
        .iter()
        .map(|(row, reasons, short)| {
            let mut figure = table_figure(&totals(&[row], &[], &format!("session {}", row.key())));
            figure["key"] = json!(row.key());
            figure["session"] = json!(short);
            figure["reasons"] = json!(reasons);
            figure
        })
        .collect::<Vec<_>>());
    let short_of = |key: &str| {
        sessions
            .iter()
            .zip(&short)
            .find(|(row, _)| row.key() == key)
            .map(|(_, short)| short.clone())
    };
    let families: Vec<Value> = report["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["family"].is_object())
        .map(|row| {
            let family = &row["family"];
            let key = row["key"].as_str().unwrap_or_default();
            let mut unassigned = table_figure(&family["unassigned"]["totals"]);
            unassigned["children"] = family["unassigned"]["children"].clone();
            unassigned["unmeasured"] = family["unassigned"]["unmeasured"].clone();
            let residual: Vec<Value> = family["reconciliation"]["residual"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|counter| counter["status"] != "reconciled")
                .map(|counter| json!({"counter": counter["counter"], "difference": counter["difference"], "status": counter["status"]}))
                .collect();
            json!({
                "key": key,
                "session": short_of(key),
                "bound": family["bound"].as_array().into_iter().flatten()
                    .filter_map(|key| short_of(key.as_str()?)).collect::<Vec<_>>(),
                "unassigned": unassigned,
                "elsewhere": family["children"].as_array().into_iter().flatten()
                    .filter(|child| child["bound"] == true && child["row"] != true).count(),
                "missing": family["children"].as_array().into_iter().flatten()
                    .filter(|child| child["recording"] == "missing").count(),
                "reconciliation": family["reconciliation"]["status"],
                "residual": residual,
            })
        })
        .collect();
    let mut value = json!({
        "rows": rows,
        "total": total,
        "families": families,
        "unattributed": outside,
        "coverage": {"qualifications": total_qualifications, "by_kind": qualifications},
        "by": view.by,
        "groups": groups,
        "prices": report["prices"],
    });
    if report["union"]["allocated"].is_object() {
        let mut allocated = table_figure(&report["union"]["allocated"]);
        allocated["allocation"] = report["union"]["allocated"]["allocation"].clone();
        value["allocated"] = allocated;
    }
    if report["union"]["estimated"].is_object() {
        let mut estimated = table_figure(&report["union"]["estimated"]);
        estimated["estimate"] = report["union"]["estimated"]["estimate"].clone();
        value["estimated"] = estimated;
    }
    value
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

/// How a session's figure divides among the work its spans cover: each
/// segment with its covering items and runs, each item's share and run's
/// figure, the unattributed part by reason, and the whole in the segments'
/// basis, which the shares and the unattributed part add up to.
fn split_json(row: &SessionRow, table: Option<&agent_tapes_core::pricing::PriceTable>) -> Value {
    match row.split {
        Split::Whole => return json!({"status": "whole"}),
        Split::Unsplit(reason) => return json!({"status": "unsplit", "reason": reason}),
        Split::Divided => {}
    }
    let Some(spans) = row.segments() else {
        return json!({"status": "unsplit", "reason": span::UNMEASURED});
    };
    let scope = |what: &str| format!("session {} {what}", row.key());
    let segments: Vec<Value> = spans
        .segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            json!({"from": segment.from, "to": segment.to, "work": segment.work,
                "runs": segment.runs, "requests": segment.requests,
                "unattributed": span::unattributed_reason(&spans.segments, index)})
        })
        .collect();
    let shares: Vec<Value> = row
        .membership
        .iter()
        .filter_map(|work| {
            let share = row.share(&[work], Take::Measured, table)?;
            Some(json!({"work": work.label(), "totals": totals(&[&share], &[], &scope(&format!("share of {}", work.label())))}))
        })
        .collect();
    let runs: Vec<Value> = row
        .spans
        .iter()
        .filter_map(|run| {
            let share = row.run_share(&run.dispatch_event_id, table)?;
            Some(json!({"dispatch_event_id": run.dispatch_event_id,
                "totals": totals(&[&share], &[], &format!("run {}", run.dispatch_event_id))}))
        })
        .collect();
    let parts: Vec<Value> = row
        .unattributed(table)
        .iter()
        .map(|(reason, requests, part)| {
            json!({"reason": reason, "requests": requests,
                "totals": totals(&[part], &[], &scope(&format!("unattributed {reason}")))})
        })
        .collect();
    let unattributed = row.unattributed_figure(table).map(|(figure, reasons)| {
        json!({"reasons": reasons, "totals": totals(&[&figure], &[], &scope("unattributed"))})
    });
    let whole = spans
        .whole
        .as_deref()
        .zip(row.measured.facts.as_ref())
        .map(|(whole, facts)| {
            let mut figure = span::sum_facts(facts, whole, &[(whole, 1)]);
            price(&mut figure, table);
            totals(
                &[&row.with_facts(figure)],
                &[],
                &scope("in the segments' basis"),
            )
        });
    let mut value = json!({"status": "divided", "basis": spans.whole.as_ref().and_then(|whole| whole.basis.clone()),
        "segments": segments, "shares": shares, "runs": runs,
        "unattributed": unattributed, "unattributed_parts": parts, "whole": whole,
        "conservation": conservation(row, spans),
        "statement": "the shares and the unattributed part add up to the whole in the segments' basis, per counter, unless conservation names a counter they do not"});
    if let Some(estimate) = estimate_json(row, spans, table) {
        value["estimate"] = estimate;
    }
    value
}

/// A participant's working-stretch estimate of its unattributed part: each
/// item's estimated share, what stays unattributed, and whether the measured
/// shares, the estimated shares and that remainder add up to the whole.
fn estimate_json(
    row: &SessionRow,
    spans: &CostSpans,
    table: Option<&agent_tapes_core::pricing::PriceTable>,
) -> Option<Value> {
    let estimate = spans.estimate.as_ref()?;
    let mut value = estimate_label(estimate.idle_ms);
    if let Some(reason) = &estimate.withheld {
        value["status"] = json!("withheld");
        value["reason"] = json!(reason);
        return Some(value);
    }
    let scope = |what: &str| format!("session {} {what}", row.key());
    let shares: Vec<Value> = row
        .membership
        .iter()
        .filter_map(|work| {
            let label = work.label();
            let share = row.estimate_part(table, Some(&label))?;
            Some(json!({"work": label, "estimated": true,
                "totals": totals(&[&share], &[], &scope(&format!("estimated share of {label}")))}))
        })
        .collect();
    let remaining = row
        .estimate_part(table, None)
        .map(|part| totals(&[&part], &[], &scope("unattributed after estimates")));
    // The estimate parts stand in for the segments they cut, so the parts
    // adding up to the whole is the estimate's own conservation.
    let mut cut = spans.clone();
    cut.segments = spans
        .segments
        .iter()
        .flat_map(|segment| {
            if segment.estimates.is_empty() {
                return vec![segment.clone()];
            }
            segment
                .estimates
                .iter()
                .map(|part| CostSegment {
                    from: part.from,
                    to: part.to,
                    work: part.work.iter().cloned().collect(),
                    runs: Vec::new(),
                    requests: part.requests,
                    facts: part.facts.clone(),
                    estimates: Vec::new(),
                })
                .collect()
        })
        .collect();
    value["status"] = json!("estimated");
    value["shares"] = json!(shares);
    value["unattributed"] = json!(remaining);
    value["conservation"] = conservation(row, &cut);
    Some(value)
}

/// Whether the segment figures add up to the whole, and the counters where
/// they do not.
fn conservation(row: &SessionRow, spans: &CostSpans) -> Value {
    let held = match row.measured.facts.as_ref() {
        Some(facts) => span::conservation(facts, spans),
        None => span::Conservation {
            uncompared: vec!["whole".to_string()],
            ..Default::default()
        },
    };
    let status = if !held.differs.is_empty() {
        "not-conserved"
    } else if !held.uncompared.is_empty() {
        "unestablished"
    } else {
        "conserved"
    };
    json!({"status": status, "differs": held.differs, "uncompared": held.uncompared})
}

fn session_json(row: &SessionRow, table: Option<&agent_tapes_core::pricing::PriceTable>) -> Value {
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
        "agent": row.agent_label(),
        "agent_source": row.agent.as_ref().map(|(_, source)| *source),
        "agent_conflict": row.agent_conflict.as_ref().map(|(coordinate, source)| json!({"coordinate": coordinate, "source": source})),
        "roles": row.roles(),
        "spans": row.spans.iter().map(|span| json!({
            "dispatch_event_id": span.dispatch_event_id,
            "dispatched_at": span.dispatched_at,
            "ended_at": span.ended_at,
            "forked_from": span.forked_from,
            "round": span.round,
            "work": span.work.label(),
        })).collect::<Vec<_>>(),
        "membership": row.membership.iter().map(WorkKey::label).collect::<Vec<_>>(),
        "per_run": if row.participation.is_some() { "not-a-run" } else if row.spans.len() > 1 && row.split == Split::Divided { "split" } else if row.spans.len() > 1 { "shared-among-spans" } else { "whole-session" },
        "measurement": measurement,
        "totals": totals(&[row], &[], &scope),
        "split": split_json(row, table),
    });
    if let Some(participation) = &row.participation {
        value["participant"] = json!({
            "roles": participation.roles.iter().map(|role| role.name()).collect::<Vec<_>>(),
            "spans": row.membership.len(),
        });
    }
    if let Some(liveness) = &row.liveness {
        value["measurement"]["liveness"] = liveness.json();
    }
    if let Some(parent) = &row.parent {
        value["parent"] = json!(format!("{}:{parent}", row.harness));
    }
    value
}

fn describe_quantity(quantity: &Value) -> String {
    let unit = quantity["unit"].as_str().unwrap_or_default();
    let label = match quantity["kind"].as_str() {
        Some(USD_KIND) => "harness-calculated estimate".to_string(),
        Some(LIST_KIND) => "list-price estimate".to_string(),
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
        (_, Value::Number(known)) => match quantity["bound"]
            .get("at_most")
            .or_else(|| quantity["known_bound"].get("at_most"))
        {
            Some(most) => {
                format!("{label} {status} (known subtotal {known} {unit}, at most {most} {unit})")
            }
            None => format!("{label} {status} (known subtotal {known} {unit})"),
        },
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
        let shares: Vec<&str> = item["direct"]["shares"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !shares.is_empty() {
            emit!(out, "  span shares: {}", shares.join(", "));
        }
        if item["estimated"]["direct"].is_object() {
            write_totals(out, "  ", "with estimates", &item["estimated"]["direct"]);
        }
        if item["allocated"]["direct"].is_object() {
            let label = format!(
                "allocated {}",
                item["allocated"]["direct"]["allocation"]
                    .as_str()
                    .unwrap_or_default()
            );
            write_totals(out, "  ", &label, &item["allocated"]["direct"]);
        }
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
    if report["union"]["estimated"].is_object() {
        write_totals(out, "  ", "with estimates", &report["union"]["estimated"]);
    }
    if report["union"]["allocated"].is_object() {
        let label = format!(
            "allocated {}",
            report["union"]["allocated"]["allocation"]
                .as_str()
                .unwrap_or_default()
        );
        write_totals(out, "  ", &label, &report["union"]["allocated"]);
    }
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
        write_split(out, &session["split"]);
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

/// A divided session's segments, shares and unattributed part, or why a
/// session with several spans is not divided.
fn write_split(out: &mut String, split: &Value) {
    match split["status"].as_str() {
        Some("unsplit") => emit!(
            out,
            "    not divided: {}",
            split["reason"].as_str().unwrap_or_default()
        ),
        Some("divided") => {
            emit!(
                out,
                "    divided into {} segments [{}]",
                split["segments"].as_array().map_or(0, Vec::len),
                split["basis"]
                    .as_str()
                    .unwrap_or("accounting basis unknown")
            );
            if split["conservation"]["status"] == "not-conserved" {
                emit!(
                    out,
                    "    the segments do not add up to the whole in: {}",
                    role_names(&split["conservation"]["differs"])
                );
            }
            for share in split["shares"].as_array().into_iter().flatten() {
                let label = format!("share of {}", share["work"].as_str().unwrap_or_default());
                write_totals(out, "      ", &label, &share["totals"]);
            }
            for run in split["runs"].as_array().into_iter().flatten() {
                let label = format!(
                    "run {}",
                    run["dispatch_event_id"].as_str().unwrap_or_default()
                );
                write_totals(out, "      ", &label, &run["totals"]);
            }
            if split["unattributed"].is_object() {
                let label = format!(
                    "unattributed ({})",
                    role_names(&split["unattributed"]["reasons"])
                );
                write_totals(out, "      ", &label, &split["unattributed"]["totals"]);
            }
        }
        _ => {}
    }
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

    /// A model share's estimate is the estimate tapes gives that share of a
    /// usage answer, category by category: a Codex share whose cache reads
    /// exceed its input leaves uncached input `undetermined` and nothing
    /// else. Rejects pricing a share as the whole figure of its own
    /// restatement, which also names that category a missing counter and an
    /// unattributed part.
    #[test]
    fn a_share_estimate_carries_only_the_reasons_tapes_gives_the_share() {
        use agent_tapes_core::model::{TokenConvention, Tokens};
        use agent_tapes_core::pricing::{PriceTable, PricedFigure, RequestTerms};
        let table = PriceTable::parse(
            r#"schema = "tapes-price-table/1"
currency = "USD"

[[models]]
id = "cheap-model"
effective = 2026-01-01
source = "fixture"
per_million = { uncached_input = 1.0, cache_read = 0.1, cache_write_5m = 1.25, output = 4.0 }
"#,
        )
        .unwrap();
        let convention = TokenConvention::of("codex").unwrap();
        let usage = agent_tapes_core::usage::ModelUsage {
            model: "cheap-model".to_string(),
            variant: None,
            tokens: Some(Tokens {
                input: Some(1_000_000),
                output: Some(0),
                reasoning: Some(0),
                cache_read: Some(2_000_000),
                cache_write: Some(0),
            }),
            cost: None,
            cost_coverage: None,
            request_count: None,
            measurement_coverage: BTreeMap::new(),
            measurement_incomplete: Vec::new(),
            measurement_completeness: MeasurementCompleteness::default(),
            list_price_estimate: None,
            cache_write_by_ttl: None,
        };
        let shares = std::slice::from_ref(&usage);
        let tapes = PricedFigure {
            disjoint: None,
            convention: Some(convention),
            model: None,
            selections: &[],
            observation: None,
            by_model: Some(shares),
            terms: RequestTerms {
                requests: None,
                billing: None,
                from: None,
                to: None,
            },
        }
        .estimate(&table)
        .by_model
        .remove(0)
        .expect("tapes prices the share");
        let [share] = &model_shares(shares, &MeasurementCompleteness::default(), convention)[..]
        else {
            panic!("one share");
        };
        let disjoint = tapes_disjoint(&share.disjoint).unwrap();
        let arc = price_model(
            &disjoint,
            &share.model,
            false,
            &table,
            &CostRequestTerms::default(),
        )
        .finish()
        .expect("arc prices the share");
        assert_eq!(
            arc.measurement_completeness, tapes.measurement_completeness,
            "{tapes:#?}"
        );
        assert_eq!(arc.amount, tapes.amount);
    }

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
