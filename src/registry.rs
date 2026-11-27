//! Which projects arc knows about, and where each one currently lives.
//!
//! A ledger lives inside its repository's Git common dir, where nothing can
//! enumerate it. A journal lives in one flat root, keyed by its project's
//! path. So the journal root is the only place that knows the set of projects,
//! and this module turns it into one: for each journal directory, the anchor it
//! belongs to, whether that anchor still resolves, and whether a ledger sits
//! there.
//!
//! Everything here is derived and read-only. A registry entry records where a
//! project was last seen; a stale one is a fact to report, never a thing to
//! quietly correct. Retiring one is an explicit, recorded operation
//! (`arc workspace retire`), and its record lives in the journal's own
//! `bindings.jsonl`, so it travels with the directory.

use crate::config::Config;
use crate::store::Store;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// How an entry's anchor was arrived at. A recorded binding is the project's
/// own statement; a reconstruction is arc's best reading of a lossy directory
/// name, and is only ever reported once the filesystem has confirmed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnchorSource {
    /// Read from the journal's own `bindings.jsonl`.
    Binding,
    /// Reconstructed from the directory name and confirmed against the
    /// filesystem, for a journal written before bindings were recorded.
    Reconstructed,
    /// Declared by a `[journals] dirs` path scope, for a project whose journal
    /// lives outside the default root.
    Configured,
    /// The name could not be resolved to exactly one existing directory.
    Unresolved,
}

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    /// The journal directory's name: the project path, slugged.
    pub slug: String,
    pub journal_dir: PathBuf,
    /// Where the project lives, when that is known.
    pub anchor: Option<PathBuf>,
    pub anchor_source: AnchorSource,
    /// Whether `anchor` is a directory that exists right now.
    pub reachable: bool,
    /// The project's ledger, when it has one.
    pub ledger: Option<PathBuf>,
    /// Where this project's ledger belongs, even before the first write.
    #[serde(skip)]
    pub(crate) ledger_root: Option<PathBuf>,
    /// The retirement in force, when the registration was retired. A retired
    /// registration stays in the registry so it can be listed and restored,
    /// and workspace views leave it out of what they observe.
    pub retired: Option<crate::journal::Retirement>,
}

impl Project {
    pub(crate) fn refresh_ledger(&mut self) {
        self.ledger = self
            .ledger_root
            .as_deref()
            .and_then(|root| match existing_ledger(root) {
                Ok(found) => found,
                // An unreadable store is not an absent one.
                Err(error) => {
                    eprintln!(
                        "warning: cannot read the ledger for {}: {error:#}",
                        self.slug
                    );
                    None
                }
            });
    }

    /// A short label for output: the anchor's own name where one is known,
    /// falling back to the slug, which is at least unambiguous.
    pub fn label(&self) -> String {
        self.anchor
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.slug.clone())
    }

    /// An orphan: a journal that says it belongs to a project which is not
    /// there. The case worth reporting loudly, because no per-project command
    /// can reach it — standing in the project is how every other view starts.
    ///
    /// Artifacts are not what makes one. Opening a change registers a project
    /// with a binding and nothing else, so a repository moved before anything
    /// was written to its journal leaves a directory that holds no artifacts
    /// and still names a project with open changes. A recorded anchor that no
    /// longer resolves is the signal; content only adds to it. A retired
    /// registration is not one: its missing anchor is the recorded reason.
    pub fn is_orphan(&self) -> bool {
        if self.reachable || self.retired.is_some() {
            return false;
        }
        matches!(
            self.anchor_source,
            AnchorSource::Binding | AnchorSource::Configured
        ) || self.has_content()
    }

    fn has_content(&self) -> bool {
        let Ok(entries) = std::fs::read_dir(&self.journal_dir) else {
            return false;
        };
        entries.filter_map(|entry| entry.ok()).any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".md"))
        })
    }
}

/// The journal root: one directory per project, plus their cold archives.
pub fn journals_root(cfg: &Config) -> PathBuf {
    cfg.ai_home.join("journals")
}

