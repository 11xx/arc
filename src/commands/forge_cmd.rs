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
    let st = state::reduce(&store.load_events(&change_id)?)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let event = ctx.event(
        &store,
        &change_id,
        Payload::ForgeProjection {
            host,
            base_repo,
            base_ref,
            head_repo,
            head_ref,
            policy,
        },
    );
    store.append_event(&event)?;
    println!("forge projection declared: {change_id}");
    println!("event: {}", event.event_id);
    Ok(())
}

/// Record the observed post-creation PR tuple. Fail closed: the observed
/// tuple must equal the declared tuple exactly and satisfy the declared
/// policy. On any violation, no event is appended and the command exits 10.
pub fn forge_link(ctx: &Ctx, reference: &str, args: ForgeLinkArgs) -> Result<i32> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = state::reduce(&store.load_events(&change_id)?)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let observed = crate::forge::ForgeTuple {
        base_repo: args.base_repo.clone(),
        base_ref: args.base_ref.clone(),
        head_repo: args.head_repo.clone(),
        head_ref: args.head_ref.clone(),
    };
    if let Err(refusal) = crate::forge::validate_link(st.forge.projection.as_ref(), &observed) {
        eprintln!("forge link refused: {}", refusal.message());
        return Ok(10);
    }
    let event = ctx.event(
        &store,
        &change_id,
        Payload::ForgeLink {
            pr_number: args.pr_number,
            url: args.url,
            base_repo: args.base_repo,
            base_ref: args.base_ref,
            head_repo: args.head_repo,
            head_ref: args.head_ref,
            head_sha: args.head_sha,
        },
    );
    store.append_event(&event)?;
    println!("forge link recorded: {change_id} PR #{}", args.pr_number);
    println!("event: {}", event.event_id);
    Ok(0)
}

/// Record an observed hosted-check rollup at an exact PR head.
pub fn forge_checks(
    ctx: &Ctx,
    reference: &str,
    pr_head: String,
    check_state: crate::forge::ForgeCheckState,
    detail: Option<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = state::reduce(&store.load_events(&change_id)?)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let event = ctx.event(
        &store,
        &change_id,
        Payload::ForgeChecks {
            pr_head,
            state: check_state,
            detail,
        },
    );
    store.append_event(&event)?;
    println!(
        "forge checks recorded: {change_id} {}",
        check_state.as_str()
    );
    println!("event: {}", event.event_id);
    Ok(())
}

/// Record the observed PR lifecycle state. `merged` requires `--merge-sha`.
pub fn forge_pr_state(
    ctx: &Ctx,
    reference: &str,
    pr_state: crate::forge::ForgePrState,
    merge_sha: Option<String>,
) -> Result<()> {
    if pr_state == crate::forge::ForgePrState::Merged && merge_sha.is_none() {
        bail!("forge pr-state merged requires --merge-sha");
    }
    if pr_state != crate::forge::ForgePrState::Merged && merge_sha.is_some() {
        bail!("--merge-sha is only valid with state merged");
    }
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = state::reduce(&store.load_events(&change_id)?)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let event = ctx.event(
        &store,
        &change_id,
        Payload::ForgePrState {
            state: pr_state,
            merge_sha,
        },
    );
    store.append_event(&event)?;
    println!("forge pr-state recorded: {change_id} {}", pr_state.as_str());
    println!("event: {}", event.event_id);
    Ok(())
}
