use super::*;
use crate::chain::{Chain, ChainMember, ChainPlan, CHAIN_SCHEMA};

pub fn chain(ctx: &Ctx, tag: String, json: bool) -> Result<()> {
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
