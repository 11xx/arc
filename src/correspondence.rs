//! An asserted commit correspondence: the proof that stands in for ancestry
//! when a change's commits reached its target as other commits that arc did
//! not produce (a cherry-pick, a rebase elsewhere, or a squash merge).
//!
//! The file binds the change, its patchset and source head, the revision the
//! target stood at before (`target_before`) and the asserted revision.
//!
//! - In `commit` mode (default when `mode` is omitted), every source commit
//!   maps 1:1 to one landed commit in `target_before..revision`, the landed
//!   commits follow source order along that range, and each pair has the same
//!   stable patch id (or identical parent and result trees).
//! - In `squash` mode, every source commit in `base..head` maps to one single
//!   non-merge landed commit in `target_before..revision` whose patch id equals
//!   the combined diff `base..head` (or whose parent/result trees equal
//!   `base`/`head`).

use crate::gitio;
use crate::model::CorrespondedCommit;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

pub const CORRESPONDENCE_SCHEMA: &str = "arc-correspondence/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    #[default]
    Commit,
    Squash,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Squash => "squash",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    schema: String,
    #[serde(default)]
    mode: Mode,
    change: String,
    patchset: String,
    source_head: String,
    target_before: String,
    revision: String,
    commits: Vec<CorrespondedCommit>,
}

/// What a correspondence must be about.
pub struct Subject<'a> {
    pub change_id: &'a str,
    pub patchset_id: &'a str,
    pub base: &'a str,
    pub head: &'a str,
    pub target_before: &'a str,
    pub revision: &'a str,
}

/// An automatically derived and verified `arc-correspondence/1` mapping.
pub struct Derived {
    pub mode: Mode,
    pub pairs: Vec<CorrespondedCommit>,
}

/// Read the file at `path` (or stdin for `-`) and check it against `subject`,
/// returning the pairs in source order with every name resolved to its full
/// object id.
pub fn check(cwd: &Path, path: &Path, subject: &Subject) -> Result<Vec<CorrespondedCommit>> {
    let bytes = if path == Path::new("-") {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("cannot read correspondence from stdin")?;
        buf
    } else {
        std::fs::read(path)
            .with_context(|| format!("cannot read correspondence {}", path.display()))?
    };
    let file: File = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not an {CORRESPONDENCE_SCHEMA} file", path.display()))?;
    check_file(cwd, &file, subject)
}

