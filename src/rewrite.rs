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

/// Every recorded rewrite, flattened into one old-to-fate mapping.
#[derive(Debug, Default, Clone)]
pub struct RewriteMap {
    steps: BTreeMap<String, Option<String>>,
}

impl RewriteMap {
    pub fn load(store: &Store) -> Result<Self> {
        let mut steps: BTreeMap<String, Option<String>> = BTreeMap::new();
        for event in store.load_repository_events()? {
            if let Payload::HistoryRewritten { mapping, .. } = &event.payload {
                for (old, new) in mapping {
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
        let mut seen: Vec<String> = Vec::new();
        let mut current = revision.to_string();
        loop {
            let (key, next) = self.step(&current)?;
            match next {
                None => return Some(Fate::Dropped),
                Some(next) => {
                    // Only a hand-written map can cycle. Stopping beats
                    // looping, and beats reporting an arbitrary node as a
                    // survivor.
                    if seen.iter().any(|step| step == &next) {
                        return None;
                    }
                    seen.push(key);
                    current = next;
                }
            }
            if self.step(&current).is_none() {
                return Some(Fate::Rewritten(current));
            }
        }
    }

    /// One hop, resolving `revision` against the map by prefix in either
    /// direction. Returns the matched key and its fate.
    fn step(&self, revision: &str) -> Option<(String, Option<String>)> {
        let mut matches = self
            .steps
            .iter()
            .filter(|(old, _)| old.starts_with(revision) || revision.starts_with(old.as_str()));
        let (old, fate) = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some((old.clone(), fate.clone()))
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

    #[test]
    fn a_map_with_no_rewritten_revision_is_refused() {
        assert!(parse_commit_map("\n# nothing\n").is_err());
        assert!(parse_commit_map("old new\n").is_err());
        assert!(parse_commit_map("not-a-revision bbbbbbbbbb\n").is_err());
    }
}
