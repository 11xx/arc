use super::*;

/// Record a forge projection declaration. Allowed for any profile: a local
/// change may later be projected. Latest declaration supersedes, history
/// stays append-only.
#[allow(clippy::too_many_arguments)]
pub fn forge_declare(
    ctx: &Ctx,
    reference: &str,
    host: String,
    base_repo: String,
    base_ref: String,
    head_repo: String,
    head_ref: String,
    policy: String,
) -> Result<()> {
    let policy = crate::forge::ForgePolicy::parse(&policy)?;
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;
    let payload = Payload::ForgeProjection {
        host,
        base_repo,
        base_ref,
        head_repo,
        head_ref,
        policy,
    };
    crate::forge_observation::validate_projection(&payload, &st.forge)?;
    ensure_append_allowed(&st, &payload)?;
    let mut event = ctx.event(&store, &change_id, payload)?;
    store.append_local_event(&mut event)?;
    println!("forge projection declared: {change_id}");
    println!("event: {}", event.event_id);
    Ok(())
}

use crate::forge_observation::{ForgeObservation, ItemFacts, ObservationRecord};

fn observation<'a>(
    st: &'a ChangeState,
    named: Option<&str>,
) -> Result<Option<&'a ObservationRecord>> {
    let Some(named) = named else {
        return Ok(None);
    };
    if named.trim().is_empty() {
        bail!("empty observation reference");
    }
    let exact: Vec<_> = st
        .forge
        .observations
        .iter()
        .filter(|r| r.document.read_id == named || r.names_event(named))
        .collect();
    match exact.as_slice() {
        [r] if r.usable() => return Ok(Some(*r)),
        [..] if exact.len() == 1 => bail!("observation is conflicting or invalid"),
        [] => {}
        _ => bail!("observation reference is ambiguous between read and event IDs"),
    }
    let matches: Vec<_> = st
        .forge
        .observations
        .iter()
        .filter(|r| r.names_prefix(named))
        .collect();
    match matches.as_slice() {
        [r] if r.usable() => Ok(Some(*r)),
        [_] => bail!("observation is conflicting or invalid"),
        _ => bail!("observation reference must name exactly one recorded read"),
    }
}
fn value<T: Clone + PartialEq>(explicit: Option<T>, observed: Option<T>, name: &str) -> Result<T> {
    if let (Some(a), Some(b)) = (&explicit, &observed) {
        if a != b {
            bail!("explicit {name} disagrees with observation");
        }
    }
    explicit
        .or(observed)
        .ok_or_else(|| anyhow::anyhow!("{name} is required without a usable observation"))
}
fn current_link<'a>(
    st: &'a ChangeState,
    named: Option<&str>,
    obs: Option<&ObservationRecord>,
) -> Result<&'a crate::forge::ForgeLinkRecord> {
    let current = st
        .forge
        .link
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no forge link recorded"))?;
    if let Some(named) = named {
        if named.trim().is_empty() {
            bail!("empty link reference");
        }
        let matches: Vec<_> = st
            .forge
            .links
            .iter()
            .filter(|r| r.event_id.starts_with(named))
            .collect();
        match matches.as_slice() {
            [r] if r.event_id == current.event_id => {}
            _ => bail!("link reference must name the current link unambiguously"),
        }
    }
    if let Some(obs) = obs {
        crate::forge_observation::validate_current_identity(&obs.document, &st.forge)?;
    }
    Ok(current)
}
fn receipt(
    command: &str,
    change: &str,
    event_id: &str,
    obs: Option<&ObservationRecord>,
    outcome: &str,
    json: bool,
) -> Result<()> {
    let doc = obs.map(|o| &o.document);
    let result = serde_json::json!({
        "schema":"arc-forge-receipt/1", "command":command, "change":change,
        "event_ids":[event_id], "observation_id":obs.map(|o| &o.event_id),
        "read_id":doc.map(|d| &d.read_id), "generation_ids":doc.map(|d| d.generations.iter().map(|g| &g.generation_id).collect::<Vec<_>>()).unwrap_or_default(),
        "outcome":outcome, "coverage":doc.map(|d| &d.coverage),
        "availability":doc.map(|d| d.responses.iter().map(|r| &r.content).chain(d.items.iter().map(|i| &i.content)).map(|r| serde_json::json!({"provider":r.provider,"handle":r.handle,"sha256":r.sha256,"byte_length":r.byte_length,"reported_capture":r.capture,"availability":"unmaterialized"})).collect::<Vec<_>>()).unwrap_or_default()
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("forge {command} {outcome}: {change}");
        println!("event: {event_id}");
        if let Some(d) = doc {
            println!("read: {} observation: {}", d.read_id, obs.unwrap().event_id);
            println!(
                "coverage: {:?}; generations: {}; references: unmaterialized",
                d.coverage.overall.state,
                d.generations.len()
            );
        } else {
            println!("unretained assertion; acquisition time: unknown");
        }
    }
    Ok(())
}

pub fn forge_observe(ctx: &Ctx, reference: &str, file: &str, json: bool) -> Result<()> {
    let document: ForgeObservation = serde_json::from_slice(&std::fs::read(file)?)?;
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;
    if let Some(original) = st
        .forge
        .observations
        .iter()
        .find(|r| r.document.read_id == document.read_id)
    {
        if !original.usable() {
            bail!("read_id is conflicting or invalid");
        }
        if original.document != document {
            bail!("read_id conflicts with recorded observation");
        }
        return receipt(
            "observe",
            &change_id,
            &original.event_id,
            Some(original),
            "already-recorded",
            json,
        );
    }
    document.validate(&st.forge.observations)?;
    let payload = Payload::ForgeObservationRecorded {
        observation: document.clone(),
    };
    crate::forge_observation::validate_projection(&payload, &st.forge)?;
    ensure_append_allowed(&st, &payload)?;
    let mut event = ctx.event(&store, &change_id, payload)?;
    store.append_local_event(&mut event)?;
    let obs = ObservationRecord {
        event_id: event.event_id.clone(),
        created_at: event.created_at,
        document,
        aliases: Vec::new(),
        conflict: false,
        invalid: None,
    };
    receipt(
        "observe",
        &change_id,
        &event.event_id,
        Some(&obs),
        "recorded",
        json,
    )
}

pub fn forge_link(ctx: &Ctx, reference: &str, args: ForgeLinkArgs) -> Result<i32> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;
    let obs = observation(&st, args.observation.as_deref())?;
    let p = obs.map(|r| &r.document.provider_identity);
    let tuple = crate::forge::ForgeTuple {
        base_repo: value(args.base_repo, p.map(|p| p.base_repo.clone()), "base-repo")?,
        base_ref: value(args.base_ref, p.map(|p| p.base_ref.clone()), "base-ref")?,
        head_repo: value(args.head_repo, p.map(|p| p.head_repo.clone()), "head-repo")?,
        head_ref: value(args.head_ref, p.map(|p| p.head_ref.clone()), "head-ref")?,
    };
    if let Err(refusal) = crate::forge::validate_link(st.forge.projection.as_ref(), &tuple) {
        eprintln!("forge link refused: {}", refusal.message());
        return Ok(10);
    }
    if let Some(p) = p {
        if st
            .forge
            .projection
            .as_ref()
            .is_none_or(|d| d.host != p.host)
        {
            eprintln!("forge link refused: observation host differs from declared host");
            return Ok(10);
        }
    }
    let head = obs.and_then(|o| o.document.observed_head_sha.clone());
    if obs.is_some() && head.is_none() {
        bail!("observation has no raw head");
    }
    let payload = Payload::ForgeLink {
        observation: obs.map(|r| r.event_id.clone()),
        pr_number: value(args.pr_number, p.map(|p| p.pr_number), "pr")?,
        url: value(args.url, p.map(|p| p.url.clone()), "url")?,
        base_repo: tuple.base_repo,
        base_ref: tuple.base_ref,
        head_repo: tuple.head_repo,
        head_ref: tuple.head_ref,
        head_sha: value(args.head_sha, head, "head-sha")?,
    };
    crate::forge_observation::validate_projection(&payload, &st.forge)?;
    ensure_append_allowed(&st, &payload)?;
    let mut event = ctx.event(&store, &change_id, payload)?;
    store.append_local_event(&mut event)?;
    receipt(
        "link",
        &change_id,
        &event.event_id,
        obs,
        "recorded",
        args.json,
    )?;
    Ok(0)
}