/// Derive and verify an unambiguous `arc-correspondence/1` mapping from
/// `subject.base..subject.head` to `subject.target_before..subject.revision`.
///
/// Prefers an unambiguous 1:1 order-preserving commit correspondence (rebase /
/// cherry-pick); falls back to an unambiguous single-commit squash match when
/// commits were squashed into one landed commit matching the combined diff or
/// tree equivalence.
pub fn derive(cwd: &Path, subject: &Subject) -> Result<Derived> {
    let sources = gitio::range_commits(cwd, subject.base, subject.head)?;
    if sources.is_empty() {
        bail!(
            "{} has no commits, so there is nothing to correspond",
            subject.patchset_id
        );
    }
    let landed_commits = gitio::range_commits(cwd, subject.target_before, subject.revision)?;
    if landed_commits.is_empty() {
        bail!(
            "{}..{} has no commits to correspond against {}",
            subject.target_before,
            subject.revision,
            subject.patchset_id
        );
    }

    // Attempt 1: 1:1 commit-for-commit match (rebase / cherry-pick).
    let commit_attempt = derive_commit_mode(cwd, &sources, &landed_commits)?;
    match commit_attempt {
        CommitMatch::Unique(pairs) => {
            let file = File {
                schema: CORRESPONDENCE_SCHEMA.into(),
                mode: Mode::Commit,
                change: subject.change_id.into(),
                patchset: subject.patchset_id.into(),
                source_head: subject.head.into(),
                target_before: subject.target_before.into(),
                revision: subject.revision.into(),
                commits: pairs,
            };
            let verified = check_file(cwd, &file, subject)?;
            Ok(Derived {
                mode: Mode::Commit,
                pairs: verified,
            })
        }
        CommitMatch::Ambiguous(detail) => bail!(
            "cannot derive commit correspondence unambiguously: {detail}; \
             write an explicit {CORRESPONDENCE_SCHEMA} file and pass --correspondence <FILE>"
        ),
        CommitMatch::None(commit_reason) => {
            // Attempt 2: single-commit squash match.
            match derive_squash_mode(cwd, subject, &sources, &landed_commits)? {
                SquashMatch::Unique(landed) => {
                    let pairs = sources
                        .iter()
                        .map(|source| CorrespondedCommit {
                            source: source.clone(),
                            landed: landed.clone(),
                        })
                        .collect();
                    let file = File {
                        schema: CORRESPONDENCE_SCHEMA.into(),
                        mode: Mode::Squash,
                        change: subject.change_id.into(),
                        patchset: subject.patchset_id.into(),
                        source_head: subject.head.into(),
                        target_before: subject.target_before.into(),
                        revision: subject.revision.into(),
                        commits: pairs,
                    };
                    let verified = check_file(cwd, &file, subject)?;
                    Ok(Derived {
                        mode: Mode::Squash,
                        pairs: verified,
                    })
                }
                SquashMatch::Ambiguous(candidates) => bail!(
                    "cannot derive squash correspondence unambiguously: multiple commits in \
                     {}..{} match the combined diff of {} ({}); write an explicit \
                     {CORRESPONDENCE_SCHEMA} file and pass --correspondence <FILE>",
                    subject.target_before,
                    subject.revision,
                    subject.patchset_id,
                    candidates.join(", ")
                ),
                SquashMatch::None => bail!(
                    "could not derive correspondence for {} in {}..{}: {commit_reason}, and no \
                     single commit matches the combined patch or tree of {}..{}; supply an \
                     explicit {CORRESPONDENCE_SCHEMA} file with --correspondence <FILE>",
                    subject.patchset_id,
                    subject.target_before,
                    subject.revision,
                    subject.base,
                    subject.head
                ),
            }
        }
    }
}

enum CommitMatch {
    Unique(Vec<CorrespondedCommit>),
    Ambiguous(String),
    None(String),
}

enum SquashMatch {
    Unique(String),
    Ambiguous(Vec<String>),
    None,
}

fn derive_commit_mode(
    cwd: &Path,
    sources: &[String],
    landed_commits: &[String],
) -> Result<CommitMatch> {
    let mut source_patches = Vec::with_capacity(sources.len());
    for source in sources {
        match ordinary(cwd, source, "source") {
            Ok(patch) => source_patches.push(patch),
            Err(err) => return Ok(CommitMatch::None(err.to_string())),
        }
    }

    let mut landed_ordinary: Vec<(usize, Option<String>)> = Vec::new();
    for (idx, landed) in landed_commits.iter().enumerate() {
        if gitio::commit_parents(cwd, landed)?.len() > 1 {
            continue;
        }
        if let Some(patch) = gitio::patch_id(cwd, landed)? {
            landed_ordinary.push((idx, Some(patch)));
        }
    }

    let mut candidates: Vec<Vec<usize>> = Vec::with_capacity(sources.len());
    for (source, source_patch) in sources.iter().zip(&source_patches) {
        let mut matched = Vec::new();
        for (idx, landed_patch) in &landed_ordinary {
            let landed = &landed_commits[*idx];
            if landed_patch.as_deref() == Some(source_patch.as_str())
                || same_parent_and_result_trees(cwd, source, landed)?
            {
                matched.push(*idx);
            }
        }
        if matched.is_empty() {
            return Ok(CommitMatch::None(format!(
                "source commit {source} has no patch-equal commit in target range"
            )));
        }
        candidates.push(matched);
    }

    let mut solutions: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::with_capacity(sources.len());
    find_ordered_assignments(
        cwd,
        landed_commits,
        &candidates,
        0,
        &mut current,
        &mut solutions,
    )?;

    match solutions.len() {
        0 => Ok(CommitMatch::None(
            "matching landed commits do not preserve source ancestry order".into(),
        )),
        1 => {
            let chosen = &solutions[0];
            let pairs = sources
                .iter()
                .zip(chosen.iter())
                .map(|(source, &idx)| CorrespondedCommit {
                    source: source.clone(),
                    landed: landed_commits[idx].clone(),
                })
                .collect();
            Ok(CommitMatch::Unique(pairs))
        }
        _ => {
            let describe = |sol: &[usize]| {
                sol.iter()
                    .map(|&i| &landed_commits[i][..landed_commits[i].len().min(12)])
                    .collect::<Vec<_>>()
                    .join(" -> ")
            };
            Ok(CommitMatch::Ambiguous(format!(
                "multiple order-preserving sequences match (e.g. [{}] vs [{}])",
                describe(&solutions[0]),
                describe(&solutions[1])
            )))
        }
    }
}

