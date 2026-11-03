//! An asserted commit-for-commit correspondence: the proof that stands in for
//! ancestry when a change's commits reached its target as other commits that
//! arc did not produce (a cherry-pick, a rebase elsewhere).
//!
//! The file binds the change, its patchset and source head, the revision the
//! target stood at before (`target_before`) and the asserted revision. Every
//! source commit maps to exactly one landed commit in
//! `target_before..revision`, the landed commits follow source order along
//! that range, and each pair has the same stable patch id.
use crate::gitio;
use crate::model::CorrespondedCommit;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const CORRESPONDENCE_SCHEMA: &str = "arc-correspondence/1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    schema: String,
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

/// Read the file at `path` and check it against `subject`, returning the
/// pairs in source order with every name resolved to its full object id.
pub fn check(cwd: &Path, path: &Path, subject: &Subject) -> Result<Vec<CorrespondedCommit>> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("cannot read correspondence {}", path.display()))?;
    let file: File = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not an {CORRESPONDENCE_SCHEMA} file", path.display()))?;
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
    let mut source_patches = BTreeMap::new();
    for source in &sources {
        source_patches.insert(source.as_str(), ordinary(cwd, source, "source")?);
    }
    let landed_range: BTreeSet<String> =
        gitio::range_commits(cwd, subject.target_before, subject.revision)?
            .into_iter()
            .collect();

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
    for source in &sources {
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
        if *source_patch != landed_patch {
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
