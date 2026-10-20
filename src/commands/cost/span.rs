//! How arc reads a session's division among the work its ledger spans
//! cover, through tapes' attribution engine. The ledger gives the evidence:
//! an executor's run from its dispatch to its recorded end, open while it has
//! none, and a participant's own events on each item. The engine cuts the
//! session's timeline into the segments and estimate windows a read takes one
//! figure for, and divides the figures a read retained among the items. arc
//! keeps the figure arithmetic and the reasons that come from its ledger.

use super::{WorkKey, CATEGORIES};
use crate::model::{
    CostCacheWrites, CostConvention, CostDisjoint, CostFacts, CostModelShare, CostPricing,
    CostRequestTerms, CostSegment, CostSpans,
};
use agent_tapes_core::attribution::{
    self, merge_reasons, sum_amounts, sum_counts, Allocation, Attribution, Basis, Conservation,
    Cover, Evidence, Figure, Figures, Requests, Take, Tier, Window, ABSENT_CATEGORY,
};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Instant = agent_tapes_core::span::Instant;

pub(super) const UNSUPPORTED: &str = "span reading unsupported";
pub(super) const RETAINED_MISMATCH: &str = "retained spans do not match the ledger";
pub(super) const UNMEASURED: &str = "unmeasured";
pub(super) const INCOMPLETE_MEMBERSHIP: &str = "ledger records are unreadable";
/// Why a participant with an item only events recorded on its behalf name
/// is not divided: those events carry the invoker's instants, not its own.
pub(super) const ON_BEHALF: &str = "recorded on its behalf";

/// The evidence the ledger gives one session with several spans: an
/// executor's runs, or a participant's own events.
#[derive(Clone, Default)]
pub(super) struct Plan {
    pub spans: Vec<attribution::Span<WorkKey>>,
    pub events: Vec<attribution::Event<WorkKey>>,
}

impl Plan {
    /// The ledger gives the session no spans to divide it by.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty() && self.events.is_empty()
    }

    /// The segments and estimate windows a read takes one figure for. A
    /// participant's segments no item covers are cut by its working
    /// stretches, whose first stamps are `starts`, where the read has them.
    pub fn cut(&self, starts: Option<&[Instant]>) -> attribution::Plan<WorkKey> {
        attribution::Plan::new(&self.spans, &self.events, starts)
    }
}

/// The covering work items of a segment, by their view labels.
pub(super) fn work_labels(segment: &attribution::Segment<WorkKey>) -> Vec<String> {
    segment
        .work
        .iter()
        .map(|cover| cover.work.label())
        .collect()
}

/// Whether a retained segment has this definition.
pub(super) fn defines(segment: &attribution::Segment<WorkKey>, retained: &CostSegment) -> bool {
    retained.bounds()
        == (
            segment.from,
            segment.to,
            &work_labels(segment)[..],
            &segment.runs[..],
        )
}

/// Why a participant's read estimated nothing: its reader keeps no request
/// timeline for it.
pub(super) const TIMELINE_UNAVAILABLE: &str = "request timeline unavailable";
/// Why retained facts estimate nothing for a view: they were estimated at
/// another idle gap than the one it asks for.
pub(super) const RETAINED_IDLE: &str = "retained facts were estimated at another idle gap";

/// The plan retained segments were read along, by their items' view labels.
/// Each segment keeps its items in the order it retained them, which even
/// shares are indexed by. A covered segment with a run is an executor's
/// span; one without is a participant's event window.
pub(super) fn retained_plan(spans: &CostSpans) -> attribution::Plan<String> {
    let segments = spans
        .segments
        .iter()
        .map(|segment| attribution::Segment {
            from: segment.from,
            to: segment.to,
            work: segment
                .work
                .iter()
                .map(|work| Cover {
                    work: work.clone(),
                    evidence: Evidence {
                        tier: Tier::Bound,
                        basis: if segment.runs.is_empty() {
                            Basis::EventWindow
                        } else {
                            Basis::Span
                        },
                    },
                })
                .collect(),
            runs: segment.runs.clone(),
        })
        .collect();
    let windows = spans
        .segments
        .iter()
        .enumerate()
        .flat_map(|(index, segment)| {
            segment.estimates.iter().map(move |part| Window {
                segment: index,
                from: part.from,
                to: part.to,
                work: part.work.clone().map(|work| (work, Tier::Estimated)),
            })
        })
        .collect();
    attribution::Plan { segments, windows }
}

