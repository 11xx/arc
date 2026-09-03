use crate::ids;
use crate::model::{ActorSource, Event, Payload};
use crate::render;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use super::Ctx;

struct PassHistory {
    events: Vec<Event>,
    passes: BTreeMap<String, PassRecord>,
}

struct PassRecord {
    pass_id: String,
    opened: Event,
    members: Vec<String>,
    note: Option<String>,
    ending: Option<PassEnding>,
}

enum PassEnding {
    Completed { event: Event, note: Option<String> },
    Abandoned { event: Event, reason: String },
}

#[derive(Serialize)]
struct PassView {
    pass_id: String,
    opened_event_id: String,
    opened_at: DateTime<Utc>,
    opened_by: String,
    opened_actor_source: Option<ActorSource>,
    opened_on_behalf_of: Option<String>,
    opened_harness: Option<String>,
    opened_session: Option<String>,
    members: Vec<PassMemberView>,
    note: Option<String>,
    state: String,
    ending_event_id: Option<String>,
    ending_at: Option<DateTime<Utc>>,
    ending_by: Option<String>,
    ending_actor_source: Option<ActorSource>,
    ending_on_behalf_of: Option<String>,
    ending_harness: Option<String>,
    ending_session: Option<String>,
    ending_note: Option<String>,
    reason: Option<String>,
}

#[derive(Serialize)]
struct PassMemberView {
    member: String,
    change_id: String,
    patchset_id: String,
    covered: bool,
    verdict_event_id: Option<String>,
}

struct MemberParts {
    change_id: String,
    patchset_id: String,
}

pub fn open_pass(ctx: &Ctx, raw_members: Vec<String>, note: Option<String>) -> Result<()> {
    let note = optional_text(note, "--note")?;
    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    let members = validate_members(&store, raw_members)?;
    let pass_id = ids::new_event_id();
    let event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::ReviewPassOpened {
            pass_id: pass_id.clone(),
            members,
            note,
        },
    );
    store.append_repository_event(&event)?;
    println!("pass: {pass_id}");
    println!("review pass declared by caller");
    println!("event: {}", event.event_id);
    Ok(())
}

pub fn complete_pass(ctx: &Ctx, pass_id: String, note: Option<String>) -> Result<()> {
    let note = optional_text(note, "--note")?;
    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    let history = load_history(&store)?;
    let pass = find_pass(&history, &pass_id)?;
    refuse_ended(pass)?;
    let canonical_id = pass.pass_id.clone();
    let event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::ReviewPassCompleted {
            pass_id: canonical_id.clone(),
            note,
        },
    );
    store.append_repository_event(&event)?;
    println!("pass completed by caller: {canonical_id}");
    println!("event: {}", event.event_id);
    Ok(())
}

pub fn abandon_pass(ctx: &Ctx, pass_id: String, reason: String) -> Result<()> {
    let reason = required_text(reason, "--reason")?;
    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    let history = load_history(&store)?;
    let pass = find_pass(&history, &pass_id)?;
    refuse_ended(pass)?;
    let canonical_id = pass.pass_id.clone();
    let event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::ReviewPassAbandoned {
            pass_id: canonical_id.clone(),
            reason,
        },
    );
    store.append_repository_event(&event)?;
    println!("pass abandoned by caller: {canonical_id}");
    println!("event: {}", event.event_id);
    Ok(())
}

pub fn list_passes(ctx: &Ctx, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let _repository_events = store.lock_repository_events()?;
    let history = load_history(&store)?;
    let mut views = history
        .passes
        .values()
        .map(|pass| pass_view(&store, pass))
        .collect::<Result<Vec<_>>>()?;
    views.sort_by(|a, b| b.opened_event_id.cmp(&a.opened_event_id));

    if json {
        println!("{}", serde_json::to_string_pretty(&views)?);
        return Ok(());
    }
    if views.is_empty() {
        println!("no review passes");
        return Ok(());
    }

    let mut passes = history.passes.values().collect::<Vec<_>>();
    passes.sort_by(|a, b| b.opened.event_id.cmp(&a.opened.event_id));
    for pass in passes {
        let view = pass_view(&store, pass)?;
        render_pass(pass, &view);
    }
    Ok(())
}

