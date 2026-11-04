//! Derived ledger analytics: the durations and counts the append-only
//! ledger already holds, projected per change and aggregated. Pure
//! derivation — no writes, no new events.

use super::*;
use crate::model::{
    BlockerRef, ClaimStage, Closure, Event, JournalRefVia, KeptKind, Payload, ReviewCause,
    Severity, VerifyResult,
};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

/// Which changes `arc stats` reports over.
pub enum StatsSelection {
    All,
    Change(String),
    Tag(String),
}

/// What `arc stats` reports over the selection.
pub enum StatsView {
    /// One row per change, with aggregate percentiles.
    Changes,
    /// One row per delegated identity.
    ByModel,
    /// How often each provenance record was written where it could have been.
    Provenance,
    /// Each integrated change's final landing, split into phases.
    RoundTrip,
}

#[derive(Serialize)]
struct StatsOutput {
    schema: &'static str,
    changes: Vec<ChangeStats>,
    aggregate: Aggregate,
}

#[derive(Serialize)]
struct ChangeStats {
    change_id: String,
    slug: String,
    state: &'static str,
    /// open → integrated wall time; absent until integrated.
    wall_time_seconds: Option<u64>,
    /// Seconds spent in each typed executor stage.
    stage_seconds: BTreeMap<String, u64>,
    /// snapshot → first verdict on any patchset; absent until reviewed.
    review_latency_seconds: Option<u64>,
    /// Observed gate wall time (summed runs); attested evidence excluded.
    gate_seconds: BTreeMap<String, u64>,
    findings_by_severity: BTreeMap<String, usize>,
    review_rounds_by_cause: BTreeMap<String, usize>,
    blocks_by_kind: BTreeMap<String, usize>,
    changes_requested_rounds: usize,
    completed_rework_rounds: usize,
    reworked: bool,
    first_pass_approval: bool,
    patchset_count: usize,
}

#[derive(Serialize)]
struct Aggregate {
    changes: usize,
    stage: BTreeMap<String, Percentiles>,
    gate: BTreeMap<String, Percentiles>,
    /// p90 per stage rounded up to a clean duration — a suggestion for
    /// `stage-budget` tuning, never applied automatically.
    suggested_stage_budgets: BTreeMap<String, u64>,
    review_rounds_by_cause: BTreeMap<String, usize>,
    blocks_by_kind: BTreeMap<String, usize>,
    changes_reworked: usize,
    first_pass_approvals: usize,
    completed_rework_rounds: usize,
}

#[derive(Serialize)]
struct Percentiles {
    samples: usize,
    median_seconds: u64,
    p90_seconds: u64,
}

/// One executor identity's contribution across the selection.
///
/// The interesting number is not a ranking. It is the pair "this identity
/// produced N patchsets and caused M rework rounds", because that ratio is
/// what an executor tier actually costs — and the cost is billed to whichever
/// pool the reviewers run on.
#[derive(Serialize)]
struct ModelStats {
    /// The delegated subject recorded on the event, verbatim. Model identity
    /// is a convention rather than a schema: leads write it into
    /// `--on-behalf-of`, and nothing enforces its shape.
    identity: String,
    changes: usize,
    /// Patchsets contributed as implementer.
    patchsets: usize,
    /// Rework rounds opened against a patchset this identity contributed —
    /// what its work cost, not what it cleaned up. The revision that answers a
    /// round belongs to whoever wrote it, and crediting the round there would
    /// charge the fixer for the defect. A round is a revision cycle, so
    /// several changes-requested verdicts on one patchset count once, exactly
    /// as they do per change.
    rework_rounds_caused: usize,
    /// Verdicts issued as reviewer, before integration and after it. An audit
    /// is a review that happened; leaving it out would understate exactly the
    /// reviewers a debt-carrying change depended on.
    verdicts: usize,
}

#[derive(Serialize)]
struct ByModelOutput {
    schema: &'static str,
    models: Vec<ModelStats>,
}

/// The subject an event is attributed to. A lead runs the snapshot ceremony on
/// an executor's behalf, so attributing by actor would credit the lead for
/// every line the executor wrote. Where no subject was recorded the row is
/// unknown rather than the lead's, and is counted in its own row rather than
/// distributed silently.
const UNATTRIBUTED: &str = "(unattributed)";

fn subject(event: &Event) -> &str {
    event
        .on_behalf_of
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(UNATTRIBUTED)
}

fn by_model(store: &Store, change_ids: &[String], json: bool) -> Result<()> {
    #[derive(Default)]
    struct Row {
        changes: BTreeSet<String>,
        patchsets: usize,
        rework_rounds: usize,
        verdicts: usize,
    }
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for change_id in change_ids {
        let events = store.load_events(change_id)?;
        // Who contributed each patchset, so a rework round lands on the
        // identity whose work opened it rather than on the reviewer.
        let mut patchset_subject: BTreeMap<&str, &str> = BTreeMap::new();
        for event in &events {
            // Touching a change is any recorded contribution to it, so an
            // identity that only filed findings or ran gates has a row rather
            // than vanishing.
            let row = rows.entry(subject(event).to_string()).or_default();
            row.changes.insert(change_id.clone());
            match &event.payload {
                Payload::PatchsetAdded { patchset_id, .. } => {
                    patchset_subject.insert(patchset_id.as_str(), subject(event));
                    row.patchsets += 1;
                }
                Payload::VerdictRecorded { .. } | Payload::AuditVerdictRecorded { .. } => {
                    row.verdicts += 1
                }
                _ => {}
            }
        }
        for patchset_id in derive_rework(&events).reworked_patchsets {
            let subject = patchset_subject
                .get(patchset_id.as_str())
                .copied()
                .unwrap_or(UNATTRIBUTED);
            rows.entry(subject.to_string()).or_default().rework_rounds += 1;
        }
    }

    let models: Vec<ModelStats> = rows
        .into_iter()
        .map(|(identity, row)| ModelStats {
            identity,
            changes: row.changes.len(),
            patchsets: row.patchsets,
            rework_rounds_caused: row.rework_rounds,
            verdicts: row.verdicts,
        })
        .collect();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ByModelOutput {
                schema: "arc-stats-by-model/1",
                models,
            })?
        );
        return Ok(());
    }
    println!(
        "{:<40} {:>8} {:>10} {:>8} {:>9}",
        "identity", "changes", "patchsets", "caused-rw", "verdicts"
    );
    for model in &models {
        println!(
            "{:<40} {:>8} {:>10} {:>8} {:>9}",
            truncate(&model.identity, 40),
            model.changes,
            model.patchsets,
            model.rework_rounds_caused,
            model.verdicts
        );
    }
    Ok(())
}

/// A count and the population it was counted in, so a reader sees a rate and
/// what it is a rate of.
#[derive(Serialize, Default, Clone, Copy)]
struct Ratio {
    count: usize,
    of: usize,
}

impl Ratio {
    fn tally(&mut self, hit: bool) {
        self.of += 1;
        self.count += usize::from(hit);
    }
}

impl std::fmt::Display for Ratio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} of {}", self.count, self.of)
    }
}

/// Passing non-probe gate evidence, by where its falsification came from.
#[derive(Serialize, Default)]
struct FalsificationProvenance {
    /// Of passes: carries a declared `falsification`.
    declared: Ratio,
    /// Of passes: carries `falsification_inferred` and no declaration.
    inferred: Ratio,
    /// Of passes: carries neither.
    none: Ratio,
    /// Of `none`: an earlier failure of the same check on the same change
    /// precedes it, so the inference rule would name one. These passes were
    /// recorded before arc derived the inference.
    none_after_failure: Ratio,
}

