//! `arc candidate verify | select | promote`, and the read requirements a
//! brief declares with `--must-read`.
//!
//! An evaluation runs a required gate against a candidate's tree in a
//! scratch checkout. A selection validates a named choice against every
//! ground and records the basis it rests on; the choice is always the
//! caller's. A promotion is the selection's effect: a transaction that moves
//! the destination branch to the shipped tree and records the patchset, with
//! a ref written before the branch moves so an interrupted run is found and
//! finished or discarded.

use super::candidate::load_ledger;
use super::gatekeeping::{
    checkout_tracked_dirt, checkout_writes, declared_environment, gate_environments, run_gate,
    ScratchWorktree,
};
use super::relations::repository_relations;
use super::Ctx;
use crate::candidate::{self, Ledger, Selection};
use crate::gitio;
use crate::integration::TargetCheckout;
use crate::model::{
    EnvironmentEvidence, PatchsetCandidate, Payload, ReadRequirement, RequiredExtent, VerifyResult,
};
use crate::relations::{self, line_range, Read};
use crate::selection::{self, Observations, Proposal, RequiredGate, RequiredRead};
use crate::state::{self, ChangeState};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use std::path::Path;

/// The requirements `--must-read` locators name, each resolved now and
/// named once.
pub(crate) fn read_requirements(ctx: &Ctx, locators: &[String]) -> Result<Vec<ReadRequirement>> {
    let mut requirements = Vec::new();
    for locator in locators {
        let requirement = read_requirement(ctx, locator)?;
        if !requirements.contains(&requirement) {
            requirements.push(requirement);
        }
    }
    Ok(requirements)
}

/// A journal artifact, `<file>` or `<journal-dir>::<file>` with an optional
/// `@sha256:<hex>`, or a file at a revision, `<revision>:<path>[:<from>-<to>]`.
fn read_requirement(ctx: &Ctx, locator: &str) -> Result<ReadRequirement> {
    if let Some((file, hex)) = locator.rsplit_once("@sha256:") {
        let digest = format!("sha256:{}", hex.to_ascii_lowercase());
        if !relations::is_digest(&digest) {
            bail!("unknown-locator: {locator:?} names no `sha256:` digest");
        }
        return Ok(ReadRequirement::Artifact {
            file: artifact_name(ctx, locator, file)?,
            digest,
        });
    }
    if locator.contains(crate::journal::REFERENCE_SEPARATOR) || !locator.contains(':') {
        let file = artifact_name(ctx, locator, locator)?;
        let digest = crate::journal::artifact_digest(ctx, locator)
            .map_err(|error| anyhow::anyhow!("unknown-locator: {locator:?}: {error:#}"))?;
        return Ok(ReadRequirement::Artifact { file, digest });
    }
    file_requirement(ctx, locator)
}

/// An artifact as a read of it names it: by file name in this project's
/// journal, as `<journal-dir>::<file>` in another project's.
fn artifact_name(ctx: &Ctx, locator: &str, reference: &str) -> Result<String> {
    let location = crate::journal::locate_artifact(ctx, reference)
        .map_err(|error| anyhow::anyhow!("unknown-locator: {locator:?}: {error:#}"))?;
    if crate::journal::parse_artifact_name(&location.file).is_none() {
        bail!(
            "unknown-locator: {locator:?} is not a journal artifact name \
             (<timestamp>-<topic>-<kind>.md)"
        );
    }
    let archived = crate::journal::archive_dir(&location.hot).join(&location.file);
    if !location.hot.join(&location.file).is_file() && !archived.is_file() {
        let searched = format!("{} or its cold archive", location.hot.display());
        let missing = crate::journal::missing_artifact(&location, &searched);
        bail!("unknown-locator: {locator:?}: {missing}");
    }
    if !location.foreign {
        return Ok(location.file);
    }
    let hot = std::fs::canonicalize(&location.hot).unwrap_or(location.hot);
    Ok(format!(
        "{}{}{}",
        hot.display(),
        crate::journal::REFERENCE_SEPARATOR,
        location.file
    ))
}

