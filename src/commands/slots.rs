//! `arc slots`: the pool's state, warming, and operator release and
//! recovery of leases whose holder is gone.

use super::*;
use crate::slots::{self, Finished, Phase, Pool};
use crate::state::ChangeState;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;

fn declaration(ctx: &Ctx) -> Result<slots::Declaration> {
    crate::warm_worktree::slot_declaration(&gitio::toplevel(&ctx.cwd)?, true)?
        .context("no [slots] pool is declared in .arc/worktrees.toml")
}

fn pool(ctx: &Ctx, declaration: &slots::Declaration) -> Result<Pool> {
    Pool::existing(&ctx.cwd, &slots::declared_root(&ctx.cwd, declaration)?)
}

/// An open change owns its slot until integration or closure releases it.
/// This is lease authority, not an append guard: what a closed change may
/// record is the store's append policy.
fn holds_its_slot(change: &ChangeState) -> bool {
    change.closure.is_none()
}

fn holder(store: Option<&Store>, change_id: &str) -> slots::Holder {
    let state = store.map(|store| leased_change(store, change_id));
    let landing = match (store, &state) {
        (Some(store), Some(Ok(None))) => landing_holder(store, change_id).ok().flatten(),
        _ => None,
    };
    slots::Holder {
        change_id: change_id.into(),
        slug: match &state {
            Some(Ok(Some(state))) => Some(state.slug.clone()),
            _ => None,
        },
        state: match &state {
            None | Some(Err(_)) => "unreadable".into(),
            Some(Ok(None)) => landing.unwrap_or("absent").into(),
            Some(Ok(Some(state))) => match &state.closure {
                None => "open".into(),
                Some(closure) => serde_json::to_value(closure.outcome)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "closed".into()),
            },
        },
    }
}

