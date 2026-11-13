//! Host-local measurements of exact declaration sets, separate from gate evidence.

use crate::gates::{self, Gate};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    pub passed: bool,
    pub run_ms: Option<u64>,
    pub cpu_user_ms: Option<u64>,
    pub cpu_system_ms: Option<u64>,
    pub wait_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acceptance {
    pub id: String,
    pub producing_store: String,
    pub host: String,
    pub measured_at: chrono::DateTime<chrono::Utc>,
    pub members: BTreeMap<String, serde_json::Value>,
    pub rounds: Vec<BTreeMap<String, Sample>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub batch_id: String,
    pub acceptance: Acceptance,
}

pub fn members<'a>(
    gates: impl IntoIterator<Item = (&'a String, &'a Gate)>,
) -> BTreeMap<String, serde_json::Value> {
    gates
        .into_iter()
        .filter(|(_, gate)| gate.concurrent)
        .map(|(name, gate)| (name.clone(), gates::declaration_value(gate)))
        .collect()
}

impl Acceptance {
    pub fn same_claim(&self, other: &Self) -> bool {
        self.producing_store == other.producing_store
            && self.host == other.host
            && self.members == other.members
    }

    pub fn valid(&self) -> bool {
        let mut value = self.clone();
        value.id.clear();
        self.id == crate::base_pass::digest(&value)
            && self.members.len() >= 2
            && self
                .members
                .values()
                .all(|value| value["concurrent"] == true)
            && self.rounds.len() == 5
            && self.rounds.iter().all(|round| {
                round.keys().eq(self.members.keys())
                    && round
                        .values()
                        .all(|sample| sample.passed && sample.run_ms.is_some())
            })
    }

    pub fn matches<'a>(&self, gates: impl IntoIterator<Item = (&'a String, &'a Gate)>) -> bool {
        self.valid() && self.members == members(gates)
    }
}

pub fn host() -> Option<String> {
    static HOST: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        let identity = std::fs::read_to_string("/etc/machine-id").ok()?;
        (!identity.trim().is_empty()).then(|| crate::base_pass::digest(&identity.trim()))
    })
    .clone()
}

fn file(store: &crate::store::Store, host: &str) -> std::path::PathBuf {
    store.root.join("concurrent").join(format!(
        "{}.json",
        crate::base_pass::digest(&(host, &store.repository_id))
    ))
}

pub fn load(cwd: &Path) -> Option<Acceptance> {
    let store = crate::store::Store::discover_existing_for_reading(cwd).ok()??;
    let host = host()?;
    let acceptance: Acceptance =
        serde_json::from_slice(&std::fs::read(file(&store, &host)).ok()?).ok()?;
    (acceptance.valid()
        && acceptance.host == host
        && acceptance.producing_store == store.repository_id)
        .then_some(acceptance)
}

pub fn save(store: &crate::store::Store, acceptance: &mut Acceptance) -> Result<()> {
    acceptance.id.clear();
    acceptance.id = crate::base_pass::digest(acceptance);
    ensure!(acceptance.valid(), "concurrent measurement is incomplete");
    store.adopt_concurrency()?;
    let path = file(store, &acceptance.host);
    let directory = path.parent().context("measurement has no parent")?;
    std::fs::create_dir_all(directory)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    crate::commands::write_atomically(
        &path,
        &serde_json::to_vec_pretty(acceptance)?,
        crate::commands::FileMode::Create(0o600),
    )?;
    Ok(())
}

pub fn accepted<'a>(required: &[(&'a String, &'a Gate)]) -> Option<&'a Acceptance> {
    let (_, first) = required.iter().find(|(_, gate)| gate.concurrent)?;
    first
        .concurrent_acceptance
        .as_ref()
        .filter(|acceptance| acceptance.matches(required.iter().copied()))
}

pub fn receipt_matches(receipt: &Receipt, name: Option<&str>, declaration: Option<&str>) -> bool {
    receipt.acceptance.valid()
        && !receipt.batch_id.is_empty()
        && name
            .and_then(|name| receipt.acceptance.members.get(name))
            .is_some_and(|value| Some(crate::base_pass::digest(value)).as_deref() == declaration)
}

pub fn accepted_here(gate: &Gate, batch: Option<&Receipt>) -> bool {
    batch.is_none_or(|batch| {
        gate.concurrent_acceptance.as_ref().is_some_and(|current| {
            current.same_claim(&batch.acceptance) && batch.acceptance.valid()
        })
    })
}

pub fn lock_errors<'a>(required: impl IntoIterator<Item = (&'a String, &'a Gate)>) -> Vec<String> {
    let mut locks = BTreeMap::new();
    let mut errors = Vec::new();
    for (name, gate) in required {
        if !gate.concurrent {
            continue;
        }
        if let Some(lock) = &gate.exclusive {
            if let Some(first) = locks.insert(lock, name) {
                errors.push(format!(
                    "concurrent gates {first} and {name} require the same exclusive lock {lock:?}"
                ));
            }
        }
    }
    errors
}

/// Declaration cost uses a maximum only for an exact locally accepted batch.
pub fn total(required: &[(&String, &Gate)]) -> Option<u64> {
    let accepted = accepted(required).is_some();
    let mut alone = 0_u64;
    let mut together = 0_u64;
    for (_, gate) in required {
        let budget = gate.budget?;
        if accepted && gate.concurrent {
            together = together.max(budget);
        } else {
            alone = alone.checked_add(budget)?;
        }
    }
    alone.checked_add(together)
}

pub fn revoke(store: &crate::store::Store, host: &str) -> Result<()> {
    store.adopt_concurrency()?;
    match std::fs::remove_file(file(store, host)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn payload_uses_concurrency(payload: &crate::model::Payload) -> bool {
    use crate::model::Payload;
    let authorization = |basis: &crate::model::AuthorizationBasis| {
        basis
            .gates
            .values()
            .any(|gate| gate.concurrent || gate.concurrent_acceptance.is_some())
    };
    match payload {
        Payload::VerificationRunStarted {
            mode: crate::model::VerificationRunMode::Declared,
            ..
        } => true,
        Payload::VerificationRecorded { timing, .. }
        | Payload::CandidateVerified { timing, .. } => timing.concurrent_batch.is_some(),
        Payload::ChangeIntegrated {
            authorization: Some(basis),
            ..
        }
        | Payload::ChangeProvisionallyIntegrated {
            authorization: basis,
            ..
        }
        | Payload::ReadyToSend {
            authorization: basis,
            ..
        } => authorization(basis),
        Payload::LandingCompleted { members, .. } => members
            .iter()
            .any(|member| authorization(&member.authorization)),
        Payload::GateBaselineRecorded { baseline } => baseline
            .gates
            .values()
            .any(|gate| gate.execution.timing.concurrent_batch.is_some()),
        _ => false,
    }
}
