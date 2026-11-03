//! Must-land-together relations and landing attempts, read from the
//! repository events that record them.
//!
//! A relation is symmetric and its components are the transitive closure of
//! every declaration not withdrawn since. A landing attempt is pending from
//! its `landing-prepared` event until a `landing-completed` or
//! `landing-abandoned` event names it. A member's integrated closure, and the
//! debt a `--debt` landing declared for it, are derived from the completion:
//! the events this module synthesizes join the member's own events before
//! reduction and are never written to its ledger.

use crate::model::{Event, LandedMember, LandingMembership, Payload};
use crate::state::ChangeState;
use std::collections::{BTreeMap, BTreeSet};

/// Everything the repository events say about relations and landings.
#[derive(Debug, Default, Clone)]
pub struct Landings {
    edges: BTreeSet<(String, String)>,
    prepared: BTreeMap<String, Event>,
    completed: BTreeMap<String, Event>,
    abandoned: BTreeMap<String, Event>,
}

/// Refuse a relation one landing could never satisfy: a closed end, which
/// can no longer land, or two targets, which no single landing moves.
pub fn ensure_relatable(a: &ChangeState, b: &ChangeState) -> anyhow::Result<()> {
    for member in [a, b] {
        if member.is_closed() {
            anyhow::bail!(
                "{} is closed; only open changes can be required to land together",
                member.change_id
            );
        }
    }
    if a.target_branch != b.target_branch {
        anyhow::bail!(
            "{} targets {} and {} targets {}; one landing moves one target",
            a.change_id,
            a.target_branch,
            b.change_id,
            b.target_branch
        );
    }
    Ok(())
}

/// The two ends of a relation as recorded: sorted, distinct.
pub fn pair(a: &str, b: &str) -> Vec<String> {
    let mut pair = vec![a.to_string(), b.to_string()];
    pair.sort();
    pair
}

impl Landings {
    /// Read repository events in their recorded order.
    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a Event>) -> Self {
        let mut landings = Landings::default();
        for event in events {
            match &event.payload {
                Payload::LandsWithDeclared { changes } if changes.len() == 2 => {
                    landings
                        .edges
                        .insert((changes[0].clone(), changes[1].clone()));
                }
                Payload::LandsWithWithdrawn { changes, .. } if changes.len() == 2 => {
                    landings
                        .edges
                        .remove(&(changes[0].clone(), changes[1].clone()));
                }
                Payload::LandingPrepared { attempt, .. } => {
                    landings
                        .prepared
                        .entry(attempt.clone())
                        .or_insert_with(|| event.clone());
                }
                Payload::LandingCompleted { attempt, .. } => {
                    landings
                        .completed
                        .entry(attempt.clone())
                        .or_insert_with(|| event.clone());
                }
                Payload::LandingAbandoned { attempt, .. } => {
                    landings
                        .abandoned
                        .entry(attempt.clone())
                        .or_insert_with(|| event.clone());
                }
                _ => {}
            }
        }
        landings
    }

    /// Whether a live relation joins the two changes directly.
    pub fn related(&self, a: &str, b: &str) -> bool {
        let pair = pair(a, b);
        self.edges.contains(&(pair[0].clone(), pair[1].clone()))
    }

    /// The change's component, itself included.
    pub fn component(&self, change: &str) -> BTreeSet<String> {
        let mut component = BTreeSet::from([change.to_string()]);
        let mut frontier = vec![change.to_string()];
        while let Some(next) = frontier.pop() {
            for (a, b) in &self.edges {
                let other = if a == &next {
                    b
                } else if b == &next {
                    a
                } else {
                    continue;
                };
                if component.insert(other.clone()) {
                    frontier.push(other.clone());
                }
            }
        }
        component
    }

    /// The other members of the change's component, sorted.
    pub fn partners(&self, change: &str) -> Vec<String> {
        self.component(change)
            .into_iter()
            .filter(|member| member != change)
            .collect()
    }

    /// The prepared, unfinished attempt the change is a member of.
    pub fn pending(&self, change: &str) -> Option<&str> {
        self.prepared
            .iter()
            .filter(|(attempt, _)| {
                !self.completed.contains_key(*attempt) && !self.abandoned.contains_key(*attempt)
            })
            .find(|(_, event)| match &event.payload {
                Payload::LandingPrepared { members, .. } => {
                    members.iter().any(|member| member.change_id == change)
                }
                _ => false,
            })
            .map(|(attempt, _)| attempt.as_str())
    }

    /// The events a change's completed landings derive for it, in ID order:
    /// each member's debt, when the landing declared one, then its closure.
    pub fn derived_events(&self, change: &str) -> Vec<Event> {
        let mut derived = Vec::new();
        for completion in self.completed.values() {
            let Payload::LandingCompleted {
                attempt,
                target_branch,
                target_before,
                target_after,
                members,
                started_at,
                ..
            } = &completion.payload
            else {
                continue;
            };
            let Some(member) = members.iter().find(|member| member.change_id == change) else {
                continue;
            };
            if let Some(debt) = &member.debt {
                let mut event = completion.clone();
                event.event_id = debt.event_id.clone();
                event.change_id = change.to_string();
                event.payload = Payload::DebtDeclared {
                    reason: debt.reason.clone(),
                    patchset_id: Some(debt.patchset_id.clone()),
                    missing: debt.missing,
                    coverage: debt.coverage.clone(),
                    production: debt.production.clone(),
                    ineligible: None,
                };
                derived.push(event);
            }
            let mut event = completion.clone();
            event.event_id = member.closure_event_id.clone();
            event.change_id = change.to_string();
            event.payload = member_closure(
                member,
                attempt,
                &completion.event_id,
                target_branch,
                target_before,
                target_after,
                *started_at,
            );
            derived.push(event);
        }
        derived.sort_by(|a, b| a.event_id.cmp(&b.event_id));
        derived
    }
}

