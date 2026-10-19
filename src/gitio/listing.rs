//! Answers to the Git reads a status report makes, gathered for every change
//! a listing reports on by a fixed number of batched Git commands.
//!
//! A listing names what its reports will ask with `prefetch`; each read in
//! `gitio` then takes its answer from here before running Git on its own.
//! Answers are held only while the process shares its reads, which a command
//! does only when it never moves a ref, so a held answer is what Git would
//! still say. A question no batch could answer exactly is left out, and its
//! read runs Git as it would without a listing.

use super::{batch_check, git, git_piped, read_commits, MergeOutcome};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

/// The reads a listing's status reports make, by revision as each read
/// is asked.
#[derive(Debug, Default)]
pub struct Wanted {
    /// `(target, head)`: the merge outcome of head into target, and the
    /// messages of every commit in `target..head`.
    pub merges: Vec<(String, String)>,
    /// `(target, head)`: whether either is an ancestor of the other.
    pub ancestry: Vec<(String, String)>,
    /// Worktrees whose cleanliness is read.
    pub worktrees: Vec<PathBuf>,
}

type Pair = (String, String);
type Messages = Vec<(String, Vec<u8>)>;

#[derive(Default)]
struct Answers {
    merges: HashMap<Pair, MergeOutcome>,
    ancestors: HashMap<Pair, bool>,
    ranges: HashMap<Pair, Messages>,
    changed: HashMap<Pair, Vec<String>>,
}

static ANSWERS: OnceLock<Mutex<HashMap<PathBuf, Answers>>> = OnceLock::new();
static CLEAN: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();

fn with<T>(cwd: &Path, read: impl FnOnce(&mut Answers) -> T) -> T {
    let mut answers = ANSWERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    read(answers.entry(cwd.to_path_buf()).or_default())
}

fn pair(a: &str, b: &str) -> Pair {
    (a.to_string(), b.to_string())
}

/// Answer `wanted` for the repository at `cwd`. Does nothing unless the
/// process shares its reads.
pub fn prefetch(cwd: &Path, wanted: &Wanted) {
    if !crate::store::shares_reads() {
        return;
    }
    let revisions: BTreeSet<&str> = wanted
        .merges
        .iter()
        .chain(&wanted.ancestry)
        .flat_map(|(a, b)| [a.as_str(), b.as_str()])
        .collect();
    let commits = resolve_commits(cwd, revisions.into_iter());
    let resolved = |(a, b): &Pair| Some((commits.get(a)?.clone(), commits.get(b)?.clone()));
    let merges: Vec<(&Pair, Pair)> = wanted
        .merges
        .iter()
        .filter_map(|asked| Some((asked, resolved(asked)?)))
        .collect();
    let ancestry: Vec<(&Pair, Pair)> = wanted
        .ancestry
        .iter()
        .filter_map(|asked| Some((asked, resolved(asked)?)))
        .collect();

    let outcomes = merge_outcomes(cwd, merges.iter().map(|(_, ids)| ids));
    let mut heads_by_target: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (_, (target, head)) in merges.iter().chain(&ancestry) {
        heads_by_target.entry(target).or_default().insert(head);
    }
    let walks: HashMap<&str, Walk> = heads_by_target
        .into_iter()
        .filter_map(|(target, heads)| Some((target, Walk::from(cwd, target, &heads)?)))
        .collect();

    let ranges: Vec<(&Pair, Vec<String>)> = merges
        .iter()
        .filter_map(|(asked, (target, head))| {
            Some((*asked, walks.get(target.as_str())?.range(head)?))
        })
        .collect();
    let listed: BTreeSet<&str> = ranges
        .iter()
        .flat_map(|(_, commits)| commits.iter().map(String::as_str))
        .collect();
    let listed: Vec<&str> = listed.into_iter().collect();
    let messages: HashMap<&str, Vec<u8>> = read_commits(cwd, &listed)
        .map(|read| {
            listed
                .iter()
                .copied()
                .zip(read.into_iter().map(|commit| commit.message))
                .collect()
        })
        .unwrap_or_default();

    with(cwd, |answers| {
        for (asked, ids) in &merges {
            if let Some(outcome) = outcomes.get(ids) {
                answers.merges.insert((*asked).clone(), outcome.clone());
            }
        }
        for ((target, head), (target_id, head_id)) in &ancestry {
            if let Some(walk) = walks.get(target_id.as_str()) {
                let (forward, backward) = walk.ancestry(head_id);
                answers.ancestors.insert(pair(target, head), forward);
                answers.ancestors.insert(pair(head, target), backward);
            }
        }
        for (asked, commits) in &ranges {
            let read: Option<Messages> = commits
                .iter()
                .map(|commit| Some((commit.clone(), messages.get(commit.as_str())?.clone())))
                .collect();
            if let Some(read) = read {
                answers.ranges.insert((*asked).clone(), read);
            }
        }
    });

    let cleanliness = cleanliness(cwd, &wanted.worktrees);
    CLEAN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .extend(cleanliness);
}