fn file_requirement(ctx: &Ctx, locator: &str) -> Result<ReadRequirement> {
    let (revision, rest) = locator
        .split_once(':')
        .context("a file locator is <revision>:<path>[:<from>-<to>]")?;
    let (path, extent) = match rest.rsplit_once(':') {
        Some((path, range)) if is_range(range) => {
            let (from, to) = range.split_once('-').unwrap_or_default();
            let (from, to) = (from.parse::<u64>()?, to.parse::<u64>()?);
            if from == 0 || to < from {
                bail!("unknown-locator: {locator:?}: lines {range} is not a one-based, ascending range");
            }
            (path, RequiredExtent::Lines { from, to })
        }
        _ => (rest, RequiredExtent::Whole),
    };
    let path = path.trim_start_matches("./");
    if revision.is_empty() || revision.starts_with('-') || path.is_empty() {
        bail!("unknown-locator: {locator:?} is not <revision>:<path>[:<from>-<to>]");
    }
    let commit = gitio::rev_parse(&ctx.cwd, &format!("{revision}^{{commit}}"))
        .map_err(|_| anyhow::anyhow!("unknown-locator: {revision:?} names no commit"))?;
    let Some(blob) = gitio::blob_oid(&ctx.cwd, &commit, path) else {
        bail!("unknown-locator: {path:?} names no file at {commit}");
    };
    if let RequiredExtent::Lines { from, to } = extent {
        let bytes = gitio::blob_bytes(&ctx.cwd, &blob)?;
        let lines = bytes.split_inclusive(|byte| *byte == b'\n').count() as u64;
        if to > lines || line_range(&bytes, from, to).is_none() {
            bail!("unknown-locator: {path} at {commit} has {lines} lines, not {from}-{to}");
        }
    }
    Ok(ReadRequirement::File {
        revision: commit,
        path: path.to_string(),
        blob,
        extent,
    })
}

fn is_range(text: &str) -> bool {
    text.split_once('-').is_some_and(|(from, to)| {
        !from.is_empty()
            && !to.is_empty()
            && from.bytes().all(|b| b.is_ascii_digit())
            && to.bytes().all(|b| b.is_ascii_digit())
    })
}

/// A repository event sorting after every one already recorded, so a replay
/// meets a selection before its promotion and a registration before its
/// evaluations. Taken under the repository-events lock.
fn repository_event(ctx: &Ctx, store: &Store, payload: Payload) -> Result<crate::model::Event> {
    let mut event = ctx.event(store, Store::REPOSITORY_SCOPE, payload);
    let latest = store
        .load_repository_events()?
        .into_iter()
        .map(|event| event.event_id)
        .max();
    if let Some(latest) = latest {
        if event.event_id <= latest {
            event.event_id = super::event_id_after(&latest)?;
        }
    }
    Ok(event)
}

/// Whether the destination's phase admits the patchset a promotion ends in,
/// asked before anything is written.
fn patchset_admitted(st: &ChangeState) -> Result<()> {
    super::ensure_append_allowed(
        st,
        &Payload::PatchsetAdded {
            patchset_id: String::new(),
            base: String::new(),
            head: String::new(),
            merge_base: None,
            brief_ref: None,
            author_name: None,
            author_email: None,
            committer_name: None,
            committer_email: None,
            contributors: Vec::new(),
            claim_id: None,
            claim_actor: None,
            journal_refs: Vec::new(),
            thread: None,
            candidate: None,
        },
    )
}

/// The change a candidate's contract is recorded on, as its ledger holds it.
fn contract_state(ctx: &Ctx, store: &Store, change_id: &str) -> Result<ChangeState> {
    Ok(ctx.load_state(store, change_id)?.1)
}

/// A checkout holding exactly `tree`, removed when dropped.
fn scratch_checkout(ctx: &Ctx, tree: &str, name: &str) -> Result<ScratchWorktree> {
    let commit = gitio::commit_tree_with_parents(&ctx.cwd, tree, &[], "arc candidate checkout")?;
    let path = super::lifecycle::worktree_path_for(&ctx.cwd, name)?;
    ScratchWorktree::create(&ctx.cwd, path, &commit)
}

