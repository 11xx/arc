//! Gate verification keeps observed process evidence distinct from attestation.
//! Executed gates run in a process group, capture a bounded combined output
//! tail, and honor an optional declared timeout; attested gates carry only the
//! externally supplied result.

use super::*;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{ExitStatus, Stdio};
use std::sync::mpsc::{self, TryRecvError};

const OUTPUT_TAIL_BYTES: usize = 4096;
const SIGKILL: i32 = 9;

extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

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

struct VerificationInput {
    gate: Option<String>,
    command: String,
    timeout_seconds: Option<u64>,
    attested_result: Option<VerifyResult>,
    note: Option<String>,
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
                VerificationInput {
                    gate: Some(name.clone()),
                    command: gate.command.clone(),
                    timeout_seconds: gate.timeout,
                    attested_result: None,
                    note: note.clone(),
                },
            )?;
            if result == 0 {
                passed += 1;
            }
        }
        println!("gates: {passed}/{total} pass");
        return Ok(if passed == total { 0 } else { 1 });
    }
    let (cmd, timeout) = match (&gate, command) {
        (Some(name), None) => {
            let gates = gates::load(&toplevel)?;
            let declared = gates
                .gates
                .get(name)
                .with_context(|| format!("gate {name:?} not declared in .arc/gates.toml"))?;
            (declared.command.clone(), declared.timeout)
        }
        (None, Some(c)) => (c, None),
        (Some(_), Some(_)) => bail!("--gate and --command are mutually exclusive"),
        (None, None) => bail!("provide --gate <name> or --command <cmd>"),
    };
    record_verification(
        ctx,
        &store,
        &change_id,
        VerificationInput {
            gate,
            command: cmd,
            timeout_seconds: timeout,
            attested_result,
            note,
        },
    )
}

fn record_verification(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    input: VerificationInput,
) -> Result<i32> {
    let VerificationInput {
        gate,
        command: cmd,
        timeout_seconds,
        attested_result,
        note,
    } = input;
    let revision = gitio::head(&ctx.cwd)?;
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());

    let attest = attested_result.is_some();
    let (result, exit_code, duration_ms, output_tail, timed_out) = match attested_result {
        // Attested evidence has only the caller's result because arc did not
        // execute a process or observe an exit code or duration.
        Some(result) => (result, None, None, None, false),
        None => {
            eprintln!("running: {cmd}");
            let started = std::time::Instant::now();
            let observed = run_gate(&cmd, &ctx.cwd, timeout_seconds)?;
            let duration_ms = started.elapsed().as_millis() as u64;
            let exit_code = observed.status.code().unwrap_or(-1);
            let result = if observed.status.success() && !observed.timed_out {
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
            (
                result,
                Some(exit_code),
                Some(duration_ms),
                observed.output_tail,
                observed.timed_out,
            )
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
            output_tail,
            timed_out,
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

struct GateRun {
    status: ExitStatus,
    output_tail: Option<String>,
    timed_out: bool,
}

fn run_gate(cmd: &str, cwd: &Path, timeout_seconds: Option<u64>) -> Result<GateRun> {
    let started = Instant::now();
    let deadline = timeout_seconds
        .map(|seconds| {
            started
                .checked_add(Duration::from_secs(seconds))
                .context("gate timeout is too large")
        })
        .transpose()?;
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("exec 2>&1\n{cmd}"))
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("failed to run gate command")?;
    let stdout = child
        .stdout
        .take()
        .context("gate output pipe unavailable")?;
    let (reader_done_tx, reader_done_rx) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let output = read_output_tail(stdout);
        let _ = reader_done_tx.send(());
        output
    });

    let mut status = None;
    let mut reader_done = false;
    let timed_out = loop {
        if status.is_none() {
            status = child
                .try_wait()
                .context("failed to wait for gate command")?;
        }
        if !reader_done {
            reader_done = match reader_done_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => true,
                Err(TryRecvError::Empty) => false,
            };
        }
        if status.is_some() && reader_done {
            break false;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if let Err(error) = kill_process_group(child.id()) {
                // The final group member may exit between the completion poll
                // and kill(2). Preserve its verification evidence instead of
                // turning that normal race into a command error.
                if error.raw_os_error() != Some(3) {
                    return Err(error).context("failed to kill timed-out gate process group");
                }
            }
            if status.is_none() {
                status = Some(
                    child
                        .wait()
                        .context("failed to reap timed-out gate command")?,
                );
            }
            break true;
        }
        thread::sleep(Duration::from_millis(10));
    };

    let output = reader
        .join()
        .map_err(|_| anyhow::anyhow!("gate output reader panicked"))?
        .context("failed to read gate output")?;
    let status = status.context("gate leader exited without an observed status")?;
    let output_tail = (!output.is_empty()).then(|| String::from_utf8_lossy(&output).into_owned());
    Ok(GateRun {
        status,
        output_tail,
        timed_out,
    })
}