fn find_ordered_assignments(
    cwd: &Path,
    landed_commits: &[String],
    candidates: &[Vec<usize>],
    depth: usize,
    current: &mut Vec<usize>,
    solutions: &mut Vec<Vec<usize>>,
) -> Result<()> {
    if solutions.len() >= 2 {
        return Ok(());
    }
    if depth == candidates.len() {
        solutions.push(current.clone());
        return Ok(());
    }
    let previous = current.last().copied();
    for &idx in &candidates[depth] {
        if current.contains(&idx) {
            continue;
        }
        if let Some(prev_idx) = previous {
            if idx <= prev_idx
                || !gitio::is_ancestor(cwd, &landed_commits[prev_idx], &landed_commits[idx])?
            {
                continue;
            }
        }
        current.push(idx);
        find_ordered_assignments(
            cwd,
            landed_commits,
            candidates,
            depth + 1,
            current,
            solutions,
        )?;
        current.pop();
        if solutions.len() >= 2 {
            return Ok(());
        }
    }
    Ok(())
}

fn derive_squash_mode(
    cwd: &Path,
    subject: &Subject,
    _sources: &[String],
    landed_commits: &[String],
) -> Result<SquashMatch> {
    let base_tree = gitio::commit_tree(cwd, subject.base)?;
    let head_tree = gitio::commit_tree(cwd, subject.head)?;
    if base_tree == head_tree {
        return Ok(SquashMatch::None);
    }
    let combined_patch = gitio::range_patch_id(cwd, subject.base, subject.head)?;

    let mut matched = Vec::new();
    for landed in landed_commits {
        let parents = gitio::commit_parents(cwd, landed)?;
        if parents.len() > 1 {
            continue;
        }
        let patch_matches =
            combined_patch.is_some() && gitio::patch_id(cwd, landed)? == combined_patch;
        let tree_matches = if let [parent] = parents.as_slice() {
            gitio::commit_tree(cwd, parent)? == base_tree
                && gitio::commit_tree(cwd, landed)? == head_tree
        } else {
            false
        };
        if patch_matches || tree_matches {
            matched.push(landed.clone());
        }
    }
    match matched.len() {
        0 => Ok(SquashMatch::None),
        1 => Ok(SquashMatch::Unique(matched.remove(0))),
        _ => Ok(SquashMatch::Ambiguous(matched)),
    }
}

