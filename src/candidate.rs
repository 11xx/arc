//! The candidate ledger: registrations, judgements, and retirements, replayed
//! from repository-scoped events and checked on the way in.
//!
//! A registration is written once and nothing later alters it. Two
//! registrations of one tree are two identities that share storage and nothing
//! else. The rules here are the ones that need no local objects, so a bundle's
//! candidate events are judged by exactly the rules a local registration is;
//! the checks that read the object store or a change's log live with the
//! command that registers.

use crate::ids;
use crate::model::{CandidateBriefRef, CandidateJudgement, Event, Payload};
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

/// Where a registered candidate's tree is pinned.
pub fn candidate_ref(candidate_id: &str) -> String {
    format!("{CANDIDATE_REF_PREFIX}{candidate_id}")
}

pub const CANDIDATE_REF_PREFIX: &str = "refs/arc/candidate/";

#[derive(Debug, Clone)]
pub struct Registration {
    pub candidate_id: String,
    pub tree: String,
    pub brief: CandidateBriefRef,
    pub producers: Vec<String>,
    pub parents: Vec<String>,
    pub adopts: Vec<String>,
    pub episodes: Vec<String>,
    pub event_id: String,
    pub declarant: String,
    pub recorded_at: DateTime<Utc>,
}

impl Registration {
    /// The contract a parent must share: the change and the brief version.
    /// The digest follows from those two.
    fn contract(&self) -> (&str, &str) {
        (&self.brief.change_id, &self.brief.brief_event_id)
    }
}

#[derive(Debug, Clone)]
pub struct Judgement {
    pub candidate_id: String,
    pub judgement: CandidateJudgement,
    pub reason: String,
    pub event_id: String,
    pub declarant: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Retirement {
    pub event_id: String,
    pub declarant: String,
    pub recorded_at: DateTime<Utc>,
}

/// A record that keeps a candidate's content wanted: a selection, a
/// promotion, or a declared root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub kind: &'static str,
    pub event_id: String,
    /// The candidate the root names directly.
    pub candidate_id: String,
}

impl fmt::Display for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} (names {})",
            self.kind, self.event_id, self.candidate_id
        )
    }
}

/// A write the ledger refuses. Each carries its stable code first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Malformed {
        event_id: String,
        detail: String,
    },
    DuplicateCandidate(String),
    NoProducers(String),
    UnknownParent {
        candidate: String,
        parent: String,
    },
    ParentOtherContract {
        candidate: String,
        parent: String,
        parent_contract: String,
        contract: String,
    },
    UnknownAdopted {
        candidate: String,
        adopted: String,
    },
    AdoptionDropsProducer {
        candidate: String,
        adopted: String,
        dropped: Vec<String>,
    },
    UnknownCandidate(String),
    SelfSupersession(String),
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::Malformed { .. } => "malformed-candidate-event",
            Refusal::DuplicateCandidate(_) => "duplicate-candidate",
            Refusal::NoProducers(_) => "no-producers",
            Refusal::UnknownParent { .. } => "unknown-parent",
            Refusal::ParentOtherContract { .. } => "parent-other-contract",
            Refusal::UnknownAdopted { .. } => "unknown-adopted",
            Refusal::AdoptionDropsProducer { .. } => "adoption-drops-producer",
            Refusal::UnknownCandidate(_) => "unknown-candidate",
            Refusal::SelfSupersession(_) => "self-supersession",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.code())?;
        match self {
            Refusal::Malformed { event_id, detail } => write!(f, "event {event_id}: {detail}"),
            Refusal::DuplicateCandidate(id) => write!(f, "candidate {id} is already registered"),
            Refusal::NoProducers(id) => {
                write!(f, "candidate {id} names no producer; name at least one")
            }
            Refusal::UnknownParent { candidate, parent } => write!(
                f,
                "candidate {candidate} names parent {parent}, which is not registered"
            ),
            Refusal::ParentOtherContract {
                candidate,
                parent,
                parent_contract,
                contract,
            } => write!(
                f,
                "candidate {candidate} answers {contract} but parent {parent} answers \
                 {parent_contract}; content carried into another contract is adopted \
                 (--adopts), not parented"
            ),
            Refusal::UnknownAdopted { candidate, adopted } => write!(
                f,
                "candidate {candidate} adopts {adopted}, which is not registered"
            ),
            Refusal::AdoptionDropsProducer {
                candidate,
                adopted,
                dropped,
            } => write!(
                f,
                "candidate {candidate} adopts {adopted} but drops its producer(s) {}; an \
                 adoption carries every producer along the adopted parent chain",
                dropped.join(", ")
            ),
            Refusal::UnknownCandidate(id) => write!(f, "no candidate {id} is registered"),
            Refusal::SelfSupersession(id) => {
                write!(f, "candidate {id} cannot be superseded by itself")
            }
        }
    }
}

