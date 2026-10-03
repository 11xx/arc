//! Gate verification keeps observed process evidence distinct from attestation.
//! Executed gates run in a process group, capture a bounded combined output
//! tail, and honor an optional declared timeout; attested gates carry only the
//! externally supplied result. Declared gates may run concurrently, but their
//! evidence is appended afterward in deterministic gate-name order.

use super::*;
use crate::integration::{
    self, Decision, IntegrationFacts, IntegrationPlan, Refusal, TargetCheckout,
};
use std::collections::BTreeMap;
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
    pub environment: Option<String>,
    pub note: Option<String>,
    pub waive_dirty: Option<String>,
    pub falsified_by: Option<String>,
    pub predicted: Option<String>,
    pub against: Option<String>,
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
    environment_identity: Option<String>,
    note: Option<String>,
    falsification: Option<Falsification>,
    tested_tree: Option<String>,
    worktree_dirty: Option<bool>,
    worktree_dirty_tracked: Option<bool>,
    worktree_dirty_untracked: Option<bool>,
    tree_moved: bool,
    against_target: Option<String>,
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
        environment,
        note,
        waive_dirty,
        falsified_by,
        predicted,
        against,
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
    // Reuse is a whole-gate-set question: it needs a set to choose within.
    // `--all` supplies one at the change's head, `--against` at the merged
    // tree; a single named check has nothing to select from.
    if skip_green && !all && against.is_none() {
        bail!("--skip-green requires --all or --against");
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
    let (change_id, st) = ctx.load_state(&store, reference)?;
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
    let st = if let Some(reason) = waive_dirty.as_deref() {
        let reason = reason.trim();
        if reason.is_empty() {
            bail!(
                "--waive-dirty must say why dirty evidence should count; an empty reason waives the gate without recording a reason"
            );
        }
        let revision = gitio::head(&run_ctx.cwd)?;
        let ev = ctx.event(
            &store,
            &change_id,
            Payload::DirtyTreeWaived {
                reason: reason.to_string(),
                revision: revision.clone(),
            },
        );
        ensure_append_allowed(&st, &ev.payload)?;
        store.append_event(&ev)?;
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
        let falsification = resolve_falsification(&st, None, &declaration.command, falsified_by)?;
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
        return verify_against(ctx, &store, &change_id, &st, &target, note, skip_green);
    }
    // Every path below records gate evidence, which status counts only at the
    // change's head.
    match &tested_revision {
        Some(revision) => warn_if_attested_off_head(ctx, &st, revision),
        None => ensure_at_change_head(run_ctx, &st)?,
    }
    if all {
        let gates = ctx.declarations(&st)?.gates;
        let required = gates.required_for(&st.profile);
        if required.is_empty() {
            bail!("no gates declared for profile {}", st.profile);
        }
        let total = required.len();
        let head = gitio::head(&run_ctx.cwd)?;
        let tree = gitio::commit_tree(&run_ctx.cwd, &head)?;
        let mode = if parallel {
            VerificationRunMode::Parallel
        } else {
            VerificationRunMode::Sequential
        };
        let run_id = start_verification_run(
            run_ctx, &store, &change_id, &head, mode, skip_green, &required,
        )?;
        // Applicability is a property of this checkout, so every declared
        // probe is read before reuse is decided. A probe is run once per
        // distinct command, however many gates declare it.
        let environments = gate_environments(
            &run_ctx.cwd,
            required.iter().map(|(name, gate)| (name.as_str(), *gate)),
        )?;
        let legacy_trees = status::legacy_evidence_trees(&st, &run_ctx.cwd);
        let resolve_tree = |revision: &str| legacy_trees.get(revision).cloned();
        let mut reused = Vec::new();
        let mut to_run = Vec::new();
        for (name, gate) in required {
            let reusable = skip_green
                .then(|| {
                    reusable_evidence(
                        &st,
                        name,
                        gate,
                        &tree,
                        &resolve_tree,
                        declared_environment(&environments, gate),
                    )
                })
                .flatten();
            if let Some(evidence) = reusable {
                println!(
                    "gate {name}: skipped (green at head; declared by {})",
                    gate.declared_by.join(", ")
                );
                reused.push((name.clone(), evidence.event_id.clone()));
            } else {
                to_run.push((name, gate));
            }
        }
        append_reuses(ctx, &store, &change_id, &run_id, &head, &tree, &reused)?;
        if parallel {
            return verify_all_parallel(
                run_ctx,
                &store,
                &change_id,
                to_run,
                total,
                reused.len(),
                &run_id,
                &head,
                &environments,
                note,
            );
        }
        let mut passed = reused.len();
        for (name, gate) in to_run {
            let result = record_verification(
                run_ctx,
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
                    environment_identity: declared_environment(&environments, gate)
                        .map(str::to_owned),
                    note: note.clone(),
                    falsification: None,
                    against_target: None,
                },
            )?;
            if result == 0 {
                passed += 1;
            }
        }
        println!("gates: {passed}/{total} pass");
        return Ok(if passed == total { 0 } else { 1 });
    }
    let (cmd, timeout, declared_environment) = match (&gate, command) {
        (Some(name), None) => {
            let gates = ctx.declarations(&st)?.gates;
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
            (
                declared.command.clone(),
                declared.timeout,
                declared.environment.clone(),
            )
        }
        (None, Some(c)) => (c, None, None),
        (Some(_), Some(_)) => bail!("--gate and --command are mutually exclusive"),
        (None, None) => bail!("provide --gate <name> or --command <cmd>"),
    };
    // An attested run happened where arc cannot observe an environment, so
    // the caller states the identity. A run arc performs reads it from the
    // gate's declared probe; a declaration with no probe has nothing for an
    // identity to be compared against, so one is refused rather than kept as
    // unreferenced metadata.
    let environment_identity = match (attest, &declared_environment) {
        (true, Some(_)) => Some(environment.with_context(|| {
            format!(
                "--attest for gate {:?} requires --environment <IDENTITY>: the gate declares an \
                 environment probe",
                gate.as_deref().unwrap_or_default()
            )
        })?),
        (true, None) => {
            if environment.is_some() {
                bail!(
                    "--environment applies only to a gate whose declaration has an environment \
                     probe"
                );
            }
            None
        }
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
    let falsification = resolve_falsification(&st, gate.as_deref(), &cmd, falsified_by)?;
    record_verification(
        run_ctx,
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
            environment_identity,
            note,
            falsification,
            against_target: None,
        },
    )
}