/// Patchset journal links, split by whether the change was opened from a
/// journal artifact.
#[derive(Serialize, Default)]
struct JournalRefProvenance {
    /// Of all patchsets: carries at least one `journal_refs` entry.
    patchsets_with_refs: Ratio,
    /// Of patchsets on changes opened from a journal artifact.
    opened_from_journal: Ratio,
    /// Of patchsets on changes opened without one.
    opened_without_journal: Ratio,
    /// Of all references on those patchsets, by the recorded `via`;
    /// `unrecorded` for links written before arc named their source.
    via: BTreeMap<&'static str, Ratio>,
}

#[derive(Serialize, Default)]
struct BriefProvenance {
    /// Of all brief versions: carries a `plan_ref`.
    versions_with_plan_ref: Ratio,
    /// Of selected changes: the in-force brief carries a `plan_ref`.
    changes_with_plan_ref: Ratio,
}

#[derive(Serialize)]
struct ProvenanceOutput {
    schema: &'static str,
    changes: usize,
    falsification: FalsificationProvenance,
    journal_refs: JournalRefProvenance,
    /// Of integrated changes: at least one kept fact of kind `rejected`.
    rejected_alternatives: Ratio,
    briefs: BriefProvenance,
    /// Of kept facts: cites at least one event.
    cited_kept_facts: Ratio,
    /// Of selected changes: at least one read record on the change's ledger.
    changes_with_reads: Ratio,
}

const VIA_UNRECORDED: &str = "unrecorded";

fn via_name(via: Option<JournalRefVia>) -> &'static str {
    via.map_or(VIA_UNRECORDED, JournalRefVia::as_str)
}

fn provenance(store: &Store, change_ids: &[String], json: bool) -> Result<()> {
    let read = store.read()?;
    read.rewrites()?;
    let mut falsification = FalsificationProvenance::default();
    let mut journal_refs = JournalRefProvenance::default();
    let mut via_counts: BTreeMap<&'static str, usize> = [
        JournalRefVia::Begin,
        JournalRefVia::Brief,
        JournalRefVia::Flag,
    ]
    .into_iter()
    .map(|via| (via.as_str(), 0))
    .chain([(VIA_UNRECORDED, 0)])
    .collect();
    let mut rejected_alternatives = Ratio::default();
    let mut briefs = BriefProvenance::default();
    let mut cited_kept_facts = Ratio::default();
    let mut changes_with_reads = Ratio::default();

    let mut passes = 0;
    let mut none_after_failure = 0;
    for change_id in change_ids {
        let events = read.events(change_id)?;
        let state = read.project(&events)?;

        for (index, entry) in state.verifications.iter().enumerate() {
            if entry.result != VerifyResult::Pass || entry.probe.is_some() {
                continue;
            }
            passes += 1;
            if entry.falsification.is_some() {
                falsification.declared.count += 1;
            } else if entry.falsification_inferred.is_some() {
                falsification.inferred.count += 1;
            } else {
                falsification.none.count += 1;
                let prior = &state.verifications[..index];
                if state::infer_falsification(prior, entry.gate.as_deref(), &entry.command)
                    .is_some()
                {
                    none_after_failure += 1;
                }
            }
        }

        let from_journal = state.journal_ref.is_some();
        for patchset in &state.patchsets {
            let linked = !patchset.journal_refs.is_empty();
            journal_refs.patchsets_with_refs.tally(linked);
            if from_journal {
                journal_refs.opened_from_journal.tally(linked);
            } else {
                journal_refs.opened_without_journal.tally(linked);
            }
            for reference in &patchset.journal_refs {
                *via_counts.entry(via_name(reference.via)).or_default() += 1;
            }
        }

        let integrated = state
            .closure
            .as_ref()
            .is_some_and(|closure| closure.outcome == Closure::Integrated);
        if integrated {
            rejected_alternatives.tally(
                state
                    .kept
                    .iter()
                    .any(|fact| fact.kind == KeptKind::Rejected),
            );
        }

        for brief in &state.briefs {
            briefs
                .versions_with_plan_ref
                .tally(brief.plan_ref.is_some());
        }
        briefs.changes_with_plan_ref.tally(
            state
                .latest_brief()
                .is_some_and(|brief| brief.plan_ref.is_some()),
        );

        for fact in &state.kept {
            cited_kept_facts.tally(!fact.cites.is_empty());
        }

        changes_with_reads.tally(
            events
                .iter()
                .any(|event| matches!(event.payload, Payload::ContextRead { .. })),
        );
    }

    for ratio in [
        &mut falsification.declared,
        &mut falsification.inferred,
        &mut falsification.none,
    ] {
        ratio.of = passes;
    }
    falsification.none_after_failure = Ratio {
        count: none_after_failure,
        of: falsification.none.count,
    };
    let references: usize = via_counts.values().sum();
    journal_refs.via = via_counts
        .into_iter()
        .map(|(via, count)| {
            (
                via,
                Ratio {
                    count,
                    of: references,
                },
            )
        })
        .collect();

    let output = ProvenanceOutput {
        schema: "arc-stats-provenance/1",
        changes: change_ids.len(),
        falsification,
        journal_refs,
        rejected_alternatives,
        briefs,
        cited_kept_facts,
        changes_with_reads,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    let f = &output.falsification;
    println!(
        "falsification: {} passes; declared {}, inferred {}, none {} ({} after an earlier failure)",
        passes, f.declared.count, f.inferred.count, f.none.count, f.none_after_failure.count
    );
    let j = &output.journal_refs;
    let via = j
        .via
        .iter()
        .map(|(via, ratio)| format!("{via} {}", ratio.count))
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "journal refs: {} patchsets carry one; opened from journal {}, opened without {}; {} references: {}",
        j.patchsets_with_refs, j.opened_from_journal, j.opened_without_journal, references, via
    );
    println!(
        "rejected alternatives: {} integrated changes",
        output.rejected_alternatives
    );
    println!(
        "plan-linked briefs: {} versions, {} changes in force",
        output.briefs.versions_with_plan_ref, output.briefs.changes_with_plan_ref
    );
    println!("cited kept facts: {}", output.cited_kept_facts);
    println!("changes with read records: {}", output.changes_with_reads);
    Ok(())
}

/// One landing: the integrated patchset's recording, or the start of the
/// `arc rebase` that recorded it, to the integration event.
///
/// The phases partition that window. `rebase` runs from the recorded rebase
/// start to the patchset. `gates` is the union of the arc-observed gate runs
/// recorded after the patchset, each run spanning its `duration_ms` back from
/// its own event, so concurrent runs count once. `integration` runs from the
/// recorded start of `arc integrate`, or without one from the last event
/// before the integration event, to that event. `waiting` is the rest. A
/// reuse runs nothing and takes no time.
#[derive(Serialize, Debug, PartialEq)]
struct Landing {
    change_id: String,
    slug: String,
    patchset_id: String,
    /// The `patchset-added` event that opens the window.
    start_event_id: String,
    /// The final integration event, which closes it.
    integration_event_id: String,
    /// The opening patchset's base differs from the base recorded before it.
    rebased: bool,
    /// The pool slot whose kept checkout the change used, when it leased
    /// one before its integration; null for an ordinary checkout.
    slot: Option<String>,
    round_trip_seconds: u64,
    /// Null unless the opening patchset records when its `arc rebase`
    /// started.
    rebase_seconds: Option<u64>,
    gate_seconds: u64,
    waiting_seconds: u64,
    integration_seconds: u64,
    /// `recorded` when the integration event records when `arc integrate`
    /// started; `last-event` when the phase opens at the last event before it.
    integration_start: &'static str,
    /// Each gate's own observed run time inside the window, keyed by gate
    /// name, or by command for an unnamed run. Concurrent runs overlap, so
    /// these may sum above `gate_seconds`.
    gates: BTreeMap<String, u64>,
    gate_runs: usize,
    /// `verification-reused` events inside the window. Counted, never timed.
    reused: usize,
    /// The longest of gates, waiting, integration and rebase; the first on a
    /// tie.
    dominant_phase: &'static str,
}

