//! Bounded suspensions of gate budget judgement, recorded by the operator.
//!
//! A suspension binds the ledger that recorded it and nothing else: a reader
//! honours one only when its own store wrote the event, so an exported or
//! replicated ledger carries no acceptance with it.

use crate::model::{Event, Payload};
use crate::verification_timing::VerificationTiming;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The longest a suspension may run. It answers a condition of the host, and
/// a host stays loaded for a working day at most before somebody looks again;
/// a longer one would stand in for the declared budget.
pub const CEILING_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suspension {
    pub event_id: String,
    pub actor: String,
    pub reason: String,
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    /// Empty covers every gate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<String>,
}

impl Suspension {
    pub fn covers(&self, gate: &str) -> bool {
        self.gates.is_empty() || self.gates.iter().any(|name| name == gate)
    }

    pub fn in_force(&self, now: DateTime<Utc>) -> bool {
        self.since <= now && now < self.until
    }

    /// A window no recorder could have written is no suspension.
    fn bounded(&self) -> bool {
        (self.until - self.since)
            .num_milliseconds()
            .try_into()
            .is_ok_and(|ms: u64| ms > 0 && ms <= CEILING_MS)
    }

    pub fn scope(&self) -> String {
        if self.gates.is_empty() {
            "every gate".into()
        } else {
            format!("gate {}", self.gates.join(", "))
        }
    }

    /// One line saying what is suspended, until when, by whom and why.
    pub fn describe(&self) -> String {
        format!(
            "budgets suspended for {} until {} by {}: {} (suspension {})",
            self.scope(),
            self.until
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            self.actor,
            self.reason,
            self.event_id
        )
    }
}

/// The suspensions `own` store recorded and nobody resumed, whatever the time.
pub fn recorded(repository: &[Event], own: &str) -> Vec<Suspension> {
    let resumed: BTreeSet<&str> = repository
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::GateBudgetsResumed {
                suspension_event_ids,
            } => Some(suspension_event_ids.iter().map(String::as_str)),
            _ => None,
        })
        .flatten()
        .collect();
    repository
        .iter()
        .filter(|event| event.repository_id == own && !resumed.contains(event.event_id.as_str()))
        .filter_map(|event| match &event.payload {
            Payload::GateBudgetsSuspended {
                until,
                reason,
                gates,
            } => Some(Suspension {
                event_id: event.event_id.clone(),
                actor: event.actor.clone(),
                reason: reason.clone(),
                since: event.created_at,
                until: *until,
                gates: gates.clone(),
            }),
            _ => None,
        })
        .filter(Suspension::bounded)
        .collect()
}

pub fn in_force(recorded: &[Suspension], now: DateTime<Utc>) -> Vec<Suspension> {
    recorded
        .iter()
        .filter(|suspension| suspension.in_force(now))
        .cloned()
        .collect()
}

/// The suspensions in force on `store`'s ledger now.
pub fn in_force_on(store: &crate::store::Store) -> anyhow::Result<Vec<Suspension>> {
    Ok(in_force(
        &recorded(&store.repository_events()?, &store.repository_id),
        Utc::now(),
    ))
}

/// The suspension covering `gate` that ends last.
pub fn covering<'a>(in_force: &'a [Suspension], gate: &str) -> Option<&'a Suspension> {
    in_force
        .iter()
        .filter(|suspension| suspension.covers(gate))
        .max_by_key(|suspension| suspension.until)
}

/// `suspension` while it is still in force: a judgement reads the clock when
/// it is made, however long ago the declarations were read.
pub fn current(suspension: Option<&Suspension>) -> Option<&Suspension> {
    suspension.filter(|suspension| suspension.in_force(Utc::now()))
}

/// Whether the run was accepted over its budget under a suspension when recorded.
pub fn was_suspended(timing: &VerificationTiming) -> bool {
    timing
        .budget_at_run
        .as_ref()
        .is_some_and(|b| b.verdict == crate::gate_budget::BudgetVerdict::Suspended)
}

/// Records an older reader would misread: it skips the suspension and takes a
/// run accepted under one for an ordinary overrun.
pub fn payload_uses_suspension(payload: &Payload) -> bool {
    match payload {
        Payload::GateBudgetsSuspended { .. } | Payload::GateBudgetsResumed { .. } => true,
        Payload::VerificationRecorded { timing, .. }
        | Payload::CandidateVerified { timing, .. } => was_suspended(timing),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn event(id: &str, repository: &str, at: DateTime<Utc>, payload: Payload) -> Event {
        let mut value = serde_json::json!({
            "schema_version": crate::model::SCHEMA_VERSION,
            "event_id": id, "repository_id": repository, "change_id": "repository",
            "actor": "operator", "created_at": at,
        });
        value.as_object_mut().unwrap().extend(
            serde_json::to_value(payload)
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
        );
        serde_json::from_value(value).unwrap()
    }

    fn suspended(until: DateTime<Utc>, gates: &[&str]) -> Payload {
        Payload::GateBudgetsSuspended {
            until,
            reason: "loaded host".into(),
            gates: gates.iter().map(|gate| gate.to_string()).collect(),
        }
    }

    /// Claim: a suspension is in force only inside its own bounded window, on
    /// the store that recorded it, for the gates it names, until resumed.
    /// Rejects a foreign, unbounded, expired or resumed suspension accepting.
    #[test]
    fn only_a_local_bounded_unresumed_suspension_covers_its_gates() {
        let start = Utc::now();
        let hour = Duration::hours(1);
        let events = [
            event("01", "here", start, suspended(start + hour, &["unit"])),
            event("02", "there", start, suspended(start + hour, &[])),
            event(
                "03",
                "here",
                start,
                suspended(start + Duration::hours(25), &[]),
            ),
            event("04", "here", start, suspended(start + hour, &["lint"])),
            event(
                "05",
                "there",
                start,
                Payload::GateBudgetsResumed {
                    suspension_event_ids: vec!["04".into()],
                },
            ),
        ];
        let held = recorded(&events, "here");
        assert_eq!(held.len(), 1, "{held:?}");
        let now = in_force(&held, start + Duration::minutes(30));
        assert_eq!(
            covering(&now, "unit").map(|s| s.event_id.as_str()),
            Some("01")
        );
        assert!(covering(&now, "lint").is_none());
        assert!(in_force(&held, start + hour).is_empty());
        assert!(in_force(&held, start - Duration::seconds(1)).is_empty());
    }
}
