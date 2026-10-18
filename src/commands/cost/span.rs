//! A session's consumption divided among the work its spans cover. The
//! ledger gives each span: an executor's run from its dispatch to its
//! recorded end, open while it has none, and a participant's events on one
//! item, first to last inclusive. The session's timeline is cut at every span
//! boundary into disjoint segments, each read once, so no request is counted
//! twice. A segment one item covers is that item's share; one several items
//! cover, and one no item covers, is nobody's, with a fixed reason.

use super::{WorkKey, CATEGORIES};
use crate::model::{
    CostCacheWrites, CostConvention, CostDisjoint, CostFacts, CostModelShare, CostPricing,
    CostSegment,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Instant = chrono::DateTime<chrono::Utc>;

pub(super) const OVERLAP: &str = "overlap";
pub(super) const BEFORE: &str = "before first item";
pub(super) const BETWEEN: &str = "between items";
pub(super) const AFTER: &str = "after last item";
pub(super) const UNTIMED: &str = "request time unrecorded";
pub(super) const NOT_SPLITTABLE: &str = "recorded total not splittable";
pub(super) const UNSUPPORTED: &str = "span reading unsupported";
pub(super) const RETAINED_MISMATCH: &str = "retained spans do not match the ledger";
pub(super) const UNMEASURED: &str = "unmeasured";
pub(super) const INCOMPLETE_MEMBERSHIP: &str = "ledger records are unreadable";

/// One span the ledger gives a session.
#[derive(Clone)]
pub(super) struct ItemSpan {
    pub work: WorkKey,
    /// The run's dispatch, for an executor.
    pub run: Option<String>,
    pub from: Instant,
    /// Exclusive; open while a run has no recorded end.
    pub to: Option<Instant>,
}

impl ItemSpan {
    /// A participant's span on one item: its first to its last event there,
    /// inclusive, so the end is one nanosecond past the last event.
    pub fn events(work: WorkKey, first: Instant, last: Instant) -> Self {
        Self {
            work,
            run: None,
            from: first,
            to: Some(last + chrono::Duration::nanoseconds(1)),
        }
    }

    fn covers(&self, from: Option<Instant>, to: Option<Instant>) -> bool {
        from.is_some_and(|from| self.from <= from)
            && self.to.is_none_or(|end| to.is_some_and(|to| to <= end))
    }
}

/// One disjoint segment `[from, to)` of a session's timeline and the spans
/// that cover it.
#[derive(Clone)]
pub(super) struct Segment {
    pub from: Option<Instant>,
    pub to: Option<Instant>,
    pub work: BTreeSet<WorkKey>,
    pub runs: BTreeSet<String>,
}

impl Segment {
    pub fn work_labels(&self) -> Vec<String> {
        self.work.iter().map(WorkKey::label).collect()
    }

    pub fn bounds(&self) -> agent_tapes_core::span::SpanBounds {
        agent_tapes_core::span::SpanBounds::Time {
            from: self.from,
            to: self.to,
        }
    }

    /// Whether a retained segment has this definition.
    pub fn defines(&self, retained: &CostSegment) -> bool {
        let runs: Vec<String> = self.runs.iter().cloned().collect();
        retained.bounds() == (self.from, self.to, &self.work_labels()[..], &runs[..])
    }
}

/// The session's timeline cut at every boundary of its spans: the segment
/// before the first boundary, one between each pair, and the one after the
/// last, each with the spans covering all of it.
pub(super) fn segments(spans: &[ItemSpan]) -> Vec<Segment> {
    let cuts: BTreeSet<Instant> = spans
        .iter()
        .flat_map(|span| std::iter::once(span.from).chain(span.to))
        .collect();
    let mut edges: Vec<Option<Instant>> = vec![None];
    edges.extend(cuts.into_iter().map(Some));
    edges.push(None);
    edges
        .windows(2)
        .map(|pair| {
            let (from, to) = (pair[0], pair[1]);
            let covering: Vec<&ItemSpan> =
                spans.iter().filter(|span| span.covers(from, to)).collect();
            Segment {
                from,
                to,
                work: covering.iter().map(|span| span.work.clone()).collect(),
                runs: covering
                    .iter()
                    .filter_map(|span| span.run.clone())
                    .collect(),
            }
        })
        .collect()
}

/// Why a segment is no single item's, or `None` when one item covers it.
pub(super) fn unattributed_reason(segments: &[CostSegment], index: usize) -> Option<&'static str> {
    match segments[index].work.len() {
        1 => None,
        0 => {
            let covered = |segment: &CostSegment| !segment.work.is_empty();
            if !segments[..index].iter().any(covered) {
                Some(BEFORE)
            } else if !segments[index + 1..].iter().any(covered) {
                Some(AFTER)
            } else {
                Some(BETWEEN)
            }
        }
        _ => Some(OVERLAP),
    }
}

