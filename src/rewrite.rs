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
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What became of a recorded revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fate {
    /// It survives at this revision, after following every recorded rewrite.
    Rewritten(String),
    /// A rewrite dropped it. Nothing survives it, and saying so is the point:
    /// "unchanged" and "gone" are different answers.
    Dropped,
}

fn describe(fate: &Option<String>) -> String {
    match fate {
        Some(revision) => revision.clone(),
        None => "dropped".to_string(),
    }
}

/// One resolution step. Ambiguity is not a variant: an abbreviation matching
/// two recorded revisions is a question the map cannot answer, and answering
/// "nothing rewrote it" would be a wrong answer rather than no answer.
enum Step {
    Unmapped,
    Mapped { key: String, fate: Option<String> },
}

/// The shortest abbreviation that may stand for a revision.
///
/// Seven hex digits is what Git itself considers abbreviated, and short of it
/// a prefix stops naming a commit: six digits collide across sixteen million
/// objects, and one names a sixteenth of every object in the repository.
pub(crate) const ABBREVIATION_FLOOR: usize = 7;

/// A revision spelled the way the ledger stores it. Git reads an object name
/// in either case and records it in lowercase, so a name spelled otherwise
/// must compare as the commit it names. A token that is not hex is no object
/// name and is left as written.
pub(crate) fn canonical_revision(revision: &str) -> String {
    if revision.chars().all(|c| c.is_ascii_hexdigit()) {
        revision.to_ascii_lowercase()
    } else {
        revision.to_string()
    }
}

/// Whether two spellings name one commit: equal once canonical, or one
/// abbreviating the other from the length at which an abbreviation names one.
fn names_one_commit(left: &str, right: &str) -> bool {
    let (left, right) = (canonical_revision(left), canonical_revision(right));
    if left == right {
        return !left.is_empty();
    }
    left.len().min(right.len()) >= ABBREVIATION_FLOOR
        && (left.starts_with(&right) || right.starts_with(&left))
}

/// One ref move a rewrite has planned: the value the ref held when the
/// rewrite read it, and the value it is to hold instead.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RefMove {
    pub old: String,
    pub new: String,
}

/// A rewrite's whole result, written down before any of it is applied.
///
/// A rewrite recreates commits, then moves refs onto them, then records the
/// map. Each of those is durable and the sequence is not, so an interruption
/// between any two leaves a repository that describes two histories at once.
/// The intent is what makes the sequence finishable: the commits already
/// exist, the refs each name one of two known values, and the map is here to
/// be recorded — so the only thing left to decide is which steps remain.
///
/// Without it, the second run has to recreate the commits, which signs them
/// afresh and yields different ids: a second successor for every commit the
/// first run already moved a ref onto, and a recorded map that contradicts
/// itself.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RewriteIntent {
    pub schema_version: u32,
    /// The branch whose history this rewrites.
    pub branch: String,
    /// The oldest commit the rewrite recreated.
    pub from: String,
    /// The reason and tool the mapping event is to carry.
    pub reason: String,
    pub tool: Option<String>,
    /// The map exactly as the event will record it.
    pub mapping: BTreeMap<String, Option<String>>,
    /// Every ref the rewrite moves, the rewritten branch included.
    pub refs: BTreeMap<String, RefMove>,
    /// The refs created to keep the commits the rewrite replaces reachable,
    /// each with the commit it holds.
    #[serde(default)]
    pub retained: BTreeMap<String, String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Validate withdrawals and identify the maps that do not participate in
/// revision resolution. IDs, rather than event order, bind the withdrawal.
pub fn withdrawn_event_ids<'a>(events: &[&'a crate::model::Event]) -> Result<BTreeSet<&'a str>> {
    let mut withdrawn = BTreeSet::new();
    for event in events {
        if let Payload::HistoryRewriteWithdrawn {
            rewrite_event_id,
            reason,
        } = &event.payload
        {
            if reason.trim().is_empty() {
                anyhow::bail!("withdrawal {} has an empty reason", event.event_id);
            }
            if !events.iter().any(|target| {
                target.event_id == *rewrite_event_id
                    && target.change_id == Store::REPOSITORY_SCOPE
                    && matches!(target.payload, Payload::HistoryRewritten { .. })
            }) {
                anyhow::bail!(
                    "withdrawal {} names {rewrite_event_id}, which is not a held \
                               history-rewritten repository event",
                    event.event_id
                );
            }
            withdrawn.insert(rewrite_event_id.as_str());
        }
    }
    Ok(withdrawn)
}

/// Active recorded rewrites, flattened into one old-to-fate mapping.
#[derive(Debug, Default, Clone)]
pub struct RewriteMap {
    steps: BTreeMap<String, Option<String>>,
    /// The keys of `steps` whose successor differs in nothing but its
    /// signature, as the event that recorded them judged.
    signature_only: BTreeSet<String>,
    /// The keys of `steps` whose successor differs only by role entries
    /// appended to its message, and perhaps its signature, as the event that
    /// recorded them judged.
    role_additions: BTreeSet<String>,
    /// The links of permitted maps a withdrawal removed: each old revision
    /// with every successor one of them named. They resolve nothing; they
    /// tell a reference a withdrawal stranded from one no map supported.
    withdrawn_steps: BTreeMap<String, BTreeSet<String>>,
    /// The distinct lengths of the keys of `steps`, shortest first: the only
    /// lengths at which a key can be a prefix of a queried revision.
    key_lengths: std::sync::OnceLock<Vec<usize>>,
}

impl RewriteMap {
    pub fn load(store: &Store) -> Result<Self> {
        Self::from_events(store.repository_events()?.iter())
    }

