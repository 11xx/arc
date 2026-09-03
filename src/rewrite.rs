//! Recorded Git history rewrites, and following a recorded revision forward.
//!
//! Git history is occasionally rewritten for reasons unrelated to any change:
//! signing an old commit, purging a secret, correcting an author, an upstream
//! force-push. Every revision the ledger recorded then names an object that no
//! longer exists.
//!
//! The ledger is append-only, so migrating those events is not available — and
//! should not be. A ledger that rewrites itself when Git moves is a projection
//! of whatever Git currently says, which is the thing arc exists to supplement
//! rather than mirror. So the rewrite is recorded as the fact it is, and
//! readers follow revisions forward through it. Nothing already written
//! changes.

use crate::model::Payload;
use crate::store::Store;
use anyhow::Result;
use std::collections::BTreeMap;

/// What became of a recorded revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fate {
    /// It survives at this revision, after following every recorded rewrite.
    Rewritten(String),
    /// A rewrite dropped it. Nothing survives it, and saying so is the point:
    /// "unchanged" and "gone" are different answers.
    Dropped,
}

fn describe(fate: &Option<String>) -> String {
    match fate {
        Some(revision) => revision.clone(),
        None => "dropped".to_string(),
    }
}

/// One resolution step. Ambiguity is not a variant: an abbreviation matching
/// two recorded revisions is a question the map cannot answer, and answering
/// "nothing rewrote it" would be a wrong answer rather than no answer.
enum Step {
    Unmapped,
    Mapped { key: String, fate: Option<String> },
}

/// The shortest abbreviation that may stand for a revision.
///
/// Seven hex digits is what Git itself considers abbreviated, and short of it
/// a prefix stops naming a commit: six digits collide across sixteen million
/// objects, and one names a sixteenth of every object in the repository.
const ABBREVIATION_FLOOR: usize = 7;

/// One ref move a rewrite has planned: the value the ref held when the
/// rewrite read it, and the value it is to hold instead.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RefMove {
    pub old: String,
    pub new: String,
}

/// A rewrite's whole result, written down before any of it is applied.
///
/// A rewrite recreates commits, then moves refs onto them, then records the
/// map. Each of those is durable and the sequence is not, so an interruption
/// between any two leaves a repository that describes two histories at once.
/// The intent is what makes the sequence finishable: the commits already
/// exist, the refs each name one of two known values, and the map is here to
/// be recorded — so the only thing left to decide is which steps remain.
///
/// Without it, the second run has to recreate the commits, which signs them
/// afresh and yields different ids: a second successor for every commit the
/// first run already moved a ref onto, and a recorded map that contradicts
/// itself.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RewriteIntent {
    pub schema_version: u32,
    /// The branch whose history this rewrites.
    pub branch: String,
    /// The oldest commit the rewrite recreated.
    pub from: String,
    /// The reason and tool the mapping event is to carry.
    pub reason: String,
    pub tool: Option<String>,
    /// The map exactly as the event will record it.
    pub mapping: BTreeMap<String, Option<String>>,
    /// Every ref the rewrite moves, the rewritten branch included.
    pub refs: BTreeMap<String, RefMove>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Every recorded rewrite, flattened into one old-to-fate mapping.
#[derive(Debug, Default, Clone)]
pub struct RewriteMap {
    steps: BTreeMap<String, Option<String>>,
}

impl RewriteMap {
    pub fn load(store: &Store) -> Result<Self> {
        Self::from_events(store.load_repository_events()?.iter())
    }

