//! `arc slots`: the pool's state, and operator release and recovery of
//! leases whose holder is gone.

use super::*;
use crate::slots::{self, Finished, Phase, Pool};
use crate::state::ChangeState;

fn declaration(ctx: &Ctx) -> Result<slots::Declaration> {
    crate::warm_worktree::slot_declaration(&gitio::toplevel(&ctx.cwd)?, true)?
        .context("no [slots] pool is declared in .arc/worktrees.toml")
}

fn pool(ctx: &Ctx, declaration: &slots::Declaration) -> Result<Pool> {
    Pool::existing(&ctx.cwd, &slots::declared_root(&ctx.cwd, declaration)?)
}

pub fn status(ctx: &Ctx, json: bool) -> Result<()> {
    let declaration = declaration(ctx)?;
    let pool = pool(ctx, &declaration)?;
    let top = gitio::toplevel(&ctx.cwd)?;
    let identity = slots::identity(&top, &declaration).ok();
    let target = gitio::primary_worktree_branch(&ctx.cwd)
        .ok()
        .flatten()
        .and_then(|branch| {
            gitio::branch_head(&ctx.cwd, &branch)
                .ok()
                .map(|head| (branch, head))
        });
    let views = slots::view(
        &pool,
        &ctx.cwd,
        &declaration,
        identity.as_deref(),
        target.as_ref().map(|(_, head)| head.as_str()),
    );
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": slots::STATUS_SCHEMA,
                "root": pool.root,
                "pool": pool.id,
                "size": pool.size,
                "default": declaration.default,
                "warm": declaration.warm,
                "target": target.as_ref().map(|(branch, head)| serde_json::json!({
                    "branch": branch,
                    "head": head,
                })),
                "slots": views,
            }))?
        );
        return Ok(());
    }
    println!(
        "slot pool {} (size {}{})",
        pool.root.display(),
        pool.size,
        if declaration.default {
            ", default for worktree checkouts"
        } else {
            ""
        }
    );
    if let Some((branch, head)) = &target {
        println!("ranked for {branch} at {}", render::short_sha(head));
    }
    for view in views {
        let mut line = format!("{}  ", view.name);
        match (&view.error, &view.lease) {
            (Some(error), _) => line.push_str(&format!("unreadable: {error}")),
            (None, Some(lease)) => line.push_str(&format!(
                "{} by {} on {} since {} token {}",
                view.phase.as_deref().unwrap_or("leased"),
                lease.change_id,
                lease.branch,
                lease.leased_at,
                lease.token
            )),
            (None, None) => line.push_str(view.phase.as_deref().unwrap_or("free")),
        }
        line.push_str(&format!("  checkout {}", view.checkout));
        if let Some(head) = &view.head {
            line.push_str(&format!(" at {}", render::short_sha(head)));
        }
        if let Some(built) = &view.built {
            line.push_str(&format!(
                "  built {}{}",
                render::short_sha(&built.revision),
                match view.identity_matches {
                    Some(false) => " (other identity)",
                    _ => "",
                }
            ));
        }
        if let Some(rank) = &view.rank {
            line.push_str(&format!("  rank {rank}"));
        }
        if view.in_use {
            line.push_str("  in use");
        }
        println!("{line}");
    }
    Ok(())
}

/// The ledger's view of the change a lease names, if it exists.
fn leased_change(store: &Store, change_id: &str) -> Result<Option<ChangeState>> {
    match store.state(change_id) {
        Ok(state) => Ok(Some(state)),
        Err(_) if store.resolve_change(change_id).is_err() => Ok(None),
        Err(error) => Err(error),
    }
}

/// Record on `state` that its lease `token` on `slot` ended, when the change
/// recorded that lease and has not recorded its end.
fn record_release(
    ctx: &Ctx,
    store: &Store,
    state: &ChangeState,
    slot: &str,
    token: &str,
) -> Result<()> {
    if state
        .slot
        .as_ref()
        .is_some_and(|record| record.slot == slot && record.token == token && record.held())
    {
        let mut event = ctx.event(
            store,
            &state.change_id,
            Payload::SlotReleased {
                slot: slot.into(),
                token: token.into(),
            },
        )?;
        store.append_local_event(&mut event)?;
    }
    Ok(())
}

/// End a closed change's slot lease after integration or closure: detach
/// the clean checkout at `landed` (its `HEAD` otherwise), delete `merged`
/// with `git branch -d`, record the release and free the slot. Anything
/// that refuses keeps the lease and says how to release it.
pub(crate) fn release_change_slot(
    ctx: &Ctx,
    store: &Store,
    state: &ChangeState,
    landed: Option<&str>,
    merged: Option<&str>,
) {
    let Some(record) = state.slot.as_ref().filter(|record| record.held()) else {
        return;
    };
    let kept = |reason: String| {
        println!(
            "kept slot {} leased at {}: {reason}; once resolved, run `arc slots release {} --token {}`",
            record.slot, record.path, record.slot, record.token
        );
    };
    let outcome = (|| -> Result<Option<String>> {
        let pool = Pool::existing(&ctx.cwd, Path::new(&record.root))?;
        match slots::detach(&pool, &ctx.cwd, &record.slot, &record.token, landed, merged)? {
            Finished::Kept(reason) => Ok(Some(reason)),
            Finished::Detached(_use) => {
                record_release(ctx, store, state, &record.slot, &record.token)?;
                pool.free(&record.slot, &record.token)?;
                Ok(None)
            }
        }
    })();
    match outcome {
        Ok(None) => println!(
            "released slot {}; kept checkout {} detached",
            record.slot, record.path
        ),
        Ok(Some(reason)) => kept(reason),
        Err(error) => kept(format!("{error:#}")),
    }
}

