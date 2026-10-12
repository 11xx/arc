//! How much of a change's work is still in the code: the share of the lines
//! its diff added that `git blame` still attributes to the change's own
//! commits at a revision.
//!
//! The answer is a reading of git at one revision, cached once per change
//! and reused at any later revision that has not touched a path the blame
//! read, so a moving HEAD does not blame every change again. A
//! change whose range or files cannot be read is unknown, with a fixed reason;
//! it is never a zero, because a zero says nothing is left to refresh.

use crate::gitio;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// The on-disk cache layout; a file of another layout is ignored.
const CACHE_FORMAT: u32 = 2;

/// Added lines of a change, and how many of them survive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Survival {
    pub added: u64,
    pub live: u64,
}

impl Survival {
    /// The surviving share. A change that added no lines has nothing to
    /// refresh and nothing to measure, so it has no fraction.
    pub fn fraction(&self) -> Option<f64> {
        (self.added > 0).then(|| self.live.min(self.added) as f64 / self.added as f64)
    }
}

pub type Reason = &'static str;

pub const RANGE_UNREADABLE: Reason = "range-unreadable";
pub const NOT_REACHABLE_FROM_HEAD: Reason = "not-reachable-from-head";
pub const BLAME_FAILED: Reason = "blame-failed";
pub const NO_ADDED_LINES: Reason = "no-added-lines";

/// What one change's blame came to, and what it depended on. One entry per
/// change, rewritten in place as HEAD moves.
#[derive(Serialize, Deserialize)]
struct CacheEntry {
    format: u32,
    /// The revision the blame was read at.
    head: String,
    change_id: String,
    /// Every path the blame read, as the change named it and as `head` names
    /// it. A later commit touching none of them cannot have moved a live line.
    paths: Vec<String>,
    #[serde(flatten)]
    survival: Survival,
}

fn cache_path(directory: &Path, change_id: &str) -> PathBuf {
    directory.join(format!("{change_id}.json"))
}

fn cached(directory: &Path, change_id: &str) -> Option<CacheEntry> {
    let bytes = std::fs::read(cache_path(directory, change_id)).ok()?;
    let entry: CacheEntry = serde_json::from_slice(&bytes).ok()?;
    (entry.format == CACHE_FORMAT && entry.change_id == change_id).then_some(entry)
}

/// Every path a diff between two revisions names, either side of a rename.
fn paths_touched(cwd: &Path, from: &str, to: &str) -> anyhow::Result<HashSet<String>> {
    let out = gitio::git(cwd, &["diff", "-z", "--name-status", "-M", from, to])?;
    let mut touched = HashSet::new();
    let mut fields = out.split('\0');
    while let Some(status) = fields.next() {
        let Some(kind) = status.chars().next() else {
            continue;
        };
        let names = if matches!(kind, 'R' | 'C') { 2 } else { 1 };
        for _ in 0..names {
            if let Some(path) = fields.next() {
                touched.insert(path.to_string());
            }
        }
    }
    Ok(touched)
}

/// Whether the entry still describes `head`: it was read at an ancestor, and
/// nothing between touched a path it read. Anything unreadable is a no.
fn holds_at(cwd: &Path, entry: &CacheEntry, head: &str) -> bool {
    if entry.head == head {
        return true;
    }
    gitio::is_ancestor(cwd, &entry.head, head).unwrap_or(false)
        && paths_touched(cwd, &entry.head, head)
            .is_ok_and(|touched| entry.paths.iter().all(|path| !touched.contains(path)))
}

/// Best effort: a cache that cannot be written costs the next read a blame.
/// Writing an entry retires every file named by revision that earlier
/// layouts kept for the same change.
fn remember(directory: &Path, entry: &CacheEntry) {
    let Ok(bytes) = serde_json::to_vec(entry) else {
        return;
    };
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }
    let path = cache_path(directory, &entry.change_id);
    let staged = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&staged, bytes).is_ok() && std::fs::rename(&staged, path).is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    let suffix = format!("-{}.json", entry.change_id);
    for file in std::fs::read_dir(directory).into_iter().flatten().flatten() {
        let name = file.file_name().to_string_lossy().to_string();
        if name.strip_suffix(&suffix).is_some_and(is_object_name) {
            let _ = std::fs::remove_file(file.path());
        }
    }
}

