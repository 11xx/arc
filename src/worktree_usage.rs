//! Disk cost of the worktrees arc opened.
//!
//! `begin` creates a worktree per change and each carries a full build, so a
//! session of parallel changes can quietly fill a filesystem while every
//! command reports success. This is the accounting half of that: what the
//! open changes and the fork checkouts occupy, measured read-only, for
//! `catchup` and `doctor` to surface. Nothing here deletes, moves, or decides
//! anything — a tool that reclaims build output on its own advice would be a
//! different kind of tool.
//!
//! Forks are counted apart from changes rather than folded into them. A fork
//! is outside the change lifecycle, so it is never closed by an integration
//! and is routinely the longest-lived checkout on the disk; a total that
//! silently included it would answer a question nobody asked, and one that
//! silently omitted it would understate the cost of exactly the checkouts
//! that persist.

use crate::state::ChangeState;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The one measurement method implemented: `du`'s apparent size, the bytes
/// the files claim.
const METHOD: &str = "du-apparent";

/// What physical cost is reported as while no method for obtaining it exists.
const PHYSICAL_UNKNOWN: &str = "unknown";

/// Free space below which creating another worktree is worth a word. A full
/// build is a gigabyte or so and several changes run at once, so a mount with
/// less than this is one ordinary session from failing.
const LOW_FREE_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// One open change's worktree and what it occupies. `bytes` stays `None` when
/// the size could not be measured — the worktree is pruned, or `du` is
/// unavailable — rather than inventing a zero a reader would sum.
#[derive(Debug, Serialize)]
pub struct WorktreeUsage {
    pub change_id: String,
    pub path: String,
    pub bytes: Option<u64>,
}

/// One fork's checkout and what it occupies, named by the slug the fork
/// resolver answers with.
#[derive(Debug, Serialize)]
pub struct ForkUsage {
    pub slug: String,
    pub path: String,
    pub bytes: Option<u64>,
}

/// A fork checkout to measure, as the fork resolver reports it. Accounting
/// takes the answer rather than deriving one from a branch or directory name:
/// which fork a checkout belongs to has a single owner, and it is not here.
#[derive(Debug)]
pub struct ForkWorktree {
    pub slug: String,
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct UnknownWorktree {
    pub change_id: String,
    pub path: String,
    pub reason: String,
}

/// How the sizes in this accounting were obtained, and what they are not.
///
/// `du` sums apparent size: the bytes a file claims, not the blocks the
/// filesystem spent on it. Where compression, deduplication, or reflinks are
/// in play the two differ by an unbounded factor, so an apparent total is an
/// upper bound on what deleting the tree returns and never a statement of
/// what it presently occupies. The method travels with every total because a
/// number whose method goes unstated is spent as though it were physical.
#[derive(Debug, Serialize)]
pub struct Measurement {
    /// How the byte counts were obtained.
    pub method: &'static str,
    /// Physical bytes, once a method for this filesystem exists. `unknown`
    /// while none does — a claim arc cannot support is not made quietly.
    pub physical: &'static str,
    /// Why physical cost is unobtainable, when the filesystem names a
    /// specific reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub physical_reason: Option<String>,
    pub filesystem: Filesystem,
}

/// The filesystem holding the worktrees root: where the cost of the next
/// worktree lands, and where the space for it has to already exist.
#[derive(Debug, Serialize)]
pub struct Filesystem {
    /// The mount point, when it could be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
    /// The filesystem type, or `unknown` when no mount table reader answered.
    pub fstype: String,
    /// Whether the mount stores data compressed, when its options say. `None`
    /// is unread rather than uncompressed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compressed: Option<bool>,
    /// Bytes available to an unprivileged writer, when they could be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_bytes: Option<u64>,
}

impl Filesystem {
    fn unknown() -> Self {
        Filesystem {
            mount: None,
            fstype: PHYSICAL_UNKNOWN.to_string(),
            compressed: None,
            free_bytes: None,
        }
    }
}

impl Measurement {
    /// The clause a size line carries so its number is read for what it is:
    /// the method, and the physical cost that is not on offer.
    pub fn caveat(&self) -> String {
        match &self.physical_reason {
            Some(reason) => format!("{}; physical: {} ({reason})", self.method, self.physical),
            None => format!("{}; physical: {}", self.method, self.physical),
        }
    }

