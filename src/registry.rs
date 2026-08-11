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
//! quietly correct.

use crate::config::Config;
use crate::store::Store;
use anyhow::{Context, Result};
use serde::Serialize;
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
}

impl Project {
    /// A short label for output: the anchor's own name where one is known,
    /// falling back to the slug, which is at least unambiguous.
    pub fn label(&self) -> String {
        self.anchor
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.slug.clone())
    }

    /// An orphan: a journal holding work whose project cannot be found. The
    /// case worth reporting loudly, because no per-project command can reach
    /// it — standing in the project is how every other view starts.
    pub fn is_orphan(&self) -> bool {
        !self.reachable && self.has_content()
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

/// Every project arc knows about, sorted by journal directory.
///
/// The default root holds one directory per project; `[journals] dirs` may
/// route a project's journal elsewhere, and those are registered too — that
/// config is the documented way to give a non-repository project a journal, so
/// a registry that ignored it would miss exactly the projects that need it.
pub fn projects(cfg: &Config) -> Result<Vec<Project>> {
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
    for directory in cfg.journal_dirs.values() {
        let path = crate::config::expand_tilde(directory)?;
        if dirs.iter().any(|(_, known)| known == &path) {
            continue;
        }
        let slug = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        dirs.push((slug, path));
    }
    dirs.sort();

    let mut projects = Vec::new();
    for (slug, journal_dir) in dirs {
        let configured = configured_anchor(cfg, &journal_dir)?;
        let (anchor, anchor_source) = match crate::journal::recorded_anchor(&journal_dir)? {
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
        let ledger = match anchor.as_deref().filter(|_| reachable) {
            Some(path) => match ledger_at(cfg, path) {
                Ok(found) => found,
                // An unreadable store is not an absent one, and this whole
                // feature exists so that work stops going unseen.
                Err(error) => {
                    eprintln!("warning: cannot read the ledger for {slug}: {error:#}");
                    None
                }
            },
            None => None,
        };
        projects.push(Project {
            slug,
            journal_dir,
            anchor,
            anchor_source,
            reachable,
            ledger,
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

/// The path scope that routes this journal directory, when one does. The
/// longest matching scope wins, mirroring how the journal itself resolves.
fn configured_anchor(cfg: &Config, journal_dir: &Path) -> Result<Option<PathBuf>> {
    let mut best: Option<(usize, PathBuf)> = None;
    for (anchor, directory) in &cfg.journal_dirs {
        if crate::config::expand_tilde(directory)? != journal_dir {
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
fn ledger_at(cfg: &Config, anchor: &Path) -> Result<Option<PathBuf>> {
    let root = match &cfg.data_root {
        Some(data_root) => data_root.join(crate::config::path_slug(anchor)),
        None => match crate::gitio::common_dir(anchor) {
            Ok(common) => common.join("arc"),
            Err(_) => return Ok(None),
        },
    };
    match Store::open_at(&root)? {
        Some(_) => Ok(Some(root)),
        None => Ok(None),
    }
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