impl std::error::Error for Refusal {}

/// Every candidate fact the repository holds.
#[derive(Debug, Default)]
pub struct Ledger {
    registrations: BTreeMap<String, Registration>,
    judgements: Vec<Judgement>,
    retirements: BTreeMap<String, Retirement>,
    /// Records that keep a candidate wanted. The events that declare them
    /// are selections, promotions, and declared roots.
    roots: Vec<Root>,
}

impl Ledger {
    /// Replay candidate events in order, refusing at the first one the
    /// ledger would not have recorded. Events of other kinds are skipped.
    pub fn replay<'a>(events: impl IntoIterator<Item = &'a Event>) -> Result<Ledger, Refusal> {
        let mut ledger = Ledger::default();
        for event in events {
            ledger.record(event)?;
        }
        Ok(ledger)
    }

    /// Check one event against the ledger and apply it.
    pub fn record(&mut self, event: &Event) -> Result<(), Refusal> {
        let declarant = event
            .on_behalf_of
            .clone()
            .unwrap_or_else(|| event.actor.clone());
        match &event.payload {
            Payload::CandidateRegistered {
                candidate_id,
                tree,
                brief,
                producers,
                parents,
                adopts,
                episodes,
            } => {
                let registration = Registration {
                    candidate_id: candidate_id.clone(),
                    tree: tree.clone(),
                    brief: brief.clone(),
                    producers: producers.clone(),
                    parents: parents.clone(),
                    adopts: adopts.clone(),
                    episodes: episodes.clone(),
                    event_id: event.event_id.clone(),
                    declarant,
                    recorded_at: event.created_at,
                };
                well_formed(&registration).map_err(|detail| Refusal::Malformed {
                    event_id: event.event_id.clone(),
                    detail,
                })?;
                self.check_registration(&registration)?;
                self.registrations
                    .insert(registration.candidate_id.clone(), registration);
            }
            Payload::CandidateJudged {
                candidate_id,
                judgement,
                reason,
            } => {
                self.check_judgement(candidate_id, judgement)?;
                if reason.trim().is_empty() {
                    return Err(Refusal::Malformed {
                        event_id: event.event_id.clone(),
                        detail: "a judgement carries a reason".to_string(),
                    });
                }
                self.judgements.push(Judgement {
                    candidate_id: candidate_id.clone(),
                    judgement: judgement.clone(),
                    reason: reason.clone(),
                    event_id: event.event_id.clone(),
                    declarant,
                    recorded_at: event.created_at,
                });
            }
            Payload::CandidateRetired { candidate_id } => {
                self.known(candidate_id)?;
                // A second retirement records nothing new; the first stands.
                self.retirements
                    .entry(candidate_id.clone())
                    .or_insert(Retirement {
                        event_id: event.event_id.clone(),
                        declarant,
                        recorded_at: event.created_at,
                    });
            }
            _ => {}
        }
        Ok(())
    }

    /// The registration rules that need no local objects, in the order a
    /// refusal is reported.
    pub fn check_registration(&self, registration: &Registration) -> Result<(), Refusal> {
        let id = &registration.candidate_id;
        if self.registrations.contains_key(id) {
            return Err(Refusal::DuplicateCandidate(id.clone()));
        }
        if registration.producers.is_empty() {
            return Err(Refusal::NoProducers(id.clone()));
        }
        for parent in &registration.parents {
            let Some(found) = self.registrations.get(parent) else {
                return Err(Refusal::UnknownParent {
                    candidate: id.clone(),
                    parent: parent.clone(),
                });
            };
            if found.contract() != registration.contract() {
                return Err(Refusal::ParentOtherContract {
                    candidate: id.clone(),
                    parent: parent.clone(),
                    parent_contract: contract_label(&found.brief),
                    contract: contract_label(&registration.brief),
                });
            }
        }
        for adopted in &registration.adopts {
            let Some(found) = self.registrations.get(adopted) else {
                return Err(Refusal::UnknownAdopted {
                    candidate: id.clone(),
                    adopted: adopted.clone(),
                });
            };
            let carried: BTreeSet<&str> =
                registration.producers.iter().map(String::as_str).collect();
            let dropped: BTreeSet<String> = self
                .lineage(found)
                .iter()
                .flat_map(|ancestor| ancestor.producers.iter())
                .filter(|producer| !carried.contains(producer.as_str()))
                .cloned()
                .collect();
            if !dropped.is_empty() {
                return Err(Refusal::AdoptionDropsProducer {
                    candidate: id.clone(),
                    adopted: adopted.clone(),
                    dropped: dropped.into_iter().collect(),
                });
            }
        }
        Ok(())
    }

    pub fn check_judgement(
        &self,
        candidate_id: &str,
        judgement: &CandidateJudgement,
    ) -> Result<(), Refusal> {
        self.known(candidate_id)?;
        if let CandidateJudgement::SupersededBy {
            candidate_id: other,
        } = judgement
        {
            self.known(other)?;
            if other == candidate_id {
                return Err(Refusal::SelfSupersession(candidate_id.to_string()));
            }
        }
        Ok(())
    }

    fn known(&self, candidate_id: &str) -> Result<&Registration, Refusal> {
        self.registrations
            .get(candidate_id)
            .ok_or_else(|| Refusal::UnknownCandidate(candidate_id.to_string()))
    }

    pub fn registration(&self, candidate_id: &str) -> Option<&Registration> {
        self.registrations.get(candidate_id)
    }

    pub fn registrations(&self) -> impl Iterator<Item = &Registration> {
        self.registrations.values()
    }

    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }

    pub fn judgements_of(&self, candidate_id: &str) -> Vec<&Judgement> {
        self.judgements
            .iter()
            .filter(|judgement| judgement.candidate_id == candidate_id)
            .collect()
    }

    pub fn retirement(&self, candidate_id: &str) -> Option<&Retirement> {
        self.retirements.get(candidate_id)
    }

    /// A registration and every registration along its parent chain, each
    /// once, the registration first. An adopted registration is not on it.
    pub fn lineage<'a>(&'a self, start: &'a Registration) -> Vec<&'a Registration> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        let mut queue = VecDeque::from([start]);
        while let Some(current) = queue.pop_front() {
            if !seen.insert(current.candidate_id.as_str()) {
                continue;
            }
            out.push(current);
            queue.extend(
                current
                    .parents
                    .iter()
                    .filter_map(|parent| self.registrations.get(parent)),
            );
        }
        out
    }

    /// Every candidate whose content `from` carries: itself, its parents,
    /// and what it adopts, transitively.
    fn carried_by<'a>(&'a self, from: &'a str) -> BTreeSet<&'a str> {
        let mut reached = BTreeSet::new();
        let mut queue = VecDeque::from([from]);
        while let Some(current) = queue.pop_front() {
            if !reached.insert(current) {
                continue;
            }
            if let Some(registration) = self.registrations.get(current) {
                queue.extend(registration.parents.iter().map(String::as_str));
                queue.extend(registration.adopts.iter().map(String::as_str));
            }
        }
        reached
    }

    /// The roots that reach a candidate: every root naming it or a candidate
    /// whose parents or adoptions lead to it. A candidate no root reaches is
    /// one `retire` may unpin; arc never unpins one on its own.
    pub fn roots_reaching(&self, candidate_id: &str) -> Vec<&Root> {
        self.roots
            .iter()
            .filter(|root| self.carried_by(&root.candidate_id).contains(candidate_id))
            .collect()
    }

    /// Registrations grouped by the tree they share, for every tree more
    /// than one registration names. Sharing storage shares nothing else.
    pub fn shared_trees(&self) -> BTreeMap<&str, Vec<&str>> {
        let mut by_tree: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for registration in self.registrations.values() {
            by_tree
                .entry(registration.tree.as_str())
                .or_default()
                .push(registration.candidate_id.as_str());
        }
        by_tree.retain(|_, candidates| candidates.len() > 1);
        by_tree
    }

    /// Registrations whose pin is gone while a root still reaches them: the
    /// content a root wants is no longer held against collection.
    pub fn rooted_without_pin<'a>(
        &'a self,
        pinned: &BTreeSet<String>,
    ) -> Vec<(&'a Registration, Vec<&'a Root>)> {
        self.registrations
            .values()
            .filter(|registration| !pinned.contains(&registration.candidate_id))
            .filter_map(|registration| {
                let roots = self.roots_reaching(&registration.candidate_id);
                (!roots.is_empty()).then_some((registration, roots))
            })
            .collect()
    }
}