    /// The filesystem in one clause, for a reader deciding whether another
    /// worktree fits.
    pub fn root_line(&self) -> Option<String> {
        let mount = self.filesystem.mount.as_deref()?;
        let free = self
            .filesystem
            .free_bytes
            .map(|bytes| format!(", {} free", human(bytes)))
            .unwrap_or_default();
        Some(format!(
            "worktree root: {mount} ({}){free}",
            self.filesystem.fstype
        ))
    }
}

#[derive(Debug, Serialize)]
pub struct WorktreeAccounting {
    pub changes: Vec<WorktreeUsage>,
    /// Fork checkouts, never summed into `total_bytes`.
    pub forks: Vec<ForkUsage>,
    /// The open changes' total, when every one of them was measured.
    pub total_bytes: Option<u64>,
    /// The fork checkouts' total, when every one of them was measured.
    pub fork_total_bytes: Option<u64>,
    pub measurement: Measurement,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<UnknownWorktree>,
}

impl WorktreeAccounting {
    fn empty() -> Self {
        WorktreeAccounting {
            changes: Vec::new(),
            forks: Vec::new(),
            total_bytes: None,
            fork_total_bytes: None,
            measurement: Measurement {
                method: METHOD,
                physical: PHYSICAL_UNKNOWN,
                physical_reason: None,
                filesystem: Filesystem::unknown(),
            },
            unknown: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.forks.is_empty() && self.unknown.is_empty()
    }
}

/// Resolve a recorded path against the command cwd, then use its canonical
/// spelling when the path exists so it can be compared with Git's inventory.
fn resolve_path(cwd: &Path, path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    fs::canonicalize(&path).unwrap_or(path)
}

fn git_worktree_inventory(cwd: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("cannot run git worktree list: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("git worktree list exited with {}", output.status)
        } else {
            format!("git worktree list failed: {detail}")
        });
    }
    let paths = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let line = std::str::from_utf8(line).ok()?;
            line.strip_prefix("worktree ")
                .map(|path| resolve_path(cwd, Path::new(path)))
        })
        .collect::<BTreeSet<_>>();
    if paths.is_empty() {
        return Err("git worktree list returned no worktree paths".to_string());
    }
    Ok(paths)
}

pub fn measure(
    cwd: &Path,
    states: &BTreeMap<String, ChangeState>,
    forks: &[ForkWorktree],
) -> WorktreeAccounting {
    let candidates = states
        .iter()
        .filter(|(_, state)| state.closure.is_none() && state.worktree.is_some())
        .collect::<Vec<_>>();
    if candidates.is_empty() && forks.is_empty() {
        return WorktreeAccounting::empty();
    }

    let inventory = git_worktree_inventory(cwd);
    let mut changes = Vec::new();
    let mut unknown = Vec::new();
    // One set across both lists: a path is measured once, whichever list
    // reaches it first, so no byte is counted in two places.
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    let mut total_known = true;
    for (change_id, state) in candidates {
        let worktree = state.worktree.as_deref().expect("candidate has a worktree");
        let resolved = resolve_path(cwd, Path::new(worktree));
        if !seen.insert(resolved.clone()) {
            continue;
        }
        let path = resolved.display().to_string();
        let Some(registered) = inventory.as_ref().ok() else {
            let reason = inventory
                .as_ref()
                .expect_err("the Ok arm was taken above")
                .clone();
            unknown.push(UnknownWorktree {
                change_id: change_id.clone(),
                path,
                reason,
            });
            total_known = false;
            continue;
        };
        if !registered.contains(&resolved) {
            unknown.push(UnknownWorktree {
                change_id: change_id.clone(),
                path,
                reason: "recorded path does not match Git worktree inventory".to_string(),
            });
            total_known = false;
            continue;
        }
        let bytes = du_bytes(cwd, &resolved);
        if let Some(size) = bytes {
            total += size;
        } else {
            total_known = false;
        }
        changes.push(WorktreeUsage {
            change_id: change_id.clone(),
            path,
            bytes,
        });
    }

    // A fork checkout arrives already resolved against Git's inventory, so it
    // is measured rather than re-validated: the resolver reports a worktree
    // only while Git still calls the path live.
    let mut fork_usage = Vec::new();
    let mut fork_total = 0u64;
    let mut fork_total_known = true;
    for fork in forks {
        let resolved = resolve_path(cwd, &fork.path);
        if !seen.insert(resolved.clone()) {
            continue;
        }
        let bytes = du_bytes(cwd, &resolved);
        if let Some(size) = bytes {
            fork_total += size;
        } else {
            fork_total_known = false;
        }
        fork_usage.push(ForkUsage {
            slug: fork.slug.clone(),
            path: resolved.display().to_string(),
            bytes,
        });
    }

    let measured = if total_known && unknown.is_empty() && !changes.is_empty() {
        Some(total)
    } else {
        None
    };
    let fork_measured = if fork_total_known && !fork_usage.is_empty() {
        Some(fork_total)
    } else {
        None
    };
    WorktreeAccounting {
        changes,
        forks: fork_usage,
        total_bytes: measured,
        fork_total_bytes: fork_measured,
        measurement: measurement(worktrees_root(cwd).as_deref().unwrap_or(cwd)),
        unknown,
    }
}