/// A divided session's retained figures, as the engine divides them.
pub(super) struct Retained<'a> {
    session: &'a CostFacts,
    spans: &'a CostSpans,
    pub plan: attribution::Plan<String>,
    pub requests: Requests,
    figures: Figures<Held<'a>>,
}

/// One retained figure, or a share of one.
#[derive(Clone)]
struct Held<'a>(Cow<'a, CostFacts>);

impl Figure for Held<'_> {
    fn even_share(&self, shares: usize, index: usize) -> Self {
        Held(Cow::Owned(even_share(&self.0, shares, index)))
    }
}

impl<'a> Retained<'a> {
    pub fn new(session: &'a CostFacts, spans: &'a CostSpans) -> Self {
        let held = |facts: &'a CostFacts| Held(Cow::Borrowed(facts));
        let parts = || spans.segments.iter().flat_map(|segment| &segment.estimates);
        Self {
            session,
            spans,
            plan: retained_plan(spans),
            requests: Requests {
                segments: spans
                    .segments
                    .iter()
                    .map(|segment| segment.requests)
                    .collect(),
                windows: parts().map(|part| part.requests).collect(),
                remainder: spans.remainder_requests,
            },
            figures: Figures {
                segments: spans
                    .segments
                    .iter()
                    .map(|segment| held(&segment.facts))
                    .collect(),
                windows: parts().map(|part| held(&part.facts)).collect(),
                remainder: spans.remainder.as_deref().map(held),
            },
        }
    }

    /// The session's layers, with the allocation `method` asks for.
    pub fn attribute(&self, method: Option<Allocation>) -> Option<Attribution<String>> {
        let splittable = !self
            .spans
            .whole
            .as_ref()
            .is_some_and(|whole| whole.basis != self.session.basis);
        self.plan.attribute(&self.requests, method, splittable)
    }

    /// The layer `take` names.
    pub fn layer(&self, take: Take) -> Option<attribution::Layer<String>> {
        let method = match take {
            Take::Allocated(method) => Some(method),
            Take::Conserving | Take::Estimated => None,
        };
        let attribution = self.attribute(method)?;
        attribution.layer(take).cloned()
    }

    /// The figure `parts` add up to, in the segments' basis.
    pub fn sum(&self, parts: &[attribution::Part]) -> Option<CostFacts> {
        let whole = self.spans.whole.as_deref()?;
        let held = self.figures.parts(parts)?;
        let parts: Vec<Part<'_>> = held
            .iter()
            .map(|(figure, requests)| (&*figure.0, *requests))
            .collect();
        Some(sum_facts(self.session, whole, &parts))
    }

    /// How the parts of layer `take` stand against the whole in the
    /// segments' basis.
    pub fn conservation(&self, take: Take) -> Conservation {
        let sum = self.layer(take).and_then(|layer| {
            let parts: Vec<attribution::Part> = layer.parts.iter().map(|(part, _)| *part).collect();
            self.sum(&parts)
        });
        let (Some(whole), Some(sum)) = (self.spans.whole.as_deref(), sum) else {
            return Conservation {
                uncompared: vec!["whole".to_string()],
                ..Conservation::default()
            };
        };
        let mut held = Conservation::default();
        for category in CATEGORIES {
            held.compare(
                category,
                super::counter(&sum.counters, category),
                super::counter(&whole.counters, category),
            );
        }
        for category in ["uncached_input", "cache_read", "cache_write", "output"] {
            let amount = |disjoint: &Option<CostDisjoint>| {
                disjoint
                    .as_ref()
                    .and_then(|disjoint| super::disjoint_amount(disjoint, category))
            };
            held.compare(
                format!("disjoint {category}"),
                amount(&sum.disjoint),
                amount(&whole.disjoint),
            );
        }
        held
    }
}

