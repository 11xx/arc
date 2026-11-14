//! Process-tree work and portable witnesses for busy-host budget judgements.

use crate::model::{Event, Payload, VerifyResult};
use crate::verification_timing::VerificationTiming;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Host {
    pub logical_cpus: u64,
    pub cpu_model: String,
    pub unrestricted: bool,
}
impl Host {
    pub fn read() -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
            let cpu_model = info
                .lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| matches!(k.trim(), "model name" | "Processor"))?
                .1
                .trim()
                .to_owned();
            let logical_cpus =
                u64::try_from(unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) }).ok()?;
            let mut mask = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
            if unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&mask), &mut mask) } != 0 {
                return None;
            }
            Some(Self {
                logical_cpus,
                cpu_model,
                unrestricted: unsafe { libc::CPU_COUNT(&mask) } as u64 == logical_cpus,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
}

/// A leader that narrows its inherited CPU set cannot use whole-host capacity.
pub fn check_gate_affinity(timing: &mut VerificationTiming, pid: u32) {
    #[cfg(target_os = "linux")]
    if let Some(host) = &mut timing.host_cpu {
        let mut mask = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
        // SAFETY: the unreaped command still owns its PID; sched_getaffinity initializes mask.
        host.unrestricted &=
            unsafe { libc::sched_getaffinity(pid as i32, std::mem::size_of_val(&mask), &mut mask) }
                == 0
                && unsafe { libc::CPU_COUNT(&mask) } as u64 == host.logical_cpus;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (timing, pid);
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub gate: String,
    pub declaration: String,
    pub hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<Host>,
}
impl Identity {
    pub fn new(
        gate: String,
        declaration: String,
        hostname: String,
        timing: &VerificationTiming,
    ) -> Self {
        Self {
            gate,
            declaration,
            hostname,
            host: timing.host_cpu.as_deref().cloned(),
        }
    }
}

/// Recorded thresholds belong to the rule that produced the receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rule {
    pub version: u64,
    pub instruction_ceiling_basis_points: u64,
    pub minimum_instruction_rate_basis_points: u64,
    pub minimum_other_basis_points: u64,
    pub wall_slack_basis_points: u64,
    pub minimum_prior_window_ms: u64,
}
impl Rule {
    fn current() -> Self {
        Self {
            version: 1,
            instruction_ceiling_basis_points: 10300,
            minimum_instruction_rate_basis_points: 1000,
            minimum_other_basis_points: 2500,
            wall_slack_basis_points: 12500,
            minimum_prior_window_ms: 100,
        }
    }
}
impl Default for Rule {
    fn default() -> Self {
        Self::current()
    }
}
impl Rule {
    fn supported(&self) -> bool {
        self == &Self::current()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Window {
    pub busy_ms: u64,
    pub capacity_ms: u64,
    pub arc_cpu_ms: u64,
    pub elapsed_ms: u64,
}
impl Window {
    fn share(&self) -> Option<u64> {
        (self.capacity_ms > 0 && self.busy_ms <= self.capacity_ms).then(|| {
            (u128::from(self.busy_ms.saturating_sub(self.arc_cpu_ms)) * 10000
                / u128::from(self.capacity_ms)) as u64
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Conditions {
    pub rule: Rule,
    pub prior: Option<Window>,
    pub concurrent_unattributed: bool,
    pub invocation_cpu_ms: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tree {
    pub tree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub clean: bool,
    pub unmoved: bool,
    pub parallel_probe: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub execution: Box<Event>,
    pub digest: String,
    pub instructions: u64,
    pub run_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ancestry: Option<[String; 2]>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Excuse {
    pub reference_event_id: String,
    pub reference_instructions: u64,
    pub instructions: u64,
    pub other_cpu_basis_points: u64,
}

pub fn was_excused(timing: &VerificationTiming) -> bool {
    timing
        .budget_at_run
        .as_ref()
        .is_some_and(|b| b.verdict == crate::gate_budget::BudgetVerdict::Excused)
}
pub fn other_cpu_basis_points(timing: &VerificationTiming) -> Option<u64> {
    let capacity = timing.host_capacity_ms?;
    let tree = observed_tree_cpu_ms(timing)?;
    let own = if let Some(conditions) = &timing.work_conditions {
        tree.max(conditions.invocation_cpu_ms?)
    } else {
        tree
    };
    let busy = timing.host_busy_ms?;
    if capacity == 0 || busy > capacity {
        return None;
    }
    Some((u128::from(busy.saturating_sub(own)) * 10000 / u128::from(capacity)) as u64)
}

fn plausible_rate(
    count: u64,
    reference_user: u64,
    reference_count: u64,
    user: u64,
    floor: u64,
) -> bool {
    let left = u128::from(count)
        .checked_mul(u128::from(reference_user))
        .and_then(|n| n.checked_mul(10000));
    let right = u128::from(reference_count)
        .checked_mul(u128::from(user))
        .and_then(|n| n.checked_mul(u128::from(floor)));
    left.zip(right).is_some_and(|(l, r)| l >= r)
}

pub fn excuse(timing: &VerificationTiming, budget: u64) -> Option<Excuse> {
    let reference = timing.work_reference.as_ref()?;
    let instructions = timing.instructions?;
    let share = other_cpu_basis_points(timing)?;
    if reference.run_ms > budget || reference.instructions == 0 {
        return None;
    }
    // A judgement without its recorded rule has nothing to be read by.
    let conditions = timing.work_conditions.as_ref()?;
    let rule = &conditions.rule;
    let prior = conditions.prior.as_ref()?;
    let identity = timing.work_identity.as_ref()?;
    let host = identity.host.as_ref()?;
    let source = reference_timing(&reference.execution, identity)?;
    let user_ms = timing.cpu_user_ms.filter(|n| *n > 0)?;
    let source_user_ms = source.cpu_user_ms.filter(|n| *n > 0)?;
    // The reference's clock already holds the delay of the load it ran under,
    // so the allowance scales by the capacity each run had left.
    let source_share = other_cpu_basis_points(source).filter(|s| *s < 10000)?;
    if !rule.supported()
        || !host.unrestricted
        || conditions.concurrent_unattributed
        || prior.elapsed_ms < rule.minimum_prior_window_ms
        || prior.share()? < rule.minimum_other_basis_points
        || share < rule.minimum_other_basis_points
        || share >= 10000
        || instructions == 0
        || u128::from(instructions) * 10000
            > u128::from(reference.instructions) * u128::from(rule.instruction_ceiling_basis_points)
        || !plausible_rate(
            instructions,
            source_user_ms,
            reference.instructions,
            user_ms,
            rule.minimum_instruction_rate_basis_points,
        )
        || u128::from(timing.run_ms?) * u128::from(10000 - share) * 10000
            > u128::from(reference.run_ms)
                * u128::from(rule.wall_slack_basis_points)
                * u128::from(10000 - source_share)
    {
        return None;
    }
    Some(Excuse {
        reference_event_id: reference.execution.event_id.clone(),
        reference_instructions: reference.instructions,
        instructions,
        other_cpu_basis_points: share,
    })
}

/// Direct runs only: copied, attested and excused observations cannot establish work.
fn observations(event: &Event) -> Vec<(&str, &VerificationTiming)> {
    match &event.payload {
        Payload::VerificationRecorded {
            gate: Some(gate),
            declaration,
            hostname,
            timing,
            result: VerifyResult::Pass,
            exit_code: Some(0),
            attested: false,
            timed_out: false,
            tested_tree,
            worktree_dirty,
            tree_moved,
            ..
        } if timing.work_identity.as_ref().is_some_and(|id| {
            id.gate == *gate
                && declaration.as_ref().is_none_or(|d| id.declaration == *d)
                && id.hostname == *hostname
                && (id.host.is_none()
                    || (*worktree_dirty == Some(false)
                        && !*tree_moved
                        && timing
                            .work_tree
                            .as_ref()
                            .is_some_and(|t| tested_tree.as_ref() == Some(&t.tree))))
        }) =>
        {
            vec![(gate, timing)]
        }
        Payload::CandidateVerified {
            gate,
            hostname,
            timing,
            result: VerifyResult::Pass,
            exit_code: Some(0),
            timed_out: false,
            ..
        } if timing
            .work_identity
            .as_ref()
            .is_some_and(|id| id.gate == *gate && id.hostname == *hostname) =>
        {
            vec![(gate, timing)]
        }
        Payload::PublicationVerified { gates, .. } => gates
            .iter()
            .filter(|(gate, receipt)| {
                receipt.result == VerifyResult::Pass
                    && receipt.timing.work_identity.as_ref().is_some_and(|id| {
                        id.gate == **gate && Some(&id.hostname) == receipt.hostname.as_ref()
                    })
            })
            .map(|(gate, receipt)| (gate.as_str(), &receipt.timing))
            .collect(),
        _ => Vec::new(),
    }
}

fn reference_timing<'a>(event: &'a Event, identity: &Identity) -> Option<&'a VerificationTiming> {
    observations(event).into_iter().find_map(|(gate, timing)| {
        (gate == identity.gate
            && timing.work_identity.as_deref() == Some(identity)
            && !was_excused(timing)
            && !timing.work_copied
            && (identity.host.is_none()
                || timing
                    .work_tree
                    .as_ref()
                    .is_some_and(|t| t.clean && t.unmoved && !t.parallel_probe)))
        .then_some(timing)
    })
}

/// A single lazy history read serves all eligible overruns in an invocation.
#[derive(Default)]
pub struct References {
    events: Option<Vec<Event>>,
}

impl References {
    pub fn attach(
        &mut self,
        store: &crate::store::Store,
        cwd: &std::path::Path,
        identity: Identity,
        budget: Option<u64>,
        timing: &mut VerificationTiming,
    ) -> Result<()> {
        if timing.instructions.is_none() {
            return Ok(());
        }
        timing.work_identity = Some(Box::new(identity.clone()));
        let Some(budget) = budget.filter(|b| timing.run_ms.is_some_and(|r| r > *b)) else {
            return Ok(());
        };
        if !known_host(&identity.hostname) {
            return Ok(());
        }
        if self.events.is_none() {
            let mut events = store.load_repository_events()?;
            for change in store.list_change_ids()? {
                events.extend(store.load_events(&change)?);
            }
            events.sort_by(|a, b| (b.created_at, &b.event_id).cmp(&(a.created_at, &a.event_id)));
            self.events = Some(events);
        }
        let best = self
            .events
            .as_ref()
            .expect("loaded history")
            .iter()
            .find_map(|event| {
                if event.created_at > chrono::Utc::now() {
                    return None;
                }
                let source = reference_timing(event, &identity)?;
                let instructions = source.instructions.filter(|n| *n > 0)?;
                if identity.host.is_some() && source.cpu_user_ms.is_none_or(|n| n == 0) {
                    return None;
                }
                let run_ms = source.run_ms.filter(|r| *r <= budget)?;
                let ancestry = if identity.host.is_some() {
                    let destination = timing.work_tree.as_ref()?;
                    let origin = source.work_tree.as_ref()?;
                    if origin.tree == destination.tree {
                        None
                    } else if crate::gitio::is_ancestor(
                        cwd,
                        &store
                            .rewrites()
                            .ok()?
                            .current(origin.revision.as_deref()?)
                            .ok()?,
                        destination.revision.as_deref()?,
                    )
                    .unwrap_or(false)
                    {
                        Some([origin.revision.clone()?, destination.revision.clone()?])
                    } else {
                        return None;
                    }
                } else {
                    None
                };
                Some((instructions, run_ms, event, ancestry))
            });
        if let Some((instructions, run_ms, event, ancestry)) = best {
            timing.work_reference = Some(Box::new(Reference {
                execution: Box::new(event.clone()),
                digest: event_digest(event),
                instructions,
                run_ms,
                ancestry,
            }));
        }
        if let Some(reference) = &timing.work_reference {
            if let (Some(c), Some(instructions), Some(user), Some(source_user)) = (
                &timing.work_conditions,
                timing.instructions,
                timing.cpu_user_ms,
                reference_timing(&reference.execution, &identity).and_then(|s| s.cpu_user_ms),
            ) {
                if instructions == 0
                    || (user > 0
                        && source_user > 0
                        && !plausible_rate(
                            instructions,
                            source_user,
                            reference.instructions,
                            user,
                            c.rule.minimum_instruction_rate_basis_points,
                        ))
                {
                    timing.instructions = None;
                }
            }
        }
        Ok(())
    }
}

fn event_digest(event: &Event) -> String {
    crate::base_pass::digest(&serde_json::to_value(event).expect("serializable execution"))
}

fn known_host(host: &str) -> bool {
    !host.trim().is_empty() && host != "unknown"
}

pub fn payload_uses_work(payload: &Payload) -> bool {
    let uses = |timing: &VerificationTiming| {
        (timing.work_copied && timing.work_identity.is_some())
            || timing.work_reference.is_some()
            || was_excused(timing)
    };
    match payload {
        Payload::VerificationRecorded { timing, .. }
        | Payload::CandidateVerified { timing, .. } => uses(timing),
        Payload::PublicationVerified { gates, .. } => {
            gates.values().any(|receipt| uses(&receipt.timing))
        }
        _ => false,
    }
}

pub fn validate_timing(
    event_id: &str,
    created_at: chrono::DateTime<chrono::Utc>,
    gate: Option<&str>,
    declaration: Option<&str>,
    hostname: Option<&str>,
    timing: &VerificationTiming,
) -> Result<()> {
    if let Some(tree) = &timing.work_tree {
        ensure!(
            crate::gitio::is_full_object_id(&tree.tree)
                && tree
                    .revision
                    .as_deref()
                    .is_none_or(crate::gitio::is_full_object_id),
            "invalid work tree coordinate"
        );
    }
    if let Some(identity) = &timing.work_identity {
        ensure!(
            gate == Some(identity.gate.as_str())
                && declaration.is_none_or(|d| d == identity.declaration)
                && hostname.is_some_and(|h| h == identity.hostname)
                && crate::base_pass::valid_digest(&identity.declaration),
            "invalid gate work identity"
        );
    }
    ensure!(
        timing
            .host_busy_ms
            .zip(timing.host_capacity_ms)
            .is_none_or(|(b, c)| b <= c),
        "invalid host CPU accounting"
    );
    if let Some(conditions) = &timing.work_conditions {
        let r = &conditions.rule;
        ensure!(
            r.supported(),
            "thresholds do not match busy-host rule version"
        );
        ensure!(
            conditions
                .prior
                .as_ref()
                .is_none_or(|w| w.busy_ms <= w.capacity_ms),
            "invalid prior host accounting"
        );
    }
    if let Some(identity) = &timing.work_identity {
        if let Some(host) = &identity.host {
            ensure!(
                timing.work_conditions.is_some(),
                "host identity has no rule version"
            );
            ensure!(
                host.logical_cpus > 0
                    && !host.cpu_model.trim().is_empty()
                    && timing.host_cpu.as_deref() == Some(host),
                "invalid host CPU identity"
            );
        }
    }
    if let Some(reference) = &timing.work_reference {
        let identity = timing
            .work_identity
            .as_ref()
            .context("work reference has no identity")?;
        ensure!(
            known_host(&identity.hostname),
            "work reference has no known host"
        );
        let source = &reference.execution;
        crate::ids::validate_id_component(&source.event_id)?;
        crate::ids::validate_id_component(&source.repository_id)?;
        crate::ids::validate_id_component(&source.change_id)?;
        ensure!(
            source.event_id.as_str() < event_id
                && source.created_at <= created_at
                && reference.digest == event_digest(source),
            "invalid or later work reference"
        );
        validate_event(source)?;
        let source_timing = reference_timing(source, identity).context(
            "work reference is not a direct unexcused pass of this declaration and host",
        )?;
        if timing.work_conditions.is_some() {
            let origin = source_timing
                .work_tree
                .as_ref()
                .context("reference has no tree")?;
            let destination = timing.work_tree.as_ref().context("run has no tree")?;
            ensure!(
                origin.tree == destination.tree
                    || reference
                        .ancestry
                        .as_ref()
                        .is_some_and(|a| Some(&a[0]) == origin.revision.as_ref()
                            && Some(&a[1]) == destination.revision.as_ref()),
                "work reference has no destination lineage"
            );
        }
        ensure!(
            source_timing.instructions == Some(reference.instructions)
                && reference.instructions > 0
                && source_timing.run_ms == Some(reference.run_ms),
            "work reference measurements mismatch"
        );
    }
    let has_excuse = timing
        .budget_at_run
        .as_ref()
        .is_some_and(|b| b.excuse.is_some());
    if timing.work_reference.is_some() || was_excused(timing) || has_excuse {
        let judgement = timing
            .budget_at_run
            .as_ref()
            .context("work reference has no recorded budget")?;
        let expected = crate::gate_budget::judge(judgement.budget_ms, &judgement.sources, timing);
        ensure!(
            judgement.run_ms == timing.run_ms
                && judgement.verdict == expected.verdict
                && judgement.excuse == expected.excuse,
            "invalid recorded busy-host judgement"
        );
    }
    Ok(())
}

pub fn validate_event(event: &Event) -> Result<()> {
    let validate = |gate, declaration, hostname, timing| {
        validate_timing(
            &event.event_id,
            event.created_at,
            gate,
            declaration,
            hostname,
            timing,
        )
    };
    match &event.payload {
        Payload::VerificationRecorded {
            gate,
            declaration,
            hostname,
            timing,
            attested,
            tested_tree,
            revision,
            worktree_dirty,
            tree_moved,
            ..
        } => {
            ensure!(
                !timing.work_copied,
                "direct verification marked as copied work"
            );
            if let Some(tree) = &timing.work_tree {
                ensure!(
                    tested_tree.as_ref().is_none_or(|t| t == &tree.tree)
                        && tree.revision.as_ref() == Some(revision)
                        && tree.clean == (*worktree_dirty == Some(false))
                        && tree.unmoved == !*tree_moved,
                    "work tree does not match execution"
                );
            }
            ensure!(
                !*attested || timing.work_reference.is_none(),
                "attestation has a work reference"
            );
            validate(
                gate.as_deref(),
                declaration.as_deref(),
                Some(hostname),
                timing,
            )?;
        }
        Payload::CandidateVerified {
            gate,
            hostname,
            timing,
            tree,
            ..
        } => {
            ensure!(
                timing.work_tree.as_ref().is_none_or(|t| &t.tree == tree),
                "candidate work tree mismatch"
            );
            ensure!(
                !timing.work_copied,
                "candidate execution marked as copied work"
            );
            validate(Some(gate), None, Some(hostname), timing)?;
        }
        Payload::PublicationVerified { gates, tree, .. } => {
            for (gate, receipt) in gates {
                ensure!(
                    receipt
                        .timing
                        .work_tree
                        .as_ref()
                        .is_none_or(|t| &t.tree == tree),
                    "publication work tree mismatch"
                );
                validate(
                    Some(gate),
                    None,
                    receipt.hostname.as_deref(),
                    &receipt.timing,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Aggregate CPU ticks; guest time is already included in user and nice.
pub struct HostSample {
    busy: u64,
    ticks_per_second: u64,
    started_ns: u64,
    cores: u64,
}
impl HostSample {
    pub fn start() -> Option<Self> {
        let text = std::fs::read_to_string("/proc/stat").ok()?;
        let fields = text
            .lines()
            .next()?
            .split_whitespace()
            .skip(1)
            .take(8)
            .map(str::parse::<u64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .ok()?;
        if fields.len() != 8 {
            return None;
        }
        let busy = [0, 1, 2, 5, 6, 7]
            .into_iter()
            .try_fold(0_u64, |n, i| n.checked_add(fields[i]))?;
        // SAFETY: sysconf takes no borrowed memory.
        let ticks_per_second = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) })
            .ok()?
            .max(1);
        Some(Self {
            busy,
            ticks_per_second,
            started_ns: crate::verification_timing::monotonic_ns()?,
            cores: text
                .lines()
                .filter(|l| {
                    l.strip_prefix("cpu")
                        .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_digit()))
                })
                .count() as u64,
        })
    }
    pub fn finish(&self, timing: &mut VerificationTiming) {
        let Some(end) = Self::start() else {
            return;
        };
        if let Some(host) = &mut timing.host_cpu {
            let Some(current) = Host::read() else {
                host.unrestricted = false;
                return;
            };
            if host.logical_cpus != current.logical_cpus
                || host.cpu_model != current.cpu_model
                || host.logical_cpus != self.cores
            {
                host.unrestricted = false;
                return;
            }
            host.unrestricted &= current.unrestricted;
        }
        if self.cores == 0
            || self.cores != end.cores
            || self.ticks_per_second != end.ticks_per_second
        {
            return;
        }
        let Some(ticks) = end.busy.checked_sub(self.busy) else {
            return;
        };
        let Some(ns) = end.started_ns.checked_sub(self.started_ns) else {
            return;
        };
        let busy = u128::from(ticks) * 1000 / u128::from(self.ticks_per_second);
        let capacity = u128::from(ns) * u128::from(self.cores) / 1_000_000;
        if busy <= capacity {
            timing.host_busy_ms = u64::try_from(busy).ok();
            timing.host_capacity_ms = u64::try_from(capacity).ok();
        }
    }
}

/// Debug executables alone accept controlled observations, as fixture input.
pub fn test_observations(timing: &mut VerificationTiming) -> Result<()> {
    #[cfg(debug_assertions)]
    if let Ok(raw) = std::env::var("ARC_TEST_GATE_WORK") {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Readings {
            instructions: Option<u64>,
            tree_cpu_ms: Option<u64>,
            host_busy_ms: Option<u64>,
            host_capacity_ms: Option<u64>,
            cpu_user_ms: Option<u64>,
            cpu_system_ms: Option<u64>,
            prior: Option<Window>,
            host: Option<Host>,
            rule: Option<Rule>,
            invocation_cpu_ms: Option<u64>,
        }
        let value: Readings =
            serde_json::from_str(&raw).context("invalid test gate work observations")?;
        timing.instructions = value.instructions;
        timing.host_busy_ms = value.host_busy_ms;
        timing.host_capacity_ms = value.host_capacity_ms;
        timing.cpu_user_ms = value.cpu_user_ms;
        timing.cpu_system_ms = value.cpu_system_ms;
        timing.tree_cpu_ms = value
            .tree_cpu_ms
            .or_else(|| {
                value
                    .cpu_user_ms
                    .zip(value.cpu_system_ms)
                    .and_then(|(u, s)| u.checked_add(s))
            })
            .map(Box::new);
        timing.host_cpu = value
            .host
            .map(Box::new)
            .or_else(|| timing.host_cpu.take())
            .or_else(|| Host::read().map(Box::new));
        timing.work_conditions = Some(Box::new(Conditions {
            rule: value.rule.unwrap_or_default(),
            prior: value.prior,
            concurrent_unattributed: false,
            invocation_cpu_ms: value.invocation_cpu_ms.or_else(|| {
                value
                    .cpu_user_ms
                    .zip(value.cpu_system_ms)
                    .and_then(|(u, s)| u.checked_add(s))
            }),
        }));
    }
    #[cfg(not(debug_assertions))]
    let _ = timing;
    Ok(())
}

#[derive(Clone, Copy)]
pub struct ArcCpu {
    own: u64,
    children: u64,
}
pub fn arc_cpu() -> Option<ArcCpu> {
    let mut values = [0_u64; 2];
    for (value, who) in values
        .iter_mut()
        .zip([libc::RUSAGE_SELF, libc::RUSAGE_CHILDREN])
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage initializes usage on success.
        if unsafe { libc::getrusage(who, usage.as_mut_ptr()) } != 0 {
            return None;
        }
        let usage = unsafe { usage.assume_init() };
        for time in [usage.ru_utime, usage.ru_stime] {
            *value = value
                .checked_add(u64::try_from(time.tv_sec).ok()?.checked_mul(1000)?)?
                .checked_add(u64::try_from(time.tv_usec).ok()? / 1000)?;
        }
    }
    Some(ArcCpu {
        own: values[0],
        children: values[1],
    })
}
pub fn observed_tree_cpu_ms(timing: &VerificationTiming) -> Option<u64> {
    let waited = timing
        .cpu_user_ms
        .zip(timing.cpu_system_ms)
        .and_then(|(u, s)| u.checked_add(s));
    match (timing.tree_cpu_ms.as_deref().copied(), waited) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}
fn tree_cpu(timing: &VerificationTiming) -> Option<u64> {
    timing.tree_cpu_ms.as_ref()?;
    observed_tree_cpu_ms(timing)
}
/// Tree CPU that no wait reported: children the tree reaped automatically.
/// Waited CPU already reaches arc through its own children's usage.
fn unwaited_tree_cpu(timing: &VerificationTiming) -> Option<u64> {
    let waited = timing
        .cpu_user_ms
        .zip(timing.cpu_system_ms)
        .map_or(0, |(u, s)| u.saturating_add(s));
    Some(tree_cpu(timing)?.saturating_sub(waited))
}
pub fn cpu_during(before: Option<ArcCpu>, timing: &VerificationTiming) -> Option<u64> {
    let before = before?;
    let after = arc_cpu()?;
    // Reaped, the tree's waited CPU is among arc's children; not yet reaped,
    // only the tree's own clock holds it.
    after.own.checked_sub(before.own)?.checked_add(
        after
            .children
            .checked_sub(before.children)?
            .checked_add(unwaited_tree_cpu(timing)?)?
            .max(tree_cpu(timing)?),
    )
}
struct Invocation {
    host: HostSample,
    cpu: ArcCpu,
    trees: std::sync::atomic::AtomicU64,
    complete: std::sync::atomic::AtomicBool,
}
static INVOCATION: std::sync::OnceLock<Option<Invocation>> = std::sync::OnceLock::new();
pub fn record_tree_cpu(timing: &VerificationTiming) {
    use std::sync::atomic::Ordering::Relaxed;
    if let Some(Some(invocation)) = INVOCATION.get() {
        if let Some(cpu) = tree_cpu(timing) {
            let _ = invocation
                .trees
                .fetch_update(Relaxed, Relaxed, |n| Some(n.saturating_add(cpu)));
        } else {
            invocation.complete.store(false, Relaxed);
        }
    }
}
pub fn begin_invocation() {
    INVOCATION.get_or_init(|| {
        Some(Invocation {
            host: HostSample::start()?,
            cpu: arc_cpu()?,
            trees: std::sync::atomic::AtomicU64::new(0),
            complete: std::sync::atomic::AtomicBool::new(true),
        })
    });
}
pub fn conditions_before_spawn() -> Conditions {
    let prior = INVOCATION.get().and_then(Option::as_ref).and_then(|start| {
        use std::sync::atomic::Ordering::Relaxed;
        if !start.complete.load(Relaxed) {
            return None;
        }
        let current = arc_cpu()?;
        let mut timing = VerificationTiming::default();
        start.host.finish(&mut timing);
        Some(Window {
            busy_ms: timing.host_busy_ms?,
            capacity_ms: timing.host_capacity_ms?,
            // A tree's holder may not be reaped yet, so its waited CPU can be
            // missing from arc's children; every earlier tree is subtracted in
            // full beside them. Counting a reaped tree twice only lowers the
            // share left to other work.
            arc_cpu_ms: current
                .own
                .checked_sub(start.cpu.own)?
                .checked_add(current.children.checked_sub(start.cpu.children)?)?
                .checked_add(start.trees.load(Relaxed))?,
            elapsed_ms: (crate::verification_timing::monotonic_ns()?
                .checked_sub(start.host.started_ns)?)
                / 1_000_000,
        })
    });
    Conditions {
        rule: Rule::default(),
        prior,
        concurrent_unattributed: false,
        invocation_cpu_ms: None,
    }
}
pub fn refuse_unattributed_batch(timing: &mut VerificationTiming) {
    if let Some(c) = &mut timing.work_conditions {
        c.concurrent_unattributed = true;
    }
}