#[derive(Serialize)]
struct SlowLanding {
    change_id: String,
    round_trip_seconds: u64,
    dominant_phase: &'static str,
}

#[derive(Serialize)]
struct RoundTripAggregate {
    landings: usize,
    /// Nearest-rank median of `round_trip_seconds`; absent with no landings.
    median_seconds: Option<u64>,
    worst_seconds: Option<u64>,
    /// The five longest landings, longest first.
    slowest: Vec<SlowLanding>,
}

#[derive(Serialize)]
struct RoundTripOutput {
    schema: &'static str,
    landings: Vec<Landing>,
    aggregate: RoundTripAggregate,
}

fn is_integration(payload: &Payload) -> bool {
    matches!(
        payload,
        Payload::ChangeIntegrated { .. }
            | Payload::IntegrationAsserted { .. }
            | Payload::ChangeClosed {
                outcome: Closure::Integrated,
                ..
            }
    )
}

/// The change's final landing, or `None` when nothing integrated it or the
/// integrated patchset was never recorded on this ledger.
fn landing(change_id: &str, slug: &str, events: &[Event]) -> Option<Landing> {
    let end = events
        .iter()
        .rposition(|event| is_integration(&event.payload))?;
    let integrated_patchset = match &events[end].payload {
        Payload::ChangeIntegrated {
            source_patchset_id, ..
        }
        | Payload::IntegrationAsserted {
            source_patchset_id, ..
        } => Some(source_patchset_id.as_str()),
        // An integration closed before arc named its patchset landed the
        // latest one recorded before it.
        _ => None,
    };
    let start = events[..end]
        .iter()
        .rposition(|event| match &event.payload {
            Payload::PatchsetAdded { patchset_id, .. } => {
                integrated_patchset.is_none_or(|wanted| wanted == patchset_id)
            }
            _ => false,
        })?;
    let Payload::PatchsetAdded {
        patchset_id,
        base,
        rebase_started_at,
        ..
    } = &events[start].payload
    else {
        unreachable!("the window opens on a patchset");
    };
    let slot = events[..end]
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            Payload::SlotLeased { slot, .. } => Some(slot.clone()),
            _ => None,
        });
    let prior_base = events[..start]
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            Payload::PatchsetAdded { base, .. } | Payload::ChangeOpened { base, .. } => Some(base),
            _ => None,
        });

    let recorded = events[start].created_at.timestamp_millis();
    let opened = rebase_started_at.map_or(recorded, |at| at.timestamp_millis().min(recorded));
    let closed = events[end].created_at.timestamp_millis().max(recorded);
    let integrate_started = match &events[end].payload {
        Payload::ChangeIntegrated {
            started_at: Some(at),
            ..
        } => Some(at.timestamp_millis().clamp(recorded, closed)),
        _ => None,
    };
    let integration_from = integrate_started.unwrap_or_else(|| {
        if end > start + 1 {
            events[end - 1].created_at.timestamp_millis()
        } else {
            recorded
        }
        .clamp(recorded, closed)
    });

    let mut gates: BTreeMap<String, i64> = BTreeMap::new();
    let mut spans: Vec<(i64, i64)> = Vec::new();
    let mut reused = 0;
    for event in &events[start + 1..end] {
        match &event.payload {
            Payload::VerificationRecorded {
                gate,
                command,
                duration_ms: Some(duration_ms),
                attested: false,
                ..
            } => {
                let ran_until = event.created_at.timestamp_millis();
                let from = (ran_until - *duration_ms as i64).clamp(recorded, integration_from);
                let to = ran_until.clamp(recorded, integration_from);
                *gates
                    .entry(gate.clone().unwrap_or_else(|| command.clone()))
                    .or_default() += to - from;
                spans.push((from, to));
            }
            Payload::VerificationReused { .. } => reused += 1,
            _ => {}
        }
    }
    let gate_runs = spans.len();
    spans.sort_unstable();
    let mut busy = 0;
    let mut covered = recorded;
    for (from, to) in spans {
        let from = from.max(covered);
        if to > from {
            busy += to - from;
            covered = to;
        }
    }

    let seconds = |millis: i64| (millis / 1000) as u64;
    let round_trip_seconds = seconds(closed - opened);
    let rebase_seconds = rebase_started_at.map(|_| seconds(recorded - opened));
    let gate_seconds = seconds(busy);
    let integration_seconds = seconds(closed - integration_from);
    // Flooring each phase separately can only lose time to the remainder, so
    // the phases still sum to the round trip.
    let waiting_seconds =
        round_trip_seconds - rebase_seconds.unwrap_or(0) - gate_seconds - integration_seconds;
    let dominant_phase = [
        ("gates", gate_seconds),
        ("waiting", waiting_seconds),
        ("integration", integration_seconds),
        ("rebase", rebase_seconds.unwrap_or(0)),
    ]
    .into_iter()
    .rev()
    .max_by_key(|(_, phase)| *phase)
    .map(|(name, _)| name)
    .unwrap_or("gates");

    Some(Landing {
        change_id: change_id.to_string(),
        slug: slug.to_string(),
        patchset_id: patchset_id.clone(),
        start_event_id: events[start].event_id.clone(),
        integration_event_id: events[end].event_id.clone(),
        rebased: prior_base.is_some_and(|prior| prior != base),
        slot,
        round_trip_seconds,
        rebase_seconds,
        gate_seconds,
        waiting_seconds,
        integration_seconds,
        integration_start: if integrate_started.is_some() {
            "recorded"
        } else {
            "last-event"
        },
        gates: gates
            .into_iter()
            .map(|(gate, millis)| (gate, seconds(millis)))
            .collect(),
        gate_runs,
        reused,
        dominant_phase,
    })
}

fn round_trip_aggregate(landings: &[Landing]) -> RoundTripAggregate {
    let mut by_length: Vec<&Landing> = landings.iter().collect();
    by_length.sort_by(|a, b| {
        b.round_trip_seconds
            .cmp(&a.round_trip_seconds)
            .then_with(|| a.change_id.cmp(&b.change_id))
    });
    let mut samples: Vec<u64> = landings.iter().map(|l| l.round_trip_seconds).collect();
    samples.sort_unstable();
    RoundTripAggregate {
        landings: landings.len(),
        median_seconds: (!samples.is_empty()).then(|| nearest_rank(&samples, 0.5)),
        worst_seconds: samples.last().copied(),
        slowest: by_length
            .into_iter()
            .take(5)
            .map(|landing| SlowLanding {
                change_id: landing.change_id.clone(),
                round_trip_seconds: landing.round_trip_seconds,
                dominant_phase: landing.dominant_phase,
            })
            .collect(),
    }
}

