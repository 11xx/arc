//! Review extents: what a review obligation requires read, and what a read
//! covered, as typed data resolved against one subject.
//!
//! An extent names its subject domain (the candidate tree at a revision, or
//! the diff from a base to a revision) and, when scoped, the exact entries it
//! covers: each a path with its blob at the revision, or a deletion with its
//! blob at the base. A glob is input only; it resolves to entries before
//! anything is written, so an extent never depends on a later reading of the
//! tree.
//!
//! [`covers`] is the one coverage rule. It compares two extents and reads
//! nothing else: the subject first, then the entries.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// What a review obligation requires read, or what a read covered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extent {
    #[serde(flatten)]
    pub subject: ExtentSubject,
    /// The entries a scoped extent covers. Absent, the extent is the whole
    /// subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ExtentScope>,
}

/// The subject an extent is resolved against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "domain", rename_all = "kebab-case")]
pub enum ExtentSubject {
    /// The candidate tree at `revision`.
    Tree { revision: String, tree: String },
    /// The change from `base` to `revision`.
    Diff { base: String, revision: String },
}

/// The entries a scoped extent covers, and those it expressly excludes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtentScope {
    pub entries: Vec<ExtentEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusions: Vec<ExtentEntry>,
}

/// One path of a scoped extent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExtentEntry {
    pub path: String,
    #[serde(flatten)]
    pub content: EntryContent,
}

/// What an entry names at its path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryContent {
    /// The blob at the extent's revision.
    Blob(String),
    /// A deletion: the blob at the base, with none at the revision.
    Deleted(String),
}

impl ExtentSubject {
    pub fn domain(&self) -> Domain {
        match self {
            ExtentSubject::Tree { .. } => Domain::Tree,
            ExtentSubject::Diff { .. } => Domain::Diff,
        }
    }

    pub fn revision(&self) -> &str {
        match self {
            ExtentSubject::Tree { revision, .. } | ExtentSubject::Diff { revision, .. } => revision,
        }
    }

    pub fn base(&self) -> Option<&str> {
        match self {
            ExtentSubject::Tree { .. } => None,
            ExtentSubject::Diff { base, .. } => Some(base),
        }
    }

    /// Every revision this subject names, for following recorded rewrites.
    pub fn revisions_mut(&mut self) -> Vec<&mut String> {
        match self {
            ExtentSubject::Tree { revision, .. } => vec![revision],
            ExtentSubject::Diff { base, revision } => vec![base, revision],
        }
    }
}

impl Extent {
    pub fn is_whole(&self) -> bool {
        self.scope.is_none()
    }

    /// One line naming the extent: its domain, subject and scope.
    pub fn label(&self) -> String {
        let short = |revision: &str| revision.chars().take(8).collect::<String>();
        let subject = match &self.subject {
            ExtentSubject::Tree { revision, .. } => format!("tree at {}", short(revision)),
            ExtentSubject::Diff { base, revision } => {
                format!("diff {}..{}", short(base), short(revision))
            }
        };
        match &self.scope {
            None => format!("{subject}, whole"),
            Some(scope) if scope.exclusions.is_empty() => format!(
                "{subject}, scoped to {} entr{}",
                scope.entries.len(),
                if scope.entries.len() == 1 { "y" } else { "ies" }
            ),
            Some(scope) => format!(
                "{subject}, scoped to {} entr{} excluding {}",
                scope.entries.len(),
                if scope.entries.len() == 1 { "y" } else { "ies" },
                scope.exclusions.len()
            ),
        }
    }
}

/// An extent's subject domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Domain {
    /// The change's diff from its base to its head.
    #[default]
    Diff,
    /// The candidate tree at the change's head.
    Tree,
}

impl Domain {
    pub fn as_str(self) -> &'static str {
        match self {
            Domain::Diff => "diff",
            Domain::Tree => "tree",
        }
    }
}

/// The one value `--extent` takes: the whole subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum WholeExtent {
    /// The whole subject: every entry of the domain
    Whole,
}

/// What an obligation requires, as [`covers`] compares a read against it.
#[derive(Debug, Clone, Copy)]
pub enum Obligation<'a> {
    /// The obligation recorded an extent.
    Extent(&'a Extent),
    /// The obligation recorded none; it reviews the subject that shipped,
    /// from `base` to `revision`, with its scope not established.
    Unestablished { base: &'a str, revision: &'a str },
}

/// Whether a read covers an obligation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// The read's extent covers the obligation's.
    Covered,
    /// Neither the read nor the obligation recorded an extent, so coverage
    /// is not established and the rule without extents applies.
    NotEstablished,
    /// The read does not cover the obligation.
    Gap(CoverageGap),
}