fn load_history(store: &Store) -> Result<PassHistory> {
    let events = store.load_repository_events()?;
    let mut passes = BTreeMap::new();
    for event in &events {
        match &event.payload {
            Payload::ReviewPassOpened {
                pass_id,
                members,
                note,
            } => {
                ids::validate_id_component(pass_id).with_context(|| {
                    format!(
                        "review pass event {} has an invalid pass id",
                        event.event_id
                    )
                })?;
                if members.is_empty() {
                    bail!("review pass {pass_id} has no members");
                }
                for member in members {
                    parse_member(member).with_context(|| {
                        format!("review pass {pass_id} has invalid member {member:?}")
                    })?;
                }
                if passes
                    .insert(
                        pass_id.clone(),
                        PassRecord {
                            pass_id: pass_id.clone(),
                            opened: event.clone(),
                            members: members.clone(),
                            note: note.clone(),
                            ending: None,
                        },
                    )
                    .is_some()
                {
                    bail!("review pass {pass_id} was opened more than once");
                }
            }
            Payload::ReviewPassCompleted { pass_id, note } => {
                add_ending(
                    &mut passes,
                    pass_id,
                    PassEnding::Completed {
                        event: event.clone(),
                        note: note.clone(),
                    },
                )?;
            }
            Payload::ReviewPassAbandoned { pass_id, reason } => {
                add_ending(
                    &mut passes,
                    pass_id,
                    PassEnding::Abandoned {
                        event: event.clone(),
                        reason: reason.clone(),
                    },
                )?;
            }
            _ => {}
        }
    }
    Ok(PassHistory { events, passes })
}

fn add_ending(
    passes: &mut BTreeMap<String, PassRecord>,
    pass_id: &str,
    ending: PassEnding,
) -> Result<()> {
    let pass = passes
        .get_mut(pass_id)
        .with_context(|| format!("review pass ending event references unknown pass {pass_id:?}"))?;
    if pass.ending.is_some() {
        bail!("review pass {pass_id} has more than one ending");
    }
    pass.ending = Some(ending);
    Ok(())
}

fn find_pass<'a>(history: &'a PassHistory, pass_id: &str) -> Result<&'a PassRecord> {
    ids::validate_id_component(pass_id)?;
    if let Some(pass) = history.passes.get(pass_id) {
        return Ok(pass);
    }
    if history.events.iter().any(|event| event.event_id == pass_id) {
        bail!("event {pass_id} is not a review pass");
    }
    bail!("no review pass has id {pass_id}");
}

fn refuse_ended(pass: &PassRecord) -> Result<()> {
    let Some(ending) = &pass.ending else {
        return Ok(());
    };
    match ending {
        PassEnding::Completed { event, .. } => bail!(
            "review pass {} already ended as completed (ending event {})",
            pass.pass_id,
            event.event_id
        ),
        PassEnding::Abandoned { event, reason, .. } => bail!(
            "review pass {} already ended as abandoned (ending event {}): {}",
            pass.pass_id,
            event.event_id,
            render::one_line(reason)
        ),
    }
}

fn validate_members(store: &Store, raw_members: Vec<String>) -> Result<Vec<String>> {
    if raw_members.is_empty() {
        bail!("pass open requires at least one --member");
    }
    let mut seen = BTreeSet::new();
    let mut members = Vec::with_capacity(raw_members.len());
    for raw in raw_members {
        let parts = parse_member(&raw)
            .with_context(|| format!("pass member {raw:?} is not <change-id>:<patchset-id>"))?;
        let change_id = store
            .resolve_change(&parts.change_id)
            .with_context(|| format!("pass member {raw:?}"))?;
        let state = store
            .state(&change_id)
            .with_context(|| format!("pass member {raw:?}"))?;
        if !state
            .patchsets
            .iter()
            .any(|patchset| patchset.id == parts.patchset_id)
        {
            bail!(
                "pass member {raw:?}: change {change_id} has no patchset {:?}",
                parts.patchset_id
            );
        }
        let member = format!("{change_id}:{}", parts.patchset_id);
        if !seen.insert(member.clone()) {
            bail!("pass member {raw:?} is repeated");
        }
        members.push(member);
    }
    Ok(members)
}