fn read_output_tail(mut output: impl Read) -> io::Result<Vec<u8>> {
    let mut tail = Vec::with_capacity(OUTPUT_TAIL_BYTES);
    let mut chunk = [0_u8; 8192];
    loop {
        let read = output.read(&mut chunk)?;
        if read == 0 {
            return Ok(tail);
        }
        if read >= OUTPUT_TAIL_BYTES {
            tail.clear();
            tail.extend_from_slice(&chunk[read - OUTPUT_TAIL_BYTES..read]);
            continue;
        }
        let overflow = tail
            .len()
            .saturating_add(read)
            .saturating_sub(OUTPUT_TAIL_BYTES);
        if overflow > 0 {
            tail.drain(..overflow);
        }
        tail.extend_from_slice(&chunk[..read]);
    }
}

fn kill_process_group(pid: u32) -> io::Result<()> {
    let pid = i32::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "gate pid exceeds i32"))?;
    // SAFETY: `kill` is called with a negated child PID created as the leader
    // of its own process group; SIGKILL requires no borrowed memory contract.
    if unsafe { kill(-pid, SIGKILL) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
    reference: Option<&str>,
    tags: Vec<String>,
    into: Option<String>,
    message: Option<String>,
    cleanup: bool,
) -> Result<i32> {
    match (reference, tags.is_empty()) {
        (Some(reference), true) => integrate_one(
            ctx,
            reference,
            into,
            message,
            cleanup,
            ClosedBehavior::Refuse,
        ),
        (None, false) => {
            if into.is_some() {
                bail!("--into is only valid when integrating one change");
            }
            if message.is_some() {
                bail!("--message is only valid when integrating one change");
            }
            integrate_tagged(ctx, normalize_tags(tags)?, cleanup)
        }
        (Some(_), false) => bail!("provide a change or --tag, not both"),
        (None, true) => bail!("provide a change or at least one --tag"),
    }
}

#[derive(Clone, Copy)]
enum ClosedBehavior {
    Refuse,
    SkipTagged,
}

/// Integrate one already-selected change. Tagged integration reuses this
/// guarded path for every open member so each merge gets the normal target,
/// approval, gate, and dependency checks.
fn integrate_one(
    ctx: &Ctx,
    reference: &str,
    into: Option<String>,
    message: Option<String>,
    cleanup: bool,
    closed_behavior: ClosedBehavior,
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
    if st.is_closed() && matches!(closed_behavior, ClosedBehavior::SkipTagged) {
        println!("{}: {}", st.change_id, change_status(&st));
        return Ok(0);
    }
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

fn integrate_tagged(ctx: &Ctx, tags: Vec<String>, cleanup: bool) -> Result<i32> {
    let batch_ctx = Ctx {
        cwd: gitio::primary_worktree(&ctx.cwd)?,
        actor: ctx.actor.clone(),
        harness: ctx.harness.clone(),
        session: ctx.session.clone(),
    };
    let store = batch_ctx.store()?;
    let selected = batch_ctx
        .load_all_states(&store)?
        .into_iter()
        .filter(|(_, state)| tags.iter().all(|tag| state.tags.contains(tag)))
        .collect::<BTreeMap<_, _>>();
    if selected.is_empty() {
        bail!("no changes match tags {}", tags.join(", "));
    }

    for change_id in dependency_order(&selected)? {
        let code = integrate_one(
            &batch_ctx,
            &change_id,
            None,
            None,
            cleanup,
            ClosedBehavior::SkipTagged,
        )?;
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}

/// Return selected changes in dependency order. Unrelated members are stable
/// by their ledger opening time, then immutable change ID.
pub(crate) fn dependency_order(selected: &BTreeMap<String, ChangeState>) -> Result<Vec<String>> {
    let mut pending = selected.keys().cloned().collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(pending.len());

    while !pending.is_empty() {
        let mut ready = pending
            .iter()
            .filter(|change_id| {
                selected[*change_id]
                    .blocked_by
                    .iter()
                    .filter(|blocker| selected.contains_key(*blocker))
                    .all(|blocker| !pending.contains(blocker))
            })
            .cloned()
            .collect::<Vec<_>>();
        ready.sort_by(|left, right| {
            selected[left]
                .opened_at
                .cmp(&selected[right].opened_at)
                .then_with(|| left.cmp(right))
        });
        let Some(next) = ready.into_iter().next() else {
            bail!("selected changes contain a dependency cycle");
        };
        pending.remove(&next);
        ordered.push(next);
    }

    Ok(ordered)
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