/// Why a read does not cover an obligation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverageGap {
    /// The obligation recorded an extent and the read recorded none.
    ReadUnestablished,
    /// The read is scoped and the obligation recorded no extent, so nothing
    /// shows the scope is what the obligation requires.
    ObligationUnestablished,
    /// The read and the obligation read different domains.
    Domain { read: Domain, obligation: Domain },
    /// The read's diff starts from another base.
    Base,
    /// The read is of another revision.
    Revision,
    /// A scoped read cannot cover a whole obligation.
    ScopedRead,
    /// Obligation entries the read expressly excluded.
    Excluded(Vec<String>),
    /// Obligation entries the read does not list at the same blob.
    Missing(Vec<String>),
}

impl CoverageGap {
    /// The gap's stable name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadUnestablished => "read-extent-not-established",
            Self::ObligationUnestablished => "obligation-extent-not-established",
            Self::Domain { .. } => "domain-mismatch",
            Self::Base => "base-mismatch",
            Self::Revision => "revision-mismatch",
            Self::ScopedRead => "scoped-read",
            Self::Excluded(_) => "entries-excluded",
            Self::Missing(_) => "entries-missing",
        }
    }

    /// The obligation paths the gap names, when it names any.
    pub fn paths(&self) -> &[String] {
        match self {
            Self::Excluded(paths) | Self::Missing(paths) => paths,
            _ => &[],
        }
    }

    /// Why the read does not cover the obligation, phrased to follow "it".
    pub fn reason(&self) -> String {
        let list = |paths: &[String]| {
            const SHOWN: usize = 5;
            let mut named = paths
                .iter()
                .take(SHOWN)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            if paths.len() > SHOWN {
                named.push_str(&format!(" and {} more", paths.len() - SHOWN));
            }
            named
        };
        match self {
            Self::ReadUnestablished => {
                "records no extent, and the obligation names one: coverage is not established"
                    .into()
            }
            Self::ObligationUnestablished => "is scoped, and the obligation names no extent for \
                                               a scope to cover"
                .into(),
            Self::Domain { read, obligation } => format!(
                "reads the {} domain; the obligation is of the {}",
                read.as_str(),
                obligation.as_str()
            ),
            Self::Base => "reads a diff from another base".into(),
            Self::Revision => "reads another revision".into(),
            Self::ScopedRead => "is scoped, and the obligation is whole".into(),
            Self::Excluded(paths) => format!("excludes {}", list(paths)),
            Self::Missing(paths) => format!("does not cover {}", list(paths)),
        }
    }
}

/// Whether `read` covers `obligation`.
///
/// First the subject: the same domain, the same base for a diff, and the
/// same revision, where both revisions have been followed through the
/// signature-only and role-addition rewrites an approval follows. Then the
/// entries: a whole read covers any scope of its subject; a scoped read
/// covers a scoped obligation when it lists every obligation entry at the
/// same blob, deletions included, and excludes none of them. A scoped read
/// never covers a whole obligation, and several reads are never combined.
pub fn covers(read: Option<&Extent>, obligation: Obligation<'_>) -> Coverage {
    let read = match (read, obligation) {
        (None, Obligation::Unestablished { .. }) => return Coverage::NotEstablished,
        (None, Obligation::Extent(_)) => return Coverage::Gap(CoverageGap::ReadUnestablished),
        (Some(read), _) => read,
    };
    let required = match obligation {
        Obligation::Extent(required) => required,
        Obligation::Unestablished { base, revision } => {
            if !read.is_whole() {
                return Coverage::Gap(CoverageGap::ObligationUnestablished);
            }
            if read
                .subject
                .base()
                .is_some_and(|read_base| read_base != base)
            {
                return Coverage::Gap(CoverageGap::Base);
            }
            if read.subject.revision() != revision {
                return Coverage::Gap(CoverageGap::Revision);
            }
            return Coverage::Covered;
        }
    };
    if read.subject.domain() != required.subject.domain() {
        return Coverage::Gap(CoverageGap::Domain {
            read: read.subject.domain(),
            obligation: required.subject.domain(),
        });
    }
    if read.subject.base() != required.subject.base() {
        return Coverage::Gap(CoverageGap::Base);
    }
    if read.subject != required.subject {
        return Coverage::Gap(CoverageGap::Revision);
    }
    let (read_scope, required_scope) = match (&read.scope, &required.scope) {
        (None, _) => return Coverage::Covered,
        (Some(_), None) => return Coverage::Gap(CoverageGap::ScopedRead),
        (Some(read_scope), Some(required_scope)) => (read_scope, required_scope),
    };
    let excluded: Vec<String> = required_scope
        .entries
        .iter()
        .filter(|entry| {
            read_scope
                .exclusions
                .iter()
                .any(|exclusion| exclusion.path == entry.path)
        })
        .map(|entry| entry.path.clone())
        .collect();
    if !excluded.is_empty() {
        return Coverage::Gap(CoverageGap::Excluded(excluded));
    }
    let missing: Vec<String> = required_scope
        .entries
        .iter()
        .filter(|entry| !read_scope.entries.contains(entry))
        .map(|entry| entry.path.clone())
        .collect();
    if !missing.is_empty() {
        return Coverage::Gap(CoverageGap::Missing(missing));
    }
    Coverage::Covered
}