/// Where new worktrees are created, absolute. A relative configured root is
/// resolved against the invoking checkout, which is how `begin` resolves it.
pub fn worktrees_root(cwd: &Path) -> Option<PathBuf> {
    let root = crate::config::load().ok()?.worktrees_dir;
    Some(if root.is_absolute() {
        root
    } else {
        cwd.join(root)
    })
}

/// How a size for `path`'s filesystem was obtained and what it omits.
pub fn measurement(path: &Path) -> Measurement {
    let filesystem = filesystem(path);
    // Compression is the case where apparent and physical size diverge
    // without bound, so it is named: a reader who knows why the physical
    // number is missing can decide whether the apparent one answers.
    let physical_reason = (filesystem.compressed == Some(true))
        .then(|| format!("{} compression", filesystem.fstype));
    Measurement {
        method: METHOD,
        physical: PHYSICAL_UNKNOWN,
        physical_reason,
        filesystem,
    }
}

/// The filesystem holding `path`. The path need not exist yet — a preflight
/// asks before creating the worktree — so the query lands on the nearest
/// existing ancestor, which shares the mount unless an operator mounted into
/// the directory about to be created. Every read failure is a silent
/// omission: an advisory number must never be invented, and must never block
/// the work it describes.
pub fn filesystem(path: &Path) -> Filesystem {
    let Some(probe) = path.ancestors().find(|candidate| candidate.exists()) else {
        return Filesystem::unknown();
    };
    let mounted = mount_facts(probe);
    let (free_bytes, df_mount) = df_facts(probe);
    Filesystem {
        mount: mounted
            .as_ref()
            .and_then(|facts| facts.target.clone())
            .or(df_mount),
        fstype: mounted
            .as_ref()
            .and_then(|facts| facts.fstype.clone())
            .unwrap_or_else(|| PHYSICAL_UNKNOWN.to_string()),
        compressed: mounted
            .as_ref()
            .and_then(|facts| facts.options.as_deref())
            .map(compresses),
        free_bytes,
    }
}

/// Print what the filesystem about to hold a new worktree has left, and warn
/// when it is running out.
///
/// A filesystem that fills does not announce itself: writes fail inside
/// whatever was running, and the surrounding command reports success with an
/// empty result, which reads as nothing-to-do. The line is advice and never a
/// refusal — arc does not decide that a worktree may not be created.
pub fn report_root_free(root: &Path) {
    let filesystem = filesystem(root);
    let Some(free) = filesystem.free_bytes else {
        return;
    };
    let mount = match filesystem.mount.as_deref() {
        Some(mount) => mount.to_string(),
        None => root.display().to_string(),
    };
    println!("worktree root free: {} on {mount}", human(free));
    if free < LOW_FREE_BYTES {
        println!(
            "warning: {mount} has less than {} free; a worktree carries a full \
             build, and a filesystem that fills reports as success everywhere else",
            human(LOW_FREE_BYTES)
        );
    }
}

struct MountFacts {
    target: Option<String>,
    fstype: Option<String>,
    options: Option<String>,
}

#[derive(Deserialize)]
struct FindmntReply {
    filesystems: Vec<FindmntRow>,
}

#[derive(Deserialize)]
struct FindmntRow {
    target: Option<String>,
    fstype: Option<String>,
    options: Option<String>,
}

