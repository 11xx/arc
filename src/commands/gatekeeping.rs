//! Gate verification keeps observed process evidence distinct from attestation.
//! Executed gates run in a process group, capture a bounded combined output
//! tail, and honor an optional declared timeout; attested gates carry only the
//! externally supplied result. Declared gates may run concurrently, but their
//! evidence is appended afterward in deterministic gate-name order.

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

pub fn check_selection(
    ctx: &Ctx,
    reference: Option<&str>,
    tags: Vec<String>,
    explain: bool,
    json: bool,
) -> Result<i32> {
    match (reference, tags.is_empty()) {
        (Some(reference), true) => check(ctx, reference, explain, json),
        (None, false) => check_tagged(ctx, normalize_tags(tags)?),
        (Some(_), false) => bail!("provide a change or --tag, not both"),
        (None, true) => bail!("provide a change or at least one --tag"),
    }
}

#[derive(serde::Serialize)]
struct CheckOutput<'a> {
    schema: &'static str,
    change_id: &'a str,
    ready: bool,
    exit_code: i32,
    blockers: Vec<CheckBlocker>,
    /// What a lead should know and arc will not refuse for. Never affects
    /// `ready` or the exit code; a change may legitimately ship with one
    /// reviewer, and an orchestrator's review is a valid review unless a
    /// project's policy says otherwise.
    #[serde(skip_serializing_if = "<[crate::status::Advisory]>::is_empty")]
    advisories: &'a [crate::status::Advisory],
}

#[derive(serde::Serialize)]
struct CheckBlocker {
    blocker: &'static str,
    exit_code: i32,
}

pub struct VerifyArgs {
    pub all: bool,
    pub parallel: bool,
    pub skip_green: bool,
    pub gate: Option<String>,
    pub command: Option<String>,
    pub probe: Option<String>,
    pub brief_version: Option<usize>,
    pub probe_phase: Option<ProbePhase>,
    pub attest: bool,
    pub result: Option<VerifyResult>,
    pub tested_revision: Option<String>,
    pub execution_host: Option<String>,
    pub runner: Option<String>,
    pub note: Option<String>,
}

struct VerificationInput {
    run_id: Option<String>,
    probe: Option<ProbeEvidenceRef>,
    gate: Option<String>,
    command: String,
    timeout_seconds: Option<u64>,
    attested_result: Option<VerifyResult>,
    tested_revision: Option<String>,
    execution_host: Option<String>,
    runner: Option<String>,
    note: Option<String>,
}

struct CompletedVerification {
    run_id: Option<String>,
    probe: Option<ProbeEvidenceRef>,
    gate: Option<String>,
    command: String,
    timeout_seconds: Option<u64>,
    revision: String,
    result: VerifyResult,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    output_tail: Option<String>,
    timed_out: bool,
    hostname: String,
    attested: bool,
    runner: Option<String>,
    note: Option<String>,
    tested_tree: Option<String>,
    worktree_dirty: Option<bool>,
    tree_moved: bool,
}

