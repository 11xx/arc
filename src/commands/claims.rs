//! Advisory executor claims and stages remain separate ledger events.
//! `stage --claim` only composes their acquisition and progress intent.

use super::*;

/// Select and claim one ready change while holding the repository graph lock.
/// Competing `take` calls therefore cannot observe and claim the same winner.
pub fn take(ctx: &Ctx, tags: Vec<String>, ttl: Option<String>, json: bool) -> Result<i32> {
    let owner = command_identity(ctx)?;
    let tags = normalize_tags(tags)?;
    let ttl_seconds = ttl
        .as_deref()
        .map(parse_duration)
        .transpose()?
        .unwrap_or(2 * 60 * 60);
    let store = ctx.store()?;
    let _graph = store.lock_graph()?;
    let states = ctx.load_all_states(&store)?;
    let now = chrono::Utc::now();
    let mut candidates = states
        .values()
        .filter(|candidate| {
            !candidate.is_closed()
                && candidate.hold.is_none()
                && tags.iter().all(|tag| candidate.tags.contains(tag))
                && !dependency_status(candidate, &states).blocked
                && candidate.claim.as_ref().is_none_or(|claim| {
                    let timing = state::claim_timing_at(claim, now);
                    !timing.active || timing.stale
                })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.opened_at.cmp(&b.opened_at))
    });
    let Some(candidate) = candidates.first() else {
        return Ok(2);
    };
    let mut previous_event_id = store
        .load_events(&candidate.change_id)?
        .last()
        .context("change has no opening event")?
        .event_id
        .clone();
    if let Some(claim) = candidate.claim.as_ref().filter(|claim| {
        let timing = state::claim_timing_at(claim, now);
        timing.active && timing.stale
    }) {
        let mut release = identity_event_at(
            ctx,
            &store,
            &candidate.change_id,
            now,
            &owner,
            Payload::ClaimReleased {
                claim_id: claim.claim_id.clone(),
            },
        );
        release.event_id = event_id_after(&previous_event_id)?;
        store.append_event(&release)?;
        previous_event_id = release.event_id;
    }
    let mut event = identity_event_at(
        ctx,
        &store,
        &candidate.change_id,
        now,
        &owner,
        Payload::ClaimSet {
            claim_id: ids::new_event_id(),
            ttl_seconds,
            stage_budgets: default_stage_budgets(),
            displaced: None,
        },
    );
    event.event_id = event_id_after(&previous_event_id)?;
    store.append_event(&event)?;
    if json {
        let state = state::reduce(&store.load_events(&candidate.change_id)?)?;
        let output = status_output(ctx, &store, &state)?;
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", candidate.change_id);
    }
    Ok(0)
}

pub fn claim(
    ctx: &Ctx,
    reference: &str,
    ttl: Option<String>,
    stage_budgets: Vec<String>,
    takeover: bool,
) -> Result<i32> {
    let owner = command_identity(ctx)?;
    let ttl_seconds = ttl
        .as_deref()
        .map(parse_duration)
        .transpose()?
        .unwrap_or(2 * 60 * 60);
    let mut budgets = default_stage_budgets();
    for raw in stage_budgets {
        let (key, seconds) = parse_stage_budget(&raw)?;
        budgets.insert(key, seconds);
    }

    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let state = state::reduce(&store.load_events(&change_id)?)?;
    let now = chrono::Utc::now();
    if state.is_closed() {
        bail!("change {change_id} is closed");
    }
    let displaced = if let Some(existing) = &state.claim {
        let timing = state::claim_timing_at(existing, now);
        if timing.active && existing.owner != owner {
            if !timing.stale {
                print_claim_conflict("claim is already held", existing, &timing);
                eprintln!("--takeover is unavailable because the claim is not yet stale");
                return Ok(8);
            }
            if !takeover {
                print_claim_conflict("claim is already held", existing, &timing);
                eprintln!("--takeover would displace this stale claim");
                return Ok(8);
            }
            Some(DisplacedClaim {
                claim_id: existing.claim_id.clone(),
                actor: existing.owner.actor.clone(),
                harness: existing.owner.harness.clone(),
                session: existing.owner.session.clone(),
                stage: timing.stage,
            })
        } else {
            None
        }
    } else {
        None
    };
    let claim_id = state
        .claim
        .as_ref()
        .filter(|claim| {
            displaced.is_none() && claim.owner == owner && state::claim_timing_at(claim, now).active
        })
        .map(|claim| claim.claim_id.clone())
        .unwrap_or_else(ids::new_event_id);

    let event = identity_event_at(
        ctx,
        &store,
        &change_id,
        now,
        &owner,
        Payload::ClaimSet {
            claim_id,
            ttl_seconds,
            stage_budgets: budgets,
            displaced: displaced.clone(),
        },
    );
    store.append_event(&event)?;
    if let Some(displaced) = displaced {
        println!(
            "displaced: owner={} harness={} session={} stage={}",
            displaced.actor, displaced.harness, displaced.session, displaced.stage
        );
    }
    println!("claimed: {change_id} for {ttl_seconds}s");
    println!("event: {}", event.event_id);
    Ok(0)
}

pub fn release_claim(ctx: &Ctx, reference: &str) -> Result<i32> {
    let caller = command_identity(ctx)?;
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let state = state::reduce(&store.load_events(&change_id)?)?;
    let now = chrono::Utc::now();
    let Some(existing) = &state.claim else {
        eprintln!("claim conflict: {change_id} has no live claim");
        return Ok(8);
    };
    let timing = state::claim_timing_at(existing, now);
    if !timing.active {
        print_claim_conflict("claim is expired", existing, &timing);
        return Ok(8);
    }
    let event = identity_event_at(
        ctx,
        &store,
        &change_id,
        now,
        &caller,
        Payload::ClaimReleased {
            claim_id: existing.claim_id.clone(),
        },
    );
    store.append_event(&event)?;
    println!("claim released: {change_id}");
    println!("event: {}", event.event_id);
    Ok(0)
}

