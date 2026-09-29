//! Repository integration policy from project and operator declarations.
//!
//! `.arc/policy.toml` travels with a repository. The operator policy is kept
//! beside the repository ledger under Git's common directory, so a clone can
//! carry local review policy without adding it to the contributed tree.

use crate::config::{GitIdentityMode, ProvenanceBehavior};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Effective policy declarations and the source files that supplied them.
#[derive(Debug, Default)]
pub struct PolicyFile {
    pub policy: Policy,
    pub review: Review,
    pub provenance: ProvenanceBehavior,
    pub danger: Danger,
    /// Declared when the repository receives contributions rather than
    /// merges: its history shape is the receiver's, and `integrate` records a
    /// change as ready to send instead of merging it.
    pub contribution: Option<Contribution>,
    pub sources: PolicySources,
}

/// How a contributed change's history must look when it is sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum History {
    /// Every commit of the change is sent as it stands.
    #[default]
    Preserve,
    /// The change is sent as one commit on its base.
    Squash,
}

impl History {
    pub fn as_str(self) -> &'static str {
        match self {
            History::Preserve => "preserve",
            History::Squash => "squash",
        }
    }
}

/// A repository whose integration happens at a receiver arc cannot see.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Contribution {
    pub history: History,
}

/// Stable origin labels for every policy declaration.
#[derive(Debug, Default)]
pub struct PolicySources {
    entries: BTreeMap<String, BTreeSet<String>>,
}

impl PolicySources {
    fn record(&mut self, rule: String, source: &str) {
        self.entries
            .entry(rule)
            .or_default()
            .insert(source.to_string());
    }

    pub fn as_map(&self) -> BTreeMap<String, Vec<String>> {
        self.entries
            .iter()
            .map(|(rule, sources)| (rule.clone(), sources.iter().cloned().collect()))
            .collect()
    }

