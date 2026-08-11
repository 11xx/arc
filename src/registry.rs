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

/// Every project the journal root knows about, sorted by slug.
///
/// Cold archives are excluded by the same `-archive` suffix rule that creates
/// them, so an archive is never mistaken for a project of its own.
pub fn projects(cfg: &Config) -> Result<Vec<Project>> {
    let root = journals_root(cfg);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(&root)
        .with_context(|| format!("cannot read journal root {}", root.display()))?;
    let mut slugs: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| !name.ends_with("-archive"))
        .collect();
    slugs.sort();

    let mut projects = Vec::new();
    for slug in slugs {
        let journal_dir = root.join(&slug);
        let (anchor, anchor_source) = match crate::journal::recorded_anchor(&journal_dir)? {
            Some(recorded) => (Some(PathBuf::from(recorded)), AnchorSource::Binding),
            None => match unslug(&slug) {
                Some(path) => (Some(path), AnchorSource::Reconstructed),
                None => (None, AnchorSource::Unresolved),
            },
        };
        let reachable = anchor.as_deref().is_some_and(Path::is_dir);
        let ledger = anchor
            .as_deref()
            .filter(|_| reachable)
            .and_then(|path| ledger_at(path).ok().flatten());
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

/// The store inside a project's Git common dir, when that project has one.
/// Opening never creates: a project with no ledger reports none.
fn ledger_at(anchor: &Path) -> Result<Option<PathBuf>> {
    let common = crate::gitio::common_dir(anchor)?;
    let root = common.join("arc");
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
    let mut found = Vec::new();
    descend(Path::new("/"), rest, &mut found);
    match found.len() {
        1 => found.pop(),
        _ => None,
    }
}

/// Depth-limited so a pathological name cannot walk the whole filesystem: no
/// real project path is anywhere near this deep.
const MAX_DEPTH: usize = 24;

/// Every existing path whose slug is `rest`, reading `-` as either a separator
/// or a literal character in the name.
fn descend(at: &Path, rest: &str, found: &mut Vec<PathBuf>) {
    if found.len() > 1 || at.components().count() > MAX_DEPTH {
        return;
    }
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
            found.push(next);
        } else if let Some(deeper) = tail.strip_prefix('-') {
            descend(&next, deeper, found);
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
        let mut found = Vec::new();
        descend(Path::new("/"), slug.strip_prefix('-').unwrap(), &mut found);
        assert_eq!(found, vec![nested], "slug {slug}");
    }

    #[test]
    fn unslug_refuses_an_ambiguous_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        // Two readings of the same slug, both of which exist.
        fs::create_dir_all(base.join("foo-bar")).unwrap();
        fs::create_dir_all(base.join("foo").join("bar")).unwrap();

        let slug = format!("{}-foo-bar", crate::config::path_slug(&base));
        let mut found = Vec::new();
        descend(Path::new("/"), slug.strip_prefix('-').unwrap(), &mut found);
        assert!(found.len() > 1, "expected ambiguity, got {found:?}");
        assert_eq!(unslug(&slug), None);
    }

    #[test]
    fn unslug_returns_nothing_for_a_path_that_does_not_exist() {
        assert_eq!(unslug("-no-such-path-anywhere-at-all-really"), None);
    }
}