fn round_trips(store: &Store, change_ids: &[String], json: bool) -> Result<()> {
    let read = store.read()?;
    read.rewrites()?;
    let mut landings = Vec::new();
    for change_id in change_ids {
        let events = read.events(change_id)?;
        let state = read.project(&events)?;
        let events = read.events_with_landings(change_id)?;
        landings.extend(landing(change_id, &state.slug, &events));
    }
    let aggregate = round_trip_aggregate(&landings);
    if json {
        let output = RoundTripOutput {
            schema: "arc-stats-round-trip/1",
            landings,
            aggregate,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    println!(
        "{:<28} {:>10} {:>8} {:>8} {:>8} {:>8} {:>6} {:>7} {:>7} dominant",
        "change", "round-trip", "rebase", "gates", "waiting", "integ", "reused", "rebased", "slot"
    );
    for landing in &landings {
        println!(
            "{:<28} {:>10} {:>8} {:>8} {:>8} {:>8} {:>6} {:>7} {:>7} {}",
            truncate(&landing.slug, 28),
            format_duration(landing.round_trip_seconds),
            landing
                .rebase_seconds
                .map_or_else(|| "-".to_string(), format_duration),
            format_duration(landing.gate_seconds),
            format_duration(landing.waiting_seconds),
            format_duration(landing.integration_seconds),
            landing.reused,
            if landing.rebased { "yes" } else { "no" },
            landing.slot.as_deref().unwrap_or("-"),
            landing.dominant_phase
        );
    }
    match (aggregate.median_seconds, aggregate.worst_seconds) {
        (Some(median), Some(worst)) => {
            println!(
                "\nRound trip ({} landings): median {}, worst {}",
                aggregate.landings,
                format_duration(median),
                format_duration(worst)
            );
            println!("slowest:");
            for slow in &aggregate.slowest {
                println!(
                    "  {:<40} {:>10} {}",
                    slow.change_id,
                    format_duration(slow.round_trip_seconds),
                    slow.dominant_phase
                );
            }
        }
        _ => println!("\nno landings: no selected change is integrated"),
    }
    Ok(())
}

pub fn stats(ctx: &Ctx, selection: StatsSelection, json: bool, view: StatsView) -> Result<()> {
    let store = ctx.store()?;
    let change_ids = match &selection {
        StatsSelection::Change(reference) => vec![ctx.load_state(&store, reference)?.0],
        StatsSelection::All => store.readable_states()?.into_keys().collect(),
        StatsSelection::Tag(tag) => {
            let tag = tag.trim().to_string();
            store
                .readable_states()?
                .into_values()
                .filter(|state| state.tags.contains(&tag))
                .map(|state| state.change_id)
                .collect()
        }
    };

    match view {
        StatsView::ByModel => return by_model(&store, &change_ids, json),
        StatsView::Provenance => return provenance(&store, &change_ids, json),
        StatsView::RoundTrip => return round_trips(&store, &change_ids, json),
        StatsView::Changes => {}
    }

    let mut changes = Vec::new();
    // Per-gate run durations across the selection, for aggregate percentiles.
    let mut gate_runs: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let read = store.read()?;
    read.rewrites()?;
    for change_id in change_ids {
        let events = read.events(&change_id)?;
        let mut state = read.project(&events)?;
        crate::replica::localize_change(&store.repository_id, &events, &mut state);
        let events = read.events_with_landings(&change_id)?;
        for (gate, seconds) in observed_gate_runs(&events) {
            gate_runs.entry(gate).or_default().push(seconds);
        }
        changes.push(change_stats(&events, &state));
    }

    let aggregate = aggregate_stats(&changes, &gate_runs);
    if json {
        let output = StatsOutput {
            schema: "arc-stats/1",
            changes,
            aggregate,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_table(&changes, &aggregate);
    }
    Ok(())
}

fn change_stats(events: &[Event], state: &ChangeState) -> ChangeStats {
    let mut findings_by_severity: BTreeMap<String, usize> = BTreeMap::new();
    for finding in state.findings.values() {
        *findings_by_severity
            .entry(severity_name(finding.severity).to_string())
            .or_default() += 1;
    }
    let mut gate_seconds: BTreeMap<String, u64> = BTreeMap::new();
    for (gate, seconds) in observed_gate_runs(events) {
        *gate_seconds.entry(gate).or_default() += seconds;
    }

    let rework = derive_rework(events);
    ChangeStats {
        change_id: state.change_id.clone(),
        slug: state.slug.clone(),
        state: if state.is_closed() { "closed" } else { "open" },
        wall_time_seconds: wall_time(events),
        stage_seconds: stage_durations(events),
        review_latency_seconds: review_latency(events),
        gate_seconds,
        findings_by_severity,
        review_rounds_by_cause: review_rounds_by_cause(events),
        blocks_by_kind: blocks_by_kind(events),
        changes_requested_rounds: rework.changes_requested_rounds,
        completed_rework_rounds: rework.completed_rework_rounds,
        reworked: rework.completed_rework_rounds > 0,
        first_pass_approval: rework.first_pass_approval,
        patchset_count: state.patchsets.len(),
    }
}

/// Open → integrated wall time in seconds, if the change has been integrated.
fn wall_time(events: &[Event]) -> Option<u64> {
    let opened = events
        .iter()
        .find(|event| matches!(event.payload, Payload::ChangeOpened { .. }))?
        .created_at;
    let closed = events
        .iter()
        .find(|event| {
            matches!(
                event.payload,
                Payload::ChangeIntegrated { .. }
                    | Payload::IntegrationAsserted { .. }
                    | Payload::ChangeClosed {
                        outcome: crate::model::Closure::Integrated,
                        ..
                    }
            )
        })?
        .created_at;
    Some(seconds_between(opened, closed))
}

/// Seconds spent in each typed stage, from stage transitions bounded by
/// claim release or, for an unreleased claim, its expiry.
fn stage_durations(events: &[Event]) -> BTreeMap<String, u64> {
    let mut totals: BTreeMap<String, u64> = BTreeMap::new();
    let mut open: Option<(String, DateTime<Utc>)> = None;
    let mut expiry: Option<DateTime<Utc>> = None;

    let mut close = |open: &mut Option<(String, DateTime<Utc>)>, at: DateTime<Utc>| {
        if let Some((stage, start)) = open.take() {
            *totals.entry(stage).or_default() += seconds_between(start, at);
        }
    };

    for event in events {
        match &event.payload {
            Payload::ClaimSet { ttl_seconds, .. } => {
                close(&mut open, event.created_at);
                expiry = Some(event.created_at + Duration::seconds(*ttl_seconds as i64));
            }
            Payload::StageSet { stage, .. } => {
                close(&mut open, event.created_at);
                open = Some((stage_name(*stage).to_string(), event.created_at));
            }
            Payload::ClaimReleased { .. } => {
                close(&mut open, event.created_at);
            }
            _ => {}
        }
    }
    // A stage still open at the end of the ledger is bounded by claim expiry.
    if let (Some((stage, start)), Some(expiry)) = (open, expiry) {
        if expiry > start {
            *totals.entry(stage).or_default() += seconds_between(start, expiry);
        }
    }
    totals
}

/// snapshot → first recorded verdict on that patchset, chronologically.
fn review_latency(events: &[Event]) -> Option<u64> {
    let mut snapshot_at: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
    for event in events {
        match &event.payload {
            Payload::PatchsetAdded { patchset_id, .. } => {
                snapshot_at
                    .entry(patchset_id.clone())
                    .or_insert(event.created_at);
            }
            Payload::VerdictRecorded { patchset_id, .. } => {
                if let Some(snapshot) = snapshot_at.get(patchset_id) {
                    return Some(seconds_between(*snapshot, event.created_at));
                }
            }
            _ => {}
        }
    }
    None
}

/// One (gate, seconds) pair per verification arc actually ran and timed.
/// Attested evidence (no observed duration) is excluded by construction.
fn observed_gate_runs(events: &[Event]) -> Vec<(String, u64)> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::VerificationRecorded {
                gate: Some(gate),
                duration_ms: Some(duration_ms),
                attested: false,
                ..
            } => Some((gate.clone(), duration_ms / 1000)),
            _ => None,
        })
        .collect()
}

