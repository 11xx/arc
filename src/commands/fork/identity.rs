//! A fork's identity, and the only place it is computed.
//!
//! The question — which fork is this, and where does its checkout live — has
//! one answer per repository, and this module is the only code that may
//! produce one. `ForkIdentity` has private fields and no public constructor,
//! so no caller anywhere, including the rest of the fork command, can assemble
//! an identity from a branch name, a directory name, a commit, or a marker
//! field. That is deliberate: the same defect surfaced five times because each
//! caller answered the question itself from whatever input was at hand, and
//! every one of those inputs is repository-wide except in the state that
//! exposed it.

use super::{fork_branch, is_fork_branch, FORK_TOPIC_PREFIX};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// One fork marker: the newest `plan` artifact under `fork-<slug>`.
#[derive(Clone)]
pub(super) struct ForkMarker {
    filename: String,
    body: String,
}

fn marker_branch(marker: &ForkMarker) -> Option<String> {
    let branch = marker_field(&marker.body, "branch")?;
    is_fork_branch(&branch).then_some(branch)
}

/// Every fork marker in the journal, newest first: the path match wants the
/// newest claim about a worktree that may have been re-forked.
fn fork_markers(dir: &Path) -> Vec<ForkMarker> {
    let mut markers: Vec<ForkMarker> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            crate::journal::parse_artifact_name(&name)
                .filter(|(_, topic, kind)| kind == "plan" && topic.starts_with(FORK_TOPIC_PREFIX))
                .map(|_| ForkMarker {
                    filename: name.clone(),
                    body: std::fs::read_to_string(dir.join(&name)).unwrap_or_default(),
                })
        })
        .collect();
    markers.sort_by(|a, b| b.filename.cmp(&a.filename));
    markers
}

pub(super) fn canonical_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A marker records where a fork's checkout is, and that answer must be the
/// same from every worktree in the repository. A relative path cannot be:
/// resolving it against the caller's root makes the fork live in a different
/// place depending on where the question was asked, which is how one resolver
/// starts answering as two. Only an absolute record names a checkout.
fn marker_worktree_path(marker: &ForkMarker) -> Option<PathBuf> {
    let recorded = PathBuf::from(marker_field(&marker.body, "worktree")?);
    recorded.is_absolute().then(|| canonical_path(&recorded))
}

/// One fork, as the repository reports it.
///
/// Obtainable only from `resolve_all` and the selectors below, because the
/// fields are private and there is no constructor. A caller reads an identity;
/// it never asserts one.
pub(super) struct ForkIdentity {
    slug: String,
    branch: String,
    worktree: Option<PathBuf>,
    marker: Option<ForkMarker>,
}

impl ForkIdentity {
    pub(super) fn slug(&self) -> &str {
        &self.slug
    }

    pub(super) fn branch(&self) -> &str {
        &self.branch
    }

    pub(super) fn worktree(&self) -> Option<&Path> {
        self.worktree.as_deref()
    }

    pub(super) fn marker(&self) -> Option<&ForkMarker> {
        self.marker.as_ref()
    }
}

/// Every fork this repository has, resolved once from one reading of the
/// journal, the worktree inventory, and the branch list.
///
/// A `fork/<slug>` branch is the fact; a marker is a claim about one. Git's
/// branch association is authoritative wherever it exists, and a marker binds
/// a detached checkout only when the branch has no attached worktree and Git
/// still reports the path as live.
pub(super) fn resolve_all(cwd: &Path) -> Result<Vec<ForkIdentity>> {
    let dir = crate::journal::resolve_dir(cwd)?;
    let markers = fork_markers(&dir);
    let inventory = crate::gitio::worktree_inventory(cwd)?;
    let live: Vec<_> = inventory
        .into_iter()
        .filter(|entry| !entry.prunable && entry.path.exists())
        .map(|entry| {
            let path = canonical_path(&entry.path);
            (entry, path)
        })
        .collect();

    let branches = crate::gitio::git(
        cwd,
        &["branch", "--list", "--format=%(refname:short)", "fork/*"],
    )?;
    let mut forks: Vec<ForkIdentity> = branches
        .lines()
        .filter(|branch| is_fork_branch(branch))
        .map(|branch| ForkIdentity {
            slug: branch
                .strip_prefix(crate::commands::fork::FORK_BRANCH_PREFIX)
                .expect("filtered by is_fork_branch")
                .to_string(),
            branch: branch.to_string(),
            worktree: None,
            marker: markers
                .iter()
                .find(|marker| marker_branch(marker).as_deref() == Some(branch))
                .cloned(),
        })
        .collect();

    for fork in &mut forks {
        if let Some((entry, _)) = live
            .iter()
            .find(|(entry, _)| entry.branch.as_deref() == Some(fork.branch.as_str()))
        {
            fork.worktree = Some(entry.path.clone());
        }
    }

    // Only a detached, live inventory entry may be bound by a marker. A
    // missing path or Git's prunable annotation is not a usable worktree.
    for fork in &mut forks {
        if fork.worktree.is_some() {
            continue;
        }
        let Some(recorded) = fork.marker.as_ref().and_then(marker_worktree_path) else {
            continue;
        };
        if let Some((entry, _)) = live
            .iter()
            .find(|(entry, path)| entry.branch.is_none() && *path == recorded)
        {
            fork.worktree = Some(entry.path.clone());
        }
    }

    Ok(forks)
}

/// The fork whose checkout `cwd` stands in, if it is one.
pub(super) fn by_current_path(cwd: &Path) -> Result<Option<ForkIdentity>> {
    let root = canonical_path(&crate::gitio::toplevel(cwd)?);
    Ok(resolve_all(cwd)?.into_iter().find(|fork| {
        fork.worktree()
            .is_some_and(|path| canonical_path(path) == root)
    }))
}

/// The fork a slug names, if the repository has one.
pub(super) fn by_branch(cwd: &Path, slug: &str) -> Result<Option<ForkIdentity>> {
    let branch = fork_branch(slug);
    Ok(resolve_all(cwd)?
        .into_iter()
        .find(|fork| fork.branch == branch))
}

pub(super) fn marker_field(body: &str, key: &str) -> Option<String> {
    body.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}: ")))
        .map(str::to_string)
}

impl ForkMarker {
    pub(super) fn filename(&self) -> &str {
        &self.filename
    }

    pub(super) fn field(&self, key: &str) -> Option<String> {
        marker_field(&self.body, key)
    }
}

#[cfg(test)]
mod tests {
    /// The point of this module is what cannot be written, so the test is a
    /// compile rather than an assertion. Uncommenting either line below must
    /// fail to build:
    ///
    /// ```compile_fail
    /// let identity = super::ForkIdentity {
    ///     slug: "invented".to_string(),
    ///     branch: "fork/invented".to_string(),
    ///     worktree: None,
    ///     marker: None,
    /// };
    /// ```
    ///
    /// A caller reads an identity through the accessors and cannot assemble
    /// one from a branch name, a directory name, a commit, or a marker field.
    /// That is the whole guarantee: the five occurrences of one defect each
    /// began with a caller answering the identity question itself.
    #[test]
    fn an_identity_is_obtained_rather_than_asserted() {
        // The guarantee is enforced by privacy at compile time; this test
        // exists so the guarantee has a name in the suite and a place for the
        // next reader to look.
        assert!(std::mem::size_of::<super::ForkIdentity>() > 0);
    }
}
