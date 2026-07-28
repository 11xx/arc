//! Advisory executor claims and stages remain separate ledger events.
//! `stage --claim` only composes their acquisition and progress intent.

use super::*;
use crate::state::ClaimIdentity;

pub(crate) fn ready_candidate<'a>(
    states: &'a BTreeMap<String, ChangeState>,
    tags: &[String],
    now: chrono::DateTime<chrono::Utc>,
) -> Option<&'a ChangeState> {
    let mut candidates = states
        .values()
        .filter(|candidate| {
            !candidate.is_closed()
                && candidate.hold.is_none()
                && tags.iter().all(|tag| candidate.tags.contains(tag))
                && !dependency_status(candidate, states).blocked
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
    candidates.first().copied()
}

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
    let Some(candidate) = ready_candidate(&states, &tags, now) else {
        return Ok(2);
    };
    let previous_event_id = store
        .load_events(&candidate.change_id)?
        .last()
        .context("change has no opening event")?
        .event_id
        .clone();
    let displaced = candidate.claim.as_ref().map(|claim| {
        let timing = state::claim_timing_at(claim, now);
        displaced_claim(claim, &timing)
    });
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
            displaced,
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
    let (code, _) = claim_inner(
        ctx,
        reference,
        ttl,
        stage_budgets,
        ClaimMode::Standard { takeover },
    )?;
    Ok(code)
}

pub(crate) fn takeover_abandoned(
    ctx: &Ctx,
    reference: &str,
) -> Result<(i32, Option<ClaimIdentity>)> {
    claim_inner(
        ctx,
        reference,
        None,
        Vec::new(),
        ClaimMode::RequireAbandoned,
    )
}

#[derive(Clone, Copy)]
enum ClaimMode {
    Standard { takeover: bool },
    RequireAbandoned,
}

