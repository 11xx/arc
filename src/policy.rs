//! Repository-declared integration policy.
//!
//! Policy is loaded from `.arc/policy.toml`. An absent file preserves the
//! default behavior, with every policy disabled.

use crate::config::ProvenanceBehavior;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Integration policy committed at `.arc/policy.toml` in the repository.
#[derive(Debug, Default, Deserialize)]
pub struct PolicyFile {
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub review: Review,
    #[serde(default)]
    pub provenance: ProvenanceBehavior,
    #[serde(default)]
    pub danger: Danger,
}

/// Surfaces the project has declared dangerous. A change touching one needs a
/// verdict from somebody other than its author; everywhere else a
/// self-recorded verdict satisfies the gate.
///
/// The declaration is a judgement made once, in a reviewable commit, by an
/// identifiable author — rather than one made per change by the party under
/// pressure to ship, which is the party with the incentive to skip it.
#[derive(Debug, Default, Deserialize)]
pub struct Danger {
    /// Path globs. `*` matches within a path segment, `**` across segments.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Files deliberately classified as not dangerous. An entry is a claim
    /// somebody made and a reviewer accepted, not an absence of one.
    #[serde(default)]
    pub acknowledged_safe: Vec<String>,
    /// Directory prefixes inside which classification is closed: every tracked
    /// file must match `paths` or `acknowledged_safe`, and `arc doctor` fails
    /// on one that matches neither.
    ///
    /// Without a declared root the list is open-world, which fails in the
    /// permissive direction: a file nobody classified matches nothing, looks
    /// safe, and lands on a self-verdict. Declaring the root turns adding or
    /// renaming a file from a silent escalation into a loud one.
    #[serde(default)]
    pub source_roots: Vec<String>,
}

impl Danger {
    /// Whether a path is inside a declared closed-world root.
    pub fn within_source_root(&self, path: &str) -> bool {
        self.source_roots
            .iter()
            .any(|root| path.starts_with(root.as_str()))
    }

    /// Whether a path carries an explicit not-dangerous classification.
    pub fn acknowledged_safe(&self, path: &str) -> bool {
        self.acknowledged_safe
            .iter()
            .any(|pattern| glob_match(pattern, path))
    }

    /// Whether a path is declared dangerous.
    pub fn is_dangerous(&self, path: &str) -> bool {
        self.paths.iter().any(|pattern| glob_match(pattern, path))
    }

    /// Declared patterns every one of `changed` is checked against, returning
    /// the paths that matched. Empty means the change touched nothing the
    /// project called dangerous.
    pub fn matching<'a>(&self, changed: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let mut hits: Vec<String> = changed
            .into_iter()
            .filter(|path| self.paths.iter().any(|pattern| glob_match(pattern, path)))
            .map(str::to_string)
            .collect();
        hits.sort();
        hits.dedup();
        hits
    }

    pub fn is_declared(&self) -> bool {
        !self.paths.is_empty()
    }
}

/// Minimal path glob: `**` spans separators, `*` stops at one, everything
/// else is literal. A trailing `/` matches everything beneath a directory.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    // A trailing `/` names a subtree, which is `/**` — spelled out rather than
    // matched by prefix so wildcards keep working ahead of it.
    if let Some(prefix) = pattern.strip_suffix('/') {
        return matches_from(format!("{prefix}/**").as_bytes(), path.as_bytes());
    }
    matches_from(pattern.as_bytes(), path.as_bytes())
}

