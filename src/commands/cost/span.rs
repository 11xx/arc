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
    CostRequestTerms, CostSegment,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Instant = chrono::DateTime<chrono::Utc>;

pub(super) const OVERLAP: &str = "overlap";
pub(super) const BEFORE: &str = "before first item";
pub(super) const BETWEEN: &str = "between items";
pub(super) const AFTER: &str = "after last item";
pub(super) const UNTIMED: &str = "request time unrecorded";
pub(super) const NOT_SPLITTABLE: &str = "recorded total not splittable";
/// Why a share is outside a view's total: the item it belongs to is not selected.
pub(super) const UNSELECTED: &str = "unselected item";
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

/// How a session with several spans is read: its segments, and for a
/// participant, its own events on each item, which its working-stretch
/// estimate is cut by.
#[derive(Clone, Default)]
pub(super) struct Plan {
    pub segments: Vec<Segment>,
    pub events: Vec<(Instant, WorkKey)>,
}

impl std::ops::Deref for Plan {
    type Target = [Segment];

    fn deref(&self) -> &[Segment] {
        &self.segments
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

/// The working-stretch estimate's basis, as figures name it.
pub(super) const ESTIMATE_BASIS: &str = "working-stretch";
/// Why a participant's read estimated nothing: its reader keeps no request
/// timeline for it.
pub(super) const TIMELINE_UNAVAILABLE: &str = "request timeline unavailable";
/// Why retained facts estimate nothing for a view: they were estimated at
/// another idle gap than the one it asks for.
pub(super) const RETAINED_IDLE: &str = "retained facts were estimated at another idle gap";

/// One part `[from, to)` of a segment no item covers, as a participant's
/// working stretches and its own events in them cut it, and the item the
/// estimate gives it: none for a stretch with no event of the participant.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Window {
    pub segment: usize,
    pub from: Option<Instant>,
    pub to: Option<Instant>,
    pub work: Option<WorkKey>,
}

/// The later of two starts, an absent one being the timeline's beginning.
fn later(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, None) | (None, a) => a,
    }
}

/// The earlier of two ends, an absent one being the timeline's end.
fn earlier(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) | (None, a) => a,
    }
}

fn nonempty(from: Option<Instant>, to: Option<Instant>) -> bool {
    from.zip(to).is_none_or(|(from, to)| from < to)
}