pub fn status(ctx: &Ctx, json: bool) -> Result<()> {
    let declaration = declaration(ctx)?;
    let root = slots::declared_root(&ctx.cwd, &declaration)?;
    if !root.exists() {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema": slots::STATUS_SCHEMA,
                    "root": root,
                    "created": false,
                    "pool": null,
                    "size": declaration.size,
                    "default": declaration.default,
                    "warm": declaration.warm,
                    "target": null,
                    "slots": [],
                }))?
            );
        } else {
            println!(
                "slot pool {} (size {}) is declared and not yet created; \
                 `arc begin --slot` or `arc slots warm` creates it",
                root.display(),
                declaration.size
            );
        }
        return Ok(());
    }
    let pool = Pool::existing(&ctx.cwd, &root)?;
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
    let mut views = slots::view(
        &pool,
        &ctx.cwd,
        &declaration,
        identity.as_deref(),
        target.as_ref().map(|(_, head)| head.as_str()),
    );
    let store = ctx.store().ok();
    for view in &mut views {
        if let Some(change_id) = view
            .lease
            .as_ref()
            .and_then(|lease| lease.change_id.as_deref())
        {
            view.holder = Some(holder(store.as_ref(), change_id));
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": slots::STATUS_SCHEMA,
                "root": pool.root,
                "created": true,
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
            (None, Some(lease)) => line.push_str(&match &lease.change_id {
                Some(change_id) => format!(
                    "{} by change {change_id}{} on {} since {} token {}",
                    view.phase.as_deref().unwrap_or("leased"),
                    view.holder
                        .as_ref()
                        .map(|holder| format!(
                            " ({}, {})",
                            holder.slug.as_deref().unwrap_or("?"),
                            holder.state
                        ))
                        .unwrap_or_default(),
                    lease.branch.as_deref().unwrap_or("?"),
                    lease.leased_at,
                    lease.token
                ),
                None => format!(
                    "{} for a warm at {} since {} token {}",
                    view.phase.as_deref().unwrap_or("leased"),
                    render::short_sha(&lease.target),
                    lease.leased_at,
                    lease.token
                ),
            }),
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

/// A lease a landing attempt took to gate in names the attempt where a
/// change would be: its state, when `id` is an attempt.
fn landing_holder(store: &Store, id: &str) -> Result<Option<&'static str>> {
    let events = store.repository_events()?;
    let landings = crate::landing::Landings::from_events(events.iter());
    Ok(if landings.prepared(id).is_none() {
        None
    } else if landings.completion(id).is_some() {
        Some("landing-completed")
    } else if landings.abandonment(id).is_some() {
        Some("landing-abandoned")
    } else {
        Some("landing-pending")
    })
}

/// Refuse to free a slot whose lease names a landing attempt still pending:
/// its gates may run there, and only the attempt's recovery ends it.
fn refuse_pending_landing(store: &Store, name: &str, id: &str) -> Result<()> {
    if landing_holder(store, id)? == Some("landing-pending") {
        bail!(
            "{name} is held by pending landing {id}; finish it with `arc land --recover {id}`, \
             which frees the slot"
        );
    }
    Ok(())
}

/// Free each slot a finished landing attempt leased to gate in, leaving its
/// checkout detached where it stands. A slot leased to anything else is
/// untouched; one still in use or left dirty keeps its lease, and the line
/// printed names the release to run once it is resolved.
pub(crate) fn release_landing_slots(ctx: &Ctx, attempt: &str) {
    let outcome = (|| -> Result<()> {
        let toplevel = gitio::toplevel(&ctx.cwd)?;
        let Some(declaration) = crate::warm_worktree::slot_declaration(&toplevel, true)? else {
            return Ok(());
        };
        let root = slots::declared_root(&ctx.cwd, &declaration)?;
        if !root.exists() {
            return Ok(());
        }
        let pool = Pool::existing(&ctx.cwd, &root)?;
        for name in pool.names() {
            let Some(lease) = pool.read(&name)?.lease else {
                continue;
            };
            if lease.change_id.as_deref() != Some(attempt) {
                continue;
            }
            let kept = |reason: String| {
                println!(
                    "kept slot {name} leased to landing {attempt}: {reason}; once resolved, run \
                     `arc slots release {name} --token {}`",
                    lease.token
                )
            };
            match slots::detach(&pool, &ctx.cwd, &name, &lease.token, None, None) {
                Ok(Finished::Detached(_use)) => match pool.free(&name, &lease.token) {
                    Ok(()) => println!(
                        "released slot {name}; kept checkout {} detached",
                        pool.checkout(&name).display()
                    ),
                    Err(error) => kept(format!("{error:#}")),
                },
                Ok(Finished::Kept(reason)) => kept(reason),
                Err(error) => kept(format!("{error:#}")),
            }
        }
        Ok(())
    })();
    if let Err(error) = outcome {
        println!("kept any slot landing {attempt} leased: {error:#}");
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
    if !slots::owned_here(&ctx.cwd, Path::new(&record.root)) {
        println!(
            "slot {} is recorded in the pool at {}, which this repository does not own; nothing to release here",
            record.slot, record.root
        );
        return;
    }
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
    let change = match &lease.change_id {
        Some(change_id) => leased_change(&store, change_id)?,
        None => None,
    };
    if let (Some(id), None) = (&lease.change_id, &change) {
        refuse_pending_landing(&store, &name, id)?;
    }
    if let Some(change) = change.as_ref().filter(|change| holds_its_slot(change)) {
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
    let path = pool.checkout(&name);
    let _use = slots::UseLease::exclusive(&path)?.with_context(|| {
        format!("{name} is in use by a running command; retry when it finishes")
    })?;
    let state = pool.read(&name)?;
    let lease = current_lease(&state, token)?.clone();
    let store = ctx.store()?;
    let Some(change_id) = &lease.change_id else {
        // A warm records nothing on any change: finishing it detaches the
        // checkout where it stands and frees the slot.
        return finish(ctx, &store, &pool, &name, token, None);
    };
    let change = leased_change(&store, change_id)?;
    if change.is_none() {
        refuse_pending_landing(&store, &name, change_id)?;
    }
    match (state.phase, change) {
        (Phase::Free, _) => bail!("{name} is free"),
        (Phase::Leasing | Phase::Switching | Phase::Recorded, Some(change))
            if holds_its_slot(&change) =>
        {
            let recorded = change
                .slot
                .as_ref()
                .is_some_and(|record| record.slot == name && record.token == token);
            let holds = change.worktree.as_deref().is_some_and(|worktree| {
                crate::config::resolved(Path::new(worktree)) == crate::config::resolved(&path)
            });
            pool.require_owned(&ctx.cwd, &state)
                .with_context(|| format!("cannot establish ownership of {name}"))?;
            if !gitio::is_clean(&path)? {
                bail!(
                    "{} has uncommitted or untracked work; {name} keeps its lease",
                    path.display()
                );
            }
            if !holds {
                bail!(
                    "change {} does not record {} as its checkout; inspect it before recording its lease",
                    change.change_id,
                    path.display()
                );
            }
            if lease.branch.as_deref() != Some(change.branch.as_str())
                || gitio::current_branch(&path)?.as_deref() != Some(change.branch.as_str())
            {
                bail!("{name} does not hold change {} on branch {}; inspect it before recording its lease", change.change_id, change.branch);
            }
            if !recorded {
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
            let branch = lease.branch.as_deref().unwrap_or_default();
            let made = !branch.is_empty()
                && gitio::branch_exists(&ctx.cwd, branch)
                && gitio::branch_head(&ctx.cwd, branch)? == lease.target;
            match slots::detach(&pool, &ctx.cwd, &name, token, None, None)? {
                Finished::Kept(reason) => bail!("{reason}; {name} keeps its lease"),
                Finished::Detached(_use) => {
                    if made {
                        match gitio::git(&ctx.cwd, &["branch", "-D", branch]) {
                            Ok(_) => println!("deleted branch {branch} (no commits)"),
                            Err(error) => println!("kept branch {branch}: {error:#}"),
                        }
                    } else if !branch.is_empty() && gitio::branch_exists(&ctx.cwd, branch) {
                        println!("kept branch {branch}: it holds commits");
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

/// Warm up to `limit` stale free slots at `target` (the primary checkout's
/// branch head by default), one at a time, at the lowest CPU and I/O
/// priority.
pub fn warm(ctx: &Ctx, target: Option<&str>, limit: u32) -> Result<()> {
    let declaration = declaration(ctx)?;
    let script = declaration
        .warm
        .clone()
        .context("no warm command is declared: set `warm` in [slots] of .arc/worktrees.toml")?;
    let root = slots::declared_root(&ctx.cwd, &declaration)?;
    if let Some(prefix) = ctx.excluded_by_sandbox(&root)? {
        bail!(
            "slot pool {} is outside the sandbox at {}",
            root.display(),
            prefix.display()
        );
    }
    let target = match target {
        Some(target) => gitio::rev_parse(&ctx.cwd, &format!("{target}^{{commit}}"))?,
        None => {
            let branch = gitio::primary_worktree_branch(&ctx.cwd)?
                .context("the primary checkout is detached; pass --target")?;
            gitio::branch_head(&ctx.cwd, &branch)?
        }
    };
    deprioritize();
    let identity = slots::identity(&gitio::toplevel(&ctx.cwd)?, &declaration)?;
    let open_worktrees: Vec<(PathBuf, String)> = ctx
        .store()?
        .readable_states()?
        .into_values()
        .filter(holds_its_slot)
        .filter_map(|state| {
            let worktree = state.worktree?;
            Some((
                crate::config::resolved(Path::new(&worktree)),
                state.change_id,
            ))
        })
        .collect();
    let short = render::short_sha(&target);
    let mut warmed = 0;
    while warmed < limit {
        let pool = Pool::declared(&ctx.cwd, &declaration)?;
        let request = slots::LeaseRequest {
            change_id: None,
            branch: None,
            target: &target,
            actor: Some(&ctx.actor),
            session: ctx.session.as_deref(),
        };
        let leased = match slots::select(
            &ctx.cwd,
            pool,
            &declaration,
            &request,
            &identity,
            |pool, state| !slots::current(pool, state, &target, &identity),
        )? {
            slots::Selected::Leased(leased) => leased,
            slots::Selected::None {
                table, passed_over, ..
            } => {
                if warmed == 0 {
                    println!(
                        "nothing to warm at {short}: {passed_over} free slot(s) already built there"
                    );
                    for row in table {
                        println!("  {row}");
                    }
                }
                break;
            }
        };
        let path = crate::config::resolved(&leased.path);
        let open_change = open_worktrees
            .iter()
            .find(|(worktree, _)| *worktree == path)
            .map(|(_, change)| change.as_str());
        println!(
            "warming {} at {} for {short}",
            leased.name,
            leased.path.display()
        );
        if let Err(error) = leased.warm(&ctx.cwd, &declaration, &script, open_change) {
            return Err(error.context(leased.kept()));
        }
        println!("warmed {}: built {short}", leased.name);
        warmed += 1;
    }
    Ok(())
}

/// Run this process and everything it starts at the lowest CPU priority
/// and in the idle I/O class, as `nice -n 19` and `ionice -c 3` would.
fn deprioritize() {
    const IOPRIO_WHO_PROCESS: libc::c_long = 1;
    const IOPRIO_IDLE: libc::c_long = 3 << 13;
    // SAFETY: both calls take plain integers and affect only this process.
    let (cpu, io) = unsafe {
        (
            libc::setpriority(libc::PRIO_PROCESS, 0, 19),
            libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0, IOPRIO_IDLE),
        )
    };
    if cpu != 0 || io != 0 {
        eprintln!(
            "warning: cannot lower the warm's priority: {}",
            std::io::Error::last_os_error()
        );
    }
}

/// After a successful integration, start one detached
/// `arc slots warm --limit 1` when `[slots] warm_after_integrate` asks for
/// it and no slot of the pool is in use. Reports what it did; never fails.
pub(crate) fn warm_after_integration(ctx: &Ctx) {
    match start_warm(ctx) {
        Ok(Some(report)) => println!("slots: {report}"),
        Ok(None) => {}
        Err(error) => println!("slots: no warm started: {error:#}"),
    }
}

fn start_warm(ctx: &Ctx) -> Result<Option<String>> {
    let top = gitio::toplevel(&ctx.cwd)?;
    let Some(declaration) = crate::warm_worktree::slot_declaration(&top, false)? else {
        return Ok(None);
    };
    if !declaration.warm_after_integrate {
        return Ok(None);
    }
    let root = slots::declared_root(&ctx.cwd, &declaration)?;
    if !root.exists() {
        return Ok(Some(format!(
            "no warm started: the pool at {} is not created yet",
            root.display()
        )));
    }
    let pool = Pool::existing(&ctx.cwd, &root)?;
    if let Some(name) = pool.names().into_iter().find(|name| pool.in_use(name)) {
        return Ok(Some(format!(
            "no warm started: {name} is in use by a running command"
        )));
    }
    let checkout = gitio::primary_worktree(&ctx.cwd).unwrap_or(top);
    let log = pool.root.join("warm.log");
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&log)
        .with_context(|| format!("cannot open {}", log.display()))?;
    let child =
        crate::child_command::command(std::env::current_exe().context("cannot locate arc")?)
            .args(["slots", "warm", "--limit", "1"])
            .current_dir(&checkout)
            .stdin(std::process::Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output)
            .process_group(0)
            .spawn()
            .context("cannot start arc slots warm")?;
    Ok(Some(format!(
        "started `arc slots warm --limit 1` (pid {}); its output is in {}",
        child.id(),
        log.display()
    )))
}
