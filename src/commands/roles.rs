use super::*;

pub(crate) fn assertions(
    ctx: &Ctx,
    planned_by: Vec<String>,
    orchestrated_by: Vec<String>,
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut roles = BTreeMap::new();
    for (role, values) in [
        ("Planned-by", planned_by),
        ("Orchestrated-by", orchestrated_by),
    ] {
        for value in values {
            let value = if value.is_empty() {
                invoking_identity(ctx)?
            } else {
                value.trim().to_string()
            };
            roles
                .entry(role.to_string())
                .or_insert_with(Vec::new)
                .push(value);
        }
    }
    crate::trailers::validate_roles(&roles)?;
    Ok(roles)
}

fn invoking_identity(ctx: &Ctx) -> Result<String> {
    if !ctx.actor_source.declared() {
        bail!("role assertion requires a declared invoking identity; pass --actor or an explicit role value");
    }
    if ctx.actor.contains('<') && crate::trailers::value_is_readable(&ctx.actor) {
        return Ok(ctx.actor.clone());
    }
    let model = ctx.resolve_model().model;
    let (Some(harness), Some(model)) = (ctx.harness.as_deref(), model) else {
        bail!("role assertion has unresolved harness or model; pass an explicit role value or declare --harness and --model");
    };
    Ok(format!("{harness}:{model}"))
}

pub(crate) fn record(
    ctx: &Ctx,
    reference: &str,
    planned_by: Vec<String>,
    orchestrated_by: Vec<String>,
    withdraw: Vec<String>,
) -> Result<()> {
    let asserted = assertions(ctx, planned_by, orchestrated_by)?;
    let mut withdrawn = BTreeMap::new();
    for withdrawal in withdraw {
        let (role, value) = withdrawal
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--withdraw requires ROLE=VALUE"))?;
        withdrawn
            .entry(role.to_string())
            .or_insert_with(Vec::new)
            .push(value.trim().to_string());
    }
    crate::trailers::validate_roles(&withdrawn)?;
    if asserted.is_empty() && withdrawn.is_empty() {
        bail!("roles requires at least one --planned-by, --orchestrated-by, or --withdraw");
    }
    let store = ctx.store()?;
    let (change_id, _transition, state) = locked_state(&store, reference)?;
    append(ctx, &store, &change_id, &state, asserted, withdrawn)?;
    println!("recorded contribution roles on {change_id}");
    Ok(())
}

pub(crate) fn append(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    state: &ChangeState,
    asserted: BTreeMap<String, Vec<String>>,
    withdrawn: BTreeMap<String, Vec<String>>,
) -> Result<()> {
    if asserted.is_empty() && withdrawn.is_empty() {
        return Ok(());
    }
    let payload = Payload::ChangeRolesAsserted {
        asserted,
        withdrawn,
    };
    ensure_append_allowed(state, &payload)?;
    store.append_event(&ctx.event(store, change_id, payload))?;
    Ok(())
}

pub(crate) fn print(ctx: &Ctx, reference: &str, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    let projection = crate::trailers::for_change(&state);
    if json {
        println!("{}", serde_json::to_string_pretty(&projection)?);
    } else {
        for line in projection.trailers {
            println!("{line}");
        }
        for note in projection.notes {
            eprintln!("note: {note}");
        }
    }
    Ok(())
}