fn aggregate_stats(changes: &[ChangeStats], gate_runs: &BTreeMap<String, Vec<u64>>) -> Aggregate {
    let mut stage_samples: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for change in changes {
        for (stage, seconds) in &change.stage_seconds {
            stage_samples
                .entry(stage.clone())
                .or_default()
                .push(*seconds);
        }
    }

    let stage: BTreeMap<String, Percentiles> = stage_samples
        .iter()
        .map(|(name, samples)| (name.clone(), percentiles(samples)))
        .collect();
    let gate: BTreeMap<String, Percentiles> = gate_runs
        .iter()
        .map(|(name, samples)| (name.clone(), percentiles(samples)))
        .collect();
    let suggested_stage_budgets = stage
        .iter()
        .map(|(name, p)| (name.clone(), round_up_clean(p.p90_seconds)))
        .collect();
    let mut review_rounds_by_cause = BTreeMap::new();
    let mut blocks_by_kind = BTreeMap::new();
    for change in changes {
        for (cause, count) in &change.review_rounds_by_cause {
            *review_rounds_by_cause.entry(cause.clone()).or_default() += count;
        }
        for (kind, count) in &change.blocks_by_kind {
            *blocks_by_kind.entry(kind.clone()).or_default() += count;
        }
    }
    let changes_reworked = changes.iter().filter(|change| change.reworked).count();
    let first_pass_approvals = changes
        .iter()
        .filter(|change| change.first_pass_approval)
        .count();
    let completed_rework_rounds = changes
        .iter()
        .map(|change| change.completed_rework_rounds)
        .sum();

    Aggregate {
        changes: changes.len(),
        stage,
        gate,
        suggested_stage_budgets,
        review_rounds_by_cause,
        blocks_by_kind,
        changes_reworked,
        first_pass_approvals,
        completed_rework_rounds,
    }
}

fn blocks_by_kind(events: &[Event]) -> BTreeMap<String, usize> {
    let mut blocks = BTreeMap::new();
    for event in events {
        if let Payload::StageSet {
            stage: ClaimStage::BlockedOn,
            blocker,
            ..
        } = &event.payload
        {
            let kind = match blocker {
                Some(BlockerRef::Brief { .. }) => "brief",
                Some(BlockerRef::Finding { .. }) => "finding",
                Some(BlockerRef::Change { .. }) => "change",
                Some(BlockerRef::External) => "external",
                None => "unclassified",
            };
            *blocks.entry(kind.to_string()).or_default() += 1;
        }
    }
    blocks
}

struct ReworkStats {
    changes_requested_rounds: usize,
    completed_rework_rounds: usize,
    first_pass_approval: bool,
    /// The patchsets whose rounds completed, so a round can be attributed to
    /// whoever contributed the work it asked to be revised.
    reworked_patchsets: Vec<String>,
}

fn derive_rework(events: &[Event]) -> ReworkStats {
    let patchsets: BTreeMap<&str, usize> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match &event.payload {
            Payload::PatchsetAdded { patchset_id, .. } => Some((patchset_id.as_str(), index)),
            _ => None,
        })
        .collect();
    // Re-snapshotting at the same head is how a patchset binds to a corrected
    // brief. Counting that as a revision cycle would inflate the rework signal
    // for exactly the leads careful enough to correct a brief. What answers a
    // request is the next patchset after it, so that is the one compared —
    // not whichever patchset was eventually approved, which may be a later
    // round's or may coincidentally share the requested head.
    let ordered: Vec<(usize, &str, &str)> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match &event.payload {
            Payload::PatchsetAdded {
                patchset_id, head, ..
            } => Some((index, patchset_id.as_str(), head.as_str())),
            _ => None,
        })
        .collect();
    let head_of = |wanted: &str| {
        ordered
            .iter()
            .find(|(_, id, _)| *id == wanted)
            .map(|(_, _, head)| *head)
    };
    let answering_head = |after: usize| {
        ordered
            .iter()
            .find(|(index, _, _)| *index > after)
            .map(|(_, _, head)| *head)
    };
    // A round is a revision cycle, not a verdict event. Several
    // changes-requested verdicts on one patchset are answered by one revision,
    // so they open one round, dated from the first of them.
    let mut requested: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, event) in events.iter().enumerate() {
        if let Payload::VerdictRecorded {
            patchset_id,
            verdict: Verdict::ChangesRequested,
            ..
        } = &event.payload
        {
            requested.entry(patchset_id.as_str()).or_insert(index);
        }
    }
    let approvals: Vec<(usize, &str)> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match &event.payload {
            Payload::VerdictRecorded {
                patchset_id,
                verdict: Verdict::Approved,
                ..
            } => Some((index, patchset_id.as_str())),
            _ => None,
        })
        .collect();

    let reworked_patchsets: Vec<String> = requested
        .iter()
        .filter(|(requested_id, request_index)| {
            // The revision that answers this request moved the code, and some
            // later approval closed the round it opened.
            answering_head(**request_index) != head_of(requested_id)
                && approvals.iter().any(|(approval_index, patchset_id)| {
                    approval_index > *request_index
                        && patchsets
                            .get(patchset_id)
                            .is_some_and(|patchset_index| patchset_index > *request_index)
                })
        })
        .map(|(patchset_id, _)| (*patchset_id).to_string())
        .collect();
    let completed_rework_rounds = reworked_patchsets.len();
    let first_patchset = patchsets
        .iter()
        .min_by_key(|(_, index)| *index)
        .map(|(id, _)| *id);
    let first_pass_approval = first_patchset.is_some_and(|first_patchset| {
        approvals
            .iter()
            .find(|(_, patchset_id)| *patchset_id == first_patchset)
            .is_some_and(|(approval_index, _)| {
                requested
                    .values()
                    .all(|request_index| request_index > approval_index)
            })
    });

    ReworkStats {
        changes_requested_rounds: requested.len(),
        completed_rework_rounds,
        first_pass_approval,
        reworked_patchsets,
    }
}