pub fn verify(ctx: &Ctx, candidate_id: &str, only: Option<&str>) -> Result<i32> {
    let store = ctx.store()?;
    let ledger = load_ledger(&store)?;
    let Some(registration) = ledger.registration(candidate_id) else {
        bail!("unknown-candidate: no candidate {candidate_id} is registered");
    };
    let st = contract_state(ctx, &store, &registration.brief.change_id)?;
    let declarations = ctx.declarations(&st)?;
    if declarations.target_unreadable {
        bail!(
            "target-unreadable: {} cannot be resolved, so nothing says which gates {} owes",
            st.target_branch,
            st.change_id
        );
    }
    let required = declarations.gates.required_for(&st.profile);
    let selected: Vec<_> = match only {
        Some(name) => {
            let found: Vec<_> = required
                .into_iter()
                .filter(|(gate, _)| gate.as_str() == name)
                .collect();
            if found.is_empty() {
                bail!(
                    "unknown-gate: {name} is not a gate {} is required to pass",
                    st.change_id
                );
            }
            found
        }
        None => required,
    };
    if selected.is_empty() {
        bail!("no gates declared for profile {}", st.profile);
    }
    let tree = registration.tree.clone();
    let scratch = scratch_checkout(ctx, &tree, &format!("candidate-{candidate_id}"))?;
    println!("candidate: {candidate_id}");
    println!("tree: {tree}");
    let environments = gate_environments(
        &scratch.path,
        selected.iter().map(|(name, gate)| (name.as_str(), *gate)),
    )?;
    let hostname = hostname::get()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());
    let total = selected.len();
    let mut passed = 0;
    for (name, gate) in selected {
        eprintln!("running: {}", gate.command);
        let started = std::time::Instant::now();
        let run = run_gate(&gate.command, &scratch.path, gate.timeout)?;
        let duration_ms = started.elapsed().as_millis() as u64;
        let result = if run.status.success() && !run.timed_out {
            VerifyResult::Pass
        } else {
            VerifyResult::Fail
        };
        let payload = Payload::CandidateVerified {
            candidate_id: candidate_id.to_string(),
            tree: tree.clone(),
            gate: name.clone(),
            command: gate.command.clone(),
            timeout_seconds: gate.timeout,
            environment_probe: gate.environment.clone(),
            environment: declared_environment(&environments, gate).map(|identity| {
                EnvironmentEvidence {
                    identity: identity.to_string(),
                    producing_store: store.repository_id.clone(),
                }
            }),
            result,
            exit_code: Some(run.status.code().unwrap_or(-1)),
            duration_ms: Some(duration_ms),
            output_tail: run.output_tail,
            timed_out: run.timed_out,
            hostname: hostname.clone(),
        };
        // The gate is an arbitrary command and may itself run arc, so the
        // lock is taken only once it has returned.
        let _repository_events = store.lock_repository_events()?;
        let mut ledger = load_ledger(&store)?;
        let event = repository_event(ctx, &store, payload)?;
        ledger.record(&event)?;
        ctx.ensure_declared_actor(&store)?;
        store.append_repository_event(&event)?;
        let outcome = match result {
            VerifyResult::Pass => "pass",
            _ => "fail",
        };
        println!("gate {name}: {outcome}");
        println!("evaluation: {}", event.event_id);
        if result == VerifyResult::Pass {
            passed += 1;
        }
    }
    println!("gates: {passed}/{total} pass at the candidate's tree");
    Ok(if passed == total { 0 } else { 1 })
}

pub struct SelectArgs {
    pub chosen: String,
    pub into: String,
    pub target: String,
    pub evaluations: Vec<String>,
    pub rationale: String,
}

struct GitResolve<'a> {
    cwd: &'a Path,
}

impl selection::Resolve for GitResolve<'_> {
    fn read_path(&self, read: &Read) -> Option<String> {
        match &read.blob {
            Some(blob) => Some(blob.path.clone()),
            None => super::relations::repository_path(self.cwd, &read.path)
                .ok()
                .flatten(),
        }
    }
}

fn rationale(raw: &str) -> Result<String> {
    let text = match raw.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("cannot read the rationale from {path}"))?,
        None => raw.to_string(),
    };
    if text.trim().is_empty() {
        bail!("--rationale must not be empty");
    }
    Ok(text)
}