/// One part of a split figure: its facts and the requests it holds.
pub(super) type Part<'a> = (&'a CostFacts, u64);

/// A reason that says only that a part holding no request recorded nothing.
fn empty_part_reason(reason: &str) -> bool {
    reason == "recorded category is absent"
}

fn add_tokens(sum: Option<u64>, part: Option<u64>, requests: u64) -> Option<u64> {
    let part = match part {
        None if requests == 0 => 0,
        part => part?,
    };
    sum?.checked_add(part)
}

/// The figure the parts add up to, in the accounting basis of `basis`, with
/// the session-wide facts of `session`. A part with no request adds zero; a
/// counter a part with requests did not record leaves the sum absent.
pub(super) fn sum_facts(session: &CostFacts, basis: &CostFacts, parts: &[Part<'_>]) -> CostFacts {
    let mut facts = session.clone();
    facts.spans = None;
    facts.residual = None;
    facts.events = None;
    facts.list_price = None;
    facts.basis.clone_from(&basis.basis);
    facts.coverage.clone_from(&basis.coverage);
    let zero = Some(0);
    for category in CATEGORIES {
        let sum = parts.iter().fold(zero, |sum, (part, requests)| {
            add_tokens(sum, super::counter(&part.counters, category), *requests)
        });
        *counter_mut(&mut facts.counters, category) = sum;
    }
    facts.harness_estimate_usd = parts.iter().try_fold(0.0, |sum, (part, requests)| {
        match part.harness_estimate_usd {
            None if *requests == 0 => Some(sum),
            amount => Some(sum + amount?),
        }
    });
    let convention = basis
        .disjoint
        .as_ref()
        .or(session.disjoint.as_ref())
        .map(|disjoint| disjoint.convention.clone());
    facts.disjoint = convention.clone().map(|convention| {
        let held: Vec<(Option<&CostDisjoint>, u64)> = parts
            .iter()
            .map(|(part, requests)| (part.disjoint.as_ref(), *requests))
            .collect();
        sum_disjoint(convention, &held)
    });
    let models: BTreeSet<&String> = parts
        .iter()
        .flat_map(|(part, _)| part.by_model.iter().map(|share| &share.model))
        .collect();
    facts.by_model = match convention {
        Some(convention) => models
            .into_iter()
            .map(|model| {
                // A part with no share of the model consumed none of it.
                let held: Vec<(Option<&CostDisjoint>, u64)> = parts
                    .iter()
                    .map(|(part, requests)| {
                        match part.by_model.iter().find(|share| &share.model == model) {
                            Some(share) => (Some(&share.disjoint), *requests),
                            None => (None, 0),
                        }
                    })
                    .collect();
                CostModelShare {
                    model: model.clone(),
                    disjoint: sum_disjoint(convention.clone(), &held),
                }
            })
            .collect(),
        None => Vec::new(),
    };
    let mut gaps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (part, requests) in parts {
        for (name, reasons) in part.quantity_gaps.iter().flatten() {
            merge_reasons(&mut gaps, name, reasons, *requests);
        }
    }
    for category in CATEGORIES {
        if super::counter(&facts.counters, category).is_none() {
            merge_reasons(
                &mut gaps,
                category,
                &["recorded category is absent".to_string()],
                1,
            );
        }
    }
    if facts.harness_estimate_usd.is_none() {
        merge_reasons(
            &mut gaps,
            "usd",
            &["recorded category is absent".to_string()],
            1,
        );
    }
    facts.quantity_gaps = Some(gaps);
    let priced: Vec<&CostPricing> = parts
        .iter()
        .filter(|(_, requests)| *requests > 0)
        .filter_map(|(part, _)| part.cost_coverage.as_ref())
        .collect();
    facts.cost_coverage = (!priced.is_empty()).then(|| CostPricing {
        complete: priced.iter().all(|coverage| coverage.complete),
        zero_written_requests: priced
            .iter()
            .map(|coverage| coverage.zero_written_requests)
            .sum(),
        zero_written_observations: priced
            .iter()
            .map(|coverage| coverage.zero_written_observations)
            .sum(),
    });
    // Parts served by one model at different efforts are that model's.
    let served: BTreeSet<Option<&String>> = parts
        .iter()
        .filter(|(_, requests)| *requests > 0)
        .map(|(part, _)| part.served_model.as_ref())
        .collect();
    if !served.is_empty() {
        let models: BTreeSet<Option<&str>> = served
            .iter()
            .map(|model| model.and_then(|model| model.split('#').next()))
            .collect();
        facts.served_model = match (
            &served.iter().collect::<Vec<_>>()[..],
            &models.into_iter().collect::<Vec<_>>()[..],
        ) {
            ([Some(coordinate)], _) => Some((*coordinate).clone()),
            (_, [Some(model)]) => Some(model.to_string()),
            _ => None,
        };
    }
    for (part, _) in parts {
        for reason in &part.incomplete {
            if !facts.incomplete.contains(reason) {
                facts.incomplete.push(reason.clone());
            }
        }
    }
    facts
}

/// Disjoint categories added up, each with the requests its part holds.
fn sum_disjoint(
    convention: CostConvention,
    parts: &[(Option<&CostDisjoint>, u64)],
) -> CostDisjoint {
    let zero = Some(0);
    let category = |name: &str| {
        parts.iter().fold(zero, |sum, (part, requests)| {
            let amount = part.and_then(|disjoint| match name {
                "uncached_input" => disjoint.uncached_input,
                "cache_read" => disjoint.cache_read,
                "output" => disjoint.output,
                _ => disjoint.reasoning,
            });
            add_tokens(sum, amount, *requests)
        })
    };
    let bucket = |pick: fn(&CostCacheWrites) -> u64| {
        parts.iter().fold(zero, |sum, (part, requests)| {
            let amount = part
                .and_then(|disjoint| disjoint.cache_write.as_ref())
                .map(pick);
            add_tokens(sum, amount, *requests)
        })
    };
    let writes = bucket(|writes| writes.ephemeral_5m)
        .zip(bucket(|writes| writes.ephemeral_1h))
        .zip(bucket(|writes| writes.ttl_unrecorded))
        .map(
            |((ephemeral_5m, ephemeral_1h), ttl_unrecorded)| CostCacheWrites {
                ephemeral_5m,
                ephemeral_1h,
                ttl_unrecorded,
            },
        );
    let mut gaps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (part, requests) in parts {
        for (name, reasons) in part
            .and_then(|disjoint| disjoint.gaps.as_ref())
            .into_iter()
            .flatten()
        {
            merge_reasons(&mut gaps, name, reasons, *requests);
        }
    }
    CostDisjoint {
        convention,
        uncached_input: category("uncached_input"),
        cache_read: category("cache_read"),
        cache_write: writes,
        output: category("output"),
        reasoning: category("reasoning"),
        gaps: Some(gaps),
    }
}

fn merge_reasons(
    gaps: &mut BTreeMap<String, Vec<String>>,
    name: &str,
    reasons: &[String],
    requests: u64,
) {
    for reason in reasons {
        if requests == 0 && empty_part_reason(reason) {
            continue;
        }
        let held = gaps.entry(name.to_string()).or_default();
        if !held.contains(reason) {
            held.push(reason.clone());
            held.sort();
        }
    }
}

fn counter_mut<'a>(
    counters: &'a mut crate::model::CostCounters,
    category: &str,
) -> &'a mut Option<u64> {
    match category {
        "input" => &mut counters.input,
        "output" => &mut counters.output,
        "reasoning" => &mut counters.reasoning,
        "cache_read" => &mut counters.cache_read,
        _ => &mut counters.cache_write,
    }
}