fn member_closure(
    member: &LandedMember,
    attempt: &str,
    completion_event_id: &str,
    target_branch: &str,
    target_before: &str,
    target_after: &str,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Payload {
    Payload::ChangeIntegrated {
        authority_grant_event_id: None,
        provisional_resolutions: Vec::new(),
        integrated_commit: member.integrated_commit.clone(),
        source_patchset_id: member.source_patchset_id.clone(),
        source_head: member.source_head.clone(),
        target_branch: target_branch.to_string(),
        target_before: target_before.to_string(),
        authorization: Some(member.authorization.clone()),
        already_contained: false,
        shape: Some(crate::model::IntegrationShape::FastForward),
        started_at,
        landing: Some(LandingMembership {
            attempt: attempt.to_string(),
            completion_event_id: completion_event_id.to_string(),
            range_base: member.range_base.clone(),
            target_after: target_after.to_string(),
        }),
    }
}

/// A change's events with the events its landings derive, merged in ID order.
pub fn with_derived(events: &[Event], derived: Vec<Event>) -> Vec<Event> {
    if derived.is_empty() {
        return events.to_vec();
    }
    let mut merged = Vec::with_capacity(events.len() + derived.len());
    let mut derived = derived.into_iter().peekable();
    for event in events {
        while let Some(next) = derived.peek() {
            if next.event_id < event.event_id {
                merged.push(derived.next().expect("peeked"));
            } else {
                break;
            }
        }
        merged.push(event.clone());
    }
    merged.extend(derived);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, payload: Payload) -> Event {
        let mut event: Event = serde_json::from_value(serde_json::json!({
            "schema_version": crate::model::SCHEMA_VERSION,
            "event_id": id,
            "repository_id": "repo",
            "change_id": "repository",
            "actor": "lead",
            "created_at": "2026-11-03T00:00:00Z",
            "event_type": "landing-abandoned",
            "attempt": "x",
            "reason": "x",
        }))
        .unwrap();
        event.payload = payload;
        event
    }

    #[test]
    fn components_are_transitive_and_withdrawal_splits_them() {
        let events = [
            event(
                "01",
                Payload::LandsWithDeclared {
                    changes: pair("a", "b"),
                },
            ),
            event(
                "02",
                Payload::LandsWithDeclared {
                    changes: pair("c", "b"),
                },
            ),
            event(
                "03",
                Payload::LandsWithDeclared {
                    changes: pair("d", "e"),
                },
            ),
        ];
        let landings = Landings::from_events(&events);
        assert_eq!(landings.partners("a"), ["b", "c"]);
        assert_eq!(landings.partners("d"), ["e"]);
        assert!(landings.partners("f").is_empty());
        let mut events = events.to_vec();
        events.push(event(
            "04",
            Payload::LandsWithWithdrawn {
                changes: pair("b", "c"),
                reason: "split".into(),
            },
        ));
        let landings = Landings::from_events(&events);
        assert_eq!(landings.partners("a"), ["b"]);
        assert!(landings.partners("c").is_empty());
    }
}
