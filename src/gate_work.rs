//! Process-tree work and portable witnesses for busy-host budget judgements.

use crate::model::{Event, Payload, VerifyResult};
use crate::verification_timing::VerificationTiming;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub gate: String,
    pub declaration: String,
    pub hostname: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub execution: Box<Event>,
    pub digest: String,
    pub instructions: u64,
    pub run_ms: u64,
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
    if capacity == 0 {
        return None;
    }
    let own = timing.cpu_user_ms?.checked_add(timing.cpu_system_ms?)?;
    let busy = timing.host_busy_ms?;
    if busy > timing.host_capacity_ms? {
        return None;
    }
    let other = busy.saturating_sub(own);
    Some((u128::from(other) * 10_000 / u128::from(capacity)) as u64)
}

pub fn excuse(timing: &VerificationTiming, budget: u64) -> Option<Excuse> {
    let reference = timing.work_reference.as_ref()?;
    let instructions = timing.instructions?;
    let share = other_cpu_basis_points(timing)?;
    if reference.run_ms > budget
        || reference.instructions == 0
        || u128::from(instructions) * 100 > u128::from(reference.instructions) * 103
        || share < 2500
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
            ..
        } if timing.work_identity.as_ref().is_some_and(|id| {
            id.gate == *gate
                && declaration.as_ref().is_none_or(|d| id.declaration == *d)
                && id.hostname == *hostname
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
            && !was_excused(timing))
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
            self.events = Some(events);
        }
        let best = self
            .events
            .as_ref()
            .expect("loaded history")
            .iter()
            .filter_map(|event| {
                if event.created_at > chrono::Utc::now() {
                    return None;
                }
                let source = reference_timing(event, &identity)?;
                let instructions = source.instructions.filter(|n| *n > 0)?;
                let run_ms = source.run_ms.filter(|r| *r <= budget)?;
                Some((instructions, &event.event_id, run_ms, event))
            })
            .max_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        if let Some((instructions, _, run_ms, event)) = best {
            timing.work_reference = Some(Box::new(Reference {
                execution: Box::new(event.clone()),
                digest: event_digest(event),
                instructions,
                run_ms,
            }));
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
    let uses = |timing: &VerificationTiming| timing.work_reference.is_some() || was_excused(timing);
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
            ..
        } => {
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
            ..
        } => validate(Some(gate), None, Some(hostname), timing)?,
        Payload::PublicationVerified { gates, .. } => {
            for (gate, receipt) in gates {
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
    pub fn finish(self, timing: &mut VerificationTiming) {
        let Some(end) = Self::start() else {
            return;
        };
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
            host_busy_ms: Option<u64>,
            host_capacity_ms: Option<u64>,
            cpu_user_ms: Option<u64>,
            cpu_system_ms: Option<u64>,
        }
        let value: Readings =
            serde_json::from_str(&raw).context("invalid test gate work observations")?;
        timing.instructions = value.instructions;
        timing.host_busy_ms = value.host_busy_ms;
        timing.host_capacity_ms = value.host_capacity_ms;
        timing.cpu_user_ms = value.cpu_user_ms;
        timing.cpu_system_ms = value.cpu_system_ms;
    }
    #[cfg(not(debug_assertions))]
    let _ = timing;
    Ok(())
}