/// `change@brief-event`, the way a contract is named in refusals and views.
pub fn contract_label(brief: &CandidateBriefRef) -> String {
    format!("{}@{}", brief.change_id, brief.brief_event_id)
}

/// The shape every registration has whether it was typed here or arrived in
/// a bundle: identifiers usable as ref and path components, an object id for
/// the tree, a `sha256:` digest, and no blank or repeated names.
fn well_formed(registration: &Registration) -> Result<(), String> {
    ids::validate_id_component(&registration.candidate_id).map_err(|error| error.to_string())?;
    if !is_object_id(&registration.tree) {
        return Err(format!(
            "tree {:?} is not a full object id",
            registration.tree
        ));
    }
    ids::validate_id_component(&registration.brief.change_id).map_err(|error| error.to_string())?;
    ids::validate_id_component(&registration.brief.brief_event_id)
        .map_err(|error| error.to_string())?;
    let digest_ok = registration
        .brief
        .digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
    if !digest_ok {
        return Err(format!(
            "brief digest {:?} is not a sha256: digest",
            registration.brief.digest
        ));
    }
    for (label, names) in [
        ("producer", &registration.producers),
        ("episode", &registration.episodes),
    ] {
        distinct(label, names, |name| {
            if name.trim().is_empty() || name.trim() != name {
                Err(format!("{label} {name:?} is blank or padded"))
            } else {
                Ok(())
            }
        })?;
    }
    for (label, names) in [
        ("parent", &registration.parents),
        ("adopted", &registration.adopts),
    ] {
        distinct(label, names, |name| {
            ids::validate_id_component(name).map_err(|error| error.to_string())
        })?;
    }
    if registration.parents.contains(&registration.candidate_id)
        || registration.adopts.contains(&registration.candidate_id)
    {
        return Err(format!(
            "candidate {} names itself",
            registration.candidate_id
        ));
    }
    Ok(())
}