    /// The same map, built from a given sequence of events. Import uses it to
    /// judge the combination of what a bundle carries and what is already
    /// here — two events with different IDs can still disagree about one
    /// revision, which no per-event check can see.
    pub fn from_events<'a>(events: impl Iterator<Item = &'a crate::model::Event>) -> Result<Self> {
        let mut steps: BTreeMap<String, Option<String>> = BTreeMap::new();
        for event in events {
            if let Payload::HistoryRewritten { mapping, .. } = &event.payload {
                // Both write paths refuse a mapping that cannot mean what it
                // says, so a valid ledger holds none. One that arrived another
                // way is skipped rather than made fatal: a map nobody can read
                // is worse than a map missing an entry, and `arc doctor`
                // reports it as `invalid-rewrite-mapping`.
                if validate_mapping(mapping).is_err() {
                    continue;
                }
                for (old, new) in mapping {
                    // Two rewrites may each move a revision — that is a chain,
                    // and following it is the point. But two *different*
                    // successors for the same revision is a contradiction, and
                    // silently keeping the last one would answer a question the
                    // ledger cannot answer.
                    if let Some(existing) = steps.get(old) {
                        if existing != new {
                            anyhow::bail!(
                                "recorded rewrites disagree about {old}: {} and {}",
                                describe(existing),
                                describe(new)
                            );
                        }
                    }
                    steps.insert(old.clone(), new.clone());
                }
            }
        }
        Ok(Self { steps })
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// What became of a recorded revision, following successive rewrites.
    /// `Ok(None)` means no recorded rewrite touched it, so a caller can tell
    /// "unchanged" from "moved" and from "dropped".
    ///
    /// A query that names a recorded revision exactly resolves to it. Failing
    /// that it may abbreviate one, or be abbreviated by one — a map may
    /// abbreviate what the ledger stores in full, and a caller may abbreviate
    /// what the map stores in full — and then it resolves only when exactly
    /// one recorded revision answers. An abbreviation that two answer is an
    /// error naming them: the map holds the answer and cannot say which, and
    /// reporting the revision as untouched would be a different claim.
    pub fn fate(&self, revision: &str) -> Result<Option<Fate>> {
        let mut visited: Vec<String> = Vec::new();
        let mut current = revision.to_string();
        loop {
            match self.step(&current)? {
                // Nothing rewrote this one. Either the caller asked about a
                // revision no rewrite touched, or we followed the chain to
                // its end and this is where it survives.
                Step::Unmapped => {
                    return Ok((!visited.is_empty()).then_some(Fate::Rewritten(current)))
                }
                Step::Mapped { key, fate } => {
                    // Only a hand-written map can cycle. Visiting a key twice
                    // is what a cycle is, whatever spelling led back to it.
                    if visited.contains(&key) {
                        anyhow::bail!(
                            "recorded rewrites lead {revision} back to {key}; the chain names no \
                             surviving commit"
                        );
                    }
                    visited.push(key);
                    match fate {
                        None => return Ok(Some(Fate::Dropped)),
                        Some(next) => current = next,
                    }
                }
            }
        }
    }

    /// The revision a recorded one names in this repository: itself when no
    /// recorded rewrite touched it, and its successor once every recorded
    /// rewrite has been followed.
    ///
    /// A revision a rewrite dropped answers itself. Nothing survives it, and
    /// answering with the last readable link would name a commit the record
    /// does not claim.
    pub fn current(&self, revision: &str) -> Result<String> {
        Ok(match self.fate(revision)? {
            Some(Fate::Rewritten(successor)) => successor,
            Some(Fate::Dropped) | None => revision.to_string(),
        })
    }

    /// Follow a recorded revision forward in place.
    pub fn advance(&self, revision: &mut String) -> Result<()> {
        if self.steps.is_empty() {
            return Ok(());
        }
        *revision = self.current(revision)?;
        Ok(())
    }

    /// Follow an optional recorded revision forward in place.
    pub fn advance_opt(&self, revision: &mut Option<String>) -> Result<()> {
        match revision.as_mut() {
            Some(revision) => self.advance(revision),
            None => Ok(()),
        }
    }

    /// Whether two revisions name one commit once both are followed forward.
    ///
    /// Equal resolved ids are one commit. Failing that either may abbreviate
    /// the other — a recorded map and a Git read of the same commit need not
    /// agree on length, and a comparison that demanded they did would answer
    /// "different commit" for one commit — and an abbreviation stands for a
    /// commit only from the length at which it names one.
    pub fn same(&self, left: &str, right: &str) -> Result<bool> {
        let (left, right) = (self.current(left)?, self.current(right)?);
        if left == right {
            return Ok(!left.is_empty());
        }
        let abbreviation = left.len().min(right.len());
        Ok(abbreviation >= ABBREVIATION_FLOOR
            && (left.starts_with(&right) || right.starts_with(&left)))
    }

    /// One hop: the recorded revision `revision` names, exactly where it names
    /// one and by abbreviation in either direction otherwise.
    fn step(&self, revision: &str) -> Result<Step> {
        if let Some(fate) = self.steps.get(revision) {
            return Ok(Step::Mapped {
                key: revision.to_string(),
                fate: fate.clone(),
            });
        }
        // Short of the floor, or spelled with anything but hex, a query is not
        // an abbreviation of a commit and matching it against one would be a
        // coincidence rather than a resolution.
        if revision.len() < ABBREVIATION_FLOOR
            || !revision.chars().all(|byte| byte.is_ascii_hexdigit())
        {
            return Ok(Step::Unmapped);
        }
        let candidates: Vec<&String> = self
            .steps
            .keys()
            .filter(|old| old.starts_with(revision) || revision.starts_with(old.as_str()))
            .collect();
        match candidates.as_slice() {
            [] => Ok(Step::Unmapped),
            [old] => Ok(Step::Mapped {
                key: (*old).clone(),
                fate: self.steps[*old].clone(),
            }),
            many => anyhow::bail!(
                "{revision} abbreviates {} recorded revisions ({}); it names none of them",
                many.len(),
                many.iter()
                    .map(|old| old.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// Parse a commit map: `<old> <new>` per line, which is what `git filter-repo`
/// writes — including its `old`/`new` header line, which names the columns
/// rather than a commit.
///
/// A line whose new revision is all zeroes records a commit the rewrite
/// dropped; it survives at nothing, and following it forward must say so
/// rather than reporting it unchanged. An identity line records a commit the
/// rewrite left alone, which is not a move and is not recorded.
pub fn parse_commit_map(text: &str) -> Result<BTreeMap<String, Option<String>>> {
    let mut mapping: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(old), Some(new)) = (parts.next(), parts.next()) else {
            anyhow::bail!("line {}: expected `<old> <new>`, got {line:?}", number + 1);
        };
        if parts.next().is_some() {
            anyhow::bail!("line {}: expected two revisions, got {line:?}", number + 1);
        }
        // git filter-repo writes a column header before the first mapping.
        if old.eq_ignore_ascii_case("old") && new.eq_ignore_ascii_case("new") {
            continue;
        }
        if !is_revision(old) {
            anyhow::bail!("line {}: {old:?} is not a revision", number + 1);
        }
        let dropped = new.chars().all(|c| c == '0');
        if !dropped && !is_revision(new) {
            anyhow::bail!("line {}: {new:?} is not a revision", number + 1);
        }
        if !dropped && new == old {
            continue;
        }
        let fate = (!dropped).then(|| new.to_string());
        if let Some(existing) = mapping.get(old) {
            if existing != &fate {
                anyhow::bail!(
                    "line {}: {old} is mapped twice, to different revisions",
                    number + 1
                );
            }
            continue;
        }
        mapping.insert(old.to_string(), fate);
    }
    if mapping.is_empty() {
        anyhow::bail!("the commit map records no rewritten revision");
    }
    Ok(mapping)
}

/// Whether a token looks like a Git object name: hex, and long enough to
/// abbreviate one. Matching is by prefix elsewhere, so the length floor is
/// what keeps `a` from matching half the repository.
/// Whether a recorded mapping means what a recorded mapping can mean.
///
/// `parse_commit_map` enforces this on the way in, but a bundle carries the
/// payload directly: without the same check, an imported map could say a
/// revision was rewritten to the zero object — which the recording path spells
/// as "dropped" — and every reader would report a rewrite to nothing.
pub fn validate_mapping(mapping: &BTreeMap<String, Option<String>>) -> Result<()> {
    for (old, new) in mapping {
        if !is_revision(old) {
            anyhow::bail!("{old:?} is not a revision");
        }
        match new {
            None => {}
            Some(new) if new.chars().all(|c| c == '0') => anyhow::bail!(
                "{old} maps to the zero object; a dropped commit is recorded as no successor"
            ),
            Some(new) if !is_revision(new) => anyhow::bail!("{new:?} is not a revision"),
            Some(new) if new == old => {
                anyhow::bail!("{old} maps to itself, which is not a rewrite")
            }
            Some(_) => {}
        }
    }
    if mapping.is_empty() {
        anyhow::bail!("the rewrite records no revision");
    }
    Ok(())
}

fn is_revision(token: &str) -> bool {
    token.len() >= 7
        && token.len() <= 64
        && token.chars().all(|c| c.is_ascii_hexdigit())
        // The zero object is how Git spells "no object". A rewrite from it
        // would claim a successor for a commit that never existed.
        && !token.chars().all(|c| c == '0')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, Option<&str>)]) -> RewriteMap {
        RewriteMap {
            steps: pairs
                .iter()
                .map(|(old, new)| (old.to_string(), new.map(str::to_string)))
                .collect(),
        }
    }

    #[test]
    fn a_revision_rewritten_twice_follows_the_chain() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("bbbbbbbbbb", Some("cccccccccc")),
        ]);
        assert_eq!(
            map.fate("aaaaaaaaaa").unwrap(),
            Some(Fate::Rewritten("cccccccccc".into()))
        );
        assert_eq!(map.fate("cccccccccc").unwrap(), None);
    }

    /// A map may abbreviate what the ledger records in full, and a caller may
    /// abbreviate what the map records in full. Byte equality would silently
    /// fail to follow either.
    #[test]
    fn revisions_match_by_abbreviation_in_both_directions() {
        let full = map(&[(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )]);
        assert!(full.fate("aaaaaaaaaa").unwrap().is_some());
        let short = map(&[("aaaaaaaaaa", Some("bbbbbbbbbb"))]);
        assert!(short
            .fate("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap()
            .is_some());
    }

    /// A recorded revision that a longer one abbreviates is still the one the
    /// query names exactly. Preferring the abbreviation match would make the
    /// pair ambiguous and refuse a revision the map answers for.
    #[test]
    fn an_exact_match_wins_over_an_abbreviation_of_it() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("aaaaaaaaaaaa", Some("cccccccccc")),
        ]);
        assert_eq!(
            map.fate("aaaaaaaaaa").unwrap(),
            Some(Fate::Rewritten("bbbbbbbbbb".into()))
        );
        assert_eq!(
            map.fate("aaaaaaaaaaaa").unwrap(),
            Some(Fate::Rewritten("cccccccccc".into()))
        );
    }

    /// An abbreviation two recorded revisions answer names neither, and the
    /// refusal has to name them: the map holds the answer and cannot say
    /// which, which is not the same as no rewrite having touched it.
    #[test]
    fn an_ambiguous_abbreviation_is_refused_naming_the_candidates() {
        let map = map(&[
            ("aaaaaaaa11", Some("cccccccccc")),
            ("aaaaaaaa22", Some("dddddddddd")),
        ]);
        let refusal = map.fate("aaaaaaaa").unwrap_err().to_string();
        assert!(refusal.contains("aaaaaaaa11"), "{refusal}");
        assert!(refusal.contains("aaaaaaaa22"), "{refusal}");
    }

    /// Short of seven hex digits a prefix names a sixteenth of the repository
    /// per missing digit, so it stands for no commit at all.
    #[test]
    fn an_abbreviation_below_the_floor_names_nothing() {
        let map = map(&[("aaaaaaaaaa", Some("bbbbbbbbbb"))]);
        assert_eq!(map.fate("aaaaaa").unwrap(), None);
        assert_eq!(
            map.fate("aaaaaaa").unwrap(),
            Some(Fate::Rewritten("bbbbbbbbbb".into()))
        );
        assert!(!map.same("aaaaaa", "aaaaaaaaaa").unwrap());
        assert!(map.same("aaaaaaa", "bbbbbbbbbb").unwrap());
    }

    #[test]
    fn a_dropped_revision_is_not_a_survivor() {
        let map = map(&[("aaaaaaaaaa", None)]);
        assert_eq!(map.fate("aaaaaaaaaa").unwrap(), Some(Fate::Dropped));
    }

    /// Ambiguity anywhere along a chain makes the answer unavailable, not the
    /// ambiguous node a survivor.
    #[test]
    fn an_ambiguous_chain_is_refused() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("bbbbbbbbbb11", Some("cccccccccc")),
            ("bbbbbbbbbb22", Some("dddddddddd")),
        ]);
        assert!(map.fate("aaaaaaaaaa").is_err());
    }

    /// A cycle is visiting a key twice, whatever spelling led back to it.
    #[test]
    fn a_chain_that_returns_to_a_visited_key_is_refused() {
        let map = map(&[("aaaaaaaaaa", Some("aaaaaaaaaaaa"))]);
        assert!(map.fate("aaaaaaaaaa").is_err());
    }

    #[test]
    fn a_cyclic_mapping_is_refused_rather_than_answered_with_a_bogus_survivor() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("bbbbbbbbbb", Some("aaaaaaaaaa")),
        ]);
        assert!(map.fate("aaaaaaaaaa").is_err());
    }

    /// The real `git filter-repo` map opens with a column header, records
    /// dropped commits as all zeroes, and leaves untouched commits mapped to
    /// themselves.
    #[test]
    fn the_filter_repo_format_parses() {
        let parsed = parse_commit_map(
            "old                                      new\n\
             aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
             cccccccccccccccccccccccccccccccccccccccc 0000000000000000000000000000000000000000\n\
             dddddddddddddddddddddddddddddddddddddddd dddddddddddddddddddddddddddddddddddddddd\n",
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed
                .get("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap()
                .as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        assert_eq!(
            parsed.get("cccccccccccccccccccccccccccccccccccccccc"),
            Some(&None),
            "a dropped commit is recorded as dropped, not discarded"
        );
        assert!(
            !parsed.contains_key("dddddddddddddddddddddddddddddddddddddddd"),
            "an untouched commit is not a rewrite"
        );
    }

    #[test]
    fn a_conflicting_duplicate_is_refused() {
        assert!(parse_commit_map("aaaaaaaaaa bbbbbbbbbb\naaaaaaaaaa cccccccccc\n").is_err());
        assert!(parse_commit_map("aaaaaaaaaa bbbbbbbbbb\naaaaaaaaaa bbbbbbbbbb\n").is_ok());
    }

    /// Two events, two IDs, one revision, two answers. Neither event is
    /// wrong on its own, which is exactly why the combination has to be
    /// judged rather than each event in turn.
    #[test]
    fn separate_events_that_disagree_about_one_revision_are_refused() {
        let event = |id: &str, old: &str, new: &str| crate::model::Event {
            schema_version: crate::model::SCHEMA_VERSION,
            event_id: id.to_string(),
            repository_id: "repo".into(),
            change_id: crate::store::Store::REPOSITORY_SCOPE.to_string(),
            actor: "tester".into(),
            actor_source: None,
            harness: None,
            session: None,
            on_behalf_of: None,
            model: None,
            created_at: chrono::Utc::now(),
            payload: Payload::HistoryRewritten {
                mapping: BTreeMap::from([(old.to_string(), Some(new.to_string()))]),
                reason: "test".into(),
                tool: None,
            },
        };
        let agreeing = [
            event("01A", "aaaaaaaaaa", "bbbbbbbbbb"),
            event("01B", "aaaaaaaaaa", "bbbbbbbbbb"),
        ];
        assert!(RewriteMap::from_events(agreeing.iter()).is_ok());

        let disagreeing = [
            event("01A", "aaaaaaaaaa", "bbbbbbbbbb"),
            event("01B", "aaaaaaaaaa", "cccccccccc"),
        ];
        assert!(RewriteMap::from_events(disagreeing.iter()).is_err());
    }

    /// The recording path spells a dropped commit as no successor. A payload
    /// that instead maps to the zero object means something no recorded map
    /// can mean, and a reader would repeat it as a rewrite to nothing.
    /// The zero object is how Git spells "no object". A rewrite *from* it
    /// would claim a successor for a commit that never existed.
    #[test]
    fn a_zero_old_revision_is_not_a_revision() {
        assert!(parse_commit_map(
            "0000000000000000000000000000000000000000 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
        )
        .is_err());
        let zero_old = BTreeMap::from([(
            "0000000000000000000000000000000000000000".to_string(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
        )]);
        assert!(validate_mapping(&zero_old).is_err());
    }

    #[test]
    fn a_mapping_that_no_recorded_map_could_mean_is_refused() {
        let zeroed = BTreeMap::from([(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            Some("0000000000000000000000000000000000000000".to_string()),
        )]);
        assert!(validate_mapping(&zeroed).is_err());

        let identity = BTreeMap::from([("aaaaaaaaaa".to_string(), Some("aaaaaaaaaa".to_string()))]);
        assert!(validate_mapping(&identity).is_err());

        let dropped = BTreeMap::from([("aaaaaaaaaa".to_string(), None)]);
        assert!(validate_mapping(&dropped).is_ok());
    }

    #[test]
    fn a_map_with_no_rewritten_revision_is_refused() {
        assert!(parse_commit_map("\n# nothing\n").is_err());
        assert!(parse_commit_map("old new\n").is_err());
        assert!(parse_commit_map("not-a-revision bbbbbbbbbb\n").is_err());
    }
}
