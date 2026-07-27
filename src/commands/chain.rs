use super::*;
use crate::chain::{Chain, ChainMember, ChainPlan, ChainReview, ChainReviewWindow, CHAIN_SCHEMA};

pub fn chain(ctx: &Ctx, tag: String, json: bool, review: bool) -> Result<()> {
    let tags = normalize_tags(vec![tag])?;
    let tag = tags
        .first()
        .context("chain requires exactly one non-empty tag")?
        .clone();
    let store = ctx.store()?;
    let _graph = store.lock_graph()?;
    let states = ctx.load_all_states(&store)?;
    let selected = states
        .iter()
        .filter(|(_, state)| state.tags.contains(&tag))
        .map(|(change_id, state)| (change_id.clone(), state.clone()))
        .collect::<BTreeMap<_, _>>();
    let ordered = dependency_order(&selected)?;

    let members = ordered
        .iter()
        .map(|change_id| {
            let state = &selected[change_id];
            let brief = state.latest_brief();
            let review = review.then(|| {
                let Some(patchset) = state.latest_patchset() else {
                    return ChainReview {
                        subject: None,
                        non_self_verdict: false,
                        at_final: ChainReviewWindow {
                            verdicts: 0,
                            identities: Vec::new(),
                            findings: 0,
                            ad_hoc_verifications: 0,
                        },
                        lifetime: ChainReviewWindow {
                            verdicts: 0,
                            identities: Vec::new(),
                            findings: 0,
                            ad_hoc_verifications: 0,
                        },
                    };
                };
                let subject = patchset
                    .on_behalf_of
                    .as_deref()
                    .unwrap_or(&patchset.actor)
                    .to_string();
                let window = |patchset_ids: &BTreeSet<&str>, heads: &BTreeSet<&str>| {
                    let verdicts = state
                        .verdicts
                        .iter()
                        .filter(|verdict| patchset_ids.contains(verdict.patchset_id.as_str()))
                        .collect::<Vec<_>>();
                    let identities = verdicts
                        .iter()
                        .map(|verdict| {
                            verdict
                                .on_behalf_of
                                .as_deref()
                                .unwrap_or(&verdict.actor)
                                .to_string()
                        })
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>();
                    ChainReviewWindow {
                        verdicts: verdicts.len(),
                        identities,
                        findings: state
                            .findings
                            .values()
                            .filter(|finding| {
                                finding
                                    .patchset_id
                                    .as_deref()
                                    .is_some_and(|id| patchset_ids.contains(id))
                            })
                            .count(),
                        ad_hoc_verifications: state
                            .verifications
                            .iter()
                            .filter(|entry| {
                                heads.contains(entry.revision.as_str()) && entry.gate.is_none()
                            })
                            .count(),
                    }
                };
                let final_patchset_ids = BTreeSet::from([patchset.id.as_str()]);
                let final_heads = BTreeSet::from([patchset.head.as_str()]);
                let lifetime_patchset_ids = state
                    .patchsets
                    .iter()
                    .map(|patchset| patchset.id.as_str())
                    .collect::<BTreeSet<_>>();
                let lifetime_heads = state
                    .patchsets
                    .iter()
                    .map(|patchset| patchset.head.as_str())
                    .collect::<BTreeSet<_>>();
                let at_final = window(&final_patchset_ids, &final_heads);
                let lifetime = window(&lifetime_patchset_ids, &lifetime_heads);
                ChainReview {
                    non_self_verdict: lifetime
                        .identities
                        .iter()
                        .any(|identity| identity != &subject),
                    subject: Some(subject),
                    at_final,
                    lifetime,
                }
            });
            ChainMember {
                change_id: state.change_id.clone(),
                slug: state.slug.clone(),
                title: state.title.clone(),
                state: if state.is_closed() {
                    "closed".into()
                } else {
                    "open".into()
                },
                plan_ref: brief.and_then(|brief| brief.plan_ref.clone()),
                plan_slice: brief.and_then(|brief| brief.plan_slice.clone()),
                base_revision: brief.and_then(|brief| brief.base_revision.clone()),
                review,
            }
        })
        .collect::<Vec<_>>();

    let mut plan_events = Vec::new();
    for state in selected.values() {
        if let Some(plan_ref) = state
            .journal_ref
            .as_ref()
            .filter(|reference| reference.ends_with("-plan.md"))
        {
            plan_events.push((state.opened_at, state.change_id.clone(), plan_ref.clone()));
        }
        plan_events.extend(state.briefs.iter().filter_map(|brief| {
            brief
                .plan_ref
                .as_ref()
                .map(|plan_ref| (brief.ts, brief.event_id.clone(), plan_ref.clone()))
        }));
    }
    plan_events.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let mut seen = BTreeSet::new();
    let mut plans = plan_events
        .into_iter()
        .filter_map(|(_, _, plan_ref)| {
            seen.insert(plan_ref.clone()).then_some(ChainPlan {
                plan_ref,
                current: false,
            })
        })
        .collect::<Vec<_>>();
    if let Some(current) = plans.last_mut() {
        current.current = true;
    }

    let next_ready = claims::ready_candidate(&states, &tags, chrono::Utc::now())
        .map(|state| state.change_id.clone());
    let output = Chain {
        schema: CHAIN_SCHEMA,
        tag,
        members,
        plans,
        next_ready,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Chain: {}", output.tag);
        if output.members.is_empty() {
            println!("Members: none");
        } else {
            println!("Members:");
            for member in &output.members {
                println!(
                    "- {} — {} [{}]",
                    member.change_id, member.title, member.state
                );
                if let (Some(plan_ref), Some(plan_slice)) = (&member.plan_ref, &member.plan_slice) {
                    println!("  plan: {plan_ref} ({plan_slice})");
                }
                if let Some(review) = &member.review {
                    println!(
                        "  review: {} verdicts ({} at final), {} identities, {} findings, {} ad hoc verifications",
                        review.lifetime.verdicts,
                        review.at_final.verdicts,
                        review.lifetime.identities.len(),
                        review.lifetime.findings,
                        review.lifetime.ad_hoc_verifications
                    );
                }
            }
        }
        if output.plans.is_empty() {
            println!("Plans: none");
        } else {
            println!("Plans:");
            for plan in &output.plans {
                let marker = if plan.current { " (current)" } else { "" };
                println!("- {}{marker}", plan.plan_ref);
            }
        }
        match &output.next_ready {
            Some(change_id) => println!("Next ready: {change_id}"),
            None => println!("Next ready: none"),
        }
    }
    Ok(())
}