pub fn select(ctx: &Ctx, args: SelectArgs) -> Result<i32> {
    let rationale = rationale(&args.rationale)?;
    let store = ctx.store()?;
    let change_id = store
        .resolve_change(&args.into)
        .map_err(|error| anyhow::anyhow!("unknown-destination: {:?}: {error:#}", args.into))?;
    let st = contract_state(ctx, &store, &change_id)?;
    let ledger = load_ledger(&store)?;
    let proposed_target = if args.target.is_empty() || args.target.starts_with('-') {
        None
    } else {
        gitio::rev_parse(&ctx.cwd, &format!("{}^{{commit}}", args.target)).ok()
    };
    let Some(proposed_target) = proposed_target else {
        bail!("unknown-revision: {:?} names no commit", args.target);
    };
    let Some(chosen) = ledger.registration(&args.chosen) else {
        eprintln!("selection refused:");
        eprintln!(
            "{}",
            selection::Refusal::UnknownCandidate(args.chosen.clone())
        );
        return Ok(1);
    };
    let declarations = ctx.declarations(&st)?;
    if declarations.target_unreadable {
        bail!(
            "target-unreadable: {} cannot be resolved, so nothing says what {} owes",
            st.target_branch,
            st.change_id
        );
    }
    let observed_target = gitio::branch_head(&ctx.cwd, &st.target_branch)?;
    let head = gitio::branch_head(&ctx.cwd, &st.branch)?;

    // The environment is observed where the shipped content is checked out,
    // as an evaluation observed it.
    let required = declarations.gates.required_for(&st.profile);
    let environments = if required.iter().any(|(_, gate)| gate.environment.is_some()) {
        let scratch = scratch_checkout(
            ctx,
            &chosen.tree,
            &format!("candidate-{}-select", chosen.candidate_id),
        )?;
        gate_environments(
            &scratch.path,
            required.iter().map(|(name, gate)| (name.as_str(), *gate)),
        )?
    } else {
        Default::default()
    };
    let gates = required
        .iter()
        .map(|(name, gate)| RequiredGate {
            name: (*name).clone(),
            command: gate.command.clone(),
            timeout: gate.timeout,
            environment_probe: gate.environment.clone(),
            environment: declared_environment(&environments, gate).map(str::to_string),
        })
        .collect();

    // What the chosen registration's contract requires to have been read,
    // and what that contract supplied without anyone reading it.
    let contract = if chosen.brief.change_id == change_id {
        st.clone()
    } else {
        contract_state(ctx, &store, &chosen.brief.change_id)?
    };
    let brief = contract
        .briefs
        .iter()
        .find(|brief| brief.event_id == chosen.brief.brief_event_id);
    let reads = brief
        .map(|brief| brief.must_read.clone())
        .unwrap_or_default()
        .into_iter()
        .map(|requirement| {
            let bytes = match &requirement {
                ReadRequirement::File { blob, .. } => gitio::blob_bytes(&ctx.cwd, blob).ok(),
                ReadRequirement::Artifact { .. } => None,
            };
            RequiredRead { requirement, bytes }
        })
        .collect();
    let supplied = brief
        .and_then(|brief| brief.plan_ref.clone())
        .into_iter()
        .chain(contract.journal_ref.clone())
        .collect();

    let observations = Observations {
        reuse: declarations.policy.candidates.evaluation_reuse,
        target: observed_target.clone(),
        destination_open: patchset_admitted(&st).is_ok(),
        gates,
        reads,
        supplied,
    };
    let proposal = Proposal {
        chosen: args.chosen.clone(),
        change_id: change_id.clone(),
        target: proposed_target.clone(),
        evaluations: args.evaluations.clone(),
    };
    let relations = repository_relations(&store)?;
    let resolve = GitResolve { cwd: &ctx.cwd };
    let basis = match selection::evaluate(&ledger, &relations, &resolve, &proposal, &observations) {
        Ok(basis) => basis,
        Err(refusals) => {
            eprintln!(
                "selection refused: {} ground{}",
                refusals.len(),
                if refusals.len() == 1 { "" } else { "s" }
            );
            for refusal in &refusals {
                eprintln!("{refusal}");
            }
            return Ok(1);
        }
    };

    let selection_id = {
        let _repository_events = store.lock_repository_events()?;
        let mut ledger = load_ledger(&store)?;
        let supersedes = ledger
            .standing_unpromoted(&change_id)
            .map(|standing| standing.event_id.clone());
        if ledger.retirement(&args.chosen).is_some() {
            bail!(
                "candidate-retired: {} was retired while it was validated",
                args.chosen
            );
        }
        let event = repository_event(
            ctx,
            &store,
            Payload::CandidateSelected {
                candidate_id: args.chosen.clone(),
                destination: change_id.clone(),
                head,
                target_branch: st.target_branch.clone(),
                target: proposed_target,
                tree: basis.tree,
                evaluations: basis.evaluations,
                reads: basis.reads,
                reuse: basis.reuse,
                contributors: basis.contributors,
                selector: ctx
                    .on_behalf_of
                    .clone()
                    .unwrap_or_else(|| ctx.actor.clone()),
                rationale,
                supersedes: supersedes.clone(),
            },
        )?;
        ledger.record(&event)?;
        ctx.ensure_declared_actor(&store)?;
        store.append_repository_event(&event)?;
        println!("selection: {}", event.event_id);
        println!("chosen: {}", args.chosen);
        if let Some(superseded) = supersedes {
            println!("supersedes: {superseded}");
        }
        event.event_id
    };
    paused_before_promotion()?;
    promote_selection(ctx, &store, &selection_id, false)
}

