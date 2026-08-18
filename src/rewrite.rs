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

/// One resolution step. Ambiguity is not the same as no match: the first
/// means the answer is unknown, the second that the chain ended.
enum Step {
    Unmapped,
    Ambiguous,
    Mapped { key: String, fate: Option<String> },
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
    /// `None` means no recorded rewrite touched it, so a caller can tell
    /// "unchanged" from "moved" and from "dropped".
    ///
    /// A recorded revision is matched by prefix in both directions: a map may
    /// abbreviate what the ledger stores in full, and a caller may abbreviate
    /// what the map stores in full. An abbreviation matching more than one
    /// recorded revision names none of them.
    pub fn fate(&self, revision: &str) -> Option<Fate> {
        let mut visited: Vec<String> = Vec::new();
        let mut current = revision.to_string();
        loop {
            match self.step(&current) {
                // Nothing rewrote this one. Either the caller asked about a
                // revision no rewrite touched, or we followed the chain to
                // its end and this is where it survives.
                Step::Unmapped => {
                    return visited
                        .is_empty()
                        .then_some(())
                        .map_or(Some(Fate::Rewritten(current)), |()| None)
                }
                // An abbreviation matching two recorded revisions names
                // neither, and a chain that reaches one cannot be followed —
                // reporting the ambiguous node as a survivor would be a guess.
                Step::Ambiguous => return None,
                Step::Mapped { key, fate } => {
                    // Only a hand-written map can cycle. Visiting a key twice
                    // is what a cycle is, whatever spelling led back to it.
                    if visited.contains(&key) {
                        return None;
                    }
                    visited.push(key);
                    match fate {
                        None => return Some(Fate::Dropped),
                        Some(next) => current = next,
                    }
                }
            }
        }
    }

    /// One hop, resolving `revision` against the map by prefix in either
    /// direction.
    fn step(&self, revision: &str) -> Step {
        let mut matches = self
            .steps
            .iter()
            .filter(|(old, _)| old.starts_with(revision) || revision.starts_with(old.as_str()));
        let Some((old, fate)) = matches.next() else {
            return Step::Unmapped;
        };
        if matches.next().is_some() {
            return Step::Ambiguous;
        }
        Step::Mapped {
            key: old.clone(),
            fate: fate.clone(),
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
    token.len() >= 7 && token.len() <= 64 && token.chars().all(|c| c.is_ascii_hexdigit())
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
            map.fate("aaaaaaaaaa"),
            Some(Fate::Rewritten("cccccccccc".into()))
        );
        assert_eq!(map.fate("cccccccccc"), None);
    }

    /// A map may abbreviate what the ledger records in full, and a caller may
    /// abbreviate what the map records in full. Byte equality would silently
    /// fail to follow either.
    #[test]
    fn revisions_match_by_prefix_in_both_directions() {
        let full = map(&[(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )]);
        assert!(full.fate("aaaaaaaaaa").is_some());
        let short = map(&[("aaaaaaaaaa", Some("bbbbbbbbbb"))]);
        assert!(short
            .fate("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .is_some());
    }

    /// An abbreviation that matches two recorded revisions names neither.
    #[test]
    fn an_ambiguous_abbreviation_resolves_to_nothing() {
        let map = map(&[
            ("aaaaaaaa11", Some("cccccccccc")),
            ("aaaaaaaa22", Some("dddddddddd")),
        ]);
        assert_eq!(map.fate("aaaaaaaa"), None);
    }

    #[test]
    fn a_dropped_revision_is_not_a_survivor() {
        let map = map(&[("aaaaaaaaaa", None)]);
        assert_eq!(map.fate("aaaaaaaaaa"), Some(Fate::Dropped));
    }

    /// Ambiguity anywhere along a chain makes the answer unknown, not the
    /// ambiguous node a survivor.
    #[test]
    fn an_ambiguous_chain_resolves_to_nothing() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("bbbbbbbbbb11", Some("cccccccccc")),
            ("bbbbbbbbbb22", Some("dddddddddd")),
        ]);
        assert_eq!(map.fate("aaaaaaaaaa"), None);
    }

    /// A cycle is visiting a key twice, whatever spelling led back to it.
    #[test]
    fn a_chain_that_returns_to_a_visited_key_resolves_to_nothing() {
        let map = map(&[("aaaaaaaaaa", Some("aaaaaaaaaaaa"))]);
        assert_eq!(map.fate("aaaaaaaaaa"), None);
    }

    #[test]
    fn a_cyclic_mapping_resolves_to_nothing_rather_than_a_bogus_survivor() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("bbbbbbbbbb", Some("aaaaaaaaaa")),
        ]);
        assert_eq!(map.fate("aaaaaaaaaa"), None);
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
