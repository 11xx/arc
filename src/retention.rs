//! Keeping a commit the ledger cites reachable once a rewrite replaces it.
//!
//! A rewrite moves the refs that pin a change's commits onto their
//! successors, and the branch with them. The commit a verdict read is then on
//! nothing, and the next garbage collection takes it, while the ledger goes on
//! saying it was read. The successor stands in for it everywhere a projection
//! follows a recorded revision forward; this is what keeps the object itself.
//!
//! Which commits count is decided by the ledger, not by which ref happens to
//! hold one: a patchset's head, the revision an external or audit verdict
//! was recorded on, a commit a forge fact records as the provider's own,
//! the source, tested and report commits received with a candidate, Git
//! support selected by a disclosure, and the revision and change heads a
//! checkpoint recorded.
//! A pin released at integration, or one an outside tool moved, leaves those
//! commits as exposed as a pin the rewrite moves.

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
    /// verdict or source fact on it.
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
/// verdict recorded on some other commit is named by its own event. A
/// provider-observed commit is named by the forge event that recorded it, and
/// only a full object ID can be held: a ref is named for the commit it holds.
/// A checkpoint's revision and the change heads it recorded are named by the
/// checkpoint's event, in repository scope, unless something else already
/// cites the commit.
pub fn cited(store: &Store) -> Result<Vec<Cited>> {
    let mut found = Vec::new();
    let read = store.read()?;
    for change_id in read.change_ids()? {
        let events = read.events(&change_id)?;
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
            if let Payload::GateBudgetRaised { raise } = &event.payload {
                add(&event.event_id, "budget raise head".into(), &raise.head);
                add(
                    &event.event_id,
                    "budget raise destination".into(),
                    &raise.target_revision,
                );
            }
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
        for event in &events {
            for commit in crate::forge_round_trip::disclosure_commits(&event.payload) {
                add(&event.event_id, "disclosure Git support".into(), commit);
            }
            for (what, commit) in crate::forge_observation::observed_commits(&event.payload)
                .into_iter()
                .flatten()
                .filter(|(_, commit)| matches!(commit.len(), 40 | 64))
            {
                add(&event.event_id, what.to_string(), commit);
            }
        }
    }
    let repository_events = store.load_repository_events()?;
    let registrations: BTreeMap<_, _> = repository_events
        .iter()
        .filter_map(|event| {
            if let Payload::CandidateRegistered {
                candidate_id,
                brief,
                ..
            } = &event.payload
            {
                Some((candidate_id.as_str(), brief.change_id.as_str()))
            } else {
                None
            }
        })
        .collect();
    for event in &repository_events {
        for commit in crate::forge_round_trip::disclosure_commits(&event.payload) {
            found.push(Cited {
                change_id: Store::REPOSITORY_SCOPE.into(),
                subject: event.event_id.clone(),
                what: "disclosure Git support".into(),
                commit: commit.to_string(),
            });
        }
        if let Payload::CandidateEvidenceReceived {
            candidate_id,
            evidence,
        } = &event.payload
        {
            if let Some(change_id) = registrations.get(candidate_id.as_str()) {
                for (role, head) in evidence.heads.each() {
                    found.push(Cited {
                        change_id: change_id.to_string(),
                        subject: candidate_id.clone(),
                        what: format!("received {} head", role.as_str()),
                        commit: head.commit.clone(),
                    });
                }
            }
        }
    }
    let mut cited_commits: BTreeSet<String> =
        found.iter().map(|cited| cited.commit.clone()).collect();
    for event in &repository_events {
        for (what, commit) in checkpoint_commits(&event.payload) {
            if crate::bundle::valid_object_id(commit) && cited_commits.insert(commit.to_string()) {
                found.push(Cited {
                    change_id: Store::REPOSITORY_SCOPE.into(),
                    subject: event.event_id.clone(),
                    what,
                    commit: commit.to_string(),
                });
            }
        }
    }
    Ok(found)
}

