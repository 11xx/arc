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
    let payload = Payload::ForgeProjection {
        host,
        base_repo,
        base_ref,
        head_repo,
        head_ref,
        policy,
    };
    ensure_append_allowed(&st, &payload)?;
    let event = ctx.event(&store, &change_id, payload);
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
    let payload = Payload::ForgeLink {
        pr_number: args.pr_number,
        url: args.url,
        base_repo: args.base_repo,
        base_ref: args.base_ref,
        head_repo: args.head_repo,
        head_ref: args.head_ref,
        head_sha: args.head_sha,
    };
    ensure_append_allowed(&st, &payload)?;
    let event = ctx.event(&store, &change_id, payload);
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
    let payload = Payload::ForgeChecks {
        pr_head,
        state: check_state,
        detail,
    };
    ensure_append_allowed(&st, &payload)?;
    let event = ctx.event(&store, &change_id, payload);
    store.append_event(&event)?;
    println!(
        "forge checks recorded: {change_id} {}",
        check_state.as_str()
    );
    println!("event: {}", event.event_id);
    Ok(())
}

/// Record the observed PR lifecycle state, bound to the link it was read at.
/// `merged` requires `--merge-sha`. The head comes from the resolved link
/// rather than the caller, so a lifecycle fact can never claim a head its PR
/// does not have.
pub fn forge_pr_state(
    ctx: &Ctx,
    reference: &str,
    pr_state: crate::forge::ForgePrState,
    merge_sha: Option<String>,
    link: Option<String>,
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
    let Some(current) = st.forge.link.as_ref() else {
        bail!("no forge link recorded on {change_id}; record one before its state");
    };
    if let Some(named) = link.as_deref() {
        // Resolved against every link this change recorded, not just the
        // current one: a prefix shared by a superseded link and the current
        // one names neither, and silently recording against the current PR is
        // exactly the confusion this binding exists to prevent.
        if named.trim().is_empty() {
            bail!("name the link this state was read at; an empty reference matches every link");
        }
        let matches: Vec<&str> = st
            .forge
            .links
            .iter()
            .map(|link| link.event_id.as_str())
            .filter(|event_id| event_id.starts_with(named))
            .collect();
        match matches.as_slice() {
            [one] if *one == current.event_id => {}
            [one] => bail!(
                "{one} is not the current link on {change_id} (that is {}); a lifecycle fact \
                 about a superseded PR cannot be recorded as current",
                current.event_id
            ),
            [] => bail!("{named} is not a link recorded on {change_id}"),
            many => bail!(
                "{named} matches {} links on {change_id}; name one exactly",
                many.len()
            ),
        }
    }
    let link = current;
    let payload = Payload::ForgePrState {
        state: pr_state,
        merge_sha,
        link_event_id: Some(link.event_id.clone()),
        pr_head: Some(link.head_sha.clone()),
    };
    ensure_append_allowed(&st, &payload)?;
    let event = ctx.event(&store, &change_id, payload);
    store.append_event(&event)?;
    println!("forge pr-state recorded: {change_id} {}", pr_state.as_str());
    println!("event: {}", event.event_id);
    Ok(())
}
