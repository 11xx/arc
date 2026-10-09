//! Keeping a commit the ledger cites reachable once a rewrite replaces it.
//!
//! A rewrite moves the refs that pin a change's commits onto their
//! successors, and the branch with them. The commit a verdict read is then on
//! nothing, and the next garbage collection takes it, while the ledger goes on
//! saying it was read. The successor stands in for it everywhere a projection
//! follows a recorded revision forward; this is what keeps the object itself.
//!
//! Which commits count is decided by the ledger, not by which ref happens to
//! hold one: a patchset's head, and the revision an external or audit verdict
//! was recorded on. A pin released at integration, or one an outside tool
//! moved, leaves those commits as exposed as a pin the rewrite moves.

use crate::gitio::{self, RefUpdate};
use crate::model::Payload;
use crate::rewrite::RefMove;
use crate::store::Store;
use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A commit the ledger cites as read, and what cites it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cited {
    pub change_id: String,
    /// The patchset the commit was the head of, or the event that recorded a
    /// verdict on it.
    pub subject: String,
    /// How a reader is told what cites it.
    pub what: String,
    pub commit: String,
}

impl Cited {
    pub fn retained_ref(&self) -> String {
        gitio::retained_ref(&self.change_id, &self.subject, &self.commit)
    }
}

/// Every commit the ledger cites as read, as the events recorded it. A rewrite
/// map is not followed: the commit a verdict read is the one it names.
///
/// A commit appears once per change. A patchset head is named by its
/// patchset, which is also what a verdict on that patchset read; only a
/// verdict recorded on some other commit is named by its own event.
pub fn cited(store: &Store) -> Result<Vec<Cited>> {
    let mut found = Vec::new();
    for change_id in store.list_change_ids()? {
        let events = store.load_events(&change_id)?;
        let mut seen = BTreeSet::new();
        let mut add = |subject: &str, what: String, commit: &str| {
            if seen.insert(commit.to_string()) {
                found.push(Cited {
                    change_id: change_id.clone(),
                    subject: subject.to_string(),
                    what,
                    commit: commit.to_string(),
                });
            }
        };
        for event in &events {
            if let Payload::PatchsetAdded {
                patchset_id, head, ..
            } = &event.payload
            {
                add(patchset_id, format!("{patchset_id} head"), head);
            }
        }
        for event in &events {
            match &event.payload {
                Payload::ExternalVerdictRecorded { revision, .. } => {
                    add(&event.event_id, "external verdict".to_string(), revision)
                }
                Payload::AuditVerdictRecorded { revision, .. } => {
                    add(&event.event_id, "audit".to_string(), revision)
                }
                _ => {}
            }
        }
    }
    Ok(found)
}

/// What a rewrite is to hold, and what it cannot.
#[derive(Debug, Default)]
pub struct Plan {
    /// Every ref that keeps a replaced commit reachable, with the commit.
    pub retain: BTreeMap<String, String>,
    /// Cited commits the rewrite replaces that this repository does not
    /// hold, so nothing can keep them.
    pub gone: Vec<Cited>,
}

/// What a rewrite that replaces the commits of `mapping` and moves `moved`
/// retains.
///
/// Two sources agree on one name where they meet. A moved pin names the
/// commit it held, whether or not the ledger cites it. The ledger names every
/// cited commit the map replaces, whether or not a pin moved with it.
pub fn plan(
    cwd: &Path,
    store: &Store,
    mapping: &BTreeMap<String, Option<String>>,
    moved: &BTreeMap<String, RefMove>,
) -> Result<Plan> {
    let mut plan = Plan::default();
    let mut held: BTreeSet<(String, String)> = BTreeSet::new();
    for (name, moved) in moved {
        let Some(rest) = name.strip_prefix(gitio::KEEP_PREFIX) else {
            continue;
        };
        let Some((change_id, patchset_id)) = rest.split_once('/') else {
            continue;
        };
        plan.retain.insert(
            gitio::retained_ref(change_id, patchset_id, &moved.old),
            moved.old.clone(),
        );
        held.insert((change_id.to_string(), moved.old.clone()));
    }
    let replaced = replaced_by(mapping);
    let candidates: Vec<Cited> = cited(store)?
        .into_iter()
        .filter(|cited| replaced(&cited.commit))
        .filter(|cited| !held.contains(&(cited.change_id.clone(), cited.commit.clone())))
        .collect();
    let present: BTreeSet<String> =
        gitio::commits_among(cwd, candidates.iter().map(|cited| cited.commit.as_str()))?
            .into_iter()
            .collect();
    for cited in candidates {
        if present.contains(&cited.commit) {
            plan.retain.insert(cited.retained_ref(), cited.commit);
        } else {
            plan.gone.push(cited);
        }
    }
    Ok(plan)
}

