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
    // Before anything that reads them: a malformed repository event must be
    // reported as the problem it is, not surface as a fatal error inside
    // whatever happened to read it first — which is what doctor exists to
    // prevent for every other kind of event.
    inspect_repository_events(&Store::discover(&ctx.cwd)?, &mut problems);
    inspect_dangling_revisions(&ctx.cwd, &Store::discover(&ctx.cwd)?, &states, &mut advice)?;
    inspect_refs(ctx, &states, &known_patchsets, &mut advice)?;
    inspect_danger_paths(&ctx.cwd, &mut problems);
    inspect_closed_worktrees(&ctx.cwd, &states, &mut advice)?;
    inspect_audit_debt(&states, &mut advice);
    inspect_hold_releases(&Store::discover(&ctx.cwd)?, &states, &mut advice);

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

/// Recorded revisions that Git can no longer resolve.
///
/// A history rewrite leaves the ledger intact and its evidence unreachable:
/// every recorded SHA still says what was verified and reviewed, and none of
/// it can be checked out. That is advice rather than a problem — the ledger is
/// not malformed, and the rewrite was somebody's deliberate act — but it is
/// the difference between evidence and a claim, so it has to be visible
/// without writing a bespoke script.
fn inspect_dangling_revisions(
    cwd: &Path,
    store: &Store,
    states: &BTreeMap<String, ChangeState>,
    advice: &mut Vec<Finding>,
) -> Result<()> {
    let mut wanted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (change_id, state) in states {
        // Every revision the ledger records, because the point is that the
        // evidence still resolves, and a hexadecimal string is what Git will
        // be asked to resolve however short it is.
        let mut note = |revision: &str, what: String| {
            let revision = revision.trim();
            if !revision.is_empty() && revision.chars().all(|c| c.is_ascii_hexdigit()) {
                wanted
                    .entry(revision.to_string())
                    .or_default()
                    .insert(format!("{change_id} {what}"));
            }
        };
        note(&state.base, "base".to_string());
        for patchset in &state.patchsets {
            note(&patchset.head, format!("{} head", patchset.id));
            note(&patchset.base, format!("{} base", patchset.id));
            if let Some(merge_base) = patchset.merge_base.as_deref() {
                note(merge_base, format!("{} merge-base", patchset.id));
            }
        }
        for brief in &state.briefs {
            if let Some(base) = brief.base_revision.as_deref() {
                note(base, "brief base".to_string());
            }
        }
        for verification in &state.verifications {
            note(&verification.revision, "verification".to_string());
        }
        for run in &state.verification_runs {
            note(&run.revision, "verification run".to_string());
        }
        for audit in &state.audit_verdicts {
            note(&audit.revision, "audit".to_string());
        }
        for finding in state.findings.values().chain(state.audit_findings.values()) {
            for disposition in &finding.dispositions {
                if let Some(commit) = disposition.commit.as_deref() {
                    note(commit, format!("{} disposition", finding.id));
                }
            }
        }
        if let Some(commit) = state
            .closure
            .as_ref()
            .and_then(|closure| closure.integrated_commit.as_deref())
        {
            note(commit, "integration".to_string());
        }
        // A forge records revisions too, and a rewritten branch strands them
        // exactly as it strands a local one. These come from the events rather
        // than from reduced state, because forge facts are latest-wins: an
        // earlier head is still recorded, and still names something that was
        // supposed to exist.
        // A malformed event file is already reported as its own problem, and
        // a scan for unreachable revisions should not be the thing that fails
        // because of it.
        for event in store.load_events(change_id).unwrap_or_default() {
            match &event.payload {
                Payload::ForgeLink { head_sha, .. } => {
                    note(head_sha, "forge link head".to_string())
                }
                Payload::ForgeChecks { pr_head, .. } => {
                    note(pr_head, "forge checks head".to_string())
                }
                Payload::ForgePrState {
                    merge_sha: Some(merge_sha),
                    ..
                } => note(merge_sha, "forge merge".to_string()),
                _ => {}
            }
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    // One batch rather than a process per revision: a ledger of any age holds
    // thousands, and a doctor that costs a minute stops being run.
    // A map that cannot be read is reported by `inspect_repository_events`;
    // here it means only that no revision can be followed forward, which is
    // the same answer as no rewrite having been recorded.
    let rewrites = crate::rewrite::RewriteMap::load(store).unwrap_or_default();
    for revision in gitio::missing_objects(cwd, wanted.keys().map(String::as_str))? {
        let short = &revision[..revision.len().min(8)];
        let referents = wanted
            .get(&revision)
            .map(|set| set.iter().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        // A revision a recorded rewrite moved is not a casualty: the event
        // still says what it said, and the reader can follow it forward.
        match rewrites.fate(&revision) {
            Some(crate::rewrite::Fate::Rewritten(successor)) => advice.push(Finding {
                code: "revision-rewritten",
                detail: format!(
                    "{short} was rewritten to {}: {referents}",
                    &successor[..successor.len().min(8)]
                ),
            }),
            Some(crate::rewrite::Fate::Dropped) => advice.push(Finding {
                code: "revision-dropped",
                detail: format!("{short} was dropped by a recorded rewrite: {referents}"),
            }),
            None => advice.push(Finding {
                code: "dangling-revision",
                detail: format!(
                    "{short} is recorded but no longer in this repository: {referents}"
                ),
            }),
        }
    }
    Ok(())
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
/// A declared dangerous path that names nothing.
///
/// This fails in the worst direction: the entry reads as coverage while the
/// surface it was meant to protect stays on a self-recorded verdict. A rename
/// is enough to cause it, and nothing else would ever say so. Only literals
/// are checked — a wildcard legitimately matches nothing today.
fn inspect_danger_paths(cwd: &Path, problems: &mut Vec<Finding>) {
    let Ok(toplevel) = crate::gitio::toplevel(cwd) else {
        return;
    };
    let Ok(policy) = crate::policy::load(&toplevel) else {
        return;
    };
    for pattern in &policy.danger.paths {
        if pattern.contains('*') || pattern.ends_with('/') {
            continue;
        }
        if !toplevel.join(pattern).exists() {
            problems.push(Finding {
                code: "danger-path-matches-nothing",
                detail: format!(
                    "{pattern} is declared dangerous but does not exist; \
                     the surface it names is on a self-verdict"
                ),
            });
        }
    }
}

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

/// A release that names no active hold. Replay ignores it so the ledger stays
/// readable; this is where it becomes visible, because it means either a hold
/// was released twice or the events do not say what somebody thought.
fn inspect_hold_releases(
    store: &Store,
    states: &BTreeMap<String, ChangeState>,
    advice: &mut Vec<Finding>,
) {
    for change_id in states.keys() {
        let mut active: BTreeSet<String> = BTreeSet::new();
        for event in store.load_events(change_id).unwrap_or_default() {
            match &event.payload {
                Payload::HoldSet { .. } => {
                    active.insert(event.event_id.clone());
                }
                Payload::HoldReleased {
                    hold_event_id: Some(id),
                    ..
                } => {
                    if !active.remove(id) {
                        advice.push(Finding {
                            code: "hold-release-names-no-hold",
                            detail: format!(
                                "{change_id}: release {} names {id}, which was not an active hold",
                                event.event_id
                            ),
                        });
                    }
                }
                Payload::HoldReleased { .. } => active.clear(),
                _ => {}
            }
        }
    }
}

/// Repository-scoped events, checked the way a change's events are: a
/// malformed one must be reported as a problem rather than surfacing as a
/// failure inside whatever first tried to read it.
fn inspect_repository_events(store: &Store, problems: &mut Vec<Finding>) {
    let files = match store.repository_event_files() {
        Ok(files) => files,
        Err(error) => {
            problems.push(Finding {
                code: "malformed-repository-event",
                detail: error.to_string(),
            });
            return;
        }
    };
    for (event_id, bytes) in files {
        // An ID that could not have been written is still on disk, and every
        // path that reads by ID validates: reporting it is how the owner
        // learns why those paths refuse the file.
        if let Err(error) = crate::ids::validate_id_component(&event_id) {
            problems.push(Finding {
                code: "malformed-repository-event",
                detail: format!("{event_id}: {error}"),
            });
            // Every later check would report the same cause; one finding per
            // broken file is what makes the report readable.
            continue;
        }
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(error) => {
                problems.push(Finding {
                    code: "malformed-repository-event",
                    detail: format!("{event_id}: {error}"),
                });
                continue;
            }
        };
        if let Err(error) = crate::bundle::parse_typed_event(&value) {
            problems.push(Finding {
                code: "malformed-repository-event",
                detail: format!("{event_id}: {error}"),
            });
            // Everything below reads fields this file does not have, and
            // would report the same broken file two or three more times.
            continue;
        }
        if let Ok(Some(event)) = crate::bundle::parse_typed_event(&value) {
            if let Payload::HistoryRewritten { mapping, .. } = &event.payload {
                if let Err(error) = crate::rewrite::validate_mapping(mapping) {
                    problems.push(Finding {
                        code: "invalid-rewrite-mapping",
                        detail: format!("{event_id}: {error}"),
                    });
                }
            }
        }
        let scope = value.get("change_id").and_then(serde_json::Value::as_str);
        if scope != Some(Store::REPOSITORY_SCOPE) {
            problems.push(Finding {
                code: "misscoped-repository-event",
                detail: format!(
                    "{event_id} is stored as a repository event but names change {}",
                    scope.unwrap_or("(none)")
                ),
            });
        }
        if value.get("event_id").and_then(serde_json::Value::as_str) != Some(event_id.as_str()) {
            problems.push(Finding {
                code: "malformed-repository-event",
                detail: format!("{event_id} contains a different event_id"),
            });
        }
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
