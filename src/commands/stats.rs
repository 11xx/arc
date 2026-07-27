//! Derived ledger analytics: the durations and counts the append-only
//! ledger already holds, projected per change and aggregated. Pure
//! derivation — no writes, no new events.

use super::*;
use crate::model::{ClaimStage, Event, Payload, Severity};
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
}

#[derive(Serialize)]
struct Percentiles {
    samples: usize,
    median_seconds: u64,
    p90_seconds: u64,
}

pub fn stats(ctx: &Ctx, selection: StatsSelection, json: bool) -> Result<()> {
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

    ChangeStats {
        change_id: state.change_id.clone(),
        slug: state.slug.clone(),
        state: if state.is_closed() { "closed" } else { "open" },
        wall_time_seconds: wall_time(events),
        stage_seconds: stage_durations(events),
        review_latency_seconds: review_latency(events),
        gate_seconds,
        findings_by_severity,
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
                Payload::ChangeClosed {
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

    Aggregate {
        changes: changes.len(),
        stage,
        gate,
        suggested_stage_budgets,
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
        "{:<28} {:<10} {:>8} {:>8} {:>4} {:>8}",
        "change", "state", "wall", "review", "ps", "findings"
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
            "{:<28} {:<10} {:>8} {:>8} {:>4} {:>8}",
            truncate(&change.slug, 28),
            change.state,
            wall,
            review,
            change.patchset_count,
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
            on_behalf_of: None,
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
                },
            ),
            event(
                "3",
                120,
                Payload::StageSet {
                    claim_id: "cl".into(),
                    stage: ClaimStage::Verifying,
                    note: None,
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
                    body: None,
                    findings: Vec::new(),
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
