//! Gate verification keeps observed process evidence distinct from attestation.
//! Executed gates run in a process group, retain private combined diagnostics
//! and a bounded output tail, and honor an optional timeout; attested gates carry only the
//! externally supplied result. Declared gates may run concurrently, but their
//! evidence is appended afterward in deterministic gate-name order.

use super::*;
mod together;
use crate::integration::{
    self, Decision, IntegrationFacts, IntegrationPlan, Refusal, TargetCheckout,
};
use crate::operation_println as println;
use crate::process_group::join_before_deadline;
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::process::{ExitStatus, Stdio};
use std::sync::mpsc::{self, TryRecvError};
pub fn check_selection(
    ctx: &Ctx,
    reference: Option<&str>,
    tags: Vec<String>,
    explain: bool,
    json: bool,
    shape: Option<integration::Shape>,
) -> Result<i32> {
    match (reference, tags.is_empty()) {
        (Some(reference), true) => check(ctx, reference, explain, json, shape),
        (None, false) => check_tagged(ctx, normalize_tags(tags)?, shape),
        (Some(_), false) => bail!("provide a change or --tag, not both"),
        (None, true) => bail!("provide a change or at least one --tag"),
    }
}

#[derive(serde::Serialize)]
struct CheckOutput<'a> {
    gates: &'a [crate::status::GateStatus],
    budget_raises: &'a [crate::gate_raise::Applicability],
    budget_errors: &'a [String],
    budget_rules: &'a Option<crate::gate_budget::BudgetRules>,
    #[serde(skip_serializing_if = "Option::is_none")]
    review_context: Option<&'a crate::review_context::ReviewContext>,
    verdict_assessments: &'a [crate::status::VerdictAssessment],
    selected_authorizing_event: Option<&'a str>,
    schema: &'static str,
    change_id: &'a str,
    ready: bool,
    exit_code: i32,
    blockers: Vec<CheckBlocker>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changelog_none: Option<&'a str>,
    /// What a lead should know and arc will not refuse for. Never affects
    /// `ready` or the exit code; a change may legitimately ship with one
    /// reviewer, an orchestrator's review is a valid review unless a
    /// project's policy says otherwise, and contribution trailers, missing
    /// or carrying a placeholder or a session id, never block integration.
    #[serde(skip_serializing_if = "<[crate::status::Advisory]>::is_empty")]
    advisories: &'a [crate::status::Advisory],
    /// Historical bypass judgements, by gate name; these grant no authority.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    bypassed: BTreeMap<&'a str, &'a crate::state::BypassEntry>,
    /// Reads that would discharge the change's review debt but for their
    /// extent, each listed on its own beside the obligation.
    #[serde(skip_serializing_if = "<[crate::state::PartialCoverage]>::is_empty")]
    debt_partial_coverage: &'a [crate::state::PartialCoverage],
}

#[derive(serde::Serialize)]
struct CheckBlocker {
    blocker: &'static str,
    exit_code: i32,
}

pub struct VerifyArgs {
    pub event: Option<GateEvent>,
    pub all: bool,
    pub parallel: bool,
    /// Run declared gates even where a pass at this tree would answer.
    pub rerun: bool,
    pub gate: Option<String>,
    pub command: Option<String>,
    pub probe: Option<String>,
    pub brief_version: Option<usize>,
    pub probe_phase: Option<ProbePhase>,
    pub attest: bool,
    /// Record the attestation as a receipt for an integrated change's
    /// shipped tree rather than as gate evidence.
    pub after_integration: bool,
    pub result: Option<VerifyResult>,
    pub tested_revision: Option<String>,
    pub execution_host: Option<String>,
    pub runner: Option<String>,
    pub environment: Option<String>,
    pub note: Option<String>,
    pub waive_dirty: Option<String>,
    pub falsified_by: Option<String>,
    pub predicted: Option<String>,
    pub against: Option<String>,
    pub bypass: Option<String>,
    /// The landing attempt a whole-set run gates, recorded on its evidence.
    pub landing: Option<String>,
}

struct VerificationInput {
    timings: Option<String>,
    declaration: Option<String>,
    budget: Option<crate::gate_budget::BudgetJudgement>,
    exclusive: Option<crate::exclusive::Request>,
    preparation_ms: u64,
    run_id: Option<String>,
    probe: Option<ProbeEvidenceRef>,
    gate: Option<String>,
    command: String,
    timeout_seconds: Option<u64>,
    attested_result: Option<VerifyResult>,
    tested_revision: Option<String>,
    execution_host: Option<String>,
    runner: Option<String>,
    /// The environment identity the evidence carries: the digest an attested
    /// caller supplied, or the digest the gate's declared probe produced at
    /// the checkout the command ran in. `None` for a check that declares no
    /// environment probe.
    environment_identity: Option<String>,
    note: Option<String>,
    falsification: Option<Falsification>,
    /// The target head a synthesized merge was computed against, on a run
    /// evaluating that merge rather than a commit either branch holds.
    against_target: Option<String>,
    /// The landing attempt this run gates.
    landing: Option<String>,
}

struct CompletedVerification {
    declaration: Option<String>,
    run_id: Option<String>,
    probe: Option<ProbeEvidenceRef>,
    gate: Option<String>,
    command: String,
    timeout_seconds: Option<u64>,
    revision: String,
    result: VerifyResult,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    timing: crate::verification_timing::VerificationTiming,
    output_tail: Option<String>,
    output_artifact: Option<crate::verification_output::OutputArtifact>,
    timed_out: bool,
    hostname: String,
    attested: bool,
    runner: Option<String>,
    environment_identity: Option<String>,
    note: Option<String>,
    falsification: Option<Falsification>,
    tested_tree: Option<String>,
    worktree_dirty: Option<bool>,
    worktree_dirty_tracked: Option<bool>,
    worktree_dirty_untracked: Option<bool>,
    tree_moved: bool,
    against_target: Option<String>,
    landing: Option<String>,
}

pub fn verify(ctx: &Ctx, reference: &str, args: VerifyArgs) -> Result<i32> {
    verify_with_landing(ctx, reference, args, None)
}

/// Verify a stack against the destination and gate union pinned by its plan.
pub(crate) fn verify_landing_stack(
    ctx: &Ctx,
    reference: &str,
    args: VerifyArgs,
    target: &str,
    gates: &[(String, gates::Gate)],
) -> Result<i32> {
    verify_with_landing(ctx, reference, args, Some((target, gates)))
}

fn verify_with_landing(
    ctx: &Ctx,
    reference: &str,
    args: VerifyArgs,
    landing_gates: Option<(&str, &[(String, gates::Gate)])>,
) -> Result<i32> {
    let VerifyArgs {
        event,
        all,
        parallel,
        rerun,
        gate,
        command,
        probe,
        brief_version,
        probe_phase,
        attest,
        after_integration,
        result,
        tested_revision,
        execution_host,
        runner,
        environment,
        note,
        waive_dirty,
        falsified_by,
        predicted,
        against,
        bypass,
        landing,
    } = args;
    if bypass.is_some() {
        bail!("{}", gates::BYPASS_RETIRED);
    }
    // A receipt is a check somebody else ran against what shipped. Arc has
    // nothing left to run it in, and the flags that select gate sets, excuse
    // dirt, or answer a failure all describe evidence a decision can count.
    if after_integration {
        if !attest {
            bail!(
                "--after-integration records a check arc did not run, so it requires --attest \
                 with --result, --tested-revision, --execution-host, and --runner"
            );
        }
        if probe.is_some() || waive_dirty.is_some() || falsified_by.is_some() {
            bail!(
                "--after-integration records one gate or command, so it cannot be combined \
                 with --probe, --waive-dirty, or --falsified-by"
            );
        }
    }
    if event.is_some() && !all && against.is_none() {
        bail!("--event requires --all or --against");
    }
    let event = event.unwrap_or_default();
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
    if all && (falsified_by.is_some() || predicted.is_some()) {
        bail!(
            "--falsified-by and --predicted cannot be combined with --all: a falsification \
             answers one check, and a batch runs several"
        );
    }
    // The reference without the reason records that something failed and not
    // why it was expected to; the reason without the reference is an
    // unsupported claim. Neither half is evidence on its own.
    let falsified_by = match (falsified_by, predicted) {
        (Some(event_id), Some(reason)) => {
            let reason = reason.trim().to_string();
            if reason.is_empty() {
                bail!("--predicted must state why the check was expected to fail");
            }
            Some((event_id, reason))
        }
        (Some(_), None) => bail!("--falsified-by requires --predicted <reason>"),
        (None, Some(_)) => bail!("--predicted requires --falsified-by <event-id>"),
        (None, None) => None,
    };
    // Evaluating a merge is a whole-gate-set question about content that is
    // on no branch, so the flags naming one check, asserting a result arc did
    // not observe, or describing this worktree have nothing to select or
    // excuse there.
    if against.is_some()
        && (gate.is_some()
            || command.is_some()
            || probe.is_some()
            || attest
            || parallel
            || waive_dirty.is_some()
            || falsified_by.is_some())
    {
        bail!(
            "--against runs the required gate set on a synthesized merge, so it cannot \
             be combined with --gate, --command, --probe, --attest, --parallel, \
             --waive-dirty, or --falsified-by"
        );
    }
    if parallel && !all {
        bail!("--parallel requires --all");
    }
    // A parallel batch records cleanliness as unknown, and a waiver excuses
    // only dirt somebody observed. Accepting the pair would record a waiver
    // that can never make anything green, which reads as permission granted.
    if parallel && waive_dirty.is_some() {
        bail!(
            "--waive-dirty cannot be combined with --parallel: a parallel batch \
             records worktree cleanliness as unknown, and a waiver excuses \
             observed dirt rather than a tree nobody could see. Run the gates \
             sequentially to waive."
        );
    }
    // Only a declared gate arc runs itself has a pass to reuse.
    if rerun && (attest || command.is_some() || probe.is_some()) {
        bail!(
            "--rerun forces a declared gate to run where a pass would be reused, so it cannot \
             be combined with --attest, --command, or --probe"
        );
    }
    let reuse = !rerun;
    if probe.is_some() && (gate.is_some() || command.is_some()) {
        bail!("--probe is mutually exclusive with --gate and --command");
    }
    if probe.is_none() && (brief_version.is_some() || probe_phase.is_some()) {
        bail!("--brief-version and --probe-phase require --probe");
    }
    // --attest records evidence arc did not observe (so it needs the caller's
    // --result); without it arc runs the command and observing --result is a bug.
    if attest {
        let missing: Vec<&str> = [
            (result.is_none(), "--result pass|fail"),
            (tested_revision.is_none(), "--tested-revision <REV>"),
            (execution_host.is_none(), "--execution-host <VALUE>"),
            (runner.is_none(), "--runner <VALUE>"),
        ]
        .into_iter()
        .filter_map(|(absent, flag)| absent.then_some(flag))
        .collect();
        if !missing.is_empty() {
            bail!("--attest requires {}", missing.join(", "));
        }
    }
    let attested_result = match (attest, result) {
        (true, Some(result)) => Some(result),
        (true, None) => unreachable!("checked with the other attestation fields"),
        (false, Some(_)) => bail!("--result is only valid with --attest"),
        (false, None) => None,
    };
    // A locally observed run learns its environment from the gate's declared
    // probe; only an attested run has an environment arc cannot observe.
    if environment.is_some() && !attest {
        bail!("--environment is only valid with --attest");
    }
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
    let (change_id, mut st) = ctx.load_state(&store, reference)?;
    if let Some((target, _)) = landing_gates {
        st.target_branch = target.to_string();
    }
    if after_integration {
        return verify_after_integration(
            ctx,
            &store,
            &change_id,
            &st,
            Receipt {
                gate,
                command,
                result: attested_result.context("--after-integration requires --result")?,
                tested_revision: tested_revision
                    .context("--after-integration requires --tested-revision")?,
                execution_host: execution_host
                    .context("--after-integration requires --execution-host")?,
                runner: runner.context("--after-integration requires --runner")?,
                environment,
                note,
            },
        );
    }
    refuse_late_attestation(&st, attest)?;
    // Gates and final probes run where the change lives. Baseline probes
    // choose between the invoking checkout and a scratch checkout at the base.
    // Attestation and merge evidence have no recorded worktree to choose.
    let redirected = if attest
        || against.is_some()
        || (probe.is_some() && probe_phase == Some(ProbePhase::Baseline))
    {
        None
    } else {
        gate_run_ctx(ctx, &st)?
    };
    let run_ctx = redirected.as_ref().unwrap_or(ctx);
    // Which gates are required is read where `arc status` reads it, from the
    // change's target and the change's own branch, so what a run discharges
    // and what status still owes cannot disagree. Only the tree the command
    // reads follows the change.
    // Declared before anything runs, so the evidence this invocation records
    // is judged under the waiver rather than needing a second pass to excuse
    // it. It names the head it was declared at — the head of the checkout the
    // gate runs in, which is the tree whose dirt it excuses — and that is the
    // only revision it covers.
    if waive_dirty.is_some()
        && !attest
        && probe.is_some()
        && probe_phase == Some(ProbePhase::Baseline)
    {
        let brief = match brief_version {
            Some(0) => bail!("brief version 0 not found"),
            Some(version) => st
                .briefs
                .get(version - 1)
                .with_context(|| format!("brief version {version} not found"))?,
            None => st
                .latest_brief()
                .context("no brief recorded for acceptance probe")?,
        };
        let base = brief
            .base_revision
            .as_deref()
            .context("legacy brief has no base revision for baseline probe evidence")?;
        if gitio::head(&run_ctx.cwd)? != base {
            bail!("--waive-dirty cannot be combined with a scratch baseline: the scratch checkout is clean and the waiver would bind to a revision the probe does not test");
        }
    }
    let st = if let Some(reason) = waive_dirty.as_deref() {
        let reason = reason.trim();
        if reason.is_empty() {
            bail!(
                "--waive-dirty must say why dirty evidence should count; an empty reason waives the gate without recording a reason"
            );
        }
        let revision = gitio::head(&run_ctx.cwd)?;
        let mut ev = ctx.event(
            &store,
            &change_id,
            Payload::DirtyTreeWaived {
                reason: reason.to_string(),
                revision: revision.clone(),
            },
        )?;
        ensure_append_allowed(&st, &ev.payload)?;
        store.append_local_event(&mut ev)?;
        println!(
            "dirty-tree waived at {}: {reason}",
            &revision[..revision.len().min(8)]
        );
        store.state(&change_id)?
    } else {
        st
    };
    if let Some(probe_name) = probe {
        if environment.is_some() {
            bail!("--environment applies only to a declared gate with an environment probe");
        }
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
        let mut baseline_base = None;
        if phase == ProbePhase::Baseline {
            let base = brief
                .base_revision
                .as_deref()
                .context("legacy brief has no base revision for baseline probe evidence")?;
            baseline_base = Some(base);
            if let Some(tested_revision) = &tested_revision {
                if tested_revision != base {
                    bail!(
                        "attested baseline probe requires --tested-revision {base}, got {tested_revision}"
                    );
                }
            }
        }
        // Baseline evidence belongs at the brief's base; final evidence
        // belongs at the change head.
        if phase_counts_at_head(phase) {
            match &tested_revision {
                Some(revision) => warn_if_attested_off_head(ctx, &st, revision),
                None => ensure_at_change_head(run_ctx, &st)?,
            }
        }
        let expected = match phase {
            ProbePhase::Baseline => VerifyResult::Fail,
            ProbePhase::Final => VerifyResult::Pass,
        };
        let falsification =
            resolve_falsification(&store, &change_id, None, &declaration.command, falsified_by)?;
        let scratch = match baseline_base {
            Some(base) if !attest && gitio::head(&run_ctx.cwd)? != base => Some(
                create_baseline_checkout(ctx, &st.slug, base, &probe_name, version)?,
            ),
            _ => None,
        };
        let command_cwd = scratch.as_deref().unwrap_or(&run_ctx.cwd);
        let result = record_verification_at(
            ctx,
            command_cwd,
            &store,
            &change_id,
            VerificationInput {
                timings: None,
                declaration: None,
                budget: None,
                exclusive: None,
                preparation_ms: 0,
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
                environment_identity: None,
                note,
                falsification,
                against_target: None,
                landing: None,
            },
        );
        if let Some(path) = scratch {
            if let Err(error) = gitio::remove_worktree(&ctx.cwd, &path, true) {
                eprintln!(
                    "warning: cannot remove baseline scratch checkout {}: {error:#}; remove it with: git worktree remove --force {}",
                    path.display(),
                    crate::context::shell_quote(&path.display().to_string())
                );
            }
        }
        let code = result?;
        let observed = if code == 0 {
            VerifyResult::Pass
        } else {
            VerifyResult::Fail
        };
        return Ok(if observed == expected { 0 } else { 1 });
    }
    // Evidence for a merge is counted at its tree rather than at the change's
    // head, so the head check below is not the question this path answers.
    if let Some(target) = against {
        return verify_against(ctx, &store, &change_id, &st, &target, note, reuse, event);
    }
    // The remaining paths record evidence for the selected event at the
    // change's head.
    match &tested_revision {
        Some(revision) => warn_if_attested_off_head(ctx, &st, revision),
        None => ensure_at_change_head(run_ctx, &st)?,
    }
    if all {
        let gates = if let Some((_, planned)) = landing_gates {
            gates::GatesFile {
                gates: planned.iter().cloned().collect(),
                ..Default::default()
            }
        } else {
            ctx.declarations(&st)?.gates
        };
        let required = gates.required_for_event(&st.profile, event);
        if required.is_empty() {
            bail!("no gates declared for profile {}", st.profile);
        }
        if gates.has_budgets() {
            store.adopt_budgets()?;
        }
        let preparing = Instant::now();
        crate::warm_worktree::prepare(&run_ctx.cwd)?;
        let mut preparation_ms = preparing.elapsed().as_millis() as u64;
        let total = required.len();
        let head = gitio::head(&run_ctx.cwd)?;
        let tree = gitio::commit_tree(&run_ctx.cwd, &head)?;
        let mode = if parallel {
            VerificationRunMode::Parallel
        } else {
            together::mode(&required)
        };
        let run_id = start_verification_run_for_event(
            run_ctx, &store, &change_id, &head, mode, reuse, &required, event,
        )?;
        // Applicability is a property of this checkout, so every declared
        // probe is read before reuse is decided. A probe is run once per
        // distinct command, however many gates declare it.
        let environments = gate_environments(
            &run_ctx.cwd,
            required.iter().map(|(name, gate)| (name.as_str(), *gate)),
        )?;
        let Partition {
            budget_refused,
            mut reused,
            delta_reuses,
            base_reuses,
            to_run,
        } = partition(
            run_ctx,
            &st,
            required,
            Answering {
                tree: &tree,
                head: &head,
                label: "green at head",
                against_target: None,
            },
            &environments,
            event,
            reuse,
        );
        append_base_reuses(ctx, &store, &change_id, &run_id, &head, &tree, &base_reuses)?;
        reused.extend(base_reuses.iter().map(|(name, witness)| {
            (
                name.clone(),
                crate::base_pass::execution(witness).event_id.clone(),
            )
        }));
        append_reuses(
            ctx,
            &store,
            &change_id,
            &run_id,
            &head,
            &tree,
            &reused
                .iter()
                .filter(|(name, _)| !base_reuses.iter().any(|(base, _)| base == name))
                .cloned()
                .collect::<Vec<_>>(),
        )?;
        append_delta_reuses(
            ctx,
            &store,
            &change_id,
            &run_id,
            &head,
            &tree,
            &delta_reuses,
        )?;
        reused.extend(
            delta_reuses
                .iter()
                .map(|(name, source, _)| (name.clone(), source.clone())),
        );
        if parallel {
            return verify_all_parallel(
                run_ctx,
                &store,
                &change_id,
                to_run,
                total,
                reused.len() - budget_refused,
                &run_id,
                &head,
                &environments,
                preparation_ms,
                note,
                landing,
            );
        }
        let passed = reused.len() - budget_refused
            + together::run_required(run_ctx, &store, &change_id, to_run, |name, gate| {
                VerificationInput {
                    timings: gate.timings.clone(),
                    budget: Some(gate.budget_judgement(None)),
                    exclusive: crate::exclusive::Request::for_gate(gate),
                    preparation_ms: std::mem::take(&mut preparation_ms),
                    run_id: Some(run_id.clone()),
                    probe: None,
                    gate: Some(name.clone()),
                    command: gate.command.clone(),
                    timeout_seconds: gate.timeout,
                    declaration: gates::coverage_identity(gate),
                    attested_result: None,
                    tested_revision: None,
                    execution_host: None,
                    runner: None,
                    environment_identity: declared_environment(&environments, gate)
                        .map(str::to_owned),
                    note: note.clone(),
                    falsification: None,
                    against_target: None,
                    landing: landing.clone(),
                }
            })?;
        println!("gates: {passed}/{total} pass");
        return Ok(if passed == total { 0 } else { 1 });
    }
    let DeclaredCheck {
        command: cmd,
        timeout,
        environment: declared_environment,
        exclusive,
        declaration,
        budget,
        adopt_budget_format,
        profiled_gate,
        timings,
    } = declared_check(ctx, &st, gate.as_deref(), command)?;
    if adopt_budget_format {
        store.adopt_budgets()?;
    }
    if ctx
        .declarations(&st)?
        .gates
        .gates
        .values()
        .any(|gate| gate.concurrent)
    {
        store.adopt_concurrency()?;
    }
    let preparing = Instant::now();
    if !attest {
        crate::warm_worktree::prepare(&run_ctx.cwd)?;
    }
    let preparation_ms = if attest {
        0
    } else {
        preparing.elapsed().as_millis() as u64
    };
    let environment_identity = match (attest, &declared_environment) {
        (true, declared) => attested_environment(gate.as_deref(), declared.is_some(), environment)?,
        // A run arc performs reads the identity from the gate's declared probe.
        (false, Some(probe)) => {
            let run = gates::environment_probe(&run_ctx.cwd, probe, timeout)?;
            if let Some(failure) = &run.failure {
                eprintln!(
                    "warning: environment probe {probe:?} for gate {:?} yielded no identity: {}",
                    gate.as_deref().unwrap_or_default(),
                    failure.describe()
                );
            }
            run.identity
        }
        (false, None) => None,
    };
    // A falsification is a claim about the run it names, so it always runs.
    if let Some(name) = gate
        .as_deref()
        .filter(|_| reuse && !attest && falsified_by.is_none())
    {
        if let Some(code) = reuse_named_gate(
            run_ctx,
            &store,
            &change_id,
            &st,
            name,
            environment_identity.as_deref(),
        )? {
            return Ok(code);
        }
    }
    let falsification =
        resolve_falsification(&store, &change_id, gate.as_deref(), &cmd, falsified_by)?;
    let run_id = if !attest {
        profiled_gate
            .as_ref()
            .zip(gate.as_ref())
            .map(|(declared, name)| {
                let head = gitio::head(&run_ctx.cwd)?;
                start_verification_run_for_event(
                    run_ctx,
                    &store,
                    &change_id,
                    &head,
                    VerificationRunMode::Sequential,
                    reuse,
                    &[(name, declared)],
                    event,
                )
            })
            .transpose()?
    } else {
        None
    };
    record_verification(
        run_ctx,
        &store,
        &change_id,
        VerificationInput {
            timings,
            declaration,
            budget,
            exclusive,
            preparation_ms,
            run_id,
            probe: None,
            gate,
            command: cmd,
            timeout_seconds: timeout,
            attested_result,
            tested_revision,
            execution_host,
            runner,
            environment_identity,
            note,
            falsification,
            against_target: None,
            landing: None,
        },
    )
}

/// Record reuse of a pass that already answers for one named gate at the
/// head of `run_ctx`, and say whether there was one.
///
/// The reuse is recorded as a run of that one gate, the way a whole-set run
/// records each gate it does not run.
fn reuse_named_gate(
    run_ctx: &Ctx,
    store: &Store,
    change_id: &str,
    st: &ChangeState,
    name: &str,
    environment: Option<&str>,
) -> Result<Option<i32>> {
    let declarations = run_ctx.declarations(st)?.gates;
    let Some((name, gate)) = declarations.gates.get_key_value(name) else {
        return Ok(None);
    };
    let head = gitio::head(&run_ctx.cwd)?;
    let tree = gitio::commit_tree(&run_ctx.cwd, &head)?;
    if gate.coverage.base_passes && crate::base_pass::unobserved(st, name) {
        if let Some(source) = crate::base_pass::sources(&run_ctx.cwd, store, st, &head)
            .and_then(|sources| sources.for_gate(name).cloned())
        {
            let paths = gates::delta_paths(&run_ctx.cwd, &source.baseline.tree, &tree);
            let clean = gitio::git(
                &run_ctx.cwd,
                &["status", "--porcelain=v1", "--untracked-files=normal"],
            )?
            .is_empty();
            if let Some(witness) = paths.filter(|_| clean).and_then(|paths| {
                crate::base_pass::select(
                    st,
                    &source,
                    name,
                    gate,
                    &head,
                    &tree,
                    None,
                    environment,
                    &paths,
                )
            }) {
                let source_execution = crate::base_pass::execution(&witness);
                let timing = &source_execution.timing;
                let hostname = &source_execution.hostname;
                let judgement = gate.budget_judgement(timing.run_ms);
                if let Some(reason) = judgement.reason(timing, hostname) {
                    println!("gate {name}: {reason}; rerun explicitly with --rerun");
                }
                let run_id = start_verification_run(
                    run_ctx,
                    store,
                    change_id,
                    &head,
                    VerificationRunMode::Sequential,
                    true,
                    &[(name, gate)],
                )?;
                println!(
                    "gate {name}: base pass from {} at {}",
                    witness.baseline_event_id, witness.baseline.revision
                );
                append_base_reuses(
                    run_ctx,
                    store,
                    change_id,
                    &run_id,
                    &head,
                    &tree,
                    &[(name.clone(), witness)],
                )?;
                return Ok(Some(if judgement.accepted() { 0 } else { 1 }));
            }
        }
    }
    let legacy_trees = status::legacy_evidence_trees(st, &run_ctx.cwd);
    let resolve_tree = |revision: &str| legacy_trees.get(revision).cloned();
    let label = "green at head";
    let evidence = reusable_evidence_for_event(
        &run_ctx.cwd,
        st,
        name,
        gate,
        &Answering {
            tree: &tree,
            head: &head,
            label,
            against_target: None,
        },
        &resolve_tree,
        environment,
        GateEvent::Integration,
    );
    let Some(evidence) = evidence else {
        let declared_names = declarations.gates.keys().cloned().collect::<Vec<_>>();
        let mut deltas = BTreeMap::new();
        let Some((source, delta)) = incremental_evidence(
            run_ctx,
            st,
            name,
            gate,
            &tree,
            &declared_names,
            &mut deltas,
            environment,
            GateEvent::Integration,
        ) else {
            return Ok(None);
        };
        let source_evidence = st
            .verifications
            .iter()
            .find(|e| e.event_id == source)
            .context("selected reuse source is missing")?;
        let judgement = gate.budget_judgement(source_evidence.timing.run_ms);
        let run_id = start_verification_run(
            run_ctx,
            store,
            change_id,
            &head,
            VerificationRunMode::Sequential,
            true,
            &[(name, gate)],
        )?;
        println!(
            "gate {name}: skipped ({} proves unchanged scope from {})",
            if delta.selector == gates::PATH_SELECTOR {
                "arc paths"
            } else {
                "project selector"
            },
            delta.source_tree
        );
        append_delta_reuses(
            run_ctx,
            store,
            change_id,
            &run_id,
            &head,
            &tree,
            &[(name.clone(), source, delta)],
        )?;
        if let Some(reason) = judgement.reason(&source_evidence.timing, &source_evidence.hostname) {
            println!("gate {name}: {reason}; rerun explicitly with --rerun");
        }
        return Ok(Some(if judgement.accepted() { 0 } else { 1 }));
    };
    let judgement = gate.budget_judgement(evidence.timing.run_ms);
    let label = if judgement.accepted() {
        label
    } else {
        "structurally eligible receipt"
    };
    let reused = [(name.clone(), evidence.event_id.clone())];
    let run_id = start_verification_run(
        run_ctx,
        store,
        change_id,
        &head,
        VerificationRunMode::Sequential,
        true,
        &[(name, gate)],
    )?;
    println!(
        "gate {name}: skipped ({label}; declared by {})",
        gate.declared_by.join(", ")
    );
    append_reuses(run_ctx, store, change_id, &run_id, &head, &tree, &reused)?;
    if let Some(reason) = judgement.reason(&evidence.timing, &evidence.hostname) {
        println!("gate {name}: {reason}; rerun explicitly with --rerun");
    }
    Ok(Some(if judgement.accepted() { 0 } else { 1 }))
}