/// One part of a split figure: its facts and the requests it holds.
pub(super) type Part<'a> = (&'a CostFacts, u64);

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
    for category in CATEGORIES {
        *counter_mut(&mut facts.counters, category) = sum_counts(
            parts
                .iter()
                .map(|(part, requests)| (super::counter(&part.counters, category), *requests)),
        );
    }
    facts.harness_estimate_usd = sum_amounts(
        parts
            .iter()
            .map(|(part, requests)| (part.harness_estimate_usd, *requests)),
    );
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
    let mut gaps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (part, requests) in parts {
        for (name, reasons) in part.quantity_gaps.iter().flatten() {
            merge_reasons(&mut gaps, name, reasons, *requests);
        }
    }
    for category in CATEGORIES {
        if super::counter(&facts.counters, category).is_none() {
            merge_reasons(&mut gaps, category, &[ABSENT_CATEGORY.to_string()], 1);
        }
    }
    if facts.harness_estimate_usd.is_none() {
        merge_reasons(&mut gaps, "usd", &[ABSENT_CATEGORY.to_string()], 1);
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
    facts.attribution_uncertain = parts
        .iter()
        .any(|(part, requests)| *requests > 0 && part.attribution_uncertain);
    // Where the parts name several models, or a part is priced share by
    // share, a part one model served whole is that model's share.
    let by_shares =
        facts.served_model.is_none() || parts.iter().any(|(part, _)| !part.by_model.is_empty());
    let shares: Vec<Vec<(String, &CostDisjoint)>> = parts
        .iter()
        .map(|(part, _)| {
            if !part.by_model.is_empty() {
                return part
                    .by_model
                    .iter()
                    .map(|share| (share.model.clone(), &share.disjoint))
                    .collect();
            }
            part.served_model
                .as_deref()
                .filter(|_| by_shares)
                .and_then(super::model_of)
                .zip(part.disjoint.as_ref())
                .into_iter()
                .collect()
        })
        .collect();
    let models: BTreeSet<&String> = shares.iter().flatten().map(|(model, _)| model).collect();
    facts.by_model = match convention {
        Some(convention) => models
            .into_iter()
            .map(|model| {
                // A part with no share of the model consumed none of it; a
                // part whose facts keep a share per effort, as another
                // calculation retained them, adds every one of them.
                let held: Vec<(Option<&CostDisjoint>, u64)> = parts
                    .iter()
                    .zip(&shares)
                    .flat_map(|((_, requests), shares)| {
                        let shares: Vec<(Option<&CostDisjoint>, u64)> = shares
                            .iter()
                            .filter(|(held, _)| held == model)
                            .map(|(_, disjoint)| (Some(*disjoint), *requests))
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
pub(super) fn sum_disjoint(
    convention: CostConvention,
    parts: &[(Option<&CostDisjoint>, u64)],
) -> CostDisjoint {
    let category = |name: &str| {
        sum_counts(parts.iter().map(|(part, requests)| {
            let amount = part.and_then(|disjoint| match name {
                "uncached_input" => disjoint.uncached_input,
                "cache_read" => disjoint.cache_read,
                "output" => disjoint.output,
                _ => disjoint.reasoning,
            });
            (amount, *requests)
        }))
    };
    let bucket = |pick: fn(&CostCacheWrites) -> u64| {
        sum_counts(parts.iter().map(|(part, requests)| {
            let amount = part
                .and_then(|disjoint| disjoint.cache_write.as_ref())
                .map(pick);
            (amount, *requests)
        }))
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
pub(super) fn conservation(session: &CostFacts, spans: &CostSpans) -> Conservation {
    Retained::new(session, spans).conservation(Take::Conserving)
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
    let split = |count: u64| attribution::even_split(count, shares, index);
    let mut share = facts.clone();
    for category in CATEGORIES {
        let slot = counter_mut(&mut share.counters, category);
        *slot = slot.map(split);
    }
    share.harness_estimate_usd = facts
        .harness_estimate_usd
        .map(|amount| attribution::even_amount(amount, shares));
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

    fn facts(input: u64) -> CostFacts {
        serde_json::from_value(serde_json::json!({
            "reader": "r", "calculation": "c", "frontier": 0, "counters": {"input": input},
        }))
        .unwrap()
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