/// A cause explains the round it belongs to, not each verdict that mentions it,
/// so a cause repeated across several verdicts on one patchset counts once. A
/// round citing two causes counts under both, so this tally sums above the
/// round count by design.
fn review_rounds_by_cause(events: &[Event]) -> BTreeMap<String, usize> {
    let mut by_round: BTreeMap<&str, BTreeSet<&'static str>> = BTreeMap::new();
    for event in events {
        if let Payload::VerdictRecorded {
            patchset_id,
            verdict: Verdict::ChangesRequested,
            causes,
            ..
        } = &event.payload
        {
            let round = by_round.entry(patchset_id.as_str()).or_default();
            round.extend(causes.iter().map(|cause| review_cause_name(*cause)));
        }
    }
    let mut rounds = BTreeMap::new();
    for causes in by_round.values() {
        for cause in causes {
            *rounds.entry((*cause).to_string()).or_default() += 1;
        }
    }
    rounds
}

fn review_cause_name(cause: ReviewCause) -> &'static str {
    match cause {
        ReviewCause::Brief => "brief",
        ReviewCause::Executor => "executor",
        ReviewCause::IntegrationStaleness => "integration-staleness",
    }
}

/// Nearest-rank median and p90 over a sample set.
fn percentiles(samples: &[u64]) -> Percentiles {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    Percentiles {
        samples: sorted.len(),
        median_seconds: nearest_rank(&sorted, 0.5),
        p90_seconds: nearest_rank(&sorted, 0.9),
    }
}

fn nearest_rank(sorted: &[u64], quantile: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[index]
}

/// Round a duration up to the next clean budget value.
fn round_up_clean(seconds: u64) -> u64 {
    const LADDER: [u64; 12] = [
        30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 14400, 28800, 86400,
    ];
    for step in LADDER {
        if seconds <= step {
            return step;
        }
    }
    // Above a day, round up to whole days.
    seconds.div_ceil(86400) * 86400
}

fn seconds_between(start: DateTime<Utc>, end: DateTime<Utc>) -> u64 {
    (end - start).num_seconds().max(0) as u64
}

fn stage_name(stage: ClaimStage) -> &'static str {
    match stage {
        ClaimStage::Started => "started",
        ClaimStage::SpecRead => "spec-read",
        ClaimStage::Implementing => "implementing",
        ClaimStage::Verifying => "verifying",
        ClaimStage::BlockedOn => "blocked-on",
        ClaimStage::Snapshotted => "snapshotted",
    }
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "critical",
        Severity::Major => "major",
        Severity::Minor => "minor",
        Severity::Note => "note",
    }
}