    pub fn sources_for(&self, rule: &str) -> Vec<String> {
        self.entries
            .get(rule)
            .map(|sources| sources.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Surfaces the project or operator has declared dangerous. A change touching
/// one needs a verdict from somebody other than its author; everywhere else a
/// self-recorded verdict satisfies the gate.
///
/// The declaration is a judgement made once, in a reviewable commit or local
/// operator policy, by an identifiable author — rather than one made per
/// change by the party under pressure to ship.
#[derive(Debug, Default)]
pub struct Danger {
    /// Path globs. `*` matches within a path segment, `**` across segments.
    pub paths: Vec<String>,
    /// Files deliberately classified as not dangerous. An entry is a claim
    /// somebody made and a reviewer accepted, not an absence of one.
    pub acknowledged_safe: Vec<String>,
    /// Directory prefixes inside which classification is closed: every tracked
    /// file must match `paths` or `acknowledged_safe`, and `arc doctor` fails
    /// on one that matches neither.
    ///
    /// Without a declared root the list is open-world, which fails in the
    /// permissive direction: a file nobody classified matches nothing, looks
    /// safe, and lands on a self-verdict. Declaring the root turns adding or
    /// renaming a file from a silent escalation into a loud one.
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
struct PolicyLayer {
    policy: Option<PolicyLayerPolicy>,
    review: Option<ReviewLayer>,
    provenance: Option<ProvenanceLayer>,
    danger: Option<DangerLayer>,
    contribution: Option<ContributionLayer>,
}

#[derive(Debug, Default, Deserialize)]
struct ContributionLayer {
    history: Option<History>,
}

#[derive(Debug, Default, Deserialize)]
struct PolicyLayerPolicy {
    forbid_self_approval: Option<bool>,
    require_declared_actor: Option<bool>,
    debt_count_threshold: Option<usize>,
    debt_age_threshold_seconds: Option<u64>,
    worktree_free_floor_bytes: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct ReviewLayer {
    checklist: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct ProvenanceLayer {
    git_identity: Option<GitIdentityMode>,
}

#[derive(Debug, Default, Deserialize)]
struct DangerLayer {
    paths: Option<Vec<String>>,
    acknowledged_safe: Option<Vec<String>>,
    source_roots: Option<Vec<String>>,
}

/// Operator policy is stored in the repository's shared Git directory, next
/// to the ledger, so linked worktrees and clones resolve one local file.
pub fn operator_path(repo_toplevel: &Path) -> Result<PathBuf> {
    Ok(crate::gitio::common_dir(repo_toplevel)?.join("arc/operator-policy.toml"))
}

const PROJECT_SOURCE: &str = ".arc/policy.toml";
const OPERATOR_SOURCE: &str = "<git-common-dir>/arc/operator-policy.toml";

/// Validate the policy portion of an operator-policy document.
pub fn validate_operator_text(text: &str) -> Result<()> {
    toml::from_str::<PolicyLayer>(text).context("malformed operator policy TOML")?;
    Ok(())
}

/// Load the policy in force in the checkout at `repo_toplevel`.
pub fn load(repo_toplevel: &Path) -> Result<PolicyFile> {
    let path = repo_toplevel.join(PROJECT_SOURCE);
    let project = read_optional(&path)?.map(|text| (path.display().to_string(), text));
    layered(repo_toplevel, project)
}

/// Load the policy with the project layer read from the tree committed at
/// `revision`, wherever `cwd` stands in the repository.
pub fn load_at(cwd: &Path, revision: &str) -> Result<PolicyFile> {
    let project = crate::gitio::file_at(cwd, revision, PROJECT_SOURCE)?
        .map(|text| (format!("{PROJECT_SOURCE} at {revision}"), text));
    layered(cwd, project)
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Layer the project declaration, given as its origin and text, under the
/// operator's. `repo` is any path inside the repository.
fn layered(repo: &Path, project: Option<(String, String)>) -> Result<PolicyFile> {
    let operator = operator_path(repo)?;
    let operator = read_optional(&operator)?.map(|text| (operator.display().to_string(), text));
    let mut layers = Vec::new();
    for (source, layer) in [(PROJECT_SOURCE, project), (OPERATOR_SOURCE, operator)] {
        let Some((origin, text)) = layer else {
            continue;
        };
        let layer =
            toml::from_str::<PolicyLayer>(&text).with_context(|| format!("malformed {origin}"))?;
        layers.push((source.to_string(), layer));
    }

    let mut policy = Policy::default();
    let mut review = Review::default();
    let mut danger = Danger::default();
    let mut sources = PolicySources::default();
    let mut provenance_modes = Vec::new();
    let mut debt_count_threshold = Vec::new();
    let mut debt_age_threshold_seconds = Vec::new();
    let mut worktree_free_floor_bytes = Vec::new();
    let mut checklist = Vec::new();
    let mut checklist_seen = BTreeSet::new();
    let mut paths = Vec::new();
    let mut paths_seen = BTreeSet::new();
    let mut acknowledged_safe = Vec::new();
    let mut acknowledged_safe_seen = BTreeSet::new();
    let mut source_roots = Vec::new();
    let mut source_roots_seen = BTreeSet::new();
    let mut contribution: Option<Contribution> = None;

    for (source, layer) in layers {
        if let Some(raw) = layer.policy {
            if let Some(value) = raw.forbid_self_approval {
                policy.forbid_self_approval |= value;
                sources.record(format!("policy.forbid_self_approval={value}"), &source);
            }
            if let Some(value) = raw.require_declared_actor {
                policy.require_declared_actor |= value;
                sources.record(format!("policy.require_declared_actor={value}"), &source);
            }
            if let Some(value) = raw.debt_count_threshold {
                debt_count_threshold.push(value);
                sources.record(format!("policy.debt_count_threshold={value}"), &source);
            }
            if let Some(value) = raw.debt_age_threshold_seconds {
                debt_age_threshold_seconds.push(value);
                sources.record(
                    format!("policy.debt_age_threshold_seconds={value}"),
                    &source,
                );
            }
            if let Some(value) = raw.worktree_free_floor_bytes {
                worktree_free_floor_bytes.push(value);
                sources.record(format!("policy.worktree_free_floor_bytes={value}"), &source);
            }
        }
        if let Some(raw) = layer.review {
            if let Some(entries) = raw.checklist {
                for entry in entries {
                    sources.record(format!("review.checklist[{entry:?}]"), &source);
                    if checklist_seen.insert(entry.clone()) {
                        checklist.push(entry);
                    }
                }
            }
        }
        if let Some(raw) = layer.provenance {
            let mode = raw.git_identity.unwrap_or_default();
            sources.record(
                format!("provenance.git_identity={}", mode.as_str()),
                &source,
            );
            provenance_modes.push(mode);
        }
        if let Some(raw) = layer.contribution {
            // Declaring contribution in either file makes the repository one;
            // squash is the stricter shape and wins over preserve.
            let history = raw.history.unwrap_or_default();
            sources.record(
                format!("contribution.history={}", history.as_str()),
                &source,
            );
            let current = contribution.get_or_insert_with(Contribution::default);
            if history == History::Squash {
                current.history = History::Squash;
            }
        }
        if let Some(raw) = layer.danger {
            if let Some(entries) = raw.paths {
                for pattern in entries {
                    sources.record(format!("danger.paths[{pattern:?}]"), &source);
                    if paths_seen.insert(pattern.clone()) {
                        paths.push(pattern);
                    }
                }
            }
            if let Some(entries) = raw.acknowledged_safe {
                for pattern in entries {
                    sources.record(format!("danger.acknowledged_safe[{pattern:?}]"), &source);
                    if acknowledged_safe_seen.insert(pattern.clone()) {
                        acknowledged_safe.push(pattern);
                    }
                }
            }
            if let Some(entries) = raw.source_roots {
                for root in entries {
                    sources.record(format!("danger.source_roots[{root:?}]"), &source);
                    if source_roots_seen.insert(root.clone()) {
                        source_roots.push(root);
                    }
                }
            }
        }
    }

    policy.debt_count_threshold = debt_count_threshold.into_iter().min();
    policy.debt_age_threshold_seconds = debt_age_threshold_seconds.into_iter().min();
    policy.worktree_free_floor_bytes = worktree_free_floor_bytes.into_iter().max();
    review.checklist = checklist;
    danger.paths = paths;
    danger.acknowledged_safe = acknowledged_safe;
    danger.source_roots = source_roots;

    let git_identity = if provenance_modes
        .iter()
        .any(|mode| matches!(mode, GitIdentityMode::PerActor))
    {
        GitIdentityMode::PerActor
    } else if !provenance_modes.is_empty() {
        GitIdentityMode::Shared
    } else {
        let config = crate::config::load()?;
        if config_declares_git_identity(&config.config_path)? {
            sources.record(
                format!(
                    "provenance.git_identity={}",
                    config.provenance_git_identity.as_str()
                ),
                "<arc-config>/config.toml",
            );
        }
        config.provenance_git_identity
    };

    Ok(PolicyFile {
        policy,
        review,
        provenance: ProvenanceBehavior { git_identity },
        danger,
        contribution,
        sources,
    })
}

fn config_declares_git_identity(path: &Path) -> Result<bool> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let config: toml::Value =
        toml::from_str(&text).with_context(|| format!("malformed {}", path.display()))?;
    Ok(config
        .get("provenance")
        .and_then(toml::Value::as_table)
        .is_some_and(|table| table.contains_key("git_identity")))
}

/// Policy that applies to every change in the repository.
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

#[derive(Debug, Default)]
pub struct Review {
    pub checklist: Vec<String>,
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
        // A trailing `/` names a directory and everything beneath it.
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

    #[test]
    fn project_policy_requires_independent_review_for_declaration_files() {
        let project: PolicyLayer = toml::from_str(include_str!("../.arc/policy.toml")).unwrap();
        let danger = project.danger.unwrap();
        let paths = danger.paths.unwrap();
        assert!(paths.contains(&".arc/gates.toml".to_string()));
        assert!(paths.contains(&".arc/policy.toml".to_string()));
    }
}