pub fn stage(
    ctx: &Ctx,
    reference: &str,
    stage: StageArg,
    note: Option<String>,
    claim_if_needed: bool,
) -> Result<i32> {
    let owner = command_identity(ctx)?;
    let stage = ClaimStage::from(stage);
    let note = note.and_then(|note| {
        let trimmed = note.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    });
    if stage == ClaimStage::BlockedOn && note.is_none() {
        bail!("blocked-on requires a nonempty --note");
    }

    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let events = store.load_events(&change_id)?;
    let mut state = state::reduce(&events)?;
    let mut previous_event_id = events
        .last()
        .context("change has no opening event")?
        .event_id
        .clone();
    if state.is_closed() {
        bail!("change {change_id} is closed");
    }
    let now = chrono::Utc::now();
    let has_owned_live_claim = state
        .claim
        .as_ref()
        .is_some_and(|claim| claim.owner == owner && state::claim_timing_at(claim, now).active);
    if claim_if_needed && !has_owned_live_claim {
        if let Some(existing) = &state.claim {
            let timing = state::claim_timing_at(existing, now);
            if timing.active && existing.owner != owner {
                print_claim_conflict("claim is already held", existing, &timing);
                return Ok(8);
            }
        }
        let claim_id = ids::new_event_id();
        let mut claim_event = identity_event_at(
            ctx,
            &store,
            &change_id,
            now,
            &owner,
            Payload::ClaimSet {
                claim_id,
                ttl_seconds: 2 * 60 * 60,
                stage_budgets: default_stage_budgets(),
                displaced: None,
            },
        );
        claim_event.event_id = event_id_after(&previous_event_id)?;
        store.append_event(&claim_event)?;
        previous_event_id = claim_event.event_id.clone();
        println!("claimed: {change_id} for 7200s");
        println!("event: {}", claim_event.event_id);
        state = state::reduce(&store.load_events(&change_id)?)?;
    }
    let Some(existing) = &state.claim else {
        eprintln!(
            "claim conflict: {change_id} has no claim for stage {}",
            stage.as_str()
        );
        return Ok(8);
    };
    let timing = state::claim_timing_at(existing, now);
    if !timing.active {
        print_claim_conflict("claim is expired", existing, &timing);
        return Ok(8);
    }
    if existing.owner != owner {
        print_claim_conflict("stage caller does not own claim", existing, &timing);
        return Ok(8);
    }

    let mut event = identity_event_at(
        ctx,
        &store,
        &change_id,
        now,
        &owner,
        Payload::StageSet {
            claim_id: existing.claim_id.clone(),
            stage,
            note,
        },
    );
    event.event_id = event_id_after(&previous_event_id)?;
    store.append_event(&event)?;
    println!("stage: {}", stage.as_str());
    println!("event: {}", event.event_id);
    Ok(0)
}

pub(super) fn owns_live_claim(ctx: &Ctx, reference: &str) -> Result<bool> {
    let owner = command_identity(ctx)?;
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    Ok(state.claim.as_ref().is_some_and(|claim| {
        claim.owner == owner && state::claim_timing_at(claim, chrono::Utc::now()).active
    }))
}

fn command_identity(ctx: &Ctx) -> Result<state::ClaimIdentity> {
    let actor = ctx.actor.trim();
    if actor.is_empty() {
        bail!("claim/stage commands require a nonempty actor");
    }
    let harness = ctx
        .harness
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("claim/stage commands require nonempty ARC_HARNESS or --harness")?;
    let session = ctx
        .session
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("claim/stage commands require nonempty ARC_SESSION or --session")?;
    Ok(state::ClaimIdentity {
        actor: actor.to_string(),
        harness: harness.to_string(),
        session: session.to_string(),
    })
}

fn identity_event_at(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    created_at: chrono::DateTime<chrono::Utc>,
    identity: &state::ClaimIdentity,
    payload: Payload,
) -> Event {
    let mut event = ctx.event_at(store, change_id, created_at, payload);
    event.actor = identity.actor.clone();
    event.harness = Some(identity.harness.clone());
    event.session = Some(identity.session.clone());
    event
}

fn print_claim_conflict(message: &str, claim: &state::ClaimState, timing: &state::ClaimTiming) {
    eprintln!(
        "claim conflict: {message}; owner={} harness={} session={} stage={} expires={}",
        claim.owner.actor,
        claim.owner.harness,
        claim.owner.session,
        timing.stage,
        timing.expires_at
    );
}

fn parse_stage_budget(raw: &str) -> Result<(StageBudget, u64)> {
    let (name, duration) = raw
        .split_once('=')
        .with_context(|| format!("invalid stage budget {raw:?}; expected <name>=<duration>"))?;
    let key = match name {
        "launch" => StageBudget::Launch,
        "started" => StageBudget::Started,
        "spec-read" => StageBudget::SpecRead,
        "implementing" => StageBudget::Implementing,
        "verifying" => StageBudget::Verifying,
        _ => bail!(
            "unknown stage budget {name:?}; expected launch, started, spec-read, implementing, or verifying"
        ),
    };
    Ok((key, parse_duration(duration)?))
}

fn default_stage_budgets() -> BTreeMap<StageBudget, u64> {
    [
        (StageBudget::Launch, 60),
        (StageBudget::Started, 5 * 60),
        (StageBudget::SpecRead, 2 * 60),
        (StageBudget::Implementing, 30 * 60),
        (StageBudget::Verifying, 15 * 60),
    ]
    .into_iter()
    .collect()
}