fn claim_inner(
    ctx: &Ctx,
    reference: &str,
    ttl: Option<String>,
    stage_budgets: Vec<String>,
    mode: ClaimMode,
) -> Result<(i32, Option<ClaimIdentity>)> {
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
    let mut previous_owner = None;
    let displaced = if let Some(existing) = &state.claim {
        let timing = state::claim_timing_at(existing, now);
        if timing.active && existing.owner != owner {
            if !timing.stale {
                print_claim_conflict("claim is already held", existing, &timing);
                eprintln!("--takeover is unavailable because the claim is not yet stale");
                return Ok((8, None));
            }
            if matches!(mode, ClaimMode::Standard { takeover: false }) {
                print_claim_conflict("claim is already held", existing, &timing);
                eprintln!("--takeover would displace this stale claim");
                return Ok((8, None));
            }
            previous_owner = Some(existing.owner.clone());
            Some(DisplacedClaim {
                claim_id: existing.claim_id.clone(),
                actor: existing.owner.actor.clone(),
                harness: existing.owner.harness.clone(),
                session: existing.owner.session.clone(),
                stage: timing.stage,
            })
        } else if timing.expired
            && (matches!(mode, ClaimMode::Standard { .. }) || existing.owner != owner)
        {
            previous_owner = Some(existing.owner.clone());
            Some(displaced_claim(existing, &timing))
        } else if matches!(mode, ClaimMode::RequireAbandoned) {
            eprintln!(
                "rescue --take requires a claim owned by another identity that is stale or expired"
            );
            return Ok((8, None));
        } else {
            None
        }
    } else if matches!(mode, ClaimMode::RequireAbandoned) {
        eprintln!(
            "rescue --take requires a claim owned by another identity that is stale or expired"
        );
        return Ok((8, None));
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

    let payload = Payload::ClaimSet {
        claim_id,
        ttl_seconds,
        stage_budgets: budgets,
        displaced: displaced.clone(),
    };
    ensure_append_allowed(&state, &payload)?;
    let event = identity_event_at(ctx, &store, &change_id, now, &owner, payload);
    store.append_event(&event)?;
    if matches!(mode, ClaimMode::Standard { .. }) {
        if let Some(displaced) = &displaced {
            println!(
                "displaced: owner={} harness={} session={} stage={}",
                displaced.actor, displaced.harness, displaced.session, displaced.stage
            );
        }
        println!("claimed: {change_id} for {ttl_seconds}s");
        println!("event: {}", event.event_id);
    }
    Ok((0, previous_owner))
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
    blocker: Option<String>,
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
    if stage == ClaimStage::BlockedOn && blocker.is_none() {
        bail!("blocked-on requires --blocker");
    }
    if stage != ClaimStage::BlockedOn && blocker.is_some() {
        bail!("--blocker is only valid with blocked-on");
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
    let blocker = blocker
        .as_deref()
        .map(|raw| resolve_blocker(&store, &state, raw))
        .transpose()?;
    let now = chrono::Utc::now();
    let has_owned_live_claim = state
        .claim
        .as_ref()
        .is_some_and(|claim| claim.owner == owner && state::claim_timing_at(claim, now).active);
    if claim_if_needed && !has_owned_live_claim {
        let displaced = if let Some(existing) = &state.claim {
            let timing = state::claim_timing_at(existing, now);
            if timing.active && existing.owner != owner {
                print_claim_conflict("claim is already held", existing, &timing);
                return Ok(8);
            }
            Some(displaced_claim(existing, &timing))
        } else {
            None
        };
        let claim_id = ids::new_event_id();
        let payload = Payload::ClaimSet {
            claim_id,
            ttl_seconds: 2 * 60 * 60,
            stage_budgets: default_stage_budgets(),
            displaced,
        };
        ensure_append_allowed(&state, &payload)?;
        let mut claim_event = identity_event_at(ctx, &store, &change_id, now, &owner, payload);
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

    let payload = Payload::StageSet {
        claim_id: existing.claim_id.clone(),
        stage,
        note,
        blocker,
    };
    ensure_append_allowed(&state, &payload)?;
    let mut event = identity_event_at(ctx, &store, &change_id, now, &owner, payload);
    event.event_id = event_id_after(&previous_event_id)?;
    store.append_event(&event)?;
    println!("stage: {}", stage.as_str());
    println!("event: {}", event.event_id);
    Ok(0)
}

fn resolve_blocker(store: &Store, state: &ChangeState, raw: &str) -> Result<BlockerRef> {
    if raw == "external" {
        return Ok(BlockerRef::External);
    }
    let (kind, reference) = raw
        .split_once(':')
        .context("invalid --blocker; expected brief:vN, finding:ID, change:ID, or external")?;
    if reference.is_empty() {
        bail!("blocker reference cannot be empty");
    }
    match kind {
        "brief" => {
            let version = reference
                .strip_prefix('v')
                .context("brief blocker must use brief:vN")?
                .parse::<usize>()
                .context("brief blocker version must be a positive integer")?;
            if version == 0 {
                bail!("brief blocker version must be a positive integer");
            }
            let brief = state
                .briefs
                .get(version - 1)
                .with_context(|| format!("brief v{version} does not exist"))?;
            Ok(BlockerRef::Brief {
                brief_event_id: brief.event_id.clone(),
            })
        }
        "finding" => Ok(BlockerRef::Finding {
            finding_id: state.resolve_finding_id(reference)?,
        }),
        "change" => Ok(BlockerRef::Change {
            change_id: store.resolve_change(reference)?,
        }),
        _ => bail!("unknown blocker kind {kind:?}; expected brief, finding, change, or external"),
    }
}

pub(super) fn owns_live_claim(ctx: &Ctx, reference: &str) -> Result<bool> {
    let owner = command_identity(ctx)?;
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    Ok(state.claim.as_ref().is_some_and(|claim| {
        claim.owner == owner && state::claim_timing_at(claim, chrono::Utc::now()).active
    }))
}

fn displaced_claim(existing: &state::ClaimState, timing: &state::ClaimTiming) -> DisplacedClaim {
    DisplacedClaim {
        claim_id: existing.claim_id.clone(),
        actor: existing.owner.actor.clone(),
        harness: existing.owner.harness.clone(),
        session: existing.owner.session.clone(),
        stage: timing.stage.clone(),
    }
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
        "blocked-on" => StageBudget::BlockedOn,
        "snapshotted" => StageBudget::Snapshotted,
        _ => bail!(
            "unknown stage budget {name:?}; expected launch, started, spec-read, implementing, verifying, blocked-on, or snapshotted"
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
        (StageBudget::BlockedOn, 15 * 60),
        (StageBudget::Snapshotted, 60 * 60),
    ]
    .into_iter()
    .collect()
}