/// An extent as a caller states it: a domain and either the whole subject
/// or path globs, which [`resolve`] turns into entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtentInput {
    pub domain: Domain,
    pub scope: InputScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputScope {
    Whole,
    Paths {
        include: Vec<String>,
        exclude: Vec<String>,
    },
}

impl ExtentInput {
    /// The input the command-line flags state, or `None` when they state
    /// none.
    pub fn from_flags(
        whole: bool,
        include: Vec<String>,
        exclude: Vec<String>,
        domain: Option<Domain>,
    ) -> Result<Option<ExtentInput>> {
        let scope = match (whole, include.is_empty()) {
            (true, true) if exclude.is_empty() => InputScope::Whole,
            (true, _) => bail!(
                "--extent whole names the whole subject; scope a read with --extent-paths \
                 and --extent-exclude instead"
            ),
            (false, false) => InputScope::Paths { include, exclude },
            (false, true) if !exclude.is_empty() => {
                bail!("--extent-exclude narrows --extent-paths, which names none")
            }
            (false, true) if domain.is_some() => {
                bail!("--extent-domain needs --extent whole or --extent-paths")
            }
            (false, true) => return Ok(None),
        };
        Ok(Some(ExtentInput {
            domain: domain.unwrap_or_default(),
            scope,
        }))
    }
}

/// `input` resolved against the subject from `base` to `head`: the tree at
/// `head`, or the diff from `base` to `head`, with every glob resolved to
/// the entries it matches. A glob that matches nothing, or a scope left with
/// no entries, is refused rather than recorded as a claim about nothing.
pub fn resolve(cwd: &Path, input: &ExtentInput, base: &str, head: &str) -> Result<Extent> {
    let subject = match input.domain {
        Domain::Tree => ExtentSubject::Tree {
            revision: head.to_string(),
            tree: crate::gitio::commit_tree(cwd, head)
                .with_context(|| format!("cannot read the tree of {head}"))?,
        },
        Domain::Diff => ExtentSubject::Diff {
            base: base.to_string(),
            revision: head.to_string(),
        },
    };
    let (include, exclude) = match &input.scope {
        InputScope::Whole => {
            return Ok(Extent {
                subject,
                scope: None,
            })
        }
        InputScope::Paths { include, exclude } => (include, exclude),
    };
    let available = match input.domain {
        Domain::Tree => tree_entries(cwd, head)?,
        Domain::Diff => diff_entries(cwd, base, head)?,
    };
    let subject_name = match input.domain {
        Domain::Tree => format!("the tree at {head}"),
        Domain::Diff => format!("the diff {base}..{head}"),
    };
    let matching = |pattern: &String| -> Result<Vec<&ExtentEntry>> {
        let hits: Vec<&ExtentEntry> = available
            .iter()
            .filter(|entry| crate::policy::glob_match(pattern, &entry.path))
            .collect();
        if hits.is_empty() {
            bail!("extent path {pattern:?} matches no entry of {subject_name}");
        }
        Ok(hits)
    };
    let mut entries = BTreeMap::new();
    for pattern in include {
        for entry in matching(pattern)? {
            entries.insert(entry.path.clone(), entry.clone());
        }
    }
    let mut exclusions = BTreeMap::new();
    for pattern in exclude {
        for entry in matching(pattern)? {
            exclusions.insert(entry.path.clone(), entry.clone());
        }
    }
    entries.retain(|path, _| !exclusions.contains_key(path));
    if entries.is_empty() {
        bail!("the extent excludes every entry it names in {subject_name}");
    }
    Ok(Extent {
        subject,
        scope: Some(ExtentScope {
            entries: entries.into_values().collect(),
            exclusions: exclusions.into_values().collect(),
        }),
    })
}

