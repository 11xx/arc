//! Derived ledger analytics: the durations and counts the append-only
//! ledger already holds, projected per change and aggregated. Pure
//! derivation — no writes, no new events.

use super::*;
use crate::model::{BlockerRef, ClaimStage, Event, Payload, ReviewCause, Severity};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

/// Which changes `arc stats` reports over.
pub enum StatsSelection {
    All,
    Change(String),
    Tag(String),
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

pub fn stats(ctx: &Ctx, selection: StatsSelection, json: bool, by_model_view: bool) -> Result<()> {
    let store = ctx.store()?;
    let change_ids = match &selection {
        StatsSelection::Change(reference) => vec![store.resolve_change(reference)?],
        StatsSelection::All => store.list_change_ids()?,
        StatsSelection::Tag(tag) => {
            let tag = tag.trim().to_string();
            let mut selected = Vec::new();
            for change_id in store.list_change_ids()? {
                let state = state::reduce(&store.load_events(&change_id)?)?;
                if state.tags.contains(&tag) {
                    selected.push(change_id);
                }
            }
            selected
        }
    };

    if by_model_view {
        return by_model(&store, &change_ids, json);
    }

    let mut changes = Vec::new();
    // Per-gate run durations across the selection, for aggregate percentiles.
    let mut gate_runs: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for change_id in change_ids {
        let events = store.load_events(&change_id)?;
        let state = state::reduce(&events)?;
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
                tested_tree: None,
                worktree_dirty: None,
                tree_moved: false,
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

fn format_duration(seconds: u64) -> String {
    if seconds == 0 {
        return "0s".into();
    }
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    let mut out = String::new();
    if hours > 0 {
        out.push_str(&format!("{hours}h"));
    }
    if minutes > 0 {
        out.push_str(&format!("{minutes}m"));
    }
    if secs > 0 && hours == 0 {
        out.push_str(&format!("{secs}s"));
    }
    out
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
            on_behalf_of: None,
            model: None,
            harness: Some("test".into()),
            session: Some("s".into()),
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
                    claim_id: None,
                    claim_actor: None,
                },
            ),
            event(
                "2",
                45,
                Payload::VerdictRecorded {
                    patchset_id: "ps-01".into(),
                    verdict: Verdict::Approved,
                    causes: Vec::new(),
                    body: None,
                    findings: Vec::new(),
                    relation: None,
                    provisional: None,
                    route_version: None,
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
                timed_out: false,
                hostname: "host".into(),
                attested: false,
                run_id: None,
                probe: None,
                runner: None,
                note: None,
            },
        );
        let attested = event(
            "2",
            0,
            Payload::VerificationRecorded {
                timeout_seconds: None,
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
                timed_out: false,
                hostname: "host".into(),
                attested: true,
                run_id: None,
                probe: None,
                runner: Some("external".into()),
                note: None,
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
                    blocked_by: Vec::new(),
                    tags: Vec::new(),
                    journal_ref: None,
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