/// A detached checkout that lives only as long as one evaluation.
///
/// A synthesized merge is on no branch, so gating it needs a tree of its own.
/// Removal happens however the run ends, including a gate that fails or
/// errors: the checkout holds content nothing else refers to, and one left
/// behind makes the next evaluation refuse.
pub(super) struct ScratchWorktree {
    repo: PathBuf,
    pub(super) path: PathBuf,
}

impl ScratchWorktree {
    pub(super) fn create(repo: &Path, path: PathBuf, revision: &str) -> Result<Self> {
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
        Ok(Self {
            repo: repo.to_path_buf(),
            path,
        })
    }
}

impl Drop for ScratchWorktree {
    fn drop(&mut self) {
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
/// `skip_green` reuses the evidence already recorded at that merged tree
/// instead of rerunning the gate that produced it.
fn verify_against(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    st: &ChangeState,
    target: &str,
    note: Option<String>,
    skip_green: bool,
) -> Result<i32> {
    let mut selected = st.clone();
    selected.target_branch = target.to_string();
    let st = &selected;
    let declarations = ctx.declarations(st)?.gates;
    let required = declarations.required_for(&st.profile);
    if required.is_empty() {
        bail!("no gates declared for profile {}", st.profile);
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
    let run_id = start_verification_run(
        ctx,
        store,
        change_id,
        &synthesized,
        VerificationRunMode::Sequential,
        skip_green,
        &required,
    )?;
    let legacy_trees = status::legacy_evidence_trees(st, &ctx.cwd);
    let resolve_tree = |revision: &str| legacy_trees.get(revision).cloned();
    // A checkout exists only to run a command or a declared probe in, and a
    // probe reads the merged content: applicability is a property of the tree
    // that would ship. A declared probe therefore needs the checkout before
    // reuse can be decided; without one, a gate answered at this tree already
    // leaves nothing to run.
    let cheap_reusable = |name: &String, gate: &gates::Gate| {
        skip_green
            .then(|| reusable_evidence(st, name, gate, &merged_tree, &resolve_tree, None))
            .flatten()
    };
    let any_probe = required.iter().any(|(_, gate)| gate.environment.is_some());
    let answered_already = required
        .iter()
        .all(|(name, gate)| cheap_reusable(name, gate).is_some());
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
    let mut reused = Vec::new();
    let mut to_run = Vec::new();
    for (name, gate) in required {
        let reusable = skip_green
            .then(|| {
                reusable_evidence(
                    st,
                    name,
                    gate,
                    &merged_tree,
                    &resolve_tree,
                    declared_environment(&environments, gate),
                )
            })
            .flatten();
        if let Some(evidence) = reusable {
            println!(
                "gate {name}: skipped (green at the merged tree; declared by {})",
                gate.declared_by.join(", ")
            );
            reused.push((name.clone(), evidence.event_id.clone()));
        } else {
            to_run.push((name, gate));
        }
    }
    append_reuses(
        ctx,
        store,
        change_id,
        &run_id,
        &synthesized,
        &merged_tree,
        &reused,
    )?;

    let mut passed = reused.len();
    if !to_run.is_empty() {
        let scratch_ctx = scratch_ctx
            .as_ref()
            .context("a gate is left to run but its checkout was not created")?;
        for (name, gate) in to_run {
            let code = record_verification(
                scratch_ctx,
                store,
                change_id,
                VerificationInput {
                    run_id: Some(run_id.clone()),
                    probe: None,
                    gate: Some(name.clone()),
                    command: gate.command.clone(),
                    timeout_seconds: gate.timeout,
                    attested_result: None,
                    tested_revision: Some(synthesized.clone()),
                    execution_host: None,
                    runner: None,
                    environment_identity: declared_environment(&environments, gate)
                        .map(str::to_owned),
                    note: note.clone(),
                    falsification: None,
                    against_target: Some(target_head.clone()),
                },
            )?;
            if code == 0 {
                passed += 1;
            }
        }
    }
    println!("gates: {passed}/{total} pass at the merged tree");
    Ok(if passed == total { 0 } else { 1 })
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
    skip_green: bool,
    gates: &[(&String, &gates::Gate)],
) -> Result<String> {
    let _transition = store.lock_transition(change_id)?;
    let state = store.state(change_id)?;
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

/// The passing evidence `--skip-green` reuses for `name` at `tree`, if any.
///
/// Reuse is keyed by tree exactly as readiness is, so a gate a run skips is a
/// gate status already counts as green, whichever commit the run that answered
/// was against. Reuse is reuse of a *run*: the recorded command must be the
/// one declared now, and evidence from another environment ran, but not here.
/// Only the newest evidence that applies is a candidate, because it is the
/// one readiness counts; an older pass never stands in for it. The reuse
/// event names the tree, and replay checks it against the content key the
/// evidence itself carries, so newest evidence keyed only by a revision is
/// rerun.
fn reusable_evidence<'a>(
    st: &'a ChangeState,
    name: &str,
    gate: &gates::Gate,
    tree: &str,
    resolve_tree: &dyn Fn(&str) -> Option<String>,
    environment: Option<&str>,
) -> Option<&'a state::VerificationEntry> {
    st.gate_evidence_at_tree_matching(name, tree, resolve_tree, |evidence| {
        status::matches_declaration(evidence, gate)
            && status::matches_environment(evidence, gate, environment)
    })
    .filter(|evidence| {
        evidence.recorded_tree() == Some(tree)
            && evidence.green_at_head(st.dirty_tree_waiver.as_ref())
    })
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
    let mut state = state::reduce_following(&events, &store.rewrites()?)?;
    crate::replica::localize_change(&store.repository_id, &events, &mut state);
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
            tree: Some(tree.to_owned()),
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

#[allow(clippy::too_many_arguments)]
pub fn snapshot_with_verify(
    ctx: &Ctx,
    reference: &str,
    base: Option<String>,
    brief_version: Option<usize>,
    verify_requested: bool,
    gates: Vec<String>,
    all: bool,
    contributors: Option<Vec<String>>,
    solo: bool,
    journal_refs: Vec<String>,
    thread: Option<String>,
) -> Result<i32> {
    if !verify_requested && (!gates.is_empty() || all) {
        bail!("--gate and --all require --verify");
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
    verify_recorded(ctx, reference, verify_requested, gates, all)
}

/// The verification `snapshot --verify` runs once the patchset is recorded:
/// every required gate, or the named ones.
fn verify_recorded(
    ctx: &Ctx,
    reference: &str,
    verify_requested: bool,
    gates: Vec<String>,
    all: bool,
) -> Result<i32> {
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
                environment: None,
                note: None,
                waive_dirty: None,
                falsified_by: None,
                predicted: None,
                against: None,
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
        let revision = gitio::head(&run_ctx.cwd)?;
        let run_id = start_verification_run(
            run_ctx,
            &store,
            &change_id,
            &revision,
            VerificationRunMode::Sequential,
            false,
            &selected,
        )?;
        let environments = gate_environments(
            &run_ctx.cwd,
            selected.iter().map(|(name, gate)| (name.as_str(), *gate)),
        )?;
        let total = selected.len();
        let mut passed = 0;
        for (name, gate) in selected {
            let code = record_verification(
                run_ctx,
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
                    environment_identity: declared_environment(&environments, gate)
                        .map(str::to_owned),
                    note: None,
                    falsification: None,
                    against_target: None,
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
                environment: None,
                note: None,
                waive_dirty: None,
                falsified_by: None,
                predicted: None,
                against: None,
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
                environment: None,
                note: None,
                waive_dirty: None,
                falsified_by: None,
                predicted: None,
                against: None,
            },
        )?;
    }
    check(ctx, reference, false, false, None)
}

/// Replay a change's branch onto its target branch and record the result.
///
/// The rebase runs in the worktree that holds the branch: its index and reflog
/// are the only place a stopped rebase can be continued from. A conflict stops
/// the rebase and leaves it in progress, because the partial resolution is a
/// person's work and aborting would discard it.
pub fn rebase(
    ctx: &Ctx,
    reference: &str,
    verify_requested: bool,
    contributors: Option<Vec<String>>,
    solo: bool,
) -> Result<i32> {
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

    match gitio::rebase(&wt, &target_head)? {
        gitio::RebaseOutcome::Stopped => {
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
        gitio::RebaseOutcome::Replayed => {
            let replayed = gitio::branch_head(&ctx.cwd, &st.branch)?;
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
            let patchset_id = super::review::snapshot_holding(
                ctx,
                &store,
                &change_id,
                &transition,
                None,
                None,
                super::review::SnapshotRequest::attributed(requested_contributors),
            )?;
            // Verification takes the lock for each run it records.
            drop(transition);
            let code = verify_recorded(ctx, reference, verify_requested, Vec::new(), false)?;
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
            environment_identity: declared_environment(environments, gate).map(str::to_owned),
            note: note.clone(),
            falsification: None,
            against_target: None,
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

/// Turn `--falsified-by`/`--predicted` into the reference that will be
/// recorded, or refuse.
///
/// The revision is read from the referenced failure rather than asked for: the
/// two can then never disagree, and there is no third value for a caller to
/// get wrong. Refusal happens before the command runs, because a verification
/// arc will not record is a command nobody should have paid for.
fn resolve_falsification(
    state: &ChangeState,
    gate: Option<&str>,
    command: &str,
    falsified_by: Option<(String, String)>,
) -> Result<Option<Falsification>> {
    let Some((event_id, predicted_reason)) = falsified_by else {
        return Ok(None);
    };
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
    let path = super::lifecycle::worktree_path_for(&ctx.cwd, &name)?;
    let quoted_path = crate::context::shell_quote(&path.display().to_string());
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
    let revision = match &input.tested_revision {
        Some(revision) => revision.clone(),
        None => gitio::head(command_cwd)?,
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
        Some(gitio::worktree_tree(command_cwd)?)
    } else {
        None
    };
    let mut completed = execute_verification(input, command_cwd, revision.clone(), hostname)?;
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
        environment_identity,
        note,
        falsification,
        against_target,
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
        environment_identity,
        note,
        falsification,
        against_target,
        // Filled in by the caller, which is what sees the worktree on both
        // sides of the run.
        tested_tree: None,
        worktree_dirty: None,
        worktree_dirty_tracked: None,
        worktree_dirty_untracked: None,
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
    let mut st = state::reduce_following(&events, &store.rewrites()?)?;
    crate::replica::localize_change(&store.repository_id, &events, &mut st);
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
        let falsification_inferred = (item.result == VerifyResult::Pass && item.probe.is_none())
            .then(|| {
                state::infer_falsification(&st.verifications, item.gate.as_deref(), &item.command)
            })
            .flatten();
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

pub(super) struct GateRun {
    pub(super) status: ExitStatus,
    pub(super) output_tail: Option<String>,
    pub(super) timed_out: bool,
}

pub(super) fn run_gate(cmd: &str, cwd: &Path, timeout_seconds: Option<u64>) -> Result<GateRun> {
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

/// The review obligation an integration declares in place of a verdict
/// nobody recorded, as the caller spelled it.
pub struct DebtDeclaration {
    pub reason: String,
    pub kind: Option<DebtMissing>,
}

/// Everything an integration was asked for beyond which changes to land.
pub struct IntegrateArgs {
    pub shape: Option<integration::Shape>,
    pub tags: Vec<String>,
    pub into: Option<String>,
    pub message: Option<String>,
    pub cleanup: Option<bool>,
    pub dry_run: bool,
    /// Print the dry run's plan as `arc-integration-plan` JSON.
    pub json: bool,
    /// A plan an earlier dry run printed, to compare the fresh decision with.
    pub expect_basis: Option<PathBuf>,
    pub debt: Option<DebtDeclaration>,
}

pub fn integrate(ctx: &Ctx, references: &[String], args: IntegrateArgs) -> Result<i32> {
    let IntegrateArgs {
        shape,
        tags,
        into,
        message,
        cleanup,
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
                    ..Default::default()
                },
            )?;
            let flags = crate::style::StyleFlags {
                shape,
                cleanup,
                ..Default::default()
            };
            let shape = style.shape();
            let cleanup = style.cleanup();
            if shape == integration::Shape::FfOnly && message.is_some() {
                bail!("--message names a merge commit; select --shape no-ff");
            }
            integrate_one(
                ctx,
                reference,
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
                    debt: debt.filter(|_| !dry_run),
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
            integrate_queue(ctx, selection, cleanup, dry_run, shape)
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
                    ..Default::default()
                },
                &mut projected_targets,
                &mut unresolved_targets,
                &mut projected_closures,
            )
        } else {
            queue_step(
                &queue_ctx,
                &store,
                change_id,
                crate::style::StyleFlags {
                    shape,
                    cleanup,
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
fn queue_step(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    flags: crate::style::StyleFlags,
    cleanup_checkout: &Path,
) -> Result<QueueStep> {
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
    ctx.ensure_declared_actor(store)?;
    ctx.ensure_target_declares_actor(&st)?;
    let policy = crate::policy::load_at(&ctx.cwd, &target_head)?;
    let style = crate::style::resolved_style_with_policy(&policy, flags.clone())?;
    integration_history_with_policy(ctx, &st, &target_head, &policy)?;
    if let (Some(contribution), Some(patchset)) = (&policy.contribution, st.latest_patchset()) {
        contribution_shape(ctx, &st.target_branch, &patchset.head, contribution.history)?;
    }
    let shape = style.shape();
    let cleanup = style.cleanup();
    println!("{change_id}: integrating into {}", st.target_branch);

    if shape == integration::Shape::FfOnly && ff_diverged(ctx, &st, &st.target_branch)? {
        return Ok(QueueStep::Stopped {
            code: refuse_divergence(&st, &st.target_branch, &st.target_branch),
            reason: "target diverged; rebase and fresh review or declared debt required".into(),
        });
    }
    let mut report = ctx.report_for_integration(store, &st, &st.target_branch, shape)?;
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
    if shape == integration::Shape::NoFf && report.needs_rebase {
        let code = rebase(ctx, change_id, false, None, false)?;
        if code != 0 {
            return Ok(QueueStep::Stopped {
                code,
                reason: format!("replaying onto {} needs a person", st.target_branch),
            });
        }
        let st = store.state(change_id)?;
        report = ctx.report_for_integration(store, &st, &st.target_branch, shape)?;
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
        let code = verify_against(ctx, store, change_id, &st, &target, None, true)?;
        let st = store.state(change_id)?;
        report = ctx.report_for_integration(store, &st, &st.target_branch, shape)?;
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
        change_id,
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
                eprintln!("resolve review authorization with arc review {change_id} --verdict approved; a refusing verdict requires a superseding review");
            }
            return Ok(QueueStep::Stopped {
                code: blocker.exit_code(),
                reason: blocker.as_str().into(),
            });
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

/// The resolved behavior and the flags whose precedence each reading preserves.
struct IntegrationStyle {
    shape: integration::Shape,
    cleanup: bool,
    flags: crate::style::StyleFlags,
}

impl IntegrationStyle {
    fn validate(&self, policy: &crate::policy::PolicyFile) -> Result<()> {
        let style = crate::style::resolved_style_with_policy(policy, self.flags.clone())
            .map_err(|_| Refusal::ConfigurationMoved.into_error())?;
        if style.shape() != self.shape || style.cleanup() != self.cleanup {
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
fn target_checkout(ctx: &Ctx, st: &ChangeState, target: &str) -> Result<TargetCheckout> {
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
    options: SingleOptions,
) -> Result<i32> {
    let SingleOptions {
        cleanup_checkout,
        debt,
        shape,
        style_flags,
        dry_run,
        json,
        expect_basis,
    } = options;
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    if let Some(expected) = &expect_basis {
        if expected.change_id != change_id {
            bail!(
                "the expected basis describes {}, not {change_id}",
                expected.change_id
            );
        }
    }
    let initial = store.state(&change_id)?;
    if initial.iterating && (dry_run || debt.is_none()) {
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
            return integrate_dry_run_json(ctx, &store, &initial, &target, &selection);
        }
        return integrate_dry_run(
            ctx,
            &store,
            &initial,
            &target,
            message.as_deref(),
            &selection,
        );
    }
    // The same store the merge's closure event will be appended to, so the
    // merge and the record are judged by one reading of the policy.
    ctx.ensure_declared_actor(&store)?;
    let mut selected = initial.clone();
    selected.target_branch = target.clone();
    ctx.ensure_target_declares_actor(&selected)?;
    // Cross-change order is always target, then change. This serializes the
    // target worktree without allowing an integration/metadata lock cycle.
    let target_lock = store.lock_target(&target)?;
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
                    integration_facts(ctx, &store, &st, &report, &target, &selection),
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
            super::audit::declare_debt_locked(ctx, &store, &st, &debt.reason, debt.kind)?;
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
    let report = ctx.report_for_integration(&store, &st, &target, shape)?;
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
    let facts = integration_facts(ctx, &store, &st, &report, &target, &selection);
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
    if actual_shape == crate::model::IntegrationShape::FastForward {
        refuse_initialized_gitlink_changes(&wt, &old_target, &approved_head)?;
    }
    let already_contained = actual_shape == crate::model::IntegrationShape::AlreadyContained;
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
            already_contained,
            shape: Some(actual_shape),
        },
    );
    if let Err(error) = store.append_event(&ev) {
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
    // Git updates the checkout before the ledger records the closure.
    // Retention and worktree cleanup are post-closure maintenance and may
    // invoke Git hooks or other arc commands, so no state lock spans them.
    drop(transition);
    drop(target_lock);
    if already_contained {
        println!("integrated: {merged} (already contained {approved_head}; no merge created)");
    } else {
        println!("integrated: {merged}");
    }
    println!("event: {}", ev.event_id);
    if let Err(error) = release_retention_refs(ctx, &change_id, Some(&merged)) {
        eprintln!(
            "warning: integration recorded; retention cleanup failed: {error:#}. Remaining refs under {} and {} stay pinned",
            gitio::retention_prefix(&change_id),
            gitio::tree_retention_prefix(&change_id),
        );
    }
    if let Some(advisory) = crate::status::missing_changelog_advisory(&st) {
        eprintln!("advice: {}", advisory.detail);
    }
    crate::journal::auto_log(
        ctx,
        &st.slug,
        &format!("integrated {change_id} at {merged}"),
    );
    advise_plan_promotions(&store, &st);

    if cleanup {
        let checkout = cleanup_checkout.as_deref().unwrap_or(&ctx.cwd);
        if let Err(error) = cleanup_integrated_change(ctx, &st, &wt, checkout) {
            eprintln!(
                "warning: integration recorded; cleanup for branch {} failed: {error:#}",
                st.branch
            );
            println!("kept branch {}: cleanup could not complete", st.branch);
        }
    }
    Ok(0)
}

fn cleanup_integrated_change(
    ctx: &Ctx,
    st: &ChangeState,
    target: &Path,
    checkout: &Path,
) -> Result<()> {
    if let Some(wt_path) = &st.worktree {
        let path = PathBuf::from(wt_path);
        if path.exists() {
            let canonical_path = path.canonicalize()?;
            let reason = if !st.worktree_created {
                Some("not created by arc".to_string())
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
            } else if let Some(content) = cleanup_local_content(&path)? {
                Some(format!("holds local content not in the branch: {content}"))
            } else {
                match gitio::git(target, &["worktree", "remove", wt_path]) {
                    Ok(_) => {
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

fn cleanup_local_content(checkout: &Path) -> Result<Option<String>> {
    let status = gitio::git(
        checkout,
        &[
            "status",
            "--porcelain",
            "-z",
            "--ignored",
            "--untracked-files=all",
        ],
    )?;
    let unused_outbox = unused_outbox_metadata(checkout)?;
    let mut entries = status.split('\0').filter(|entry| !entry.is_empty());
    let mut paths = Vec::new();
    while let Some(entry) = entries.next() {
        if let Some(path) = entry.get(3..) {
            let metadata = unused_outbox
                && matches!(&entry[..2], "??" | "!!")
                && matches!(path, ".arc/outbox/" | ".arc/outbox/.gitignore");
            if !metadata {
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
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Ok(None);
    }
    let mut summary = paths.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    if paths.len() > 5 {
        summary.push_str(", ...");
    }
    Ok(Some(summary))
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

    let mut prerequisites = Vec::new();
    for blocker in &st.blocked_by {
        // A basis missing a prerequisite is a basis that misstates what was
        // checked, so an unreadable one refuses the merge rather than being
        // quietly omitted.
        let events = store.load_events(blocker).with_context(|| {
            format!("prerequisite {blocker} cannot be read, so the authorization basis                      would be incomplete")
        })?;
        let mut blocker_id = blocker.clone();
        let mut blocker_state = state::reduce_following(&events, &store.rewrites()?)?;
        crate::replica::localize_change(&store.repository_id, &events, &mut blocker_state);
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
            blocker_state = state::reduce_following(&events, &store.rewrites()?)?;
            crate::replica::localize_change(&store.repository_id, &events, &mut blocker_state);
        }
        if let Some(closure) = &blocker_state.closure {
            prerequisites.push(crate::model::PrerequisiteClosure {
                change_id: blocker_id,
                closure_event_id: closure.event_id.clone(),
                integrated_commit: closure.integrated_commit.clone(),
            });
        }
    }

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

    Ok(crate::model::AuthorizationBasis {
        verdict_event_id: approval.verdict_event_id,
        external_verdict: approval.external_verdict,
        verdict_provisional: approval.verdict_provisional,
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

fn ensure_destination_actor(ctx: &Ctx, policy: &crate::policy::PolicyFile) -> Result<()> {
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

fn normalized_declarations(
    declarations: crate::declarations::Declarations,
    st: &ChangeState,
) -> integration::Declarations {
    let (gates, policy) = (declarations.gates, declarations.policy);
    let gates = gates
        .required_for(&st.profile)
        .into_iter()
        .map(|(name, gate)| {
            (
                name.clone(),
                crate::model::NormalizedGate {
                    command: gate.command.clone(),
                    profiles: gate.profiles.clone(),
                    timeout: gate.timeout,
                    declared_by: gate.declared_by.clone(),
                },
            )
        })
        .collect();
    integration::Declarations {
        gates,
        policy: crate::model::NormalizedPolicy {
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
    let event = ctx.event(store, &st.change_id, payload);
    store.append_event(&event)?;
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
/// change and its target once each. Nothing is written.
fn integration_facts(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    report: &crate::status::StatusReport,
    target: &str,
    selection: &IntegrationStyle,
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
    // Read after the first basis: agreement between the two is the evidence
    // that the configuration held still across the decision.
    let confirmation = ctx
        .report_for_integration(store, st, target, shape)
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

/// The plan `integrate` would carry out, decided from the same facts and by
/// the same function as the merge, printed as JSON. Nothing is locked or
/// written.
fn integrate_dry_run_json(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    target: &str,
    selection: &IntegrationStyle,
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
    let report = ctx.report_for_integration(store, st, target, shape)?;
    let facts = integration_facts(ctx, store, st, &report, target, selection);
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
    println!("{}", serde_json::to_string_pretty(&plan)?);
    Ok(0)
}

fn integrate_dry_run(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    target: &str,
    message: Option<&str>,
    selection: &IntegrationStyle,
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
        let report = ctx.report_for_integration(store, st, target, shape)?;
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
    let checkout = target_checkout(ctx, st, target)?;
    checkout_tracked_dirt(&checkout.path)?;
    let report = ctx.report_for_integration(store, st, target, shape)?;
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
        abandoned,
        superseded_by,
        external_reference,
    } = args;
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;
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
            // authoritative.
            if external_reference.is_none() && !gitio::is_ancestor(&ctx.cwd, &patchset.head, &rev)?
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
            (
                Payload::IntegrationAsserted {
                    integrated_commit: rev.clone(),
                    source_patchset_id: patchset.id.clone(),
                    source_head: patchset.head.clone(),
                    target_branch: target,
                    target_before,
                    external_reference: external_reference.clone(),
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
    drop(transition);
    println!("closed: {change_id}");
    println!("event: {}", ev.event_id);
    if let Err(error) = release_retention_refs(ctx, &change_id, integrated_rev.as_deref()) {
        eprintln!(
            "warning: closure recorded; retention cleanup failed: {error:#}. Remaining refs under {} and {} stay pinned",
            gitio::retention_prefix(&change_id),
            gitio::tree_retention_prefix(&change_id),
        );
    }
    crate::journal::auto_log(ctx, &st.slug, &format!("closed change {change_id}"));
    advise_plan_promotions(&store, &st);
    Ok(())
}

/// Name the `journal consume` command when closing this change left its
/// `journal_ref` plan with no open promotion. Best-effort: the closure it
/// describes is already recorded, so a join that cannot be read is a warning
/// rather than a failed close.
fn advise_plan_promotions(store: &Store, st: &crate::state::ChangeState) {
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
            schema: "arc-check/3",
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
    if let Some(queue) = &review_queue {
        queue.render();
    }
    render::advisories(&report);
    Ok(code)
}

fn check_tagged(ctx: &Ctx, tags: Vec<String>, shape: Option<integration::Shape>) -> Result<i32> {
    let store = ctx.store()?;
    let states = store.readable_states()?;
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
            gitio::delete_ref(&ctx.cwd, &name)?;
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
                gitio::delete_ref(&ctx.cwd, &name)?;
            }
        }
    }
    Ok(())
}