/// The `index`th of `shares` even shares of a figure. Tokens stay whole: each
/// share is the quotient, and the first shares take one token each of the
/// remainder, so the shares add up to the figure.
pub(super) fn even_share(facts: &CostFacts, shares: usize, index: usize) -> CostFacts {
    let shares = shares.max(1) as u64;
    let index = index as u64;
    let split = |count: u64| count / shares + u64::from(index < count % shares);
    let mut share = facts.clone();
    for category in CATEGORIES {
        let slot = counter_mut(&mut share.counters, category);
        *slot = slot.map(split);
    }
    share.harness_estimate_usd = facts
        .harness_estimate_usd
        .map(|amount| amount / shares as f64);
    let scale = |disjoint: &mut CostDisjoint| {
        for slot in [
            &mut disjoint.uncached_input,
            &mut disjoint.cache_read,
            &mut disjoint.output,
            &mut disjoint.reasoning,
        ] {
            *slot = slot.map(split);
        }
        if let Some(writes) = disjoint.cache_write.as_mut() {
            writes.ephemeral_5m = split(writes.ephemeral_5m);
            writes.ephemeral_1h = split(writes.ephemeral_1h);
            writes.ttl_unrecorded = split(writes.ttl_unrecorded);
        }
    };
    share.disjoint.iter_mut().for_each(&scale);
    share
        .by_model
        .iter_mut()
        .for_each(|model| scale(&mut model.disjoint));
    share
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: u32) -> Instant {
        chrono::DateTime::parse_from_rfc3339(&format!("2026-01-01T10:{minute:02}:00Z"))
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn span(work: &str, from: u32, to: Option<u32>) -> ItemSpan {
        ItemSpan {
            work: WorkKey::Change(work.to_string()),
            run: None,
            from: at(from),
            to: to.map(at),
        }
    }

    #[test]
    fn segments_cut_the_timeline_at_every_boundary_and_never_share_a_request() {
        let cut = segments(&[
            span("a", 1, Some(5)),
            span("b", 3, Some(8)),
            span("c", 9, None),
        ]);
        let covered: Vec<Vec<String>> = cut.iter().map(Segment::work_labels).collect();
        assert_eq!(
            covered,
            [
                vec![],
                vec!["change:a".to_string()],
                vec!["change:a".to_string(), "change:b".to_string()],
                vec!["change:b".to_string()],
                vec![],
                vec!["change:c".to_string()],
            ]
        );
        assert_eq!(cut.first().unwrap().from, None);
        assert_eq!(cut.last().unwrap().to, None);
        for pair in cut.windows(2) {
            assert_eq!(pair[0].to, pair[1].from);
        }
    }

    #[test]
    fn even_shares_add_up_to_the_figure() {
        let mut facts: CostFacts = serde_json::from_value(serde_json::json!({
            "reader": "r", "calculation": "c", "frontier": 0,
            "counters": {"input": 10, "output": 7},
            "harness_estimate_usd": 0.3,
        }))
        .unwrap();
        facts.counters.cache_read = Some(2);
        let shares: Vec<CostFacts> = (0..3).map(|index| even_share(&facts, 3, index)).collect();
        let input: u64 = shares
            .iter()
            .map(|share| share.counters.input.unwrap())
            .sum();
        let output: u64 = shares
            .iter()
            .map(|share| share.counters.output.unwrap())
            .sum();
        let read: u64 = shares
            .iter()
            .map(|share| share.counters.cache_read.unwrap())
            .sum();
        assert_eq!((input, output, read), (10, 7, 2));
        assert_eq!(shares[0].counters.input, Some(4));
        assert_eq!(shares[2].counters.input, Some(3));
    }
}
