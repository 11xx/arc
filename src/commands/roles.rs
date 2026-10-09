use super::*;

/// The role assertions a write records. Each value, typed or resolved from
/// the invoking identity, is a fresh coordinate: a placeholder or a session id
/// refuses, and the assertion is omitted instead.
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
                invoking_identity(ctx, role)?
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
    for (role, values) in &roles {
        for value in values {
            if let Some(defect) =
                crate::coordinate::coordinate_defect(value, ctx.session.as_deref())
            {
                bail!(
                    "{role} value {value:?}: {}; omit the assertion",
                    defect.reason()
                );
            }
        }
    }
    Ok(roles)
}

/// The coordinate a role flag given without a value stands for: a declared
/// person's own `Name <email>`, or else the invocation's resolved harness and
/// model, under a declared actor or one arc derived from the harness session.
/// A role assertion is no approval, so a derived actor may make one; an
/// identity assumed from git config is nobody's claim and stands for none.
fn invoking_identity(ctx: &Ctx, role: &str) -> Result<String> {
    const REMEDY: &str = "omit the role, or pass a real coordinate as its value";
    if ctx.actor_source.declared()
        && ctx.actor.contains('<')
        && crate::trailers::value_is_readable(&ctx.actor)
    {
        return Ok(ctx.actor.trim().to_string());
    }
    if ctx.actor_source == ActorSource::GitFallback {
        bail!(
            "{role} without a value requires a declared invoking identity or one derived from \
             the harness session (`eval \"$(arc env)\"`); pass --actor, or {REMEDY}"
        );
    }
    let model = ctx.resolve_model().model;
    let (Some(harness), Some(model)) = (ctx.harness.as_deref(), model) else {
        bail!("{role} without a value has unresolved harness or model; {REMEDY}");
    };
    if let Some(defect) =
        crate::coordinate::coordinate_defect(&format!("{harness}:{model}"), ctx.session.as_deref())
    {
        bail!(
            "{role} without a value resolves harness {harness:?} and model {model:?}: {}; {REMEDY}",
            defect.reason()
        );
    }
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
    store.append_event(&ctx.event(store, change_id, payload)?)?;
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