/// The hot journal directories under the default journal root, excluding cold
/// archives that still have their hot sibling.
pub(crate) fn journal_directories(cfg: &Config) -> Result<Vec<(String, PathBuf)>> {
    let root = journals_root(cfg);
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    if root.is_dir() {
        let entries = std::fs::read_dir(&root)
            .with_context(|| format!("cannot read journal root {}", root.display()))?;
        let names: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .collect();
        for name in &names {
            if is_cold_archive(name, &names) {
                continue;
            }
            dirs.push((name.clone(), root.join(name)));
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn canonical_or_normalized(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let mut suffix = Vec::new();
    let mut cursor = path;
    while let Some(parent) = cursor.parent() {
        if let Some(name) = cursor.file_name() {
            suffix.push(name.to_os_string());
        }
        if let Ok(mut canonical) = std::fs::canonicalize(parent) {
            for part in suffix.into_iter().rev() {
                canonical.push(part);
            }
            return canonical;
        }
        cursor = parent;
    }
    path.to_path_buf()
}

/// Discover concrete `(owner, journal_dir)` pairs for a relative
/// `[journals.dirs]` destination under an existing absolute prefix `anchor`.
///
/// Relative destinations resolve against each matching checkout root rather
/// than the prefix directory itself, so an enclosing Git prefix or a parent
/// container holding nested repositories can own one relative journal per
/// checkout. Populated journals under `anchor` whose own resolution agrees are
/// discovered in breadth-first order up to `MAX_DIRS_READ`; `anchor` itself is
/// included while unpopulated only when it resolves directly to
/// `anchor.join(relative_dir)` and does not act as a non-Git container for
/// nested checkouts.
fn discover_relative_configured_journals(
    cfg: &Config,
    anchor: &Path,
    relative_dir: &Path,
) -> Result<Vec<(PathBuf, PathBuf)>> {
    let canonical_anchor = std::fs::canonicalize(anchor)
        .with_context(|| format!("cannot canonicalize configured scope {}", anchor.display()))?;
    let mut found: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut discovered_journals: HashSet<PathBuf> = HashSet::new();
    let mut has_nested_owner = false;
    let mut queue = VecDeque::new();
    queue.push_back(canonical_anchor.clone());
    let mut budget = MAX_DIRS_READ;

    while let Some(dir) = queue.pop_front() {
        let candidate = canonical_or_normalized(&dir.join(relative_dir));
        if candidate.is_dir() && crate::journal::is_journal_directory(&candidate).unwrap_or(false) {
            if let Ok(resolved) = crate::journal::resolve_project_dir_without_env(cfg, &dir) {
                if canonical_or_normalized(&resolved) == candidate {
                    found.push((dir.clone(), candidate.clone()));
                    discovered_journals.insert(candidate.clone());
                    if dir != canonical_anchor {
                        has_nested_owner = true;
                    }
                }
            }
        }
        if dir != canonical_anchor && !has_nested_owner && dir.join(".git").exists() {
            if let Ok(resolved) = crate::journal::resolve_project_dir_without_env(cfg, &dir) {
                if canonical_or_normalized(&resolved) == candidate {
                    has_nested_owner = true;
                }
            }
        }
        if budget == 0 {
            continue;
        }
        budget -= 1;
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
            .filter(|entry| entry.file_name() != ".git")
            .map(|entry| entry.path())
            .filter(|child| !discovered_journals.contains(&canonical_or_normalized(child)))
            .collect();
        children.sort();
        queue.extend(children);
    }

    let direct_target = canonical_or_normalized(&canonical_anchor.join(relative_dir));
    let anchor_already_found = found.iter().any(|(owner, _)| owner == &canonical_anchor);
    if !anchor_already_found {
        let resolves_directly =
            crate::journal::resolve_project_dir_without_env(cfg, &canonical_anchor)
                .map(|resolved| canonical_or_normalized(&resolved) == direct_target)
                .unwrap_or(false);
        let is_git_root = canonical_anchor.join(".git").exists();
        if resolves_directly && (is_git_root || !has_nested_owner) {
            found.push((canonical_anchor, direct_target));
        }
    }

    Ok(found)
}

struct KnownJournal {
    slug: String,
    journal_dir: PathBuf,
    configured_anchor: Option<PathBuf>,
}

fn known_journal_entries(cfg: &Config) -> Result<Vec<KnownJournal>> {
    let mut entries: Vec<KnownJournal> = journal_directories(cfg)?
        .into_iter()
        .map(|(slug, journal_dir)| {
            let configured_anchor = configured_absolute_anchor(cfg, &journal_dir)?;
            Ok(KnownJournal {
                slug,
                journal_dir,
                configured_anchor,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    for (raw_anchor, raw_directory) in &cfg.journal_dirs {
        let directory = crate::config::expand_tilde(raw_directory)?;
        let anchor = crate::config::expand_tilde(raw_anchor)?;
        if directory.is_absolute() {
            let normalized = canonical_or_normalized(&directory);
            if let Some(existing) = entries
                .iter_mut()
                .find(|entry| canonical_or_normalized(&entry.journal_dir) == normalized)
            {
                let depth = anchor.components().count();
                if existing
                    .configured_anchor
                    .as_ref()
                    .is_none_or(|best| depth > best.components().count())
                {
                    existing.configured_anchor = Some(anchor);
                }
                continue;
            }
            let slug = directory
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| directory.display().to_string());
            entries.push(KnownJournal {
                slug,
                journal_dir: directory,
                configured_anchor: Some(anchor),
            });
            continue;
        }

        if !anchor.is_absolute() || !anchor.is_dir() {
            if entries.iter().any(|entry| entry.journal_dir == directory) {
                continue;
            }
            let slug = directory
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| directory.display().to_string());
            entries.push(KnownJournal {
                slug,
                journal_dir: directory,
                configured_anchor: Some(anchor),
            });
            continue;
        }

        for (owner, journal_dir) in discover_relative_configured_journals(cfg, &anchor, &directory)?
        {
            let normalized = canonical_or_normalized(&journal_dir);
            if let Some(existing) = entries
                .iter_mut()
                .find(|entry| canonical_or_normalized(&entry.journal_dir) == normalized)
            {
                if existing.configured_anchor.is_none() {
                    existing.configured_anchor = Some(owner);
                }
                continue;
            }
            let slug = crate::config::path_slug(&owner);
            entries.push(KnownJournal {
                slug,
                journal_dir,
                configured_anchor: Some(owner),
            });
        }
    }

    entries.sort_by(|a, b| (&a.slug, &a.journal_dir).cmp(&(&b.slug, &b.journal_dir)));
    Ok(entries)
}

/// Every hot journal directory arc knows about, with its slug, sorted: those
/// under the default root and those `[journals] dirs` routes elsewhere.
pub(crate) fn known_journals(cfg: &Config) -> Result<Vec<(String, PathBuf)>> {
    Ok(known_journal_entries(cfg)?
        .into_iter()
        .map(|entry| (entry.slug, entry.journal_dir))
        .collect())
}

/// Every project arc knows about, sorted by journal directory.
///
/// The default root holds one directory per project; `[journals] dirs` may
/// route a project's journal elsewhere, and those are registered too — that
/// config is the documented way to give a non-repository project a journal, so
/// a registry that ignored it would miss exactly the projects that need it.
pub fn projects(cfg: &Config) -> Result<Vec<Project>> {
    let mut projects = journal_projects(cfg)?;
    for project in &mut projects {
        let Some(path) = project
            .anchor
            .as_deref()
            .filter(|_| project.reachable && project.retired.is_none())
        else {
            continue;
        };
        project.ledger_root = ledger_root(cfg, path);
        project.refresh_ledger();
    }
    Ok(projects)
}

/// [`projects`] as their journals record them, without asking Git where each
/// ledger lives: `ledger` is `None` on every one. Enough to resolve a project
/// by slug or label.
pub(crate) fn journal_projects(cfg: &Config) -> Result<Vec<Project>> {
    let mut projects = Vec::new();
    for entry in known_journal_entries(cfg)? {
        let slug = entry.slug;
        let journal_dir = entry.journal_dir;
        let configured = entry.configured_anchor;
        let recorded = if journal_dir.is_absolute() {
            crate::journal::recorded_anchor(&journal_dir)?
        } else {
            None
        };
        let (anchor, anchor_source) = match recorded {
            Some(recorded) => (Some(PathBuf::from(recorded)), AnchorSource::Binding),
            None => match configured {
                Some(path) => (Some(path), AnchorSource::Configured),
                None => match unslug(&slug) {
                    Some(path) => (Some(path), AnchorSource::Reconstructed),
                    None => (None, AnchorSource::Unresolved),
                },
            },
        };
        let reachable = anchor.as_deref().is_some_and(Path::is_dir);
        let retired = if journal_dir.is_absolute() {
            crate::journal::registration_retirement(&journal_dir)?
        } else {
            None
        };
        projects.push(Project {
            slug,
            journal_dir,
            anchor,
            anchor_source,
            reachable,
            ledger: None,
            ledger_root: None,
            retired,
        });
    }
    Ok(projects)
}

/// Whether `name` is another journal's cold archive rather than a project of
/// its own. The suffix alone does not settle it: a project may legitimately be
/// called `something-archive`. An archive is a *sibling*, so it is one only
/// when the journal it was split from is also present.
fn is_cold_archive(name: &str, siblings: &[String]) -> bool {
    name.strip_suffix("-archive")
        .is_some_and(|hot| siblings.iter().any(|sibling| sibling == hot))
}

/// The path scope that routes an absolute journal directory, when one does.
/// The longest matching scope wins, mirroring how the journal itself resolves.
fn configured_absolute_anchor(cfg: &Config, journal_dir: &Path) -> Result<Option<PathBuf>> {
    let target = canonical_or_normalized(journal_dir);
    let mut best: Option<(usize, PathBuf)> = None;
    for (anchor, directory) in &cfg.journal_dirs {
        let expanded = crate::config::expand_tilde(directory)?;
        if !expanded.is_absolute() || canonical_or_normalized(&expanded) != target {
            continue;
        }
        let path = crate::config::expand_tilde(anchor)?;
        let depth = path.components().count();
        if best.as_ref().is_none_or(|(best, _)| depth > *best) {
            best = Some((depth, path));
        }
    }
    Ok(best.map(|(_, path)| path))
}

/// A project's ledger, wherever the configuration puts it: inside the Git
/// common dir by default, or under `data_root` keyed by path slug. Resolving it
/// the same way the store itself does keeps the registry from reporting an
/// empty queue for a project whose ledger simply lives elsewhere.
///
/// Opening never creates: a project with no ledger reports none. A project that
/// is not a Git repository has no ledger either, which is not an error — the
/// journal is what registered it.
pub(crate) fn ledger_at(cfg: &Config, anchor: &Path) -> Result<Option<PathBuf>> {
    match ledger_root(cfg, anchor).as_deref() {
        Some(root) => existing_ledger(root),
        None => Ok(None),
    }
}

fn ledger_root(cfg: &Config, anchor: &Path) -> Option<PathBuf> {
    let common = crate::gitio::common_dir(anchor).ok();
    // `ARC_DATA_DIR` names one store for one repository, so it cannot stand for
    // every project in a sweep — but it is exactly right for the project the
    // caller is standing in, whose ledger would otherwise be reported missing
    // from its own workspace views.
    if let (Some(exact), Some(common)) = (std::env::var_os("ARC_DATA_DIR"), &common) {
        if std::env::current_dir()
            .ok()
            .and_then(|cwd| crate::gitio::common_dir(&cwd).ok())
            .as_ref()
            == Some(common)
        {
            return Some(PathBuf::from(exact));
        }
    }
    match common {
        Some(common) => Some(Store::root_for_common_dir(cfg, &common)),
        None => cfg
            .data_root
            .as_ref()
            .map(|root| root.join(crate::config::path_slug(anchor))),
    }
}

fn existing_ledger(root: &Path) -> Result<Option<PathBuf>> {
    Ok(Store::repository_id_at(root)?.map(|_| root.to_path_buf()))
}

/// Recover the path a slug was made from, by walking the filesystem.
///
/// The slug function maps both `/` and `.` to `-`, so it is lossy and cannot
/// be inverted by string surgery alone: `-home-x-foo-bar` is `/home/x/foo/bar`,
/// `/home/x/foo-bar`, `/home/x/foo.bar`, and more. Rather than guess, walk from
/// the root and let the filesystem say which readings exist. A name resolves
/// only when exactly one reading does; anything else stays unresolved, because
/// a registry that guesses is worse than one that admits it does not know.
fn unslug(slug: &str) -> Option<PathBuf> {
    let rest = slug.strip_prefix('-')?;
    let mut walk = Walk {
        found: Vec::new(),
        budget: MAX_DIRS_READ,
    };
    descend(Path::new("/"), rest, &mut walk);
    // A truncated walk cannot prove uniqueness, so it resolves nothing rather
    // than claiming the first match it happened to reach.
    if walk.budget == 0 {
        return None;
    }
    match walk.found.len() {
        1 => walk.found.pop(),
        _ => None,
    }
}

/// Every candidate path found so far, and how many directories may still be
/// read. The budget bounds the whole walk rather than any one branch: depth
/// alone does not stop a wide tree of prefix-matching names.
struct Walk {
    found: Vec<PathBuf>,
    budget: usize,
}

/// Generous enough that no real layout reaches it, small enough that a
/// pathological name costs a blink. Reconstruction only runs for a journal
/// with no recorded binding, and every journal records one on its next write.
const MAX_DIRS_READ: usize = 4096;

/// Every existing path whose slug is `rest`, reading `-` as either a separator
/// or a literal character in the name.
fn descend(at: &Path, rest: &str, walk: &mut Walk) {
    if walk.found.len() > 1 || walk.budget == 0 {
        return;
    }
    walk.budget -= 1;
    let Ok(entries) = std::fs::read_dir(at) else {
        return;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // A name's own `/` is impossible and its `.` is slugged to `-`, so the
        // comparison happens in slug space on both sides.
        let slugged: String = name
            .chars()
            .map(|c| if c == '.' { '-' } else { c })
            .collect();
        let Some(tail) = rest.strip_prefix(slugged.as_str()) else {
            continue;
        };
        let next = at.join(&name);
        if tail.is_empty() {
            walk.found.push(next);
        } else if let Some(deeper) = tail.strip_prefix('-') {
            descend(&next, deeper, walk);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn unslug_reads_a_dash_as_separator_or_as_part_of_a_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let nested = base.join("ai-agent").join("skills");
        fs::create_dir_all(&nested).unwrap();

        let slug = crate::config::path_slug(&nested);
        let mut walk = Walk {
            found: Vec::new(),
            budget: MAX_DIRS_READ,
        };
        descend(Path::new("/"), slug.strip_prefix('-').unwrap(), &mut walk);
        assert_eq!(walk.found, vec![nested], "slug {slug}");
    }

    #[test]
    fn unslug_refuses_an_ambiguous_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        // Two readings of the same slug, both of which exist.
        fs::create_dir_all(base.join("foo-bar")).unwrap();
        fs::create_dir_all(base.join("foo").join("bar")).unwrap();

        let slug = format!("{}-foo-bar", crate::config::path_slug(&base));
        let mut walk = Walk {
            found: Vec::new(),
            budget: MAX_DIRS_READ,
        };
        descend(Path::new("/"), slug.strip_prefix('-').unwrap(), &mut walk);
        assert!(
            walk.found.len() > 1,
            "expected ambiguity, got {:?}",
            walk.found
        );
        assert_eq!(unslug(&slug), None);
    }

    #[test]
    fn unslug_returns_nothing_for_a_path_that_does_not_exist() {
        assert_eq!(unslug("-no-such-path-anywhere-at-all-really"), None);
    }

    /// A cold archive is a sibling of the journal it was split from, so the
    /// suffix alone cannot identify one: a project may be called that.
    #[test]
    fn a_project_named_archive_is_not_mistaken_for_a_cold_sibling() {
        let names = vec![
            "-home-x-notes".to_string(),
            "-home-x-notes-archive".to_string(),
            "-home-x-mail-archive".to_string(),
        ];
        assert!(is_cold_archive("-home-x-notes-archive", &names));
        // Nothing was split from this one; it is a project in its own right.
        assert!(!is_cold_archive("-home-x-mail-archive", &names));
        assert!(!is_cold_archive("-home-x-notes", &names));
    }

    /// The walk is bounded in total, not per branch, and a truncated walk
    /// resolves nothing rather than claiming whatever it reached first.
    #[test]
    fn an_exhausted_budget_resolves_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let nested = base.join("deep");
        fs::create_dir_all(&nested).unwrap();
        let slug = crate::config::path_slug(&nested);

        let mut walk = Walk {
            found: Vec::new(),
            budget: 1,
        };
        descend(Path::new("/"), slug.strip_prefix('-').unwrap(), &mut walk);
        assert_eq!(walk.budget, 0);
        assert!(walk.found.is_empty(), "{:?}", walk.found);
    }
}
