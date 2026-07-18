use super::*;

pub fn check_selection(ctx: &Ctx, reference: Option<&str>, tags: Vec<String>) -> Result<i32> {
    match (reference, tags.is_empty()) {
        (Some(reference), true) => check(ctx, reference),
        (None, false) => check_tagged(ctx, normalize_tags(tags)?),
        (Some(_), false) => bail!("provide a change or --tag, not both"),
        (None, true) => bail!("provide a change or at least one --tag"),
    }
}

pub struct VerifyArgs {
    pub all: bool,
    pub gate: Option<String>,
    pub command: Option<String>,
    pub attest: bool,
    pub result: Option<VerifyResult>,
    pub note: Option<String>,
}

pub fn verify(ctx: &Ctx, reference: &str, args: VerifyArgs) -> Result<i32> {
    let VerifyArgs {
        all,
        gate,
        command,
        attest,
        result,
        note,
    } = args;
    if all && (gate.is_some() || command.is_some() || attest || result.is_some()) {
        bail!("--all cannot be combined with --gate, --command, --attest, or --result");
    }
    // --attest records evidence arc did not observe (so it needs the caller's
    // --result); without it arc runs the command and observing --result is a bug.
    let attested_result = match (attest, result) {
        (true, Some(result)) => Some(result),
        (true, None) => bail!("--attest requires --result pass|fail"),
        (false, Some(_)) => bail!("--result is only valid with --attest"),
        (false, None) => None,
    };
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let toplevel = gitio::toplevel(&ctx.cwd)?;
    if all {
        let gates = gates::load(&toplevel)?;
        let required = gates.required_for(&st.profile);
        if required.is_empty() {
            bail!("no gates declared for profile {}", st.profile);
        }
        let total = required.len();
        let mut passed = 0;
        for (name, gate) in required {
            let result = record_verification(
                ctx,
                &store,
                &change_id,
                Some(name.clone()),
                gate.command.clone(),
                None,
                note.clone(),
            )?;
            if result == 0 {
                passed += 1;
            }
        }
        println!("gates: {passed}/{total} pass");
        return Ok(if passed == total { 0 } else { 1 });
    }
    let cmd = match (&gate, command) {
        (Some(name), None) => {
            let gates = gates::load(&toplevel)?;
            gates
                .gates
                .get(name)
                .with_context(|| format!("gate {name:?} not declared in .arc/gates.toml"))?
                .command
                .clone()
        }
        (None, Some(c)) => c,
        (Some(_), Some(_)) => bail!("--gate and --command are mutually exclusive"),
        (None, None) => bail!("provide --gate <name> or --command <cmd>"),
    };
    record_verification(ctx, &store, &change_id, gate, cmd, attested_result, note)
}

fn record_verification(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    gate: Option<String>,
    cmd: String,
    attested_result: Option<VerifyResult>,
    note: Option<String>,
) -> Result<i32> {
    let revision = gitio::head(&ctx.cwd)?;
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());

    let attest = attested_result.is_some();
    let (result, exit_code, duration_ms) = match attested_result {
        // Attested: record the caller's result without running anything. The
        // gate did not execute here, so there is no observed exit code, timing,
        // or head movement to re-check.
        Some(result) => {
            let exit_code = if result == VerifyResult::Pass { 0 } else { 1 };
            (result, exit_code, 0)
        }
        None => {
            eprintln!("running: {cmd}");
            let started = std::time::Instant::now();
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .current_dir(&ctx.cwd)
                .status()
                .context("failed to run gate command")?;
            let duration_ms = started.elapsed().as_millis() as u64;
            let exit_code = out.code().unwrap_or(-1);
            let result = if out.success() {
                VerifyResult::Pass
            } else {
                VerifyResult::Fail
            };
            // The gate may have moved the branch head while it ran. Evidence
            // stays pinned to the pre-gate revision; warn so a lead knows the
            // recorded revision no longer matches the working head.
            let post = gitio::head(&ctx.cwd)?;
            if post != revision {
                eprintln!(
                    "warning: head moved during verification ({revision} -> {post}); \
                     evidence recorded at {revision}"
                );
            }
            (result, exit_code, duration_ms)
        }
    };

    // Gates are arbitrary external commands and may legitimately invoke arc.
    // Acquire the append lock only after they return, then re-check closure.
    let (_, _transition, st) = locked_state(store, change_id)?;
    if st.is_closed() {
        bail!("change {change_id} closed while verification was running");
    }
    let ev = ctx.event(
        store,
        change_id,
        Payload::VerificationRecorded {
            gate,
            command: cmd,
            revision: revision.clone(),
            result,
            exit_code,
            duration_ms,
            hostname,
            attested: attest,
            note,
        },
    );
    store.append_event(&ev)?;
    let marker = if attest { " (attested)" } else { "" };
    println!("verification: {result:?}{marker} at {revision}");
    println!("event: {}", ev.event_id);
    Ok(if result == VerifyResult::Pass { 0 } else { 1 })
}

