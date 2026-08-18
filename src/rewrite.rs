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

/// Every recorded rewrite, flattened into one old-to-new mapping.
#[derive(Debug, Default, Clone)]
pub struct RewriteMap {
    steps: BTreeMap<String, String>,
}

impl RewriteMap {
    pub fn load(store: &Store) -> Result<Self> {
        let mut steps = BTreeMap::new();
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

    /// Where a recorded revision ended up, following successive rewrites. A
    /// revision nothing rewrote resolves to itself and returns `None`, so a
    /// caller can tell "unchanged" from "moved".
    ///
    /// A mapping that cycles — which only a hand-written one can — stops at
    /// the first repeat rather than looping, because reporting one truthful
    /// successor beats hanging.
    pub fn successor(&self, revision: &str) -> Option<String> {
        let mut seen = vec![revision.to_string()];
        let mut current = self.steps.get(revision)?.clone();
        while let Some(next) = self.steps.get(&current) {
            if seen.iter().any(|step| step == next) {
                break;
            }
            seen.push(current.clone());
            current = next.clone();
        }
        Some(current)
    }
}

/// Parse a commit map: `<old> <new>` per line, which is what `git filter-repo`
/// writes. A line whose new revision is all zeroes records a commit the
/// rewrite dropped, and is not a successor.
pub fn parse_commit_map(text: &str) -> Result<BTreeMap<String, String>> {
    let mut mapping = BTreeMap::new();
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
        if new.chars().all(|c| c == '0') {
            continue;
        }
        mapping.insert(old.to_string(), new.to_string());
    }
    if mapping.is_empty() {
        anyhow::bail!("the commit map records no rewritten revision");
    }
    Ok(mapping)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revision_rewritten_twice_follows_the_chain() {
        let mut steps = BTreeMap::new();
        steps.insert("aaa".to_string(), "bbb".to_string());
        steps.insert("bbb".to_string(), "ccc".to_string());
        let map = RewriteMap { steps };
        assert_eq!(map.successor("aaa").as_deref(), Some("ccc"));
        assert_eq!(map.successor("ccc"), None);
    }

    #[test]
    fn a_cyclic_mapping_stops_instead_of_looping() {
        let mut steps = BTreeMap::new();
        steps.insert("aaa".to_string(), "bbb".to_string());
        steps.insert("bbb".to_string(), "aaa".to_string());
        let map = RewriteMap { steps };
        assert!(map.successor("aaa").is_some());
    }

    #[test]
    fn a_dropped_commit_is_not_a_successor() {
        let mapping =
            parse_commit_map("aaa bbb\nccc 0000000000000000000000000000000000000000\n# comment\n")
                .unwrap();
        assert_eq!(mapping.len(), 1);
        assert_eq!(mapping.get("aaa").unwrap(), "bbb");
    }

    #[test]
    fn a_map_with_no_rewritten_revision_is_refused() {
        assert!(parse_commit_map("\n# nothing\n").is_err());
    }
}