/// Whether a map replaces a commit. A map may spell what the ledger holds in
/// full as an abbreviation, which names the commit only when it is long
/// enough to.
fn replaced_by(mapping: &BTreeMap<String, Option<String>>) -> impl Fn(&str) -> bool + '_ {
    let abbreviated: Vec<&str> = mapping
        .keys()
        .map(String::as_str)
        .filter(|key| key.len() < 40)
        .collect();
    move |commit| {
        mapping.contains_key(commit)
            || abbreviated.iter().any(|key| {
                key.len() >= crate::rewrite::ABBREVIATION_FLOOR && commit.starts_with(key)
            })
    }
}

/// The ref creations that bring the repository to holding `retained`.
///
/// A ref already holding its commit is left alone, which is what makes
/// finishing an interrupted rewrite, and recording a rewrite whose commits an
/// earlier one retained, repeatable. A commit that is no longer here cannot be
/// held and is returned beside the creations rather than failing a rewrite
/// that has already moved its refs.
pub fn creations(
    cwd: &Path,
    retained: &BTreeMap<String, String>,
) -> Result<(Vec<RefUpdate>, Vec<String>)> {
    let mut creations = Vec::new();
    let mut absent = Vec::new();
    let present: BTreeSet<String> =
        gitio::commits_among(cwd, retained.values().map(String::as_str))?
            .into_iter()
            .collect();
    for (name, commit) in retained {
        match gitio::ref_value(cwd, name)?.as_deref() {
            Some(value) if value == commit => {}
            Some(value) => bail!(
                "{name} holds {value}, and a retention ref holds only the commit it is named for; \
                 settle it by hand"
            ),
            None if present.contains(commit) => creations.push(RefUpdate {
                name: name.clone(),
                old: gitio::absent_oid(commit),
                new: commit.clone(),
            }),
            None => absent.push(name.clone()),
        }
    }
    Ok((creations, absent))
}

/// Undo creations a failed attempt made.
pub fn withdrawals(creations: &[RefUpdate]) -> Vec<RefUpdate> {
    creations
        .iter()
        .map(|created| RefUpdate {
            name: created.name.clone(),
            old: created.new.clone(),
            new: created.old.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(keys: &[&str]) -> BTreeMap<String, Option<String>> {
        keys.iter()
            .map(|key| (key.to_string(), Some("b".repeat(40))))
            .collect()
    }

    #[test]
    fn a_map_replaces_a_commit_it_names_in_full_or_abbreviates_long_enough() {
        let commit = "a".repeat(40);
        let full = map(&[&commit]);
        assert!(replaced_by(&full)(&commit));
        assert!(!replaced_by(&full)(&"c".repeat(40)));

        let long = map(&[&commit[..8]]);
        assert!(replaced_by(&long)(&commit));
        let short = map(&[&commit[..6]]);
        assert!(!replaced_by(&short)(&commit));
    }

    #[test]
    fn a_retention_ref_names_the_commit_it_holds() {
        let cited = Cited {
            change_id: "change".into(),
            subject: "ps-01".into(),
            what: "ps-01 head".into(),
            commit: "a".repeat(40),
        };
        assert_eq!(
            cited.retained_ref(),
            format!("refs/arc/retained/change/ps-01/{}", "a".repeat(40))
        );
    }
}