fn check_file(cwd: &Path, file: &File, subject: &Subject) -> Result<Vec<CorrespondedCommit>> {
    if file.schema != CORRESPONDENCE_SCHEMA {
        bail!(
            "correspondence schema is {:?}; this arc reads {CORRESPONDENCE_SCHEMA}",
            file.schema
        );
    }
    let commit = |name: &str, field: &str| {
        gitio::rev_parse(cwd, name)
            .with_context(|| format!("correspondence {field} {name} is not a commit here"))
    };
    if file.change != subject.change_id {
        bail!(
            "correspondence is about change {}, not {}",
            file.change,
            subject.change_id
        );
    }
    if file.patchset != subject.patchset_id {
        bail!(
            "correspondence is about patchset {}, not {}",
            file.patchset,
            subject.patchset_id
        );
    }
    for (field, named, expected) in [
        ("source_head", &file.source_head, subject.head),
        ("target_before", &file.target_before, subject.target_before),
        ("revision", &file.revision, subject.revision),
    ] {
        let resolved = commit(named, field)?;
        if resolved != expected {
            bail!("correspondence {field} is {resolved}, not {expected}");
        }
    }

    let sources = gitio::range_commits(cwd, subject.base, subject.head)?;
    if sources.is_empty() {
        bail!(
            "{} has no commits, so there is nothing to correspond",
            subject.patchset_id
        );
    }
    let landed_range: BTreeSet<String> =
        gitio::range_commits(cwd, subject.target_before, subject.revision)?
            .into_iter()
            .collect();

    match file.mode {
        Mode::Commit => check_commit_mode(cwd, file, subject, &sources, &landed_range),
        Mode::Squash => check_squash_mode(cwd, file, subject, &sources, &landed_range),
    }
}

fn check_commit_mode(
    cwd: &Path,
    file: &File,
    subject: &Subject,
    sources: &[String],
    landed_range: &BTreeSet<String>,
) -> Result<Vec<CorrespondedCommit>> {
    let commit = |name: &str, field: &str| {
        gitio::rev_parse(cwd, name)
            .with_context(|| format!("correspondence {field} {name} is not a commit here"))
    };
    let mut source_patches = BTreeMap::new();
    for source in sources {
        source_patches.insert(source.as_str(), ordinary(cwd, source, "source")?);
    }

    let mut by_source: BTreeMap<String, String> = BTreeMap::new();
    let mut seen_landed = BTreeSet::new();
    for pair in &file.commits {
        let source = commit(&pair.source, "source")?;
        let landed = commit(&pair.landed, "landed")?;
        if !sources.contains(&source) {
            bail!(
                "source commit {source} is not a commit of {}: a correspondence maps only the \
                 patchset's own commits",
                subject.patchset_id
            );
        }
        if !seen_landed.insert(landed.clone()) {
            bail!("landed commit {landed} is mapped twice: a correspondence is one to one");
        }
        if by_source.insert(source.clone(), landed).is_some() {
            bail!("source commit {source} is mapped twice: a correspondence is one to one");
        }
    }
    let mut pairs = Vec::with_capacity(sources.len());
    let mut previous: Option<(&str, String)> = None;
    for source in sources {
        let Some(landed) = by_source.get(source) else {
            bail!(
                "source commit {source} is unmapped: a correspondence maps every commit of {}",
                subject.patchset_id
            );
        };
        if !landed_range.contains(landed) {
            bail!(
                "landed commit {landed} (for source {source}) is outside {}..{}: a \
                 correspondence names only commits the asserted revision introduced",
                subject.target_before,
                subject.revision
            );
        }
        let landed_patch = ordinary(cwd, landed, "landed")?;
        if let Some((earlier_source, earlier_landed)) = &previous {
            if !gitio::is_ancestor(cwd, earlier_landed, landed)? {
                bail!(
                    "landed commit {landed} (for source {source}) does not follow {earlier_landed} \
                     (for source {earlier_source}): landed commits keep source order"
                );
            }
        }
        let source_patch = &source_patches[source.as_str()];
        if *source_patch != landed_patch && !same_parent_and_result_trees(cwd, source, landed)? {
            bail!(
                "landed commit {landed} does not carry the patch of source commit {source} \
                 (patch ids {landed_patch} and {source_patch}): each pair is patch-equal"
            );
        }
        pairs.push(CorrespondedCommit {
            source: source.clone(),
            landed: landed.clone(),
        });
        previous = Some((source, landed.clone()));
    }
    Ok(pairs)
}