/// The estimate parts of every segment no item covers. Working stretch `i`
/// holds the requests from its first stamp, `starts[i]`, to the next
/// stretch's, the first from the timeline's beginning and the last to its
/// end, as tapes places requests in stretches, and the participant's events
/// in that window are the stretch's. Within a stretch, the requests up to
/// and including an event serve that event's item, and those after its last
/// event serve the same item; a stretch with no event serves none. A
/// segment several items cover is left to its evidence.
pub(super) fn estimate_windows(
    segments: &[Segment],
    starts: &[Instant],
    events: &[(Instant, WorkKey)],
) -> Vec<Window> {
    let mut pieces: Vec<(Option<Instant>, Option<Instant>, Option<WorkKey>)> = Vec::new();
    for index in 0..starts.len() {
        let from = (index > 0).then(|| starts[index]);
        let to = starts.get(index + 1).copied();
        let mut inside: Vec<&(Instant, WorkKey)> = events
            .iter()
            .filter(|(at, _)| from.is_none_or(|from| *at >= from) && to.is_none_or(|to| *at < to))
            .collect();
        inside.sort_by_key(|(at, _)| *at);
        let Some((_, last)) = inside.last() else {
            pieces.push((from, to, None));
            continue;
        };
        let mut cursor = from;
        for (at, work) in &inside {
            let cut = Some(*at + chrono::Duration::nanoseconds(1));
            pieces.push((cursor, cut, Some(work.clone())));
            cursor = cut;
        }
        pieces.push((cursor, to, Some(last.clone())));
    }
    let mut windows: Vec<Window> = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        if !segment.work.is_empty() {
            continue;
        }
        for (from, to, work) in &pieces {
            let from = later(segment.from, *from);
            let to = earlier(segment.to, *to);
            if !nonempty(from, to) {
                continue;
            }
            match windows.last_mut() {
                Some(last) if last.segment == index && &last.work == work && last.to == from => {
                    last.to = to;
                }
                _ => windows.push(Window {
                    segment: index,
                    from,
                    to,
                    work: work.clone(),
                }),
            }
        }
    }
    windows
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
    facts.request_terms = request_terms(session, parts);
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
                // A part with no share of the model consumed none of it; a
                // part that served it at several efforts holds a share for
                // each, and every one of them adds.
                let held: Vec<(Option<&CostDisjoint>, u64)> = parts
                    .iter()
                    .flat_map(|(part, requests)| {
                        let shares: Vec<(Option<&CostDisjoint>, u64)> = part
                            .by_model
                            .iter()
                            .filter(|share| &share.model == model)
                            .map(|share| (Some(&share.disjoint), *requests))
                            .collect();
                        if shares.is_empty() {
                            vec![(None, 0)]
                        } else {
                            shares
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
    // A part that holds no request measured nothing to qualify.
    for (part, _) in parts.iter().filter(|(_, requests)| *requests > 0) {
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

/// How the segments and the remainder stand against the whole: the counters
/// and disjoint categories they do not add up to, and those one side leaves
/// unknown, which cannot be compared. A category neither side records is
/// neither.
#[derive(Default)]
pub(super) struct Conservation {
    pub differs: Vec<String>,
    pub uncompared: Vec<String>,
}

pub(super) fn conservation(session: &CostFacts, spans: &crate::model::CostSpans) -> Conservation {
    let mut held = Conservation::default();
    let Some(whole) = spans.whole.as_deref() else {
        held.uncompared.push("whole".to_string());
        return held;
    };
    let parts: Vec<Part<'_>> = spans
        .segments
        .iter()
        .map(|segment| (&*segment.facts, segment.requests))
        .chain(
            spans
                .remainder
                .as_deref()
                .map(|remainder| (remainder, spans.remainder_requests)),
        )
        .collect();
    let sum = sum_facts(session, whole, &parts);
    let mut compare = |name: String, pair: (Option<u64>, Option<u64>)| match pair {
        (Some(parts), Some(total)) if parts != total => held.differs.push(name),
        (Some(_), None) | (None, Some(_)) => held.uncompared.push(name),
        _ => {}
    };
    for category in CATEGORIES {
        compare(
            category.to_string(),
            (
                super::counter(&sum.counters, category),
                super::counter(&whole.counters, category),
            ),
        );
    }
    let categories = ["uncached_input", "cache_read", "cache_write", "output"];
    for category in categories {
        let amount = |disjoint: &Option<CostDisjoint>| {
            disjoint
                .as_ref()
                .and_then(|disjoint| super::disjoint_amount(disjoint, category))
        };
        compare(
            format!("disjoint {category}"),
            (amount(&sum.disjoint), amount(&whole.disjoint)),
        );
    }
    held
}

/// The requests the parts hold, one by one where every part with requests
/// kept them, and the activity they span, open at an end a part leaves
/// unknown.
fn request_terms(session: &CostFacts, parts: &[Part<'_>]) -> CostRequestTerms {
    let held: Vec<&CostRequestTerms> = parts
        .iter()
        .filter(|(_, requests)| *requests > 0)
        .map(|(part, _)| &part.request_terms)
        .collect();
    let requests = held.iter().try_fold(
        agent_tapes_core::pricing::RequestUses::default(),
        |mut sum, terms| {
            sum.extend(terms.requests.as_ref()?);
            Some(sum)
        },
    );
    let from = held
        .iter()
        .map(|terms| terms.from)
        .try_fold(None, |earliest: Option<Instant>, from| {
            Some(earliest.into_iter().chain(Some(from?)).min())
        })
        .flatten();
    let to = held
        .iter()
        .map(|terms| terms.to)
        .try_fold(None, |latest: Option<Instant>, to| {
            Some(latest.into_iter().chain(Some(to?)).max())
        })
        .flatten();
    CostRequestTerms {
        requests,
        billing: session.request_terms.billing.clone(),
        from,
        to,
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
    // A share of each request is no request of its own.
    share.request_terms.requests = None;
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

    fn facts(input: u64) -> CostFacts {
        serde_json::from_value(serde_json::json!({
            "reader": "r", "calculation": "c", "frontier": 0, "counters": {"input": input},
        }))
        .unwrap()
    }

    // Claim: a participant's events cut the segments no item covers within
    // each working stretch: requests up to an event serve its item, those
    // after a stretch's last event serve the same item, a stretch with no
    // event serves none, and a segment one item covers is not cut. Rejects
    // an estimate that crosses an idle gap or touches measured segments.
    #[test]
    fn working_stretches_and_events_cut_the_uncovered_segments() {
        let change = |name: &str| WorkKey::Change(name.to_string());
        let plain = segments(&[
            ItemSpan::events(change("a"), at(20), at(20)),
            ItemSpan::events(change("b"), at(50), at(50)),
        ]);
        let events = vec![(at(20), change("a")), (at(50), change("b"))];
        let nanosecond = chrono::Duration::nanoseconds(1);
        // One stretch from 10:00, and another from 10:40 with no event of its
        // own before the second verdict at 10:50.
        let windows = estimate_windows(&plain, &[at(0), at(40)], &events);
        let cut: Vec<(Option<Instant>, Option<Instant>, Option<String>)> = windows
            .iter()
            .map(|window| {
                (
                    window.from,
                    window.to,
                    window.work.as_ref().map(WorkKey::label),
                )
            })
            .collect();
        assert_eq!(
            cut,
            vec![
                (None, Some(at(20)), Some("change:a".to_string())),
                (
                    Some(at(20) + nanosecond),
                    Some(at(40)),
                    Some("change:a".to_string())
                ),
                (Some(at(40)), Some(at(50)), Some("change:b".to_string())),
                (
                    Some(at(50) + nanosecond),
                    None,
                    Some("change:b".to_string())
                ),
            ]
        );
        assert!(windows
            .iter()
            .all(|window| plain[window.segment].work.is_empty()));
        // A stretch with no event of the participant serves no item.
        let windows = estimate_windows(&plain, &[at(0), at(30), at(45)], &events);
        assert!(windows.iter().any(|window| window.from == Some(at(30))
            && window.to == Some(at(45))
            && window.work.is_none()));
    }

    // Claim: a split whose segment figures exceed the whole, as a counter
    // reset leaves them, names the counter that does not conserve, and one
    // whose figures add up names none. Rejects reporting conservation the
    // figures do not show.
    #[test]
    fn a_split_names_the_counters_its_segments_do_not_conserve() {
        let segment = |input: u64| CostSegment {
            from: None,
            to: None,
            work: Vec::new(),
            runs: Vec::new(),
            requests: 1,
            facts: Box::new(facts(input)),
            estimates: Vec::new(),
        };
        let spans = |whole: u64| crate::model::CostSpans {
            unsplit: None,
            segments: vec![segment(3), segment(4)],
            remainder: None,
            remainder_requests: 0,
            whole: Some(Box::new(facts(whole))),
            estimate: None,
        };
        assert!(conservation(&facts(7), &spans(7)).differs.is_empty());
        assert_eq!(
            conservation(&facts(5), &spans(5)).differs,
            vec!["input".to_string()]
        );
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