/// Free a slot whose holder is gone. The lease's change must be closed or
/// absent, the token current and the checkout clean; it is left detached.
pub fn release(ctx: &Ctx, slot: &str, token: &str) -> Result<()> {
    let declaration = declaration(ctx)?;
    let pool = pool(ctx, &declaration)?;
    let name = pool.name_of(slot)?;
    let state = pool.read(&name)?;
    let lease = current_lease(&state, token)?;
    let store = ctx.store()?;
    let change = leased_change(&store, &lease.change_id)?;
    // Lease authority, not an append guard: an open change owns its slot
    // until integration or closure releases it.
    if let Some(change) = change.as_ref().filter(|change| change.closure.is_none()) {
        bail!(
            "{name} holds open change {}; integrate or close it, which releases the slot",
            change.change_id
        );
    }
    finish(ctx, &store, &pool, &name, token, change.as_ref())
}

fn current_lease<'a>(state: &'a slots::SlotState, token: &str) -> Result<&'a slots::Lease> {
    let lease = state
        .lease
        .as_ref()
        .with_context(|| format!("{} is not leased", state.name))?;
    if lease.token != token {
        bail!(
            "token {token} is obsolete: {} is leased under another token",
            state.name
        );
    }
    Ok(lease)
}

fn finish(
    ctx: &Ctx,
    store: &Store,
    pool: &Pool,
    name: &str,
    token: &str,
    change: Option<&ChangeState>,
) -> Result<()> {
    match slots::detach(pool, &ctx.cwd, name, token, None, None)? {
        Finished::Kept(reason) => bail!("{reason}; {name} keeps its lease"),
        Finished::Detached(_use) => {
            if let Some(change) = change {
                record_release(ctx, store, change, name, token)?;
            }
            pool.free(name, token)?;
            println!(
                "released {name}; kept checkout {} detached",
                pool.checkout(name).display()
            );
            Ok(())
        }
    }
}

/// Resume an interrupted phase under its current token.
pub fn recover(ctx: &Ctx, slot: &str, token: &str) -> Result<()> {
    let declaration = declaration(ctx)?;
    let pool = pool(ctx, &declaration)?;
    let name = pool.name_of(slot)?;
    let state = pool.read(&name)?;
    let lease = current_lease(&state, token)?.clone();
    let store = ctx.store()?;
    let change = leased_change(&store, &lease.change_id)?;
    let path = pool.checkout(&name);
    match (state.phase, change) {
        (Phase::Free, _) => bail!("{name} is free"),
        (Phase::Leasing | Phase::Switching | Phase::Recorded, Some(change))
            if change.closure.is_none() =>
        {
            let recorded = change
                .slot
                .as_ref()
                .is_some_and(|record| record.slot == name && record.token == token);
            let holds = change.worktree.as_deref().is_some_and(|worktree| {
                crate::config::resolved(Path::new(worktree)) == crate::config::resolved(&path)
            });
            if !recorded {
                if !holds {
                    bail!(
                        "change {} does not record {} as its checkout; inspect it before releasing",
                        change.change_id,
                        path.display()
                    );
                }
                let mut event = ctx.event(
                    &store,
                    &change.change_id,
                    Payload::SlotLeased {
                        root: pool.root.display().to_string(),
                        slot: name.clone(),
                        path: path.display().to_string(),
                        token: token.into(),
                    },
                )?;
                store.append_local_event(&mut event)?;
            }
            pool.update(&name, token, |state| state.phase = Phase::Recorded)?;
            println!(
                "{name} is recorded on open change {}; integrate or close it to release the slot",
                change.change_id
            );
            Ok(())
        }
        (Phase::Leasing | Phase::Switching, None) => {
            // A begin that never opened its change: undo the switch, and
            // delete the branch only while it still sits where it was made.
            let made = gitio::branch_exists(&ctx.cwd, &lease.branch)
                && gitio::branch_head(&ctx.cwd, &lease.branch)? == lease.target;
            match slots::detach(&pool, &ctx.cwd, &name, token, None, None)? {
                Finished::Kept(reason) => bail!("{reason}; {name} keeps its lease"),
                Finished::Detached(_use) => {
                    if made {
                        match gitio::git(&ctx.cwd, &["branch", "-D", &lease.branch]) {
                            Ok(_) => println!("deleted branch {} (no commits)", lease.branch),
                            Err(error) => println!("kept branch {}: {error:#}", lease.branch),
                        }
                    } else if gitio::branch_exists(&ctx.cwd, &lease.branch) {
                        println!("kept branch {}: it holds commits", lease.branch);
                    }
                    pool.free(&name, token)?;
                    println!("released {name}; kept checkout {} detached", path.display());
                    Ok(())
                }
            }
        }
        (_, change) => finish(ctx, &store, &pool, &name, token, change.as_ref()),
    }
}