/// Every blob in the tree at `revision`, with its path.
fn tree_entries(cwd: &Path, revision: &str) -> Result<Vec<ExtentEntry>> {
    let listing = crate::gitio::git(cwd, &["ls-tree", "-r", "-z", "--full-tree", revision])?;
    let mut entries = Vec::new();
    for record in listing.split('\0').filter(|record| !record.is_empty()) {
        let (meta, path) = record
            .split_once('\t')
            .with_context(|| format!("unreadable ls-tree record {record:?}"))?;
        let oid = meta
            .split_whitespace()
            .nth(2)
            .with_context(|| format!("unreadable ls-tree record {record:?}"))?;
        entries.push(ExtentEntry {
            path: path.to_string(),
            content: EntryContent::Blob(oid.to_string()),
        });
    }
    Ok(entries)
}

/// Every path the diff from `base` to `revision` changes: its blob at
/// `revision`, or its blob at `base` when the diff deletes it.
fn diff_entries(cwd: &Path, base: &str, revision: &str) -> Result<Vec<ExtentEntry>> {
    let listing = crate::gitio::git(
        cwd,
        &["diff-tree", "-r", "-z", "--no-renames", base, revision],
    )?;
    let mut fields = listing.split('\0').filter(|field| !field.is_empty());
    let mut entries = Vec::new();
    while let Some(meta) = fields.next() {
        let path = fields
            .next()
            .with_context(|| format!("unreadable diff-tree record {meta:?}"))?;
        let parts: Vec<&str> = meta.trim_start_matches(':').split_whitespace().collect();
        let [_, _, before, after, status] = parts[..] else {
            bail!("unreadable diff-tree record {meta:?}");
        };
        let content = if status.starts_with('D') {
            EntryContent::Deleted(before.to_string())
        } else {
            EntryContent::Blob(after.to_string())
        };
        entries.push(ExtentEntry {
            path: path.to_string(),
            content,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(path: &str, oid: &str) -> ExtentEntry {
        ExtentEntry {
            path: path.into(),
            content: EntryContent::Blob(oid.into()),
        }
    }

    fn deleted(path: &str, oid: &str) -> ExtentEntry {
        ExtentEntry {
            path: path.into(),
            content: EntryContent::Deleted(oid.into()),
        }
    }

    fn diff(base: &str, entries: Option<Vec<ExtentEntry>>, exclusions: Vec<ExtentEntry>) -> Extent {
        Extent {
            subject: ExtentSubject::Diff {
                base: base.into(),
                revision: "r1".into(),
            },
            scope: entries.map(|entries| ExtentScope {
                entries,
                exclusions,
            }),
        }
    }

    /// Claim: an extent's stored shape names its domain, subject, and each
    /// entry's blob or deletion. Rejects a shape a reader cannot tell a
    /// deletion from a blob in.
    #[test]
    fn an_extent_serializes_its_domain_and_entries() {
        let extent = diff(
            "b0",
            Some(vec![blob("sdk/a", "aa"), deleted("sdk/old", "dd")]),
            vec![blob("sdk/gen", "gg")],
        );
        let value = serde_json::to_value(&extent).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "domain": "diff", "base": "b0", "revision": "r1",
                "scope": {
                    "entries": [{"path": "sdk/a", "blob": "aa"}, {"path": "sdk/old", "deleted": "dd"}],
                    "exclusions": [{"path": "sdk/gen", "blob": "gg"}],
                },
            })
        );
        assert_eq!(serde_json::from_value::<Extent>(value).unwrap(), extent);
    }

    /// Claim: the same final path and blob inventory from a different base
    /// is a different subject. Rejects coverage decided by entries alone.
    #[test]
    fn identical_entries_from_another_base_do_not_cover() {
        let entries = vec![blob("p", "aa")];
        let obligation = diff("b0", Some(entries.clone()), Vec::new());
        let read = diff("b1", Some(entries), Vec::new());
        assert_eq!(
            covers(Some(&read), Obligation::Extent(&obligation)),
            Coverage::Gap(CoverageGap::Base)
        );
        let whole = diff("b1", None, Vec::new());
        assert_eq!(
            covers(Some(&whole), Obligation::Extent(&obligation)),
            Coverage::Gap(CoverageGap::Base)
        );
    }

    /// Claim: a deletion in the obligation is covered only by a read listing
    /// that deletion. Rejects comparing blobs at the revision alone.
    #[test]
    fn a_deletion_is_covered_only_by_a_read_listing_it() {
        let obligation = diff(
            "b0",
            Some(vec![blob("a", "aa"), deleted("old", "dd")]),
            Vec::new(),
        );
        let without = diff("b0", Some(vec![blob("a", "aa")]), Vec::new());
        assert_eq!(
            covers(Some(&without), Obligation::Extent(&obligation)),
            Coverage::Gap(CoverageGap::Missing(vec!["old".into()]))
        );
        let with = diff(
            "b0",
            Some(vec![blob("a", "aa"), deleted("old", "dd")]),
            Vec::new(),
        );
        assert_eq!(
            covers(Some(&with), Obligation::Extent(&obligation)),
            Coverage::Covered
        );
    }

    /// Claim: a tree read never covers a diff obligation, nor the reverse,
    /// whole or scoped. Rejects a whole read covering across domains.
    #[test]
    fn a_tree_read_never_covers_a_diff_obligation() {
        let tree = Extent {
            subject: ExtentSubject::Tree {
                revision: "r1".into(),
                tree: "t1".into(),
            },
            scope: None,
        };
        let whole_diff = diff("b0", None, Vec::new());
        assert!(matches!(
            covers(Some(&tree), Obligation::Extent(&whole_diff)),
            Coverage::Gap(CoverageGap::Domain { .. })
        ));
        assert!(matches!(
            covers(Some(&whole_diff), Obligation::Extent(&tree)),
            Coverage::Gap(CoverageGap::Domain { .. })
        ));
    }

    /// Claim: a whole read covers any scope of its subject, a scoped read
    /// never covers a whole obligation, a scoped read covers a scoped one
    /// only entry for entry at the same blob, and an exclusion is never
    /// covered. Rejects a union, a stale blob and an excluded entry counting.
    #[test]
    fn scope_containment_is_entry_for_entry() {
        let scoped = diff(
            "b0",
            Some(vec![blob("sdk/a", "aa"), blob("sdk/b", "bb")]),
            Vec::new(),
        );
        let whole = diff("b0", None, Vec::new());
        assert_eq!(
            covers(Some(&whole), Obligation::Extent(&scoped)),
            Coverage::Covered
        );
        assert_eq!(
            covers(Some(&scoped), Obligation::Extent(&whole)),
            Coverage::Gap(CoverageGap::ScopedRead)
        );
        let stale = diff(
            "b0",
            Some(vec![blob("sdk/a", "aa"), blob("sdk/b", "b2")]),
            Vec::new(),
        );
        assert_eq!(
            covers(Some(&stale), Obligation::Extent(&scoped)),
            Coverage::Gap(CoverageGap::Missing(vec!["sdk/b".into()]))
        );
        let excluding = diff(
            "b0",
            Some(vec![blob("sdk/a", "aa")]),
            vec![blob("sdk/b", "bb")],
        );
        assert_eq!(
            covers(Some(&excluding), Obligation::Extent(&scoped)),
            Coverage::Gap(CoverageGap::Excluded(vec!["sdk/b".into()]))
        );
        let wider = diff(
            "b0",
            Some(vec![
                blob("sdk/a", "aa"),
                blob("sdk/b", "bb"),
                blob("x", "xx"),
            ]),
            Vec::new(),
        );
        assert_eq!(
            covers(Some(&wider), Obligation::Extent(&scoped)),
            Coverage::Covered
        );
    }

    /// Claim: an obligation with an extent is never covered by a read
    /// without one; two without keep the rule without extents; a whole read
    /// of the shipped subject covers an obligation with none, and a scoped
    /// read does not. Rejects an absent extent reading as whole.
    #[test]
    fn absent_extents_cover_only_each_other() {
        let whole = diff("b0", None, Vec::new());
        assert_eq!(
            covers(None, Obligation::Extent(&whole)),
            Coverage::Gap(CoverageGap::ReadUnestablished)
        );
        let shipped = Obligation::Unestablished {
            base: "b0",
            revision: "r1",
        };
        assert_eq!(covers(None, shipped), Coverage::NotEstablished);
        assert_eq!(covers(Some(&whole), shipped), Coverage::Covered);
        let scoped = diff("b0", Some(vec![blob("a", "aa")]), Vec::new());
        assert_eq!(
            covers(Some(&scoped), shipped),
            Coverage::Gap(CoverageGap::ObligationUnestablished)
        );
        let elsewhere = Obligation::Unestablished {
            base: "b0",
            revision: "r2",
        };
        assert_eq!(
            covers(Some(&whole), elsewhere),
            Coverage::Gap(CoverageGap::Revision)
        );
    }
}