/// A detached checkout that lives only as long as one evaluation.
///
/// A synthesized merge is on no branch, so gating it needs a tree of its own.
/// Removal covers preparation errors and failing gates. Preparation that
/// changes source preserves the checkout for inspection.
pub(super) struct ScratchWorktree {
    repo: PathBuf,
    pub(super) path: PathBuf,
    preserve: bool,
    pub(super) preparation_ms: u64,
}

impl ScratchWorktree {
    pub(super) fn create(repo: &Path, path: PathBuf, revision: &str) -> Result<Self> {
        let mut scratch = Self::create_for_probe(repo, path, revision)?;
        let preparing = Instant::now();
        if let Err(error) = crate::warm_worktree::prepare(&scratch.path) {
            scratch.preserve = error.is::<crate::warm_worktree::SourceMutation>();
            return Err(error);
        }
        scratch.preparation_ms = preparing.elapsed().as_millis() as u64;
        Ok(scratch)
    }

    /// Environment probes observe source without running build preparation.
    pub(super) fn create_for_probe(repo: &Path, path: PathBuf, revision: &str) -> Result<Self> {
        Self::populate(repo, &path, revision)?;
        Ok(Self {
            repo: repo.to_path_buf(),
            path,
            preserve: false,
            preparation_ms: 0,
        })
    }

    fn populate(repo: &Path, path: &Path, revision: &str) -> Result<()> {
        let display = path.display().to_string();
        // A checkout an interrupted run left behind is stale by construction:
        // it holds one revision and this run is asking for another.
        let _ = gitio::git(repo, &["worktree", "remove", "--force", &display]);
        if path.exists() {
            bail!(
                "{display} exists and is not a worktree arc can remove; delete it and run this \
                 again"
            );
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        gitio::git(repo, &["worktree", "add", "--detach", &display, revision])?;
        Ok(())
    }
}

impl status::EvaluationCheckout for ScratchWorktree {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchWorktree {
    fn drop(&mut self) {
        if self.preserve {
            return;
        }
        let display = self.path.display().to_string();
        if let Err(error) = gitio::git(&self.repo, &["worktree", "remove", "--force", &display]) {
            eprintln!("warning: could not remove the scratch worktree {display} ({error:#})");
        }
    }
}

/// Run the required gates against the merge that would ship.
///
/// A change behind its target merges to content neither branch committed, so
/// nothing has been run against what would actually land. That content is
/// written as a commit, checked out on its own, gated there, and the evidence
/// is recorded against the tree — the coordinate the merge preserves and the
/// commit id does not. The target head is recorded beside it, because a merge
/// with a moved target is a different merge and this evidence is about the
/// earlier one.
///
/// `reuse` answers a gate from the evidence already recorded at that merged
/// tree instead of rerunning the gate that produced it.
#[allow(clippy::too_many_arguments)]
fn verify_against(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    st: &ChangeState,
    target: &str,
    note: Option<String>,
    reuse: bool,
    event: GateEvent,
) -> Result<i32> {
    let mut selected = st.clone();
    selected.target_branch = target.to_string();
    let st = &selected;
    let declarations = ctx.declarations(st)?.gates;
    let required = declarations.required_for_event(&st.profile, event);
    if required.is_empty() {
        bail!("no gates declared for profile {}", st.profile);
    }
    if declarations.has_budgets() {
        store.adopt_budgets()?;
    }
    let target_head = gitio::branch_head(&ctx.cwd, target)
        .with_context(|| format!("cannot resolve target {target:?}"))?;
    let change_head = gitio::branch_head(&ctx.cwd, &st.branch)?;
    let merged_tree = gitio::merge_outcome(&ctx.cwd, &target_head, &change_head)?
        .tree
        .with_context(|| {
            format!(
                "merging {} into {target} conflicts textually, so there is no single tree to \
                 evaluate; rebase first",
                st.branch
            )
        })?;
    let synthesized = gitio::commit_tree_with_parents(
        &ctx.cwd,
        &merged_tree,
        &[&target_head, &change_head],
        "arc synthesized merge",
    )?;
    // The evidence about to be recorded names this commit and nothing else
    // refers to it, so without a ref it is collectable and the record would
    // cite content that is gone.
    gitio::update_ref(
        &ctx.cwd,
        &gitio::merge_retention_ref(change_id, &synthesized),
        &synthesized,
    )?;

    println!("merged tree: {merged_tree}");
    println!("synthesized merge: {synthesized}");

    let total = required.len();
    let run_id = start_verification_run_for_event(
        ctx,
        store,
        change_id,
        &synthesized,
        together::mode(&required),
        reuse,
        &required,
        event,
    )?;
    let answering = Answering {
        tree: &merged_tree,
        head: &change_head,
        label: "green at the merged tree",
        against_target: Some(&target_head),
    };
    // A checkout exists only to run a command or a declared probe in, and a
    // probe reads the merged content: applicability is a property of the tree
    // that would ship. A declared probe therefore needs the checkout before
    // reuse can be decided; without one, a gate answered at this tree already
    // leaves nothing to run.
    let any_probe = required.iter().any(|(_, gate)| gate.environment.is_some());
    let answered_already = reuse && {
        let legacy_trees = status::legacy_evidence_trees(st, &ctx.cwd);
        let resolve_tree = |revision: &str| legacy_trees.get(revision).cloned();
        required.iter().all(|(name, gate)| {
            reusable_evidence_for_event(
                &ctx.cwd,
                st,
                name,
                gate,
                &answering,
                &resolve_tree,
                None,
                event,
            )
            .is_some()
        })
    };
    let mut _scratch = None;
    let mut scratch_ctx = None;
    if any_probe || !answered_already {
        let path = super::lifecycle::worktree_path_for(&ctx.cwd, &format!("{}-against", st.slug))?;
        _scratch = Some(ScratchWorktree::create(&ctx.cwd, path, &synthesized)?);
        scratch_ctx = _scratch
            .as_ref()
            .map(|scratch| ctx.with_cwd(scratch.path.clone()));
    }
    let probe_ctx = scratch_ctx.as_ref().unwrap_or(ctx);
    let environments = gate_environments(
        &probe_ctx.cwd,
        required.iter().map(|(name, gate)| (name.as_str(), *gate)),
    )?;
    let Partition {
        budget_refused,
        mut reused,
        delta_reuses,
        base_reuses,
        to_run,
    } = partition(
        probe_ctx,
        st,
        required,
        answering,
        &environments,
        event,
        reuse,
    );
    append_base_reuses(
        ctx,
        store,
        change_id,
        &run_id,
        &synthesized,
        &merged_tree,
        &base_reuses,
    )?;
    let base_names = base_reuses
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    reused.extend(base_reuses.iter().map(|(name, witness)| {
        (
            name.clone(),
            crate::base_pass::execution(witness).event_id.clone(),
        )
    }));
    append_reuses(
        ctx,
        store,
        change_id,
        &run_id,
        &synthesized,
        &merged_tree,
        &reused
            .iter()
            .filter(|(name, _)| !base_names.contains(name))
            .cloned()
            .collect::<Vec<_>>(),
    )?;
    append_delta_reuses(
        ctx,
        store,
        change_id,
        &run_id,
        &synthesized,
        &merged_tree,
        &delta_reuses,
    )?;
    reused.extend(
        delta_reuses
            .iter()
            .map(|(name, source, _)| (name.clone(), source.clone())),
    );

    let mut passed = reused.len() - budget_refused;
    let mut preparation_ms = _scratch
        .as_ref()
        .map_or(0, |scratch| scratch.preparation_ms);
    if !to_run.is_empty() {
        let scratch_ctx = scratch_ctx
            .as_ref()
            .context("a gate is left to run but its checkout was not created")?;
        passed += together::run_required(scratch_ctx, store, change_id, to_run, |name, gate| {
            VerificationInput {
                timings: gate.timings.clone(),
                budget: Some(gate.budget_judgement(None)),
                exclusive: crate::exclusive::Request::for_gate(gate),
                preparation_ms: std::mem::take(&mut preparation_ms),
                run_id: Some(run_id.clone()),
                probe: None,
                gate: Some(name.clone()),
                command: gate.command.clone(),
                timeout_seconds: gate.timeout,
                declaration: gates::coverage_identity(gate),
                attested_result: None,
                tested_revision: Some(synthesized.clone()),
                execution_host: None,
                runner: None,
                environment_identity: declared_environment(&environments, gate).map(str::to_owned),
                note: note.clone(),
                falsification: None,
                against_target: Some(target_head.clone()),
                landing: None,
            }
        })?;
    }
    println!("gates: {passed}/{total} pass at the merged tree");
    Ok(if passed == total { 0 } else { 1 })
}

/// The gates a composed landing runs once, at the commit it composed.
pub(crate) struct LandingGateRun<'a> {
    /// The member the run is recorded on.
    pub change_id: &'a str,
    /// That member's state, read against the landing's target.
    pub state: &'a ChangeState,
    /// Every gate a member requires, each under the one declaration all of
    /// them share.
    pub gates: &'a [(String, gates::Gate)],
    pub commit: &'a str,
    pub tree: &'a str,
    /// A checkout holding `commit`, where every gate runs.
    pub checkout: &'a Path,
    pub attempt: &'a str,
}

/// Run a composed landing's gates in its checkout, recording the evidence on
/// one member with the landing named. A pass already recorded at the
/// composed tree answers its gate under the rule every run follows: a
/// tree-bound pass by tree, a commit-bound pass only at the composed commit.
/// Returns the exit code and, per gate, the passing evidence that answers it
/// at the composed commit.
pub(crate) fn verify_landing_head(
    ctx: &Ctx,
    store: &Store,
    run: LandingGateRun<'_>,
) -> Result<(i32, BTreeMap<String, String>)> {
    let event = GateEvent::Integration;
    let required: Vec<(&String, &gates::Gate)> =
        run.gates.iter().map(|(name, gate)| (name, gate)).collect();
    let checkout_ctx = ctx.with_cwd(run.checkout.to_path_buf());
    // The evidence about to be recorded cites the composed head, which a
    // failed attempt leaves on no branch: without a pin the record would
    // cite content that is gone, and no other store could import it.
    gitio::update_ref(
        &ctx.cwd,
        &gitio::landing_retention_ref(run.change_id, run.commit),
        run.commit,
    )?;
    let run_id = start_verification_run_for_event(
        ctx,
        store,
        run.change_id,
        run.commit,
        together::mode(&required),
        true,
        &required,
        event,
    )?;
    let answering = || Answering {
        tree: run.tree,
        head: run.commit,
        label: "green at the composed head",
        against_target: None,
    };
    let environments = gate_environments(
        run.checkout,
        required.iter().map(|(name, gate)| (name.as_str(), *gate)),
    )?;
    let Partition {
        budget_refused,
        reused,
        delta_reuses,
        base_reuses,
        to_run,
    } = partition(
        &checkout_ctx,
        run.state,
        required.clone(),
        answering(),
        &environments,
        event,
        true,
    );
    append_base_reuses(
        ctx,
        store,
        run.change_id,
        &run_id,
        run.commit,
        run.tree,
        &base_reuses,
    )?;
    append_reuses(
        ctx,
        store,
        run.change_id,
        &run_id,
        run.commit,
        run.tree,
        &reused,
    )?;
    append_delta_reuses(
        ctx,
        store,
        run.change_id,
        &run_id,
        run.commit,
        run.tree,
        &delta_reuses,
    )?;
    let to_run_count = to_run.len();
    let passed =
        together::run_required(&checkout_ctx, store, run.change_id, to_run, |name, gate| {
            VerificationInput {
                timings: gate.timings.clone(),
                budget: Some(gate.budget_judgement(None)),
                exclusive: crate::exclusive::Request::for_gate(gate),
                preparation_ms: 0,
                run_id: Some(run_id.clone()),
                probe: None,
                gate: Some(name.clone()),
                command: gate.command.clone(),
                timeout_seconds: gate.timeout,
                declaration: gates::coverage_identity(gate),
                attested_result: None,
                tested_revision: Some(run.commit.to_string()),
                execution_host: None,
                runner: None,
                environment_identity: declared_environment(&environments, gate).map(str::to_owned),
                note: None,
                falsification: None,
                against_target: None,
                landing: Some(run.attempt.to_string()),
            }
        })?;
    let failed = budget_refused + to_run_count - passed;
    let state = store.state(run.change_id)?;
    let legacy_trees = status::legacy_evidence_trees(&state, &ctx.cwd);
    let resolve_tree = |revision: &str| legacy_trees.get(revision).cloned();
    let mut evidence = BTreeMap::new();
    for (name, gate) in &required {
        if let Some(found) = reusable_evidence_for_event(
            &ctx.cwd,
            &state,
            name,
            gate,
            &answering(),
            &resolve_tree,
            declared_environment(&environments, gate),
            event,
        ) {
            evidence.insert((*name).clone(), found.event_id.clone());
        }
    }
    let total = required.len();
    println!(
        "gates: {}/{total} pass at the composed head",
        total - failed.min(total)
    );
    let code = if failed == 0 && evidence.len() == total {
        0
    } else {
        1
    };
    Ok((code, evidence))
}

/// The checkout a gate run belongs in, when it is not the invoking one.
///
/// Gate evidence is counted at the change's head, so the tree a gate reads
/// has to be the change's own. A run started from another checkout of the
/// repository is redirected to the change's recorded worktree rather than
/// gating whatever that checkout happens to hold. `None` says the invoking
/// checkout is already the right one, including the case of a change that
/// records no worktree — there the head check speaks for itself.
///
/// A sandbox bounds the redirect: a recorded path outside the prefix is a
/// checkout arc will not run anything in, so it counts as none at all.
fn gate_run_ctx(ctx: &Ctx, st: &ChangeState) -> Result<Option<Ctx>> {
    let Some(recorded) = st.worktree.as_deref().map(PathBuf::from) else {
        return Ok(None);
    };
    let toplevel = gitio::toplevel(&ctx.cwd)?;
    if config::resolved(&toplevel) == config::resolved(&recorded) {
        return Ok(None);
    }
    if let Some(prefix) = ctx.excluded_by_sandbox(&recorded)? {
        let change_head = gitio::branch_head(&ctx.cwd, &st.branch)?;
        bail!(
            "{}'s recorded checkout {} lies outside the sandbox at {}, so its gates have \
             nowhere to run\n\
             tip: give it one inside the sandbox with `git worktree add {}/<name> {}`, or \
             record evidence arc did not run with `arc verify --attest --tested-revision \
             {change_head} ...`",
            st.change_id,
            recorded.display(),
            prefix.display(),
            prefix.display(),
            st.branch
        );
    }
    let checkout = gitio::toplevel(&recorded)
        .ok()
        .filter(|top| config::resolved(top) == config::resolved(&recorded));
    if checkout.is_none() {
        let change_head = gitio::branch_head(&ctx.cwd, &st.branch)?;
        bail!(
            "{}'s worktree {} is not a checkout, so its gates have nowhere to run\n\
             tip: give it one with `git worktree add {} {}`, or record evidence arc did \
             not run with `arc verify --attest --tested-revision {change_head} ...`",
            st.change_id,
            recorded.display(),
            recorded.display(),
            st.branch
        );
    }
    println!("running in {} ({})", recorded.display(), st.change_id);
    Ok(Some(ctx.with_cwd(recorded)))
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
    let recorded = st
        .worktree
        .as_deref()
        .map(std::path::Path::new)
        .filter(|recorded| config::resolved(&ctx.cwd).starts_with(config::resolved(recorded)));
    if let Some(recorded) = recorded {
        bail!(
            "{} lives in {} but that worktree's HEAD is not its branch head ({change_head}), so \
             gate evidence would be recorded where status will never count it\n\
             tip: `git -C {} checkout {}`",
            st.change_id,
            recorded.display(),
            recorded.display(),
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
    reuse: bool,
    gates: &[(&String, &gates::Gate)],
) -> Result<String> {
    start_verification_run_for_event(
        ctx,
        store,
        change_id,
        revision,
        mode,
        reuse,
        gates,
        GateEvent::Integration,
    )
}

#[allow(clippy::too_many_arguments)]
fn start_verification_run_for_event(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    revision: &str,
    mode: VerificationRunMode,
    reuse: bool,
    gates: &[(&String, &gates::Gate)],
    event: GateEvent,
) -> Result<String> {
    if gates.iter().any(|(_, gate)| gate.budget.is_some()) {
        store.adopt_budgets()?;
    }
    let _transition = store.lock_transition(change_id)?;
    let state = store.state(change_id)?;
    let payload = Payload::VerificationRunStarted {
        profile: Some(state.profile.clone()),
        event,
        revision: revision.to_owned(),
        mode,
        skip_green: reuse,
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
    let mut event = ctx.event(store, change_id, payload)?;
    store.append_local_event(&mut event)?;
    Ok(event.event_id.clone())
}

/// The content a run answers for, and what it says when a gate is reused.
struct Answering<'a> {
    tree: &'a str,
    /// The change's head, which a commit-bound gate's evidence must name.
    head: &'a str,
    label: &'a str,
    /// The target head a synthesized merge of `head` was computed against.
    against_target: Option<&'a str>,
}

/// Which of a run's gates are answered by evidence already recorded, and
/// which are left to run.
struct Partition<'a> {
    budget_refused: usize,
    reused: Vec<(String, String)>,
    delta_reuses: Vec<(String, String, DeltaSelection)>,
    base_reuses: Vec<(String, crate::base_pass::Selected)>,
    to_run: Vec<(&'a String, &'a gates::Gate)>,
}

/// Answer each gate from a pass already recorded for this content where one
/// exists, unless `reuse` is off, and print the gates that will not run.
///
/// `ctx` is the checkout holding that content: incremental reuse asks a
/// project selector about it, and a commit-bound gate's merge evidence is told
/// apart by its parents there.
fn partition<'a>(
    ctx: &Ctx,
    st: &ChangeState,
    gates: Vec<(&'a String, &'a gates::Gate)>,
    answering: Answering<'_>,
    environments: &DeclaredEnvironments,
    event: GateEvent,
    reuse: bool,
) -> Partition<'a> {
    let mut partition = Partition {
        base_reuses: Vec::new(),
        budget_refused: 0,
        reused: Vec::new(),
        delta_reuses: Vec::new(),
        to_run: Vec::new(),
    };
    if !reuse {
        partition.to_run = gates;
        return partition;
    }
    let legacy_trees = status::legacy_evidence_trees(st, &ctx.cwd);
    let resolve_tree = |revision: &str| legacy_trees.get(revision).cloned();
    let declared_names = gates
        .iter()
        .map(|(name, _)| (*name).clone())
        .collect::<Vec<_>>();
    let mut deltas = BTreeMap::new();
    let baselines = (event == GateEvent::Integration
        && gates.iter().any(|(name, gate)| {
            gate.coverage.base_passes && crate::base_pass::unobserved(st, name)
        }))
    .then(|| ctx.store().ok())
    .flatten()
    .and_then(|store| crate::base_pass::sources(&ctx.cwd, &store, st, answering.head));
    let evaluated_revision = baselines
        .as_ref()
        .filter(|sources| !sources.is_empty())
        .and_then(|_| gitio::head(&ctx.cwd).ok());
    let clean = evaluated_revision.is_some()
        && gitio::git(
            &ctx.cwd,
            &["status", "--porcelain=v1", "--untracked-files=normal"],
        )
        .is_ok_and(|text| text.is_empty());
    for (name, gate) in gates {
        let environment = declared_environment(environments, gate);
        if let Some(evidence) = reusable_evidence_for_event(
            &ctx.cwd,
            st,
            name,
            gate,
            &answering,
            &resolve_tree,
            environment,
            event,
        ) {
            let judgement = gate.budget_judgement(evidence.timing.run_ms);
            println!(
                "gate {name}: skipped ({}; declared by {})",
                if judgement.accepted() {
                    answering.label
                } else {
                    "structurally eligible receipt"
                },
                gate.declared_by.join(", ")
            );
            if let Some(reason) = judgement.reason(&evidence.timing, &evidence.hostname) {
                println!("gate {name}: {reason}; rerun explicitly with --rerun");
                partition.budget_refused += 1;
            }
            partition
                .reused
                .push((name.clone(), evidence.event_id.clone()));
        } else if let Some((source, mut delta)) = incremental_evidence(
            ctx,
            st,
            name,
            gate,
            answering.tree,
            &declared_names,
            &mut deltas,
            environment,
            event,
        ) {
            delta.against_target = answering.against_target.map(str::to_owned);
            println!(
                "gate {name}: skipped ({} proves unchanged scope from {})",
                if delta.selector == gates::PATH_SELECTOR {
                    "arc paths"
                } else {
                    "project selector"
                },
                delta.source_tree
            );
            if let Some(evidence) = st.verifications.iter().find(|e| e.event_id == source) {
                if let Some(reason) = gate
                    .budget_judgement(evidence.timing.run_ms)
                    .reason(&evidence.timing, &evidence.hostname)
                {
                    println!("gate {name}: {reason}; rerun explicitly with --rerun");
                    partition.budget_refused += 1;
                }
            }
            partition.delta_reuses.push((name.clone(), source, delta));
        } else if let Some(witness) = baselines
            .as_ref()
            .filter(|_| crate::base_pass::unobserved(st, name))
            .and_then(|sources| sources.for_gate(name))
            .filter(|_| clean)
            .and_then(|source| {
                let paths = deltas
                    .entry(source.baseline.tree.clone())
                    .or_insert_with(|| {
                        gates::delta_paths(&ctx.cwd, &source.baseline.tree, answering.tree)
                    })
                    .as_deref()?;
                crate::base_pass::select(
                    st,
                    source,
                    name,
                    gate,
                    evaluated_revision.as_deref()?,
                    answering.tree,
                    answering.against_target,
                    environment,
                    paths,
                )
            })
        {
            let source_execution = crate::base_pass::execution(&witness);
            let timing = &source_execution.timing;
            let hostname = &source_execution.hostname;
            if let Some(reason) = gate
                .budget_judgement(timing.run_ms)
                .reason(timing, hostname)
            {
                println!("gate {name}: {reason}; rerun explicitly with --rerun");
                partition.budget_refused += 1;
            }
            println!(
                "gate {name}: base pass from {} at {} (executed {})",
                witness.baseline_event_id,
                witness.baseline.revision,
                crate::base_pass::execution(&witness).created_at
            );
            partition.base_reuses.push((name.clone(), witness));
        } else {
            partition.to_run.push((name, gate));
        }
    }
    partition
}

/// The passing evidence a run reuses for `name` at the content it answers
/// for, if any.
///
/// Reuse is keyed the way readiness is: by tree, so a gate a run skips is a
/// gate status already counts as green whichever commit the run that answered
/// was against, and by commit as well for a gate bound to commits. Reuse is
/// reuse of a *run*: the recorded command must be the one declared now, and
/// evidence from another environment ran, but not here.
/// Only the newest evidence that applies is a candidate, because it is the
/// one readiness counts; an older pass never stands in for it. The reuse
/// event names the tree, and replay checks it against the content key the
/// evidence itself carries, so newest evidence keyed only by a revision is
/// rerun.
#[allow(clippy::too_many_arguments)]
fn reusable_evidence_for_event<'a>(
    cwd: &Path,
    st: &'a ChangeState,
    name: &str,
    gate: &gates::Gate,
    answering: &Answering<'_>,
    resolve_tree: &dyn Fn(&str) -> Option<String>,
    environment: Option<&str>,
    event: GateEvent,
) -> Option<&'a state::VerificationEntry> {
    let tree = answering.tree;
    st.gate_evidence_at_tree_matching(name, tree, resolve_tree, |evidence| {
        state::verification_event(st, evidence) == event
            && status::answers_for_commit(evidence, gate, answering.head, Some(cwd))
            && status::matches_declaration(evidence, gate)
            && status::matches_environment(evidence, gate, environment)
    })
    .filter(|evidence| {
        evidence.recorded_tree() == Some(tree)
            && evidence.green_at_head(st.eligible_dirty_tree_waiver())
    })
}