fn check_squash_mode(
    cwd: &Path,
    file: &File,
    subject: &Subject,
    sources: &[String],
    landed_range: &BTreeSet<String>,
) -> Result<Vec<CorrespondedCommit>> {
    let commit = |name: &str, field: &str| {
        gitio::rev_parse(cwd, name)
            .with_context(|| format!("correspondence {field} {name} is not a commit here"))
    };
    let mut by_source: BTreeMap<String, String> = BTreeMap::new();
    let mut single_landed: Option<String> = None;
    for pair in &file.commits {
        let source = commit(&pair.source, "source")?;
        let landed = commit(&pair.landed, "landed")?;
        if !sources.contains(&source) {
            bail!(
                "source commit {source} is not a commit of {}: a correspondence maps only the \
                 patchset's own commits",
                subject.patchset_id
            );
        }
        if by_source.insert(source.clone(), landed.clone()).is_some() {
            bail!("source commit {source} is mapped twice: each source commit is mapped once");
        }
        match &single_landed {
            None => single_landed = Some(landed),
            Some(existing) if existing == &landed => {}
            Some(existing) => bail!(
                "squash correspondence names multiple landed commits ({existing} and {landed}): \
                 every source commit must map to one squashed commit"
            ),
        }
    }
    for source in sources {
        if !by_source.contains_key(source) {
            bail!(
                "source commit {source} is unmapped: a squash correspondence maps every commit of {}",
                subject.patchset_id
            );
        }
    }
    let landed = single_landed.context("squash correspondence has no commits")?;
    if !landed_range.contains(&landed) {
        bail!(
            "landed commit {landed} is outside {}..{}: a correspondence names only commits the \
             asserted revision introduced",
            subject.target_before,
            subject.revision
        );
    }
    let landed_patch = ordinary(cwd, &landed, "landed")?;
    let base_tree = gitio::commit_tree(cwd, subject.base)?;
    let head_tree = gitio::commit_tree(cwd, subject.head)?;
    if base_tree == head_tree {
        bail!(
            "{} changes nothing between {} and {}, so it has no combined patch to correspond",
            subject.patchset_id,
            subject.base,
            subject.head
        );
    }
    let combined_patch = gitio::range_patch_id(cwd, subject.base, subject.head)?;
    let patch_matches = combined_patch.as_deref() == Some(landed_patch.as_str());
    let parents = gitio::commit_parents(cwd, &landed)?;
    let tree_matches = if let [parent] = parents.as_slice() {
        gitio::commit_tree(cwd, parent)? == base_tree
            && gitio::commit_tree(cwd, &landed)? == head_tree
    } else {
        false
    };
    if !patch_matches && !tree_matches {
        bail!(
            "landed commit {landed} does not match the combined patch or tree of {} ({}..{})",
            subject.patchset_id,
            subject.base,
            subject.head
        );
    }
    Ok(sources
        .iter()
        .map(|source| CorrespondedCommit {
            source: source.clone(),
            landed: landed.clone(),
        })
        .collect())
}

fn same_parent_and_result_trees(cwd: &Path, source: &str, landed: &str) -> Result<bool> {
    let source_parents = gitio::commit_parents(cwd, source)?;
    let landed_parents = gitio::commit_parents(cwd, landed)?;
    let ([sp], [lp]) = (source_parents.as_slice(), landed_parents.as_slice()) else {
        return Ok(false);
    };
    let sp_tree = gitio::commit_tree(cwd, sp)?;
    let s_tree = gitio::commit_tree(cwd, source)?;
    if sp_tree == s_tree {
        return Ok(false);
    }
    Ok(gitio::commit_tree(cwd, lp)? == sp_tree && gitio::commit_tree(cwd, landed)? == s_tree)
}

/// A commit's patch id, refusing one that has no ordinary patch identity: a
/// merge, or one that changes nothing.
fn ordinary(cwd: &Path, commit: &str, side: &str) -> Result<String> {
    if gitio::commit_parents(cwd, commit)?.len() > 1 {
        bail!(
            "{side} commit {commit} is a merge, which has no ordinary patch identity to \
             correspond"
        );
    }
    gitio::patch_id(cwd, commit)?.with_context(|| {
        format!("{side} commit {commit} changes nothing, so it has no patch identity to correspond")
    })
}