pub fn forge_checks(ctx: &Ctx, reference: &str, args: ForgeChecksArgs) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;
    let obs = observation(&st, args.observation.as_deref())?;
    if args.link.is_some() || obs.is_some() {
        current_link(&st, args.link.as_deref(), obs)?;
    }
    let observed = obs
        .map(|r| {
            let facts: Vec<_> = r
                .document
                .items
                .iter()
                .filter_map(|i| {
                    if let ItemFacts::Checks { state, .. } = i.facts {
                        Some(state)
                    } else {
                        None
                    }
                })
                .collect();
            match facts.as_slice() {
                [state] => Ok(*state),
                _ => bail!("observation must carry exactly one check rollup"),
            }
        })
        .transpose()?;
    if obs.is_some() && args.detail.is_some() {
        bail!("--detail has no typed observed value to agree with");
    }
    let pr_head = value(
        args.pr_head,
        obs.and_then(|r| r.document.observed_head_sha.clone()),
        "pr-head",
    )?;
    let payload = Payload::ForgeChecks {
        binding_recorded: true,
        link_event_id: st.forge.link.as_ref().map(|link| link.event_id.clone()),
        observation: obs.map(|r| r.event_id.clone()),
        pr_head,
        state: value(args.state, observed, "state")?,
        detail: args.detail,
    };
    crate::forge_observation::validate_projection(&payload, &st.forge)?;
    ensure_append_allowed(&st, &payload)?;
    let mut event = ctx.event(&store, &change_id, payload)?;
    store.append_local_event(&mut event)?;
    receipt(
        "checks",
        &change_id,
        &event.event_id,
        obs,
        "recorded",
        args.json,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn forge_pr_state(
    ctx: &Ctx,
    reference: &str,
    state: Option<crate::forge::ForgePrState>,
    merge_sha: Option<String>,
    link: Option<String>,
    named: Option<String>,
    json: bool,
) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;
    let obs = observation(&st, named.as_deref())?;
    let current = current_link(&st, link.as_deref(), obs)?;
    let fact = obs
        .map(|r| {
            let facts: Vec<_> = r
                .document
                .items
                .iter()
                .filter_map(|i| {
                    if let ItemFacts::PullRequest { state, merge_sha } = &i.facts {
                        Some((*state, merge_sha.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            match facts.as_slice() {
                [fact] => Ok(fact.clone()),
                _ => bail!("observation must carry exactly one PR lifecycle fact"),
            }
        })
        .transpose()?;
    let state = value(state, fact.as_ref().map(|f| f.0), "state")?;
    let merge_sha = if let Some((_, observed)) = fact {
        if merge_sha.is_some() && merge_sha != observed {
            bail!("explicit merge-sha disagrees with observation");
        }
        observed
    } else {
        merge_sha
    };
    if (state == crate::forge::ForgePrState::Merged) != merge_sha.is_some() {
        bail!("merged requires --merge-sha; merge-sha is only valid with state merged");
    }
    let payload = Payload::ForgePrState {
        observation: obs.map(|r| r.event_id.clone()),
        state,
        merge_sha,
        link_event_id: Some(current.event_id.clone()),
        pr_head: Some(current.observed_head_sha.clone()),
    };
    crate::forge_observation::validate_projection(&payload, &st.forge)?;
    ensure_append_allowed(&st, &payload)?;
    let mut event = ctx.event(&store, &change_id, payload)?;
    store.append_local_event(&mut event)?;
    receipt(
        "pr-state",
        &change_id,
        &event.event_id,
        obs,
        "recorded",
        json,
    )
}
