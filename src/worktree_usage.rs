//! Disk cost of the worktrees arc opened.
//!
//! `begin` creates a worktree per change and each carries a full build, so a
//! session of parallel changes can quietly fill a filesystem while every
//! command reports success. This is the accounting half of that: what the
//! open changes occupy, measured read-only, for `catchup` and `doctor` to
//! surface. Nothing here deletes, moves, or decides anything — a tool that
//! reclaims build output on its own advice would be a different kind of tool.

use crate::state::ChangeState;
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

/// One open change's worktree and what it occupies. `bytes` stays `None` when
/// the size could not be measured — the worktree is pruned, or `du` is
/// unavailable — rather than inventing a zero a reader would sum.
#[derive(Debug, Serialize)]
pub struct WorktreeUsage {
    pub change_id: String,
    pub path: String,
    pub bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct UnknownWorktree {
    pub change_id: String,
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Default)]
pub struct WorktreeAccounting {
    pub changes: Vec<WorktreeUsage>,
    pub total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<UnknownWorktree>,
}

impl WorktreeAccounting {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.unknown.is_empty()
    }
}

/// Resolve a recorded path against the command cwd, then use its canonical
/// spelling when the path exists so it can be compared with Git's inventory.
fn resolve_path(cwd: &Path, path: &Path) -> std::path::PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    fs::canonicalize(&path).unwrap_or(path)
}

fn git_worktree_inventory(cwd: &Path) -> Result<BTreeSet<std::path::PathBuf>, String> {
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

pub fn measure(cwd: &Path, states: &BTreeMap<String, ChangeState>) -> WorktreeAccounting {
    let candidates = states
        .iter()
        .filter(|(_, state)| state.closure.is_none() && state.worktree.is_some())
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return WorktreeAccounting::default();
    }

    let inventory = git_worktree_inventory(cwd);
    let mut changes = Vec::new();
    let mut unknown = Vec::new();
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
    let measured = if total_known && unknown.is_empty() && !changes.is_empty() {
        Some(total)
    } else {
        None
    };
    WorktreeAccounting {
        changes,
        total_bytes: measured,
        unknown,
    }
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
