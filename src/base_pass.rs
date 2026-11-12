//! Target baselines retain execution facts and checkable Git subjects.

use crate::gates::{self, Gate};
use crate::model::{Event, GateEvent, Payload, VerifyResult};
use crate::state::{ChangeState, VerificationEntry};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub fn digest(value: &impl Serialize) -> String {
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(
            serde_json::to_vec(value).expect("serializable witness")
        ))
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub declaration: Value,
    pub identity: String,
    pub execution: Event,
    pub run: Event,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub project_id: String,
    pub repository_id: String,
    pub target: String,
    pub revision: String,
    pub tree: String,
    pub profile: String,
    pub completion: Box<Event>,
    pub gates: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reuse {
    pub baseline_event_id: String,
    pub baseline: Baseline,
    pub baseline_digest: String,
    pub gate: String,
    pub revision: String,
    pub tree: String,
    pub against_target: Option<String>,
    pub changed_paths: Vec<String>,
    pub algorithm: String,
    pub target_revision: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Accounting {
    pub declaration: Value,
    pub environment: Option<crate::model::EnvironmentEvidence>,
    pub profile: String,
    pub event: GateEvent,
    pub baseline_event_id: String,
    pub target: String,
    pub revision: String,
    pub tree: String,
    pub execution_event_id: String,
    pub executed_at: chrono::DateTime<chrono::Utc>,
    pub changed_paths: Vec<String>,
}

impl Reuse {
    pub fn accounting(&self) -> Accounting {
        let execution = &self.baseline.gates[&self.gate].execution;
        let environment = match &execution.payload {
            Payload::VerificationRecorded { environment, .. } => environment.clone(),
            _ => None,
        };
        Accounting {
            declaration: self.baseline.gates[&self.gate].declaration.clone(),
            environment,
            profile: self.baseline.profile.clone(),
            event: GateEvent::Integration,
            baseline_event_id: self.baseline_event_id.clone(),
            target: self.baseline.target.clone(),
            revision: self.baseline.revision.clone(),
            tree: self.baseline.tree.clone(),
            execution_event_id: execution.event_id.clone(),
            executed_at: execution.created_at,
            changed_paths: self.changed_paths.clone(),
        }
    }

    /// Replay checks frozen structure; current budgets judge the derived receipt.
    pub fn validate(&self, profile: &str) -> Result<()> {
        if serde_json::to_vec(self)?.len() > 1_048_576 {
            bail!("base-pass witness exceeds 1 MiB");
        }
        self.baseline.validate()?;
        let entry = self
            .baseline
            .gates
            .get(&self.gate)
            .context("base pass names no baseline gate")?;
        let coverage: gates::Coverage =
            serde_json::from_value(entry.declaration["coverage"].clone())?;
        if self.algorithm != "arc-base-paths/1"
            || self.baseline_digest != digest(&self.baseline)
            || self.baseline.profile != profile
            || !coverage.base_passes
            || self
                .changed_paths
                .iter()
                .any(|path| protected(path) || coverage.affects(&self.gate, path))
        {
            bail!("invalid base-pass selection witness");
        }
        Ok(())
    }
}

impl Baseline {
    fn budget(&self, name: &str) -> Option<u64> {
        match &self.completion.payload {
            Payload::ChangeIntegrated {
                authorization: Some(auth),
                ..
            } => auth.gates.get(name).and_then(|gate| gate.budget_ms),
            Payload::LandingCompleted { members, .. } => members
                .iter()
                .filter_map(|member| {
                    member
                        .authorization
                        .gates
                        .get(name)
                        .and_then(|gate| gate.budget_ms)
                })
                .min(),
            _ => None,
        }
    }
    pub fn validate(&self) -> Result<()> {
        let (target, revision, evidence) = match &self.completion.payload {
            Payload::ChangeIntegrated {
                target_branch,
                integrated_commit,
                authorization: Some(auth),
                landing: None,
                ..
            } => (target_branch, integrated_commit, &auth.gate_evidence),
            Payload::LandingCompleted {
                target_branch,
                target_after,
                tree,
                gate_evidence,
                ..
            } if tree == &self.tree => (target_branch, target_after, gate_evidence),
            _ => bail!("baseline has no authoritative completion"),
        };
        if self.completion.repository_id != self.repository_id
            || target != &self.target
            || revision != &self.revision
        {
            bail!("baseline completion subject mismatch");
        }
        for (name, entry) in &self.gates {
            let Payload::VerificationRunStarted {
                profile: Some(profile),
                event: GateEvent::Integration,
                revision,
                gates,
                ..
            } = &entry.run.payload
            else {
                bail!("baseline execution has no integration profile");
            };
            let Payload::VerificationRecorded {
                declaration: Some(identity),
                gate: Some(gate),
                command,
                revision: executed_revision,
                result: VerifyResult::Pass,
                exit_code: Some(0),
                timing,
                attested: false,
                run_id: Some(run_id),
                tested_tree: Some(tested),
                worktree_dirty: Some(false),
                tree_moved: false,
                tree: Some(tree),
                timeout_seconds,
                timed_out: false,
                environment,
                ..
            } = &entry.execution.payload
            else {
                bail!("baseline is not a clean direct execution");
            };
            let normalized = match &self.completion.payload {
                Payload::ChangeIntegrated {
                    authorization: Some(auth),
                    ..
                } => auth.gates.get(name),
                Payload::LandingCompleted { members, .. } => members
                    .iter()
                    .find_map(|member| member.authorization.gates.get(name)),
                _ => None,
            }
            .context("baseline gate has no completed declaration")?;
            if normalized.coverage_identity.as_deref() != Some(entry.identity.as_str()) {
                bail!("baseline declaration does not match completed authorization");
            }
            let coverage: gates::Coverage =
                serde_json::from_value(entry.declaration["coverage"].clone())?;
            if !coverage.base_passes || !coverage.gates.contains_key(name) {
                bail!("baseline lacks complete coverage");
            }
            let environment_valid = match (entry.declaration["environment"].as_str(), environment) {
                (None, None) => true,
                (Some(_), Some(environment)) => {
                    environment.producing_store == self.repository_id
                        && environment.identity.starts_with("sha256:")
                        && environment.identity.len() == 71
                }
                _ => false,
            };
            if !environment_valid
                || profile != &self.profile
                || gate != name
                || run_id != &entry.run.event_id
                || revision != executed_revision
                || tested != &self.tree
                || tree != &self.tree
                || entry.execution.repository_id != self.repository_id
                || entry.run.repository_id != self.repository_id
                || entry.execution.change_id != entry.run.change_id
                || entry.run.event_id >= entry.execution.event_id
                || entry.execution.event_id >= self.completion.event_id
                || evidence.get(name) != Some(&entry.execution.event_id)
                || identity != &entry.identity
                || entry.identity != digest(&entry.declaration)
                || entry.declaration["binds"] == "commit"
                || entry.declaration["command"] != *command
                || entry.declaration["timeout"] != serde_json::json!(timeout_seconds)
                || timing.run_ms.is_none()
                || !crate::gate_budget::judge(self.budget(name), &[], timing.run_ms).accepted()
                || !gates.iter().any(|gate| {
                    gate.name == *name
                        && gate.command == *command
                        && gate.timeout_seconds == *timeout_seconds
                })
            {
                bail!("baseline execution coordinates mismatch for {name}");
            }
        }
        Ok(())
    }
}

pub fn protected(path: &str) -> bool {
    path == ".arc" || path.starts_with(".arc/")
}

fn first_parent(cwd: &Path, ancestor: &str, revision: &str) -> bool {
    crate::gitio::git(cwd, &["rev-list", "--first-parent", revision])
        .is_ok_and(|history| history.lines().any(|oid| oid == ancestor))
}

fn validate_baseline_git(cwd: &Path, baseline: &Baseline) -> Result<()> {
    if crate::gitio::commit_tree(cwd, &baseline.revision)? != baseline.tree {
        bail!("baseline Git tree mismatch");
    }
    let mut checked = BTreeSet::from([baseline.revision.as_str()]);
    for entry in baseline.gates.values() {
        let Payload::VerificationRecorded { revision, .. } = &entry.execution.payload else {
            bail!("baseline is not execution");
        };
        if checked.insert(revision) && crate::gitio::commit_tree(cwd, revision)? != baseline.tree {
            bail!("baseline execution Git tree mismatch");
        }
    }
    Ok(())
}

/// Validate Git claims using retained objects, never an asserted path digest.
pub fn validate_git(cwd: &Path, witness: &Reuse) -> Result<()> {
    validate_baseline_git(cwd, &witness.baseline)?;
    let project = crate::gitio::file_at(cwd, &witness.target_revision, ".arc/gates.toml")?
        .context("base pass has no committed project adoption")?;
    let project: gates::GatesFile = toml::from_str(&project)?;
    if let Some(against) = &witness.against_target {
        let parents = crate::gitio::commit_parents(cwd, &witness.revision)?;
        if parents.len() != 2
            || parents.first() != Some(against)
            || against != &witness.target_revision
        {
            bail!("base-pass merge target mismatch");
        }
    }
    if !project.selection.base_passes
        || !first_parent(cwd, &witness.baseline.revision, &witness.target_revision)
        || crate::gitio::commit_tree(cwd, &witness.baseline.revision)? != witness.baseline.tree
        || crate::gitio::commit_tree(cwd, &witness.revision)? != witness.tree
        || !first_parent(cwd, &witness.baseline.revision, &witness.revision)
        || gates::delta_paths(cwd, &witness.baseline.tree, &witness.tree).as_ref()
            != Some(&witness.changed_paths)
    {
        bail!("base-pass Git diff or first-parent ancestry mismatch");
    }
    Ok(())
}

pub fn validate_events(
    cwd: &Path,
    events: &[Event],
    repository: &[Event],
    scope: &crate::import_authorization::PrivilegeScope,
) -> Result<()> {
    let mut checked = BTreeSet::new();
    for event in events {
        if let Payload::VerificationBaseReused { witness, .. } = &event.payload {
            let recorded = repository
                .iter()
                .find(|event| event.event_id == witness.baseline_event_id)
                .context("base pass has no authenticated integration baseline")?;
            let Payload::GateBaselineRecorded { baseline } = &recorded.payload else {
                bail!("base pass source is not a baseline");
            };
            if recorded.event_id <= witness.baseline.completion.event_id
                || event.event_id <= recorded.event_id
                || event.repository_id != witness.baseline.repository_id
                || recorded.repository_id != witness.baseline.repository_id
                || digest(baseline.as_ref()) != witness.baseline_digest
                || !scope.permits(
                    &recorded.event_id,
                    &recorded.repository_id,
                    crate::import_authorization::Role::Integration,
                )
            {
                bail!("base pass baseline lacks integration authority");
            }
            let subject = digest(&(
                &witness.baseline_digest,
                &witness.revision,
                &witness.tree,
                &witness.target_revision,
                &witness.changed_paths,
            ));
            if checked.insert(subject) {
                validate_git(cwd, witness)?;
            }
        }
    }
    Ok(())
}

/// Other observations cannot be displaced by a delayed baseline answer.
pub fn first_run(state: &ChangeState, run_id: &str) -> bool {
    state
        .verification_runs
        .first()
        .is_some_and(|run| run.run_id == run_id)
        && state
            .verifications
            .iter()
            .all(|entry| entry.run_id.as_deref() == Some(run_id))
}

/// Select one newest eligible target baseline before considering individual gates.
pub struct Source {
    pub event_id: String,
    pub baseline: Baseline,
    pub target_revision: String,
}

pub fn source(cwd: &Path, store: &Store, state: &ChangeState, revision: &str) -> Option<Source> {
    if !state.verification_runs.is_empty() || !state.verifications.is_empty() {
        return None;
    }
    let target = crate::gitio::branch_head(cwd, &state.target_branch).ok()?;
    let declarations = gates::load_at(cwd, &target).ok()?;
    if !declarations.selection.base_passes {
        return None;
    }
    let scope = store.privilege_scope().ok()?;
    let project = crate::replica::status(store)
        .ok()?
        .project_id
        .unwrap_or_else(|| store.repository_id.clone());
    let target_history = crate::gitio::git(cwd, &["rev-list", "--first-parent", &target]).ok()?;
    let target_history = target_history.lines().collect::<BTreeSet<_>>();
    let head_history = crate::gitio::git(cwd, &["rev-list", "--first-parent", revision]).ok()?;
    let head_history = head_history.lines().collect::<BTreeSet<_>>();
    store
        .load_repository_events()
        .ok()?
        .iter()
        .rev()
        .find_map(|event| {
            let Payload::GateBaselineRecorded { baseline } = &event.payload else {
                return None;
            };
            (event.repository_id == store.repository_id
                && scope.admission_for(&event.event_id).is_none()
                && scope.permits(
                    &event.event_id,
                    &event.repository_id,
                    crate::import_authorization::Role::Integration,
                )
                && baseline.project_id == project
                && baseline.target == state.target_branch
                && baseline.profile == state.profile
                && target_history.contains(baseline.revision.as_str())
                && head_history.contains(baseline.revision.as_str())
                && baseline.validate().is_ok()
                && validate_baseline_git(cwd, baseline).is_ok()
                && crate::gitio::commit_tree(cwd, &baseline.revision)
                    .ok()
                    .as_deref()
                    == Some(baseline.tree.as_str()))
            .then(|| Source {
                event_id: event.event_id.clone(),
                baseline: baseline.as_ref().clone(),
                target_revision: target.clone(),
            })
        })
}

#[allow(clippy::too_many_arguments)]
pub fn select(
    source: &Source,
    name: &str,
    gate: &Gate,
    revision: &str,
    tree: &str,
    against_target: Option<&str>,
    environment: Option<&str>,
    paths: &[String],
) -> Option<Reuse> {
    if gate.binds != gates::Binding::Tree {
        return None;
    }
    let entry = source.baseline.gates.get(name)?;
    if entry.identity != gates::declaration_identity(gate) {
        return None;
    }
    let Payload::VerificationRecorded {
        environment: observed,
        ..
    } = &entry.execution.payload
    else {
        return None;
    };
    if gate.environment.is_some()
        && observed.as_ref().map(|env| env.identity.as_str()) != environment
    {
        return None;
    }
    if gate.environment.is_some() && environment.is_none() {
        return None;
    }
    let witness = Reuse {
        baseline_event_id: source.event_id.clone(),
        baseline: source.baseline.clone(),
        baseline_digest: digest(&source.baseline),
        gate: name.into(),
        revision: revision.into(),
        tree: tree.into(),
        against_target: against_target.map(str::to_owned),
        changed_paths: paths.to_vec(),
        algorithm: "arc-base-paths/1".into(),
        target_revision: source.target_revision.clone(),
    };
    witness.validate(&source.baseline.profile).ok()?;
    Some(witness)
}

/// Publish a completion snapshot under the caller's destination transition guard.
pub fn record(
    ctx: &crate::commands::Ctx,
    store: &Store,
    completion: &Event,
    state: &ChangeState,
    build_declarations: impl FnOnce() -> Result<gates::GatesFile>,
) -> Result<()> {
    let (target, revision, evidence) = match &completion.payload {
        Payload::ChangeIntegrated {
            target_branch,
            integrated_commit,
            authorization: Some(auth),
            landing: None,
            ..
        } => (target_branch, integrated_commit, &auth.gate_evidence),
        Payload::LandingCompleted {
            target_branch,
            target_after,
            gate_evidence,
            ..
        } => (target_branch, target_after, gate_evidence),
        _ => return Ok(()),
    };
    let before = match &completion.payload {
        Payload::ChangeIntegrated { target_before, .. }
        | Payload::LandingCompleted { target_before, .. } => target_before,
        _ => unreachable!(),
    };
    if !gates::load_at(&ctx.cwd, before)?.selection.base_passes {
        return Ok(());
    }
    if let Some(reason) = crate::replica::integration_refusal(store)? {
        bail!("baseline requires canonical integration authority: {reason}");
    }
    if crate::gitio::branch_head(&ctx.cwd, target)? != *revision {
        bail!("baseline target moved after integration");
    }
    let published = match &completion.payload {
        Payload::ChangeIntegrated { .. } => store.event_is_published(completion)?,
        Payload::LandingCompleted { .. } => store.repository_event_is_published(completion)?,
        _ => false,
    };
    if !published {
        bail!("baseline completion has not been published");
    }
    let tree = crate::gitio::commit_tree(&ctx.cwd, revision)?;
    let declarations = build_declarations()?;
    let events = store.load_events(&state.change_id)?;
    let mut entries = BTreeMap::new();
    let project_id = crate::replica::status(store)?
        .project_id
        .unwrap_or_else(|| store.repository_id.clone());
    for (name, gate) in declarations.required_for_event(&state.profile, GateEvent::Integration) {
        if gate.binds != gates::Binding::Tree {
            continue;
        }
        let Some(id) = evidence.get(name) else {
            continue;
        };
        let Some(execution) = events
            .iter()
            .find(|event| &event.event_id == id && event.repository_id == store.repository_id)
        else {
            continue;
        };
        let Payload::VerificationRecorded {
            run_id: Some(run_id),
            timing,
            declaration,
            ..
        } = &execution.payload
        else {
            continue;
        };
        if declaration.as_deref() != Some(gates::declaration_identity(gate).as_str())
            || !gate.budget_judgement(timing.run_ms).accepted()
        {
            continue;
        }
        let Some(run) = events.iter().find(|event| &event.event_id == run_id) else {
            continue;
        };
        let entry = Entry {
            declaration: gates::declaration_value(gate),
            identity: gates::declaration_identity(gate),
            execution: execution.clone(),
            run: run.clone(),
        };
        let mut candidate = Baseline {
            project_id: project_id.clone(),
            repository_id: store.repository_id.clone(),
            target: target.clone(),
            revision: revision.clone(),
            tree: tree.clone(),
            profile: state.profile.clone(),
            completion: Box::new(completion.clone()),
            gates: BTreeMap::from([(name.clone(), entry.clone())]),
        };
        if candidate.validate().is_ok() {
            entries.append(&mut candidate.gates);
        }
    }
    let baseline = Baseline {
        project_id,
        repository_id: store.repository_id.clone(),
        target: target.clone(),
        revision: revision.clone(),
        tree,
        profile: state.profile.clone(),
        completion: Box::new(completion.clone()),
        gates: entries,
    };
    baseline.validate()?;
    store.adopt_base_passes()?;
    crate::gitio::update_ref(
        &ctx.cwd,
        &format!("refs/arc/baselines/{}/target", completion.event_id),
        revision,
    )?;
    for entry in baseline.gates.values() {
        if let Payload::VerificationRecorded { revision, .. } = &entry.execution.payload {
            crate::gitio::update_ref(
                &ctx.cwd,
                &format!(
                    "refs/arc/baselines/{}/executions/{}",
                    completion.event_id, entry.execution.event_id
                ),
                revision,
            )?;
        }
    }
    let _lock = store.lock_repository_events()?;
    if store.load_repository_events()?.iter().any(|event| matches!(&event.payload, Payload::GateBaselineRecorded { baseline } if baseline.completion.event_id == completion.event_id)) { return Ok(()); }
    let mut event = ctx.event(
        store,
        Store::REPOSITORY_SCOPE,
        Payload::GateBaselineRecorded {
            baseline: Box::new(baseline),
        },
    )?;
    store.append_local_repository_event(&mut event)?;
    Ok(())
}

pub fn execution(witness: &Reuse) -> &Event {
    &witness.baseline.gates[&witness.gate].execution
}

pub fn derive(witness: &Reuse, event: &Event, run_id: &str) -> Result<VerificationEntry> {
    let source = execution(witness);
    let Payload::VerificationRecorded {
        declaration,
        gate,
        command,
        timeout_seconds,
        result,
        attested,
        duration_ms,
        timing,
        output_tail,
        output_artifact,
        timed_out,
        hostname,
        runner,
        tested_tree,
        worktree_dirty,
        worktree_dirty_tracked,
        worktree_dirty_untracked,
        tree_moved,
        environment,
        ..
    } = &source.payload
    else {
        bail!("base pass source is not execution");
    };
    Ok(VerificationEntry {
        base_pass: Some(witness.accounting()),
        declaration: declaration.clone(),
        event_id: event.event_id.clone(),
        delta: None,
        run_id: Some(run_id.into()),
        probe: None,
        gate: gate.clone(),
        command: command.clone(),
        timeout_seconds: *timeout_seconds,
        revision: witness.revision.clone(),
        recorded_revision: witness.revision.clone(),
        result: *result,
        attested: *attested,
        duration_ms: *duration_ms,
        timing: timing.clone(),
        output_tail: output_tail.clone(),
        output_artifact: output_artifact.clone(),
        timed_out: *timed_out,
        hostname: hostname.clone(),
        runner: runner.clone(),
        bypass: None,
        tested_tree: tested_tree.clone(),
        worktree_dirty: *worktree_dirty,
        worktree_dirty_tracked: *worktree_dirty_tracked,
        worktree_dirty_untracked: *worktree_dirty_untracked,
        tree_moved: *tree_moved,
        tree: Some(witness.tree.clone()),
        against_target: witness.against_target.clone(),
        falsification: None,
        falsification_inferred: None,
        environment: environment.clone(),
        created_at: event.created_at,
    })
}

/// Integration completions, rather than verification timestamps, delimit audits.
/// A pass recorded before a publication but integrated after it remains owed.
pub fn publication_participants(
    cwd: &Path,
    store: &Store,
    target: &str,
    profile: &str,
    revision: &str,
    since: Option<&str>,
) -> Result<BTreeMap<String, Gate>> {
    let baselines = store.load_repository_events()?;
    let mut participants = BTreeMap::new();
    for id in store.list_change_ids()? {
        let state = store.state(&id)?;
        let Some(closure) = &state.closure else {
            continue;
        };
        let completion_id = closure
            .landing
            .as_ref()
            .map(|landing| landing.completion_event_id.as_str())
            .unwrap_or(&closure.event_id);
        if closure.outcome != crate::model::Closure::Integrated
            || closure.target_branch.as_deref() != Some(target)
            || state.profile != profile
            || since.is_some_and(|since| completion_id <= since)
            || !closure.integrated_commit.as_ref().is_some_and(|commit| {
                crate::gitio::is_ancestor(cwd, commit, revision).unwrap_or(false)
            })
        {
            continue;
        }
        for evidence in &state.verifications {
            let Some(base) = &evidence.base_pass else {
                continue;
            };
            let Some(Payload::GateBaselineRecorded { baseline }) = baselines
                .iter()
                .find(|event| event.event_id == base.baseline_event_id)
                .map(|event| &event.payload)
            else {
                bail!("publication base-pass baseline is unavailable");
            };
            let name = evidence.gate.as_deref().context("base pass has no gate")?;
            let entry = baseline
                .gates
                .get(name)
                .context("base pass has no source declaration")?;
            let mut declaration = entry.declaration.clone();
            if let Some(seconds) = declaration["timeout"].as_u64() {
                declaration["timeout"] = serde_json::json!(format!("{seconds}s"));
            }
            let mut gate: Gate = serde_json::from_value(declaration)?;
            gate.coverage = serde_json::from_value(entry.declaration["coverage"].clone())?;
            gate.budget = baseline.budget(name);
            participants.insert(name.to_owned(), gate);
        }
    }
    Ok(participants)
}

pub fn cited_commits(payload: &Payload) -> Vec<&str> {
    let (baseline, mut commits) = match payload {
        Payload::GateBaselineRecorded { baseline } => (baseline.as_ref(), Vec::new()),
        Payload::VerificationBaseReused { witness, .. } => (
            &witness.baseline,
            vec![witness.revision.as_str(), witness.target_revision.as_str()],
        ),
        _ => return Vec::new(),
    };
    commits.push(&baseline.revision);
    for entry in baseline.gates.values() {
        if let Payload::VerificationRecorded { revision, .. } = &entry.execution.payload {
            commits.push(revision);
        }
    }
    commits
}