/// Answer `git diff --name-only <base>...<head>` for each of `ranges`. Does
/// nothing unless the process shares its reads.
pub fn prefetch_changed_paths(cwd: &Path, ranges: &[Pair]) {
    if !crate::store::shares_reads() || ranges.is_empty() {
        return;
    }
    let changed = super::changed_paths_of(cwd, ranges);
    with(cwd, |answers| answers.changed.extend(changed));
}

pub(super) fn merge_outcome(cwd: &Path, target: &str, head: &str) -> Option<MergeOutcome> {
    with(cwd, |answers| {
        answers.merges.get(&pair(target, head)).cloned()
    })
}

pub(super) fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> Option<bool> {
    with(cwd, |answers| {
        answers.ancestors.get(&pair(ancestor, descendant)).copied()
    })
}

pub(super) fn range_messages(cwd: &Path, base: &str, head: &str) -> Option<Messages> {
    with(cwd, |answers| {
        answers.ranges.get(&pair(base, head)).cloned()
    })
}

pub(super) fn changed_paths(cwd: &Path, base: &str, head: &str) -> Option<Vec<String>> {
    with(cwd, |answers| {
        answers.changed.get(&pair(base, head)).cloned()
    })
}

pub(super) fn is_clean(worktree: &Path) -> Option<bool> {
    CLEAN
        .get()?
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(worktree)
        .copied()
}

/// The commit each revision names, from one `cat-file --batch-check`.
fn resolve_commits<'a>(
    cwd: &Path,
    revisions: impl Iterator<Item = &'a str>,
) -> HashMap<String, String> {
    let names: Vec<String> = revisions.map(|rev| format!("{rev}^{{commit}}")).collect();
    batch_check(cwd, names.iter().map(String::as_str))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, answer)| {
            let (id, kind) = answer.split_once(' ')?;
            let rev = name.strip_suffix("^{commit}")?;
            (kind == "commit").then(|| (rev.to_string(), id.to_string()))
        })
        .collect()
}

/// Each merge's outcome from one `merge-tree --stdin`, which answers every
/// merge with its status (`1` clean, `0` conflicted), the tree, and the
/// conflicted entries, each field ending in NUL and each merge in an empty
/// field. A run that fails answers none of them.
fn merge_outcomes<'a>(
    cwd: &Path,
    merges: impl Iterator<Item = &'a Pair>,
) -> HashMap<Pair, MergeOutcome> {
    let merges: Vec<&Pair> = merges.collect::<BTreeSet<_>>().into_iter().collect();
    if merges.is_empty() {
        return HashMap::new();
    }
    let input: String = merges
        .iter()
        .map(|(target, head)| format!("{target} {head}\n"))
        .collect();
    let Ok(output) = git_piped(
        cwd,
        &["merge-tree", "--write-tree", "--no-messages", "--stdin"],
        input.as_bytes(),
    ) else {
        return HashMap::new();
    };
    let mut fields = output.split(|byte| *byte == 0);
    let mut outcomes = HashMap::new();
    for merge in merges {
        let (Some(status), Some(tree)) = (fields.next(), fields.next()) else {
            return HashMap::new();
        };
        let conflicts = match status {
            b"1" => false,
            b"0" => true,
            _ => return HashMap::new(),
        };
        if fields.by_ref().any(<[u8]>::is_empty) {
            let tree = String::from_utf8_lossy(tree).trim().to_string();
            if tree.is_empty() {
                return HashMap::new();
            }
            outcomes.insert(
                merge.clone(),
                MergeOutcome {
                    conflicts,
                    tree: (!conflicts).then_some(tree),
                },
            );
        } else {
            return HashMap::new();
        }
    }
    outcomes
}

/// Every commit some head reaches and the target does not, with its parents,
/// from one `rev-list --parents`.
///
/// Every commit on a path from a head down to the target descends from the
/// target, so none of them is reachable from it: the target is an ancestor
/// of a head exactly when a commit this walk lists from that head names it as
/// a parent. A head the walk does not list is reachable from the target.
struct Walk {
    target: String,
    parents: HashMap<String, Vec<String>>,
}