#[allow(clippy::too_many_arguments)]
fn incremental_evidence(
    ctx: &Ctx,
    st: &ChangeState,
    name: &str,
    gate: &gates::Gate,
    tree: &str,
    declared_names: &[String],
    deltas: &mut BTreeMap<String, Option<Vec<String>>>,
    environment: Option<&str>,
    event: GateEvent,
) -> Option<(String, DeltaSelection)> {
    // A delta is a difference between trees, which says nothing about the
    // commits a commit-bound gate reads.
    if event != GateEvent::Integration || gate.binds == gates::Binding::Commit {
        return None;
    }
    if gate.selector.is_none() && !gate.coverage.enabled() {
        return None;
    }
    let head = gitio::head(&ctx.cwd).ok()?;
    if gitio::commit_tree(&ctx.cwd, &head).ok()?.as_str() != tree
        || !gitio::git(
            &ctx.cwd,
            &["status", "--porcelain=v1", "--untracked-files=normal"],
        )
        .ok()?
        .is_empty()
    {
        return None;
    }
    let source = st.verifications.iter().rev().find(|evidence| {
        evidence.gate.as_deref() == Some(name)
            && state::verification_event(st, evidence) == event
            && status::matches_declaration(evidence, gate)
    })?;
    if !source.green_at_head(None) || !status::matches_environment(source, gate, environment) {
        return None;
    }
    let source_tree = source.recorded_tree()?;
    if source_tree == tree {
        return None;
    }
    let paths = deltas
        .entry(source_tree.to_owned())
        .or_insert_with(|| gates::delta_paths(&ctx.cwd, source_tree, tree))
        .as_deref()?;
    let delta = gates::select_delta(
        &ctx.cwd,
        gate,
        name,
        source_tree,
        tree,
        declared_names,
        paths,
    )?;
    if gitio::head(&ctx.cwd).ok()? != head
        || !gitio::git(
            &ctx.cwd,
            &["status", "--porcelain=v1", "--untracked-files=normal"],
        )
        .ok()?
        .is_empty()
    {
        return None;
    }
    Some((source.event_id.clone(), delta))
}