fn distinct(
    label: &str,
    names: &[String],
    valid: impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for name in names {
        valid(name)?;
        if !seen.insert(name.as_str()) {
            return Err(format!("{label} {name:?} is named twice"));
        }
    }
    Ok(())
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    fn brief(change: &str, version: &str) -> CandidateBriefRef {
        CandidateBriefRef {
            change_id: change.to_string(),
            brief_event_id: version.to_string(),
            digest: format!("sha256:{}", "0".repeat(64)),
        }
    }

    fn registered(
        id: &str,
        contract: CandidateBriefRef,
        producers: &[&str],
        parents: &[&str],
        adopts: &[&str],
    ) -> Event {
        let strings = |names: &[&str]| names.iter().map(|name| name.to_string()).collect();
        Event {
            schema_version: crate::model::SCHEMA_VERSION,
            event_id: format!("01EVENT{id}"),
            repository_id: "repo".to_string(),
            change_id: crate::store::Store::REPOSITORY_SCOPE.to_string(),
            actor: "lead".to_string(),
            actor_source: None,
            operator: None,
            on_behalf_of: None,
            model: None,
            model_provenance: Default::default(),
            harness: None,
            session: None,
            session_link: None,
            session_resolution: None,
            created_at: Utc::now(),
            payload: Payload::CandidateRegistered {
                candidate_id: id.to_string(),
                tree: TREE.to_string(),
                brief: contract,
                producers: strings(producers),
                parents: strings(parents),
                adopts: strings(adopts),
                episodes: Vec::new(),
            },
        }
    }

    #[test]
    fn an_adoption_carries_the_producers_along_the_parent_chain() {
        let mut ledger = Ledger::replay([
            &registered("a", brief("one", "v1"), &["x"], &[], &[]),
            &registered("a-fix", brief("one", "v1"), &["y"], &["a"], &[]),
        ])
        .unwrap();
        let refused = ledger
            .record(&registered(
                "b",
                brief("two", "v1"),
                &["y"],
                &[],
                &["a-fix"],
            ))
            .unwrap_err();
        assert_eq!(
            refused,
            Refusal::AdoptionDropsProducer {
                candidate: "b".to_string(),
                adopted: "a-fix".to_string(),
                dropped: vec!["x".to_string()],
            }
        );
        ledger
            .record(&registered(
                "b",
                brief("two", "v1"),
                &["x", "y"],
                &[],
                &["a-fix"],
            ))
            .unwrap();
    }

    #[test]
    fn a_parent_on_another_brief_version_is_another_contract() {
        let mut ledger =
            Ledger::replay([&registered("a", brief("one", "v1"), &["x"], &[], &[])]).unwrap();
        let refused = ledger
            .record(&registered("b", brief("one", "v2"), &["x"], &["a"], &[]))
            .unwrap_err();
        assert_eq!(refused.code(), "parent-other-contract");
    }

    #[test]
    fn a_root_reaches_what_its_candidate_carries_and_nothing_else() {
        let mut ledger = Ledger::replay([
            &registered("a", brief("one", "v1"), &["x"], &[], &[]),
            &registered("a-fix", brief("one", "v1"), &["y"], &["a"], &[]),
            &registered("b", brief("two", "v1"), &["x", "y"], &[], &["a-fix"]),
            &registered("c", brief("one", "v1"), &["z"], &[], &[]),
        ])
        .unwrap();
        assert!(ledger.roots_reaching("a").is_empty());
        ledger.roots.push(Root {
            kind: "selection",
            event_id: "01SELECTION".to_string(),
            candidate_id: "b".to_string(),
        });
        for reached in ["a", "a-fix", "b"] {
            assert_eq!(ledger.roots_reaching(reached).len(), 1, "{reached}");
        }
        assert!(ledger.roots_reaching("c").is_empty());
    }

    #[test]
    fn a_rooted_registration_without_its_pin_is_reported() {
        let mut ledger = Ledger::replay([
            &registered("a", brief("one", "v1"), &["x"], &[], &[]),
            &registered("b", brief("one", "v1"), &["x"], &[], &[]),
        ])
        .unwrap();
        let pinned = BTreeSet::from(["a".to_string()]);
        // Unrooted content with no pin is nobody's loss.
        assert!(ledger.rooted_without_pin(&pinned).is_empty());
        ledger.roots.push(Root {
            kind: "declared root",
            event_id: "01ROOT".to_string(),
            candidate_id: "b".to_string(),
        });
        let missing = ledger.rooted_without_pin(&pinned);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0.candidate_id, "b");
        assert_eq!(missing[0].1[0].event_id, "01ROOT");
    }

    #[test]
    fn a_registration_naming_a_blank_producer_is_malformed() {
        let refused =
            Ledger::replay([&registered("a", brief("one", "v1"), &[" "], &[], &[])]).unwrap_err();
        assert_eq!(refused.code(), "malformed-candidate-event");
    }
}
