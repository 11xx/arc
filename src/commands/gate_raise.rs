use super::*;

pub fn raise_budget(
    ctx: &Ctx,
    reference: &str,
    gate: Option<String>,
    total: bool,
    reason: &str,
) -> Result<i32> {
    let store = ctx.store()?;
    ctx.ensure_declared_actor(&store)?;
    let (id, _guard, state) = locked_state(&store, reference)?;
    let head = gitio::branch_head(&ctx.cwd, &state.branch)?;
    let target = gitio::branch_head(&ctx.cwd, &state.target_branch)?;
    if gitio::head(&ctx.cwd)? != head || !gitio::is_clean(&ctx.cwd)? {
        bail!("budget raise requires the clean change checkout at its head");
    }
    let mut subjects = gate.map(Some).into_iter().collect::<Vec<_>>();
    if total {
        subjects.push(None);
    }
    let raise = crate::gate_raise::propose(&ctx.cwd, &state, &target, &head, &subjects, reason)?;
    let payload = Payload::GateBudgetRaised {
        raise: raise.clone(),
    };
    ensure_append_allowed(&state, &payload)?;
    let mut target_gates = gates::load_at(&ctx.cwd, &target)?;
    target_gates.owe_also(gates::load_at(&ctx.cwd, &head)?, "destination", "head");
    for name in subjects.iter().flatten() {
        if let Some(gate) = target_gates.gates.get(name) {
            if let Some(probe) = &gate.environment {
                let observed = gates::environment_probe(&ctx.cwd, probe, gate.timeout)?;
                let entry = state.verifications.iter().rev().find(|e| {
                    state.direct_executions.contains_key(&e.event_id)
                        && e.gate.as_ref() == Some(name)
                        && e.recorded_revision == head
                });
                if observed.identity.is_none()
                    || entry
                        .and_then(|e| e.environment.as_ref())
                        .map(|e| &e.identity)
                        != observed.identity.as_ref()
                {
                    bail!("{name}: execution environment does not match");
                }
            }
        }
    }
    if let Some(record) = state.budget_raises.iter().find(|r| r.raise == raise) {
        println!("budget raise: {} (unchanged)", record.event_id);
        return Ok(0);
    }
    let mut proposed = state.clone();
    proposed.budget_raises.push(crate::gate_raise::Record {
        event_id: "proposal".into(),
        raise: raise.clone(),
    });
    let mut declared = gates::load_at(&ctx.cwd, &target)?;
    crate::gate_raise::apply(&ctx.cwd, &proposed, &target, &head, &mut declared)?;
    if let Some(invalid) = declared
        .raises
        .iter()
        .find(|entry| entry.record.event_id == "proposal" && !entry.applicable)
    {
        bail!(
            "budget raise basis changed while counting: {}",
            invalid.reason.as_deref().unwrap_or("inapplicable proposal")
        );
    }
    declared.owe_also(gates::load_at(&ctx.cwd, &head)?, "target", "head");
    if declared.pending_raises.contains(&None) {
        bail!("a higher total needs explicit --total in the same raise");
    }
    declared.pending_raises.clear();
    declared.raise_snapshot_missing = false;
    let errors = declared.budget_errors();
    if !errors.is_empty() {
        bail!("budget raise refused: {}", errors.join("; "));
    }
    if gitio::branch_head(&ctx.cwd, &state.target_branch)? != target
        || gitio::branch_head(&ctx.cwd, &state.branch)? != head
    {
        bail!("budget raise basis moved while counting");
    }
    let mut event = ctx.event(&store, &id, payload)?;
    store.append_local_event(&mut event)?;
    for revision in [&head, &target] {
        gitio::update_ref(
            &ctx.cwd,
            &gitio::retained_ref(&id, &event.event_id, revision),
            revision,
        )?;
    }
    println!(
        "budget raise: {}; snapshot the raise set for fresh review or recorded debt",
        event.event_id
    );
    Ok(0)
}