#[allow(clippy::too_many_arguments)]
fn append_base_reuses(
    ctx: &Ctx,
    store: &Store,
    change: &str,
    run: &str,
    revision: &str,
    tree: &str,
    reuses: &[(String, crate::base_pass::Selected)],
) -> Result<()> {
    if reuses.is_empty() {
        return Ok(());
    }
    store.adopt_base_passes()?;
    #[cfg(debug_assertions)]
    crate::import_authorization::test_rendezvous("base-pass-before-append")?;
    let _transition = store.lock_transition(change)?;
    let state = store.state(change)?;
    if reuses
        .iter()
        .any(|(gate, _)| !crate::base_pass::unobserved(&state, gate))
    {
        bail!("base pass cannot displace another verification observation; run gates with --rerun");
    }
    let mut checked = BTreeSet::new();
    let mut pinned = BTreeSet::new();
    for (gate, witness) in reuses {
        if checked.insert(witness.git_subject()) {
            crate::base_pass::validate_git(&ctx.cwd, witness, &witness.baseline)?;
        }
        for commit in std::iter::once(witness.baseline.revision.as_str()).chain(
            witness
                .baseline
                .gates
                .values()
                .map(|entry| entry.execution.revision.as_str()),
        ) {
            if pinned.insert((witness.baseline_event_id.clone(), commit)) {
                gitio::update_ref(
                    &ctx.cwd,
                    &gitio::retained_ref(change, &witness.baseline_event_id, commit),
                    commit,
                )?;
            }
        }
        let payload = Payload::VerificationBaseReused {
            run_id: run.into(),
            gate: gate.clone(),
            revision: revision.into(),
            tree: tree.into(),
            witness: Box::new(witness.witness.clone()),
        };
        let mut event = ctx.event(store, change, payload)?;
        store.append_local_event(&mut event)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_delta_reuses(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    run_id: &str,
    revision: &str,
    tree: &str,
    reused: &[(String, String, DeltaSelection)],
) -> Result<()> {
    if reused.is_empty() {
        return Ok(());
    }
    let _transition = store.lock_transition(change_id)?;
    let state = store.state(change_id)?;
    for (gate, source, delta) in reused {
        let payload = Payload::VerificationReused {
            run_id: run_id.to_owned(),
            gate: gate.clone(),
            revision: revision.to_owned(),
            tree: Some(tree.to_owned()),
            evidence_event_id: source.clone(),
            delta: Some(delta.clone()),
        };
        ensure_append_allowed(&state, &payload)?;
        let mut event = ctx.event(store, change_id, payload)?;
        store.append_local_event(&mut event)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_reuses(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    run_id: &str,
    revision: &str,
    tree: &str,
    reused: &[(String, String)],
) -> Result<()> {
    if reused.is_empty() {
        return Ok(());
    }
    let _transition = store.lock_transition(change_id)?;
    let events = store.load_events(change_id)?;
    let mut state = store.project_state(&events, &store.rewrites()?)?;
    crate::replica::localize_change(&store.repository_id, &events, &mut state);
    for (gate, evidence_event_id) in reused {
        let payload = Payload::VerificationReused {
            run_id: run_id.to_owned(),
            gate: gate.clone(),
            revision: revision.to_owned(),
            tree: Some(tree.to_owned()),
            evidence_event_id: evidence_event_id.clone(),
            delta: None,
        };
        ensure_append_allowed(&state, &payload)?;
        let mut event = ctx.event(store, change_id, payload)?;
        store.append_local_event(&mut event)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn snapshot_with_verify(
    ctx: &Ctx,
    reference: &str,
    base: Option<String>,
    brief_version: Option<usize>,
    verify_requested: bool,
    gates: Vec<String>,
    all: bool,
    rerun: bool,
    contributors: Option<Vec<String>>,
    solo: bool,
    journal_refs: Vec<String>,
    thread: Option<String>,
) -> Result<i32> {
    if !verify_requested && (!gates.is_empty() || all || rerun) {
        bail!("--gate, --all and --rerun require --verify");
    }
    if all && !gates.is_empty() {
        bail!("--all cannot be combined with --gate");
    }
    if !gates.is_empty() {
        let unique = gates.iter().collect::<BTreeSet<_>>();
        if unique.len() != gates.len() {
            bail!("--gate values must be unique within one verification run");
        }
        let store = ctx.store()?;
        let (_, st) = ctx.load_state(&store, reference)?;
        let declarations = ctx.declarations(&st)?.gates;
        for name in &gates {
            if !declarations.gates.contains_key(name) {
                bail!("gate {name:?} not declared in .arc/gates.toml");
            }
        }
    }
    super::review::snapshot(
        ctx,
        reference,
        base,
        brief_version,
        contributors,
        solo,
        journal_refs,
        thread,
    )?;
    verify_recorded(ctx, reference, verify_requested, gates, all, rerun)
}

/// The verification `snapshot --verify` runs once the patchset is recorded:
/// every required gate, or the named ones, each answered by a pass already
/// recorded for this tree unless `rerun` is set.
fn verify_recorded(
    ctx: &Ctx,
    reference: &str,
    verify_requested: bool,
    gates: Vec<String>,
    all: bool,
    rerun: bool,
) -> Result<i32> {
    if !verify_requested {
        return Ok(0);
    }
    if all || gates.is_empty() {
        return verify(
            ctx,
            reference,
            VerifyArgs {
                event: None,
                all: true,
                parallel: false,
                rerun,
                gate: None,
                command: None,
                probe: None,
                brief_version: None,
                probe_phase: None,
                attest: false,
                after_integration: false,
                result: None,
                tested_revision: None,
                execution_host: None,
                runner: None,
                environment: None,
                note: None,
                waive_dirty: None,
                falsified_by: None,
                predicted: None,
                against: None,
                bypass: None,
                landing: None,
            },
        );
    }
    if gates.len() > 1 {
        let unique = gates.iter().collect::<BTreeSet<_>>();
        if unique.len() != gates.len() {
            bail!("--gate values must be unique within one verification run");
        }
        let store = ctx.store()?;
        let (change_id, st) = ctx.load_state(&store, reference)?;
        let redirected = gate_run_ctx(ctx, &st)?;
        let run_ctx = redirected.as_ref().unwrap_or(ctx);
        let declarations = ctx.declarations(&st)?.gates;
        let selected = gates
            .iter()
            .map(|name| {
                declarations
                    .gates
                    .get_key_value(name)
                    .with_context(|| format!("gate {name:?} not declared in .arc/gates.toml"))
            })
            .collect::<Result<Vec<_>>>()?;
        if declarations.has_budgets() {
            store.adopt_budgets()?;
        }
        let revision = gitio::head(&run_ctx.cwd)?;
        let tree = gitio::commit_tree(&run_ctx.cwd, &revision)?;
        let run_id = start_verification_run(
            run_ctx,
            &store,
            &change_id,
            &revision,
            VerificationRunMode::Sequential,
            !rerun,
            &selected,
        )?;
        let environments = gate_environments(
            &run_ctx.cwd,
            selected.iter().map(|(name, gate)| (name.as_str(), *gate)),
        )?;
        let total = selected.len();
        let Partition {
            budget_refused,
            reused,
            delta_reuses,
            base_reuses,
            to_run,
        } = partition(
            run_ctx,
            &st,
            selected,
            Answering {
                tree: &tree,
                head: &revision,
                label: "green at head",
                against_target: None,
            },
            &environments,
            GateEvent::Integration,
            !rerun,
        );
        append_base_reuses(
            ctx,
            &store,
            &change_id,
            &run_id,
            &revision,
            &tree,
            &base_reuses,
        )?;
        append_reuses(ctx, &store, &change_id, &run_id, &revision, &tree, &reused)?;
        append_delta_reuses(
            ctx,
            &store,
            &change_id,
            &run_id,
            &revision,
            &tree,
            &delta_reuses,
        )?;
        let mut passed = reused.len() + delta_reuses.len() + base_reuses.len() - budget_refused;
        for (name, gate) in to_run {
            let code = record_verification(
                run_ctx,
                &store,
                &change_id,
                VerificationInput {
                    timings: gate.timings.clone(),
                    budget: Some(gate.budget_judgement(None)),
                    exclusive: crate::exclusive::Request::for_gate(gate),
                    preparation_ms: 0,
                    run_id: Some(run_id.clone()),
                    probe: None,
                    gate: Some(name.clone()),
                    command: gate.command.clone(),
                    timeout_seconds: gate.timeout,
                    declaration: gates::coverage_identity(gate),
                    attested_result: None,
                    tested_revision: None,
                    execution_host: None,
                    runner: None,
                    environment_identity: declared_environment(&environments, gate)
                        .map(str::to_owned),
                    note: None,
                    falsification: None,
                    against_target: None,
                    landing: None,
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
                event: None,
                all: false,
                parallel: false,
                rerun,
                gate: Some(gate),
                command: None,
                probe: None,
                brief_version: None,
                probe_phase: None,
                attest: false,
                after_integration: false,
                result: None,
                tested_revision: None,
                execution_host: None,
                runner: None,
                environment: None,
                note: None,
                waive_dirty: None,
                falsified_by: None,
                predicted: None,
                against: None,
                bypass: None,
                landing: None,
            },
        )?;
        if code == 0 {
            passed += 1;
        }
    }
    println!("gates: {passed}/{total} pass");
    Ok(if passed == total { 0 } else { 1 })
}

pub fn done(
    ctx: &Ctx,
    reference: &str,
    contributors: Option<Vec<String>>,
    solo: bool,
    journal_refs: Vec<String>,
    thread: Option<String>,
) -> Result<i32> {
    // An input the snapshot would refuse refuses before the claim's stage
    // moves.
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let request = super::review::SnapshotRequest::validate(
        ctx,
        &store,
        &change_id,
        contributors,
        solo,
        &journal_refs,
        thread,
    )?;
    if super::claims::owns_live_claim(ctx, reference)? {
        let code = super::claims::stage(ctx, reference, StageArg::Verifying, None, None, false)?;
        if code != 0 {
            return Ok(code);
        }
    }
    super::review::record_snapshot(ctx, &store, &change_id, None, None, request)?;
    // A profile with no declared gate has nothing to run. Reporting the check
    // state is still the whole point of `done`, and the state says plainly
    // that nothing was evaluated, so a green never stands in for a gate
    // nobody declared.
    let (_, st) = ctx.load_state(&store, reference)?;
    let declarations = ctx.declarations(&st)?.gates;
    let required = declarations.required_for(&st.profile);
    if required.is_empty() {
        println!(
            "no gates declared for profile {}; nothing was run",
            st.profile
        );
    } else {
        let _ = verify(
            ctx,
            reference,
            VerifyArgs {
                event: None,
                all: true,
                parallel: false,
                rerun: false,
                gate: None,
                command: None,
                probe: None,
                brief_version: None,
                probe_phase: None,
                attest: false,
                after_integration: false,
                result: None,
                tested_revision: None,
                execution_host: None,
                runner: None,
                environment: None,
                note: None,
                waive_dirty: None,
                falsified_by: None,
                predicted: None,
                against: None,
                bypass: None,
                landing: None,
            },
        )?;
    }
    check(ctx, reference, false, false, None)
}

/// Replay a change's branch onto its target branch and record the result.
///
/// Where Git can, arc performs the replay itself with `git replay` in an
/// isolated repository and reads the new tip from the ref that replay
/// updated, so a replay is mechanical because arc produced it, not because
/// anything in the project afterwards says one happened. A `git rebase`
/// fallback that completes is ordinary work.
///
/// The replay runs in the worktree that holds the branch: its index and state
/// files are the only place a stopped rebase can be continued from. A conflict
/// stops the rebase and leaves it in progress, because the partial resolution
/// is a person's work and aborting would discard it.
pub fn rebase(
    ctx: &Ctx,
    reference: &str,
    verify_requested: bool,
    contributors: Option<Vec<String>>,
    solo: bool,
) -> Result<i32> {
    let started_at = chrono::Utc::now();
    let requested_contributors = super::review::contributor_declaration(ctx, contributors, solo)?;
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    // Held from the claim check until the replayed head is recorded: a claim
    // taken while the replay runs would otherwise refuse the patchset after
    // the branch has moved.
    let transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;
    if st.is_closed() {
        eprintln!("{change_id} is closed; its branch has nothing left to replay");
        return Ok(status::Blocker::Closed.exit_code());
    }
    let wt = replay_worktree(ctx, &st)?;
    // Both refusals describe the same worktree, and a stopped rebase is also
    // dirty, so the more specific state is named first or the advice is wrong.
    if gitio::rebase_in_progress(&wt)? {
        let worktree = wt.display();
        eprintln!(
            "{change_id} is already mid-rebase in {worktree}; finish it with \
             `git -C {worktree} rebase --continue` or abandon it with `--abort`"
        );
        return Ok(status::Blocker::NeedsRebase.exit_code());
    }
    if !gitio::is_clean(&wt)? {
        let dirt = gitio::dirt(&wt)?;
        eprintln!(
            "worktree {} carries {}; commit the work in progress or copy it aside before rebasing, or the replay has uncommitted work to reconcile",
            wt.display(),
            describe_dirt(dirt)
        );
        return Ok(status::Blocker::NeedsRebase.exit_code());
    }

    let target_head = gitio::branch_head(&ctx.cwd, &st.target_branch)?;
    let head = gitio::branch_head(&ctx.cwd, &st.branch)?;
    // Recorded authorship and run evidence are established at the original
    // head, before mechanical replay changes the commits' timestamps.
    let replay_of = st
        .latest_patchset()
        .filter(|patchset| patchset.head == head);
    if gitio::is_ancestor(&ctx.cwd, &target_head, &head)? {
        println!(
            "{} already carries {} at {target_head}; nothing to replay",
            st.branch, st.target_branch
        );
        if st
            .latest_patchset()
            .is_none_or(|patchset| patchset.head != head)
        {
            println!("head {head} is not recorded; record it with `arc snapshot {change_id}`");
        }
        return Ok(0);
    }
    // The replayed head is recorded as a patchset, so a claim that would
    // refuse that recording refuses the rebase while the branch is untouched.
    super::review::ensure_attribution_over_claim(
        ctx,
        &st,
        requested_contributors.is_some(),
        chrono::Utc::now(),
    )?;

    let replay_attribution =
        if replay_of.is_none() && requested_contributors.is_none() && ctx.on_behalf_of.is_none() {
            let (base, _) = super::review::patchset_base(ctx, &st, &head)?;
            super::snapshot_attribution::from_runs(
                ctx,
                &store,
                &st,
                &store.load_events(&change_id)?,
                &base,
                &head,
            )
            .ok()
            .flatten()
        } else {
            None
        };

    let _slot_use = crate::slots::exclusive_in(&wt)?;
    let retained = crate::retention::replayed(&ctx.cwd, &store, &change_id, &target_head, &head)?;
    let (retentions, _) = crate::retention::creations(&ctx.cwd, &retained)?;
    gitio::update_refs(&ctx.cwd, &retentions)
        .context("cannot hold the cited commits the rebase replaces; nothing was replayed")?;
    for created in &retentions {
        println!("retained: {} {}", created.name, created.new);
    }
    match gitio::replay(&wt, &st.branch, &target_head, &head)? {
        gitio::ReplayOutcome::Stopped => {
            let conflicts = gitio::unmerged_paths(&wt)?;
            println!(
                "rebase of {} onto {} stopped on a conflict; it is left in progress",
                st.branch, st.target_branch
            );
            for path in &conflicts {
                println!("  - {path}");
            }
            let worktree = wt.display();
            println!("resolve each file, then:");
            println!("  git -C {worktree} add <path>");
            println!("  git -C {worktree} rebase --continue");
            println!("  arc snapshot {change_id}            # record the replayed head");
            println!("  arc snapshot {change_id} --verify   # and run every required gate");
            Ok(status::Blocker::NeedsRebase.exit_code())
        }
        outcome => {
            // A replay arc performed names its own tip; a fallback rebase is
            // read from the branch it moved like any other snapshot.
            let replayed = match &outcome {
                gitio::ReplayOutcome::Replayed { tip } => tip.clone(),
                _ => gitio::branch_head(&ctx.cwd, &st.branch)?,
            };
            println!(
                "rebased {} onto {} at {replayed}",
                st.branch, st.target_branch
            );
            crate::journal::auto_log(
                ctx,
                &st.slug,
                &format!(
                    "rebased {change_id} onto {} at {replayed}",
                    st.target_branch
                ),
            );
            let request = match (&outcome, replay_of, replay_attribution) {
                (gitio::ReplayOutcome::Replayed { tip }, Some(patchset), _) => {
                    super::review::SnapshotRequest::replayed(
                        patchset.id.clone(),
                        tip.clone(),
                        requested_contributors,
                    )
                }
                (gitio::ReplayOutcome::Replayed { tip }, None, Some(attribution)) => {
                    super::review::SnapshotRequest::replayed_runs(attribution, tip.clone())
                }
                _ => super::review::SnapshotRequest::attributed(requested_contributors),
            }
            .rebased_at(started_at);
            let patchset_id = super::review::snapshot_holding(
                ctx,
                &store,
                &change_id,
                &transition,
                None,
                None,
                request,
            )?;
            // Verification takes the lock for each run it records.
            drop(transition);
            let code = verify_recorded(ctx, reference, verify_requested, Vec::new(), false, false)?;
            // The gates the replayed head owes are advice about a rebase that
            // already stands, so a failure to evaluate them is a warning and
            // never the rebase's result.
            match ctx
                .load_state(&store, reference)
                .and_then(|(_, replayed_state)| ctx.report(&store, &replayed_state))
            {
                Ok(report) => print!("{}", render::gates_owed(&report)),
                Err(error) => eprintln!(
                    "warning: {patchset_id} is recorded at {replayed}; the gates it owes could not be evaluated: {error:#}"
                ),
            }
            Ok(code)
        }
    }
}

/// The worktree a change's branch is replayed in.
///
/// A checked-out branch names its worktree, and that lookup is authoritative
/// because it reads Git rather than the ledger. It answers nothing while a
/// rebase is stopped part-way, though: Git detaches HEAD for the replay, so
/// the branch belongs to no worktree until the rebase ends, and only the
/// recorded path still says where the state a person continues from lives.
///
/// A sandbox bounds the recorded fallback. Git's own answer is about this
/// repository and needs no bound; a recorded path is a statement about where
/// a checkout was, and one outside the prefix names a tree a sandboxed replay
/// must not rewrite.
fn replay_worktree(ctx: &Ctx, st: &ChangeState) -> Result<PathBuf> {
    if let Some(checked_out) = gitio::worktree_for_branch(&ctx.cwd, &st.branch)? {
        return Ok(checked_out);
    }
    let recorded = st.worktree.as_deref().map(PathBuf::from);
    let Some(recorded) = recorded.filter(|path| path.is_dir()) else {
        bail!(
            "no worktree has {:?} checked out; check it out before rebasing",
            st.branch
        );
    };
    if let Some(prefix) = ctx.excluded_by_sandbox(&recorded)? {
        bail!(
            "{}'s recorded checkout {} lies outside the sandbox at {}, so there is nowhere to \
             replay {:?}\n\
             tip: give it a checkout inside the sandbox with `git worktree add {}/<name> {}`",
            st.change_id,
            recorded.display(),
            prefix.display(),
            st.branch,
            prefix.display(),
            st.branch
        );
    }
    Ok(recorded)
}

/// What a worktree carries, in the terms the refusal needs.
fn describe_dirt(dirt: gitio::Dirt) -> &'static str {
    match (dirt.tracked, dirt.untracked) {
        (true, true) => "uncommitted changes and untracked files",
        (true, false) => "uncommitted changes",
        (false, true) => "untracked files",
        (false, false) => "changes Git reports but does not classify",
    }
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
    environments: &DeclaredEnvironments,
    preparation_ms: u64,
    note: Option<String>,
    landing: Option<String>,
) -> Result<i32> {
    if required.iter().any(|(_, gate)| gate.budget.is_some()) {
        store.adopt_budgets()?;
    }
    let hostname = super::local_hostname();
    let cwd = ctx.cwd.clone();
    // Every gate shares this worktree, so capture the batch boundary once on
    // each side. A gate that changes the worktree makes every result in the
    // batch describe no single tree; recording that conservatively is better
    // than allowing one passing result to look reproducible.
    let before = gitio::worktree_tree(&ctx.cwd)?;
    let inputs = required
        .into_iter()
        .enumerate()
        .map(|(index, (name, gate))| VerificationInput {
            timings: gate.timings.clone(),
            budget: Some(gate.budget_judgement(None)),
            exclusive: crate::exclusive::Request::for_gate(gate),
            preparation_ms: if index == 0 { preparation_ms } else { 0 },
            run_id: Some(run_id.to_owned()),
            probe: None,
            gate: Some(name.clone()),
            command: gate.command.clone(),
            timeout_seconds: gate.timeout,
            declaration: gates::coverage_identity(gate),
            attested_result: None,
            tested_revision: None,
            execution_host: None,
            runner: None,
            environment_identity: declared_environment(environments, gate).map(str::to_owned),
            note: note.clone(),
            falsification: None,
            against_target: None,
            landing: landing.clone(),
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
            let output_root = store.root.clone();
            let producing_store = store.repository_id.clone();
            let change_id = change_id.to_owned();
            thread::spawn(move || {
                execute_verification(
                    input,
                    &cwd,
                    revision,
                    hostname,
                    &output_root,
                    &producing_store,
                    &change_id,
                )
            })
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
    let current_state = store.state(change_id)?;
    let current = ctx.declarations(&current_state)?;
    if current.gates.has_budgets() {
        store.adopt_budgets()?;
    }
    let ran_passed = completed
        .iter()
        .filter(|item| {
            let judged = item
                .gate
                .as_ref()
                .and_then(|name| current.gates.gates.get(name))
                .map(|g| {
                    let mut judgement = g.budget_judgement(item.timing.run_ms);
                    let _ = crate::gate_raise::fill_advice(
                        &ctx.cwd,
                        &current_state,
                        item.gate.as_deref().unwrap(),
                        &mut judgement,
                    );
                    judgement
                });
            let budget = judged.as_ref().or(item.timing.budget_at_run.as_ref());
            if let Some(reason) = budget.and_then(|b| b.reason(&item.timing, &item.hostname)) {
                println!(
                    "gate {}: {reason}",
                    item.gate.as_deref().unwrap_or("command")
                );
            }
            item.result == VerifyResult::Pass && budget.is_none_or(|b| b.accepted())
        })
        .count();
    append_verifications(ctx, store, change_id, completed)?;
    let passed = ran_passed + skipped_green;
    println!("gates: {passed}/{total} pass");
    Ok(if passed == total { 0 } else { 1 })
}

/// Turn `--falsified-by`/`--predicted` into the reference that will be
/// recorded, or refuse.
///
/// The reference binds to the failure's original recorded revision. Resolve
/// it before running the command; append replays against the locked ledger.
fn resolve_falsification(
    store: &Store,
    change_id: &str,
    gate: Option<&str>,
    command: &str,
    falsified_by: Option<(String, String)>,
) -> Result<Option<Falsification>> {
    let Some((event_id, predicted_reason)) = falsified_by else {
        return Ok(None);
    };
    let state = state::reduce_with_repository(
        &store.load_events(change_id)?,
        &store.rewrites()?,
        &store.repository_events()?,
    )?;
    let revision = state
        .verifications
        .iter()
        .find(|entry| entry.event_id == event_id)
        .map(|entry| entry.revision.clone())
        .unwrap_or_default();
    let falsification = Falsification {
        event_id,
        revision,
        predicted_reason,
    };
    match state::falsification_mismatch(&state.verifications, gate, command, &falsification) {
        Some(mismatch) => bail!("--falsified-by {mismatch}"),
        None => Ok(Some(falsification)),
    }
}

/// The environment identity a gate's declared probe yields, when it declares
/// one and the probe answered. A cached `None` is a probe that could not
/// answer; it is not rerun for every gate that shares it.
pub(super) type DeclaredEnvironments = BTreeMap<(String, Option<u64>), Option<String>>;

pub(super) fn declared_environment<'a>(
    identities: &'a DeclaredEnvironments,
    gate: &gates::Gate,
) -> Option<&'a str> {
    gate.environment
        .as_deref()
        .and_then(|probe| identities.get(&(probe.to_owned(), gate.timeout)))
        .and_then(|identity| identity.as_deref())
}

/// Read every distinct declared probe once, keyed by its command and the
/// bound it runs under.
///
/// Gates sharing one probe describe one environment, so the command runs once;
/// the identities are read before any gate runs, so a probe that cannot answer
/// is reported before the batch records anything, and its gates then record
/// evidence that names no environment.
pub(super) fn gate_environments<'a>(
    cwd: &Path,
    gates: impl Iterator<Item = (&'a str, &'a gates::Gate)>,
) -> Result<DeclaredEnvironments> {
    let mut identities = DeclaredEnvironments::new();
    for (name, gate) in gates {
        let Some(probe) = gate.environment.as_deref() else {
            continue;
        };
        let key = (probe.to_owned(), gate.timeout);
        if identities.contains_key(&key) {
            continue;
        }
        let run = gates::environment_probe(cwd, probe, gate.timeout)?;
        if let Some(failure) = &run.failure {
            eprintln!(
                "warning: environment probe {probe:?} for gate {name:?} yielded no identity: {}",
                failure.describe()
            );
        }
        identities.insert(key, run.identity);
    }
    Ok(identities)
}

fn create_baseline_checkout(
    ctx: &Ctx,
    slug: &str,
    base: &str,
    probe: &str,
    brief_version: usize,
) -> Result<PathBuf> {
    let name = format!(
        "baseline-{slug}-{}-{}",
        &base[..base.len().min(8)],
        ulid::Ulid::new()
    );
    let chosen = super::lifecycle::worktree_path_for(&ctx.cwd, &name)?;
    let absolute = config::resolved(&gitio::toplevel(&ctx.cwd)?.join(chosen));
    let mut path = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                path.pop();
            }
            component => path.push(component.as_os_str()),
        }
    }
    let quoted_path = crate::context::shell_quote(&path.display().to_string());
    let mut allocation_attempted = false;
    let creation = (|| -> Result<()> {
        if let Some(prefix) = ctx.excluded_by_sandbox(&path)? {
            bail!(
                "{} lies outside the sandbox at {}",
                path.display(),
                prefix.display()
            );
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        crate::worktree_usage::report_root_free(&path);
        if path.exists() {
            bail!("scratch path {} already exists", path.display());
        }
        allocation_attempted = true;
        gitio::git(
            &ctx.cwd,
            &[
                "worktree",
                "add",
                "--detach",
                path.to_str().context("non-UTF8 worktree path")?,
                base,
            ],
        )?;
        Ok(())
    })();
    if let Err(error) = creation {
        if allocation_attempted {
            let mut cleanup_command = format!("git worktree remove --force --force {quoted_path}");
            let cleanup = (|| -> Result<()> {
                let registered = gitio::worktree_inventory(&ctx.cwd)?
                    .iter()
                    .any(|entry| entry.path == path);
                if registered {
                    gitio::remove_worktree(&ctx.cwd, &path, true)?;
                } else if path.exists() {
                    cleanup_command = format!("rm -rf -- {quoted_path}");
                    std::fs::remove_dir_all(&path)
                        .with_context(|| format!("cannot remove {}", path.display()))?;
                }
                let registrations = gitio::common_dir(&ctx.cwd)?.join("worktrees");
                if registrations.is_dir() {
                    for entry in std::fs::read_dir(&registrations)? {
                        let entry = entry?;
                        if !entry.file_type()?.is_dir() {
                            continue;
                        }
                        let admin = entry.path();
                        let gitdir = match std::fs::read_to_string(admin.join("gitdir")) {
                            Ok(gitdir) => gitdir,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                            Err(error) => return Err(error.into()),
                        };
                        if Path::new(gitdir.trim()) == path.join(".git") {
                            cleanup_command = format!(
                                "rm -rf -- {}",
                                crate::context::shell_quote(&admin.display().to_string())
                            );
                            std::fs::remove_dir_all(&admin)
                                .with_context(|| format!("cannot remove {}", admin.display()))?;
                        }
                    }
                }
                cleanup_command = format!("git worktree remove --force --force {quoted_path}");
                if gitio::worktree_inventory(&ctx.cwd)?
                    .iter()
                    .any(|entry| entry.path == path)
                {
                    bail!("worktree registration remains for {}", path.display());
                }
                Ok(())
            })();
            if let Err(cleanup) = cleanup {
                bail!(
                    "cannot create baseline scratch checkout {}: {error:#}
allocation retained: {cleanup:#}
cleanup: {cleanup_command}",
                    path.display()
                );
            }
        }
        bail!(
            "cannot create baseline scratch checkout {}: {error:#}
manual recipe:
git worktree add --detach {quoted_path} {base}
(cd {quoted_path} && arc verify {} --probe {} --brief-version {brief_version} --probe-phase baseline)
git worktree remove --force {quoted_path}",
            path.display(),
            crate::context::shell_quote(slug),
            crate::context::shell_quote(probe)
        );
    }
    eprintln!("baseline scratch checkout: {}", path.display());
    Ok(path)
}

fn record_verification(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    input: VerificationInput,
) -> Result<i32> {
    record_verification_at(ctx, &ctx.cwd, store, change_id, input)
}

fn record_verification_at(
    ctx: &Ctx,
    command_cwd: &Path,
    store: &Store,
    change_id: &str,
    input: VerificationInput,
) -> Result<i32> {
    if input.budget.as_ref().is_some_and(|b| b.budget_ms.is_some()) {
        store.adopt_budgets()?;
    }
    let revision = match &input.tested_revision {
        Some(revision) => revision.clone(),
        None => gitio::head(command_cwd)?,
    };
    let hostname = match &input.execution_host {
        Some(hostname) => hostname.clone(),
        None => super::local_hostname(),
    };
    if input.attested_result.is_none() {
        eprintln!("running: {}", input.command);
    }
    // What the command is about to run against, captured before it runs. arc
    // cannot know the tree a remote runner used, so attested evidence records
    // no tree rather than the local one, which would be a guess.
    let before = if input.attested_result.is_none() {
        Some(gitio::worktree_tree(command_cwd)?)
    } else {
        None
    };
    let mut completed = execute_verification(
        input,
        command_cwd,
        revision.clone(),
        hostname,
        &store.root,
        &store.repository_id,
        change_id,
    )?;
    if !completed.attested {
        let post = gitio::head(command_cwd)?;
        if post != revision {
            eprintln!(
                "warning: head moved during verification ({revision} -> {post}); \
                 evidence recorded at {revision}"
            );
        }
        if let Some(before) = before {
            let after = gitio::worktree_tree(command_cwd)?;
            let commit_tree = gitio::commit_tree(command_cwd, &revision).ok();
            completed.worktree_dirty = commit_tree.map(|tree| tree != before);
            // Which kind of dirt, so the waiver reason is self-evident and the
            // frequency premise is measurable rather than remembered.
            if let Ok(dirt) = gitio::dirt(command_cwd) {
                completed.worktree_dirty_tracked = Some(dirt.tracked);
                completed.worktree_dirty_untracked = Some(dirt.untracked);
            }
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
    let mut budget = completed.timing.budget_at_run.clone();
    if completed.landing.is_none() {
        if let Some(name) = completed.gate.as_deref() {
            let current_state = store.state(change_id)?;
            let current = ctx.declarations(&current_state)?;
            if current.gates.has_budgets() {
                store.adopt_budgets()?;
            }
            if let Some(gate) = current.gates.gates.get(name) {
                let mut judgement = gate.budget_judgement(completed.timing.run_ms);
                let _ =
                    crate::gate_raise::fill_advice(&ctx.cwd, &current_state, name, &mut judgement);
                budget = Some(judgement);
            }
        }
    }
    let accepted = budget.as_ref().is_none_or(|b| b.accepted());
    if let Some(reason) = budget.and_then(|b| b.reason(&completed.timing, &completed.hostname)) {
        println!(
            "gate {}: {reason}",
            completed.gate.as_deref().unwrap_or("command")
        );
    }
    let result = completed.result;
    append_verifications(ctx, store, change_id, vec![completed])?;
    Ok(if result == VerifyResult::Pass && accepted {
        0
    } else {
        1
    })
}

fn execute_verification(
    input: VerificationInput,
    cwd: &Path,
    revision: String,
    hostname: String,
    output_root: &Path,
    producing_store: &str,
    change_id: &str,
) -> Result<CompletedVerification> {
    let VerificationInput {
        timings,
        declaration,
        budget,
        exclusive,
        preparation_ms,
        run_id,
        probe,
        gate,
        command,
        timeout_seconds,
        attested_result,
        tested_revision: _,
        execution_host: _,
        runner,
        environment_identity,
        note,
        falsification,
        against_target,
        landing,
    } = input;
    let attested = attested_result.is_some();
    let mut timing = crate::verification_timing::VerificationTiming::default();
    let (execution_limit_ms, origin) = match timeout_seconds {
        Some(seconds) => (
            Some(seconds.checked_mul(1000).context("gate timeout overflow")?),
            Some("timeout".to_string()),
        ),
        None => (
            budget
                .as_ref()
                .and_then(|b| b.budget_ms)
                .map(|ms| ms.checked_mul(10).context("ten-times budget overflow"))
                .transpose()?,
            budget
                .as_ref()
                .and_then(|b| b.budget_ms)
                .map(|_| "budget-times-ten".to_string()),
        ),
    };
    let (result, exit_code, duration_ms, output_tail, output_artifact, timed_out) =
        match attested_result {
            // Attested evidence has only the caller's result because arc did not
            // execute a process or observe an exit code or duration.
            Some(result) => (result, None, None, None, None, false),
            None => {
                let started = std::time::Instant::now();
                let observed = run_gate_budgeted(
                    &command,
                    timings.as_deref(),
                    cwd,
                    execution_limit_ms.map(Duration::from_millis),
                    output_root,
                    producing_store,
                    exclusive
                        .as_ref()
                        .map(|request| (request, change_id, gate.as_deref().unwrap_or("command"))),
                )?;
                let duration_ms = started.elapsed().as_millis() as u64;
                timing = observed.timing;
                timing.prepare_ms = timing.prepare_ms.map(|ms| ms + preparation_ms);
                let result = VerifyResult::observed(
                    observed.status.success(),
                    observed.status.code(),
                    observed.timed_out,
                );
                let exit_code =
                    (!observed.admission_expired).then(|| observed.status.code().unwrap_or(-1));
                (
                    result,
                    exit_code,
                    Some(duration_ms),
                    observed.output_tail,
                    observed.output_artifact,
                    observed.timed_out,
                )
            }
        };
    if !attested {
        timing.exclusive = exclusive.as_ref().map(|request| request.name.clone());
        timing.admission_ms = exclusive
            .as_ref()
            .map(|request| request.admission.as_millis() as u64);
        timing.execution_limit_ms = execution_limit_ms;
        timing.execution_limit_origin = origin;
    }
    timing.budget_at_run = budget
        .filter(|b| b.budget_ms.is_some())
        .map(|b| crate::gate_budget::judge(b.budget_ms, &b.sources, timing.run_ms));
    Ok(CompletedVerification {
        declaration,
        run_id,
        probe,
        gate,
        command,
        timeout_seconds,
        revision,
        result,
        exit_code,
        duration_ms,
        timing,
        output_tail,
        output_artifact,
        timed_out,
        hostname,
        attested,
        runner,
        environment_identity,
        note,
        falsification,
        against_target,
        landing,
        // Filled in by the caller, which is what sees the worktree on both
        // sides of the run.
        tested_tree: None,
        worktree_dirty: None,
        worktree_dirty_tracked: None,
        worktree_dirty_untracked: None,
        tree_moved: false,
    })
}

/// Record what the gates observed. Whatever could not be recorded is printed
/// as `not recorded:`, so a refusal keeps the observed result without the
/// ledger holding evidence nobody wrote.
fn append_verifications(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    completed: Vec<CompletedVerification>,
) -> Result<()> {
    let observed = completed.iter().map(observed_result).collect::<Vec<_>>();
    let mut recorded = 0;
    let appended = record_verifications(ctx, store, change_id, completed, &mut recorded);
    if appended.is_err() {
        for line in &observed[recorded..] {
            eprintln!("not recorded: {line}");
        }
    }
    appended.context("verification evidence was not recorded; rerun the gate to record it")
}

fn observed_result(item: &CompletedVerification) -> String {
    let subject = match &item.gate {
        Some(gate) => format!("gate {gate}"),
        None => format!("command {}", item.command),
    };
    let exit = item
        .exit_code
        .map(|code| format!(" (exit {code})"))
        .unwrap_or_default();
    format!("{subject}: {:?}{exit} at {}", item.result, item.revision)
}

fn record_verifications(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    completed: Vec<CompletedVerification>,
    recorded: &mut usize,
) -> Result<()> {
    // Gates are arbitrary external commands and may legitimately invoke arc.
    // Acquire the append lock only after they return, then re-check closure.
    let _transition = store.lock_transition(change_id)?;
    let _repository_events = store.lock_repository_events()?;
    let mut events = store.load_events(change_id)?;
    let rewrites = store.rewrites()?;
    let mut st = store.project_state(&events, &rewrites)?;
    crate::replica::localize_change(&store.repository_id, &events, &mut st);
    for item in completed {
        let gate_label = item.gate.clone();
        let result = item.result;
        let revision = item.revision.clone();
        let attested = item.attested;
        let budget_marker = match item.timing.budget_at_run.as_ref() {
            Some(budget)
                if result == VerifyResult::Pass
                    && budget.verdict == crate::gate_budget::BudgetVerdict::OverBudget =>
            {
                format!(
                    " (over budget, ran {} of {})",
                    crate::gate_budget::duration(budget.run_ms.expect("over-budget run")),
                    crate::gate_budget::duration(budget.budget_ms.expect("declared budget"))
                )
            }
            _ => String::new(),
        };
        let captured_tree = item.tested_tree.clone();
        if let Some(budget) = item.timing.budget_at_run.as_ref().and_then(|b| b.budget_ms) {
            let mut recent = std::iter::once(item.timing.run_ms)
                .chain(
                    st.verifications
                        .iter()
                        .rev()
                        .filter(|e| {
                            e.gate == item.gate
                                && e.delta.is_none()
                                && !e.attested
                                && e.result != VerifyResult::NotAdmitted
                                && e.command == item.command
                                && e.timeout_seconds == item.timeout_seconds
                                && e.environment.as_ref().map(|e| &e.identity)
                                    == item.environment_identity.as_ref()
                        })
                        .map(|e| e.timing.run_ms),
                )
                .flatten()
                .take(20)
                .collect::<Vec<_>>();
            recent.sort_unstable();
            if let Some(median) = recent.get(recent.len().saturating_sub(1) / 2) {
                if u128::from(*median) * 100 > u128::from(budget) * 80 {
                    eprintln!(
                        "gate {}: recent command median {} exceeds 80% of budget {}",
                        item.gate.as_deref().unwrap_or("command"),
                        crate::gate_budget::duration(*median),
                        crate::gate_budget::duration(budget)
                    );
                }
            }
        }
        let falsification_inferred = (item.result == VerifyResult::Pass && item.probe.is_none())
            .then(|| {
                state::infer_falsification(&st.verifications, item.gate.as_deref(), &item.command)
            })
            .flatten();
        let payload = Payload::VerificationRecorded {
            declaration: item.declaration,
            run_id: item.run_id,
            probe: item.probe,
            gate: item.gate,
            command: item.command,
            timeout_seconds: item.timeout_seconds,
            revision: item.revision,
            result: item.result,
            exit_code: item.exit_code,
            duration_ms: item.duration_ms,
            timing: item.timing,
            output_tail: item.output_tail,
            output_artifact: item.output_artifact,
            timed_out: item.timed_out,
            hostname: item.hostname,
            attested: item.attested,
            runner: item.runner,
            environment: item
                .environment_identity
                .map(|identity| EnvironmentEvidence {
                    identity,
                    producing_store: store.repository_id.clone(),
                }),
            note: item.note,
            falsification: item.falsification,
            falsification_inferred,
            // The content the gate holds for, written beside the revision
            // carrying it so the record still says what was evaluated once
            // that commit is unreachable. Unknown rather than guessed where
            // the revision does not resolve here, which is any attested run
            // naming a revision this repository does not have.
            tree: gitio::commit_tree(&ctx.cwd, &revision).ok(),
            against_target: item.against_target,
            landing: item.landing,
            // Set below, once the tree is pinned.
            tested_tree: None,
            worktree_dirty: item.worktree_dirty,
            worktree_dirty_tracked: item.worktree_dirty_tracked,
            worktree_dirty_untracked: item.worktree_dirty_untracked,
            tree_moved: item.tree_moved,
        };
        // Whether this event may be appended at all is settled before any ref
        // is written, so a refusal — the gate closed the change while it ran —
        // leaves no pin behind for an event that never existed.
        ensure_append_allowed(&st, &payload)?;
        let mut ev = ctx.event(store, change_id, payload)?;
        let mut candidate = events.clone();
        candidate.push(ev.preview_after(&candidate)?);
        store
            .project_state(&candidate, &rewrites)
            .context("verification would make the change unreadable; nothing was written")?;
        // A recorded `tested_tree` promises the tree is still there, so the
        // claim is made only once the pin holding it exists. A run whose tree
        // cannot be pinned is recorded without one rather than pointing at
        // something collectable.
        if let Some(tree) = &captured_tree {
            let name = gitio::tree_retention_ref(change_id, tree);
            match gitio::update_ref(&ctx.cwd, &name, tree) {
                Ok(()) => {
                    if let Payload::VerificationRecorded { tested_tree, .. } = &mut ev.payload {
                        *tested_tree = Some(tree.clone());
                    }
                }
                Err(error) => {
                    if let Payload::VerificationRecorded {
                        tested_tree,
                        worktree_dirty,
                        ..
                    } = &mut ev.payload
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
        store.append_local_event(&mut ev)?;
        *recorded += 1;
        if let Payload::VerificationRecorded {
            tested_tree: Some(tree),
            ..
        } = &ev.payload
        {
            let name = gitio::tree_retention_ref(change_id, &ev.event_id);
            match gitio::update_ref(&ctx.cwd, &name, tree) {
                Ok(()) => {
                    let prepared = gitio::tree_retention_ref(change_id, tree);
                    if let Err(error) = gitio::git(&ctx.cwd, &["update-ref", "-d", &prepared, tree])
                    {
                        eprintln!(
                            "warning: could not remove preparation pin {prepared}: {error:#}"
                        );
                    }
                }
                Err(error) => {
                    eprintln!("warning: tree {tree} remains held by its preparation pin: {error:#}")
                }
            }
        }
        events.push(ev.clone());
        if let Some(gate) = gate_label {
            let declared_by = ctx
                .declarations(&st)
                .ok()
                .and_then(|declarations| {
                    declarations
                        .gates
                        .gates
                        .get(&gate)
                        .map(|item| item.declared_by.join(", "))
                })
                .filter(|sources| !sources.is_empty())
                .unwrap_or_else(|| "declaration source unavailable".to_string());
            println!("gate: {gate} (declared by {declared_by})");
        }
        let marker = if attested { " (attested)" } else { "" };
        println!("verification: {result:?}{marker}{budget_marker} at {revision}");
        if result == VerifyResult::NotAdmitted {
            if item.exit_code.is_none() && !attested {
                println!("not admitted: exclusive admission expired before any check ran; rerun the gate");
            } else {
                println!(
                    "not admitted: the command exited {} before any check ran, so nothing was \
                     evaluated; rerun the gate",
                    crate::model::NOT_ADMITTED_EXIT
                );
            }
        }
        println!("event: {}", ev.event_id);
    }
    Ok(())
}

/// The refusal for a verify that names nothing to run, with the choices that
/// would have run something and the declared gates `--all` covers.
fn missing_selector(ctx: &Ctx, st: &state::ChangeState) -> String {
    let declared = ctx
        .declarations(st)
        .map(|declarations| declarations.gates.gates.keys().cloned().collect::<Vec<_>>());
    let gates = match declared {
        Ok(names) if names.is_empty() => "no gates are declared".to_string(),
        Ok(names) => format!("declared gates: {}", names.join(", ")),
        Err(_) => "the declared gates could not be read".to_string(),
    };
    format!(
        "nothing to run: pass --all for every declared gate, --gate <name> for one, or \
         --command <cmd> for an ad hoc check ({gates})"
    )
}

fn nonempty_attestation_value(value: Option<String>, flag: &str) -> Result<String> {
    let value = value.with_context(|| format!("--attest requires {flag} <VALUE>"))?;
    let value = value.trim();
    if value.is_empty() {
        bail!("{flag} must not be empty");
    }
    Ok(value.to_owned())
}

pub(super) struct GateRun {
    pub(super) admission_expired: bool,
    pub(super) status: ExitStatus,
    pub(super) output_tail: Option<String>,
    pub(super) output_artifact: Option<crate::verification_output::OutputArtifact>,
    pub(super) timed_out: bool,
    pub(super) timing: crate::verification_timing::VerificationTiming,
}

impl GateRun {
    fn not_admitted(wait_ms: u64) -> Self {
        use std::os::unix::process::ExitStatusExt;
        Self {
            admission_expired: true,
            status: ExitStatus::from_raw(75 << 8),
            output_tail: None,
            output_artifact: None,
            timed_out: false,
            timing: crate::verification_timing::VerificationTiming {
                wait_ms: Some(wait_ms),
                ..Default::default()
            },
        }
    }
}

pub(super) fn run_gate_budgeted(
    cmd: &str,
    timings: Option<&str>,
    cwd: &Path,
    timeout: Option<Duration>,
    output_root: &Path,
    producing_store: &str,
    admission: Option<(&crate::exclusive::Request, &str, &str)>,
) -> Result<GateRun> {
    with_check_timings(timings, cwd, || {
        run_gate_with_capture(cmd, cwd, timeout, admission, || {
            crate::verification_output::Capture::new(output_root, producing_store)
        })
    })
}

pub(super) fn measure_gate(gate: &gates::Gate, name: &str, cwd: &Path) -> Result<GateRun> {
    let request = crate::exclusive::Request::for_gate(gate);
    with_check_timings(gate.timings.as_deref(), cwd, || {
        run_gate_with_capture(
            &gate.command,
            cwd,
            gate.execution_limit()?.0.map(Duration::from_millis),
            request
                .as_ref()
                .map(|request| (request, "measurement", name)),
            crate::verification_output::Capture::transient,
        )
    })
}

fn with_check_timings(
    timings: Option<&str>,
    cwd: &Path,
    run: impl FnOnce() -> Result<GateRun>,
) -> Result<GateRun> {
    let pending = timings.map(|name| crate::check_timings::Pending::begin(cwd, name));
    let mut run = run()?;
    if !run.admission_expired {
        if let Some((pending, name)) = pending.zip(timings) {
            let receipt = pending.finish(cwd, name);
            if let Some(error) = &receipt.error {
                eprintln!("check timings {name:?}: {error}; gate outcome is unchanged");
            }
            if receipt.dropped > 0 {
                eprintln!(
                    "check timings {name:?}: retained {} entries; dropped {}",
                    receipt.checks.len(),
                    receipt.dropped
                );
            }
            run.timing.check_timings = Some(receipt);
        }
    }
    Ok(run)
}

fn run_gate_with_capture(
    cmd: &str,
    cwd: &Path,
    timeout: Option<Duration>,
    admission: Option<(&crate::exclusive::Request, &str, &str)>,
    capture: impl FnOnce() -> crate::verification_output::Capture,
) -> Result<GateRun> {
    let (shell, wait_ms) = match admission {
        Some((request, change, gate)) => {
            let (lock, wait_ms) = crate::exclusive::acquire(request, cwd, change, gate)?;
            let Some(lock) = lock else {
                return Ok(GateRun::not_admitted(wait_ms));
            };
            let (shell, slot_wait_ms) = crate::slots::measured_shell_with_lock(cwd, Some(lock))?;
            (shell, wait_ms + slot_wait_ms)
        }
        None => crate::slots::measured_shell(cwd)?,
    };
    run_gate_in_shell(shell, wait_ms, cmd, cwd, timeout, capture)
}

fn run_gate_in_shell(
    mut shell: crate::slots::Shell,
    wait_ms: u64,
    cmd: &str,
    cwd: &Path,
    timeout: Option<Duration>,
    capture: impl FnOnce() -> crate::verification_output::Capture,
) -> Result<GateRun> {
    let command = &mut shell.command;
    command
        .arg("-c")
        .arg(format!("exec 2>&1\n{cmd}"))
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let preparing = Instant::now();
    let _preparation = crate::warm_worktree::prepare_command(command, cwd)?;
    let prepare_ms = preparing.elapsed().as_millis() as u64;
    #[cfg(not(test))]
    let mut deadline = None;
    #[cfg(test)]
    let mut deadline = timeout.and_then(|limit| Instant::now().checked_add(limit));
    let _watch = crate::interruption::Watch::start().context("cannot watch for interruption")?;
    crate::interruption::check()?;
    let mut child = shell.spawn().context("failed to run gate command")?;
    let stdout = child
        .child()
        .stdout
        .take()
        .context("gate output pipe unavailable")?;
    let (reader_done_tx, reader_done_rx) = mpsc::sync_channel(1);
    let capture = capture();
    let reader = thread::spawn(move || {
        capture.read_with_notify(stdout, || {
            let _ = reader_done_tx.send(());
        })
    });

    #[cfg(debug_assertions)]
    crate::import_authorization::test_rendezvous("gate-command-spawned")?;
    let mut exited = false;
    let mut reader_done = false;
    let timed_out = loop {
        if !exited {
            exited = child
                .leader_exited()
                .context("failed to wait for gate command")?;
        }
        if deadline.is_none() {
            if let (Some(limit), Some(start_ns), Some(now_ns)) = (
                timeout,
                child.observation.started_ns,
                crate::verification_timing::monotonic_ns(),
            ) {
                let elapsed = Duration::from_nanos(now_ns.saturating_sub(start_ns));
                deadline = Some(
                    Instant::now()
                        .checked_add(limit.saturating_sub(elapsed))
                        .context("gate timeout is too large")?,
                );
            }
        }
        if !reader_done {
            reader_done = match reader_done_rx.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => true,
                Err(TryRecvError::Empty) => false,
            };
        }
        if (exited && reader_done) || crate::interruption::check().is_err() {
            break false;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break true;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let finished_ns = crate::verification_timing::monotonic_ns();
    let status = child
        .stop()
        .context("failed to stop the gate's process group")?;
    crate::interruption::check()?;

    let (output, output_artifact) = join_before_deadline(reader, deadline, "gate output reader")?
        .transpose()
        .context("failed to read gate output")?
        .map(|(output, artifact)| (output, Some(artifact)))
        .unwrap_or_default();
    let output_tail = (!output.is_empty()).then(|| String::from_utf8_lossy(&output).into_owned());
    let mut timing = child.observation.timing.clone();
    timing.wait_ms = Some(wait_ms);
    timing.prepare_ms = Some(prepare_ms);
    timing.run_ms = child
        .observation
        .started_ns
        .zip(finished_ns)
        .and_then(|(start, end)| end.checked_sub(start))
        .map(|ns| ns / 1_000_000);
    #[cfg(debug_assertions)]
    if let Ok(raw) = std::env::var("ARC_TEST_COMMAND_RUN_MS") {
        timing.run_ms = Some(raw.parse().context("invalid test command run_ms")?);
    }
    Ok(GateRun {
        admission_expired: false,
        status,
        output_tail,
        output_artifact,
        timed_out,
        timing,
    })
}

pub fn hold(ctx: &Ctx, reference: &str, reason: String) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, _transition, st) = locked_state(&store, reference)?;
    let payload = Payload::HoldSet { reason };
    ensure_append_allowed(&st, &payload)?;
    let mut ev = ctx.event(&store, &change_id, payload)?;
    store.append_local_event(&mut ev)?;
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
    let mut ev = ctx.event(
        &store,
        &change_id,
        Payload::HoldReleased {
            hold_event_id: Some(held.clone()),
            reason,
        },
    )?;
    store.append_local_event(&mut ev)?;
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

/// The review obligation an integration declares in place of a verdict
/// nobody recorded, as the caller spelled it.
pub struct DebtDeclaration {
    pub reason: String,
    pub kind: Option<DebtMissing>,
    pub read: super::audit::IneligibleInputs,
}

/// Everything an integration was asked for beyond which changes to land.
pub struct IntegrateArgs {
    pub provisional: bool,
    pub for_replica: Option<String>,
    pub for_forge: Option<String>,
    pub shape: Option<integration::Shape>,
    pub tags: Vec<String>,
    pub into: Option<String>,
    pub message: Option<String>,
    pub cleanup: Option<bool>,
    pub discard: Option<crate::style::Discard>,
    pub dry_run: bool,
    /// Print the dry run's plan as `arc-integration-plan` JSON.
    pub json: bool,
    /// A plan an earlier dry run printed, to compare the fresh decision with.
    pub expect_basis: Option<PathBuf>,
    pub debt: Option<DebtDeclaration>,
}

pub fn integrate(ctx: &Ctx, references: &[String], args: IntegrateArgs) -> Result<i32> {
    if args.provisional {
        return super::provisional::integrate_provisionally(ctx, references, args);
    }
    let IntegrateArgs {
        provisional: _,
        for_replica: _,
        for_forge: _,
        shape,
        tags,
        into,
        message,
        cleanup,
        discard,
        dry_run,
        json,
        expect_basis,
        debt,
    } = args;
    if references.is_empty() && tags.is_empty() {
        bail!("provide a change or at least one --tag");
    }
    if !references.is_empty() && !tags.is_empty() {
        bail!("provide a change or --tag, not both");
    }
    if references.len() != 1 {
        if into.is_some() {
            bail!("--into is only valid when integrating one change");
        }
        if message.is_some() {
            bail!("--message is only valid when integrating one change");
        }
        if debt.is_some() {
            bail!(
                "--debt is only valid when integrating one change: a debt reason binds to \
                 one change's patchset, and a queue is for changes that are already green or \
                 already carry their verdict"
            );
        }
        if json {
            bail!("--json is only valid when integrating one change: a plan describes one merge");
        }
        if expect_basis.is_some() {
            bail!(
                "--expect-basis is only valid when integrating one change: a basis describes \
                 one change's merge"
            );
        }
    }
    let expect_basis = expect_basis
        .map(|path| read_expected_basis(&path))
        .transpose()?;
    let authority_store = ctx.store()?;
    let _authority_lock = crate::replica::lock(&authority_store)?;
    if let Some(refusal) = crate::replica::integration_refusal(&authority_store)? {
        eprintln!("Cannot integrate: {refusal}");
        return Ok(crate::replica::INTEGRATION_AUTHORITY_EXIT_CODE);
    }
    match (references, tags.is_empty()) {
        ([reference], true) => {
            let store = ctx.store()?;
            let (_, state) = ctx.load_state(&store, reference)?;
            let target = into.as_deref().unwrap_or(&state.target_branch);
            let style = crate::style::resolved_style(
                &ctx.cwd,
                target,
                crate::style::StyleFlags {
                    shape,
                    cleanup,
                    discard,
                    ..Default::default()
                },
            )?;
            let flags = crate::style::StyleFlags {
                shape,
                cleanup,
                discard,
                ..Default::default()
            };
            let shape = style.shape();
            let cleanup = CleanupStyle::from(&style);
            if shape == integration::Shape::FfOnly && message.is_some() {
                bail!("--message names a merge commit; select --shape no-ff");
            }
            integrate_one(
                ctx,
                state,
                into,
                message,
                cleanup,
                ClosedBehavior::Refuse,
                SingleOptions {
                    shape,
                    style_flags: flags,
                    dry_run,
                    json,
                    expect_basis,
                    cleanup_checkout: None,
                    debt,
                },
            )
        }
        (many, tags_empty) => {
            if many.is_empty() && tags_empty {
                bail!("provide a change or at least one --tag");
            }
            if !many.is_empty() && !tags_empty {
                bail!("provide a change or --tag, not both");
            }
            // A queue lands several merges, and each is its own commit into
            // its own recorded target. One message cannot name them all, and
            // one destination would silently retarget members whose target is
            // not the one named.
            if into.is_some() {
                bail!("--into is only valid when integrating one change");
            }
            if message.is_some() {
                bail!("--message is only valid when integrating one change");
            }
            // A debt is a judgment about one patchset: what review it owes,
            // and why that review could not run. One reason spread over every
            // member records an obligation that binds to nothing in
            // particular, which is the opposite of what the obligation is for.
            if debt.is_some() {
                bail!(
                    "--debt is only valid when integrating one change: a debt reason binds to \
                     one change's patchset, and a queue is for changes that are already green \
                     or already carry their verdict"
                );
            }
            let selection = if many.is_empty() {
                QueueSelection::Tagged(normalize_tags(tags)?)
            } else {
                QueueSelection::Named(many.to_vec())
            };
            integrate_queue(ctx, selection, cleanup, discard, dry_run, shape)
        }
    }
}

/// How a queued run names the changes it will attempt.
enum QueueSelection {
    Named(Vec<String>),
    Tagged(Vec<String>),
}

/// What one queued change's turn produced, in the terms the summary reports.
enum QueueStep {
    Landed(String),
    Planned(String),
    Skipped(String),
    Stopped { code: i32, reason: String },
}

/// Integrate in dependency order, stopping at the first member that needs
/// intervention. Fast-forward queues refuse divergence before any replay or
/// gate run; merge queues can replay conflicted branches. Each integration
/// uses the same target and change locks as a single-change invocation.
fn integrate_queue(
    ctx: &Ctx,
    selection: QueueSelection,
    cleanup: Option<bool>,
    discard: Option<crate::style::Discard>,
    dry_run: bool,
    shape: Option<integration::Shape>,
) -> Result<i32> {
    let queue_ctx = ctx.with_cwd(gitio::primary_worktree(&ctx.cwd)?);
    let store = queue_ctx.store()?;
    let selected = match &selection {
        QueueSelection::Tagged(tags) => {
            let selected = queue_ctx
                .load_all_states(&store)?
                .into_iter()
                .filter(|(_, state)| tags.iter().all(|tag| state.tags.contains(tag)))
                .collect::<BTreeMap<_, _>>();
            if selected.is_empty() {
                bail!("no changes match tags {}", tags.join(", "));
            }
            selected
        }
        QueueSelection::Named(references) => {
            let mut selected = BTreeMap::new();
            for reference in references {
                let change_id = store.resolve_change(reference)?;
                // Two spellings of one change would make the queue attempt it
                // twice, and the second attempt would find it closed by the
                // first — a confusing way to say the request was malformed.
                if selected.contains_key(&change_id) {
                    bail!("{change_id} is named more than once in one queue");
                }
                let state = store.state(&change_id)?;
                selected.insert(change_id, state);
            }
            selected
        }
    };
    let order = dependency_order(&selected)?;

    println!("queue: {} changes in dependency order", order.len());
    let mut steps: Vec<(String, QueueStep)> = Vec::new();
    let mut stop_code = 0;
    let mut projected_targets = BTreeMap::new();
    let mut unresolved_targets = std::collections::BTreeSet::new();
    let mut projected_closures = std::collections::BTreeSet::new();
    let mut remaining = order.iter();
    for change_id in remaining.by_ref() {
        println!();
        let state = store.state(change_id)?;
        if state.is_closed() {
            if dry_run {
                println!(
                    "dry-run: would skip {change_id} ({})",
                    change_status(&state)
                );
            } else {
                println!("{change_id}: {}", change_status(&state));
            }
            steps.push((
                change_id.clone(),
                QueueStep::Skipped(change_status(&state).to_string()),
            ));
            continue;
        }
        let result = if dry_run {
            queue_dry_run(
                &queue_ctx,
                &store,
                change_id,
                crate::style::StyleFlags {
                    shape,
                    cleanup,
                    discard,
                    ..Default::default()
                },
                &mut projected_targets,
                &mut unresolved_targets,
                &mut projected_closures,
            )
        } else {
            queue_step(
                &queue_ctx,
                change_id,
                crate::style::StyleFlags {
                    shape,
                    cleanup,
                    discard,
                    ..Default::default()
                },
                &ctx.cwd,
            )
        };
        let step = result.unwrap_or_else(|error| QueueStep::Stopped {
            code: 1,
            reason: format!("{error:#}"),
        });
        let stopped = matches!(step, QueueStep::Stopped { .. });
        if let QueueStep::Stopped { code, .. } = &step {
            stop_code = *code;
        }
        steps.push((change_id.clone(), step));
        if stopped {
            break;
        }
    }
    let not_attempted = remaining.cloned().collect::<Vec<_>>();

    println!();
    println!("queue summary:");
    for (change_id, step) in &steps {
        match step {
            QueueStep::Landed(merged) => println!("  landed: {change_id} at {merged}"),
            QueueStep::Planned(target) => {
                println!("  would land: {change_id} into {target} (conditional)")
            }
            QueueStep::Skipped(reason) => println!("  skipped: {change_id} — {reason}"),
            QueueStep::Stopped { reason, .. } => println!("  stopped: {change_id} — {reason}"),
        }
    }
    for change_id in &not_attempted {
        println!("  not attempted: {change_id}");
    }
    Ok(stop_code)
}

/// One change's turn in the queue.
fn queue_budget_stop(st: &ChangeState, report: &StatusReport) -> Option<QueueStep> {
    if report.budget_errors.is_empty()
        && !report.gates.iter().any(|g| {
            g.result == VerifyResult::Pass.as_str()
                && !g.green_at_head
                && !g.budget.accepted()
                && g.evidence_event_id.is_some()
        })
    {
        return None;
    }
    eprint!("{}", render::blocker_explanation(st, report));
    Some(QueueStep::Stopped {
        code: 5,
        reason: "budget refusal; inspect declarations or rerun explicitly after fixing the cost"
            .into(),
    })
}

fn queue_step(
    ctx: &Ctx,
    change_id: &str,
    flags: crate::style::StyleFlags,
    cleanup_checkout: &Path,
) -> Result<QueueStep> {
    // Preparation and landing read append policy from the same checkout,
    // freshly for each member so earlier landings govern later writes.
    let store = ctx.store()?;
    let st = store.state(change_id)?;
    if st.is_closed() {
        println!("{change_id}: {}", change_status(&st));
        return Ok(QueueStep::Skipped(change_status(&st).to_string()));
    }
    let target_head = match gitio::branch_head(&ctx.cwd, &st.target_branch) {
        Ok(head) => head,
        Err(error) => {
            return Ok(QueueStep::Stopped {
                code: status::Blocker::TargetUnreadable.exit_code(),
                reason: format!("target branch unreadable: {error}"),
            })
        }
    };
    ctx.ensure_declared_actor(&store)?;
    ctx.ensure_target_declares_actor(&st)?;
    let policy = crate::policy::load_at(&ctx.cwd, &target_head)?;
    let style = crate::style::resolved_style_with_policy(&policy, flags.clone())?;
    integration_history_with_policy(ctx, &st, &target_head, &policy)?;
    if let (Some(contribution), Some(patchset)) = (&policy.contribution, st.latest_patchset()) {
        contribution_shape(ctx, &st.target_branch, &patchset.head, contribution.history)?;
    }
    let shape = style.shape();
    let cleanup = CleanupStyle::from(&style);
    println!("{change_id}: integrating into {}", st.target_branch);

    if shape == integration::Shape::FfOnly && ff_diverged(ctx, &st, &st.target_branch)? {
        return Ok(QueueStep::Stopped {
            code: refuse_divergence(&st, &st.target_branch, &st.target_branch),
            reason: "target diverged; rebase and fresh review or declared debt required".into(),
        });
    }
    let mut report = ctx.report_evaluating_integration(&store, &st, &st.target_branch, shape)?;
    if let Some(blocker) = report.blockers().iter().find(|blocker| {
        !matches!(
            blocker,
            status::Blocker::NeedsRebase
                | status::Blocker::GatesNotGreen
                | status::Blocker::MergedTreeUnevaluated
        )
    }) {
        eprint!("{}", render::blocker_explanation(&st, &report));
        return Ok(QueueStep::Stopped {
            code: blocker.exit_code(),
            reason: blocker.as_str().into(),
        });
    }
    if let Some(stop) = queue_budget_stop(&st, &report) {
        return Ok(stop);
    }
    if shape == integration::Shape::NoFf && report.needs_rebase {
        let code = rebase(ctx, change_id, false, None, false)?;
        if code != 0 {
            return Ok(QueueStep::Stopped {
                code,
                reason: format!("replaying onto {} needs a person", st.target_branch),
            });
        }
        let st = store.state(change_id)?;
        report = ctx.report_evaluating_integration(&store, &st, &st.target_branch, shape)?;
    }

    // Only where nothing has answered for the content that would ship: a
    // fully green change owes no run, and recording one would be evidence
    // about a question already settled.
    if report.blockers().iter().any(|blocker| {
        matches!(
            blocker,
            status::Blocker::GatesNotGreen | status::Blocker::MergedTreeUnevaluated
        )
    }) {
        let st = store.state(change_id)?;
        let target = st.target_branch.clone();
        let code = verify_against(
            ctx,
            &store,
            change_id,
            &st,
            &target,
            None,
            true,
            GateEvent::Integration,
        )?;
        let st = store.state(change_id)?;
        report = ctx.report_evaluating_integration(&store, &st, &st.target_branch, shape)?;
        if code != 0 {
            let failed = report
                .gates
                .iter()
                .filter(|gate| !gate.green_at_head)
                .map(|gate| gate.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(QueueStep::Stopped {
                code: status::Blocker::GatesNotGreen.exit_code(),
                reason: format!("gate {failed} is not green at the merged tree"),
            });
        }
    }

    let st = store.state(change_id)?;
    if !report.integrate_ready() {
        eprint!("{}", render::blocker_explanation(&st, &report));
        return Ok(QueueStep::Stopped {
            code: status::check_exit_code(&report),
            reason: report.ready_reason.clone(),
        });
    }

    let code = integrate_one(
        ctx,
        st,
        None,
        None,
        cleanup,
        ClosedBehavior::SkipTagged,
        SingleOptions {
            shape,
            style_flags: flags,
            cleanup_checkout: Some(cleanup_checkout.to_path_buf()),
            ..SingleOptions::default()
        },
    )?;
    if code != 0 {
        return Ok(QueueStep::Stopped {
            code,
            reason: "integration refused".into(),
        });
    }
    // A change can close between this turn's readiness read and the target
    // lock the merge waits on, and the guarded path reports that as a skip
    // rather than an error. The closure it left says which happened.
    let closed = store.state(change_id)?;
    match closed
        .closure
        .as_ref()
        .and_then(|c| c.integrated_commit.as_deref())
    {
        Some(merged) => Ok(QueueStep::Landed(merged.to_string())),
        None => Ok(QueueStep::Skipped(change_status(&closed).to_string())),
    }
}

/// What the queue would do to one change, reading only what is already
/// recorded. Nothing is replayed, run, merged, or written.
fn queue_dry_run(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    flags: crate::style::StyleFlags,
    projected_targets: &mut BTreeMap<String, String>,
    unresolved_targets: &mut std::collections::BTreeSet<String>,
    projected_closures: &mut std::collections::BTreeSet<String>,
) -> Result<QueueStep> {
    let mut st = store.state(change_id)?;
    if st.is_closed() {
        println!("dry-run: would skip {change_id} ({})", change_status(&st));
        return Ok(QueueStep::Skipped(change_status(&st).to_string()));
    }
    let states = store.readable_states()?;
    st.blocked_by.retain(|id| {
        !super::dependency_integrated_with_projection(id, &states, projected_closures)
    });
    if unresolved_targets.contains(&st.target_branch) {
        let report = ctx.report_for_queue(store, &st, None)?;
        if let Some(blocker) = report.blockers().iter().find(|blocker| {
            matches!(
                blocker,
                status::Blocker::Closed
                    | status::Blocker::BranchMissing
                    | status::Blocker::ForkBranch
                    | status::Blocker::Iterating
                    | status::Blocker::BlockedByChanges
                    | status::Blocker::BlockingFindings
                    | status::Blocker::HoldActive
                    | status::Blocker::NoValidApproval
                    | status::Blocker::AcceptanceProbesNotGreen
            )
        }) {
            eprint!("{}", render::blocker_explanation(&st, &report));
            if *blocker == status::Blocker::NoValidApproval {
                if let Some(verdict) = &report.verdict {
                    eprintln!(
                        "recorded review verdict: {}",
                        serde_json::to_string(&verdict.verdict)?
                    );
                }
            }
            return Ok(QueueStep::Stopped {
                code: blocker.exit_code(),
                reason: blocker.as_str().into(),
            });
        }
        if let Some(stop) = queue_budget_stop(&st, &report) {
            return Ok(stop);
        }
        println!("dry-run: would integrate {change_id} into {}; destination ancestry, merged policy, style, history and authorization are conditional on earlier merges", st.target_branch);
        projected_closures.insert(change_id.to_string());
        return Ok(QueueStep::Planned(st.target_branch));
    }
    let actual = match gitio::branch_head(&ctx.cwd, &st.target_branch) {
        Ok(head) => head,
        Err(error) => {
            return Ok(QueueStep::Stopped {
                code: status::Blocker::TargetUnreadable.exit_code(),
                reason: format!("target branch unreadable: {error}"),
            })
        }
    };
    let revision = projected_targets.get(&st.target_branch).unwrap_or(&actual);
    let target = revision.as_str();
    let policy = crate::policy::load_at(&ctx.cwd, target)?;
    let style = crate::style::resolved_style_with_policy(&policy, flags)?;
    let shape = style.shape();
    integration_history_with_policy(ctx, &st, target, &policy)?;
    // Topology and constraints follow the projected destination; review,
    // findings, holds and iteration remain facts of the observed ledger.
    let report = ctx.report_for_queue(store, &st, Some(target))?;
    advise_role_trailers(&report);
    let mut advancing = None;
    let mut conflicts = report.needs_rebase;
    let mut unresolved = true;
    if report.state == "open"
        && !st.iterating
        && report.fork.is_none()
        && policy.contribution.is_none()
    {
        if let Some(patchset) = st.latest_patchset() {
            let (ancestor, contained) = integration::ancestry(&ctx.cwd, revision, &patchset.head)?;
            if shape == integration::Shape::FfOnly && !integration::ff_eligible(ancestor, contained)
            {
                return Ok(QueueStep::Stopped {
                    code: refuse_divergence(&st, &st.target_branch, &st.target_branch),
                    reason: "target diverged; rebase and fresh review or declared debt required"
                        .into(),
                });
            }
            conflicts = gitio::merge_outcome(&ctx.cwd, revision, &patchset.head)?.conflicts;
            unresolved = conflicts || (!contained && !ancestor);
            if !contained && (shape == integration::Shape::FfOnly || ancestor && !conflicts) {
                advancing = Some(patchset.head.clone());
            }
        }
    }
    let known = report
        .blockers()
        .iter()
        .copied()
        .filter(|blocker| {
            !matches!(
                blocker,
                status::Blocker::NeedsRebase
                    | status::Blocker::GatesNotGreen
                    | status::Blocker::MergedTreeUnevaluated
            )
        })
        .collect::<Vec<_>>();
    if let Some(blocker) = known.first() {
        eprint!("{}", render::blocker_explanation(&st, &report));
        return Ok(QueueStep::Stopped {
            code: blocker.exit_code(),
            reason: blocker.as_str().into(),
        });
    }
    if let Some(stop) = queue_budget_stop(&st, &report) {
        return Ok(stop);
    }
    if shape == integration::Shape::NoFf && conflicts {
        println!(
            "dry-run: would replay {change_id} onto {} first; fresh review or debt is conditional",
            st.target_branch
        );
    }
    for gate in report.gates.iter().filter(|gate| {
        !gate.green_at_head
            || advancing.is_some() && projected_targets.contains_key(&st.target_branch)
    }) {
        println!(
            "dry-run: would run gate {} for {change_id}; landing is conditional on success",
            gate.name
        );
    }
    println!("dry-run: would integrate {change_id} into {}; conditional on fresh checkout guards, authorization and required gates", st.target_branch);
    if style.cleanup() {
        println!("dry-run: would clean up {change_id} with integration.discard = {}; checkout ownership and local content checks apply", style.discard().as_str());
    }
    if let Some(head) = advancing {
        projected_targets.insert(st.target_branch.clone(), head);
    }
    if unresolved {
        unresolved_targets.insert(st.target_branch.clone());
        println!("dry-run: subsequent members of {} have conditional destination ancestry and merged policy", st.target_branch);
    }
    projected_closures.insert(change_id.to_string());
    Ok(QueueStep::Planned(st.target_branch))
}

/// What one integration was asked for beyond the merge itself.
#[derive(Default)]
struct SingleOptions {
    cleanup_checkout: Option<PathBuf>,
    debt: Option<DebtDeclaration>,
    shape: integration::Shape,
    style_flags: crate::style::StyleFlags,
    dry_run: bool,
    json: bool,
    /// The plan an earlier dry run printed. It never changes the decision;
    /// what moved since is named beside it.
    expect_basis: Option<IntegrationPlan>,
}

/// Cleanup uses the style selected before the change is merged.
struct CleanupStyle {
    enabled: bool,
    discard: crate::style::Discard,
    disposable: Vec<crate::artifacts::Rule>,
}

impl From<&crate::style::Style> for CleanupStyle {
    fn from(style: &crate::style::Style) -> Self {
        Self {
            enabled: style.cleanup(),
            discard: style.discard(),
            disposable: style.disposable(),
        }
    }
}

/// The resolved behavior and the flags whose precedence each reading preserves.
struct IntegrationStyle {
    shape: integration::Shape,
    cleanup: CleanupStyle,
    flags: crate::style::StyleFlags,
}

impl IntegrationStyle {
    fn validate(&self, policy: &crate::policy::PolicyFile) -> Result<()> {
        let style = crate::style::resolved_style_with_policy(policy, self.flags.clone())
            .map_err(|_| Refusal::ConfigurationMoved.into_error())?;
        if style.shape() != self.shape || style.cleanup() != self.cleanup.enabled {
            return Err(Refusal::ConfigurationMoved.into_error());
        }
        Ok(())
    }
}

/// Read a plan `arc integrate --dry-run --json` printed.
fn read_expected_basis(path: &Path) -> Result<IntegrationPlan> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read expected basis {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("expected basis {} is not JSON", path.display()))?;
    let schema = value.get("schema").and_then(|schema| schema.as_str());
    if schema != Some(integration::PLAN_SCHEMA) {
        bail!(
            "expected basis {} has schema {}, not {}; produce it with \
             `arc integrate <change> --dry-run --json`",
            path.display(),
            schema.unwrap_or("(none)"),
            integration::PLAN_SCHEMA
        );
    }
    serde_json::from_value(value)
        .with_context(|| format!("expected basis {} is malformed", path.display()))
}

#[derive(Clone, Copy)]
enum ClosedBehavior {
    Refuse,
    SkipTagged,
}

/// Resolve the checkout the merge into `target` runs in.
///
/// A worktree holding the target is authoritative. When none does, the one
/// shape arc still merges in is the checkout `begin --no-worktree` moved onto
/// the change's branch in place: the change's recorded worktree is the
/// worktree holding the change branch, so checking the target out there puts
/// the checkout back on the branch it stood on before `begin`. Any other
/// shape refuses, naming the missing target checkout.
pub(super) fn target_checkout(ctx: &Ctx, st: &ChangeState, target: &str) -> Result<TargetCheckout> {
    if let Some(path) = gitio::worktree_for_branch(&ctx.cwd, target)? {
        return Ok(TargetCheckout {
            path,
            switch_from: None,
        });
    }
    let recorded = st.worktree.as_deref().map(PathBuf::from);
    let holds_change = match recorded.as_deref() {
        Some(path) => gitio::worktree_for_branch(&ctx.cwd, &st.branch)?
            .is_some_and(|holding| same_path(&holding, path)),
        None => false,
    };
    match (recorded, holds_change) {
        (Some(path), true) => Ok(TargetCheckout {
            path,
            switch_from: Some(st.branch.clone()),
        }),
        _ => bail!("no worktree has {target:?} checked out; check it out first"),
    }
}

/// Whether two paths name the same directory, comparing what the filesystem
/// resolves them to when both exist.
fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Refuse when the checkout a merge or a promotion writes into carries
/// tracked modifications. Writing beside uncommitted work leaves a tree
/// nobody can name afterwards.
pub(super) fn checkout_tracked_dirt(checkout: &Path) -> Result<()> {
    integration::guard_tracked_dirt(checkout, gitio::dirt(checkout)?.tracked)
        .map_err(Refusal::into_error)
}

/// Refuse when a merge or a promotion would write over a path the checkout
/// holds untracked or ignored, and report the paths it leaves untouched.
pub(super) fn checkout_writes(
    checkout: &TargetCheckout,
    target_head: &str,
    merged_tree: Option<&str>,
) -> Result<()> {
    let collisions = write_collisions(checkout, target_head, merged_tree)?;
    integration::guard_writes(&checkout.path, collisions).map_err(Refusal::into_error)?;
    report_untouched(checkout, &gitio::untracked_and_ignored(&checkout.path)?);
    Ok(())
}

/// The untracked or ignored paths of a checkout that a merge, or a promotion
/// to `merged_tree`, would write.
///
/// A merge computes its result from the tree it produces, so the paths it
/// writes are exactly those added or changed between the target head and that
/// tree, together with the parent directories they need. A checkout that must
/// first be moved onto the target writes the paths that differ between the
/// branch it holds and the target, so those are checked too: an ignored path
/// there loses its bytes to the switch, and a switch that fails after the
/// merge has begun leaves a state nobody asked for. Each candidate is checked
/// against the checkout itself, which covers ignored paths Git would overwrite
/// without saying so and bounds the work by the size of the change rather than
/// the size of the checkout.
fn write_collisions(
    checkout: &TargetCheckout,
    target_head: &str,
    merged_tree: Option<&str>,
) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let mut parents = Vec::new();
    let mut collect = |writes: gitio::WriteSet| {
        files.extend(writes.files);
        parents.extend(writes.parents);
    };
    if let Some(branch) = &checkout.switch_from {
        let from = gitio::branch_head(&checkout.path, branch)?;
        collect(gitio::write_set(&checkout.path, &from, target_head)?);
    }
    if let Some(tree) = merged_tree {
        collect(gitio::write_set(&checkout.path, target_head, tree)?);
    }
    files.sort();
    files.dedup();
    parents.sort();
    parents.dedup();
    let writes = gitio::WriteSet { files, parents };
    gitio::write_overlap(&checkout.path, &writes)
}

/// Say which untracked or ignored paths the update leaves where they are.
fn report_untouched(checkout: &TargetCheckout, left: &[String]) {
    if !left.is_empty() {
        println!(
            "worktree {}: leaving {} untracked or ignored {} the update does not write: {}",
            checkout.path.display(),
            left.len(),
            if left.len() == 1 { "path" } else { "paths" },
            name_first_few(left, 3)
        );
    }
}

/// Name up to `limit` paths and count the rest, so a report stays one line
/// however much a checkout holds.
fn name_first_few(paths: &[String], limit: usize) -> String {
    let mut named = paths
        .iter()
        .take(limit)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > limit {
        named.push_str(&format!(", and {} more", paths.len() - limit));
    }
    named
}

/// Integrate one already-selected change, starting from the state its caller
/// read before taking any lock. Tagged integration reuses this guarded path
/// for every open member so each merge gets the normal target, approval,
/// gate, and dependency checks.
#[allow(clippy::too_many_arguments)]
fn integrate_one(
    ctx: &Ctx,
    initial: ChangeState,
    into: Option<String>,
    message: Option<String>,
    cleanup: CleanupStyle,
    closed_behavior: ClosedBehavior,
    options: SingleOptions,
) -> Result<i32> {
    let started_at = chrono::Utc::now();
    let SingleOptions {
        cleanup_checkout,
        debt,
        shape,
        style_flags,
        dry_run,
        json,
        mut expect_basis,
    } = options;
    let store = ctx.store()?;
    let change_id = initial.change_id.clone();
    if let Some(expected) = &expect_basis {
        if expected.change_id != change_id {
            bail!(
                "the expected basis describes {}, not {change_id}",
                expected.change_id
            );
        }
    }
    if initial.iterating && debt.is_none() {
        eprintln!(
            "change declares it is iterating; clear it with arc iterating {} --off",
            initial.change_id
        );
        return Ok(status::Blocker::Iterating.exit_code());
    }
    let target = into.unwrap_or_else(|| initial.target_branch.clone());
    let selection = IntegrationStyle {
        shape,
        cleanup,
        flags: style_flags,
    };
    if dry_run {
        integration_history(ctx, &initial, &target)?;
        // A dry run promises to write nothing, so there is no record for the
        // policy to be about.
        if json {
            return integrate_dry_run_json(
                ctx,
                &store,
                &initial,
                &target,
                &selection,
                debt.as_ref(),
            );
        }
        return integrate_dry_run(
            ctx,
            &store,
            &initial,
            &target,
            message.as_deref(),
            &selection,
            debt.as_ref(),
        );
    }
    // The same store the merge's closure event will be appended to, so the
    // merge and the record are judged by one reading of the policy.
    ctx.ensure_declared_actor(&store)?;
    let mut selected = initial.clone();
    selected.target_branch = target.clone();
    ctx.ensure_target_declares_actor(&selected)?;
    // Cross-change order is always target, then graph, then change. This
    // serializes the target worktree, and holds the must-land-together
    // frontier still from the readiness reading through the closure, without
    // allowing an integration/metadata lock cycle.
    let target_lock = store.lock_target(&target)?;
    let graph = store.lock_graph()?;
    let transition = store.lock_transition(&change_id)?;
    let mut st = store.state(&change_id)?;
    let recorded_target = st.target_branch.clone();
    st.target_branch = target.clone();
    if st.is_closed() && matches!(closed_behavior, ClosedBehavior::SkipTagged) {
        println!("{}: {}", st.change_id, change_status(&st));
        return Ok(0);
    }
    let Ok(target_head) = gitio::branch_head(&ctx.cwd, &target) else {
        let report = ctx.report_for_integration(&store, &st, &target, shape)?;
        eprint!("{}", render::blocker_explanation(&st, &report));
        return Ok(status::check_exit_code(&report));
    };
    let policy = crate::policy::load_at(&ctx.cwd, &target_head)?;
    ensure_destination_actor(ctx, &policy)?;
    selection.validate(&policy)?;
    integration_history_with_policy(ctx, &st, &target_head, &policy)?;
    if shape == integration::Shape::FfOnly && ff_diverged(ctx, &st, &target)? {
        let code = refuse_divergence(&st, &target, &recorded_target);
        if let Some(prior) = &expect_basis {
            if let Ok(report) = ctx.report_for_integration(&store, &st, &target, shape) {
                let outcome = integration::decide(
                    integration_facts(ctx, &store, &st, &report, &target, &selection, false),
                    Some(prior),
                );
                if let Some(moved) = integration::describe_moved(&outcome.moved) {
                    eprintln!("{moved}");
                }
            }
        }
        return Ok(code);
    }
    if super::fork::fork_slug_for_branch(&ctx.cwd, &st.branch)?.is_none() {
        if let Some(debt) = debt {
            let receipt = super::audit::declare_debt_locked(
                ctx,
                &store,
                &st,
                &debt.reason,
                debt.kind,
                &debt.read,
            )?;
            expect_basis =
                expect_basis.map(|prior| prior.after_declaring(receipt.event_written_over(&st)));
            st = store.state(&change_id)?;
            st.target_branch = target.clone();
        }
    }
    // Integration is the last command that reads the change's worktree, and
    // `--cleanup` removes it. Whatever an executor spooled there is filed
    // before anything can delete the only copy, and before readiness can turn
    // the integration back: the writes belong in the journal either way.
    if let Some(worktree) = st.worktree.as_deref() {
        crate::journal::promote_worktree_spool(ctx, Path::new(worktree));
    }
    let report = ctx.report_evaluating_integration(&store, &st, &target, shape)?;
    if let Some(claim) = &st.claim {
        let timing = state::claim_timing_at(claim, chrono::Utc::now());
        let caller = super::claims::command_identity(ctx).ok();
        if timing.active && caller.as_ref() != Some(&claim.owner) {
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
    advise_role_trailers(&report);
    let facts = integration_facts(ctx, &store, &st, &report, &target, &selection, true);
    let outcome = integration::decide(facts, expect_basis.as_ref());
    let moved = integration::describe_moved(&outcome.moved);
    let plan = match outcome.decision {
        Decision::Refused(refusal) => {
            let code = match refusal {
                Refusal::Diverged => Ok(refuse_divergence(&st, &target, &recorded_target)),
                Refusal::NotReady => {
                    eprint!("{}", render::blocker_explanation(&st, &report));
                    Ok(status::check_exit_code(&report))
                }
                refusal => Err(refusal.into_error()),
            };
            if let Some(moved) = &moved {
                eprintln!("{moved}");
            }
            return code;
        }
        Decision::ReadyToSend(send) => {
            warn_moved(moved.as_deref());
            return record_ready_to_send(
                ctx,
                &store,
                &st,
                &target,
                &send.target_revision,
                send.contribution,
                &send.approved.patchset_id,
                &send.approved.head,
                send.authorization,
            );
        }
        Decision::Provisional(_) => bail!("authoritative decision produced a provisional plan"),
        Decision::Merge(plan) => {
            warn_moved(moved.as_deref());
            plan
        }
    };
    let integration::MergePlan {
        old_target,
        approved,
        evaluated_tree,
        shape: actual_shape,
        authorization,
        checkout,
        untouched,
        ..
    } = plan;
    let integration::Approved {
        patchset_id: approved_patchset_id,
        head: approved_head,
    } = approved;

    let wt = checkout.path.clone();
    let slot_use = crate::slots::exclusive_in(&wt)?;
    if actual_shape == crate::model::IntegrationShape::FastForward {
        refuse_initialized_gitlink_changes(&wt, &old_target, &approved_head)?;
    }
    let already_contained = actual_shape == crate::model::IntegrationShape::AlreadyContained;
    let mut ev = ctx.event(
        &store,
        &change_id,
        Payload::ChangeIntegrated {
            authority_grant_event_id: crate::replica::observed_authority(&store)?
                .map(|a| a.grant_event_id),
            provisional_resolutions: if crate::replica::observed_authority(&store)?.is_some_and(
                |authority| authority.holder_repository_id.as_deref() == Some(&store.repository_id),
            ) {
                crate::provisional::acceptances(&st, &approved_patchset_id, &approved_head)?
            } else {
                Vec::new()
            },
            integrated_commit: old_target.clone(),
            source_patchset_id: approved_patchset_id.clone(),
            source_head: approved_head.clone(),
            target_branch: target.clone(),
            target_before: old_target.clone(),
            authorization: Some(authorization),
            already_contained,
            shape: Some(actual_shape),
            started_at: Some(started_at),
            landing: None,
        },
    )?;
    let merged = (|| -> Result<String> {
        report_untouched(&checkout, &untouched);
        if checkout.switch_from.is_some() {
            gitio::checkout(&checkout.path, &target).with_context(|| {
                format!(
                    "cannot check {target} out in {} for the merge",
                    checkout.path.display()
                )
            })?;
            println!("checked out {target} in {}", checkout.path.display());
        }
        let msg = message.unwrap_or_else(|| format!("merge({}): {}", st.slug, st.title));

        // Already-contained integrations retain the observed target; advancing
        // fast-forwards land at the approved head without a merge commit.
        if gitio::current_branch(&wt)?.as_deref() != Some(target.as_str())
            || gitio::head(&wt)? != old_target
        {
            bail!("target moved during integration before merge; nothing recorded");
        }
        let merged = if already_contained {
            old_target.clone()
        } else if actual_shape == crate::model::IntegrationShape::FastForward {
            let target_ref = format!("refs/heads/{target}");
            let reflog = format!("arc integrate {}: fast-forward", st.slug);
            gitio::git(
                &wt,
                &[
                    "update-ref",
                    "-m",
                    &reflog,
                    &target_ref,
                    &approved_head,
                    &old_target,
                ],
            )?;
            if let Err(error) = gitio::git(&wt, &["read-tree", "-u", "-m", &old_target, &approved_head])
            {
                let rollback = gitio::git(
                    &wt,
                    &["update-ref", &target_ref, &old_target, &approved_head],
                );
                let cause = match rollback {
                    Ok(_) => anyhow::anyhow!(
                        "checkout update failed: {error}; target ref restored to {old_target}"
                    ),
                    Err(rollback) => anyhow::anyhow!(
                        "checkout update failed: {error}; guarded ref rollback failed: {rollback}"
                    ),
                };
                return Err(cause);
            }
            let head =
                gitio::head(&wt)?;
            if head != approved_head
                || gitio::branch_head(&wt, &target).ok().as_deref() != Some(approved_head.as_str())
            {
                return Err(anyhow::anyhow!("fast-forward target moved during integration"));
            }
            head
        } else {
            if let Err(e) = gitio::git(
                &wt,
                &[
                    "-c",
                    &format!("branch.{target}.mergeOptions="),
                    "-c",
                    "merge.ff=false",
                    "merge",
                    "--no-ff",
                    "--no-squash",
                    "--no-edit",
                    "-m",
                    &msg,
                    &approved_head,
                ],
            ) {
                bail!("merge failed: {e}");
            }

            let merged = gitio::head(&wt)?;
            let parents = gitio::commit_parents(&wt, &merged)?;
            if parents != vec![old_target.clone(), approved_head.clone()] {
                bail!(
                    "merge commit {merged} has unexpected parents {parents:?}; expected [{old_target}, {approved_head}] — target moved during integration, inspect before trusting this merge"
                );
            }
            merged
        };
        // The shipped tree must match the tree evaluated by the gates.
        // Failed transitions retain Git state for operator recovery.
        if let Some(expected) = &evaluated_tree {
            let shipped = gitio::commit_tree(&wt, &merged)?;
            if &shipped != expected {
                if actual_shape == crate::model::IntegrationShape::FastForward {
                    return Err(anyhow::anyhow!("fast-forward {merged} carries tree {shipped}, not the evaluated tree {expected}"));
                }

                bail!(
                    "merge commit {merged} carries tree {shipped}, not the evaluated tree {expected}; nothing was recorded"
                );
            }
        }

        if actual_shape == crate::model::IntegrationShape::FastForward {
            let index_tree = gitio::git(&wt, &["write-tree"])?;
            let approved_tree = gitio::commit_tree(&wt, &approved_head)?;
            if gitio::head(&wt).ok().as_deref() != Some(approved_head.as_str())
                || gitio::branch_head(&wt, &target).ok().as_deref() != Some(approved_head.as_str())
                || gitio::current_branch(&wt).ok().flatten().as_deref() != Some(target.as_str())
                || index_tree != approved_tree
                || gitio::dirt(&wt).map(|dirt| dirt.tracked).unwrap_or(true)
            {
                return Err(anyhow::anyhow!("fast-forward target or checkout moved during integration"));
            }
        }
        if gitio::head(&wt)? != merged
            || gitio::branch_head(&wt, &target)? != merged
            || gitio::current_branch(&wt)?.as_deref() != Some(target.as_str())
        {
            bail!("target or checkout moved during integration verification");
        }
        Ok(merged)
    })()
    .map_err(|error| transition_failure(&wt, &target, &approved_head, error))?;
    if let Payload::ChangeIntegrated {
        integrated_commit, ..
    } = &mut ev.payload
    {
        *integrated_commit = merged.clone();
    }
    ev.created_at = chrono::Utc::now();
    if let Err(error) = store.append_local_event(&mut ev) {
        let publication = store.event_is_published(&ev);
        let (publication, next) = match publication {
            Ok(true) => ("published", "Do not retry integration. The closure is visible but its durability is unconfirmed; restore ledger durability, inspect arc show and perform any remaining retention or checkout cleanup."),
            Ok(false) => ("absent", "Repair ledger writability, confirm the reported Git state and approved head, then retry integration to record the already-contained closure."),
            Err(_) => ("unknown", "Restore ledger readability and inspect the exact event before choosing recovery; do not retry integration until publication is known."),
        };
        let cause = anyhow::anyhow!(
            "closure publication failed for event {}: {error:#}; publication state={publication}. {next}",
            ev.event_id
        );
        return Err(integration_recovery(&wt, &target, cause));
    }
    if let Err(error) =
        crate::base_pass::record(ctx, &store, &ev, &st, || Ok(ctx.declarations(&st)?.gates))
    {
        eprintln!("warning: integration recorded; baseline unavailable: {error:#}");
    }
    // Git updates the checkout before the ledger records the closure.
    // Retention and worktree cleanup are post-closure maintenance and may
    // invoke Git hooks or other arc commands, so no state lock spans them.
    drop(transition);
    drop(graph);
    drop(target_lock);
    if already_contained {
        println!("integrated: {merged} (already contained {approved_head}; no merge created)");
    } else {
        println!("integrated: {merged}");
    }
    println!("event: {}", ev.event_id);
    if let Err(error) = release_retention_refs(ctx, &change_id, Some(&merged)) {
        eprintln!(
            "warning: integration recorded; retention cleanup failed: {error:#}. Remaining refs under {}, {} and {} stay pinned",
            gitio::retention_prefix(&change_id),
            gitio::tree_retention_prefix(&change_id),
            gitio::retained_prefix(&change_id),
        );
    }
    if let Some(advisory) = crate::status::missing_changelog_advisory(&st, Some(&ctx.cwd)) {
        eprintln!("advice: {}", advisory.detail);
    }
    crate::journal::auto_log(
        ctx,
        &st.slug,
        &format!("integrated {change_id} at {merged}"),
    );
    advise_plan_promotions(&store, &st);
    super::slots::release_change_slot(ctx, &store, &st, Some(&merged), Some(&st.branch));

    if selection.cleanup.enabled {
        let checkout = cleanup_checkout.as_deref().unwrap_or(&ctx.cwd);
        if let Err(error) = cleanup_integrated_change(ctx, &st, &wt, checkout, &selection.cleanup) {
            eprintln!(
                "warning: integration recorded; cleanup for branch {} failed: {error:#}",
                st.branch
            );
            println!("kept branch {}: cleanup could not complete", st.branch);
        }
    }
    drop(slot_use);
    super::slots::warm_after_integration(ctx);
    Ok(0)
}

fn cleanup_integrated_change(
    ctx: &Ctx,
    st: &ChangeState,
    target: &Path,
    checkout: &Path,
    style: &CleanupStyle,
) -> Result<()> {
    // A slot's checkout and branch belong to the slot's release, which
    // detaches the checkout and deletes the merged branch.
    if let Some(record) = &st.slot {
        println!("kept slot checkout {}", record.path);
        return Ok(());
    }
    if let Some(wt_path) = &st.worktree {
        let path = PathBuf::from(wt_path);
        if path.exists() {
            let canonical_path = path.canonicalize()?;
            let reason = if !st.worktree_created {
                Some("not created by arc".to_string())
            } else if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                Some(
                    "the recorded path is a symlink, so its contents are not inspected".to_string(),
                )
            } else if canonical_path == gitio::toplevel(checkout)?.canonicalize()?
                || canonical_path == target.canonicalize()?
            {
                Some("is the integration checkout".to_string())
            } else if let Some(prefix) = ctx.excluded_by_sandbox(&path)? {
                Some(format!("outside the sandbox at {}", prefix.display()))
            } else if !gitio::worktree_inventory(target)?.iter().any(|entry| {
                !entry.prunable
                    && entry.branch.as_deref() == Some(st.branch.as_str())
                    && entry.path.canonicalize().ok().as_ref() == Some(&canonical_path)
            }) || !gitio::worktree_owner_matches(
                &path,
                &st.change_id,
                st.worktree_owner_token.as_deref(),
            ) {
                Some("ownership cannot be established".to_string())
            } else {
                let (content, artifacts) =
                    cleanup_local_content(&path, &style.disposable, style.discard)?;
                if let Some(content) = content {
                    Some(format!("holds local content not in the branch: {content}"))
                } else {
                    match gitio::git(target, &["worktree", "remove", wt_path]) {
                        Ok(_) => {
                            for artifact in artifacts {
                                println!(
                                    "discarded {} ({})",
                                    artifact.path, artifact.identified_by
                                );
                            }
                            println!("removed worktree {wt_path}");
                            None
                        }
                        Err(error) => {
                            let message = format!("{error:#}")
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ");
                            eprintln!("warning: integration recorded; cleanup worktree {wt_path} failed: {message}");
                            Some(format!("Git refused: {message}"))
                        }
                    }
                }
            };
            if let Some(reason) = reason {
                println!("kept worktree {wt_path}: {reason}");
            }
        }
    }
    if let Some(path) = gitio::worktree_for_branch(target, &st.branch)? {
        println!(
            "kept branch {}: checked out in kept worktree {}",
            st.branch,
            path.display()
        );
    } else {
        match gitio::git(target, &["branch", "-d", &st.branch]) {
            Ok(_) => println!("deleted branch {}", st.branch),
            Err(error) => {
                let message = format!("{error:#}")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                eprintln!(
                    "warning: integration recorded; cleanup branch {} failed: {message}",
                    st.branch
                );
                println!("kept branch {}: Git refused: {message}", st.branch);
            }
        }
    }
    Ok(())
}

/// Only the regular ignore file written by the spool may occupy an unused outbox.
fn unused_outbox_metadata(checkout: &Path) -> Result<bool> {
    if !crate::journal::outbox_emptied_by_arc(checkout)? {
        return Ok(false);
    }
    let outbox = checkout.join(".arc/outbox");
    let Ok(metadata) = std::fs::symlink_metadata(&outbox) else {
        return Ok(false);
    };
    if !metadata.is_dir() {
        return Ok(false);
    }
    let entries = std::fs::read_dir(&outbox)?.collect::<std::io::Result<Vec<_>>>()?;
    if entries.len() != 1 || entries[0].file_name() != ".gitignore" {
        return Ok(false);
    }
    let ignore = &entries[0];
    Ok(ignore.file_type()?.is_file() && std::fs::read(ignore.path())? == b"*\n")
}

/// Cleanup reads Git content without configuration shortcuts.
fn cleanup_git(checkout: &Path, args: &[&str]) -> Result<String> {
    let mut command = vec![
        "--work-tree",
        checkout
            .to_str()
            .context("cleanup checkout path is not UTF-8")?,
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
        "-c",
        "status.showUntrackedFiles=all",
        "-c",
        "core.ignoreStat=false",
        "-c",
        "core.fileMode=true",
        "-c",
        "core.symlinks=true",
        "-c",
        "core.ignoreCase=false",
        "-c",
        "core.trustCtime=true",
        "-c",
        "core.checkStat=default",
    ];
    command.extend_from_slice(args);
    gitio::git(checkout, &command)
}

fn cleanup_status(checkout: &Path, untracked: &str) -> Result<String> {
    cleanup_git(
        checkout,
        &[
            "status",
            "--porcelain",
            "-z",
            "--ignored=traditional",
            "--ignore-submodules=none",
            untracked,
        ],
    )
}

/// Inspect the checkout tree independently of Git status, without following symlinks.
fn cleanup_nested_repositories(checkout: &Path) -> Result<Vec<String>> {
    // A root that is not a real directory cannot be inspected, so it is never
    // reported clean.
    if !std::fs::symlink_metadata(checkout)
        .with_context(|| format!("cannot inspect {}", checkout.display()))?
        .is_dir()
    {
        bail!("{} is not a directory", checkout.display());
    }
    let mut pending = vec![checkout.to_path_buf()];
    let mut repositories = Vec::new();
    while let Some(directory) = pending.pop() {
        if !std::fs::symlink_metadata(&directory)
            .with_context(|| format!("cannot inspect {}", directory.display()))?
            .is_dir()
        {
            continue;
        }
        let mut children = Vec::new();
        let mut checkout_marker = false;
        let mut head = false;
        let mut objects = false;
        let mut refs = false;
        for entry in std::fs::read_dir(&directory)
            .with_context(|| format!("cannot read {}", directory.display()))?
        {
            let entry = entry
                .with_context(|| format!("cannot read an entry in {}", directory.display()))?;
            let kind = entry
                .file_type()
                .with_context(|| format!("cannot inspect {}", entry.path().display()))?;
            if directory == checkout && entry.file_name() == ".git" && kind.is_file() {
                continue;
            }
            match entry.file_name().to_str() {
                Some(".git") => checkout_marker = true,
                Some("HEAD") => head = kind.is_file() || kind.is_symlink(),
                Some("objects") => objects = kind.is_dir() || kind.is_symlink(),
                Some("refs") => refs = kind.is_dir() || kind.is_symlink(),
                _ => {}
            }
            if kind.is_dir() {
                children.push(entry.path());
            }
        }
        if checkout_marker || head && objects && refs {
            let relative = directory.strip_prefix(checkout)?;
            repositories.push(if relative.as_os_str().is_empty() {
                ".".to_string()
            } else {
                relative.to_string_lossy().into_owned()
            });
        }
        pending.extend(children);
    }
    Ok(repositories)
}

fn cleanup_local_content(
    checkout: &Path,
    rules: &[crate::artifacts::Rule],
    discard: crate::style::Discard,
) -> Result<(Option<String>, Vec<crate::artifacts::Artifact>)> {
    let mut paths = match cleanup_nested_repositories(checkout) {
        Ok(repositories) => repositories
            .into_iter()
            .map(|path| format!("{path} (nested Git repository)"))
            .collect::<Vec<_>>(),
        Err(error) => vec![format!("repository scan failed: {error:#}")],
    };
    let flagged = cleanup_git(checkout, &["ls-files", "-v", "-z"])?;
    paths.extend(
        flagged
            .split('\0')
            .filter(|entry| {
                entry
                    .as_bytes()
                    .first()
                    .is_some_and(|tag| *tag == b'S' || tag.is_ascii_lowercase())
            })
            .filter_map(|entry| entry.get(2..))
            .map(str::to_string),
    );
    let status = cleanup_status(checkout, "--untracked-files=all")?;
    let ignored = cleanup_status(checkout, "--untracked-files=normal")?;
    let ignored_entries = ignored
        .split('\0')
        .filter(|entry| entry.starts_with("!! "))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut artifacts = crate::artifacts::recognize(checkout, &ignored_entries, rules);
    let unused_outbox = unused_outbox_metadata(checkout)?;
    let collapsed_outbox = unused_outbox
        && status
            .split('\0')
            .filter_map(|entry| entry.strip_prefix("!! .arc/"))
            .eq(std::iter::once("outbox/.gitignore"));
    let mut entries = status.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        if let Some(path) = entry.get(3..) {
            let metadata = unused_outbox
                && matches!(&entry[..2], "??" | "!!")
                && matches!(path, ".arc/outbox/" | ".arc/outbox/.gitignore");
            if !metadata && !entry.starts_with("!! ") {
                paths.push(path.to_string());
            }
        }
        if entry
            .as_bytes()
            .get(..2)
            .is_some_and(|tag| tag.contains(&b'R') || tag.contains(&b'C'))
        {
            if let Some(source) = entries.next() {
                paths.push(source.to_string());
            }
        }
    }
    for entry in ignored_entries {
        let path = &entry[3..];
        let metadata = (unused_outbox && matches!(path, ".arc/outbox/" | ".arc/outbox/.gitignore"))
            || (collapsed_outbox && path == ".arc/");
        if metadata {
            continue;
        }
        let arc_content = path == ".arc" || path.starts_with(".arc/");
        let recognized = artifacts.iter().any(|artifact| artifact.path == path);
        if arc_content || !recognized && discard == crate::style::Discard::Artifacts {
            paths.push(path.to_string());
        } else if !recognized {
            artifacts.push(crate::artifacts::Artifact {
                path: path.to_string(),
                identified_by: "ignored; integration.discard = ignored".to_string(),
            });
        }
    }
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Ok((None, artifacts));
    }
    let mut summary = paths.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    if paths.len() > 5 {
        summary.push_str(", ...");
    }
    Ok((Some(summary), artifacts))
}

/// The closure that satisfied each of the change's prerequisites, followed
/// through supersession, read through one read of the ledger.
pub(super) fn prerequisite_closures(
    store: &Store,
    st: &ChangeState,
) -> Result<Vec<crate::model::PrerequisiteClosure>> {
    let mut prerequisites = Vec::new();
    if st.blocked_by.is_empty() {
        return Ok(prerequisites);
    }
    let read = store.read().context(
        "the prerequisites cannot be read, so the authorization basis would be incomplete",
    )?;
    for blocker in &st.blocked_by {
        // A basis missing a prerequisite is a basis that misstates what was
        // checked, so an unreadable one refuses the merge rather than being
        // quietly omitted.
        let events = read.events(blocker).with_context(|| {
            format!("prerequisite {blocker} cannot be read, so the authorization basis would be incomplete")
        })?;
        let mut blocker_id = blocker.clone();
        let mut blocker_state = read.project(&events)?;
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
            let Ok(events) = read.events(&successor) else {
                break;
            };
            seen.push(successor.clone());
            blocker_id = successor;
            blocker_state = read.project(&events)?;
        }
        if let Some(closure) = &blocker_state.closure {
            prerequisites.push(crate::model::PrerequisiteClosure {
                change_id: blocker_id,
                closure_event_id: closure.event_id.clone(),
                integrated_commit: closure.integrated_commit.clone(),
                landing: None,
            });
        }
    }
    Ok(prerequisites)
}

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
    target: &str,
    selection: &IntegrationStyle,
) -> Result<crate::model::AuthorizationBasis> {
    // Either a verdict approved this patchset, or a declared debt stood in for
    // the review nobody performed. One of the two must hold: a merge with
    // neither has nothing authorizing it, and this record exists to say what
    // did.
    let approval = integration::approval(st, report, approved_patchset_id);
    if approval.verdict_event_id.is_none()
        && approval.external_verdict.is_none()
        && !st.debt_waives_latest_patchset()
    {
        anyhow::bail!("integration is ready but nothing authorizes the merged patchset: no approving verdict and no declared debt");
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

    let prerequisites = prerequisite_closures(store, st)?;

    let mut selected = st.clone();
    selected.target_branch = target.to_string();
    let declarations = ctx.declarations(&selected)?;
    ensure_destination_actor(ctx, &declarations.policy)?;
    selection.validate(&declarations.policy)?;
    integration_history_with_policy(
        ctx,
        st,
        &gitio::branch_head(&ctx.cwd, target)?,
        &declarations.policy,
    )
    .map_err(|_| Refusal::ConfigurationMoved.into_error())?;
    let declarations = normalized_declarations(declarations, st);

    let selected_verdict_provenance = approval
        .verdict_event_id
        .as_ref()
        .and_then(|id| st.verdicts.iter().find(|verdict| &verdict.event_id == id))
        .map(
            |verdict| crate::import_authorization::SelectedVerdictProvenance {
                origin_replica_id: verdict.origin_replica_id.clone(),
                admission_provenance: st
                    .privilege_scope
                    .as_ref()
                    .map(|scope| scope.provenance(&verdict.event_id))
                    .unwrap_or("legacy-unknown")
                    .into(),
                receipt: st
                    .privilege_scope
                    .as_ref()
                    .and_then(|scope| scope.admission_for(&verdict.event_id))
                    .cloned(),
            },
        );
    Ok(crate::model::AuthorizationBasis {
        budget_rules: declarations.budget_rules.clone(),
        selected_verdict_provenance,
        verdict_event_id: approval.verdict_event_id,
        external_verdict: approval.external_verdict,
        verdict_provisional: approval.verdict_provisional,
        gate_evidence,
        prerequisites,
        provisional_prerequisites: Vec::new(),
        // Empty by construction: `integrate_ready` is false while either is
        // non-empty, so the event cannot be written otherwise. Recording them
        // says the guard checked, rather than leaving an auditor to infer it.
        blocking_findings: report.open_blocking_findings.clone(),
        holds: report
            .holds
            .iter()
            .map(|hold| hold.hold_event_id.clone())
            .collect(),
        gates: declarations.gates,
        policy: declarations.policy,
        danger: Some(report.danger.clone()),
        audit_debt_event_id: approval.audit_debt_event_id,
    })
}

/// The gate and policy declarations in force for the change, normalized as
/// an authorization basis records them.
fn consumed_declarations(
    ctx: &Ctx,
    st: &ChangeState,
    target_revision: &str,
) -> Result<integration::Declarations> {
    let declarations = crate::declarations::for_change_at(&ctx.cwd, st, Some(target_revision))?;
    ensure_destination_actor(ctx, &declarations.policy)?;
    Ok(normalized_declarations(declarations, st))
}

pub(super) fn ensure_destination_actor(
    ctx: &Ctx,
    policy: &crate::policy::PolicyFile,
) -> Result<()> {
    if policy.policy.require_declared_actor
        && !ctx.actor_source.declared()
        && ctx.on_behalf_of.is_none()
    {
        let sources = policy
            .sources
            .sources_for("policy.require_declared_actor=true");
        bail!("destination policy requires a declared actor (declared by {}); pass --actor or set ARC_ACTOR", sources.join(", "));
    }
    Ok(())
}

pub(super) fn normalized_declarations(
    declarations: crate::declarations::Declarations,
    st: &ChangeState,
) -> integration::Declarations {
    let (gates, policy) = (declarations.gates, declarations.policy);
    let gates_budget = gates.budget.clone();
    let gates = gates
        .required_for(&st.profile)
        .into_iter()
        .map(|(name, gate)| {
            (
                name.clone(),
                crate::model::NormalizedGate {
                    concurrent: gate.concurrent,
                    concurrent_acceptance: gate.concurrent_acceptance.clone(),
                    coverage_identity: gates::coverage_identity(gate),
                    paths: gate.paths.clone(),
                    inert: gate.coverage.inert.clone(),
                    budget_ms: gate.budget,
                    budget_sources: gate.budget_sources.clone(),
                    command: gate.command.clone(),
                    events: gate.events.clone(),
                    selector: gate.selector.clone(),
                    profiles: gate.profiles.clone(),
                    timeout: gate.timeout,
                    binds: gate.binds,
                    declared_by: gate.declared_by.clone(),
                },
            )
        })
        .collect();
    integration::Declarations {
        budget_rules: gates_budget,
        gates,
        policy: crate::model::NormalizedPolicy {
            replica: policy.replica,
            forbid_self_approval: policy.policy.forbid_self_approval,
            require_declared_actor: policy.policy.require_declared_actor,
            provenance_git_identity: policy.provenance.git_identity.as_str().to_string(),
            declared_by: policy.sources.as_map(),
        },
    }
}

/// The contribution declaration in force at the change's target, if any.
fn contribution_policy(ctx: &Ctx, st: &ChangeState) -> Result<Option<crate::policy::Contribution>> {
    Ok(ctx.declarations(st)?.policy.contribution)
}

/// Local history constraints count the reviewed head above its target.
fn integration_history(ctx: &Ctx, st: &ChangeState, target: &str) -> Result<()> {
    if st.is_closed() || !gitio::branch_exists(&ctx.cwd, target) {
        return Ok(());
    }
    let target_head = gitio::branch_head(&ctx.cwd, target)?;
    let policy = crate::policy::load_at(&ctx.cwd, &target_head)?;
    integration_history_with_policy(ctx, st, &target_head, &policy)
}

fn integration_history_with_policy(
    ctx: &Ctx,
    st: &ChangeState,
    target_revision: &str,
    policy: &crate::policy::PolicyFile,
) -> Result<()> {
    if st.is_closed() {
        return Ok(());
    }
    if policy.contribution.is_some() || policy.integration_history != crate::policy::History::Squash
    {
        return Ok(());
    }
    let Some(patchset) = st.latest_patchset() else {
        return Ok(());
    };
    let base = gitio::merge_base(&ctx.cwd, target_revision, &patchset.head)?;
    let commits = gitio::git(
        &ctx.cwd,
        &["rev-list", "--count", &format!("{base}..{}", patchset.head)],
    )?;
    let count: usize = commits.trim().parse()?;
    if count > 1 {
        bail!("integration history is squash and the change has {count} commits since {target_revision}; run `arc squash` to integrate one");
    }
    Ok(())
}

/// Refuse a head whose history is not the shape the receiver declared: a merge
/// commit anywhere since the base, or more than one commit when the history
/// is squashed.
fn contribution_shape(
    ctx: &Ctx,
    target: &str,
    head: &str,
    history: crate::policy::History,
) -> Result<()> {
    let target_head = gitio::branch_head(&ctx.cwd, target)?;
    let base = gitio::merge_base(&ctx.cwd, &target_head, head)?;
    let range = format!("{base}..{head}");
    let commits = gitio::git(&ctx.cwd, &["rev-list", &range])?;
    let count = commits.lines().filter(|line| !line.is_empty()).count();
    if count == 0 {
        bail!("{head} adds no commit to {target}; there is nothing to send");
    }
    let merges = gitio::git(&ctx.cwd, &["rev-list", "--merges", &range])?;
    if let Some(merge) = merges.lines().find(|line| !line.is_empty()) {
        bail!(
            "contributed history carries merge commit {merge}; a receiver's branch takes \
             no merge commits — rebase the change onto {target}"
        );
    }
    if history == crate::policy::History::Squash && count > 1 {
        bail!(
            "contribution history is squash and the change has {count} commits since \
             {target}; run `arc squash` to send one"
        );
    }
    Ok(())
}

/// Record that a contributed change passed every integration check at its
/// approved head, instead of merging it. The receiver merges, and its
/// decision closes the change.
#[allow(clippy::too_many_arguments)]
fn record_ready_to_send(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    target: &str,
    target_revision: &str,
    contribution: crate::policy::Contribution,
    patchset_id: &str,
    head: &str,
    authorization: AuthorizationBasis,
) -> Result<i32> {
    contribution_shape(ctx, target, head, contribution.history)?;
    if gitio::branch_head(&ctx.cwd, target)? != target_revision {
        bail!("contribution destination moved during authorization; nothing recorded");
    }
    if st.ready_to_send.as_ref().is_some_and(|ready| {
        ready.head == head
            && ready.patchset_id == patchset_id
            && ready.target_branch.as_deref() == Some(target)
            && ready.target_revision.as_deref() == Some(target_revision)
            && ready.history == contribution.history.as_str()
            && ready.authorization == authorization
    }) {
        println!("{}: already ready to send at {head}", st.change_id);
        return Ok(0);
    }
    let payload = Payload::ReadyToSend {
        patchset_id: patchset_id.to_string(),
        head: head.to_string(),
        history: contribution.history.as_str().to_string(),
        target_branch: Some(target.to_string()),
        target_revision: Some(target_revision.to_string()),
        authorization,
    };
    let mut event = ctx.event(store, &st.change_id, payload)?;
    store.append_local_event(&mut event)?;
    println!(
        "ready to send: {} at {head} ({} history); nothing was merged",
        st.change_id,
        contribution.history.as_str()
    );
    println!(
        "Next: send {} to the receiver, then record its decision with \
         `arc external verdict` or `arc close --assert-integrated`",
        st.branch
    );
    println!("event: {}", event.event_id);
    Ok(0)
}

/// Print that a prior basis moved while the fresh decision still permits.
fn warn_moved(moved: Option<&str>) {
    if let Some(moved) = moved {
        eprintln!("warning: {moved}; proceeding on the fresh decision");
    }
}

fn ff_diverged(ctx: &Ctx, st: &ChangeState, target: &str) -> Result<bool> {
    if change_status(st) != "open"
        || st.iterating
        || st.latest_patchset().is_none()
        || !gitio::branch_exists(&ctx.cwd, target)
        || super::fork::fork_slug_for_branch(&ctx.cwd, &st.branch)?.is_some()
    {
        return Ok(false);
    }
    if contribution_policy(ctx, st)?.is_some() {
        return Ok(false);
    }
    let Some(patchset) = st.latest_patchset() else {
        return Ok(false);
    };
    let revision = gitio::branch_head(&ctx.cwd, target)?;
    let (ancestor, contained) = integration::ancestry(&ctx.cwd, &revision, &patchset.head)?;
    Ok(!integration::ff_eligible(ancestor, contained))
}

fn refuse_initialized_gitlink_changes(checkout: &Path, old: &str, approved: &str) -> Result<()> {
    let diff = gitio::git(
        checkout,
        &[
            "diff",
            "--raw",
            "-z",
            "--no-renames",
            "--ignore-submodules=none",
            old,
            approved,
            "--",
        ],
    )?;
    let mut entries = diff.split('\0').filter(|entry| !entry.is_empty());
    let mut initialized = Vec::new();
    while let Some(header) = entries.next() {
        let path = entries.next().context("gitlink diff omitted its path")?;
        let modes: Vec<_> = header.split_whitespace().take(2).collect();
        if modes
            .iter()
            .any(|mode| mode.trim_start_matches(':') == "160000")
            && checkout.join(path).join(".git").exists()
        {
            initialized.push(path.to_string());
        }
    }
    if !initialized.is_empty() {
        bail!("fast-forward changes initialized submodules: {}; use --shape no-ff or integrate manually with submodule updates", initialized.join(", "));
    }
    Ok(())
}

fn transition_failure(
    checkout: &Path,
    target: &str,
    approved: &str,
    cause: anyhow::Error,
) -> anyhow::Error {
    integration_recovery(
        checkout,
        target,
        anyhow::anyhow!("{cause}; no integration event was recorded. Preserve local and competing work, inspect git diff and git reflog. If refs/heads/{target} is the intended destination and this checkout still holds {target}, reconcile it with git restore --source=refs/heads/{target} --staged --worktree --recurse-submodules -- ., resolve any unfinished merge deliberately, then retry arc integrate for approved head {approved}; arc performs no reset"),
    )
}

fn integration_recovery(checkout: &Path, target: &str, cause: anyhow::Error) -> anyhow::Error {
    let revision =
        gitio::branch_head(checkout, target).unwrap_or_else(|error| format!("unreadable: {error}"));
    let head = gitio::head(checkout).unwrap_or_else(|error| format!("unreadable: {error}"));
    let branch = gitio::current_branch(checkout)
        .map(|branch| branch.unwrap_or_else(|| "detached".into()))
        .unwrap_or_else(|error| format!("unreadable: {error}"));
    let index = gitio::git(checkout, &["write-tree"])
        .unwrap_or_else(|error| format!("unreadable: {error}"));
    let dirt = gitio::git(checkout, &["status", "--porcelain"])
        .unwrap_or_else(|error| format!("unreadable: {error}"));
    anyhow::anyhow!("{cause}; refs/heads/{target}={revision}; checkout HEAD={head} ({branch}); index tree={index}; checkout status={dirt:?}; checkout={}", checkout.display())
}

fn refuse_divergence(st: &ChangeState, target: &str, recorded_target: &str) -> i32 {
    let mut diagnostic = st.clone();
    diagnostic.target_branch = recorded_target.to_string();
    eprintln!(
        "Cannot integrate {}: target {target} is not an ancestor of the approved head. {}",
        st.change_id,
        integration::divergence_guidance(&diagnostic, target)
    );
    status::Blocker::NeedsRebase.exit_code()
}

/// Observe everything the integration decision follows from, reading the
/// change and its target once each. Nothing is recorded.
///
/// `evaluating` builds the confirming report the way `report` was built: one
/// that may evaluate in a checkout of its own and remove it, or one that only
/// observes.
fn integration_facts(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    report: &crate::status::StatusReport,
    target: &str,
    selection: &IntegrationStyle,
    evaluating: bool,
) -> IntegrationFacts {
    let shape = selection.shape;
    let approved = st.latest_patchset().map(|patchset| integration::Approved {
        patchset_id: patchset.id.clone(),
        head: patchset.head.clone(),
    });
    let patchset_id = approved
        .as_ref()
        .map(|approved| approved.patchset_id.clone())
        .unwrap_or_default();
    let basis = approved
        .as_ref()
        .context("no patchset recorded")
        .and_then(|_| authorization_basis(ctx, store, st, report, &patchset_id, target, selection));
    let basis = basis.and_then(|basis| {
        #[cfg(debug_assertions)]
        crate::import_authorization::test_rendezvous("integration-budget-confirmation")?;
        Ok(basis)
    });
    // Read after the first basis: agreement between the two is the evidence
    // that the configuration held still across the decision.
    let confirmation = if evaluating {
        ctx.report_evaluating_integration(store, st, target, shape)
    } else {
        ctx.report_for_integration(store, st, target, shape)
    }
    .map(|confirmation| integration::Confirmation {
        ready: confirmation.integrate_ready(),
        basis: authorization_basis(
            ctx,
            store,
            st,
            &confirmation,
            &patchset_id,
            target,
            selection,
        ),
    });
    let checkout = target_checkout(ctx, st, target);
    let unobserved = || anyhow::anyhow!("the target checkout could not be resolved");
    let tracked_dirt = match &checkout {
        Ok(checkout) => gitio::dirt(&checkout.path).map(|dirt| dirt.tracked),
        Err(_) => Err(unobserved()),
    };
    // Read under the target lock, so the merge is checked against the same
    // tree the authorization covers.
    let target_revision = gitio::branch_head(&ctx.cwd, target);
    let mut selected = st.clone();
    selected.target_branch = target.to_string();
    let declarations = target_revision
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))
        .and_then(|revision| consumed_declarations(ctx, &selected, revision));
    let merge_inputs = match (&target_revision, &approved) {
        (Ok(revision), Some(approved)) => Some((revision.as_str(), approved.head.as_str())),
        _ => None,
    };
    let unmerged = || anyhow::anyhow!("the target revision or the approved head is unknown");
    let evaluated_tree = match merge_inputs {
        Some((revision, head)) => {
            gitio::merge_outcome(&ctx.cwd, revision, head).map(|outcome| outcome.tree)
        }
        None => Err(unmerged()),
    };
    let write_collisions = match (&checkout, &target_revision, &evaluated_tree) {
        (Ok(checkout), Ok(revision), Ok(tree)) => {
            write_collisions(checkout, revision, tree.as_deref())
        }
        _ => Err(unobserved()),
    };
    let untouched = match &checkout {
        Ok(checkout) => gitio::untracked_and_ignored(&checkout.path),
        Err(_) => Err(unobserved()),
    };
    let ancestry = match merge_inputs {
        Some((revision, head)) => integration::ancestry(&ctx.cwd, revision, head),
        None => Err(unmerged()),
    };
    let (target_is_ancestor, already_contained) = match ancestry {
        Ok((ancestor, contained)) => (Ok(ancestor), Ok(contained)),
        Err(error) => (Err(anyhow::anyhow!("{error}")), Err(error)),
    };
    IntegrationFacts {
        mode: integration::Mode::Authoritative,
        mode_refusal: crate::replica::integration_refusal(store).ok().flatten(),
        publication_target: target.into(),
        recipient: None,
        completed_provisional: None,
        lifecycle_allows_integration: report.state == "open"
            && !st.iterating
            && report.fork.is_none(),
        shape,
        target_is_ancestor,
        target: target.to_string(),
        approval: integration::approval(st, report, &patchset_id),
        declarations,
        approved,
        ready: report.integrate_ready(),
        basis,
        confirmation,
        contribution: contribution_policy(ctx, st),
        checkout,
        tracked_dirt,
        target_revision,
        evaluated_tree,
        write_collisions,
        untouched,
        already_contained,
    }
}

fn advise_role_trailers(report: &StatusReport) {
    for advisory in &report.advisories {
        if matches!(
            advisory.code,
            "missing-contribution-trailers"
                | "placeholder-contribution-trailers"
                | "contribution-trailers-unreadable"
        ) {
            eprintln!("advice: {}", advisory.detail);
        }
    }
}

/// The change as an integration reads it once its `--debt` is declared,
/// with that declaration judged but not written. A dry run decides from this,
/// so it meets the refusals the real run meets and shows the debt it would
/// declare. A fork never declares one.
fn project_debt(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    debt: Option<&DebtDeclaration>,
) -> Result<(ChangeState, Option<integration::PlannedDebt>)> {
    let Some(debt) = debt else {
        return Ok((st.clone(), None));
    };
    if super::fork::fork_slug_for_branch(&ctx.cwd, &st.branch)?.is_some() {
        return Ok((st.clone(), None));
    }
    let plan = super::audit::plan_debt(ctx, store, st, &debt.reason, debt.kind, &debt.read)?;
    let mut projected = plan.projected(store, st)?;
    projected.target_branch = st.target_branch.clone();
    Ok((projected, Some(plan.planned())))
}

/// A plan names no event for a debt still to be declared.
fn forget_planned_debt_event(plan: &mut IntegrationPlan, debt: Option<integration::PlannedDebt>) {
    let Some(debt) = debt else { return };
    if debt.disposition == integration::DebtDisposition::WouldDeclare {
        plan.authorization.audit_debt_event_id = None;
    }
    plan.debt = Some(debt);
}

/// The plan `integrate` would carry out, decided from the same facts and by
/// the same function as the merge, printed as JSON. Nothing is locked or
/// written.
fn integrate_dry_run_json(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    target: &str,
    selection: &IntegrationStyle,
    debt: Option<&DebtDeclaration>,
) -> Result<i32> {
    let recorded_target = st.target_branch.clone();
    let mut selected = st.clone();
    selected.target_branch = target.to_string();
    let st = &selected;
    let shape = selection.shape;
    ctx.ensure_declared_actor(store)?;
    ctx.ensure_target_declares_actor(st)?;
    if shape == integration::Shape::FfOnly && ff_diverged(ctx, st, target)? {
        return Ok(refuse_divergence(st, target, &recorded_target));
    }
    let (projected, planned_debt) = project_debt(ctx, store, st, debt)?;
    let st = &projected;
    let report = ctx.report_for_integration(store, st, target, shape)?;
    advise_role_trailers(&report);
    let facts = integration_facts(ctx, store, st, &report, target, selection, false);
    let plan = match integration::decide(facts, None).decision {
        Decision::Refused(Refusal::Diverged) => {
            return Ok(refuse_divergence(st, target, &recorded_target))
        }
        Decision::Refused(Refusal::NotReady) => {
            eprint!("{}", render::blocker_explanation(st, &report));
            return Ok(status::check_exit_code(&report));
        }
        Decision::Refused(refusal) => return Err(refusal.into_error()),
        Decision::ReadyToSend(send) => {
            contribution_shape(ctx, target, &send.approved.head, send.contribution.history)?;
            IntegrationPlan::of_send(&st.change_id, &send)
        }
        Decision::Provisional(_) => bail!("authoritative decision produced a provisional plan"),
        Decision::Merge(plan) => {
            if plan.shape == crate::model::IntegrationShape::FastForward {
                refuse_initialized_gitlink_changes(
                    &plan.checkout.path,
                    &plan.old_target,
                    &plan.approved.head,
                )?;
            }
            // A conflicting merge records nothing, so there is no basis it
            // would record to print.
            if plan.merge_commit && plan.evaluated_tree.is_none() {
                eprintln!(
                    "dry-run: merging {} into {target} conflicts — rebase required",
                    st.change_id
                );
                return Ok(status::Blocker::NeedsRebase.exit_code());
            }
            IntegrationPlan::of_merge(&st.change_id, &plan)
        }
    };
    let mut plan = plan;
    forget_planned_debt_event(&mut plan, planned_debt);
    println!("{}", serde_json::to_string_pretty(&plan)?);
    Ok(0)
}

fn print_planned_debt(debt: Option<&integration::PlannedDebt>) {
    for line in debt.map(|debt| debt.describe()).unwrap_or_default() {
        println!("dry-run: {line}");
    }
}

/// Report what `integrate` would do without merging, closing, or writing
/// anything: run the same readiness preflight, then simulate the merge with
/// the needs-rebase machinery. Exit code mirrors `check`: 0 when the merge
/// would proceed cleanly, otherwise the first blocker's code.
fn integrate_dry_run(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    target: &str,
    message: Option<&str>,
    selection: &IntegrationStyle,
    debt: Option<&DebtDeclaration>,
) -> Result<i32> {
    let recorded_target = st.target_branch.clone();
    let mut selected = st.clone();
    selected.target_branch = target.to_string();
    let st = &selected;
    let shape = selection.shape;
    // The refusals a real integration makes before touching anything: an
    // undeclared actor, and a target worktree that is missing or dirty. A dry
    // run that skipped them would report a merge the real path refuses.
    ctx.ensure_declared_actor(store)?;
    ctx.ensure_target_declares_actor(st)?;
    if let Some(contribution) = contribution_policy(ctx, st)? {
        let (projected, planned_debt) = project_debt(ctx, store, st, debt)?;
        let st = &projected;
        print_planned_debt(planned_debt.as_ref());
        let report = ctx.report_for_integration(store, st, target, shape)?;
        advise_role_trailers(&report);
        if !report.integrate_ready() {
            eprint!("{}", render::blocker_explanation(st, &report));
            println!(
                "dry-run: would not record {} ready to send ({})",
                st.change_id, report.ready_reason
            );
            return Ok(status::check_exit_code(&report));
        }
        let head = st
            .latest_patchset()
            .context("no patchset recorded")?
            .head
            .clone();
        contribution_shape(ctx, target, &head, contribution.history)?;
        println!(
            "dry-run: would record {} ready to send at {} ({} history); nothing would be merged",
            st.change_id,
            &head[..head.len().min(12)],
            contribution.history.as_str()
        );
        return Ok(0);
    }
    if shape == integration::Shape::FfOnly && ff_diverged(ctx, st, target)? {
        return Ok(refuse_divergence(st, target, &recorded_target));
    }
    let (projected, planned_debt) = project_debt(ctx, store, st, debt)?;
    let st = &projected;
    print_planned_debt(planned_debt.as_ref());
    let checkout = target_checkout(ctx, st, target)?;
    checkout_tracked_dirt(&checkout.path)?;
    let report = ctx.report_for_integration(store, st, target, shape)?;
    advise_role_trailers(&report);
    if !report.integrate_ready() {
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
    let outcome = gitio::merge_outcome(&ctx.cwd, &target_head, &approved_head)?;
    checkout_writes(&checkout, &target_head, outcome.tree.as_deref())?;
    let conflicts = outcome.conflicts;
    // Git would report the merge up to date and create no merge commit. A
    // plan that named parents for a merge that cannot exist would describe a
    // different integration than the one the guard would perform.
    let (_, already_contained) = integration::ancestry(&ctx.cwd, &target_head, &approved_head)?;
    if shape == integration::Shape::FfOnly && !already_contained {
        refuse_initialized_gitlink_changes(&checkout.path, &target_head, &approved_head)?;
    }
    let msg = message
        .map(str::to_string)
        .unwrap_or_else(|| format!("merge({}): {}", st.slug, st.title));

    println!("dry-run: would integrate {} into {target}", st.change_id);
    if selection.cleanup.enabled {
        println!("dry-run: would clean up {} with integration.discard = {}; checkout ownership and local content checks apply", st.change_id, selection.cleanup.discard.as_str());
    }
    if checkout.switch_from.is_some() {
        println!(
            "dry-run: would check out {target} in {} and merge there",
            checkout.path.display()
        );
    }
    if already_contained {
        println!(
            "  merge: none — {target_head} already contains {approved_head}; the change closes \
             there"
        );
    } else if shape == integration::Shape::FfOnly {
        println!("  fast-forward: {target_head} -> {approved_head}");
    } else {
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
    }
    if let Some(tree) = &outcome.tree {
        println!("  merge tree: {tree}");
    }
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
            target,
            selection,
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
    pub correspondence: Option<std::path::PathBuf>,
    pub abandoned: bool,
    pub superseded_by: Option<String>,
    pub external_reference: Option<String>,
}

pub fn close(ctx: &Ctx, reference: &str, args: CloseArgs) -> Result<()> {
    let CloseArgs {
        assert_integrated,
        patchset,
        into,
        target_before,
        correspondence,
        abandoned,
        superseded_by,
        external_reference,
    } = args;
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    // Graph before change, as every graph writer orders them: an asserted
    // integration reads the must-land-together frontier and must not race
    // a relation written beside it.
    let _graph = assert_integrated
        .is_some()
        .then(|| store.lock_graph())
        .transpose()?;
    let transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;
    if st.is_closed() {
        bail!("change {change_id} is already closed");
    }
    super::land::refuse_pending(&st)?;
    if assert_integrated.is_some() {
        super::land::refuse_alone(&st, &store.readable_states()?)?;
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
    if correspondence.is_some() && assert_integrated.is_none() {
        bail!("--correspondence describes an asserted integration; pass --assert-integrated <REV>");
    }
    if let Some(reference) = external_reference.as_deref() {
        if reference.trim().is_empty() || reference.contains('\n') || reference.contains('\r') {
            bail!("--external-reference must be one nonempty line");
        }
        if assert_integrated.is_none() {
            bail!("--external-reference requires --assert-integrated");
        }
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
            if let Some(reference) = external_reference.as_deref() {
                let external = st
                    .latest_external_verdict_for_revision(&patchset.head)
                    .filter(|external| {
                        external.verdict == crate::model::ExternalVerdict::Approved
                            && external.reference == reference
                    })
                    .with_context(|| {
                        format!(
                            "no external approval at {} has reference {reference:?}",
                            patchset.head
                        )
                    })?;
                let _ = external;
            }
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
            // authoritative. A correspondence stands in for the ancestry
            // proof, and is checked once `target_before` is known.
            if external_reference.is_none()
                && correspondence.is_none()
                && !gitio::is_ancestor(&ctx.cwd, &patchset.head, &rev)?
            {
                bail!(
                    "{rev} does not contain {} ({}), so it is not an integration of this change",
                    patchset.id,
                    &patchset.head[..patchset.head.len().min(8)]
                );
            }
            if !gitio::is_ancestor(&ctx.cwd, &rev, &gitio::branch_head(&ctx.cwd, &target)?)? {
                bail!("{rev} is not on {target}; nothing there integrated this change");
            }
            // A merge's first parent is the prior target when the merge itself
            // integrated the change. A fast-forward onto a merge head instead
            // makes that parent part of the change. An explicit ancestor is
            // recorded as asserted; only an absent flag permits inference.
            let parents = gitio::commit_parents(&ctx.cwd, &rev)?;
            let target_before = match (target_before, parents.len()) {
                (Some(named), _) => {
                    let base = gitio::rev_parse(&ctx.cwd, &named)?;
                    if !gitio::is_ancestor(&ctx.cwd, &base, &rev)? {
                        bail!("--target-before {named} ({base}) is not an ancestor of {rev}");
                    }
                    Some(base)
                }
                (None, 2..) => parents.into_iter().next(),
                (None, _) => None,
            };
            let correspondence = match (&correspondence, &target_before) {
                (Some(path), Some(before)) => crate::correspondence::check(
                    &ctx.cwd,
                    path,
                    &crate::correspondence::Subject {
                        change_id: &change_id,
                        patchset_id: &patchset.id,
                        base: &patchset.base,
                        head: &patchset.head,
                        target_before: before,
                        revision: &rev,
                    },
                )?,
                (Some(_), None) => bail!("--correspondence needs --target-before"),
                (None, _) => Vec::new(),
            };
            (
                Payload::IntegrationAsserted {
                    integrated_commit: rev.clone(),
                    source_patchset_id: patchset.id.clone(),
                    source_head: patchset.head.clone(),
                    target_branch: target,
                    target_before,
                    external_reference: external_reference.clone(),
                    correspondence,
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
    let mut ev = ctx.event(&store, &change_id, payload)?;
    store.append_local_event(&mut ev)?;
    drop(transition);
    println!("closed: {change_id}");
    println!("event: {}", ev.event_id);
    if let Err(error) = release_retention_refs(ctx, &change_id, integrated_rev.as_deref()) {
        eprintln!(
            "warning: closure recorded; retention cleanup failed: {error:#}. Remaining refs under {}, {} and {} stay pinned",
            gitio::retention_prefix(&change_id),
            gitio::tree_retention_prefix(&change_id),
            gitio::retained_prefix(&change_id),
        );
    }
    crate::journal::auto_log(ctx, &st.slug, &format!("closed change {change_id}"));
    advise_plan_promotions(&store, &st);
    super::slots::release_change_slot(ctx, &store, &st, None, None);
    Ok(())
}

/// Name the `journal consume` command when closing this change left its
/// `journal_ref` plan with no open promotion. Best-effort: the closure it
/// describes is already recorded, so a join that cannot be read is a warning
/// rather than a failed close.
pub(super) fn advise_plan_promotions(store: &Store, st: &crate::state::ChangeState) {
    let Some(plan) = st.journal_ref.as_deref() else {
        return;
    };
    match crate::journal::last_promotion_closed(store, plan) {
        Ok(true) => eprintln!(
            "advice: every promotion of {plan} has closed; consume it with \
             `arc journal consume {plan}`"
        ),
        Ok(false) => {}
        Err(error) => eprintln!("warning: could not check promotions of {plan}: {error:#}"),
    }
}

fn check(
    ctx: &Ctx,
    reference: &str,
    explain: bool,
    json: bool,
    shape: Option<integration::Shape>,
) -> Result<i32> {
    let changelog_none = super::advisory_changelog_none(&ctx.cwd);
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let shape =
        if st.is_closed() || crate::declarations::for_change(&ctx.cwd, &st)?.target_unreadable {
            shape.unwrap_or_default()
        } else {
            crate::style::resolved_style(
                &ctx.cwd,
                &st.target_branch,
                crate::style::StyleFlags {
                    shape,
                    ..Default::default()
                },
            )?
            .shape()
        };
    let mut report = ctx.report_for_integration(&store, &st, &st.target_branch, shape)?;
    let code = status::check_exit_code(&report);
    let states = store.readable_states()?;
    let debts = super::messaging::collect_debts(ctx, &states)?;
    report.advisories.extend(debts.advisories_for(ctx, &st));
    // Capacity information for the review action: shown when review is the
    // current action or one of the offered options, so an ordinary change
    // routed debt-first still sees what a review would cost, and a change
    // whose approval is satisfied names no queue at all.
    let review_is_live =
        report.next_action == "request_review" || report.review_options.contains(&"request_review");
    let review_queue = if review_is_live {
        Some(super::messaging::collect_review_queue(&store, &states)?)
    } else {
        None
    };
    let partial_coverage = st
        .debt
        .as_ref()
        .map(|debt| debt.partial_coverage.as_slice())
        .unwrap_or_default();
    if json {
        if let Some(queue) = &review_queue {
            if !queue.is_empty() {
                report.advisories.push(crate::status::Advisory {
                    code: "review-queue",
                    detail: queue.detail(),
                });
            }
        }
        let output = CheckOutput {
            gates: &report.gates,
            budget_errors: &report.budget_errors,
            budget_raises: &report.budget_raises,
            budget_rules: &report.budget_rules,
            review_context: report.review_context.as_ref(),
            schema: "arc-check/4",
            verdict_assessments: &report.verdict_assessments,
            selected_authorizing_event: report
                .verdict_assessments
                .iter()
                .find(|verdict| verdict.selected_authorizing_event)
                .map(|verdict| verdict.event_id.as_str()),
            change_id: &change_id,
            ready: report.integrate_ready(),
            exit_code: code,
            blockers: report
                .blockers()
                .iter()
                .map(|blocker| CheckBlocker {
                    blocker: blocker.as_str(),
                    exit_code: blocker.exit_code(),
                })
                .collect(),
            advisories: &report.advisories,
            changelog_none: changelog_none.as_deref(),
            bypassed: report
                .gates
                .iter()
                .filter_map(|gate| Some((gate.name.as_str(), gate.bypass.as_ref()?)))
                .collect(),
            debt_partial_coverage: partial_coverage,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(code);
    }
    if explain {
        print!("{}", render::check_explanation(&st, &report));
    } else if report.integrate_ready() {
        // A profile with no declared gate is ready, but "all integration
        // gates pass" would claim an evaluation nobody performed.
        if report.gates.is_empty() {
            println!(
                "ready: no gates declared for profile {}; nothing was evaluated",
                report.profile
            );
        } else {
            println!("ready: all integration gates pass");
        }
    } else {
        print!("{}", render::blocker_explanation(&st, &report));
    }
    if !explain {
        for gate in report
            .gates
            .iter()
            .filter(|gate| gate.base_pass.is_some() || gate.timing.concurrent_batch.is_some())
        {
            println!("gate {}: {}", gate.name, render::gate_line(gate));
        }
    }
    crate::render::verdict_assessments(&report.verdict_assessments);
    if let Some(queue) = &review_queue {
        queue.render();
    }
    render_changelog_none(changelog_none.as_deref());
    render::advisories(&report);
    for read in partial_coverage {
        println!("debt: {}", read.label());
    }
    Ok(code)
}

fn check_tagged(ctx: &Ctx, tags: Vec<String>, shape: Option<integration::Shape>) -> Result<i32> {
    let changelog_none = super::advisory_changelog_none(&ctx.cwd);
    let store = ctx.store()?;
    let states = store.readable_states()?;
    let selected = states
        .values()
        .filter(|state| tags.iter().all(|tag| state.tags.contains(tag)))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("no changes match tags {}", tags.join(", "));
    }
    render_changelog_none(changelog_none.as_deref());
    let mut aggregate = 0;
    for state in selected {
        if state.is_closed() {
            println!("{}: {}", state.change_id, change_status(state));
            continue;
        }
        let shape = if state.is_closed()
            || crate::declarations::for_change(&ctx.cwd, state)?.target_unreadable
        {
            shape.unwrap_or_default()
        } else {
            crate::style::resolved_style(
                &ctx.cwd,
                &state.target_branch,
                crate::style::StyleFlags {
                    shape,
                    ..Default::default()
                },
            )?
            .shape()
        };
        let report = ctx.report_for_integration(&store, state, &state.target_branch, shape)?;
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

fn render_changelog_none(reason: Option<&str>) {
    if let Some(reason) = reason {
        println!(".arc/changelog.toml: none = {reason:?}");
    }
}

/// Drop a change's retention refs only for heads proven reachable from
/// the integration commit. Everything else stays pinned: abandoned or
/// externally rewritten (squash/rebase) work must never become
/// GC-collectable through arc. Unpinning by hand remains possible with
/// `git update-ref -d refs/arc/keep/<change>/<patchset>`.
///
/// The commits held under `refs/arc/retained/<change>/` follow the same rule.
/// The ledger goes on citing them after the change closes, and the commit a
/// rewrite replaced is on no branch, so it is almost never one the
/// integration reaches; where one is, history holds it and the ref is
/// redundant.
pub(super) fn release_retention_refs(
    ctx: &Ctx,
    change_id: &str,
    integrated: Option<&str>,
) -> Result<()> {
    for (prefix, what) in [
        (gitio::retention_prefix(change_id), "head"),
        (gitio::retained_prefix(change_id), "commit"),
    ] {
        for (name, oid) in gitio::list_refs(&ctx.cwd, &prefix)? {
            let reachable = match integrated {
                Some(rev) => gitio::is_ancestor(&ctx.cwd, &oid, rev)?,
                None => false,
            };
            if reachable {
                gitio::delete_ref(&ctx.cwd, &name)?;
            } else {
                println!("kept {name}: {what} {oid} is not reachable from the integrated commit");
            }
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
                gitio::delete_ref(&ctx.cwd, &name)?;
            }
        }
    }
    Ok(())
}

struct DeclaredCheck {
    timings: Option<String>,
    profiled_gate: Option<gates::Gate>,
    declaration: Option<String>,
    adopt_budget_format: bool,
    budget: Option<crate::gate_budget::BudgetJudgement>,
    command: String,
    timeout: Option<u64>,
    environment: Option<String>,
    exclusive: Option<crate::exclusive::Request>,
}

/// The execution contract a `--gate` or `--command` selection names.
fn declared_check(
    ctx: &Ctx,
    st: &state::ChangeState,
    gate: Option<&str>,
    command: Option<String>,
) -> Result<DeclaredCheck> {
    match (gate, command) {
        (Some(name), None) => {
            let gates = ctx.declarations(st)?.gates;
            let declared = match gates.gates.get(name) {
                Some(declared) => declared,
                // A gate and a probe are different objects run by adjacent
                // flags. When the miss is a probe the brief already declares,
                // the error knows the right flag and should say it.
                None if brief_declares_probe(st, name) => bail!(
                    "gate {name:?} not declared in .arc/gates.toml; the current brief declares \
                     acceptance probe {name:?} — run `arc verify --probe {name}`"
                ),
                None => bail!("gate {name:?} not declared in .arc/gates.toml"),
            };
            Ok(DeclaredCheck {
                timings: declared.timings.clone(),
                profiled_gate: declared.coverage.base_passes.then(|| declared.clone()),
                declaration: gates::coverage_identity(declared),
                adopt_budget_format: gates.has_budgets(),
                budget: Some(declared.budget_judgement(None)),
                command: declared.command.clone(),
                timeout: declared.timeout,
                environment: declared.environment.clone(),
                exclusive: crate::exclusive::Request::for_gate(declared),
            })
        }
        (None, Some(command)) => Ok(DeclaredCheck {
            timings: None,
            profiled_gate: None,
            declaration: None,
            adopt_budget_format: false,
            budget: None,
            command,
            timeout: None,
            environment: None,
            exclusive: None,
        }),
        (Some(_), Some(_)) => bail!("--gate and --command are mutually exclusive"),
        (None, None) => bail!("{}", missing_selector(ctx, st)),
    }
}

/// An attested run happened where arc cannot observe an environment, so the
/// caller states the identity. A declaration with no probe has nothing for an
/// identity to be compared against, so one is refused rather than kept as
/// unreferenced metadata.
fn attested_environment(
    gate: Option<&str>,
    declares_probe: bool,
    environment: Option<String>,
) -> Result<Option<String>> {
    if declares_probe {
        return environment.map(Some).with_context(|| {
            format!(
                "--attest for gate {:?} requires --environment <IDENTITY>: the gate declares an \
                 environment probe",
                gate.unwrap_or_default()
            )
        });
    }
    if environment.is_some() {
        bail!("--environment applies only to a gate whose declaration has an environment probe");
    }
    Ok(None)
}

/// Gate evidence is open-only, so an attestation reaching an integrated
/// change is refused with the receipt form that does fit it.
fn refuse_late_attestation(st: &state::ChangeState, attest: bool) -> Result<()> {
    let integrated = st
        .closure
        .as_ref()
        .is_some_and(|closure| closure.outcome == Closure::Integrated);
    if attest && integrated {
        bail!(
            "change {} is integrated, and gate evidence is open-only; record a check observed \
             after integration with `arc verify {} --attest --after-integration ...`",
            st.change_id,
            st.slug
        );
    }
    Ok(())
}

/// What the caller reports about a check run against an integrated change.
struct Receipt {
    gate: Option<String>,
    command: Option<String>,
    result: VerifyResult,
    tested_revision: String,
    execution_host: String,
    runner: String,
    environment: Option<String>,
    note: Option<String>,
}

/// Retain a check observed after integration as a receipt for the content
/// that shipped.
///
/// A receipt is bound to the tested revision and its tree, and that tree must
/// be the integrated commit's: a receipt for other content would sit on this
/// change reading as verification of what it shipped. It is a separate event
/// from gate evidence, so it changes no authorization basis, discharges no
/// debt, and satisfies no gate.
fn verify_after_integration(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    st: &state::ChangeState,
    receipt: Receipt,
) -> Result<i32> {
    let integrated_commit = shipped_commit(st)?;
    let DeclaredCheck {
        command,
        timeout: timeout_seconds,
        environment: declared_environment,
        ..
    } = declared_check(ctx, st, receipt.gate.as_deref(), receipt.command)?;
    let environment = attested_environment(
        receipt.gate.as_deref(),
        declared_environment.is_some(),
        receipt.environment,
    )?;
    let integrated_tree = gitio::commit_tree(&ctx.cwd, &integrated_commit).with_context(|| {
        format!(
            "integrated commit {integrated_commit} does not resolve in this repository, so the \
             tested tree cannot be compared with what shipped"
        )
    })?;
    let tree = gitio::commit_tree(&ctx.cwd, &receipt.tested_revision)?;
    if tree != integrated_tree {
        bail!(
            "tested revision {} has tree {tree}, but integrated commit {integrated_commit} has \
             tree {integrated_tree}; a receipt after integration verifies only the content that \
             shipped, so test {integrated_commit} or a revision with its tree",
            receipt.tested_revision
        );
    }
    let _transition = store.lock_transition(change_id)?;
    let st = store.state(change_id)?;
    if shipped_commit(&st)? != integrated_commit {
        bail!("change {change_id} changed while the receipt was prepared; run it again");
    }
    // The receipt names the commit the integration recorded, which no
    // withdrawal can strand; views follow it forward like every revision.
    let recorded = state::reduce_with_repository(
        &store.load_events_with_landings(change_id)?,
        &store.rewrites()?,
        &store.repository_events()?,
    )?;
    let payload = Payload::PostIntegrationVerificationRecorded {
        gate: receipt.gate.clone(),
        command,
        revision: receipt.tested_revision.clone(),
        tree,
        integrated_commit: shipped_commit(&recorded)?,
        result: receipt.result,
        timeout_seconds,
        hostname: receipt.execution_host,
        runner: receipt.runner,
        environment: environment.map(|identity| EnvironmentEvidence {
            identity,
            producing_store: store.repository_id.clone(),
        }),
        note: receipt.note,
    };
    ensure_append_allowed(&st, &payload)?;
    let mut event = ctx.event(store, change_id, payload)?;
    store.append_local_event(&mut event)?;
    if let Some(gate) = &receipt.gate {
        println!("gate: {gate}");
    }
    println!(
        "verification after integration: {:?} (attested) at {} for integrated commit {}",
        receipt.result, receipt.tested_revision, integrated_commit
    );
    println!("event: {}", event.event_id);
    Ok(if receipt.result == VerifyResult::Pass {
        0
    } else {
        1
    })
}

/// The commit an integrated change shipped, or why a receipt has nothing to
/// bind to.
fn shipped_commit(st: &state::ChangeState) -> Result<String> {
    let Some(closure) = &st.closure else {
        bail!(
            "change {} is open; --after-integration records a check observed after the change \
             integrated. Record evidence for an open change with the ordinary attestation: \
             `arc verify {} --attest --result <pass|fail> --tested-revision <rev> \
             --execution-host <host> --runner <runner> --gate <name>`",
            st.change_id,
            st.slug
        );
    };
    if closure.outcome != Closure::Integrated {
        let outcome = match closure.outcome {
            Closure::Abandoned => "abandoned",
            _ => "superseded",
        };
        bail!(
            "change {} is {outcome}; only an integrated change shipped content a receipt can \
             verify",
            st.change_id
        );
    }
    closure.integrated_commit.clone().with_context(|| {
        format!(
            "change {} recorded no integrated commit, so nothing binds a receipt to what shipped",
            st.change_id
        )
    })
}

#[cfg(test)]
mod pipe_deadline_tests {
    use super::*;

    // The unit-test executable has no holder entry point. These tests exercise
    // process-group cleanup; CLI tests exercise the command holder and receipts.
    fn run_gate(
        cmd: &str,
        cwd: &Path,
        timeout_seconds: Option<u64>,
        output_root: &Path,
        producing_store: &str,
    ) -> Result<GateRun> {
        run_gate_in_shell(
            crate::slots::shell(cwd)?,
            0,
            cmd,
            cwd,
            timeout_seconds.map(Duration::from_secs),
            || crate::verification_output::Capture::new(output_root, producing_store),
        )
    }

    #[test]
    fn escaped_gate_descendant_cannot_extend_pipe_cleanup() {
        use crate::process_group::test_support::{HANG_BOUND, OUTLIVING_SECS};
        let cwd = tempfile::tempdir().unwrap();
        let pid_file = cwd.path().join("descendant.pid");
        let started = Instant::now();
        let run = run_gate(
            &format!("setsid sh -c 'echo $$ > descendant.pid; sleep {OUTLIVING_SECS}' & exit 0"),
            cwd.path(),
            Some(1),
            cwd.path(),
            "fixture",
        )
        .unwrap();
        let elapsed = started.elapsed();
        // Terminate the fixture-owned escaped group before asserting the bound.
        if let Ok(pid) = std::fs::read_to_string(&pid_file) {
            crate::process_group::kill_process_group(pid.trim().parse().unwrap()).unwrap();
        }
        assert!(elapsed < HANG_BOUND);
        assert!(run.status.success());
        assert!(run.timed_out);
        assert!(run.output_artifact.is_none());
    }

    /// A gate that passes must not leave a descendant running: arc stops the
    /// whole group it started before it returns the result. Rejects returning
    /// as soon as the shell exits and its pipe closes while a redirected
    /// background process lives on.
    #[test]
    fn a_passing_gate_leaves_no_descendant_running() {
        use crate::process_group::test_support::{ends_within, kill, read_pid, HANG_BOUND};
        let cwd = tempfile::tempdir().unwrap();
        let run = run_gate(
            &format!(
                "sleep {} >/dev/null 2>&1 & echo $! > descendant.pid",
                crate::process_group::test_support::OUTLIVING_SECS
            ),
            cwd.path(),
            Some(HANG_BOUND.as_secs()),
            cwd.path(),
            "fixture",
        )
        .unwrap();
        let descendant = read_pid(cwd.path(), "descendant.pid");
        let ended = ends_within(descendant, HANG_BOUND);
        if !ended {
            kill(descendant);
        }
        assert!(run.status.success());
        assert!(!run.timed_out);
        assert!(ended, "gate descendant {descendant} outlived the gate");
    }

    /// The gate's group ID stays reserved by its unreaped leader until arc
    /// has signalled the group. Rejects reaping the leader as soon as it
    /// exits: with the group's only other member escaped into its own
    /// session, the reaped ID is free for reuse, and a later kill of the
    /// group would reach whatever process group next takes that number.
    #[test]
    fn the_gate_leader_stays_reserved_until_its_group_is_stopped() {
        use crate::process_group::test_support::{leader_script, LeaderObservation, HANG_BOUND};
        let cwd = tempfile::tempdir().unwrap();
        let path = cwd.path().to_owned();
        let gate = thread::spawn(move || {
            run_gate(
                &leader_script(),
                &path,
                Some(HANG_BOUND.as_secs()),
                &path,
                "fixture",
            )
        });
        let observation = LeaderObservation::new(cwd.path());
        let reserved = observation.reserved_after_exit();
        observation.kill_escaped();
        let run = gate.join().unwrap().unwrap();
        assert!(
            reserved,
            "gate leader was reaped while its group could still be signalled"
        );
        assert!(run.status.success());
        assert!(!run.timed_out);
    }
}