pub fn hold(ctx: &Ctx, reference: &str, reason: String) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, _transition, st) = locked_state(&store, reference)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let ev = ctx.event(&store, &change_id, Payload::HoldSet { reason });
    store.append_event(&ev)?;
    println!("hold set on {change_id}");
    Ok(())
}

pub fn release_hold(ctx: &Ctx, reference: &str, reason: Option<String>) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, _transition, st) = locked_state(&store, reference)?;
    if st.hold.is_none() {
        bail!("no active hold on {change_id}");
    }
    let ev = ctx.event(&store, &change_id, Payload::HoldReleased { reason });
    store.append_event(&ev)?;
    println!("hold released on {change_id}");
    Ok(())
}

pub fn integrate(
    ctx: &Ctx,
    reference: &str,
    into: Option<String>,
    message: Option<String>,
    cleanup: bool,
) -> Result<i32> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let initial = state::reduce(&store.load_events(&change_id)?)?;
    let target = into.unwrap_or_else(|| initial.target_branch.clone());
    // Cross-change order is always target, then change. This serializes the
    // target worktree without allowing an integration/metadata lock cycle.
    let target_lock = store.lock_target(&target)?;
    let transition = store.lock_transition(&change_id)?;
    let st = state::reduce(&store.load_events(&change_id)?)?;
    let report = ctx.report(&store, &st)?;
    if let Some(claim) = &st.claim {
        let timing = state::claim_timing_at(claim, chrono::Utc::now());
        let caller = state::ClaimIdentity {
            actor: ctx.actor.clone(),
            harness: ctx.harness.clone().unwrap_or_default(),
            session: ctx.session.clone().unwrap_or_default(),
        };
        if timing.active && claim.owner != caller {
            eprintln!(
                "warning: active foreign claim by {} via {}/{} at stage {}{}; integration remains lead-owned",
                claim.owner.actor,
                claim.owner.harness,
                claim.owner.session,
                timing.stage,
                if timing.stale { " (stale)" } else { "" }
            );
        }
    }
    if !report.integrate_ready {
        eprint!("{}", render::blocker_explanation(&st, &report));
        return Ok(status::check_exit_code(&report));
    }

    // The approved head, merged by exact SHA so a branch moved after
    // approval can never smuggle unreviewed commits into the merge.
    let approved_head = st
        .latest_patchset()
        .context("no patchset recorded")?
        .head
        .clone();

    let wt = gitio::worktree_for_branch(&ctx.cwd, &target)?
        .with_context(|| format!("no worktree has {target:?} checked out; check it out first"))?;
    if !gitio::is_clean(&wt)? {
        bail!("target worktree {} is not clean", wt.display());
    }
    let old_target = gitio::branch_head(&ctx.cwd, &target)?;
    let msg = message.unwrap_or_else(|| format!("merge({}): {}", st.slug, st.title));

    if let Err(e) = gitio::git(
        &wt,
        &["merge", "--no-ff", "--no-edit", "-m", &msg, &approved_head],
    ) {
        let _ = gitio::git(&wt, &["merge", "--abort"]);
        bail!("merge failed (aborted): {e}");
    }

    let merged = gitio::head(&wt)?;
    let parents = gitio::commit_parents(&wt, &merged)?;
    if parents != vec![old_target.clone(), approved_head.clone()] {
        bail!(
            "merge commit {merged} has unexpected parents {parents:?}; \
             expected [{old_target}, {approved_head}] — target moved during \
             integration, inspect before trusting this merge"
        );
    }

    let ev = ctx.event(
        &store,
        &change_id,
        Payload::ChangeClosed {
            outcome: Closure::Integrated,
            integrated_commit: Some(merged.clone()),
            superseded_by: None,
        },
    );
    store.append_event(&ev)?;
    // The merge and closure event are the atomic integration transition.
    // Retention and worktree cleanup are post-closure maintenance and may
    // invoke Git hooks or other arc commands, so no state lock spans them.
    drop(transition);
    drop(target_lock);
    release_retention_refs(ctx, &change_id, Some(&merged))?;

    println!("integrated: {merged}");
    println!("event: {}", ev.event_id);

    if cleanup {
        // Run cleanup git commands from the target worktree: ctx.cwd may be
        // inside the change worktree that is about to be removed.
        if let Some(wt_path) = &st.worktree {
            let p = PathBuf::from(wt_path);
            if p.exists() && p != wt {
                gitio::git(&wt, &["worktree", "remove", wt_path])?;
                println!("removed worktree {wt_path}");
            }
        }
        // -d refuses unless merged: exactly the safety we want.
        gitio::git(&wt, &["branch", "-d", &st.branch])?;
        println!("deleted branch {}", st.branch);
    }
    Ok(0)
}