impl Walk {
    fn from(cwd: &Path, target: &str, heads: &BTreeSet<&str>) -> Option<Walk> {
        let mut input: String = heads.iter().map(|head| format!("{head}\n")).collect();
        input.push_str(&format!("^{target}\n"));
        let listed =
            git_piped(cwd, &["rev-list", "--parents", "--stdin"], input.as_bytes()).ok()?;
        let parents = String::from_utf8_lossy(&listed)
            .lines()
            .filter_map(|line| {
                let mut ids = line.split_whitespace().map(str::to_string);
                Some((ids.next()?, ids.collect()))
            })
            .collect();
        Some(Walk {
            target: target.to_string(),
            parents,
        })
    }

    /// The commits `head` reaches in this walk, `head` first.
    fn reached<'a>(&'a self, head: &'a str) -> Vec<&'a str> {
        let mut seen = HashSet::new();
        let mut pending = vec![head];
        let mut reached = Vec::new();
        while let Some(commit) = pending.pop() {
            if let Some(parents) = self.parents.get(commit) {
                if seen.insert(commit) {
                    reached.push(commit);
                    pending.extend(parents.iter().map(String::as_str));
                }
            }
        }
        reached
    }

    /// Whether the target is an ancestor of `head`, and whether `head` is an
    /// ancestor of the target, as `merge-base --is-ancestor` answers each.
    fn ancestry(&self, head: &str) -> (bool, bool) {
        if head == self.target {
            return (true, true);
        }
        let forward = self
            .reached(head)
            .into_iter()
            .any(|commit| self.parents[commit].contains(&self.target));
        (forward, !self.parents.contains_key(head))
    }

    /// `rev-list --reverse <target>..<head>`, when the range is one line of
    /// history: then every order Git could walk it in is that line's. A range
    /// with a merge inside it is left to Git.
    fn range(&self, head: &str) -> Option<Vec<String>> {
        let reached = self.reached(head);
        let members: HashSet<&str> = reached.iter().copied().collect();
        let mut line = Vec::with_capacity(reached.len());
        let mut next = self.parents.contains_key(head).then_some(head);
        while let Some(commit) = next {
            line.push(commit.to_string());
            let mut inside = self.parents[commit]
                .iter()
                .map(String::as_str)
                .filter(|parent| members.contains(parent));
            next = inside.next();
            if inside.next().is_some() {
                return None;
            }
        }
        if line.len() != members.len() {
            return None;
        }
        line.reverse();
        Some(line)
    }
}

/// Whether each worktree is clean, as `status --porcelain` answers it, from
/// one `for-each-repo` that runs `status --porcelain=v2 --branch` in each in
/// turn. Every run begins with its `# branch.oid` header, which no entry line
/// can, so the output splits into one answer per worktree in the order they
/// were named. A worktree whose path cannot be one configuration value, or a
/// run that fails or answers a different number of worktrees, answers none.
fn cleanliness(cwd: &Path, worktrees: &[PathBuf]) -> HashMap<PathBuf, bool> {
    const KEY: &str = "arc.listing-worktree";
    let worktrees: Vec<&PathBuf> = worktrees
        .iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut args = Vec::new();
    for worktree in &worktrees {
        match worktree.to_str() {
            Some(path) if !path.contains(['\n', '\r', '\0']) => {
                args.push("-c".to_string());
                args.push(format!("{KEY}={path}"));
            }
            _ => return HashMap::new(),
        }
    }
    if worktrees.is_empty() {
        return HashMap::new();
    }
    args.push("for-each-repo".to_string());
    args.push(format!("--config={KEY}"));
    args.extend(["--", "status", "--porcelain=v2", "--branch"].map(str::to_string));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let Ok(output) = git(cwd, &args) else {
        return HashMap::new();
    };
    let mut answers: Vec<bool> = Vec::new();
    for line in output.lines() {
        if line.starts_with("# branch.oid ") {
            answers.push(true);
        } else if !line.starts_with("# ") && !line.is_empty() {
            match answers.last_mut() {
                Some(clean) => *clean = false,
                None => return HashMap::new(),
            }
        }
    }
    if answers.len() != worktrees.len() {
        return HashMap::new();
    }
    worktrees.into_iter().cloned().zip(answers).collect()
}

#[cfg(test)]
mod tests;
