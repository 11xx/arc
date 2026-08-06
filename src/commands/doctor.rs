//! Read-only diagnostics for the append-only change ledger.
//!
//! Problems identify malformed authoritative state and fail the command.
//! Advice identifies safe housekeeping or liveness concerns without changing
//! the exit status. Inspection never creates, deletes, or rewrites store data.

use crate::commands::{self, Ctx};
use crate::gitio;
use crate::ids;
use crate::model::{Closure, Event, Payload};
use crate::state::{self, claim_timing_at, ChangeState};
use crate::store::Store;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize)]
struct Finding {
    code: &'static str,
    detail: String,
}

#[derive(Debug, Serialize)]
struct Report {
    schema: &'static str,
    problems: Vec<Finding>,
    advice: Vec<Finding>,
}

pub fn run(ctx: &Ctx, json: bool, verbose: bool) -> Result<i32> {
    let root = Store::resolve_root(&ctx.cwd)?;
    let mut problems = Vec::new();
    let mut advice = Vec::new();
    if Store::repository_id_at(&root).is_err() {
        problems.push(Finding {
            code: "malformed-store-config",
            detail: root.join("config.json").display().to_string(),
        });
    }

    let mut states = BTreeMap::new();
    let mut known_patchsets = BTreeMap::<String, Vec<String>>::new();
    inspect_changes(
        &ctx.cwd,
        &root,
        &mut problems,
        &mut advice,
        &mut states,
        &mut known_patchsets,
    )?;
    inspect_refs(ctx, &states, &known_patchsets, &mut advice)?;
    inspect_closed_worktrees(&ctx.cwd, &states, &mut advice)?;
    inspect_audit_debt(&states, &mut advice);

    let open_states = states
        .iter()
        .filter(|(_, state)| state.closure.is_none())
        .map(|(id, state)| (id.clone(), state.clone()))
        .collect::<BTreeMap<_, _>>();
    if commands::dependency_order(&open_states).is_err() {
        advice.push(Finding {
            code: "dependency-cycle",
            detail: open_states.keys().cloned().collect::<Vec<_>>().join(", "),
        });
    }

    let exit = i32::from(!problems.is_empty());
    let report = Report {
        schema: "arc-doctor/1",
        problems,
        advice,
    };
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        render("problems", &report.problems);
        render_advice(&report.advice, verbose);
    }
    Ok(exit)
}

fn inspect_changes(
    cwd: &Path,
    root: &Path,
    problems: &mut Vec<Finding>,
    advice: &mut Vec<Finding>,
    states: &mut BTreeMap<String, ChangeState>,
    patchsets: &mut BTreeMap<String, Vec<String>>,
) -> Result<()> {
    let changes = root.join("changes");
    let entries = match fs::read_dir(&changes) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", changes.display()))
        }
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let change_id = entry.file_name().to_string_lossy().into_owned();
        if ids::validate_id_component(&change_id).is_err() {
            problems.push(Finding {
                code: "invalid-change-id",
                detail: entry.path().display().to_string(),
            });
            continue;
        }
        inspect_change(
            cwd,
            &change_id,
            &entry.path(),
            problems,
            advice,
            states,
            patchsets,
        )?;
    }
    Ok(())
}

fn inspect_change(
    cwd: &Path,
    change_id: &str,
    change_dir: &Path,
    problems: &mut Vec<Finding>,
    advice: &mut Vec<Finding>,
    states: &mut BTreeMap<String, ChangeState>,
    patchsets: &mut BTreeMap<String, Vec<String>>,
) -> Result<()> {
    let events_dir = change_dir.join("events");
    let entries = match fs::read_dir(&events_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            problems.push(Finding {
                code: "missing-open-event",
                detail: change_dir.display().to_string(),
            });
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", events_dir.display()))
        }
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    let mut events = Vec::new();
    let mut has_open = false;
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if name.ends_with(".tmp") {
            advice.push(Finding {
                code: "orphaned-temporary-file",
                detail: path.display().to_string(),
            });
            continue;
        }
        let Some(event_id) = name.strip_suffix(".json") else {
            continue;
        };
        let parsed = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Event>(&bytes).ok());
        let Some(event) = parsed else {
            problems.push(Finding {
                code: "malformed-event",
                detail: path.display().to_string(),
            });
            continue;
        };
        if ids::validate_id_component(event_id).is_err()
            || ids::validate_id_component(&event.event_id).is_err()
            || ids::validate_id_component(&event.repository_id).is_err()
            || ids::validate_id_component(&event.change_id).is_err()
        {
            problems.push(Finding {
                code: "invalid-event-id",
                detail: path.display().to_string(),
            });
            continue;
        }
        if matches!(event.payload, Payload::Unknown) {
            advice.push(Finding {
                code: "unknown-event-type",
                detail: path.display().to_string(),
            });
            continue;
        }
        has_open |= matches!(event.payload, Payload::ChangeOpened { .. });
        events.push(event);
    }
    if !has_open {
        problems.push(Finding {
            code: "missing-open-event",
            detail: change_dir.display().to_string(),
        });
        return Ok(());
    }
    let Ok(state) = state::reduce(&events) else {
        return Ok(());
    };
    if state.closure.is_none() && !gitio::branch_exists(cwd, &state.branch) {
        advice.push(Finding {
            code: "missing-open-branch",
            detail: format!("{}: refs/heads/{}", change_dir.display(), state.branch),
        });
    }
    if state.closure.is_none() {
        if let Some(claim) = &state.claim {
            let timing = claim_timing_at(claim, Utc::now());
            let overdue = Utc::now()
                .signed_duration_since(timing.expires_at)
                .num_seconds();
            if timing.expired && overdue > claim.ttl_seconds as i64 {
                advice.push(Finding {
                    code: "long-expired-claim",
                    detail: format!("{}: claim {}", change_dir.display(), claim.claim_id),
                });
            }
        }
    }
    patchsets.insert(
        change_id.to_string(),
        state
            .patchsets
            .iter()
            .map(|patchset| patchset.id.clone())
            .collect(),
    );
    states.insert(change_id.to_string(), state);
    Ok(())
}