    /// The same map, built from a given sequence of events. Import uses it to
    /// judge the combination of what a bundle carries and what is already
    /// here — two events with different IDs can still disagree about one
    /// revision, which no per-event check can see.
    pub fn from_events<'a>(events: impl Iterator<Item = &'a crate::model::Event>) -> Result<Self> {
        Self::from_permitted_events(events, |_| true)
    }

    /// Only permitted rewrite assertions supply identity proof. Withdrawal
    /// targets are checked against all held facts, including unadmitted maps.
    pub fn from_permitted_events<'a>(
        events: impl Iterator<Item = &'a crate::model::Event>,
        permits: impl Fn(&crate::model::Event) -> bool,
    ) -> Result<Self> {
        let events: Vec<_> = events.collect();
        let active_withdrawals: Vec<_> = events
            .iter()
            .copied()
            .filter(|event| {
                !matches!(event.payload, Payload::HistoryRewriteWithdrawn { .. }) || permits(event)
            })
            .collect();
        let withdrawn = withdrawn_event_ids(&active_withdrawals)?;
        let mut steps: BTreeMap<String, Option<String>> = BTreeMap::new();
        let mut signature_only: BTreeSet<String> = BTreeSet::new();
        let mut role_additions: BTreeSet<String> = BTreeSet::new();
        let mut withdrawn_steps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for event in events {
            if !permits(event) {
                continue;
            }
            if withdrawn.contains(event.event_id.as_str()) {
                if let Payload::HistoryRewritten { mapping, .. } = &event.payload {
                    if validate_mapping(mapping).is_ok() {
                        for (old, new) in mapping {
                            if let Some(new) = new {
                                withdrawn_steps
                                    .entry(canonical_revision(old))
                                    .or_default()
                                    .insert(canonical_revision(new));
                            }
                        }
                    }
                }
                continue;
            }
            if let Payload::HistoryRewritten {
                mapping,
                signature_only: judged,
                role_additions: added,
                ..
            } = &event.payload
            {
                // Both write paths refuse a mapping that cannot mean what it
                // says, so a valid ledger holds none. One that arrived another
                // way is skipped rather than made fatal: a map nobody can read
                // is worse than a map missing an entry, and `arc doctor`
                // reports it as `invalid-rewrite-mapping`.
                if validate_mapping(mapping).is_err() {
                    continue;
                }
                for (old, new) in mapping {
                    // A recorded map may spell a revision in any case; the
                    // map answers for the commit, whichever way it was typed.
                    let old = canonical_revision(old);
                    let new = new.as_deref().map(canonical_revision);
                    // Two rewrites may each move a revision — that is a chain,
                    // and following it is the point. But two *different*
                    // successors for the same revision is a contradiction, and
                    // silently keeping the last one would answer a question the
                    // ledger cannot answer.
                    if let Some(existing) = steps.get(&old) {
                        if existing != &new {
                            anyhow::bail!(
                                "recorded rewrites disagree about {old}: {} and {}",
                                describe(existing),
                                describe(&new)
                            );
                        }
                    }
                    steps.insert(old, new);
                }
                // A claim about a revision the event does not map is not a
                // claim about anything.
                let mapped: BTreeSet<String> = mapping
                    .keys()
                    .map(|revision| canonical_revision(revision))
                    .collect();
                let claimed = |judged: &[String]| -> Vec<String> {
                    judged
                        .iter()
                        .map(|old| canonical_revision(old))
                        .filter(|old| mapped.contains(old))
                        .collect()
                };
                signature_only.extend(claimed(judged));
                role_additions.extend(claimed(added));
            }
        }
        Ok(Self {
            steps,
            signature_only,
            role_additions,
            withdrawn_steps,
            key_lengths: Default::default(),
        })
    }

    pub(crate) fn mapped_revisions(&self) -> impl Iterator<Item = &str> {
        self.steps.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// What became of a recorded revision, following successive rewrites.
    /// `Ok(None)` means no recorded rewrite touched it, so a caller can tell
    /// "unchanged" from "moved" and from "dropped".
    ///
    /// A query that names a recorded revision exactly resolves to it. Failing
    /// that it may abbreviate one, or be abbreviated by one — a map may
    /// abbreviate what the ledger stores in full, and a caller may abbreviate
    /// what the map stores in full — and then it resolves only when exactly
    /// one recorded revision answers. An abbreviation that two answer is an
    /// error naming them: the map holds the answer and cannot say which, and
    /// reporting the revision as untouched would be a different claim.
    pub fn fate(&self, revision: &str) -> Result<Option<Fate>> {
        let mut visited: Vec<String> = Vec::new();
        let mut current = revision.to_string();
        loop {
            match self.step(&current)? {
                // Nothing rewrote this one. Either the caller asked about a
                // revision no rewrite touched, or we followed the chain to
                // its end and this is where it survives.
                Step::Unmapped => {
                    return Ok((!visited.is_empty()).then_some(Fate::Rewritten(current)))
                }
                Step::Mapped { key, fate } => {
                    // Only a hand-written map can cycle. Visiting a key twice
                    // is what a cycle is, whatever spelling led back to it.
                    if visited.contains(&key) {
                        anyhow::bail!(
                            "recorded rewrites lead {revision} back to {key}; the chain names no \
                             surviving commit"
                        );
                    }
                    visited.push(key);
                    match fate {
                        None => return Ok(Some(Fate::Dropped)),
                        Some(next) => current = next,
                    }
                }
            }
        }
    }

    /// The revision a recorded one names in this repository: itself when no
    /// recorded rewrite touched it, and its successor once every recorded
    /// rewrite has been followed.
    ///
    /// A revision a rewrite dropped answers itself. Nothing survives it, and
    /// answering with the last readable link would name a commit the record
    /// does not claim.
    pub fn current(&self, revision: &str) -> Result<String> {
        Ok(match self.fate(revision)? {
            Some(Fate::Rewritten(successor)) => successor,
            Some(Fate::Dropped) | None => revision.to_string(),
        })
    }

    /// Whether an active rewrite chain carries `revision` to this exact
    /// successor, including an intermediate commit. A shared eventual
    /// successor does not establish this directed relationship.
    pub fn rewrites_to(&self, revision: &str, successor: &str) -> Result<bool> {
        let mut visited = BTreeSet::new();
        let mut current = revision.to_string();
        let mut reached = false;
        loop {
            match self.step(&current)? {
                Step::Unmapped | Step::Mapped { fate: None, .. } => return Ok(reached),
                Step::Mapped {
                    key,
                    fate: Some(next),
                } => {
                    if !visited.insert(key.clone()) {
                        anyhow::bail!("recorded rewrites lead {revision} back to {key}; the chain names no surviving commit");
                    }
                    reached |= next == canonical_revision(successor);
                    current = next;
                }
            }
        }
    }

    /// Whether active and withdrawn maps together carry `revision` to
    /// `successor`. For a reference the active maps do not resolve, this says
    /// a withdrawal stranded it, rather than that no map ever supported it.
    pub fn withdrawn_rewrites_to(&self, revision: &str, successor: &str) -> bool {
        let successor = canonical_revision(successor);
        let mut visited = BTreeSet::new();
        let mut pending = vec![canonical_revision(revision)];
        while let Some(current) = pending.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            let active = self.steps.get(&current).cloned().flatten();
            let withdrawn = self.withdrawn_steps.get(&current).into_iter().flatten();
            for next in active.iter().chain(withdrawn) {
                if *next == successor {
                    return true;
                }
                pending.push(next.clone());
            }
        }
        false
    }

    /// Follow a recorded revision forward in place.
    pub fn advance(&self, revision: &mut String) -> Result<()> {
        if self.steps.is_empty() {
            return Ok(());
        }
        *revision = self.current(revision)?;
        Ok(())
    }

    /// The revision an approval of `revision` still stands on: `revision`
    /// followed through every recorded rewrite that changed its signature,
    /// appended role entries to its message, or both, and stopping before the
    /// first that changed anything else, that dropped it, or that no event
    /// verified.
    ///
    /// An approval binds to what was reviewed. Following a rewrite that moved
    /// the commit onto a different tree, message or author would carry it to
    /// content nobody approved; a recorded role entry declares who took part
    /// and is not a verdict.
    pub fn approved_successor(&self, revision: &str) -> Result<String> {
        self.follow(revision, |key| {
            self.signature_only.contains(key) || self.role_additions.contains(key)
        })
    }

    /// The revision `revision` is re-signed as: followed through every
    /// recorded rewrite that changed its signature and nothing else. This is
    /// what admits a signing; a rewrite that added role entries is no signing.
    pub fn signature_successor(&self, revision: &str) -> Result<String> {
        self.follow(revision, |key| self.signature_only.contains(key))
    }

    /// Whether `before` reaches `after` through links `carries` accepts, or
    /// is `after` already. `carries` sees each canonical old revision.
    fn reaches(&self, before: &str, after: &str, carries: impl Fn(&str) -> bool) -> bool {
        let mut visited = BTreeSet::new();
        let mut current = canonical_revision(before);
        loop {
            if names_one_commit(&current, after) {
                return true;
            }
            match self.step(&current) {
                Ok(Step::Mapped {
                    key,
                    fate: Some(next),
                }) if carries(&key) && !visited.contains(&key) => {
                    visited.insert(key);
                    current = next;
                }
                _ => return false,
            }
        }
    }

    fn follow(&self, revision: &str, carries: impl Fn(&str) -> bool) -> Result<String> {
        let mut visited: Vec<String> = Vec::new();
        let mut current = revision.to_string();
        loop {
            match self.step(&current)? {
                Step::Mapped {
                    key,
                    fate: Some(next),
                } if carries(&key) => {
                    if visited.contains(&key) {
                        anyhow::bail!(
                            "recorded rewrites lead {revision} back to {key}; the chain names no \
                             surviving commit"
                        );
                    }
                    visited.push(key);
                    current = next;
                }
                Step::Unmapped | Step::Mapped { .. } => return Ok(current),
            }
        }
    }

    /// Follow an optional recorded revision forward in place.
    pub fn advance_opt(&self, revision: &mut Option<String>) -> Result<()> {
        match revision.as_mut() {
            Some(revision) => self.advance(revision),
            None => Ok(()),
        }
    }

    /// Whether two revisions name one commit once both are followed forward.
    ///
    /// Equal resolved ids are one commit. Failing that either may abbreviate
    /// the other — a recorded map and a Git read of the same commit need not
    /// agree on length, and a comparison that demanded they did would answer
    /// "different commit" for one commit — and an abbreviation stands for a
    /// commit only from the length at which it names one.
    pub fn same(&self, left: &str, right: &str) -> Result<bool> {
        Ok(names_one_commit(
            &self.current(left)?,
            &self.current(right)?,
        ))
    }

    /// One hop: the recorded revision `revision` names, exactly where it names
    /// one and by abbreviation in either direction otherwise.
    fn step(&self, revision: &str) -> Result<Step> {
        if self.steps.is_empty() {
            return Ok(Step::Unmapped);
        }
        let revision = canonical_revision(revision);
        let revision = revision.as_str();
        if let Some(fate) = self.steps.get(revision) {
            return Ok(Step::Mapped {
                key: revision.to_string(),
                fate: fate.clone(),
            });
        }
        // Short of the floor, or spelled with anything but hex, a query is not
        // an abbreviation of a commit and matching it against one would be a
        // coincidence rather than a resolution.
        if revision.len() < ABBREVIATION_FLOOR
            || !revision.chars().all(|byte| byte.is_ascii_hexdigit())
        {
            return Ok(Step::Unmapped);
        }
        // The keys `revision` abbreviates sort together from it onwards, and
        // the keys abbreviating it are its own prefixes; both are found by
        // lookup, in key order, without visiting every recorded revision.
        let mut candidates: BTreeSet<&String> = self
            .steps
            .range::<str, _>((
                std::ops::Bound::Included(revision),
                std::ops::Bound::Unbounded,
            ))
            .map(|(old, _)| old)
            .take_while(|old| old.starts_with(revision))
            .collect();
        let key_lengths = self.key_lengths.get_or_init(|| {
            let lengths: BTreeSet<usize> = self.steps.keys().map(String::len).collect();
            lengths.into_iter().collect()
        });
        candidates.extend(
            key_lengths
                .iter()
                .take_while(|length| **length < revision.len())
                .filter(|length| **length > 0)
                .filter_map(|length| self.steps.get_key_value(&revision[..*length]))
                .map(|(old, _)| old),
        );
        let candidates: Vec<&String> = candidates.into_iter().collect();
        match candidates.as_slice() {
            [] => Ok(Step::Unmapped),
            [old] => Ok(Step::Mapped {
                key: (*old).clone(),
                fate: self.steps[*old].clone(),
            }),
            many => anyhow::bail!(
                "{revision} abbreviates {} recorded revisions ({}); it names none of them",
                many.len(),
                many.iter()
                    .map(|old| old.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// The old revisions of `mapping` whose successor is the same commit signed
/// differently: the same tree, author, committer, message and every other
/// header, with parents that differ only as `rewrites` translates them.
///
/// This is decided from the two commit objects, which is why it has to be
/// decided when a mapping is recorded. A revision either commit of which is
/// not readable here is left out: nothing vouches for a rewrite nobody can
/// compare, and the old commit is the one Git discards first.
pub fn signature_only_successors(
    cwd: &Path,
    mapping: &BTreeMap<String, Option<String>>,
    rewrites: &RewriteMap,
) -> Vec<String> {
    successors_where(cwd, mapping, |old, new| {
        same_frame(old, new, |before, after| {
            rewrites.same(before, after).unwrap_or(false)
        }) && old.message == new.message
    })
}

/// The old revisions of `mapping` whose successor differs from them only by
/// complete role entries appended to its message's final trailer block, and
/// perhaps its signature: the same tree, the same bytes in every other
/// header, and the message
/// [`adds_only_role_entries`](crate::trailers::adds_only_role_entries)
/// accepts.
///
/// Its parents have to be its old parents, or reached from them through
/// rewrites that preserve an approval: a link `signature_only` names, one this
/// judges, or one an earlier event recorded as either. So a successor whose
/// ancestry changed in any other way is left out, and an approval that
/// follows it stands on a range in which every rewritten commit kept its
/// content and its message bar the added entries. `signature_only` is the
/// same event's judgement from [`signature_only_successors`]. Like it, a
/// revision either commit of which is not readable here is left out.
pub fn role_addition_successors(
    cwd: &Path,
    mapping: &BTreeMap<String, Option<String>>,
    rewrites: &RewriteMap,
    signature_only: &[String],
) -> Vec<String> {
    let signature_only: BTreeSet<String> = signature_only
        .iter()
        .map(|old| canonical_revision(old))
        .collect();
    let mut links: BTreeMap<String, Link> = BTreeMap::new();
    for (old, new) in mapping {
        let Some(new) = new.as_deref() else {
            continue;
        };
        let (Ok(before), Ok(after)) = (
            crate::gitio::read_commit(cwd, old),
            crate::gitio::read_commit(cwd, new),
        ) else {
            continue;
        };
        let key = canonical_revision(old);
        let adds = same_frame(&before, &after, |_, _| true)
            && crate::trailers::adds_only_role_entries(&before.message, &after.message);
        if adds || signature_only.contains(&key) {
            let parents = before.parents.into_iter().zip(after.parents).collect();
            links.insert(
                key,
                Link {
                    old: old.clone(),
                    adds,
                    parents,
                },
            );
        }
    }
    // A link stays only while each of its parents is carried by a link that
    // stays, so one removal can remove the links above it; repeat until none
    // goes.
    loop {
        let carries = |key: &str| {
            links.contains_key(key)
                || rewrites.signature_only.contains(key)
                || rewrites.role_additions.contains(key)
        };
        let broken: Vec<String> = links
            .iter()
            .filter(|(_, link)| {
                link.parents
                    .iter()
                    .any(|(before, after)| !rewrites.reaches(before, after, carries))
            })
            .map(|(key, _)| key.clone())
            .collect();
        if broken.is_empty() {
            break;
        }
        for key in broken {
            links.remove(&key);
        }
    }
    links
        .into_values()
        .filter_map(|link| link.adds.then_some(link.old))
        .collect()
}

/// One rewritten commit an approval may follow, while its parents allow.
struct Link {
    /// The old revision, spelled as in the mapping.
    old: String,
    /// Whether the successor appended role entries, rather than only being
    /// signed differently.
    adds: bool,
    /// Each parent of the old commit beside the successor's in its place.
    parents: Vec<(String, String)>,
}

/// The old revisions of `mapping` whose successor `keeps` accepts, both
/// commits read here.
fn successors_where(
    cwd: &Path,
    mapping: &BTreeMap<String, Option<String>>,
    keeps: impl Fn(&crate::gitio::RawCommit, &crate::gitio::RawCommit) -> bool,
) -> Vec<String> {
    mapping
        .iter()
        .filter_map(|(old, new)| {
            let new = new.as_deref()?;
            let (old_commit, new_commit) = (
                crate::gitio::read_commit(cwd, old).ok()?,
                crate::gitio::read_commit(cwd, new).ok()?,
            );
            keeps(&old_commit, &new_commit).then(|| old.clone())
        })
        .collect()
}

/// Whether two commits carry the same bytes in every header but their
/// parents and signature, with parents pairing up in order as `parents`
/// accepts.
fn same_frame(
    old: &crate::gitio::RawCommit,
    new: &crate::gitio::RawCommit,
    parents: impl Fn(&str, &str) -> bool,
) -> bool {
    let carried = |commit: &crate::gitio::RawCommit| {
        commit
            .headers
            .iter()
            .filter(|header| header.field != "parent")
            .map(|header| header.block.clone())
            .collect::<Vec<_>>()
    };
    carried(old) == carried(new)
        && old.parents.len() == new.parents.len()
        && old
            .parents
            .iter()
            .zip(&new.parents)
            .all(|(before, after)| parents(before, after))
}

/// Parse a commit map: `<old> <new>` per line, which is what `git filter-repo`
/// writes — including its `old`/`new` header line, which names the columns
/// rather than a commit.
///
/// A line whose new revision is all zeroes records a commit the rewrite
/// dropped; it survives at nothing, and following it forward must say so
/// rather than reporting it unchanged. An identity line records a commit the
/// rewrite left alone, which is not a move and is not recorded.
pub fn parse_commit_map(text: &str) -> Result<BTreeMap<String, Option<String>>> {
    let mut mapping: BTreeMap<String, Option<String>> = BTreeMap::new();
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
        // git filter-repo writes a column header before the first mapping.
        if old.eq_ignore_ascii_case("old") && new.eq_ignore_ascii_case("new") {
            continue;
        }
        if !is_revision(old) {
            anyhow::bail!("line {}: {old:?} is not a revision", number + 1);
        }
        let old = canonical_revision(old);
        let old = old.as_str();
        let dropped = new.chars().all(|c| c == '0');
        if !dropped && !is_revision(new) {
            anyhow::bail!("line {}: {new:?} is not a revision", number + 1);
        }
        let new = canonical_revision(new);
        if !dropped && new == old {
            continue;
        }
        let fate = (!dropped).then_some(new);
        if let Some(existing) = mapping.get(old) {
            if existing != &fate {
                anyhow::bail!(
                    "line {}: {old} is mapped twice, to different revisions",
                    number + 1
                );
            }
            continue;
        }
        mapping.insert(old.to_string(), fate);
    }
    if mapping.is_empty() {
        anyhow::bail!("the commit map records no rewritten revision");
    }
    Ok(mapping)
}

/// Whether a token looks like a Git object name: hex, and long enough to
/// abbreviate one. Matching is by prefix elsewhere, so the length floor is
/// what keeps `a` from matching half the repository.
/// Whether a recorded mapping means what a recorded mapping can mean.
///
/// `parse_commit_map` enforces this on the way in, but a bundle carries the
/// payload directly: without the same check, an imported map could say a
/// revision was rewritten to the zero object — which the recording path spells
/// as "dropped" — and every reader would report a rewrite to nothing.
pub fn validate_mapping(mapping: &BTreeMap<String, Option<String>>) -> Result<()> {
    for (old, new) in mapping {
        if !is_revision(old) {
            anyhow::bail!("{old:?} is not a revision");
        }
        match new {
            None => {}
            Some(new) if new.chars().all(|c| c == '0') => anyhow::bail!(
                "{old} maps to the zero object; a dropped commit is recorded as no successor"
            ),
            Some(new) if !is_revision(new) => anyhow::bail!("{new:?} is not a revision"),
            Some(new) if new == old => {
                anyhow::bail!("{old} maps to itself, which is not a rewrite")
            }
            Some(_) => {}
        }
    }
    if mapping.is_empty() {
        anyhow::bail!("the rewrite records no revision");
    }
    Ok(())
}

fn is_revision(token: &str) -> bool {
    token.len() >= 7
        && token.len() <= 64
        && token.chars().all(|c| c.is_ascii_hexdigit())
        // The zero object is how Git spells "no object". A rewrite from it
        // would claim a successor for a commit that never existed.
        && !token.chars().all(|c| c == '0')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, Option<&str>)]) -> RewriteMap {
        RewriteMap {
            steps: pairs
                .iter()
                .map(|(old, new)| (old.to_string(), new.map(str::to_string)))
                .collect(),
            ..RewriteMap::default()
        }
    }

    #[test]
    fn a_revision_rewritten_twice_follows_the_chain() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("bbbbbbbbbb", Some("cccccccccc")),
        ]);
        assert_eq!(
            map.fate("aaaaaaaaaa").unwrap(),
            Some(Fate::Rewritten("cccccccccc".into()))
        );
        assert_eq!(map.fate("cccccccccc").unwrap(), None);
    }

    #[test]
    fn rewrite_proof_is_directed_and_accepts_intermediate_successors() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("bbbbbbbbbb", Some("cccccccccc")),
            ("dddddddddd", Some("cccccccccc")),
        ]);
        assert!(map.rewrites_to("aaaaaaaaaa", "bbbbbbbbbb").unwrap());
        assert!(map.rewrites_to("aaaaaaaaaa", "cccccccccc").unwrap());
        assert!(!map.rewrites_to("aaaaaaaaaa", "dddddddddd").unwrap());
        assert!(!map.rewrites_to("bbbbbbbbbb", "aaaaaaaaaa").unwrap());
        assert!(!map.rewrites_to("aaaaaaaaaa", "eeeeeeeeee").unwrap());
    }

    #[test]
    fn rewrite_proof_refuses_a_cycle_even_after_reaching_the_claimed_revision() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("bbbbbbbbbb", Some("aaaaaaaaaa")),
        ]);
        assert!(map.rewrites_to("aaaaaaaaaa", "bbbbbbbbbb").is_err());
        let dropped = self::map(&[("aaaaaaaaaa", None)]);
        assert!(!dropped.rewrites_to("aaaaaaaaaa", "bbbbbbbbbb").unwrap());
    }

    /// A map may abbreviate what the ledger records in full, and a caller may
    /// abbreviate what the map records in full. Byte equality would silently
    /// fail to follow either.
    #[test]
    fn revisions_match_by_abbreviation_in_both_directions() {
        let full = map(&[(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        )]);
        assert!(full.fate("aaaaaaaaaa").unwrap().is_some());
        let short = map(&[("aaaaaaaaaa", Some("bbbbbbbbbb"))]);
        assert!(short
            .fate("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap()
            .is_some());
    }

    /// A recorded revision that a longer one abbreviates is still the one the
    /// query names exactly. Preferring the abbreviation match would make the
    /// pair ambiguous and refuse a revision the map answers for.
    #[test]
    fn an_exact_match_wins_over_an_abbreviation_of_it() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("aaaaaaaaaaaa", Some("cccccccccc")),
        ]);
        assert_eq!(
            map.fate("aaaaaaaaaa").unwrap(),
            Some(Fate::Rewritten("bbbbbbbbbb".into()))
        );
        assert_eq!(
            map.fate("aaaaaaaaaaaa").unwrap(),
            Some(Fate::Rewritten("cccccccccc".into()))
        );
    }

    /// Claim: a step finds exactly the recorded revisions a scan of every key
    /// would, in both abbreviation directions, with any refusal naming them
    /// in the same order. Rejects a lookup that misses a key the query
    /// abbreviates or one abbreviating the query.
    #[test]
    fn a_step_finds_what_a_scan_of_every_key_finds() {
        let keys = [
            "a",
            "ab12",
            "ab12cd3",
            "ab12cd34",
            "ab12cd34ef",
            "ab12cd35",
            "ab13",
            "abcdef0",
            "abcdef01",
            "abcdef0123",
            "b0",
            "ffffffff",
        ];
        let map = map(&keys.map(|key| (key, Some("1111111111"))));
        let scan = |revision: &str| -> Vec<&str> {
            keys.iter()
                .copied()
                .filter(|old| old.starts_with(revision) || revision.starts_with(old))
                .collect()
        };
        for revision in [
            "ab12cd3",
            "ab12cd34",
            "ab12cd34e",
            "ab12cd34ef9",
            "ab12cd3x",
            "abcdef0",
            "abcdef012",
            "abcdef0123456",
            "ffffffff00",
            "ab12cd36",
            "0000000",
        ] {
            let expected = scan(revision);
            match map.step(revision) {
                Ok(Step::Unmapped) => assert!(
                    expected.is_empty()
                        || revision.len() < ABBREVIATION_FLOOR
                        || !revision.chars().all(|c| c.is_ascii_hexdigit()),
                    "{revision}: {expected:?}"
                ),
                Ok(Step::Mapped { key, .. }) => assert!(
                    key == revision || expected == [key.as_str()],
                    "{revision}: {key} against {expected:?}"
                ),
                Err(refusal) => assert!(
                    refusal.to_string().contains(&expected.join(", ")),
                    "{revision}: {refusal} against {expected:?}"
                ),
            }
        }
    }

    /// An abbreviation two recorded revisions answer names neither, and the
    /// refusal has to name them: the map holds the answer and cannot say
    /// which, which is not the same as no rewrite having touched it.
    #[test]
    fn an_ambiguous_abbreviation_is_refused_naming_the_candidates() {
        let map = map(&[
            ("aaaaaaaa11", Some("cccccccccc")),
            ("aaaaaaaa22", Some("dddddddddd")),
        ]);
        let refusal = map.fate("aaaaaaaa").unwrap_err().to_string();
        assert!(refusal.contains("aaaaaaaa11"), "{refusal}");
        assert!(refusal.contains("aaaaaaaa22"), "{refusal}");
    }

    /// Short of seven hex digits a prefix names a sixteenth of the repository
    /// per missing digit, so it stands for no commit at all.
    #[test]
    fn an_abbreviation_below_the_floor_names_nothing() {
        let map = map(&[("aaaaaaaaaa", Some("bbbbbbbbbb"))]);
        assert_eq!(map.fate("aaaaaa").unwrap(), None);
        assert_eq!(
            map.fate("aaaaaaa").unwrap(),
            Some(Fate::Rewritten("bbbbbbbbbb".into()))
        );
        assert!(!map.same("aaaaaa", "aaaaaaaaaa").unwrap());
        assert!(map.same("aaaaaaa", "bbbbbbbbbb").unwrap());
    }

    #[test]
    fn a_dropped_revision_is_not_a_survivor() {
        let map = map(&[("aaaaaaaaaa", None)]);
        assert_eq!(map.fate("aaaaaaaaaa").unwrap(), Some(Fate::Dropped));
    }

    /// Ambiguity anywhere along a chain makes the answer unavailable, not the
    /// ambiguous node a survivor.
    #[test]
    fn an_ambiguous_chain_is_refused() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("bbbbbbbbbb11", Some("cccccccccc")),
            ("bbbbbbbbbb22", Some("dddddddddd")),
        ]);
        assert!(map.fate("aaaaaaaaaa").is_err());
    }

    /// A cycle is visiting a key twice, whatever spelling led back to it.
    #[test]
    fn a_chain_that_returns_to_a_visited_key_is_refused() {
        let map = map(&[("aaaaaaaaaa", Some("aaaaaaaaaaaa"))]);
        assert!(map.fate("aaaaaaaaaa").is_err());
    }

    #[test]
    fn a_cyclic_mapping_is_refused_rather_than_answered_with_a_bogus_survivor() {
        let map = map(&[
            ("aaaaaaaaaa", Some("bbbbbbbbbb")),
            ("bbbbbbbbbb", Some("aaaaaaaaaa")),
        ]);
        assert!(map.fate("aaaaaaaaaa").is_err());
    }

    /// The real `git filter-repo` map opens with a column header, records
    /// dropped commits as all zeroes, and leaves untouched commits mapped to
    /// themselves.
    #[test]
    fn the_filter_repo_format_parses() {
        let parsed = parse_commit_map(
            "old                                      new\n\
             aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\
             cccccccccccccccccccccccccccccccccccccccc 0000000000000000000000000000000000000000\n\
             dddddddddddddddddddddddddddddddddddddddd dddddddddddddddddddddddddddddddddddddddd\n",
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed
                .get("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap()
                .as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        assert_eq!(
            parsed.get("cccccccccccccccccccccccccccccccccccccccc"),
            Some(&None),
            "a dropped commit is recorded as dropped, not discarded"
        );
        assert!(
            !parsed.contains_key("dddddddddddddddddddddddddddddddddddddddd"),
            "an untouched commit is not a rewrite"
        );
    }

    #[test]
    fn a_conflicting_duplicate_is_refused() {
        assert!(parse_commit_map("aaaaaaaaaa bbbbbbbbbb\naaaaaaaaaa cccccccccc\n").is_err());
        assert!(parse_commit_map("aaaaaaaaaa bbbbbbbbbb\naaaaaaaaaa bbbbbbbbbb\n").is_ok());
    }

    #[test]
    fn a_commit_map_is_recorded_in_the_case_git_stores() {
        let parsed = parse_commit_map(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB\n\
             CCCCCCCCCC cccccccccc\n\
             DDDDDDDDDD EEEEEEEEEE\n\
             dddddddddd eeeeeeeeee\n",
        )
        .unwrap();
        assert_eq!(
            parsed.into_iter().collect::<Vec<_>>(),
            vec![
                ("a".repeat(40), Some("b".repeat(40))),
                ("dddddddddd".to_string(), Some("eeeeeeeeee".to_string())),
            ],
            "an identity line is no rewrite whatever its case, and one revision spelled \
             two ways is one revision"
        );
        assert!(parse_commit_map("AAAAAAAAAA bbbbbbbbbb\naaaaaaaaaa cccccccccc\n").is_err());
    }

    #[test]
    fn a_map_recorded_in_upper_case_answers_for_the_commit_in_lower_case() {
        let events = [
            rewrite_event("01A", "AAAAAAAAAA", "BBBBBBBBBB", &["AAAAAAAAAA"]),
            rewrite_event("01B", "BBBBBBBBBB", "cccccccccc", &[]),
        ];
        let map = RewriteMap::from_events(events.iter()).unwrap();
        assert_eq!(map.current("aaaaaaaaaa").unwrap(), "cccccccccc");
        assert_eq!(map.current("AAAAAAAAAA").unwrap(), "cccccccccc");
        assert_eq!(map.current("aaaaaaaa").unwrap(), "cccccccccc");
        assert_eq!(map.approved_successor("aaaaaaaaaa").unwrap(), "bbbbbbbbbb");
        assert!(map.rewrites_to("aaaaaaaaaa", "BBBBBBBBBB").unwrap());
        assert!(map.same("AAAAAAAAAA", "cccccccccc").unwrap());
        assert!(map.same("aaaaaaaaaa", "CCCCCCCC").unwrap());
        let contradiction = [
            rewrite_event("01A", "AAAAAAAAAA", "bbbbbbbbbb", &[]),
            rewrite_event("01B", "aaaaaaaaaa", "cccccccccc", &[]),
        ];
        assert!(RewriteMap::from_events(contradiction.iter()).is_err());
    }

    /// A signature-only assertion follows the same commit identity as its
    /// map, even when they spell that identity in different letter cases.
    #[test]
    fn signature_only_claims_match_map_keys_without_regard_to_case() {
        for (old, judged) in [("AAAAAAAAAA", "aaaaaaaaaa"), ("aaaaaaaaaa", "AAAAAAAAAA")] {
            let events = [rewrite_event("01A", old, "bbbbbbbbbb", &[judged])];
            let map = RewriteMap::from_events(events.iter()).unwrap();
            assert_eq!(
                map.approved_successor("aaaaaaaaaa").unwrap(),
                "bbbbbbbbbb",
                "the signature-only assertion and map name the same commit"
            );
        }
    }

    fn rewrite_event(id: &str, old: &str, new: &str, judged: &[&str]) -> crate::model::Event {
        crate::model::Event {
            schema_version: crate::model::SCHEMA_VERSION,
            event_id: id.to_string(),
            repository_id: "repo".into(),
            change_id: crate::store::Store::REPOSITORY_SCOPE.to_string(),
            actor: "tester".into(),
            actor_source: None,
            operator: None,
            harness: None,
            session: None,
            session_link: None,
            session_resolution: None,
            session_resolution_reason: None,
            invocation_id: None,
            invocation_elapsed_ms: None,
            on_behalf_of: None,
            model: None,
            model_provenance: Default::default(),
            created_at: chrono::Utc::now(),
            payload: Payload::HistoryRewritten {
                mapping: BTreeMap::from([(old.to_string(), Some(new.to_string()))]),
                reason: "test".into(),
                tool: None,
                signature_only: judged.iter().map(|old| old.to_string()).collect(),
                role_additions: Vec::new(),
                refs: BTreeMap::new(),
                retained: BTreeMap::new(),
            },
        }
    }

    /// An approval follows a chain only while every link was judged to differ
    /// by signature alone, and stops before the first that was not.
    #[test]
    fn an_approval_follows_only_signature_only_links() {
        let events = [
            rewrite_event("01A", "aaaaaaaaaa", "bbbbbbbbbb", &["aaaaaaaaaa"]),
            rewrite_event("01B", "bbbbbbbbbb", "cccccccccc", &[]),
            rewrite_event("01C", "cccccccccc", "dddddddddd", &["cccccccccc"]),
        ];
        let map = RewriteMap::from_events(events.iter()).unwrap();
        assert_eq!(map.approved_successor("aaaaaaaaaa").unwrap(), "bbbbbbbbbb");
        assert_eq!(map.approved_successor("bbbbbbbbbb").unwrap(), "bbbbbbbbbb");
        assert_eq!(map.approved_successor("cccccccccc").unwrap(), "dddddddddd");
        assert_eq!(map.approved_successor("eeeeeeeeee").unwrap(), "eeeeeeeeee");
        assert_eq!(
            map.current("aaaaaaaaaa").unwrap(),
            "dddddddddd",
            "every other recorded revision follows the whole chain"
        );
    }

    /// An approval follows a role-addition link as it follows a signature
    /// link, while a signing follows only the signature links. Rejects either
    /// chain taking the other's links.
    #[test]
    fn an_approval_follows_role_additions_and_a_signing_does_not() {
        let mut added = rewrite_event("01B", "bbbbbbbbbb", "cccccccccc", &[]);
        if let Payload::HistoryRewritten { role_additions, .. } = &mut added.payload {
            role_additions.push("BBBBBBBBBB".into());
        }
        let events = [
            rewrite_event("01A", "aaaaaaaaaa", "bbbbbbbbbb", &["aaaaaaaaaa"]),
            added,
            rewrite_event("01C", "cccccccccc", "dddddddddd", &[]),
        ];
        let map = RewriteMap::from_events(events.iter()).unwrap();
        assert_eq!(map.approved_successor("aaaaaaaaaa").unwrap(), "cccccccccc");
        assert_eq!(map.signature_successor("aaaaaaaaaa").unwrap(), "bbbbbbbbbb");
        assert_eq!(map.signature_successor("bbbbbbbbbb").unwrap(), "bbbbbbbbbb");
        assert_eq!(map.current("aaaaaaaaaa").unwrap(), "dddddddddd");
    }

    /// A judgement about a revision the event does not map is about nothing.
    #[test]
    fn a_judgement_of_an_unmapped_revision_verifies_nothing() {
        let events = [rewrite_event(
            "01A",
            "aaaaaaaaaa",
            "bbbbbbbbbb",
            &["cccccccccc"],
        )];
        let map = RewriteMap::from_events(events.iter()).unwrap();
        assert_eq!(map.approved_successor("cccccccccc").unwrap(), "cccccccccc");
        assert_eq!(map.approved_successor("aaaaaaaaaa").unwrap(), "aaaaaaaaaa");
    }

    /// Two events, two IDs, one revision, two answers. Neither event is
    /// wrong on its own, which is exactly why the combination has to be
    /// judged rather than each event in turn.
    #[test]
    fn separate_events_that_disagree_about_one_revision_are_refused() {
        let event = |id: &str, old: &str, new: &str| rewrite_event(id, old, new, &[]);
        let agreeing = [
            event("01A", "aaaaaaaaaa", "bbbbbbbbbb"),
            event("01B", "aaaaaaaaaa", "bbbbbbbbbb"),
        ];
        assert!(RewriteMap::from_events(agreeing.iter()).is_ok());

        let disagreeing = [
            event("01A", "aaaaaaaaaa", "bbbbbbbbbb"),
            event("01B", "aaaaaaaaaa", "cccccccccc"),
        ];
        assert!(RewriteMap::from_events(disagreeing.iter()).is_err());
    }

    #[test]
    fn a_withdrawal_removes_only_its_map_regardless_of_event_order() {
        let wrong = rewrite_event("wrong", "aaaaaaaaaa", "bbbbbbbbbb", &["aaaaaaaaaa"]);
        let correct = rewrite_event("correct", "aaaaaaaaaa", "cccccccccc", &[]);
        let mut withdrawal = wrong.clone();
        withdrawal.event_id = "withdrawal".into();
        withdrawal.payload = Payload::HistoryRewriteWithdrawn {
            rewrite_event_id: "wrong".into(),
            reason: "wrong map".into(),
        };
        let events = [withdrawal.clone(), correct, wrong];
        let map = RewriteMap::from_events(events.iter()).unwrap();
        assert_eq!(map.current("aaaaaaaaaa").unwrap(), "cccccccccc");
        assert_eq!(map.approved_successor("aaaaaaaaaa").unwrap(), "aaaaaaaaaa");
        assert!(RewriteMap::from_events([withdrawal].iter()).is_err());
    }

    /// Claim: a withdrawn map's links answer only whether a reference was
    /// stranded, along the direction the maps record, through active links as
    /// well. Rejects: a withdrawn link resolving a revision, a shared
    /// successor or a reversed link reading as a stranding, or a withdrawal
    /// that contradicts an active map making the map unreadable.
    #[test]
    fn withdrawn_links_say_what_a_withdrawal_stranded_and_resolve_nothing() {
        let wrong = rewrite_event("wrong", "aaaaaaaaaa", "bbbbbbbbbb", &[]);
        let correct = rewrite_event("correct", "aaaaaaaaaa", "cccccccccc", &[]);
        let onward = rewrite_event("onward", "bbbbbbbbbb", "dddddddddd", &[]);
        let unrelated = rewrite_event("unrelated", "eeeeeeeeee", "bbbbbbbbbb", &[]);
        let mut withdrawal = wrong.clone();
        withdrawal.event_id = "withdrawal".into();
        withdrawal.payload = Payload::HistoryRewriteWithdrawn {
            rewrite_event_id: "wrong".into(),
            reason: "wrong map".into(),
        };
        let mut unrelated_withdrawal = withdrawal.clone();
        unrelated_withdrawal.event_id = "withdrawal-2".into();
        unrelated_withdrawal.payload = Payload::HistoryRewriteWithdrawn {
            rewrite_event_id: "unrelated".into(),
            reason: "wrong map".into(),
        };
        let events = [
            wrong,
            correct,
            onward,
            unrelated,
            withdrawal,
            unrelated_withdrawal,
        ];
        let map = RewriteMap::from_events(events.iter()).unwrap();
        assert_eq!(map.current("aaaaaaaaaa").unwrap(), "cccccccccc");
        assert!(!map.rewrites_to("aaaaaaaaaa", "bbbbbbbbbb").unwrap());
        assert!(map.withdrawn_rewrites_to("aaaaaaaaaa", "bbbbbbbbbb"));
        assert!(map.withdrawn_rewrites_to("aaaaaaaaaa", "dddddddddd"));
        assert!(map.withdrawn_rewrites_to("AAAAAAAAAA", "BBBBBBBBBB"));
        assert!(!map.withdrawn_rewrites_to("bbbbbbbbbb", "aaaaaaaaaa"));
        assert!(!map.withdrawn_rewrites_to("cccccccccc", "bbbbbbbbbb"));
        assert!(!map.withdrawn_rewrites_to("eeeeeeeeee", "aaaaaaaaaa"));
    }

    /// The recording path spells a dropped commit as no successor. A payload
    /// that instead maps to the zero object means something no recorded map
    /// can mean, and a reader would repeat it as a rewrite to nothing.
    /// The zero object is how Git spells "no object". A rewrite *from* it
    /// would claim a successor for a commit that never existed.
    #[test]
    fn a_zero_old_revision_is_not_a_revision() {
        assert!(parse_commit_map(
            "0000000000000000000000000000000000000000 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
        )
        .is_err());
        let zero_old = BTreeMap::from([(
            "0000000000000000000000000000000000000000".to_string(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
        )]);
        assert!(validate_mapping(&zero_old).is_err());
    }

    #[test]
    fn a_mapping_that_no_recorded_map_could_mean_is_refused() {
        let zeroed = BTreeMap::from([(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            Some("0000000000000000000000000000000000000000".to_string()),
        )]);
        assert!(validate_mapping(&zeroed).is_err());

        let identity = BTreeMap::from([("aaaaaaaaaa".to_string(), Some("aaaaaaaaaa".to_string()))]);
        assert!(validate_mapping(&identity).is_err());

        let dropped = BTreeMap::from([("aaaaaaaaaa".to_string(), None)]);
        assert!(validate_mapping(&dropped).is_ok());
    }

    #[test]
    fn a_map_with_no_rewritten_revision_is_refused() {
        assert!(parse_commit_map("\n# nothing\n").is_err());
        assert!(parse_commit_map("old new\n").is_err());
        assert!(parse_commit_map("not-a-revision bbbbbbbbbb\n").is_err());
    }
}