pub fn verify(ctx: &Ctx, reference: &str, args: VerifyArgs) -> Result<i32> {
    let VerifyArgs {
        all,
        parallel,
        skip_green,
        gate,
        command,
        probe,
        brief_version,
        probe_phase,
        attest,
        result,
        tested_revision,
        execution_host,
        runner,
        note,
    } = args;
    if all
        && (gate.is_some()
            || command.is_some()
            || probe.is_some()
            || brief_version.is_some()
            || probe_phase.is_some()
            || attest
            || result.is_some()
            || tested_revision.is_some()
            || execution_host.is_some()
            || runner.is_some())
    {
        bail!(
            "--all cannot be combined with --gate, --command, --probe, --brief-version, \
             --probe-phase, --attest, --result, --tested-revision, --execution-host, or --runner"
        );
    }
    if parallel && !all {
        bail!("--parallel requires --all");
    }
    if skip_green && !all {
        bail!("--skip-green requires --all");
    }
    if probe.is_some() && (gate.is_some() || command.is_some()) {
        bail!("--probe is mutually exclusive with --gate and --command");
    }
    if probe.is_none() && (brief_version.is_some() || probe_phase.is_some()) {
        bail!("--brief-version and --probe-phase require --probe");
    }
    // --attest records evidence arc did not observe (so it needs the caller's
    // --result); without it arc runs the command and observing --result is a bug.
    let attested_result = match (attest, result) {
        (true, Some(result)) => Some(result),
        (true, None) => bail!("--attest requires --result pass|fail"),
        (false, Some(_)) => bail!("--result is only valid with --attest"),
        (false, None) => None,
    };
    let (tested_revision, execution_host, runner) = if attest {
        let tested_revision =
            tested_revision.context("--attest requires --tested-revision <REV>")?;
        let execution_host = nonempty_attestation_value(execution_host, "--execution-host")?;
        let runner = nonempty_attestation_value(runner, "--runner")?;
        (
            Some(gitio::rev_parse(&ctx.cwd, &tested_revision)?),
            Some(execution_host),
            Some(runner),
        )
    } else {
        if tested_revision.is_some() || execution_host.is_some() || runner.is_some() {
            bail!("--tested-revision, --execution-host, and --runner are only valid with --attest");
        }
        (None, None, None)
    };
    let store = ctx.store()?;
    // Verification runs an arbitrary command whose effects outlive the
    // refusal, so the identity question is settled before anything executes.
    ctx.ensure_declared_actor(&store)?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let toplevel = gitio::toplevel(&ctx.cwd)?;
    if let Some(probe_name) = probe {
        let (version, brief) = match brief_version {
            Some(0) => bail!("brief version 0 not found"),
            Some(version) => (
                version,
                st.briefs
                    .get(version - 1)
                    .with_context(|| format!("brief version {version} not found"))?,
            ),
            None => (
                st.briefs.len(),
                st.latest_brief()
                    .context("no brief recorded for acceptance probe")?,
            ),
        };
        let declaration = brief
            .acceptance_probes
            .iter()
            .find(|declared| declared.name == probe_name)
            .with_context(|| {
                format!("brief v{version} does not declare acceptance probe {probe_name:?}")
            })?;
        let phase = probe_phase.unwrap_or(ProbePhase::Final);
        if phase == ProbePhase::Baseline {
            let base = brief
                .base_revision
                .as_deref()
                .context("legacy brief has no base revision for baseline probe evidence")?;
            let head = gitio::head(&ctx.cwd)?;
            if head != base {
                bail!("baseline probe requires HEAD {base}; current HEAD is {head}");
            }
            if let Some(tested_revision) = &tested_revision {
                if tested_revision != base {
                    bail!(
                        "attested baseline probe requires --tested-revision {base}, got {tested_revision}"
                    );
                }
            }
        }
        // A baseline probe is the one kind of evidence that must NOT be at the
        // change head: it runs at the brief's base revision, checked above.
        // Every other phase is counted at the head like a gate, so it earns
        // the same refusal.
        if phase_counts_at_head(phase) {
            match &tested_revision {
                Some(revision) => warn_if_attested_off_head(ctx, &st, revision),
                None => ensure_at_change_head(ctx, &st)?,
            }
        }
        let expected = match phase {
            ProbePhase::Baseline => VerifyResult::Fail,
            ProbePhase::Final => VerifyResult::Pass,
        };
        let code = record_verification(
            ctx,
            &store,
            &change_id,
            VerificationInput {
                run_id: None,
                probe: Some(ProbeEvidenceRef {
                    brief_event_id: brief.event_id.clone(),
                    name: declaration.name.clone(),
                    phase,
                }),
                gate: None,
                command: declaration.command.clone(),
                timeout_seconds: None,
                attested_result,
                tested_revision,
                execution_host,
                runner,
                note,
            },
        )?;
        let observed = if code == 0 {
            VerifyResult::Pass
        } else {
            VerifyResult::Fail
        };
        return Ok(if observed == expected { 0 } else { 1 });
    }
    // Every path below records gate evidence, which status counts only at the
    // change's head.
    match &tested_revision {
        Some(revision) => warn_if_attested_off_head(ctx, &st, revision),
        None => ensure_at_change_head(ctx, &st)?,
    }
    if all {
        let gates = gates::load(&toplevel)?;
        let required = gates.required_for(&st.profile);
        if required.is_empty() {
            bail!("no gates declared for profile {}", st.profile);
        }
        let total = required.len();
        let head = gitio::head(&ctx.cwd)?;
        let mode = if parallel {
            VerificationRunMode::Parallel
        } else {
            VerificationRunMode::Sequential
        };
        let run_id =
            start_verification_run(ctx, &store, &change_id, &head, mode, skip_green, &required)?;
        let mut reused = Vec::new();
        let mut to_run = Vec::new();
        for (name, gate) in required {
            // Reuse is reuse of a *run*, so the recorded command must be the
            // one declared now. Skipping on a name match would report a gate
            // as satisfied by a run of something else.
            let reusable = skip_green
                .then(|| st.gate_evidence_at(name, &head))
                .flatten()
                .filter(|evidence| {
                    evidence.green_at_head() && status::matches_declaration(evidence, gate)
                });
            if let Some(evidence) = reusable {
                println!("gate {name}: skipped (green at head)");
                reused.push((name.clone(), evidence.event_id.clone()));
            } else {
                to_run.push((name, gate));
            }
        }
        append_reuses(ctx, &store, &change_id, &run_id, &head, &reused)?;
        if parallel {
            return verify_all_parallel(
                ctx,
                &store,
                &change_id,
                to_run,
                total,
                reused.len(),
                &run_id,
                &head,
                note,
            );
        }
        let mut passed = reused.len();
        for (name, gate) in to_run {
            let result = record_verification(
                ctx,
                &store,
                &change_id,
                VerificationInput {
                    run_id: Some(run_id.clone()),
                    probe: None,
                    gate: Some(name.clone()),
                    command: gate.command.clone(),
                    timeout_seconds: gate.timeout,
                    attested_result: None,
                    tested_revision: None,
                    execution_host: None,
                    runner: None,
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
            let declared = match gates.gates.get(name) {
                Some(declared) => declared,
                // A gate and a probe are different objects run by adjacent
                // flags. When the miss is a probe the brief already declares,
                // the error knows the right flag and should say it.
                None if brief_declares_probe(&st, name) => bail!(
                    "gate {name:?} not declared in .arc/gates.toml; the current brief declares \
                     acceptance probe {name:?} — run `arc verify --probe {name}`"
                ),
                None => bail!("gate {name:?} not declared in .arc/gates.toml"),
            };
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
            run_id: None,
            probe: None,
            gate,
            command: cmd,
            timeout_seconds: timeout,
            attested_result,
            tested_revision,
            execution_host,
            runner,
            note,
        },
    )
}

/// Resolve a path for comparison, falling back to the path itself when it
/// cannot be canonicalized — a recorded worktree may no longer exist, and a
/// lexical comparison is still the right answer when it does not.
fn canonical_or_owned(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether evidence from this probe phase is counted at the change's head.
///
/// A total match rather than a `!=`: a phase added later must be classified
/// here deliberately instead of inheriting head treatment because it is not
/// `Baseline`.
fn phase_counts_at_head(phase: ProbePhase) -> bool {
    match phase {
        // Baseline evidence is counted at the brief's base revision, which is
        // by design not the head.
        ProbePhase::Baseline => false,
        ProbePhase::Final => true,
    }
}

/// Warn when attested evidence names a revision status will never count.
///
/// Attestation is the caller's assertion about a run arc did not observe, so
/// arc takes the revision it is given rather than overruling it. But evidence
/// off the change head is ignored exactly as it is for a gate arc ran itself,
/// and saying nothing is what turns that into a trap.
fn warn_if_attested_off_head(ctx: &Ctx, st: &state::ChangeState, tested_revision: &str) {
    let Ok(change_head) = gitio::branch_head(&ctx.cwd, &st.branch) else {
        eprintln!(
            "warning: cannot resolve {}'s branch {}, so whether this evidence will be counted \
             is unknown",
            st.change_id, st.branch
        );
        return;
    };
    if tested_revision != change_head {
        eprintln!(
            "warning: attested at {tested_revision}, which is not {}'s head ({change_head}); \
             status counts evidence only at the head, so this will not discharge the gate",
            st.change_id
        );
    }
}

/// Refuse to run a gate anywhere but at the change's own head.
///
/// Evidence is recorded at the head of whatever checkout the command ran in,
/// and status only counts evidence at the change's head. Recording it
/// elsewhere is therefore permanently ignored: `next_action` keeps answering
/// `run_gate:<name>`, and following that advice changes nothing. Refusing
/// before the command runs turns a loop that cannot be completed into one
/// step that can — and for a change with no checkout at all, names the two
/// ways to get one.
///
/// This function implements no exemption: every caller that reaches it is
/// recording evidence status counts at the head. Deciding what is exempt —
/// attested evidence, and a baseline probe — belongs at the call sites, which
/// know which kind of evidence they are about to record.
fn ensure_at_change_head(ctx: &Ctx, st: &state::ChangeState) -> Result<()> {
    let change_head = gitio::branch_head(&ctx.cwd, &st.branch)?;
    if gitio::head(&ctx.cwd)? == change_head {
        return Ok(());
    }
    // `worktree_for_branch` answers "who has the branch checked out", so a
    // worktree sitting detached on this branch's history answers None. That is
    // a checkout in the wrong state, not a missing one, and advising `worktree
    // add` beside it would be advice that cannot be followed.
    if st
        .worktree
        .as_deref()
        .map(std::path::Path::new)
        .is_some_and(|recorded| {
            canonical_or_owned(&ctx.cwd).starts_with(canonical_or_owned(recorded))
        })
    {
        bail!(
            "{} is checked out here but HEAD is not its branch head ({change_head}), so gate \
             evidence would be recorded where status will never count it\n\
             tip: `git checkout {}` in this worktree",
            st.change_id,
            st.branch
        );
    }
    match gitio::worktree_for_branch(&ctx.cwd, &st.branch)? {
        Some(worktree) => bail!(
            "gate evidence would be recorded away from {}'s head, where status will never \
             count it\ntip: run this from {}",
            st.change_id,
            worktree.display()
        ),
        None => bail!(
            "{} has no checkout, so a gate run here would record evidence at the wrong \
             revision and status would never count it\n\
             tip: give it one with `git worktree add <path> {}`, or record evidence arc did \
             not run with `arc verify --attest --tested-revision {change_head} ...`",
            st.change_id,
            st.branch
        ),
    }
}

/// Whether the change's latest brief declares an acceptance probe by this name.
fn brief_declares_probe(state: &state::ChangeState, name: &str) -> bool {
    state
        .latest_brief()
        .is_some_and(|brief| brief.acceptance_probes.iter().any(|p| p.name == name))
}

fn start_verification_run(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    revision: &str,
    mode: VerificationRunMode,
    skip_green: bool,
    gates: &[(&String, &gates::Gate)],
) -> Result<String> {
    let _transition = store.lock_transition(change_id)?;
    let state = state::reduce(&store.load_events(change_id)?)?;
    let payload = Payload::VerificationRunStarted {
        revision: revision.to_owned(),
        mode,
        skip_green,
        gates: gates
            .iter()
            .map(|(name, gate)| VerificationRunGate {
                name: (*name).clone(),
                command: gate.command.clone(),
                timeout_seconds: gate.timeout,
            })
            .collect(),
    };
    ensure_append_allowed(&state, &payload)?;
    let event = ctx.event(store, change_id, payload);
    let run_id = event.event_id.clone();
    store.append_event(&event)?;
    Ok(run_id)
}

fn append_reuses(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    run_id: &str,
    revision: &str,
    reused: &[(String, String)],
) -> Result<()> {
    if reused.is_empty() {
        return Ok(());
    }
    let _transition = store.lock_transition(change_id)?;
    let events = store.load_events(change_id)?;
    let state = state::reduce(&events)?;
    let mut previous_id = events
        .last()
        .context("change has no opening event")?
        .event_id
        .clone();
    for (gate, evidence_event_id) in reused {
        let payload = Payload::VerificationReused {
            run_id: run_id.to_owned(),
            gate: gate.clone(),
            revision: revision.to_owned(),
            evidence_event_id: evidence_event_id.clone(),
        };
        ensure_append_allowed(&state, &payload)?;
        let mut event = ctx.event(store, change_id, payload);
        previous_id = event_id_after(&previous_id)?;
        event.event_id = previous_id.clone();
        store.append_event(&event)?;
    }
    Ok(())
}

pub fn snapshot_with_verify(
    ctx: &Ctx,
    reference: &str,
    base: Option<String>,
    brief_version: Option<usize>,
    verify_requested: bool,
    gates: Vec<String>,
    all: bool,
) -> Result<i32> {
    if !verify_requested && (!gates.is_empty() || all) {
        bail!("--gate and --all require --verify");
    }
    if all && !gates.is_empty() {
        bail!("--all cannot be combined with --gate");
    }
    super::review::snapshot(ctx, reference, base, brief_version)?;
    if !verify_requested {
        return Ok(0);
    }
    if all || gates.is_empty() {
        return verify(
            ctx,
            reference,
            VerifyArgs {
                all: true,
                parallel: false,
                skip_green: false,
                gate: None,
                command: None,
                probe: None,
                brief_version: None,
                probe_phase: None,
                attest: false,
                result: None,
                tested_revision: None,
                execution_host: None,
                runner: None,
                note: None,
            },
        );
    }
    if gates.len() > 1 {
        let unique = gates.iter().collect::<BTreeSet<_>>();
        if unique.len() != gates.len() {
            bail!("--gate values must be unique within one verification run");
        }
        let store = ctx.store()?;
        let (change_id, _) = ctx.load_state(&store, reference)?;
        let toplevel = gitio::toplevel(&ctx.cwd)?;
        let declarations = gates::load(&toplevel)?;
        let selected = gates
            .iter()
            .map(|name| {
                declarations
                    .gates
                    .get_key_value(name)
                    .with_context(|| format!("gate {name:?} not declared in .arc/gates.toml"))
            })
            .collect::<Result<Vec<_>>>()?;
        let revision = gitio::head(&ctx.cwd)?;
        let run_id = start_verification_run(
            ctx,
            &store,
            &change_id,
            &revision,
            VerificationRunMode::Sequential,
            false,
            &selected,
        )?;
        let total = selected.len();
        let mut passed = 0;
        for (name, gate) in selected {
            let code = record_verification(
                ctx,
                &store,
                &change_id,
                VerificationInput {
                    run_id: Some(run_id.clone()),
                    probe: None,
                    gate: Some(name.clone()),
                    command: gate.command.clone(),
                    timeout_seconds: gate.timeout,
                    attested_result: None,
                    tested_revision: None,
                    execution_host: None,
                    runner: None,
                    note: None,
                },
            )?;
            if code == 0 {
                passed += 1;
            }
        }
        println!("gates: {passed}/{total} pass");
        return Ok(if passed == total { 0 } else { 1 });
    }
    let total = gates.len();
    let mut passed = 0;
    for gate in gates {
        let code = verify(
            ctx,
            reference,
            VerifyArgs {
                all: false,
                parallel: false,
                skip_green: false,
                gate: Some(gate),
                command: None,
                probe: None,
                brief_version: None,
                probe_phase: None,
                attest: false,
                result: None,
                tested_revision: None,
                execution_host: None,
                runner: None,
                note: None,
            },
        )?;
        if code == 0 {
            passed += 1;
        }
    }
    println!("gates: {passed}/{total} pass");
    Ok(if passed == total { 0 } else { 1 })
}

pub fn done(ctx: &Ctx, reference: &str) -> Result<i32> {
    if super::claims::owns_live_claim(ctx, reference)? {
        let code = super::claims::stage(ctx, reference, StageArg::Verifying, None, None, false)?;
        if code != 0 {
            return Ok(code);
        }
    }
    super::review::snapshot(ctx, reference, None, None)?;
    let _ = verify(
        ctx,
        reference,
        VerifyArgs {
            all: true,
            parallel: false,
            skip_green: false,
            gate: None,
            command: None,
            probe: None,
            brief_version: None,
            probe_phase: None,
            attest: false,
            result: None,
            tested_revision: None,
            execution_host: None,
            runner: None,
            note: None,
        },
    )?;
    check(ctx, reference, false, false)
}

#[allow(clippy::too_many_arguments)]
fn verify_all_parallel(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    required: Vec<(&String, &gates::Gate)>,
    total: usize,
    skipped_green: usize,
    run_id: &str,
    revision: &str,
    note: Option<String>,
) -> Result<i32> {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());
    let cwd = ctx.cwd.clone();
    // Every gate shares this worktree, so capture the batch boundary once on
    // each side. A gate that changes the worktree makes every result in the
    // batch describe no single tree; recording that conservatively is better
    // than allowing one passing result to look reproducible.
    let before = gitio::worktree_tree(&ctx.cwd)?;
    let inputs = required
        .into_iter()
        .map(|(name, gate)| VerificationInput {
            run_id: Some(run_id.to_owned()),
            probe: None,
            gate: Some(name.clone()),
            command: gate.command.clone(),
            timeout_seconds: gate.timeout,
            attested_result: None,
            tested_revision: None,
            execution_host: None,
            runner: None,
            note: note.clone(),
        })
        .collect::<Vec<_>>();
    for input in &inputs {
        eprintln!(
            "running {}: {}",
            input.gate.as_deref().unwrap_or("command"),
            input.command
        );
    }
    let handles = inputs
        .into_iter()
        .map(|input| {
            let cwd = cwd.clone();
            let revision = revision.to_owned();
            let hostname = hostname.clone();
            thread::spawn(move || execute_verification(input, &cwd, revision, hostname))
        })
        .collect::<Vec<_>>();
    let mut completed = Vec::with_capacity(handles.len());
    let mut first_error = None;
    for handle in handles {
        let outcome = handle
            .join()
            .map_err(|_| anyhow::anyhow!("gate worker panicked"))
            .and_then(|result| result);
        match outcome {
            Ok(item) => completed.push(item),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    let after = gitio::worktree_tree(&ctx.cwd)?;
    let post = gitio::head(&ctx.cwd)?;
    if post != revision {
        eprintln!(
            "warning: head moved during parallel verification ({revision} -> {post}); evidence recorded at {revision}"
        );
    }
    let tree_moved = after != before;
    if tree_moved {
        eprintln!(
            "warning: the worktree changed while parallel gates ran, so this evidence describes no single tree"
        );
    }
    eprintln!(
        "warning: parallel gates share a mutable worktree; their provenance is unknown and passing evidence is not green"
    );
    for item in &mut completed {
        item.tested_tree = Some(before.clone());
        // Boundary snapshots can miss a gate that changes and restores a file
        // while another gate is still running. Leave cleanliness unknown so
        // the shared batch cannot make a passing result look reproducible.
        item.worktree_dirty = None;
        item.tree_moved = tree_moved;
    }
    let ran_passed = completed
        .iter()
        .filter(|item| item.result == VerifyResult::Pass)
        .count();
    append_verifications(ctx, store, change_id, completed)?;
    let passed = ran_passed + skipped_green;
    println!("gates: {passed}/{total} pass");
    Ok(if passed == total { 0 } else { 1 })
}

fn record_verification(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    input: VerificationInput,
) -> Result<i32> {
    let revision = match &input.tested_revision {
        Some(revision) => revision.clone(),
        None => gitio::head(&ctx.cwd)?,
    };
    let hostname = match &input.execution_host {
        Some(hostname) => hostname.clone(),
        None => hostname::get()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "unknown".into()),
    };
    if input.attested_result.is_none() {
        eprintln!("running: {}", input.command);
    }
    // What the command is about to run against, captured before it runs. arc
    // cannot know the tree a remote runner used, so attested evidence records
    // no tree rather than the local one, which would be a guess.
    let before = if input.attested_result.is_none() {
        Some(gitio::worktree_tree(&ctx.cwd)?)
    } else {
        None
    };
    let mut completed = execute_verification(input, &ctx.cwd, revision.clone(), hostname)?;
    if !completed.attested {
        let post = gitio::head(&ctx.cwd)?;
        if post != revision {
            eprintln!(
                "warning: head moved during verification ({revision} -> {post}); \
                 evidence recorded at {revision}"
            );
        }
        if let Some(before) = before {
            let after = gitio::worktree_tree(&ctx.cwd)?;
            let commit_tree = gitio::commit_tree(&ctx.cwd, &revision).ok();
            completed.worktree_dirty = commit_tree.map(|tree| tree != before);
            completed.tree_moved = after != before;
            if completed.tree_moved {
                eprintln!(
                    "warning: the worktree changed while the command ran, so this evidence \
                     describes no single tree"
                );
            }
            completed.tested_tree = Some(before);
        }
    }
    let result = completed.result;
    append_verifications(ctx, store, change_id, vec![completed])?;
    Ok(if result == VerifyResult::Pass { 0 } else { 1 })
}

fn execute_verification(
    input: VerificationInput,
    cwd: &Path,
    revision: String,
    hostname: String,
) -> Result<CompletedVerification> {
    let VerificationInput {
        run_id,
        probe,
        gate,
        command,
        timeout_seconds,
        attested_result,
        tested_revision: _,
        execution_host: _,
        runner,
        note,
    } = input;
    let attested = attested_result.is_some();
    let (result, exit_code, duration_ms, output_tail, timed_out) = match attested_result {
        // Attested evidence has only the caller's result because arc did not
        // execute a process or observe an exit code or duration.
        Some(result) => (result, None, None, None, false),
        None => {
            let started = std::time::Instant::now();
            let observed = run_gate(&command, cwd, timeout_seconds)?;
            let duration_ms = started.elapsed().as_millis() as u64;
            let exit_code = observed.status.code().unwrap_or(-1);
            let result = if observed.status.success() && !observed.timed_out {
                VerifyResult::Pass
            } else {
                VerifyResult::Fail
            };
            (
                result,
                Some(exit_code),
                Some(duration_ms),
                observed.output_tail,
                observed.timed_out,
            )
        }
    };
    Ok(CompletedVerification {
        run_id,
        probe,
        gate,
        command,
        timeout_seconds,
        revision,
        result,
        exit_code,
        duration_ms,
        output_tail,
        timed_out,
        hostname,
        attested,
        runner,
        note,
        // Filled in by the caller, which is what sees the worktree on both
        // sides of the run.
        tested_tree: None,
        worktree_dirty: None,
        tree_moved: false,
    })
}

fn append_verifications(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    completed: Vec<CompletedVerification>,
) -> Result<()> {
    // Gates are arbitrary external commands and may legitimately invoke arc.
    // Acquire the append lock only after they return, then re-check closure.
    let _transition = store.lock_transition(change_id)?;
    let events = store.load_events(change_id)?;
    let st = state::reduce(&events)?;
    let mut previous_id = events
        .last()
        .context("change has no opening event")?
        .event_id
        .clone();
    for item in completed {
        let gate_label = item.gate.clone();
        let result = item.result;
        let revision = item.revision.clone();
        let attested = item.attested;
        previous_id = event_id_after(&previous_id)?;
        let event_id = previous_id.clone();
        let captured_tree = item.tested_tree.clone();
        let mut payload = Payload::VerificationRecorded {
            run_id: item.run_id,
            probe: item.probe,
            gate: item.gate,
            command: item.command,
            timeout_seconds: item.timeout_seconds,
            revision: item.revision,
            result: item.result,
            exit_code: item.exit_code,
            duration_ms: item.duration_ms,
            output_tail: item.output_tail,
            timed_out: item.timed_out,
            hostname: item.hostname,
            attested: item.attested,
            runner: item.runner,
            note: item.note,
            // Set below, once the tree is pinned.
            tested_tree: None,
            worktree_dirty: item.worktree_dirty,
            tree_moved: item.tree_moved,
        };
        // Whether this event may be appended at all is settled before any ref
        // is written, so a refusal — the gate closed the change while it ran —
        // leaves no pin behind for an event that never existed.
        ensure_append_allowed(&st, &payload)?;
        // A recorded `tested_tree` promises the tree is still there, so the
        // claim is made only once the pin holding it exists. A run whose tree
        // cannot be pinned is recorded without one rather than pointing at
        // something collectable.
        if let Some(tree) = &captured_tree {
            let name = gitio::tree_retention_ref(change_id, &event_id);
            match gitio::update_ref(&ctx.cwd, &name, tree) {
                Ok(()) => {
                    if let Payload::VerificationRecorded { tested_tree, .. } = &mut payload {
                        *tested_tree = Some(tree.clone());
                    }
                }
                Err(error) => {
                    if let Payload::VerificationRecorded {
                        tested_tree,
                        worktree_dirty,
                        ..
                    } = &mut payload
                    {
                        // Without the pin, the local tree is unknown even if
                        // the boundary comparison found it clean.
                        *tested_tree = None;
                        *worktree_dirty = None;
                    }
                    eprintln!(
                        "warning: could not keep tree {tree} reachable ({error:#}); recording this \
                         run without local provenance"
                    )
                }
            }
        }
        let mut ev = ctx.event(store, change_id, payload);
        ev.event_id = event_id;
        store.append_event(&ev)?;
        if let Some(gate) = gate_label {
            println!("gate: {gate}");
        }
        let marker = if attested { " (attested)" } else { "" };
        println!("verification: {result:?}{marker} at {revision}");
        println!("event: {}", ev.event_id);
    }
    Ok(())
}

fn nonempty_attestation_value(value: Option<String>, flag: &str) -> Result<String> {
    let value = value.with_context(|| format!("--attest requires {flag} <VALUE>"))?;
    let value = value.trim();
    if value.is_empty() {
        bail!("{flag} must not be empty");
    }
    Ok(value.to_owned())
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
    let payload = Payload::HoldSet { reason };
    ensure_append_allowed(&st, &payload)?;
    let ev = ctx.event(&store, &change_id, payload);
    store.append_event(&ev)?;
    // The event ID is how this hold is released, so printing it is what makes
    // an independent hold usable rather than merely recorded.
    println!("hold {} set on {change_id}", ev.event_id);
    Ok(())
}

/// Release one hold by the event that set it. Naming the hold is what lets two
/// collaborators hold the same change without either lifting the other's.
pub fn release_hold(
    ctx: &Ctx,
    reference: &str,
    hold_event_id: &str,
    reason: Option<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, _transition, st) = locked_state(&store, reference)?;
    if st.holds.is_empty() {
        bail!("no active hold on {change_id}");
    }
    let held = resolve_hold(&st, hold_event_id, &change_id)?;
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::HoldReleased {
            hold_event_id: Some(held.clone()),
            reason,
        },
    );
    store.append_event(&ev)?;
    println!("hold {held} released on {change_id}");
    let left = st.holds.len() - 1;
    if left > 0 {
        println!("{left} other hold(s) still active");
    }
    Ok(())
}

/// Resolve a hold reference to an exact active hold event, accepting a unique
/// prefix the way every other event reference in the CLI does.
fn resolve_hold(state: &ChangeState, reference: &str, change_id: &str) -> Result<String> {
    // An unset shell variable expands to the empty string, which every ID is
    // a prefix of. Releasing a hold by accident is exactly what the identity
    // exists to prevent.
    if reference.is_empty() {
        bail!("name the hold to release; an empty reference matches every hold");
    }
    let matches: Vec<&String> = state
        .holds
        .keys()
        .filter(|id| id.starts_with(reference))
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!(
            "{reference} is not an active hold on {change_id}; active: {}",
            state.holds.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
        many => bail!(
            "{reference} matches {} active holds on {change_id}; name one exactly",
            many.len()
        ),
    }
}

pub fn integrate(
    ctx: &Ctx,
    reference: Option<&str>,
    tags: Vec<String>,
    into: Option<String>,
    message: Option<String>,
    cleanup: bool,
    dry_run: bool,
) -> Result<i32> {
    match (reference, tags.is_empty()) {
        (Some(reference), true) => integrate_one(
            ctx,
            reference,
            into,
            message,
            cleanup,
            ClosedBehavior::Refuse,
            dry_run,
        ),
        (None, false) => {
            if into.is_some() {
                bail!("--into is only valid when integrating one change");
            }
            if message.is_some() {
                bail!("--message is only valid when integrating one change");
            }
            if dry_run {
                bail!("--dry-run is only valid when integrating one change");
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
#[allow(clippy::too_many_arguments)]
fn integrate_one(
    ctx: &Ctx,
    reference: &str,
    into: Option<String>,
    message: Option<String>,
    cleanup: bool,
    closed_behavior: ClosedBehavior,
    dry_run: bool,
) -> Result<i32> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let initial = state::reduce(&store.load_events(&change_id)?)?;
    let target = into.unwrap_or_else(|| initial.target_branch.clone());
    if dry_run {
        // A dry run promises to write nothing, so there is no record for the
        // policy to be about.
        return integrate_dry_run(ctx, &store, &initial, &target, message.as_deref());
    }
    // The same store the merge's closure event will be appended to, so the
    // merge and the record are judged by one reading of the policy.
    ctx.ensure_declared_actor(&store)?;
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
    let approved_patchset = st.latest_patchset().context("no patchset recorded")?;
    let approved_patchset_id = approved_patchset.id.clone();
    let approved_head = approved_patchset.head.clone();

    // Read before the merge, from the same worktree state the readiness
    // decision was made against: this is what the merge is authorized on, and
    // a later read would be a different question.
    let authorization = authorization_basis(ctx, &store, &st, &report, &approved_patchset_id)?;
    // Configuration files are not under any lock arc holds, so readiness and
    // the basis are two reads of something that can move between them.
    // Recomputing readiness against the basis's own gate set is what keeps the
    // merge from proceeding under one configuration and recording another.
    let confirmation = ctx.report(&store, &st)?;
    let confirmed_basis =
        authorization_basis(ctx, &store, &st, &confirmation, &approved_patchset_id)?;
    if confirmed_basis != authorization || !confirmation.integrate_ready {
        bail!(
            "gate or policy configuration changed while preparing the merge; nothing was \
             written — re-run once the worktree has settled"
        );
    }

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
        Payload::ChangeIntegrated {
            integrated_commit: merged.clone(),
            source_patchset_id: approved_patchset_id.clone(),
            source_head: approved_head.clone(),
            target_branch: target.clone(),
            target_before: old_target.clone(),
            authorization: Some(authorization),
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
    crate::journal::auto_log(
        ctx,
        &st.slug,
        &format!("integrated {change_id} at {merged}"),
    );

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

/// Report what `integrate` would do without merging, closing, or writing
/// anything: run the same readiness preflight, then simulate the merge with
/// the needs-rebase machinery. Exit code mirrors `check`: 0 when the merge
/// would proceed cleanly, otherwise the first blocker's code.
/// Everything the guard consumed to authorize one merge, read from the same
/// state the readiness decision was made against.
///
/// The gate and policy values are the ones actually in effect in this
/// invocation's worktree — including uncommitted ones, which is exactly the
/// state Git cannot recover for an auditor later.
fn authorization_basis(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    report: &crate::status::StatusReport,
    approved_patchset_id: &str,
) -> Result<crate::model::AuthorizationBasis> {
    // Either a verdict approved this patchset, or a declared debt stood in for
    // the review nobody performed. One of the two must hold: a merge with
    // neither has nothing authorizing it, and this record exists to say what
    // did.
    let verdict = st.verdicts.iter().rev().find(|verdict| {
        verdict.patchset_id == approved_patchset_id
            && verdict.verdict == crate::model::Verdict::Approved
    });
    if verdict.is_none() && !st.audit_debt_waives_latest_patchset() {
        anyhow::bail!("integration is ready but nothing authorizes the merged patchset: no approving verdict and no declared audit debt");
    }

    let mut gate_evidence = BTreeMap::new();
    for gate in &report.gates {
        let evidence = gate.evidence_event_id.clone().with_context(|| {
            format!(
                "gate {} is green at head with no recorded evidence event",
                gate.name
            )
        })?;
        gate_evidence.insert(gate.name.clone(), evidence);
    }

    let mut prerequisites = Vec::new();
    for blocker in &st.blocked_by {
        // A basis missing a prerequisite is a basis that misstates what was
        // checked, so an unreadable one refuses the merge rather than being
        // quietly omitted.
        let events = store.load_events(blocker).with_context(|| {
            format!("prerequisite {blocker} cannot be read, so the authorization basis                      would be incomplete")
        })?;
        let mut blocker_id = blocker.clone();
        let mut blocker_state = state::reduce(&events)?;
        // Dependency readiness follows supersession, so the basis must record
        // the closure that actually satisfied the dependency rather than the
        // superseded one, whose integrated commit is null.
        let mut seen = vec![blocker_id.clone()];
        while let Some(successor) = blocker_state
            .closure
            .as_ref()
            .filter(|closure| closure.outcome == Closure::Superseded)
            .and_then(|closure| closure.superseded_by.clone())
        {
            if seen.contains(&successor) {
                break;
            }
            let Ok(events) = store.load_events(&successor) else {
                break;
            };
            seen.push(successor.clone());
            blocker_id = successor;
            blocker_state = state::reduce(&events)?;
        }
        if let Some(closure) = &blocker_state.closure {
            prerequisites.push(crate::model::PrerequisiteClosure {
                change_id: blocker_id,
                closure_event_id: closure.event_id.clone(),
                integrated_commit: closure.integrated_commit.clone(),
            });
        }
    }

    let toplevel = gitio::toplevel(&ctx.cwd)?;
    let gates = crate::gates::load(&toplevel)?;
    let policy = crate::policy::load(&toplevel)?;
    let normalized_gates = gates
        .required_for(&st.profile)
        .into_iter()
        .map(|(name, gate)| {
            (
                name.clone(),
                crate::model::NormalizedGate {
                    command: gate.command.clone(),
                    profiles: gate.profiles.clone(),
                    timeout: gate.timeout,
                },
            )
        })
        .collect();

    Ok(crate::model::AuthorizationBasis {
        verdict_event_id: verdict.map(|verdict| verdict.event_id.clone()),
        verdict_provisional: verdict.and_then(|verdict| verdict.provisional.clone()),
        gate_evidence,
        prerequisites,
        // Empty by construction: `integrate_ready` is false while either is
        // non-empty, so the event cannot be written otherwise. Recording them
        // says the guard checked, rather than leaving an auditor to infer it.
        blocking_findings: report.open_blocking_findings.clone(),
        holds: report
            .holds
            .iter()
            .map(|hold| hold.hold_event_id.clone())
            .collect(),
        gates: normalized_gates,
        policy: crate::model::NormalizedPolicy {
            forbid_self_approval: policy.policy.forbid_self_approval,
            require_declared_actor: policy.policy.require_declared_actor,
            provenance_git_identity: policy.provenance.git_identity.as_str().to_string(),
        },
        // Only when the waiver is what let the approval stand. A debt declared
        // beside an approval that needed no waiver authorized nothing, and
        // recording it would claim the merge rested on something it did not.
        audit_debt_event_id: st
            .audit_debt
            .as_ref()
            .filter(|_| report.approval_waived_by_audit_debt)
            .map(|debt| debt.event_id.clone()),
    })
}

fn integrate_dry_run(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    target: &str,
    message: Option<&str>,
) -> Result<i32> {
    // The refusals a real integration makes before touching anything: an
    // undeclared actor, and a target worktree that is missing or dirty. A dry
    // run that skipped them would report a merge the real path refuses.
    ctx.ensure_declared_actor(store)?;
    let target_worktree = gitio::worktree_for_branch(&ctx.cwd, target)?
        .with_context(|| format!("no worktree has {target:?} checked out; check it out first"))?;
    if !gitio::is_clean(&target_worktree)? {
        bail!("target worktree {} is not clean", target_worktree.display());
    }
    let report = ctx.report(store, st)?;
    if !report.integrate_ready {
        eprint!("{}", render::blocker_explanation(st, &report));
        println!(
            "dry-run: would not integrate {} ({})",
            st.change_id, report.ready_reason
        );
        return Ok(status::check_exit_code(&report));
    }

    let approved_head = st
        .latest_patchset()
        .context("no patchset recorded")?
        .head
        .clone();
    let target_head = gitio::branch_head(&ctx.cwd, target)?;
    let conflicts = gitio::merge_conflicts(&ctx.cwd, &target_head, &approved_head)?;
    let msg = message
        .map(str::to_string)
        .unwrap_or_else(|| format!("merge({}): {}", st.slug, st.title));

    println!("dry-run: would integrate {} into {target}", st.change_id);
    println!("  merge message: {msg}");
    println!("  merge parents: [{target_head}, {approved_head}]");
    println!(
        "  merge result: {}",
        if conflicts {
            "conflict — rebase required"
        } else {
            "clean"
        }
    );
    // Only when the merge would actually happen: a conflicting dry run
    // records nothing, so printing a basis "it would record" would describe
    // an event that could not be written.
    if !conflicts {
        let basis = authorization_basis(
            ctx,
            store,
            st,
            &report,
            &st.latest_patchset().context("no patchset recorded")?.id,
        )?;
        println!("  authorization basis it would record:");
        println!("{}", render::authorization_basis(&basis));
    }
    println!("  no events, refs, or worktrees were modified");
    Ok(if conflicts {
        status::Blocker::NeedsRebase.exit_code()
    } else {
        0
    })
}

fn integrate_tagged(ctx: &Ctx, tags: Vec<String>, cleanup: bool) -> Result<i32> {
    let batch_ctx = Ctx {
        cwd: gitio::primary_worktree(&ctx.cwd)?,
        actor: ctx.actor.clone(),
        actor_source: ctx.actor_source,
        fallback_announced: ctx.fallback_announced.clone(),
        harness: ctx.harness.clone(),
        session: ctx.session.clone(),
        model: ctx.model.clone(),
        on_behalf_of: ctx.on_behalf_of.clone(),
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
            false,
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

/// How a change is being closed, as the CLI expresses it. One struct rather
/// than eight positional arguments, because the outcomes are mutually
/// exclusive and a call site should show which one it chose.
pub struct CloseArgs {
    pub assert_integrated: Option<String>,
    pub patchset: Option<String>,
    pub into: Option<String>,
    pub target_before: Option<String>,
    pub abandoned: bool,
    pub superseded_by: Option<String>,
}

pub fn close(ctx: &Ctx, reference: &str, args: CloseArgs) -> Result<()> {
    let CloseArgs {
        assert_integrated,
        patchset,
        into,
        target_before,
        abandoned,
        superseded_by,
    } = args;
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = state::reduce(&store.load_events(&change_id)?)?;
    if st.is_closed() {
        bail!("change {change_id} is already closed");
    }
    if patchset.is_some() && assert_integrated.is_none() {
        bail!("--patchset describes an asserted integration; pass --assert-integrated <REV>");
    }
    if into.is_some() && assert_integrated.is_none() {
        bail!("--into describes an asserted integration; pass --assert-integrated <REV>");
    }
    if target_before.is_some() && assert_integrated.is_none() {
        bail!("--target-before describes an asserted integration; pass --assert-integrated <REV>");
    }
    let (payload, integrated_rev) = match (assert_integrated, abandoned, superseded_by) {
        (Some(rev), false, None) => {
            let rev = gitio::rev_parse(&ctx.cwd, &rev)?;
            let patchset = match patchset {
                Some(id) => st
                    .patchsets
                    .iter()
                    .find(|patchset| patchset.id == id)
                    .with_context(|| format!("{id} is not a patchset of {change_id}"))?,
                None => st.latest_patchset().with_context(|| {
                    format!(
                        "no patchset recorded on {change_id}, so nothing says what was \
                         integrated; record one with `arc snapshot {change_id}` first"
                    )
                })?,
            };
            let target = into.unwrap_or_else(|| st.target_branch.clone());
            if !gitio::branch_exists(&ctx.cwd, &target) {
                bail!(
                    "{target} is not a branch in this repository; an assertion names where the \
                     work actually landed"
                );
            }
            // An assertion arc did not guard still has to be about this
            // change. Without these two checks it could name any commit at
            // all, and the ledger would record an integration that never
            // happened — worse than recording nothing, because it reads as
            // authoritative.
            if !gitio::is_ancestor(&ctx.cwd, &patchset.head, &rev)? {
                bail!(
                    "{rev} does not contain {} ({}), so it is not an integration of this change",
                    patchset.id,
                    &patchset.head[..patchset.head.len().min(8)]
                );
            }
            if !gitio::is_ancestor(&ctx.cwd, &rev, &target)? {
                bail!("{rev} is not on {target}; nothing there integrated this change");
            }
            // For a merge, the first parent is where the target stood before,
            // and Git can be asked. For a fast-forward it is not: the parent
            // is the previous commit *of this change*, and recording it would
            // put the change's own work outside the range it integrated.
            // Nothing in the repository says where the branch pointed, so the
            // caller supplies it or the event records none — an absent base is
            // honest, a wrong one is not.
            let parents = gitio::commit_parents(&ctx.cwd, &rev)?;
            let target_before = match (target_before, parents.len()) {
                // A merge records where the target stood, and Git is a better
                // witness than the caller: letting a flag override it would
                // record a range the merge did not integrate.
                (Some(_), 2..) => bail!(
                    "{rev} is a merge, so where the target stood is its first parent; \
                     --target-before would record a range it did not integrate"
                ),
                (None, 2..) => parents.into_iter().next(),
                (Some(named), _) => Some(gitio::rev_parse(&ctx.cwd, &named)?),
                (None, _) => None,
            };
            (
                Payload::IntegrationAsserted {
                    integrated_commit: rev.clone(),
                    source_patchset_id: patchset.id.clone(),
                    source_head: patchset.head.clone(),
                    target_branch: target,
                    target_before,
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
        _ => bail!(
            "provide exactly one of --assert-integrated <rev>, --abandoned, --superseded <change>"
        ),
    };
    let ev = ctx.event(&store, &change_id, payload);
    store.append_event(&ev)?;
    release_retention_refs(ctx, &change_id, integrated_rev.as_deref())?;
    crate::journal::auto_log(ctx, &st.slug, &format!("closed change {change_id}"));
    println!("closed: {change_id}");
    println!("event: {}", ev.event_id);
    Ok(())
}

fn check(ctx: &Ctx, reference: &str, explain: bool, json: bool) -> Result<i32> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let report = ctx.report(&store, &st)?;
    let code = status::check_exit_code(&report);
    if json {
        let output = CheckOutput {
            schema: "arc-check/2",
            change_id: &change_id,
            ready: report.integrate_ready,
            exit_code: code,
            blockers: report
                .blockers
                .iter()
                .map(|blocker| CheckBlocker {
                    blocker: blocker.as_str(),
                    exit_code: blocker.exit_code(),
                })
                .collect(),
            advisories: &report.advisories,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(code);
    }
    if explain {
        print!("{}", render::check_explanation(&st, &report));
    } else if report.integrate_ready {
        println!("ready: all integration gates pass");
    } else {
        print!("{}", render::blocker_explanation(&st, &report));
    }
    render::advisories(&report);
    Ok(code)
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
        // A tagged preflight is what a lead reads before `integrate --tag`.
        // Reporting a member ready while withholding that nobody but the
        // brief's author approved it hides the advisory exactly where it was
        // going to be acted on.
        render::advisories(&report);
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
    // A tree pinned by verification is evidence about a change that is now
    // closed. Keeping every one forever would grow a ref per verification run
    // without bound; what survives is the same thing that survives for heads —
    // whatever is not already reachable from what shipped. One object walk
    // answers for every pin, and only when there is a pin to answer for.
    let tree_refs = gitio::list_refs(&ctx.cwd, &gitio::tree_retention_prefix(change_id))?;
    if !tree_refs.is_empty() {
        let reachable = match integrated {
            Some(rev) => gitio::reachable_objects(&ctx.cwd, rev)?,
            None => Default::default(),
        };
        for (name, oid) in tree_refs {
            if reachable.contains(&oid) {
                let _ = gitio::delete_ref(&ctx.cwd, &name);
            }
        }
    }
    Ok(())
}