fn inspect_refs(
    ctx: &Ctx,
    states: &BTreeMap<String, ChangeState>,
    patchsets: &BTreeMap<String, Vec<String>>,
    advice: &mut Vec<Finding>,
) -> Result<()> {
    for (reference, _) in gitio::list_refs(&ctx.cwd, "refs/arc/keep/")? {
        let parts = reference
            .trim_start_matches("refs/arc/keep/")
            .split('/')
            .collect::<Vec<_>>();
        let orphaned = parts.len() != 2
            || !states.contains_key(parts[0])
            || !patchsets
                .get(parts[0])
                .is_some_and(|ids| ids.iter().any(|id| id == parts[1]));
        if orphaned {
            advice.push(Finding {
                code: "orphaned-retention-ref",
                detail: reference,
            });
        }
    }
    Ok(())
}

/// An undischarged review obligation is stale state in the sense doctor
/// reports: nothing is malformed, but work the ledger knows about is waiting
/// on someone, and it is invisible unless asked for by name.
fn inspect_audit_debt(states: &BTreeMap<String, ChangeState>, advice: &mut Vec<Finding>) {
    for (change_id, state) in states {
        if !state.audit_debt_outstanding() {
            continue;
        }
        let reason = state
            .audit_debt
            .as_ref()
            .map(|debt| debt.reason.as_str())
            .unwrap_or_default();
        advice.push(Finding {
            code: "audit-debt-outstanding",
            detail: format!("{change_id}: {reason}"),
        });
    }
}

fn inspect_closed_worktrees(
    cwd: &Path,
    states: &BTreeMap<String, ChangeState>,
    advice: &mut Vec<Finding>,
) -> Result<()> {
    let registered = gitio::git(cwd, &["worktree", "list", "--porcelain"])?
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    for (change_id, state) in states {
        let (Some(closure), Some(worktree)) = (&state.closure, &state.worktree) else {
            continue;
        };
        if registered.contains(worktree) {
            let outcome = match closure.outcome {
                Closure::Integrated => "integrated",
                Closure::Abandoned => "abandoned",
                Closure::Superseded => "superseded",
            };
            advice.push(Finding {
                code: "closed-change-worktree",
                detail: format!("{change_id} [{outcome}]: {worktree}"),
            });
        }
    }
    Ok(())
}

fn render(label: &str, findings: &[Finding]) {
    println!("{label}:");
    if findings.is_empty() {
        println!("  (none)");
    }
    for finding in findings {
        println!("  {}: {}", finding.code, finding.detail);
    }
}

fn render_advice(findings: &[Finding], verbose: bool) {
    println!("advice:");
    if findings.is_empty() {
        println!("  (none)");
        return;
    }
    if verbose {
        for finding in findings {
            println!("  {}: {}", finding.code, finding.detail);
        }
        return;
    }

    let mut counts = BTreeMap::new();
    for finding in findings {
        *counts.entry(finding.code).or_insert(0usize) += 1;
    }
    let mut rendered_groups = BTreeSet::new();
    for finding in findings {
        let count = counts[finding.code];
        if count >= 2 {
            if rendered_groups.insert(finding.code) {
                if let Some(summary) = grouped_summary(finding.code, count) {
                    println!("  {}: {summary}", finding.code);
                    continue;
                }
            } else if grouped_summary(finding.code, count).is_some() {
                continue;
            }
        }
        println!("  {}: {}", finding.code, finding.detail);
    }
}

fn grouped_summary(code: &str, count: usize) -> Option<String> {
    let summary = match code {
        "long-expired-claim" => format!(
            "{count} open changes have claims expired for more than one TTL; \
             run arc doctor --verbose to identify them"
        ),
        "orphaned-temporary-file" => format!(
            "{count} orphaned temporary event files; run arc doctor --verbose to list paths"
        ),
        "unknown-event-type" => {
            format!(
                "{count} unknown event files were skipped; run arc doctor --verbose to list paths"
            )
        }
        "missing-open-branch" => format!(
            "{count} open changes have missing branches; run arc doctor --verbose to identify them"
        ),
        "orphaned-retention-ref" => format!(
            "{count} retention refs do not identify a known patchset; \
             run arc doctor --verbose to list refs"
        ),
        "closed-change-worktree" => format!(
            "{count} registered worktrees belong to closed changes; \
             run arc doctor --verbose to list change/path pairs; remove only with \
             git worktree remove <path>"
        ),
        _ => return None,
    };
    Some(summary)
}
