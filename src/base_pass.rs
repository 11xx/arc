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
    pub authorized_identity: String,
    pub budget_ms: Option<u64>,
    pub execution: Execution,
    pub run: Run,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Execution {
    pub event_id: String,
    pub digest: String,
    pub repository_id: String,
    pub change_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub gate: Option<String>,
    pub run_id: Option<String>,
    pub revision: String,
    pub tree: Option<String>,
    pub tested_tree: Option<String>,
    pub declaration: Option<String>,
    pub command: String,
    pub timeout_seconds: Option<u64>,
    pub result: VerifyResult,
    pub exit_code: Option<i32>,
    pub attested: bool,
    pub worktree_dirty: Option<bool>,
    pub tree_moved: bool,
    pub timed_out: bool,
    pub environment: Option<crate::model::EnvironmentEvidence>,
    pub hostname: String,
    #[serde(flatten)]
    pub timing: crate::verification_timing::VerificationTiming,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub event_id: String,
    pub digest: String,
    pub repository_id: String,
    pub change_id: String,
    pub revision: String,
    pub profile: Option<String>,
    pub event: GateEvent,
    pub gate: crate::model::VerificationRunGate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Completion {
    pub event_id: String,
    pub repository_id: String,
    pub digest: String,
    pub event_type: String,
    pub target: String,
    pub revision: String,
    pub gate_evidence: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub project_id: String,
    pub repository_id: String,
    pub target: String,
    pub revision: String,
    pub tree: String,
    pub profile: String,
    pub completion: Completion,
    pub gates: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reuse {
    pub baseline_event_id: String,
    pub baseline_digest: String,
    pub gate: String,
    pub revision: String,
    pub tree: String,
    pub against_target: Option<String>,
    pub changed_paths: Vec<String>,
    pub algorithm: String,
    pub target_revision: String,
}

/// Shared only within one verification; serialized receipts contain references.
#[derive(Debug, Clone)]
pub struct Selected {
    pub witness: Reuse,
    pub baseline: std::sync::Arc<Baseline>,
}

impl std::ops::Deref for Selected {
    type Target = Reuse;
    fn deref(&self) -> &Reuse {
        &self.witness
    }
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
    pub fn git_subject(&self) -> String {
        digest(&(
            &self.baseline_event_id,
            &self.baseline_digest,
            &self.revision,
            &self.tree,
            &self.target_revision,
            &self.against_target,
            &self.changed_paths,
            &self.algorithm,
        ))
    }

    pub fn accounting(&self, baseline: &Baseline) -> Accounting {
        let execution = &baseline.gates[&self.gate].execution;
        let environment = execution.environment.clone();
        Accounting {
            declaration: baseline.gates[&self.gate].declaration.clone(),
            environment,
            profile: baseline.profile.clone(),
            event: GateEvent::Integration,
            baseline_event_id: self.baseline_event_id.clone(),
            target: baseline.target.clone(),
            revision: baseline.revision.clone(),
            tree: baseline.tree.clone(),
            execution_event_id: execution.event_id.clone(),
            executed_at: execution.created_at,
            changed_paths: self.changed_paths.clone(),
        }
    }

    /// Replay checks frozen structure; current budgets judge the derived receipt.
    pub fn validate(&self, baseline: &Baseline, profile: &str) -> Result<()> {
        if serde_json::to_vec(self)?.len() > 1_048_576 {
            bail!("base-pass witness exceeds 1 MiB");
        }
        baseline.validate()?;
        let entry = baseline
            .gates
            .get(&self.gate)
            .context("base pass names no baseline gate")?;
        let coverage: gates::Coverage =
            serde_json::from_value(entry.declaration["coverage"].clone())?;
        if ![&self.revision, &self.tree, &self.target_revision]
            .iter()
            .all(|value| oid(value))
            || self
                .against_target
                .as_ref()
                .is_some_and(|value| !oid(value))
            || self.algorithm != "arc-base-paths/1"
            || self.baseline_digest != digest(baseline)
            || baseline.profile != profile
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

fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn valid_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

impl Baseline {
    pub fn validate(&self) -> Result<()> {
        if !oid(&self.revision)
            || !oid(&self.tree)
            || self.completion.repository_id != self.repository_id
            || !valid_digest(&self.completion.digest)
            || !matches!(
                self.completion.event_type.as_str(),
                "change-integrated" | "landing-completed"
            )
            || self.completion.target != self.target
            || self.completion.revision != self.revision
        {
            bail!("baseline has no authoritative completion");
        }
        for (name, entry) in &self.gates {
            let execution = &entry.execution;
            crate::gate_work::validate_timing(
                &execution.event_id,
                execution.created_at,
                execution.gate.as_deref(),
                execution.declaration.as_deref(),
                Some(&execution.hostname),
                &execution.timing,
            )?;
            let run = &entry.run;
            let coverage: gates::Coverage =
                serde_json::from_value(entry.declaration["coverage"].clone())?;
            let environment_valid = match (
                entry.declaration["environment"].as_str(),
                &execution.environment,
            ) {
                (None, None) => true,
                (Some(_), Some(environment)) => {
                    environment.producing_store == self.repository_id
                        && valid_digest(&environment.identity)
                }
                _ => false,
            };
            if !coverage.base_passes
                || !coverage.gates.contains_key(name)
                || !environment_valid
                || run.profile.as_deref() != Some(&self.profile)
                || run.event != GateEvent::Integration
                || run.gate.name != *name
                || run.gate.command != execution.command
                || run.gate.timeout_seconds != execution.timeout_seconds
                || !oid(&execution.revision)
                || execution.gate.as_deref() != Some(name)
                || execution.run_id.as_deref() != Some(&run.event_id)
                || run.revision != execution.revision
                || execution.tested_tree.as_deref() != Some(&self.tree)
                || execution.tree.as_deref() != Some(&self.tree)
                || execution.repository_id != self.repository_id
                || run.repository_id != self.repository_id
                || execution.change_id != run.change_id
                || run.event_id >= execution.event_id
                || execution.event_id >= self.completion.event_id
                || self.completion.gate_evidence.get(name) != Some(&execution.event_id)
                || execution.declaration.as_deref() != Some(&entry.identity)
                || entry.authorized_identity != entry.identity
                || entry.identity != digest(&entry.declaration)
                || entry.declaration["binds"] == "commit"
                || entry.declaration["command"] != execution.command
                || entry.declaration["timeout"] != serde_json::json!(execution.timeout_seconds)
                || execution.result != VerifyResult::Pass
                || execution.exit_code != Some(0)
                || execution.attested
                || execution.timed_out
                || execution.tree_moved
                || execution
                    .timing
                    .concurrent_batch
                    .as_ref()
                    .is_some_and(|batch| {
                        batch.acceptance.producing_store != execution.repository_id
                            || !crate::gate_concurrency::receipt_matches(
                                batch,
                                execution.gate.as_deref(),
                                execution.declaration.as_deref(),
                            )
                    })
                || execution.worktree_dirty != Some(false)
                || crate::gate_work::was_excused(&execution.timing)
                || crate::gate_suspension::was_suspended(&execution.timing)
                || execution.timing.run_ms.is_none()
                || !crate::gate_budget::judge(entry.budget_ms, &[], None, &execution.timing).fits()
                || !valid_digest(&execution.digest)
                || !valid_digest(&run.digest)
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

const RETAINED_BASELINES: usize = 8;
const ANCESTRY_LIMIT: usize = 256;
thread_local! {
    static ANCESTRY: std::cell::RefCell<BTreeSet<(String, String)>> = const { std::cell::RefCell::new(BTreeSet::new()) };
}

fn first_parent(cwd: &Path, ancestor: &str, revision: &str) -> bool {
    if !oid(ancestor) || !oid(revision) {
        return false;
    }
    if ancestor == revision {
        return true;
    }
    let key = (ancestor.to_owned(), revision.to_owned());
    if ANCESTRY.with(|cache| cache.borrow().contains(&key)) {
        return true;
    }
    crate::perf::count("base_ancestry_walks", 1);
    let answer = crate::gitio::git(
        cwd,
        &[
            "rev-list",
            "--first-parent",
            &format!("--max-count={ANCESTRY_LIMIT}"),
            revision,
            &format!("^{ancestor}^@"),
        ],
    )
    .is_ok_and(|history| {
        crate::perf::count("base_ancestry_commits", history.lines().count() as u64);
        history.lines().any(|oid| oid == ancestor)
    });
    if answer {
        ANCESTRY.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() >= 64 {
                cache.clear();
            }
            cache.insert(key);
        });
    }
    answer
}

fn validate_baseline_git(cwd: &Path, baseline: &Baseline) -> Result<()> {
    if crate::gitio::commit_tree(cwd, &baseline.revision)? != baseline.tree {
        bail!("baseline Git tree mismatch");
    }
    let mut checked = BTreeSet::from([baseline.revision.as_str()]);
    for entry in baseline.gates.values() {
        let revision = &entry.execution.revision;
        if checked.insert(revision) && crate::gitio::commit_tree(cwd, revision)? != baseline.tree {
            bail!("baseline execution Git tree mismatch");
        }
    }
    Ok(())
}

/// Validate Git claims using retained objects, never an asserted path digest.
pub fn validate_git(cwd: &Path, witness: &Reuse, baseline: &Baseline) -> Result<()> {
    validate_baseline_git(cwd, baseline)?;
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
        || !first_parent(cwd, &baseline.revision, &witness.target_revision)
        || crate::gitio::commit_tree(cwd, &baseline.revision)? != baseline.tree
        || crate::gitio::commit_tree(cwd, &witness.revision)? != witness.tree
        || !first_parent(cwd, &baseline.revision, &witness.revision)
        || gates::delta_paths(cwd, &baseline.tree, &witness.tree).as_ref()
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
            if recorded.event_id <= baseline.completion.event_id
                || event.event_id <= recorded.event_id
                || event.repository_id != baseline.repository_id
                || recorded.repository_id != baseline.repository_id
                || digest(baseline.as_ref()) != witness.baseline_digest
                || !scope.permits(
                    &recorded.event_id,
                    &recorded.repository_id,
                    crate::import_authorization::Role::Integration,
                )
            {
                bail!("base pass baseline lacks integration authority");
            }
            witness.validate(baseline, &baseline.profile)?;
            if checked.insert(witness.git_subject()) {
                validate_git(cwd, witness, baseline)?;
            }
        }
    }
    Ok(())
}

/// A gate's earlier observations cannot be displaced by a baseline answer.
pub fn unobserved(state: &ChangeState, gate: &str) -> bool {
    !state
        .verifications
        .iter()
        .any(|entry| entry.gate.as_deref() == Some(gate))
}

#[derive(Clone)]
pub struct Source {
    pub event_id: String,
    pub baseline: std::sync::Arc<Baseline>,
    pub target_revision: String,
}

pub struct Sources {
    baselines: Vec<Source>,
}

impl Sources {
    pub fn for_gate(&self, gate: &str) -> Option<&Source> {
        self.baselines
            .iter()
            .find(|source| source.baseline.gates.contains_key(gate))
    }

    pub fn is_empty(&self) -> bool {
        self.baselines.is_empty()
    }
}

pub fn sources(cwd: &Path, store: &Store, state: &ChangeState, revision: &str) -> Option<Sources> {
    let _selection = crate::perf::phase("base_selection");
    let target = crate::gitio::branch_head(cwd, &state.target_branch).ok()?;
    if !gates::load_at(cwd, &target).ok()?.selection.base_passes {
        return None;
    }
    let scope = store.privilege_scope().ok()?;
    let project = crate::replica::status(store)
        .ok()?
        .project_id
        .unwrap_or_else(|| store.repository_id.clone());
    let retained = crate::gitio::list_refs(cwd, "refs/arc/baselines/")
        .ok()?
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let events = store.repository_events().ok()?;
    let baselines = events
        .iter()
        .rev()
        .filter_map(|event| {
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
                && retained.get(&format!(
                    "refs/arc/baselines/{}/target",
                    baseline.completion.event_id
                )) == Some(&baseline.revision))
            .then_some((event, baseline))
        })
        .take(RETAINED_BASELINES)
        .filter(|(_, baseline)| {
            first_parent(cwd, &baseline.revision, &target)
                && first_parent(cwd, &baseline.revision, revision)
                && baseline.validate().is_ok()
                && validate_baseline_git(cwd, baseline).is_ok()
        })
        .map(|(event, baseline)| Source {
            event_id: event.event_id.clone(),
            baseline: std::sync::Arc::new(baseline.as_ref().clone()),
            target_revision: target.clone(),
        })
        .collect();
    Some(Sources { baselines })
}

#[allow(clippy::too_many_arguments)]
pub fn select(
    state: &ChangeState,
    source: &Source,
    name: &str,
    gate: &Gate,
    revision: &str,
    tree: &str,
    against_target: Option<&str>,
    environment: Option<&str>,
    paths: &[String],
) -> Option<Selected> {
    if gate.binds != gates::Binding::Tree || !unobserved(state, name) {
        return None;
    }
    let entry = source.baseline.gates.get(name)?;
    if !crate::gate_concurrency::accepted_here(
        gate,
        entry.execution.timing.concurrent_batch.as_ref(),
    ) || entry.identity != gates::declaration_identity(gate)
        || !gate.budget_judgement(&entry.execution.timing).accepted()
    {
        return None;
    }
    let observed = &entry.execution.environment;
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
        baseline_digest: digest(source.baseline.as_ref()),
        gate: name.into(),
        revision: revision.into(),
        tree: tree.into(),
        against_target: against_target.map(str::to_owned),
        changed_paths: paths.to_vec(),
        algorithm: "arc-base-paths/1".into(),
        target_revision: source.target_revision.clone(),
    };
    witness
        .validate(&source.baseline, &source.baseline.profile)
        .ok()?;
    Some(Selected {
        witness,
        baseline: std::sync::Arc::clone(&source.baseline),
    })
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
    if crate::gitio::fresh_read(|| crate::gitio::branch_head(&ctx.cwd, target))? != *revision {
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
        if crate::gate_work::was_excused(timing)
            || crate::gate_suspension::was_suspended(timing)
            || declaration.as_deref() != Some(gates::declaration_identity(gate).as_str())
            || !crate::gate_concurrency::accepted_here(gate, timing.concurrent_batch.as_ref())
            || !gate.budget_judgement(timing).fits()
        {
            continue;
        }
        let Some(run) = events.iter().find(|event| &event.event_id == run_id) else {
            continue;
        };
        let Payload::VerificationRecorded {
            gate: executed_gate,
            revision: executed_revision,
            tree: executed_tree,
            tested_tree,
            command,
            timeout_seconds,
            result,
            exit_code,
            attested,
            worktree_dirty,
            tree_moved,
            timed_out,
            environment,
            hostname,
            ..
        } = &execution.payload
        else {
            continue;
        };
        let Payload::VerificationRunStarted {
            revision: run_revision,
            profile,
            event,
            gates,
            ..
        } = &run.payload
        else {
            continue;
        };
        let Some(run_gate) = gates.iter().find(|gate| gate.name == *name) else {
            continue;
        };
        let normalized = match &completion.payload {
            Payload::ChangeIntegrated {
                authorization: Some(auth),
                ..
            } => auth.gates.get(name),
            Payload::LandingCompleted { members, .. } => members
                .iter()
                .find_map(|member| member.authorization.gates.get(name)),
            _ => None,
        };
        let Some(normalized) = normalized else {
            continue;
        };
        let entry = Entry {
            declaration: gates::declaration_value(gate),
            identity: gates::declaration_identity(gate),
            authorized_identity: normalized.coverage_identity.clone().unwrap_or_default(),
            budget_ms: match &completion.payload {
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
                _ => normalized.budget_ms,
            },
            execution: Execution {
                event_id: execution.event_id.clone(),
                digest: digest(execution),
                repository_id: execution.repository_id.clone(),
                change_id: execution.change_id.clone(),
                created_at: execution.created_at,
                gate: executed_gate.clone(),
                run_id: Some(run_id.clone()),
                revision: executed_revision.clone(),
                tree: executed_tree.clone(),
                tested_tree: tested_tree.clone(),
                declaration: declaration.clone(),
                command: command.clone(),
                timeout_seconds: *timeout_seconds,
                result: *result,
                exit_code: *exit_code,
                attested: *attested,
                worktree_dirty: *worktree_dirty,
                tree_moved: *tree_moved,
                timed_out: *timed_out,
                environment: environment.clone(),
                hostname: hostname.clone(),
                timing: timing.clone(),
            },
            run: Run {
                event_id: run.event_id.clone(),
                digest: digest(run),
                repository_id: run.repository_id.clone(),
                change_id: run.change_id.clone(),
                revision: run_revision.clone(),
                profile: profile.clone(),
                event: *event,
                gate: run_gate.clone(),
            },
        };
        let mut candidate = Baseline {
            project_id: project_id.clone(),
            repository_id: store.repository_id.clone(),
            target: target.clone(),
            revision: revision.clone(),
            tree: tree.clone(),
            profile: state.profile.clone(),
            completion: completion_facts(completion, target, revision, evidence),
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
        completion: completion_facts(completion, target, revision, evidence),
        gates: entries,
    };
    baseline.validate()?;
    store.adopt_base_passes()?;
    crate::gitio::update_ref(
        &ctx.cwd,
        &format!("refs/arc/baselines/{}/target", completion.event_id),
        revision,
    )?;
    let mut retained = BTreeSet::from([revision.as_str()]);
    for entry in baseline.gates.values() {
        let source = &entry.execution.revision;
        if retained.insert(source) {
            crate::gitio::update_ref(
                &ctx.cwd,
                &format!(
                    "refs/arc/baselines/{}/executions/{}",
                    completion.event_id, entry.execution.event_id
                ),
                source,
            )?;
        }
    }
    let _lock = store.lock_repository_events()?;
    if store.repository_events()?.iter().any(|event| matches!(&event.payload, Payload::GateBaselineRecorded { baseline } if baseline.completion.event_id == completion.event_id)) { return Ok(()); }
    let mut event = ctx.event(
        store,
        Store::REPOSITORY_SCOPE,
        Payload::GateBaselineRecorded {
            baseline: Box::new(baseline),
        },
    )?;
    store.append_local_repository_event(&mut event)?;
    prune(ctx, store, target, &state.profile)?;
    Ok(())
}

fn prune(ctx: &crate::commands::Ctx, store: &Store, target: &str, profile: &str) -> Result<()> {
    let events = store.repository_events()?;
    let scope = store.privilege_scope()?;
    let expired = events
        .iter()
        .rev()
        .filter_map(|event| match &event.payload {
            Payload::GateBaselineRecorded { baseline }
                if event.repository_id == store.repository_id
                    && scope.admission_for(&event.event_id).is_none()
                    && baseline.target == target
                    && baseline.profile == profile =>
            {
                Some(baseline.completion.event_id.as_str())
            }
            _ => None,
        })
        .skip(RETAINED_BASELINES)
        .collect::<BTreeSet<_>>();
    for (name, _) in crate::gitio::list_refs(&ctx.cwd, "refs/arc/baselines/")? {
        if name
            .strip_prefix("refs/arc/baselines/")
            .and_then(|suffix| suffix.split('/').next())
            .is_some_and(|completion| expired.contains(completion))
        {
            crate::gitio::delete_ref(&ctx.cwd, &name)?;
        }
    }
    Ok(())
}

fn completion_facts(
    event: &Event,
    target: &str,
    revision: &str,
    evidence: &BTreeMap<String, String>,
) -> Completion {
    Completion {
        event_id: event.event_id.clone(),
        repository_id: event.repository_id.clone(),
        digest: digest(event),
        event_type: match event.payload {
            Payload::ChangeIntegrated { .. } => "change-integrated",
            _ => "landing-completed",
        }
        .into(),
        target: target.into(),
        revision: revision.into(),
        gate_evidence: evidence.clone(),
    }
}

pub fn execution(selected: &Selected) -> &Execution {
    &selected.baseline.gates[&selected.gate].execution
}

pub fn derive(
    witness: &Reuse,
    baseline: &Baseline,
    event: &Event,
    run_id: &str,
) -> Result<VerificationEntry> {
    let source = &baseline.gates[&witness.gate].execution;
    Ok(VerificationEntry {
        base_pass: Some(witness.accounting(baseline)),
        declaration: source.declaration.clone(),
        event_id: event.event_id.clone(),
        delta: None,
        run_id: Some(run_id.into()),
        probe: None,
        gate: Some(witness.gate.clone()),
        command: source.command.clone(),
        timeout_seconds: source.timeout_seconds,
        revision: witness.revision.clone(),
        recorded_revision: witness.revision.clone(),
        approved_revision: witness.revision.clone(),
        result: source.result,
        attested: false,
        duration_ms: source.timing.run_ms,
        timing: source.timing.clone(),
        output_tail: None,
        output_artifact: None,
        timed_out: false,
        hostname: source.hostname.clone(),
        runner: None,
        bypass: None,
        tested_tree: source.tested_tree.clone(),
        worktree_dirty: Some(false),
        worktree_dirty_tracked: None,
        worktree_dirty_untracked: None,
        tree_moved: false,
        tree: Some(witness.tree.clone()),
        against_target: witness.against_target.clone(),
        recorded_against_target: witness.against_target.clone(),
        target_revision: None,
        recorded_target_revision: None,
        falsification: None,
        falsification_inferred: None,
        environment: source.environment.clone(),
        created_at: event.created_at,
    })
}

/// Integration completions, rather than verification timestamps, delimit audits.
/// A pass recorded before a publication but integrated after it remains owed.
pub fn publication_participants(
    cwd: &Path,
    store: &Store,
    states: &BTreeMap<String, ChangeState>,
    target: &str,
    profile: &str,
    revision: &str,
    since: Option<&str>,
) -> Result<BTreeMap<String, Gate>> {
    let _audit = crate::perf::phase("base_publication");
    let baselines = store.repository_events()?;
    let mut participants = BTreeMap::new();
    for state in states.values() {
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
            let name = evidence.gate.as_deref().context("base pass has no gate")?;
            let baseline = baselines
                .iter()
                .find_map(|event| match &event.payload {
                    Payload::GateBaselineRecorded { baseline }
                        if event.event_id == base.baseline_event_id =>
                    {
                        Some(baseline)
                    }
                    _ => None,
                })
                .context("publication base-pass baseline is unavailable")?;
            let entry = baseline
                .gates
                .get(name)
                .context("base pass has no source declaration")?;
            let mut declaration = entry.declaration.clone();
            if let Some(seconds) = declaration["timeout"].as_u64() {
                declaration["timeout"] = serde_json::json!(format!("{seconds}s"));
            }
            let (coverage, confine, gate_trailers, invariants_obj) =
                if let Some(map) = declaration.as_object_mut() {
                    let coverage = map.remove("coverage");
                    let confine = map.remove("confine");
                    let gate_trailers = map.remove("gate_trailers");
                    let invariants_obj = map.get("invariants").and_then(|v| v.as_object()).cloned();
                    if let Some(obj) = &invariants_obj {
                        if let Some(path) = obj.get("path").cloned() {
                            map.insert("invariants".into(), path);
                        } else {
                            map.remove("invariants");
                        }
                    }
                    (coverage, confine, gate_trailers, invariants_obj)
                } else {
                    (None, None, None, None)
                };
            let mut gate: Gate = serde_json::from_value(declaration)?;
            if let Some(coverage) = coverage {
                gate.coverage = serde_json::from_value(coverage)?;
            }
            if let Some(confine) = confine {
                gate.confine = serde_json::from_value(confine)?;
            }
            if let Some(gate_trailers) = gate_trailers {
                gate.gate_trailers = serde_json::from_value(gate_trailers)?;
            }
            if let Some(obj) = invariants_obj {
                if let (Some(path), Some(sha256)) = (
                    obj.get("path").and_then(|v| v.as_str()),
                    obj.get("sha256").and_then(|v| v.as_str()),
                ) {
                    gate.resolved_invariants = Some(crate::gate_trailers::ResolvedInvariants {
                        path: path.to_string(),
                        sha256: sha256.to_string(),
                        entries: Vec::new(),
                    });
                }
            }
            gate.budget = entry.budget_ms;
            participants.insert(name.to_owned(), gate);
        }
    }
    Ok(participants)
}

pub fn cited_commits(payload: &Payload) -> Vec<&str> {
    match payload {
        Payload::GateBaselineRecorded { baseline } => std::iter::once(baseline.revision.as_str())
            .chain(
                baseline
                    .gates
                    .values()
                    .map(|entry| entry.execution.revision.as_str()),
            )
            .collect(),
        Payload::VerificationBaseReused { witness, .. } => {
            vec![&witness.revision, &witness.target_revision]
        }
        _ => Vec::new(),
    }
}
