use super::*;
use crate::gate_suspension::{self, CEILING_MS};
use chrono::{DateTime, Duration as Span, Utc};

/// Record a bounded suspension of budget judgement on this ledger.
pub fn suspend_budgets(
    ctx: &Ctx,
    duration: Option<&str>,
    until: Option<&str>,
    reason: &str,
    mut gates: Vec<String>,
) -> Result<i32> {
    let store = ctx.store()?;
    ctx.ensure_declared_actor(&store)?;
    let reason = reason.trim();
    if reason.is_empty() {
        bail!("a budget suspension needs a reason");
    }
    let now = Utc::now();
    let until = match (duration, until) {
        (Some(raw), None) => {
            let ms = parse_duration_ms(raw)?;
            i64::try_from(ms)
                .ok()
                .and_then(|ms| now.checked_add_signed(Span::milliseconds(ms)))
                .context("suspension duration overflow")?
        }
        (None, Some(raw)) => DateTime::parse_from_rfc3339(raw)
            .with_context(|| format!("--until {raw:?} is not an RFC 3339 instant"))?
            .with_timezone(&Utc),
        _ => bail!("a budget suspension needs its end: --for DURATION or --until RFC3339"),
    };
    if until <= now {
        bail!("a budget suspension ends in the future");
    }
    if (until - now).num_milliseconds() > CEILING_MS as i64 {
        bail!(
            "a budget suspension lasts at most {}; record another when this one ends",
            crate::gate_budget::duration(CEILING_MS)
        );
    }
    gates.sort();
    gates.dedup();
    if gates.iter().any(|gate| gate.trim().is_empty()) {
        bail!("a suspended gate needs a name");
    }
    if let Ok(declared) = gitio::toplevel(&ctx.cwd).and_then(|top| gates::inspect(&top)) {
        for gate in gates
            .iter()
            .filter(|gate| !declared.gates.contains_key(*gate))
        {
            eprintln!("warning: gate {gate:?} is not declared in this checkout");
        }
    }
    let _repository_events = store.lock_repository_events()?;
    let mut event = ctx.event_at(
        &store,
        Store::REPOSITORY_SCOPE,
        now,
        Payload::GateBudgetsSuspended {
            until,
            reason: reason.to_string(),
            gates,
        },
    )?;
    store.append_local_repository_event(&mut event)?;
    let suspension = gate_suspension::recorded(std::slice::from_ref(&event), &store.repository_id)
        .pop()
        .context("the recorded suspension is not in force")?;
    println!("{}", suspension.describe());
    println!(
        "a passing run over its budget is accepted for integration until then; a failing gate \
         still fails and publication stays strict; end it early with `arc gates resume`"
    );
    Ok(0)
}

/// End every suspension in force on this ledger.
pub fn resume_budgets(ctx: &Ctx) -> Result<i32> {
    let store = ctx.store()?;
    ctx.ensure_declared_actor(&store)?;
    let _repository_events = store.lock_repository_events()?;
    let in_force = gate_suspension::in_force_on(&store)?;
    if in_force.is_empty() {
        println!("no budget suspension is in force");
        return Ok(0);
    }
    let mut event = ctx.event(
        &store,
        Store::REPOSITORY_SCOPE,
        Payload::GateBudgetsResumed {
            suspension_event_ids: in_force
                .iter()
                .map(|suspension| suspension.event_id.clone())
                .collect(),
        },
    )?;
    store.append_local_repository_event(&mut event)?;
    for suspension in &in_force {
        println!(
            "budget suspension {} ended ({})",
            suspension.event_id,
            suspension.scope()
        );
    }
    Ok(0)
}
