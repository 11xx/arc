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

use crate::model::{Event, LandedMember, LandingMembership, LandingPin, Payload};
use crate::state::ChangeState;
use serde::{Deserialize, Serialize};
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

    pub fn prepared(&self, attempt: &str) -> Option<&Event> {
        self.prepared.get(attempt)
    }

    pub fn completion(&self, attempt: &str) -> Option<&Event> {
        self.completed.get(attempt)
    }

    pub fn abandonment(&self, attempt: &str) -> Option<&Event> {
        self.abandoned.get(attempt)
    }

    /// Refuse a completion naming `state`'s change at a patchset its ledger
    /// does not hold, or at a source head other than the one that patchset
    /// records: the closure it derives would claim what the ledger cannot
    /// show. A head is compared as `state` resolves both, rewrites followed.
    pub fn require_held_patchsets(&self, state: &ChangeState) -> anyhow::Result<()> {
        for completion in self.completed.values() {
            let Payload::LandingCompleted { members, .. } = &completion.payload else {
                continue;
            };
            let Some(member) = members
                .iter()
                .find(|member| member.change_id == state.change_id)
            else {
                continue;
            };
            let Some(patchset) = state
                .patchsets
                .iter()
                .find(|patchset| patchset.id == member.source_patchset_id)
            else {
                anyhow::bail!(
                    "landing completion {} names {} at patchset {}, which its ledger here does \
                     not hold; nothing was imported",
                    completion.event_id,
                    state.change_id,
                    member.source_patchset_id
                );
            };
            let named = state
                .closure
                .as_ref()
                .filter(|closure| {
                    closure
                        .landing
                        .as_ref()
                        .is_some_and(|landing| landing.completion_event_id == completion.event_id)
                })
                .and_then(|closure| closure.source_head.as_deref())
                .unwrap_or(&member.source_head);
            if named != patchset.head {
                anyhow::bail!(
                    "landing completion {} names {} at patchset {} with source head {}, but its \
                     ledger here records {} at {}; nothing was imported",
                    completion.event_id,
                    state.change_id,
                    member.source_patchset_id,
                    member.source_head,
                    patchset.id,
                    patchset.head
                );
            }
        }
        Ok(())
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
                    ineligible: debt.ineligible.clone(),
                    replayed: debt.replayed.clone(),
                    extent: None,
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

/// How far a landing attempt has gone, as its intent file records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// Planned and pinned; nothing authorized, nothing moved.
    Prepared,
    /// Revalidated under the commit guards; the exact completion is saved.
    Authorized,
    /// The target moved and its checkout matches.
    Applied,
    /// The completion is in the ledger.
    Published,
}

pub const INTENT_SCHEMA: &str = "arc-landing-intent/1";

/// What one landing attempt is about to do, written before anything moves
/// and synced at each phase, so an interrupted attempt is finished or
/// abandoned from what it recorded rather than from what the target holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    pub schema: String,
    pub attempt: String,
    pub phase: Phase,
    pub target_branch: String,
    /// The target revision the attempt planned against.
    pub target_before: String,
    /// The revision the attempt installs: a stack's top member's head, or
    /// the head a composed attempt replayed, once it has. Absent until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_after: Option<String>,
    pub members: Vec<LandingPin>,
    /// What a composed attempt replayed, member by member in landing order.
    /// These commits are the attempt's alone: no rewrite map names them,
    /// and only `refs/arc/landing/<attempt>` holds them until the target
    /// does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub composed: Vec<ComposedMember>,
    /// The attempt this one was split from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The exact `landing-completed` event, saved when the attempt is
    /// authorized and published unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<Event>,
}

impl Intent {
    /// The ID this attempt reserved and has not published yet, if any.
    pub fn reservation(&self) -> Option<&str> {
        match self.phase {
            Phase::Authorized | Phase::Applied => self
                .completion
                .as_ref()
                .map(|completion| completion.event_id.as_str()),
            Phase::Prepared | Phase::Published => None,
        }
    }
}

/// One member of a composed attempt: the commit it landed on and the
/// commits it landed as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposedMember {
    pub change_id: String,
    /// The commit the member's first commit was placed on.
    pub parent: String,
    /// The member's last landed commit.
    pub tip: String,
    /// Each original commit with the commit it landed as, in order. A member
    /// arc did not replay maps each commit to itself.
    pub commits: Vec<(String, String)>,
}

/// The ref that pins a composed attempt's head until the target holds it.
pub fn pin_ref(attempt: &str) -> String {
    format!("refs/arc/landing/{attempt}")
}

/// The ref that moves with the target in one transaction and says the
/// attempt's own compare-and-swap happened.
pub fn applied_ref(attempt: &str) -> String {
    format!("refs/arc/landing-applied/{attempt}")
}

/// Whether a repository event records a relation or a landing attempt.
pub fn is_landing_fact(event: &Event) -> bool {
    matches!(
        event.payload,
        Payload::LandsWithDeclared { .. }
            | Payload::LandsWithWithdrawn { .. }
            | Payload::LandingPrepared { .. }
            | Payload::LandingCompleted { .. }
            | Payload::LandingAbandoned { .. }
    )
}

/// One change's events joined with what the completed landings among
/// `repository_events` derive for it: the history every reader of a
/// change's closure or debt reduces.
pub fn joined(events: &[Event], repository_events: &[Event]) -> Vec<Event> {
    let Some(change_id) = events.first().map(|event| event.change_id.as_str()) else {
        return Vec::new();
    };
    with_derived(
        events,
        Landings::from_events(repository_events).derived_events(change_id),
    )
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
