//! Disk cost of the worktrees arc opened.
//!
//! `begin` creates a worktree per change and each carries a full build, so a
//! session of parallel changes can quietly fill a filesystem while every
//! command reports success. This is the accounting half of that: what the
//! open changes occupy, measured read-only, for `catchup` and `doctor` to
//! surface. Nothing here deletes, moves, or decides anything — a tool that
//! reclaims build output on its own advice would be a different kind of tool.

use crate::state::ChangeState;
use serde::Serialize;
use std::collections::BTreeMap;
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

#[derive(Debug, Serialize, Default)]
pub struct WorktreeAccounting {
    pub changes: Vec<WorktreeUsage>,
    pub total_bytes: Option<u64>,
}

impl WorktreeAccounting {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// Measure every open change's recorded worktree. Two changes sharing one
/// checkout (the `--no-worktree` adoption path) are counted once; the first
/// change id in ledger order owns the row.
pub fn measure(cwd: &Path, states: &BTreeMap<String, ChangeState>) -> WorktreeAccounting {
    let mut registered = std::collections::BTreeSet::new();
    if let Some(output) = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
    {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                registered.insert(path.to_string());
            }
        }
    }

    let mut changes = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut total = 0u64;
    let mut total_known = true;
    for (change_id, state) in states {
        if state.closure.is_some() {
            continue;
        }
        let Some(worktree) = &state.worktree else {
            continue;
        };
        if !seen.insert(worktree.clone()) || !registered.contains(worktree) {
            continue;
        }
        let bytes = du_bytes(Path::new(worktree));
        if let Some(size) = bytes {
            total += size;
        } else {
            total_known = false;
        }
        changes.push(WorktreeUsage {
            change_id: change_id.clone(),
            path: worktree.clone(),
            bytes,
        });
    }
    let measured = if total_known && !changes.is_empty() {
        Some(total)
    } else {
        None
    };
    WorktreeAccounting {
        changes,
        total_bytes: measured,
    }
}

/// Total size of a directory tree, in bytes, via portable `du -sk`. A failed
/// or absent `du` is a silent omission — this is a convenience probe, and
/// every failure mode is "no number", never a wrong one.
fn du_bytes(path: &Path) -> Option<u64> {
    let output = Command::new("du")
        .arg("-sk")
        .arg(path)
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