/// A pause the test suite injects between recording a selection and
/// promoting it, so a basis that moves in between is exercised on purpose.
/// `ARC_SELECT_PAUSE` names a file; the selection, holding no lock, waits for
/// it to exist. Unset, which is every run that is not a test, this does
/// nothing.
fn paused_before_promotion() -> Result<()> {
    let Some(release) = std::env::var_os("ARC_SELECT_PAUSE") else {
        return Ok(());
    };
    let release = Path::new(&release);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !release.exists() {
        if std::time::Instant::now() >= deadline {
            bail!("ARC_SELECT_PAUSE: {} never appeared", release.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}

pub fn promote(ctx: &Ctx, selection_id: &str) -> Result<i32> {
    let store = ctx.store()?;
    promote_selection(ctx, &store, selection_id, true)
}

/// Promote a recorded selection, or finish or discard the promotion an
/// interrupted run left behind.
///
/// Under the destination's transition lock and the repository-events lock,
/// in order: re-read the destination head and target and stand down if
/// either moved from the basis; refuse a checkout with tracked dirt; write
/// the shipped tree onto the head and keep it at the promotion ref; move the
/// branch from the head it was read at, updating a checkout of it; record
/// the patchset and then `candidate-promoted`. The target lock is not taken:
/// the branch that moves is the destination's own.
fn promote_selection(ctx: &Ctx, store: &Store, selection_id: &str, recover: bool) -> Result<i32> {
    let change_id = {
        let ledger = load_ledger(store)?;
        let Some(selection) = ledger.selection(selection_id) else {
            bail!("unknown-selection: no selection {selection_id} is recorded");
        };
        selection.change_id.clone()
    };
    let _transition = store.lock_transition(&change_id)?;
    let _repository_events = store.lock_repository_events()?;
    let ledger = load_ledger(store)?;
    let selection = ledger
        .selection(selection_id)
        .context("the selection left the ledger")?
        .clone();
    if let Some(promotion) = ledger.promotion_of(selection_id) {
        println!(
            "promoted already: selection {selection_id} is {} on {} (event {}); nothing to do",
            promotion.patchset_id, promotion.change_id, promotion.event_id
        );
        return Ok(0);
    }
    if let Some(later) = ledger.superseded_by(selection_id) {
        bail!(
            "superseded: selection {selection_id} is superseded by {}; promote that one",
            later.event_id
        );
    }
    let events = store.load_events(&change_id)?;
    let mut st = state::reduce_following(&events, &store.rewrites()?)?;
    crate::replica::localize_change(&store.repository_id, &events, &mut st);
    patchset_admitted(&st).map_err(|error| anyhow::anyhow!("destination-closed: {error:#}"))?;
    let reference = candidate::promotion_ref(&selection.candidate_id, selection_id);
    let head = gitio::branch_head(&ctx.cwd, &st.branch)?;

    if let Some(commit) = gitio::ref_value(&ctx.cwd, &reference)? {
        if head == commit {
            println!("completing: {} already points at {reference}", st.branch);
            sync_checkout(ctx, &st, &selection, &commit)?;
            return record_promotion(ctx, store, &events, &st, &selection, &commit);
        }
        gitio::delete_ref(&ctx.cwd, &reference)?;
        println!(
            "discarded: {reference} held {commit}, which {} never reached; nothing was promoted",
            st.branch
        );
        println!(
            "tip: `arc candidate promote {selection_id}` promotes from the basis while it holds; \
             after the destination or its target moves, select again"
        );
        return Ok(0);
    }

    let target = gitio::branch_head(&ctx.cwd, &selection.target_branch)?;
    let mut moved = Vec::new();
    if head != selection.head {
        moved.push(format!(
            "{} is at {head}, not {} as selected",
            st.branch, selection.head
        ));
    }
    if target != selection.target {
        moved.push(format!(
            "{} is at {target}, not {} as selected",
            selection.target_branch, selection.target
        ));
    }
    if !moved.is_empty() {
        eprintln!("basis-moved: {}", moved.join("; "));
        eprintln!(
            "selection {selection_id} stands without a promotion; a basis is never reused, so \
             select again (`arc candidate select`) to validate afresh"
        );
        return Ok(1);
    }
    if recover {
        println!("promoting selection {selection_id} from its basis");
    }

    let checkout = gitio::worktree_for_branch(&ctx.cwd, &st.branch)?;
    if let Some(path) = &checkout {
        checkout_tracked_dirt(path)?;
        checkout_writes(
            &TargetCheckout {
                path: path.clone(),
                switch_from: None,
            },
            &head,
            Some(&selection.tree),
        )?;
    }
    let commit = gitio::commit_tree_committed_by(
        &ctx.cwd,
        &selection.tree,
        &head,
        &format!(
            "Promote candidate {} (selection {selection_id})\n\n{}",
            selection.candidate_id,
            selection.rationale.trim()
        ),
        &selection.selector,
    )?;
    gitio::update_ref(&ctx.cwd, &reference, &commit)?;
    let branch_ref = format!("refs/heads/{}", st.branch);
    if let Err(error) = gitio::update_refs(
        &ctx.cwd,
        &[gitio::RefUpdate {
            name: branch_ref,
            old: head.clone(),
            new: commit.clone(),
        }],
    ) {
        eprintln!(
            "promotion stopped: {} did not move from {head} ({error:#})",
            st.branch
        );
        eprintln!(
            "{reference} holds {commit}; `arc candidate promote {selection_id}` completes the \
             promotion if the branch reaches it and discards it otherwise"
        );
        return Ok(1);
    }
    if let Some(path) = &checkout {
        gitio::git(path, &["read-tree", "-m", "-u", &head, &commit]).with_context(|| {
            format!(
                "{} moved to {commit}, and its worktree {} was not updated; `arc candidate \
                 promote {selection_id}` finishes it",
                st.branch,
                path.display()
            )
        })?;
    }
    record_promotion(ctx, store, &events, &st, &selection, &commit)
}

/// Bring a checkout of the destination branch up to the promoted commit
/// when an interrupted run moved the branch and left the checkout at the
/// basis head.
fn sync_checkout(ctx: &Ctx, st: &ChangeState, selection: &Selection, commit: &str) -> Result<()> {
    let Some(path) = gitio::worktree_for_branch(&ctx.cwd, &st.branch)? else {
        return Ok(());
    };
    let index = gitio::git(&path, &["write-tree"])?;
    if index == gitio::commit_tree(&ctx.cwd, &selection.head)? && index != selection.tree {
        gitio::git(&path, &["read-tree", "-m", "-u", &selection.head, commit])?;
        println!("worktree {}: updated to {commit}", path.display());
    }
    Ok(())
}

/// Step five: the patchset on the destination, then `candidate-promoted`.
/// A patchset an interrupted run already recorded for this selection is
/// the one the promotion names.
fn record_promotion(
    ctx: &Ctx,
    store: &Store,
    events: &[crate::model::Event],
    st: &ChangeState,
    selection: &Selection,
    commit: &str,
) -> Result<i32> {
    let recorded = st
        .patchsets
        .iter()
        .find(|patchset| {
            patchset
                .candidate
                .as_ref()
                .is_some_and(|link| link.selection == selection.event_id)
        })
        .map(|patchset| patchset.id.clone());
    let patchset_id = match recorded {
        Some(id) => id,
        None => record_patchset(ctx, store, events, st, selection, commit)?,
    };
    let event = repository_event(
        ctx,
        store,
        Payload::CandidatePromoted {
            selection: selection.event_id.clone(),
            candidate_id: selection.candidate_id.clone(),
            destination: selection.change_id.clone(),
            patchset_id: patchset_id.clone(),
            revision: commit.to_string(),
        },
    )?;
    let mut ledger = load_ledger(store)?;
    ledger.record(&event)?;
    store.append_repository_event(&event)?;
    println!(
        "promoted: {} into {}",
        selection.candidate_id, selection.change_id
    );
    println!("patchset: {patchset_id}");
    println!("head: {commit}");
    println!("contributors: {}", selection.contributors.join(","));
    println!("event: {}", event.event_id);
    Ok(0)
}

fn record_patchset(
    ctx: &Ctx,
    store: &Store,
    events: &[crate::model::Event],
    st: &ChangeState,
    selection: &Selection,
    commit: &str,
) -> Result<String> {
    let (base, merge_base) = super::review::patchset_base(ctx, st, commit)?;
    let brief = contract_brief(st, selection, store)?;
    let (journal_refs, skipped) =
        super::review::default_journal_refs(ctx, st.journal_ref.as_deref(), brief.as_ref());
    for line in &skipped {
        eprintln!("{line}");
    }
    let identity = gitio::commit_identity(&ctx.cwd, commit)?;
    let patchset_id = format!("ps-{:02}", st.patchsets.len() + 1);
    let payload = Payload::PatchsetAdded {
        patchset_id: patchset_id.clone(),
        base,
        head: commit.to_string(),
        merge_base,
        brief_ref: brief.as_ref().map(|brief| crate::model::BriefRef {
            event_id: brief.event_id.clone(),
        }),
        author_name: Some(identity.author_name),
        author_email: Some(identity.author_email),
        committer_name: Some(identity.committer_name),
        committer_email: Some(identity.committer_email),
        contributors: selection.contributors.clone(),
        claim_id: None,
        claim_actor: None,
        journal_refs,
        thread: None,
        candidate: Some(PatchsetCandidate {
            candidate_id: selection.candidate_id.clone(),
            selection: selection.event_id.clone(),
        }),
    };
    super::ensure_append_allowed(st, &payload)?;
    let mut event = ctx.event(store, &st.change_id, payload);
    event.event_id = super::event_id_after(
        &events
            .last()
            .context("change has no opening event")?
            .event_id,
    )?;
    store.append_event(&event)?;
    gitio::update_ref(
        &ctx.cwd,
        &gitio::retention_ref(&st.change_id, &patchset_id),
        commit,
    )?;
    Ok(patchset_id)
}

/// The brief version the chosen registration answers, on the destination.
fn contract_brief(
    st: &ChangeState,
    selection: &Selection,
    store: &Store,
) -> Result<Option<state::Brief>> {
    let ledger = load_ledger(store)?;
    let version = ledger
        .registration(&selection.candidate_id)
        .map(|registration| registration.brief.brief_event_id.clone());
    Ok(st
        .briefs
        .iter()
        .find(|brief| Some(&brief.event_id) == version.as_ref())
        .cloned())
}

/// A promotion ref no `candidate-promoted` event records: a promotion a run
/// wrote and never finished, or one naming no recorded selection.
pub struct Interrupted {
    pub reference: String,
    pub candidate_id: String,
    pub selection: String,
    pub commit: String,
    pub selection_known: bool,
}

pub fn interrupted_promotions(cwd: &Path, ledger: &Ledger) -> Result<Vec<Interrupted>> {
    Ok(gitio::list_refs(cwd, candidate::PROMOTION_REF_PREFIX)?
        .into_iter()
        .filter_map(|(reference, commit)| {
            let (candidate_id, selection) = candidate::parse_promotion_ref(&reference)?;
            if ledger.promotion_of(selection).is_some() {
                return None;
            }
            Some(Interrupted {
                candidate_id: candidate_id.to_string(),
                selection: selection.to_string(),
                selection_known: ledger.selection(selection).is_some(),
                reference: reference.clone(),
                commit,
            })
        })
        .collect())
}