/// The revision a checkpoint names and the head it recorded for each open
/// change, each with how a reader is told what cites it.
pub(crate) fn checkpoint_commits(payload: &Payload) -> Vec<(String, &str)> {
    let Payload::CheckpointRecorded {
        name,
        revision,
        changes,
        ..
    } = payload
    else {
        return Vec::new();
    };
    let heads = changes.iter().filter_map(|change| {
        let head = change.head.as_deref()?;
        Some((
            format!("checkpoint {name} head of {}", change.change_id),
            head,
        ))
    });
    std::iter::once((format!("checkpoint {name} revision"), revision.as_str()))
        .chain(heads)
        .collect()
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

/// What a rebase that replays `onto..head` of `change_id` retains: each
/// commit in that range the ledger cites that none of the change's keep pins
/// reaches.
///
/// A rebase moves no pin, and a pin holds its patchset's head with every
/// commit below it, so what the replay leaves exposed is a cited commit above
/// every recorded head. Holding it before the replay covers every way the
/// replay can end, a rebase stopped on a conflict and finished by hand
/// included.
pub fn replayed(
    cwd: &Path,
    store: &Store,
    change_id: &str,
    onto: &str,
    head: &str,
) -> Result<BTreeMap<String, String>> {
    let mut args = vec!["rev-list".to_string(), head.to_string(), format!("^{onto}")];
    for (_, pin) in gitio::list_refs(cwd, &gitio::retention_prefix(change_id))? {
        args.push(format!("^{pin}"));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let exposed: BTreeSet<String> = gitio::git(cwd, &args)?
        .lines()
        .map(str::to_string)
        .collect();
    if exposed.is_empty() {
        return Ok(BTreeMap::new());
    }
    Ok(cited(store)?
        .into_iter()
        .map(|cited| Cited {
            commit: crate::rewrite::canonical_revision(&cited.commit),
            ..cited
        })
        .filter(|cited| exposed.contains(&cited.commit))
        .map(|cited| (cited.retained_ref(), cited.commit))
        .collect())
}

/// What an import holds once its events are published: each commit the
/// ledger cites that the bundle `delivered`, that this repository holds, and
/// that no ref reaches, under the name [`Cited::retained_ref`] gives it.
///
/// A commit is held once, whatever else cites it. A name that already stands
/// is left as it is, and a commit the bundle did not list is never held,
/// whatever is here.
pub fn delivered(
    cwd: &Path,
    store: &Store,
    delivered: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>> {
    if delivered.is_empty() {
        return Ok(BTreeMap::new());
    }
    let candidates: Vec<Cited> = cited(store)?
        .into_iter()
        .map(|cited| Cited {
            commit: crate::rewrite::canonical_revision(&cited.commit),
            ..cited
        })
        .filter(|cited| delivered.contains(&cited.commit))
        .collect();
    let present = gitio::commits_among(cwd, candidates.iter().map(|cited| cited.commit.as_str()))?;
    let present: Vec<&str> = present.iter().map(String::as_str).collect();
    let unreachable = gitio::unreachable_from_refs(cwd, &present)?;
    let mut held = BTreeSet::new();
    let mut retain = BTreeMap::new();
    for cited in candidates {
        if !unreachable.contains(&cited.commit) || held.contains(&cited.commit) {
            continue;
        }
        let name = cited.retained_ref();
        if gitio::ref_value(cwd, &name)?.is_none() {
            held.insert(cited.commit.clone());
            retain.insert(name, cited.commit);
        }
    }
    Ok(retain)
}

/// Whether a map replaces a commit. A map may spell what the ledger holds in
/// full as an abbreviation, which names the commit only when it is long
/// enough to.
fn replaced_by(mapping: &BTreeMap<String, Option<String>>) -> impl Fn(&str) -> bool {
    let keys: BTreeSet<String> = mapping
        .keys()
        .map(|key| crate::rewrite::canonical_revision(key))
        .collect();
    let abbreviated: Vec<String> = keys
        .iter()
        .filter(|key| key.len() >= crate::rewrite::ABBREVIATION_FLOOR)
        .cloned()
        .collect();
    move |commit| {
        let commit = crate::rewrite::canonical_revision(commit);
        keys.contains(&commit)
            || abbreviated
                .iter()
                .any(|key| key.len() < commit.len() && commit.starts_with(key.as_str()))
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
    fn sha256_abbreviations_match_the_cited_commit_width() {
        let commit = "a".repeat(64);
        for width in [7, 8, 39, 40, 48, 63, 64] {
            assert!(
                replaced_by(&map(&[&commit[..width]]))(&commit),
                "width {width}"
            );
        }
        assert!(!replaced_by(&map(&[&commit[..6]]))(&commit));
        assert!(!replaced_by(&map(&[&commit[..48]]))(&"a".repeat(40)));
    }

    #[test]
    fn a_map_replaces_a_commit_whatever_case_it_spells_it_in() {
        let commit = "ab".repeat(20);
        let upper = commit.to_ascii_uppercase();
        assert!(replaced_by(&map(&[&upper]))(&commit));
        assert!(replaced_by(&map(&[&upper[..8]]))(&commit));
        assert!(replaced_by(&map(&[&commit]))(&upper));
        assert!(replaced_by(&map(&[&commit[..8]]))(&upper));
        assert!(!replaced_by(&map(&[&upper[..6]]))(&commit));
        let sha256 = "ab".repeat(32);
        assert!(replaced_by(&map(&[&sha256.to_ascii_uppercase()[..40]]))(
            &sha256
        ));
    }

    fn checkpoint(name: &str, revision: &str, heads: &[&str]) -> crate::store::LocalEvent {
        let changes: Vec<_> = heads
            .iter()
            .enumerate()
            .map(|(index, head)| {
                serde_json::json!({
                    "change_id": format!("change-{index}"), "slug": "slug",
                    "state": "open", "patchset_id": "ps-01", "head": head,
                })
            })
            .collect();
        crate::store::LocalEvent::draft(
            serde_json::from_value(serde_json::json!({
                "schema_version": 1, "event_id": "", "repository_id": "repo",
                "change_id": Store::REPOSITORY_SCOPE, "actor": "tester",
                "created_at": chrono::Utc::now(), "event_type": "checkpoint-recorded",
                "name": name, "revision": revision, "tree": "tree",
                "changes": changes, "todos": [],
            }))
            .unwrap(),
        )
    }

    /// Claim: a checkpoint cites its revision and every change head it
    /// recorded, in repository scope under its own event, and a commit
    /// already cited is cited once. Rejects: a checkpoint's change heads left
    /// out, a second checkpoint at the same revision asking for a second ref,
    /// and a recorded value that is not an object ID becoming a ref name.
    #[test]
    fn a_checkpoint_cites_its_revision_and_the_heads_it_recorded() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = Store::for_tests(tmp.path());
        let (revision, head) = ("a".repeat(40), "b".repeat(64));
        let mut first = checkpoint("beta", &revision, &[&head, "not-a-commit"]);
        store.append_local_repository_event(&mut first).unwrap();
        let mut second = checkpoint("beta-2", &revision, &[&revision]);
        store.append_local_repository_event(&mut second).unwrap();

        let cited: Vec<_> = cited(&store)
            .unwrap()
            .into_iter()
            .map(|cited| (cited.change_id, cited.subject, cited.what, cited.commit))
            .collect();
        let scope = Store::REPOSITORY_SCOPE.to_string();
        assert_eq!(
            cited,
            vec![
                (
                    scope.clone(),
                    first.event_id.clone(),
                    "checkpoint beta revision".to_string(),
                    revision
                ),
                (
                    scope,
                    first.event_id.clone(),
                    "checkpoint beta head of change-0".to_string(),
                    head
                ),
            ]
        );
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