/// The lines `base...tip` added, per path as the tip names it.
fn added_lines(cwd: &Path, base: &str, tip: &str) -> anyhow::Result<BTreeMap<String, u64>> {
    let out = gitio::git(
        cwd,
        &["diff", "-z", "--numstat", "-M", &format!("{base}...{tip}")],
    )?;
    let mut added = BTreeMap::new();
    let mut fields = out.split('\0');
    while let Some(record) = fields.next() {
        let mut parts = record.splitn(3, '\t');
        let (Some(plus), Some(_minus), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // A rename lists its two paths as the fields after an empty one.
        let path = if path.is_empty() {
            let _old = fields.next();
            match fields.next() {
                Some(new) => new,
                None => break,
            }
        } else {
            path
        };
        // Binary files print `-` and have no lines to survive.
        if let Ok(count) = plus.parse::<u64>() {
            *added.entry(path.to_string()).or_default() += count;
        }
    }
    Ok(added)
}

/// Where each path of `tip` is at `head`: `None` when `head` no longer has it.
fn paths_at_head(cwd: &Path, tip: &str, head: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut moved = BTreeMap::new();
    if tip == head {
        return Ok(moved);
    }
    let out = gitio::git(cwd, &["diff", "-z", "--name-status", "-M", tip, head])?;
    let mut fields = out.split('\0');
    while let Some(status) = fields.next() {
        let Some(kind) = status.chars().next() else {
            continue;
        };
        let Some(path) = fields.next() else {
            break;
        };
        match kind {
            'R' | 'C' => {
                let Some(new) = fields.next() else { break };
                if kind == 'R' {
                    moved.insert(path.to_string(), new.to_string());
                }
            }
            'D' => {
                moved.insert(path.to_string(), String::new());
            }
            _ => {}
        }
    }
    Ok(moved)
}

fn is_object_name(token: &str) -> bool {
    matches!(token.len(), 40 | 64) && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// How many lines of `path` at `head` blame to a commit in `own`.
fn surviving_in(cwd: &Path, head: &str, path: &str, own: &HashSet<String>) -> anyhow::Result<u64> {
    let out = gitio::git(cwd, &["blame", "--line-porcelain", head, "--", path])?;
    Ok(out
        .lines()
        .filter(|line| {
            let mut tokens = line.split(' ');
            match (tokens.next(), tokens.next(), tokens.next()) {
                (Some(sha), Some(orig), Some(fin)) => {
                    is_object_name(sha)
                        && orig.bytes().all(|byte| byte.is_ascii_digit())
                        && fin.bytes().all(|byte| byte.is_ascii_digit())
                        && own.contains(sha)
                }
                _ => false,
            }
        })
        .count() as u64)
}

/// The survival of a change and every path the measure read.
fn measure(
    cwd: &Path,
    head: &str,
    base: &str,
    tip: &str,
) -> Result<(Survival, Vec<String>), Reason> {
    if !gitio::is_ancestor(cwd, tip, head).map_err(|_| RANGE_UNREADABLE)? {
        return Err(NOT_REACHABLE_FROM_HEAD);
    }
    let own = gitio::git(cwd, &["rev-list", &format!("{base}..{tip}")])
        .map_err(|_| RANGE_UNREADABLE)?
        .lines()
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let added = added_lines(cwd, base, tip).map_err(|_| RANGE_UNREADABLE)?;
    let moved = paths_at_head(cwd, tip, head).map_err(|_| RANGE_UNREADABLE)?;
    let mut survival = Survival { added: 0, live: 0 };
    let mut paths = std::collections::BTreeSet::new();
    for (path, count) in &added {
        survival.added += count;
        paths.insert(path.clone());
        let at_head = moved.get(path).map_or(path.as_str(), String::as_str);
        if at_head.is_empty() {
            continue;
        }
        paths.insert(at_head.to_string());
        survival.live += surviving_in(cwd, head, at_head, &own).map_err(|_| BLAME_FAILED)?;
    }
    if survival.added == 0 {
        return Err(NO_ADDED_LINES);
    }
    Ok((survival, paths.into_iter().collect()))
}

/// The survival of the change whose work is `base...tip`, read at `head`.
/// `directory` holds one entry per change, reused for as long as no commit
/// since it was read has touched a path it read.
pub fn survival(
    cwd: &Path,
    directory: &Path,
    head: &str,
    change_id: &str,
    base: &str,
    tip: &str,
) -> Result<Survival, Reason> {
    if let Some(mut entry) = cached(directory, change_id) {
        if holds_at(cwd, &entry, head) {
            if entry.head != head {
                entry.head = head.to_string();
                remember(directory, &entry);
            }
            return Ok(entry.survival);
        }
    }
    let (measured, paths) = measure(cwd, head, base, tip)?;
    remember(
        directory,
        &CacheEntry {
            format: CACHE_FORMAT,
            head: head.to_string(),
            change_id: change_id.to_string(),
            paths,
            survival: measured,
        },
    );
    Ok(measured)
}