pub fn close(
    ctx: &Ctx,
    reference: &str,
    integrated: Option<String>,
    abandoned: bool,
    superseded_by: Option<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = state::reduce(&store.load_events(&change_id)?)?;
    if st.is_closed() {
        bail!("change {change_id} is already closed");
    }
    let (payload, integrated_rev) = match (integrated, abandoned, superseded_by) {
        (Some(rev), false, None) => {
            let rev = gitio::rev_parse(&ctx.cwd, &rev)?;
            (
                Payload::ChangeClosed {
                    outcome: Closure::Integrated,
                    integrated_commit: Some(rev.clone()),
                    superseded_by: None,
                },
                Some(rev),
            )
        }
        (None, true, None) => (
            Payload::ChangeClosed {
                outcome: Closure::Abandoned,
                integrated_commit: None,
                superseded_by: None,
            },
            None,
        ),
        (None, false, Some(other)) => {
            let other_id = store.resolve_change(&other)?;
            (
                Payload::ChangeClosed {
                    outcome: Closure::Superseded,
                    integrated_commit: None,
                    superseded_by: Some(other_id),
                },
                None,
            )
        }
        _ => bail!("provide exactly one of --integrated <rev>, --abandoned, --superseded <change>"),
    };
    let ev = ctx.event(&store, &change_id, payload);
    store.append_event(&ev)?;
    release_retention_refs(ctx, &change_id, integrated_rev.as_deref())?;
    println!("closed: {change_id}");
    println!("event: {}", ev.event_id);
    Ok(())
}

fn check(ctx: &Ctx, reference: &str) -> Result<i32> {
    let store = ctx.store()?;
    let (_, st) = ctx.load_state(&store, reference)?;
    let report = ctx.report(&store, &st)?;
    if report.integrate_ready {
        println!("ready: all integration gates pass");
    } else {
        print!("{}", render::blocker_explanation(&st, &report));
    }
    Ok(status::check_exit_code(&report))
}

fn check_tagged(ctx: &Ctx, tags: Vec<String>) -> Result<i32> {
    let store = ctx.store()?;
    let states = ctx.load_all_states(&store)?;
    let selected = states
        .values()
        .filter(|state| tags.iter().all(|tag| state.tags.contains(tag)))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("no changes match tags {}", tags.join(", "));
    }
    let mut aggregate = 0;
    for state in selected {
        if state.is_closed() {
            println!("{}: {}", state.change_id, change_status(state));
            continue;
        }
        let report = ctx.report(&store, state)?;
        let code = status::check_exit_code(&report);
        println!(
            "{}: {}",
            state.change_id,
            if code == 0 { "ready" } else { "blocked" }
        );
        if code != 0 {
            print!("{}", render::blocker_explanation(state, &report));
            if aggregate == 0 {
                aggregate = code;
            }
        }
    }
    Ok(aggregate)
}

/// Drop a change's retention refs only for heads proven reachable from
/// the integration commit. Everything else stays pinned: abandoned or
/// externally rewritten (squash/rebase) work must never become
/// GC-collectable through arc. Unpinning by hand remains possible with
/// `git update-ref -d refs/arc/keep/<change>/<patchset>`.
fn release_retention_refs(ctx: &Ctx, change_id: &str, integrated: Option<&str>) -> Result<()> {
    let refs = gitio::list_refs(&ctx.cwd, &gitio::retention_prefix(change_id))?;
    for (name, oid) in refs {
        let reachable = match integrated {
            Some(rev) => gitio::is_ancestor(&ctx.cwd, &oid, rev)?,
            None => false,
        };
        if reachable {
            let _ = gitio::delete_ref(&ctx.cwd, &name);
        } else {
            println!("kept {name}: head {oid} is not reachable from the integrated commit");
        }
    }
    Ok(())
}