fn parse_member(member: &str) -> Result<MemberParts> {
    if member.trim() != member {
        bail!("member has surrounding whitespace");
    }
    let (change_id, patchset_id) = member
        .split_once(':')
        .context("expected one ':' between change and patchset")?;
    if patchset_id.contains(':') {
        bail!("member contains more than one ':'");
    }
    ids::validate_id_component(change_id)?;
    ids::validate_id_component(patchset_id)?;
    Ok(MemberParts {
        change_id: change_id.to_string(),
        patchset_id: patchset_id.to_string(),
    })
}

fn optional_text(value: Option<String>, flag: &str) -> Result<Option<String>> {
    value.map(|value| required_text(value, flag)).transpose()
}

fn required_text(value: String, flag: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{flag} must not be empty");
    }
    Ok(value.to_string())
}

fn pass_view(store: &Store, pass: &PassRecord) -> Result<PassView> {
    let members = pass
        .members
        .iter()
        .map(|member| member_view(store, member))
        .collect::<Result<Vec<_>>>()?;
    let mut view = PassView {
        pass_id: pass.pass_id.clone(),
        opened_event_id: pass.opened.event_id.clone(),
        opened_at: pass.opened.created_at,
        opened_by: pass.opened.actor.clone(),
        opened_actor_source: pass.opened.actor_source,
        opened_on_behalf_of: pass.opened.on_behalf_of.clone(),
        opened_harness: pass.opened.harness.clone(),
        opened_session: pass.opened.session.clone(),
        members,
        note: pass.note.clone(),
        state: "open".to_string(),
        ending_event_id: None,
        ending_at: None,
        ending_by: None,
        ending_actor_source: None,
        ending_on_behalf_of: None,
        ending_harness: None,
        ending_session: None,
        ending_note: None,
        reason: None,
    };
    match &pass.ending {
        None => {}
        Some(PassEnding::Completed { event, note }) => {
            view.state = "completed".to_string();
            view.ending_event_id = Some(event.event_id.clone());
            view.ending_at = Some(event.created_at);
            view.ending_by = Some(event.actor.clone());
            view.ending_actor_source = event.actor_source;
            view.ending_on_behalf_of = event.on_behalf_of.clone();
            view.ending_harness = event.harness.clone();
            view.ending_session = event.session.clone();
            view.ending_note = note.clone();
        }
        Some(PassEnding::Abandoned { event, reason }) => {
            view.state = "abandoned".to_string();
            view.ending_event_id = Some(event.event_id.clone());
            view.ending_at = Some(event.created_at);
            view.ending_by = Some(event.actor.clone());
            view.ending_actor_source = event.actor_source;
            view.ending_on_behalf_of = event.on_behalf_of.clone();
            view.ending_harness = event.harness.clone();
            view.ending_session = event.session.clone();
            view.reason = Some(reason.clone());
        }
    }
    Ok(view)
}

fn member_view(store: &Store, member: &str) -> Result<PassMemberView> {
    let parts = parse_member(member)?;
    let state = store.state(&parts.change_id)?;
    let verdict_event_id = state
        .verdicts
        .iter()
        .rev()
        .find(|verdict| verdict.patchset_id == parts.patchset_id)
        .map(|verdict| verdict.event_id.clone());
    Ok(PassMemberView {
        member: member.to_string(),
        change_id: parts.change_id,
        patchset_id: parts.patchset_id,
        covered: verdict_event_id.is_some(),
        verdict_event_id,
    })
}

fn render_pass(pass: &PassRecord, view: &PassView) {
    println!(
        "pass {} [{}] (caller-declared review pass)",
        view.pass_id, view.state
    );
    println!("  {}", render::event_line(&pass.opened));
    println!("  opened event: {}", view.opened_event_id);
    if let Some(note) = &view.note {
        println!("  note: {}", render::one_line(note));
    }
    for member in &view.members {
        match &member.verdict_event_id {
            Some(event_id) => {
                println!("  {} — covered (verdict event {})", member.member, event_id)
            }
            None => println!("  {} — not covered", member.member),
        }
    }
    match &pass.ending {
        None => {}
        Some(PassEnding::Completed { event, note }) => {
            println!("  {}", render::event_line(event));
            println!("  ending event: {}", event.event_id);
            if let Some(note) = note {
                println!("  ending note: {}", render::one_line(note));
            }
        }
        Some(PassEnding::Abandoned { event, reason }) => {
            println!("  {}", render::event_line(event));
            println!("  ending event: {}", event.event_id);
            println!("  reason: {}", render::one_line(reason));
        }
    }
}