/// Mount point, type, and options for the filesystem holding `path`, via
/// `findmnt`. Absent `findmnt` the type is simply unknown: reading the mount
/// table by hand would be a second answer to a question the system already
/// answers, and a wrong filesystem type is worse than none.
fn mount_facts(path: &Path) -> Option<MountFacts> {
    let output = Command::new("findmnt")
        .arg("-J")
        .arg("-T")
        .arg(path)
        .args(["-o", "TARGET,FSTYPE,OPTIONS"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let reply: FindmntReply = serde_json::from_slice(&output.stdout).ok()?;
    let row = reply.filesystems.into_iter().next()?;
    Some(MountFacts {
        target: row.target,
        fstype: row.fstype,
        options: row.options,
    })
}

/// Whether mount options put a compressor between the bytes a file claims and
/// the blocks it costs.
fn compresses(options: &str) -> bool {
    options.split(',').any(|option| {
        let name = option.split('=').next().unwrap_or(option);
        name == "compress" || name == "compress-force"
    })
}

/// Available bytes and mount point for the filesystem holding `path`, via
/// portable `df -kP`: the mount that matters rather than the one that happens
/// to be free. `-k` is explicit because `-P` alone leaves the block size to
/// the implementation.
fn df_facts(path: &Path) -> (Option<u64>, Option<String>) {
    let Some(output) = Command::new("df")
        .arg("-kP")
        .arg(path)
        .output()
        .ok()
        .filter(|output| output.status.success())
    else {
        return (None, None);
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(line) = text.lines().nth(1) else {
        return (None, None);
    };
    let columns: Vec<&str> = line.split_whitespace().collect();
    // POSIX `df -P`: Filesystem, blocks, Used, Available, Capacity, Mounted
    // on. The mount point is the remainder of the line, so a path with spaces
    // in it survives.
    let free = columns
        .get(3)
        .and_then(|blocks| blocks.parse::<u64>().ok())
        .map(|blocks| blocks * 1024);
    let mount = (columns.len() > 5).then(|| columns[5..].join(" "));
    (free, mount)
}

/// Total size of a directory tree, in bytes, via portable `du -sk`. A failed
/// or absent `du` remains an unknown size, never a zero a reader would sum.
fn du_bytes(cwd: &Path, path: &Path) -> Option<u64> {
    let output = Command::new("du")
        .arg("-sk")
        .arg(path)
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let line = String::from_utf8_lossy(&output.stdout);
    let blocks = line.split_whitespace().next()?.parse::<u64>().ok()?;
    Some(blocks * 1024)
}

/// Human-readable bytes, binary units, one decimal below gibibytes.
pub fn human(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    match bytes {
        b if b >= GIB => format!("{:.1}G", b as f64 / GIB as f64),
        b if b >= MIB => format!("{:.1}M", b as f64 / MIB as f64),
        b if b >= KIB => format!("{:.0}K", b as f64 / KIB as f64),
        b => format!("{b}B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_options_are_recognized_by_name_not_by_substring() {
        assert!(compresses("rw,relatime,compress=zstd:1,ssd"));
        assert!(compresses("rw,compress-force=zlib"));
        assert!(compresses("rw,compress"));
        assert!(!compresses("rw,relatime,discard=async"));
        // `nocompress` and `compression_hint` are not the option that turns
        // apparent size into a guess.
        assert!(!compresses("rw,nocompress"));
        assert!(!compresses("rw,compression_hint=none"));
    }

    #[test]
    fn a_compressed_mount_says_why_physical_cost_is_unavailable() {
        let measurement = Measurement {
            method: METHOD,
            physical: PHYSICAL_UNKNOWN,
            physical_reason: Some("btrfs compression".to_string()),
            filesystem: Filesystem::unknown(),
        };
        assert_eq!(
            measurement.caveat(),
            "du-apparent; physical: unknown (btrfs compression)"
        );
    }

    #[test]
    fn a_measurement_without_a_reason_still_names_its_method() {
        let measurement = Measurement {
            method: METHOD,
            physical: PHYSICAL_UNKNOWN,
            physical_reason: None,
            filesystem: Filesystem::unknown(),
        };
        assert_eq!(measurement.caveat(), "du-apparent; physical: unknown");
    }
}