fn print_table(changes: &[ChangeStats], aggregate: &Aggregate) {
    println!(
        "{:<28} {:<10} {:>8} {:>8} {:>4} {:>4} {:>10} {:>8}",
        "change", "state", "wall", "review", "ps", "rw", "first-pass", "findings"
    );
    for change in changes {
        let wall = change
            .wall_time_seconds
            .map(format_duration)
            .unwrap_or_else(|| "—".into());
        let review = change
            .review_latency_seconds
            .map(format_duration)
            .unwrap_or_else(|| "—".into());
        let findings: usize = change.findings_by_severity.values().sum();
        println!(
            "{:<28} {:<10} {:>8} {:>8} {:>4} {:>4} {:>10} {:>8}",
            truncate(&change.slug, 28),
            change.state,
            wall,
            review,
            change.patchset_count,
            change.completed_rework_rounds,
            if change.first_pass_approval {
                "yes"
            } else {
                "no"
            },
            findings
        );
    }

    println!("\nAggregate ({} changes)", aggregate.changes);
    if !aggregate.stage.is_empty() {
        println!(
            "{:<16} {:>8} {:>8} {:>16}",
            "stage", "median", "p90", "suggested-budget"
        );
        for (name, p) in &aggregate.stage {
            let budget = aggregate
                .suggested_stage_budgets
                .get(name)
                .copied()
                .unwrap_or(0);
            println!(
                "{:<16} {:>8} {:>8} {:>16}",
                name,
                format_duration(p.median_seconds),
                format_duration(p.p90_seconds),
                format_duration(budget)
            );
        }
    }
    if !aggregate.gate.is_empty() {
        println!("{:<16} {:>8} {:>8}", "gate", "median", "p90");
        for (name, p) in &aggregate.gate {
            println!(
                "{:<16} {:>8} {:>8}",
                name,
                format_duration(p.median_seconds),
                format_duration(p.p90_seconds)
            );
        }
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.len() <= width {
        text.to_string()
    } else {
        format!("{}…", &text[..width - 1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Closure, Verdict, VerifyResult};

    fn event(id: &str, seconds: i64, payload: Payload) -> Event {
        Event {
            schema_version: 1,
            event_id: id.to_string(),
            repository_id: "repo".into(),
            change_id: "c".into(),
            actor: "tester".into(),
            actor_source: Some(ActorSource::Flag),
            operator: None,
            on_behalf_of: None,
            model: None,
            model_provenance: Default::default(),
            harness: Some("test".into()),
            session: Some("s".into()),
            session_link: None,
            session_resolution: None,
            session_resolution_reason: None,
            created_at: DateTime::<Utc>::from_timestamp(1_000_000 + seconds, 0).unwrap(),
            payload,
        }
    }

    #[test]
    fn stage_durations_sum_between_transitions() {
        let events = vec![
            event(
                "1",
                0,
                Payload::ClaimSet {
                    claim_id: "cl".into(),
                    ttl_seconds: 7200,
                    stage_budgets: Default::default(),
                    displaced: None,
                },
            ),
            event(
                "2",
                0,
                Payload::StageSet {
                    claim_id: "cl".into(),
                    stage: ClaimStage::Implementing,
                    note: None,
                    blocker: None,
                },
            ),
            event(
                "3",
                120,
                Payload::StageSet {
                    claim_id: "cl".into(),
                    stage: ClaimStage::Verifying,
                    note: None,
                    blocker: None,
                },
            ),
            event(
                "4",
                150,
                Payload::ClaimReleased {
                    claim_id: "cl".into(),
                },
            ),
        ];
        let durations = stage_durations(&events);
        assert_eq!(durations.get("implementing"), Some(&120));
        assert_eq!(durations.get("verifying"), Some(&30));
    }

    #[test]
    fn unreleased_stage_is_bounded_by_claim_expiry() {
        let events = vec![
            event(
                "1",
                0,
                Payload::ClaimSet {
                    claim_id: "cl".into(),
                    ttl_seconds: 100,
                    stage_budgets: Default::default(),
                    displaced: None,
                },
            ),
            event(
                "2",
                0,
                Payload::StageSet {
                    claim_id: "cl".into(),
                    stage: ClaimStage::Implementing,
                    note: None,
                    blocker: None,
                },
            ),
        ];
        // No release: implementing runs until expiry (100s), not forever.
        assert_eq!(stage_durations(&events).get("implementing"), Some(&100));
    }

    #[test]
    fn review_latency_measures_first_verdict_after_its_snapshot() {
        let events = vec![
            event(
                "1",
                0,
                Payload::PatchsetAdded {
                    patchset_id: "ps-01".into(),
                    base: "b".into(),
                    head: "h".into(),
                    merge_base: None,
                    brief_ref: None,
                    author_name: None,
                    author_email: None,
                    committer_name: None,
                    committer_email: None,
                    contributors: Vec::new(),
                    inherited_authors: Vec::new(),
                    claim_id: None,
                    claim_actor: None,
                    journal_refs: Vec::new(),
                    thread: None,
                    candidate: None,
                    rebase_started_at: None,
                },
            ),
            event(
                "2",
                45,
                Payload::VerdictRecorded {
                    tiebreak_reviewer: None,
                    dispositions: Vec::new(),
                    receipt: None,
                    forge_frontier: None,
                    forge_sources: Vec::new(),
                    patchset_id: "ps-01".into(),
                    verdict: Verdict::Approved,
                    authority_observed: None,
                    causes: Vec::new(),
                    body: None,
                    findings: Vec::new(),
                    relation: None,
                    provisional: None,
                    route_version: None,
                    assessment: None,
                },
            ),
        ];
        assert_eq!(review_latency(&events), Some(45));
    }

    #[test]
    fn gate_runs_exclude_attested_and_untimed_evidence() {
        let timed = event(
            "1",
            0,
            Payload::VerificationRecorded {
                timeout_seconds: None,
                tree: None,
                against_target: None,
                landing: None,
                tested_tree: None,
                worktree_dirty: None,
                worktree_dirty_tracked: None,
                worktree_dirty_untracked: None,
                tree_moved: false,
                gate: Some("build".into()),
                command: "cargo build".into(),
                revision: "h".into(),
                result: VerifyResult::Pass,
                exit_code: Some(0),
                duration_ms: Some(5000),
                output_tail: None,
                output_artifact: None,
                timed_out: false,
                hostname: "host".into(),
                attested: false,
                run_id: None,
                probe: None,
                runner: None,
                environment: None,
                note: None,
                falsification: None,
                falsification_inferred: None,
            },
        );
        let attested = event(
            "2",
            0,
            Payload::VerificationRecorded {
                timeout_seconds: None,
                tree: None,
                against_target: None,
                landing: None,
                tested_tree: None,
                worktree_dirty: None,
                worktree_dirty_tracked: None,
                worktree_dirty_untracked: None,
                tree_moved: false,
                gate: Some("test".into()),
                command: "cargo test".into(),
                revision: "h".into(),
                result: VerifyResult::Pass,
                exit_code: None,
                duration_ms: None,
                output_tail: None,
                output_artifact: None,
                timed_out: false,
                hostname: "host".into(),
                attested: true,
                run_id: None,
                probe: None,
                runner: Some("external".into()),
                environment: None,
                note: None,
                falsification: None,
                falsification_inferred: None,
            },
        );
        let runs = observed_gate_runs(&[timed, attested]);
        assert_eq!(runs, vec![("build".to_string(), 5)]);
    }

    #[test]
    fn wall_time_spans_open_to_integrated() {
        let events = vec![
            event(
                "1",
                0,
                Payload::ChangeOpened {
                    dangerous: false,
                    slug: "feat".into(),
                    title: "t".into(),
                    profile: "local".into(),
                    target_branch: "master".into(),
                    branch: "arc/feat".into(),
                    base: "b".into(),
                    worktree: None,
                    worktree_created: false,
                    worktree_owner_token: None,
                    blocked_by: Vec::new(),
                    tags: Vec::new(),
                    journal_ref: None,
                    journal_ref_digest: None,
                    from_fork: None,
                },
            ),
            event(
                "2",
                3600,
                Payload::ChangeClosed {
                    outcome: Closure::Integrated,
                    integrated_commit: Some("m".into()),
                    superseded_by: None,
                },
            ),
        ];
        assert_eq!(wall_time(&events), Some(3600));
    }

    fn patchset(id: &str, seconds: i64, patchset_id: &str, base: &str) -> Event {
        rebased_patchset(id, seconds, patchset_id, base, None)
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_000_000 + seconds, 0).unwrap()
    }

    fn rebased_patchset(
        id: &str,
        seconds: i64,
        patchset_id: &str,
        base: &str,
        rebase_started: Option<i64>,
    ) -> Event {
        event(
            id,
            seconds,
            Payload::PatchsetAdded {
                patchset_id: patchset_id.into(),
                base: base.into(),
                head: "h".into(),
                merge_base: None,
                brief_ref: None,
                author_name: None,
                author_email: None,
                committer_name: None,
                committer_email: None,
                contributors: Vec::new(),
                inherited_authors: Vec::new(),
                claim_id: None,
                claim_actor: None,
                journal_refs: Vec::new(),
                thread: None,
                candidate: None,
                rebase_started_at: rebase_started.map(at),
            },
        )
    }

    fn gate_run(id: &str, seconds: i64, gate: &str, duration_seconds: u64) -> Event {
        event(
            id,
            seconds,
            Payload::VerificationRecorded {
                timeout_seconds: None,
                tree: None,
                against_target: None,
                landing: None,
                tested_tree: Some("t".into()),
                worktree_dirty: Some(false),
                worktree_dirty_tracked: None,
                worktree_dirty_untracked: None,
                tree_moved: false,
                gate: Some(gate.into()),
                command: format!("run {gate}"),
                revision: "h".into(),
                result: VerifyResult::Pass,
                exit_code: Some(0),
                duration_ms: Some(duration_seconds * 1000),
                output_tail: None,
                output_artifact: None,
                timed_out: false,
                hostname: "host".into(),
                attested: false,
                run_id: None,
                probe: None,
                runner: None,
                environment: None,
                note: None,
                falsification: None,
                falsification_inferred: None,
            },
        )
    }

    fn reuse(id: &str, seconds: i64, gate: &str, evidence: &str) -> Event {
        event(
            id,
            seconds,
            Payload::VerificationReused {
                run_id: "run".into(),
                gate: gate.into(),
                revision: "h".into(),
                tree: None,
                evidence_event_id: evidence.into(),
                delta: None,
            },
        )
    }

    fn integrated(id: &str, seconds: i64, patchset_id: &str) -> Event {
        integrated_from(id, seconds, patchset_id, None)
    }

    fn integrated_from(
        id: &str,
        seconds: i64,
        patchset_id: &str,
        integrate_started: Option<i64>,
    ) -> Event {
        event(
            id,
            seconds,
            Payload::ChangeIntegrated {
                authority_grant_event_id: None,
                provisional_resolutions: Vec::new(),
                integrated_commit: "m".into(),
                source_patchset_id: patchset_id.into(),
                source_head: "h".into(),
                target_branch: "master".into(),
                target_before: "b".into(),
                authorization: None,
                already_contained: false,
                shape: None,
                started_at: integrate_started.map(at),
                landing: None,
            },
        )
    }

    /// Claim: gates, waiting and integration partition the round trip, with
    /// concurrent gate runs counted once and a run that began before the
    /// window counted only inside it. Rejects summing each run's duration,
    /// which charges three parallel 200-second runs as 600 seconds of a
    /// 220-second landing.
    #[test]
    fn landing_phases_sum_to_the_round_trip() {
        let events = vec![
            gate_run("0", -50, "build", 10),
            patchset("1", 0, "ps-01", "b"),
            // Started 30 seconds before the window opened.
            gate_run("2", 20, "build", 50),
            gate_run("3", 210, "lint", 200),
            gate_run("4", 210, "test", 200),
            gate_run("5", 210, "doc", 200),
            integrated("6", 220, "ps-01"),
        ];
        let landing = landing("c", "s", &events).expect("an integrated change lands");
        assert_eq!(landing.round_trip_seconds, 220);
        assert_eq!(landing.gate_seconds, 210, "[0,20] and [10,210] cover 210s");
        assert_eq!(landing.integration_seconds, 10);
        assert_eq!(landing.waiting_seconds, 0);
        assert_eq!(
            landing.gate_seconds + landing.waiting_seconds + landing.integration_seconds,
            landing.round_trip_seconds
        );
        assert_eq!(landing.gates.get("build"), Some(&20));
        assert_eq!(landing.gates.get("test"), Some(&200));
        assert_eq!(landing.gate_runs, 4);
        assert_eq!(landing.dominant_phase, "gates");
    }

    /// Claim: a reused gate costs the landing nothing; its evidence's run
    /// time belongs to the landing that ran it. Rejects charging every run on
    /// the change, which bills this landing the 100 seconds an earlier
    /// patchset's build took.
    #[test]
    fn a_reused_gate_counts_zero_seconds() {
        let events = vec![
            patchset("1", 0, "ps-01", "b"),
            gate_run("2", 100, "build", 100),
            patchset("3", 200, "ps-02", "b"),
            reuse("4", 210, "build", "2"),
            integrated("5", 220, "ps-02"),
        ];
        let landing = landing("c", "s", &events).expect("an integrated change lands");
        assert_eq!(landing.reused, 1);
        assert_eq!(landing.gate_seconds, 0);
        assert!(landing.gates.is_empty(), "{:?}", landing.gates);
        assert_eq!(landing.gate_runs, 0);
        assert_eq!(landing.round_trip_seconds, 20);
        assert_eq!(landing.waiting_seconds, 10);
        assert_eq!(landing.integration_seconds, 10);
        assert!(!landing.rebased);
    }

    /// Claim: a change integrated twice reports the landing of its last
    /// integration, from the patchset that integration names. Rejects the
    /// first integration event, which reports the 100-second landing of
    /// `ps-01`, and the change's first patchset, which spans the whole change.
    #[test]
    fn a_change_integrated_twice_reports_its_final_landing() {
        let events = vec![
            patchset("1", 0, "ps-01", "b"),
            integrated("2", 100, "ps-01"),
            patchset("3", 1000, "ps-02", "b2"),
            gate_run("4", 1050, "build", 30),
            integrated("5", 1150, "ps-02"),
        ];
        let landing = landing("c", "s", &events).expect("an integrated change lands");
        assert_eq!(landing.integration_event_id, "5");
        assert_eq!(landing.patchset_id, "ps-02");
        assert_eq!(landing.start_event_id, "3");
        assert_eq!(landing.round_trip_seconds, 150);
        assert_eq!(landing.gate_seconds, 30);
        assert_eq!(landing.integration_seconds, 100);
        assert_eq!(landing.waiting_seconds, 20);
        assert_eq!(landing.dominant_phase, "integration");
        assert!(landing.rebased, "ps-02 moved its base from b to b2");
    }

    /// Claim: a patchset recorded by `arc rebase` opens the window at the
    /// rebase's recorded start and reports the replay as its own phase.
    /// Rejects opening the window at the patchset, which drops the 40-second
    /// replay from the round trip and reports no rebase phase.
    #[test]
    fn a_recorded_rebase_start_times_the_rebase_phase() {
        let events = vec![
            patchset("1", 0, "ps-01", "b"),
            rebased_patchset("2", 100, "ps-02", "b2", Some(60)),
            gate_run("3", 130, "build", 30),
            integrated("4", 150, "ps-02"),
        ];
        let landing = landing("c", "s", &events).expect("an integrated change lands");
        assert_eq!(landing.rebase_seconds, Some(40));
        assert_eq!(landing.round_trip_seconds, 90);
        assert_eq!(landing.gate_seconds, 30);
        assert_eq!(landing.integration_seconds, 20);
        assert_eq!(landing.waiting_seconds, 0);
        assert_eq!(landing.dominant_phase, "rebase");
        assert!(landing.rebased);
    }

    /// Claim: an integration event that records when `arc integrate` started
    /// times the integration phase from that start, so the wait before the
    /// command was invoked counts as waiting. Rejects timing it from the last
    /// event before the integration, which charges the 300 seconds between
    /// the verdict and the command to integration.
    #[test]
    fn a_recorded_integrate_start_separates_the_wait_before_it() {
        let events = vec![
            patchset("1", 0, "ps-01", "b"),
            gate_run("2", 30, "build", 30),
            integrated_from("3", 340, "ps-01", Some(330)),
        ];
        let landing = landing("c", "s", &events).expect("an integrated change lands");
        assert_eq!(landing.integration_seconds, 10);
        assert_eq!(landing.integration_start, "recorded");
        assert_eq!(landing.waiting_seconds, 300);
        assert_eq!(landing.gate_seconds, 30);
        assert_eq!(landing.round_trip_seconds, 340);
        assert_eq!(landing.rebase_seconds, None);
    }

    /// Claim: a landing whose events record neither start reads exactly as
    /// the derivation without them: the window opens at the patchset and
    /// integration opens at the last event before it. Rejects treating an
    /// absent start as the window's opening, which charges the whole landing
    /// to integration.
    #[test]
    fn a_landing_without_recorded_starts_keeps_the_derived_phases() {
        let events = vec![
            patchset("1", 0, "ps-01", "b"),
            gate_run("2", 30, "build", 30),
            integrated("3", 340, "ps-01"),
        ];
        let landing = landing("c", "s", &events).expect("an integrated change lands");
        assert_eq!(landing.integration_start, "last-event");
        assert_eq!(landing.rebase_seconds, None);
        assert_eq!(landing.round_trip_seconds, 340);
        assert_eq!(landing.gate_seconds, 30);
        assert_eq!(landing.integration_seconds, 310);
        assert_eq!(landing.waiting_seconds, 0);
    }

    /// Claim: only an integration makes a landing. Rejects a zero-length row
    /// for a change that was snapshotted and verified but never integrated.
    /// Claim: a landing names the slot its change leased, so a kept checkout
    /// reads apart from an ordinary one; a change that leased none names
    /// nothing. Rejects a round trip that cannot tell the two apart.
    #[test]
    fn a_landing_names_the_slot_its_change_leased() {
        let leased = event(
            "0",
            -10,
            Payload::SlotLeased {
                root: "/pool".into(),
                slot: "slot-02".into(),
                path: "/pool/slot-02".into(),
                token: "t".into(),
            },
        );
        let mut events = vec![
            leased,
            patchset("1", 0, "ps-01", "b"),
            integrated("2", 20, "ps-01"),
        ];
        let landing = landing("c", "s", &events).expect("an integrated change lands");
        assert_eq!(landing.slot.as_deref(), Some("slot-02"));
        events.remove(0);
        let landing = landing_without_slot(&events);
        assert_eq!(landing, None, "a change without a lease named a slot");
    }

    fn landing_without_slot(events: &[Event]) -> Option<String> {
        landing("c", "s", events).and_then(|landing| landing.slot)
    }

    #[test]
    fn an_unintegrated_change_has_no_round_trip() {
        let events = vec![
            patchset("1", 0, "ps-01", "b"),
            gate_run("2", 100, "build", 100),
        ];
        assert_eq!(landing("c", "s", &events), None);
    }

    #[test]
    fn round_up_clean_snaps_to_ladder() {
        assert_eq!(round_up_clean(10), 30);
        assert_eq!(round_up_clean(65), 120);
        assert_eq!(round_up_clean(500), 600);
        assert_eq!(round_up_clean(90_000), 172_800);
    }

    #[test]
    fn nearest_rank_picks_expected_samples() {
        let sorted = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(nearest_rank(&sorted, 0.5), 5);
        assert_eq!(nearest_rank(&sorted, 0.9), 9);
    }
}