fn matches_from(pattern: &[u8], path: &[u8]) -> bool {
    match pattern.first() {
        None => path.is_empty(),
        Some(b'*') if pattern.get(1) == Some(&b'*') => {
            let rest = &pattern[2..];
            // `**/` also matches zero directories, so `src/**/*.rs` covers
            // `src/mod.rs`. Without this a pattern silently misses direct
            // children, and a miss here lowers a gate rather than raising it.
            if let Some(after) = rest.strip_prefix(b"/") {
                if matches_from(after, path) {
                    return true;
                }
            }
            // `**` otherwise consumes any run, separators included.
            (0..=path.len()).any(|split| matches_from(rest, &path[split..]))
        }
        Some(b'*') => {
            let rest = &pattern[1..];
            let limit = path
                .iter()
                .position(|byte| *byte == b'/')
                .unwrap_or(path.len());
            (0..=limit).any(|split| matches_from(rest, &path[split..]))
        }
        Some(literal) => path.first() == Some(literal) && matches_from(&pattern[1..], &path[1..]),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub forbid_self_approval: bool,
    /// Refuse to record an event whose effective author nobody declared.
    /// Opt-in, like `forbid_self_approval`: a local ledger cannot verify who
    /// an actor claims to be, but it can decline to invent one.
    #[serde(default)]
    pub require_declared_actor: bool,
    /// Raise debt summaries to advisory priority when more than this many
    /// obligations are outstanding. The comparison is strict so the declared
    /// limit itself remains ordinary.
    #[serde(default)]
    pub debt_count_threshold: Option<usize>,
    /// Raise debt summaries to advisory priority when the oldest obligation is
    /// older than this many seconds. The comparison is strict so the declared
    /// age itself remains ordinary.
    #[serde(default)]
    pub debt_age_threshold_seconds: Option<u64>,
    /// Free bytes below which `arc begin` warns that creating another
    /// worktree is about to add to a filesystem that is running out. Opt-in:
    /// the default is no floor and no warning, because a threshold guessed
    /// per-project by arc would fire wrong everywhere. The floor is never a
    /// refusal — the operator decides with the number, not instead of it.
    #[serde(default)]
    pub worktree_free_floor_bytes: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Review {
    #[serde(default)]
    pub checklist: Vec<String>,
}

pub fn load(repo_toplevel: &Path) -> Result<PolicyFile> {
    let path = repo_toplevel.join(".arc").join("policy.toml");
    if !path.is_file() {
        return Ok(PolicyFile {
            provenance: ProvenanceBehavior {
                git_identity: crate::config::load()?.provenance_git_identity,
            },
            ..PolicyFile::default()
        });
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let mut policy: PolicyFile =
        toml::from_str(&text).with_context(|| format!("malformed {}", path.display()))?;
    let tables: toml::Table =
        toml::from_str(&text).with_context(|| format!("malformed {}", path.display()))?;
    if !tables.contains_key("provenance") {
        policy.provenance.git_identity = crate::config::load()?.provenance_git_identity;
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_star_slash_matches_zero_directories() {
        // gitignore semantics: `**/` spans zero or more directories, so a
        // direct child matches. A miss here would silently lower a gate.
        assert!(glob_match("src/**/*.rs", "src/mod.rs"));
        assert!(glob_match("src/**/*.rs", "src/commands/mod.rs"));
        assert!(glob_match("src/**/*.rs", "src/a/b/c.rs"));
        assert!(!glob_match("src/**/*.rs", "src/mod.txt"));
        // A trailing slash keeps wildcards working ahead of it.
        assert!(glob_match("src/*/", "src/commands/mod.rs"));
        assert!(!glob_match("src/*/", "src/mod.rs"));
    }

    #[test]
    fn globs_match_segments_and_trees() {
        assert!(glob_match("src/store.rs", "src/store.rs"));
        assert!(!glob_match("src/store.rs", "src/store_extra.rs"));
        // `*` stops at a separator; `**` spans them.
        assert!(glob_match("src/*.rs", "src/state.rs"));
        assert!(!glob_match("src/*.rs", "src/commands/mod.rs"));
        assert!(glob_match("src/**/*.rs", "src/commands/mod.rs"));
        assert!(glob_match("src/**", "src/commands/mod.rs"));
        // A trailing slash names a directory and everything beneath it.
        assert!(glob_match("src/commands/", "src/commands/mod.rs"));
        assert!(!glob_match("src/commands/", "src/commands.rs"));
    }

    #[test]
    fn matching_reports_only_declared_hits_once_and_sorted() {
        let danger = Danger {
            paths: vec!["src/state.rs".into(), "src/commands/".into()],
            ..Danger::default()
        };
        let hits = danger.matching(vec![
            "README.md",
            "src/commands/integrate.rs",
            "src/state.rs",
            "src/commands/integrate.rs",
        ]);
        assert_eq!(hits, vec!["src/commands/integrate.rs", "src/state.rs"]);
        assert!(danger.matching(vec!["README.md"]).is_empty());
    }
}
