//! Read-only diagnostics for the append-only change ledger.
//!
//! Problems identify malformed authoritative state and fail the command.
//! Advice identifies safe housekeeping or liveness concerns without changing
//! the exit status. Inspection never creates, deletes, or rewrites store data.

use crate::commands::{self, Ctx};
use crate::config;
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
    build: crate::context::BuildRevision,
    roots: Roots,
    replica: Option<crate::replica::ReplicaStatus>,
    unfinished_provisional_integrations: Vec<super::provisional::PendingTransition>,
    problems: Vec<Finding>,
    advice: Vec<Finding>,
}

struct ChangeInspection<'a> {
    cwd: &'a Path,
    local_repository_id: &'a str,
    rewrites: crate::rewrite::RewriteMap,
    /// The repository events, whose completed landings close their members.
    repository_events: Vec<crate::model::Event>,
    /// Every inspected change shares the receiver permissions of this read.
    scope: std::sync::Arc<crate::import_authorization::PrivilegeScope>,
    /// The clearance rule the store projects every change under.
    review_clearance: crate::policy::ReviewClearance,
}

/// Where this invocation reads and writes. Every other diagnostic here is a
/// statement about state at these paths, so a reader who disagrees with a
/// finding needs to know which state was inspected — and under a sandbox that
/// is not where the same command yesterday looked.
#[derive(Debug, Serialize)]
struct Roots {
    /// The prefix standing in for the home directory, when one is in force.
    sandbox: Option<String>,
    ai_home: String,
    config_file: String,
    worktrees_dir: String,
    /// The ledger for the repository this ran in.
    ledger: String,
    journal_dir: Option<String>,
}

impl Roots {
    fn resolve(cwd: &Path, ledger: &Path) -> Result<Roots> {
        let cfg = crate::config::load()?;
        Ok(Roots {
            sandbox: cfg.sandbox.as_ref().map(|path| path.display().to_string()),
            ai_home: cfg.ai_home.display().to_string(),
            config_file: cfg.config_path.display().to_string(),
            worktrees_dir: cfg.worktrees_dir.display().to_string(),
            ledger: ledger.display().to_string(),
            // A project whose journal cannot be anchored has no journal root
            // to report, which is a fact about this directory rather than a
            // failure of the diagnostic.
            journal_dir: crate::journal::resolve_dir(cwd)
                .ok()
                .map(|dir| dir.display().to_string()),
        })
    }

    fn render(&self) {
        println!("roots:");
        match &self.sandbox {
            Some(prefix) => println!("  sandbox: {prefix}"),
            None => println!("  sandbox: (none)"),
        }
        println!("  ai-home: {}", self.ai_home);
        println!("  config-file: {}", self.config_file);
        println!("  worktrees-dir: {}", self.worktrees_dir);
        println!("  ledger: {}", self.ledger);
        println!(
            "  journal-dir: {}",
            self.journal_dir.as_deref().unwrap_or("(unanchored)")
        );
    }
}

pub fn run(ctx: &Ctx, json: bool, verbose: bool) -> Result<i32> {
    let root = Store::resolve_root(&ctx.cwd)?;
    let local_repository_id = Store::repository_id_at(&root)
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut problems: Vec<Finding> = crate::git_health::problems(&ctx.cwd)
        .into_iter()
        .map(|finding| Finding {
            code: finding.code,
            detail: finding.detail,
        })
        .collect();
    let mut advice = Vec::new();
    if Store::repository_id_at(&root).is_err() {
        problems.push(Finding {
            code: "malformed-store-config",
            detail: root.join("config.json").display().to_string(),
        });
    }

    let mut states = BTreeMap::new();
    let mut readable = BTreeMap::new();
    let mut known_patchsets = BTreeMap::<String, Vec<String>>::new();
    let store = ctx.store()?;
    match store.privilege_scope() {
        Ok(scope) => {
            // Repository diagnostics report unreadable events and maps
            // independently of changes.
            let repository_events = store.load_repository_events().unwrap_or_default();
            let rewrites = crate::rewrite::RewriteMap::from_permitted_events(
                repository_events.iter(),
                |event| {
                    scope.permits(
                        &event.event_id,
                        &event.repository_id,
                        crate::import_authorization::Role::Rewrite,
                    )
                },
            )
            .unwrap_or_default();
            inspect_changes(
                ChangeInspection {
                    cwd: &ctx.cwd,
                    local_repository_id: &local_repository_id,
                    rewrites,
                    repository_events,
                    scope,
                    review_clearance: store.review_clearance,
                },
                &root,
                &mut problems,
                &mut advice,
                Inspected {
                    states: &mut states,
                    readable: &mut readable,
                    patchsets: &mut known_patchsets,
                },
            )?;
        }
        Err(error) => problems.push(Finding {
            code: "authorization-policy-unreadable",
            detail: format!("cannot inspect change authorization: {error:#}"),
        }),
    }
    // Before anything that reads them: a malformed repository event must be
    // reported as the problem it is, not surface as a fatal error inside
    // whatever happened to read it first — which is what doctor exists to
    // prevent for every other kind of event.
    let unfinished_provisional_integrations =
        match super::provisional::pending_transitions(&ctx.cwd, &store) {
            Ok(transitions) => {
                for transition in &transitions {
                    problems.push(Finding {
                        code: "unfinished-provisional-integration",
                        detail: transition.describe(),
                    });
                }
                transitions
            }
            Err(error) => {
                problems.push(Finding {
                    code: "unreadable-provisional-intent",
                    detail: format!("{error:#}"),
                });
                Vec::new()
            }
        };
    inspect_repository_events(&ctx.cwd, &store, &mut problems);
    inspect_dangling_revisions(&ctx.cwd, &store, &states, &mut problems, &mut advice)?;
    inspect_refs(ctx, &states, &known_patchsets, &mut advice)?;
    inspect_retention(ctx, &store, &mut problems, &mut advice)?;
    inspect_custody(&store, &mut problems, &mut advice);
    inspect_receipts(&store, &states, &mut advice);
    inspect_candidates(ctx, &store, &mut problems, &mut advice)?;
    // Doctor diagnoses the invoking checkout, including uncommitted
    // declarations and tracked files. A change's target would hide local
    // mistakes until they reached that branch.
    inspect_gate_conflicts(&ctx.cwd, &mut problems, &mut advice);
    for state in states.values().filter(|s| !s.is_closed()) {
        if let Ok(declarations) = ctx.declarations(state) {
            for detail in declarations.gates.budget_errors() {
                problems.push(Finding {
                    code: "gate-budget-invalid",
                    detail: format!("{}: {detail}", state.change_id),
                });
            }
        }
    }
    let toplevel = crate::git_health::checkout_root(&ctx.cwd)?;
    let mut landing_costs = super::gate_reports::landing::Costs::new(&toplevel, true)?;
    for (id, events) in &readable {
        let integrated_at = states
            .get(id)
            .and_then(|state| state.closure.as_ref())
            .filter(|closure| closure.outcome == crate::model::Closure::Integrated)
            .map(|closure| closure.created_at);
        landing_costs.observe(id, events, integrated_at);
    }
    if let Some(detail) = landing_costs
        .summarize(&toplevel, true, Utc::now())?
        .over_budget()
    {
        advice.push(Finding {
            code: "landing-budget-exceeded",
            detail,
        });
    }
    match crate::policy::load(&toplevel) {
        Ok(policy) => {
            inspect_danger_paths(&toplevel, &policy, &mut problems);
            inspect_danger_classification(&toplevel, &policy, &mut problems);
            inspect_preparation_guard(&toplevel, &policy, &mut advice);
        }
        Err(error) => problems.push(Finding {
            code: "policy-unreadable",
            detail: format!("{error:#}"),
        }),
    }
    inspect_closed_worktrees(&ctx.cwd, &states, &mut advice)?;
    // Debt standing is judged as every other view reads it: the store's
    // states follow recorded rewrites and carry the review context, where the
    // states above keep recorded revisions for the dangling-object checks.
    match store.readable_states() {
        Ok(ledger) => {
            inspect_debt(ctx, &ledger, &mut advice)?;
            inspect_clearance(&ledger, &mut advice);
        }
        Err(error) => advice.push(Finding {
            code: "debt-unjudged",
            detail: format!("outstanding debt cannot be counted: {error:#}"),
        }),
    }
    inspect_hold_releases(&readable, &mut advice);
    let replica = match crate::replica::status(&store) {
        Ok(status) => Some(status),
        Err(error) => {
            problems.push(Finding {
                code: "invalid-replica-state",
                detail: format!("{error:#}"),
            });
            None
        }
    };
    let forks = crate::commands::fork::list_entries(ctx).unwrap_or_default();
    inspect_worktree_accounting(
        &ctx.cwd,
        &states,
        &crate::commands::fork::checkouts(&forks),
        &mut advice,
    );

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
        schema: "arc-doctor/6",
        build: crate::context::build_revision(),
        roots: Roots::resolve(&ctx.cwd, &root)?,
        replica,
        unfinished_provisional_integrations,
        problems,
        advice,
    };
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("arc build: {}", report.build.label());
        report.roots.render();
        match &report.replica {
            Some(replica) => crate::replica::render_status(replica),
            None => println!("replica: unavailable (see invalid-replica-state problem)"),
        }
        render("problems", &report.problems);
        render_advice(&report.advice, verbose);
    }
    Ok(exit)
}

/// Recorded revisions that Git can no longer resolve: those a change's events
/// record, and the revision and change heads each checkpoint recorded.
///
/// A rewrite leaves the ledger intact and its evidence unreachable: every
/// recorded SHA still says what was verified and reviewed, and none of it can
/// be checked out. Where a recorded rewrite explains it, that is advice — the
/// ledger is not malformed, the rewrite was somebody's deliberate act, and a
/// reader can follow the revision forward.
///
/// The exception is a revision a rewrite claims to have moved to a commit
/// that is not here either. Nothing about that record can be followed
/// anywhere, and reporting it as a rewrite would suppress the warning while
/// naming a successor nobody can read.
fn inspect_dangling_revisions(
    cwd: &Path,
    store: &Store,
    states: &BTreeMap<String, ChangeState>,
    problems: &mut Vec<Finding>,
    advice: &mut Vec<Finding>,
) -> Result<()> {
    let mut wanted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (change_id, state) in states {
        for (revision, what) in state.recorded_revisions() {
            wanted
                .entry(revision)
                .or_default()
                .insert(format!("{change_id} {what}"));
        }
    }
    // An unreadable repository event is reported by
    // `inspect_repository_events`, and names no revision here.
    for event in store.load_repository_events().unwrap_or_default() {
        for (what, revision) in crate::retention::checkpoint_commits(&event.payload) {
            wanted.entry(revision.to_string()).or_default().insert(what);
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    // A map that cannot be read is reported by `inspect_repository_events`;
    // here it means only that no revision can be followed forward, which is
    // the same answer as no rewrite having been recorded.
    let rewrites = crate::rewrite::RewriteMap::load(store).unwrap_or_default();
    // Every revision that has to resolve, and every successor a rewrite claims
    // for one, in a single batch. A ledger of any age holds thousands, and a
    // doctor that costs a minute stops being run.
    // A revision the map cannot follow forward is reported below on its own
    // terms; here it stands for itself, so the batch still asks Git about it.
    let successors: BTreeMap<&String, String> = wanted
        .keys()
        .map(|revision| {
            (
                revision,
                rewrites
                    .current(revision)
                    .unwrap_or_else(|_| revision.clone()),
            )
        })
        .collect();
    let mut queries: BTreeSet<&str> = wanted.keys().map(String::as_str).collect();
    queries.extend(successors.values().map(String::as_str));
    let missing: BTreeSet<String> = gitio::missing_objects(cwd, queries.into_iter())?
        .into_iter()
        .collect();
    for (revision, referents) in &wanted {
        if !missing.contains(revision) {
            continue;
        }
        let short = &revision[..revision.len().min(8)];
        let referents = referents.iter().cloned().collect::<Vec<_>>().join(", ");
        // A revision a recorded rewrite moved is not a casualty: the event
        // still says what it said, and the reader can follow it forward — so
        // long as what it leads to is here.
        match rewrites.fate(revision) {
            // A map that holds this revision's successor and cannot say which
            // it is answers nothing about it, and `invalid-rewrite-mapping` is
            // where the map itself is reported.
            Err(error) => problems.push(Finding {
                code: "invalid-rewrite-mapping",
                detail: format!("{short} cannot be followed forward: {error}: {referents}"),
            }),
            Ok(Some(crate::rewrite::Fate::Rewritten(successor))) => {
                let short_successor = &successor[..successor.len().min(8)];
                if missing.contains(&successor) {
                    problems.push(Finding {
                        code: "unresolved-revision",
                        detail: format!(
                            "{short} was rewritten to {short_successor}, which is not in this \
                             repository either: {referents}"
                        ),
                    });
                } else {
                    advice.push(Finding {
                        code: "revision-rewritten",
                        detail: format!("{short} was rewritten to {short_successor}: {referents}"),
                    });
                }
            }
            Ok(Some(crate::rewrite::Fate::Dropped)) => advice.push(Finding {
                code: "revision-dropped",
                detail: format!("{short} was dropped by a recorded rewrite: {referents}"),
            }),
            Ok(None) => advice.push(Finding {
                code: "dangling-revision",
                detail: format!(
                    "{short} is recorded but no longer in this repository: {referents}"
                ),
            }),
        }
    }
    Ok(())
}

/// What inspecting the changes keeps for the diagnostics after it: each
/// replayable change's state and patchsets, and the events of each change
/// that produced no problem.
struct Inspected<'a> {
    states: &'a mut BTreeMap<String, ChangeState>,
    readable: &'a mut BTreeMap<String, Vec<Event>>,
    patchsets: &'a mut BTreeMap<String, Vec<String>>,
}

fn inspect_changes(
    inspection: ChangeInspection<'_>,
    root: &Path,
    problems: &mut Vec<Finding>,
    advice: &mut Vec<Finding>,
    mut inspected_changes: Inspected<'_>,
) -> Result<()> {
    let changes = root.join("changes");
    let entries = match fs::read_dir(&changes) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", changes.display()))
        }
    };
    let (batches, batches_readable) = match crate::store::import_batches(root) {
        Ok(batches) => (batches, true),
        Err(error) => {
            problems.push(Finding {
                code: "malformed-import-batch",
                detail: format!("{error:#}"),
            });
            (Vec::new(), false)
        }
    };
    let mut imported = BTreeMap::<String, Vec<serde_json::Value>>::new();
    for batch in batches {
        imported
            .entry(batch.change_id)
            .or_default()
            .extend(batch.events);
    }
    let mut inspected = BTreeSet::new();
    for entry in entries.into_iter().flatten() {
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
        inspected.insert(change_id.clone());
        inspect_change(
            &inspection,
            &change_id,
            &entry.path(),
            imported
                .get(&change_id)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            problems,
            advice,
            &mut inspected_changes,
        )?;
    }
    for (change_id, events) in imported {
        if !inspected.contains(&change_id) {
            inspect_change(
                &inspection,
                &change_id,
                &changes.join(&change_id),
                &events,
                problems,
                advice,
                &mut inspected_changes,
            )?;
        }
    }
    // Without the import batches no change's events are complete.
    if !batches_readable {
        inspected_changes.readable.clear();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn inspect_change(
    inspection: &ChangeInspection<'_>,
    change_id: &str,
    change_dir: &Path,
    imported: &[serde_json::Value],
    problems: &mut Vec<Finding>,
    advice: &mut Vec<Finding>,
    inspected: &mut Inspected<'_>,
) -> Result<()> {
    let problems_before = problems.len();
    // Events a change read would refuse, without a problem of their own here.
    let mut misfiled = false;
    let events_dir = change_dir.join("events");
    let entries = match fs::read_dir(&events_dir) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", events_dir.display()))
        }
    };
    let mut paths = entries
        .into_iter()
        .flatten()
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
        misfiled |= event.event_id != event_id || event.change_id != change_id;
        has_open |= matches!(event.payload, Payload::ChangeOpened { .. });
        events.push(event);
    }
    for value in imported {
        match crate::bundle::parse_typed_event(value) {
            Ok(Some(event)) => {
                if event.change_id != change_id {
                    problems.push(Finding {
                        code: "invalid-event-envelope",
                        detail: event.event_id,
                    });
                    continue;
                }
                if let Some(existing) = events
                    .iter()
                    .find(|existing| existing.event_id == event.event_id)
                {
                    if serde_json::to_value(existing)? != *value {
                        problems.push(Finding {
                            code: "conflicting-event",
                            detail: event.event_id,
                        });
                    }
                    continue;
                }
                has_open |= matches!(event.payload, Payload::ChangeOpened { .. });
                events.push(event);
            }
            Ok(None) => advice.push(Finding {
                code: "unknown-event-type",
                detail: value["event_id"].to_string(),
            }),
            Err(error) => problems.push(Finding {
                code: "malformed-event",
                detail: format!("{error:#}"),
            }),
        }
    }
    events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    if !has_open {
        problems.push(Finding {
            code: "missing-open-event",
            detail: change_dir.display().to_string(),
        });
        return Ok(());
    }
    // Recorded revisions remain unresolved for dangling-object diagnostics;
    // admitted rewrites prove only the falsification's failure identity.
    // A landed member reads as the landing closed it; the raw events stay
    // what the remaining checks inspect.
    let joined = crate::landing::joined(&events, &inspection.repository_events);
    let mut state = match state::reduce_with_repository(
        &joined,
        &inspection.rewrites,
        &inspection.repository_events,
    ) {
        Ok(state) => state,
        Err(error) => {
            problems.push(Finding {
                code: "unreplayable-change",
                detail: format!("{}: {error:#}", change_dir.display()),
            });
            return Ok(());
        }
    };
    state.review_clearance = inspection.review_clearance;
    state.apply_privilege_scope(std::sync::Arc::clone(&inspection.scope), &joined);
    crate::replica::localize_change(inspection.local_repository_id, &joined, &mut state);
    for reference in &state.set_aside {
        advice.push(Finding {
            code: "set-aside-reference",
            detail: format!(
                "{change_id}: {} {} names {}, which only a withdrawn rewrite map carried {} to; \
                 it counts for nothing until `arc history rewrite` records a map that does",
                reference.reference, reference.event_id, reference.revision, reference.follows
            ),
        });
    }
    if state.closure.is_none() && !gitio::branch_exists(inspection.cwd, &state.branch) {
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
    inspected.patchsets.insert(
        change_id.to_string(),
        state
            .patchsets
            .iter()
            .map(|patchset| patchset.id.clone())
            .collect(),
    );
    inspected.states.insert(change_id.to_string(), state);
    if !misfiled && problems.len() == problems_before {
        inspected.readable.insert(change_id.to_string(), events);
    }
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

/// The commits the ledger cites as read against the refs that hold them.
///
/// A retention ref whose object is gone is evidence the ledger claims and
/// Git no longer has, which is a problem. A cited commit no ref reaches is
/// one the next garbage collection takes: nothing is lost yet, so it is
/// advice, with the command that holds it.
fn inspect_retention(
    ctx: &Ctx,
    store: &Store,
    problems: &mut Vec<Finding>,
    advice: &mut Vec<Finding>,
) -> Result<()> {
    let retained = gitio::list_refs(&ctx.cwd, gitio::RETAINED_PREFIX)?;
    let missing: BTreeSet<String> =
        gitio::missing_objects(&ctx.cwd, retained.iter().map(|(_, oid)| oid.as_str()))?
            .into_iter()
            .collect();
    for (reference, oid) in &retained {
        if missing.contains(oid) {
            problems.push(Finding {
                code: "retained-object-missing",
                detail: format!("{reference} holds {oid}, which is not in this repository"),
            });
        }
    }
    // A change that cannot be read is reported where its events are
    // inspected, and has no commits to answer for here.
    let Ok(cited) = crate::retention::cited(store) else {
        return Ok(());
    };
    let present: Vec<String> =
        gitio::commits_among(&ctx.cwd, cited.iter().map(|cited| cited.commit.as_str()))?;
    let present: Vec<&str> = present.iter().map(String::as_str).collect();
    let unreachable = match gitio::unreachable_from_refs(&ctx.cwd, &present) {
        Ok(unreachable) => unreachable,
        Err(error) => {
            problems.push(Finding {
                code: "retention-reachability-failed",
                detail: format!("cannot determine which cited commits refs reach: {error:#}"),
            });
            return Ok(());
        }
    };
    for cited in cited
        .iter()
        .filter(|cited| unreachable.contains(&cited.commit))
    {
        advice.push(Finding {
            code: "unretained-commit",
            detail: format!(
                "{} is recorded ({} of {}) and no ref reaches it, so Git may collect it; \
                 hold it with `git update-ref {} {}`",
                &cited.commit[..cited.commit.len().min(8)],
                cited.what,
                cited.change_id,
                cited.retained_ref(),
                cited.commit
            ),
        });
    }
    Ok(())
}

/// The bytes custody holds against the digests that name them.
///
/// Custody never reads a file whose bytes fail their digest as content, so
/// damage costs availability and nothing else: it is advice, with what
/// restores the bytes. A hidden temporary file left by a write that never
/// finished is housekeeping. Inspection removes and rewrites nothing.
fn inspect_custody(store: &Store, problems: &mut Vec<Finding>, advice: &mut Vec<Finding>) {
    let damage = match crate::custody::audit(&store.root, std::time::SystemTime::now()) {
        Ok(damage) => damage,
        Err(error) => {
            problems.push(Finding {
                code: "custody-unreadable",
                detail: format!("{error:#}"),
            });
            return;
        }
    };
    for damage in damage {
        advice.push(match damage {
            crate::custody::Damage::Mismatch { path, reason } => Finding {
                code: "custody-content-mismatch",
                detail: format!(
                    "{} does not hold the bytes its name digests ({reason}), so arc reads \
                     the content as corrupt; delete it and supply the bytes again with \
                     `arc forge observe --custody-dir` or `arc import --custody-dir`",
                    path.display()
                ),
            },
            crate::custody::Damage::Orphaned { path } => Finding {
                code: "custody-temp-orphaned",
                detail: format!(
                    "{} was left by a write that never finished; delete it",
                    path.display()
                ),
            },
        });
    }
}

/// The receipt each read cites against the bytes custody holds for it.
///
/// A missing or damaged receipt costs the read its evidence and nothing
/// else, so, as for other custody bytes, it is advice with what restores
/// it. A reference that records no length names bytes custody cannot
/// verify, which no export carries.
fn inspect_receipts(
    store: &Store,
    states: &BTreeMap<String, ChangeState>,
    advice: &mut Vec<Finding>,
) {
    let custody = crate::custody::Custody::at(&store.root);
    for (change_id, state) in states {
        let reads = state
            .verdicts
            .iter()
            .map(|verdict| (&verdict.event_id, verdict.receipt.as_ref()))
            .chain(
                state
                    .audit_verdicts
                    .iter()
                    .map(|audit| (&audit.event_id, audit.receipt.as_ref())),
            );
        for (event_id, receipt) in reads {
            let Some(receipt) = receipt else {
                continue;
            };
            let cited = format!(
                "read {event_id} on {change_id} cites receipt {} (sha256 {})",
                receipt.key, receipt.receipt_digest
            );
            let Some(content) = receipt.content() else {
                advice.push(Finding {
                    code: "receipt-length-unrecorded",
                    detail: format!(
                        "{cited} without its byte length, so no export carries its bytes and \
                         no import verifies them"
                    ),
                });
                continue;
            };
            let (code, state) = match custody.availability(&content) {
                crate::custody::Availability::Available => continue,
                crate::custody::Availability::Unavailable => (
                    "receipt-unavailable",
                    "the ledger does not hold".to_string(),
                ),
                crate::custody::Availability::Corrupt(reason) => (
                    "receipt-corrupt",
                    format!("the ledger holds damaged ({reason})"),
                ),
            };
            advice.push(Finding {
                code,
                detail: format!(
                    "{cited}, whose bytes {state}; record the receipt file again with `arc \
                     review --receipt` or `arc audit --receipt`, or import it with `arc import \
                     --custody-dir` from a sidecar that carries it"
                ),
            });
        }
    }
}

/// The candidate ledger against the pins that hold its content.
///
/// A pin with no registration holds content nothing names, which is
/// housekeeping. A registration a root reaches whose pin is gone is content
/// someone still wants and Git may already have collected, which is a problem.
/// So is a promotion ref no `candidate-promoted` event records: a promotion
/// that stopped partway, which `arc candidate promote` finishes or discards.
fn inspect_candidates(
    ctx: &Ctx,
    store: &Store,
    problems: &mut Vec<Finding>,
    advice: &mut Vec<Finding>,
) -> Result<()> {
    let ledger = match commands::candidate::load_ledger(store) {
        Ok(ledger) => ledger,
        Err(error) => {
            problems.push(Finding {
                code: "invalid-candidate-ledger",
                detail: format!("{error:#}"),
            });
            return Ok(());
        }
    };
    let prefix = crate::candidate::CANDIDATE_REF_PREFIX;
    let mut pinned = BTreeSet::new();
    for (reference, _) in gitio::list_refs(&ctx.cwd, prefix)? {
        let candidate_id = reference.trim_start_matches(prefix).to_string();
        if ledger.registration(&candidate_id).is_none() {
            advice.push(Finding {
                code: "unregistered-candidate-ref",
                detail: format!(
                    "{reference} has no registration; delete it with `git update-ref -d` \
                     or register the content it holds"
                ),
            });
        }
        pinned.insert(candidate_id);
    }
    for interrupted in commands::selection::interrupted_promotions(&ctx.cwd, &ledger)? {
        let detail = if interrupted.selection_known {
            format!(
                "{} holds {} and no candidate-promoted event records it; run `arc candidate \
                 promote {}` to complete or discard it",
                interrupted.reference, interrupted.commit, interrupted.selection
            )
        } else {
            format!(
                "{} holds {} for selection {}, which is not recorded; delete it with `git \
                 update-ref -d`",
                interrupted.reference, interrupted.commit, interrupted.selection
            )
        };
        problems.push(Finding {
            code: "interrupted-promotion",
            detail,
        });
    }
    for (registration, roots) in ledger.rooted_without_pin(&pinned) {
        problems.push(Finding {
            code: "rooted-candidate-unpinned",
            detail: format!(
                "{} is reached by {} but {} is gone",
                registration.candidate_id,
                roots
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                crate::candidate::candidate_ref(&registration.candidate_id)
            ),
        });
    }
    for registration in ledger.registrations() {
        let Some(received) = ledger.received_by(&registration.candidate_id) else {
            continue;
        };
        for diagnostic in commands::candidate::held_diagnostics(&ctx.cwd, &received.evidence) {
            advice.push(Finding {
                code: "held-evidence-mismatch",
                detail: format!(
                    "candidate {} received evidence {}: {}: {}",
                    registration.candidate_id,
                    received.event_id,
                    diagnostic.code,
                    diagnostic.detail
                ),
            });
        }
    }
    if !ledger.is_empty() {
        let declared = gitio::toplevel(&ctx.cwd)
            .ok()
            .and_then(|top| crate::policy::load(&top).ok())
            .is_some_and(|policy| policy.candidates.evaluation_reuse.is_some());
        if !declared {
            advice.push(Finding {
                code: "candidate-evaluation-reuse-undeclared",
                detail: "candidates are registered and [candidates] evaluation_reuse is not \
                         declared; selection requires \"matching-coordinates\" or \"never\""
                    .to_string(),
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
fn inspect_danger_paths(
    toplevel: &Path,
    policy: &crate::policy::PolicyFile,
    problems: &mut Vec<Finding>,
) {
    for pattern in &policy.danger.paths {
        if pattern.contains('*') || pattern.ends_with('/') {
            continue;
        }
        let target = toplevel.join(pattern);
        let declared_by = policy
            .sources
            .sources_for(&format!("danger.paths[{pattern:?}]"))
            .join(", ");
        if !target.exists() {
            problems.push(Finding {
                code: "danger-path-matches-nothing",
                detail: format!(
                    "{pattern} is declared dangerous by {declared_by} but does not exist; \
                     the surface it names is on a self-verdict"
                ),
            });
        } else if target.is_dir() {
            // Declared paths are matched against `git diff --name-only`,
            // which names files. A bare directory therefore exists, looks
            // healthy, and still matches nothing — the same silent widening
            // one level down.
            problems.push(Finding {
                code: "danger-path-matches-nothing",
                detail: format!(
                    "{pattern} is declared dangerous by {declared_by} but is a directory; \
                     declared paths match changed files, so write {pattern}/ to cover its subtree"
                ),
            });
        }
    }
}

/// Every tracked file inside a declared source root must carry exactly one
/// classification.
///
/// An open-world list fails permissively: a file nobody classified matches no
/// pattern, looks healthy, and lands on a self-verdict. Closing the world over
/// declared roots turns adding or renaming a file into a loud failure, which is
/// the direction both observed misses failed in.
fn inspect_danger_classification(
    toplevel: &Path,
    policy: &crate::policy::PolicyFile,
    problems: &mut Vec<Finding>,
) {
    if policy.danger.source_roots.is_empty() {
        return;
    }
    // From the toplevel, because `git ls-files` lists the subtree it is run
    // from and names paths relative to it. Run from a subdirectory it returns
    // a subset under names no declared root matches, so the whole closed world
    // silently stops being checked and `doctor` reports nothing wrong.
    let Ok(tracked) = crate::gitio::git(toplevel, &["ls-files"]) else {
        return;
    };
    for path in tracked
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        if !policy.danger.within_source_root(path) {
            continue;
        }
        let dangerous = policy.danger.is_dangerous(path);
        let safe = policy.danger.acknowledged_safe(path);
        if dangerous && safe {
            let dangerous_sources = policy
                .danger
                .paths
                .iter()
                .filter(|pattern| crate::policy::glob_match(pattern, path))
                .flat_map(|pattern| {
                    policy
                        .sources
                        .sources_for(&format!("danger.paths[{pattern:?}]"))
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let safe_sources = policy
                .danger
                .acknowledged_safe
                .iter()
                .filter(|pattern| crate::policy::glob_match(pattern, path))
                .flat_map(|pattern| {
                    policy
                        .sources
                        .sources_for(&format!("danger.acknowledged_safe[{pattern:?}]"))
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            problems.push(Finding {
                code: "danger-classification-conflict",
                detail: format!(
                    "{path} is declared dangerous by {} and acknowledged-safe by {}; \
                     one of the two claims is wrong",
                    dangerous_sources.join(", "),
                    safe_sources.join(", ")
                ),
            });
        } else if !dangerous && !safe {
            let root_sources = policy
                .danger
                .source_roots
                .iter()
                .filter(|root| path.starts_with(root.as_str()))
                .flat_map(|root| {
                    policy
                        .sources
                        .sources_for(&format!("danger.source_roots[{root:?}]"))
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            problems.push(Finding {
                code: "danger-unclassified",
                detail: format!(
                    "{path} is inside source root declared by {} and classified neither \
                     dangerous nor acknowledged-safe, so it is gated by a self-verdict nobody chose",
                    root_sources.join(", ")
                ),
            });
        }
    }
}

/// Preparation is read from the evaluated checkout, so where budgets judge
/// gates and `.arc/worktrees.toml` is not dangerous, a change can move gate
/// work into its `[prepare]` command and outside every budget on a self-verdict.
fn inspect_preparation_guard(
    toplevel: &Path,
    policy: &crate::policy::PolicyFile,
    advice: &mut Vec<Finding>,
) {
    const FILE: &str = ".arc/worktrees.toml";
    if policy.danger.is_dangerous(FILE)
        || !crate::warm_worktree::declares_prepare_command(toplevel)
        || !crate::gates::inspect(toplevel).is_ok_and(|gates| gates.has_budgets())
    {
        return;
    }
    advice.push(Finding {
        code: "preparation-unguarded",
        detail: format!(
            "{FILE} declares a [prepare] command and gates declare budgets, but no dangerous \
             path names it: preparation is read from the evaluated checkout, so a change can \
             move gate work into it outside every budget; add \"{FILE}\" to [danger] paths \
             in .arc/policy.toml"
        ),
    });
}

fn inspect_gate_conflicts(cwd: &Path, problems: &mut Vec<Finding>, advice: &mut Vec<Finding>) {
    let Ok(toplevel) = crate::gitio::toplevel(cwd) else {
        return;
    };
    let gates = match crate::gates::inspect(&toplevel) {
        Ok(gates) => gates,
        Err(error) => {
            problems.push(Finding {
                code: "gate-policy-unreadable",
                detail: format!("{error:#}"),
            });
            return;
        }
    };
    if !gates.gates.is_empty() && gates.confine.is_none() {
        advice.push(Finding {
            code: "gates-unconfined",
            detail: "project commands have unrestricted writes; declare [confine] writes in .arc/gates.toml (arc guide gates)".into(),
        });
    }
    if let Ok(Some(detail)) = crate::gate_raise::ceiling_warning(cwd, &gates) {
        problems.push(Finding {
            code: "gate-budget-unbounded",
            detail,
        });
    }
    for detail in gates.retired_bypass_keys() {
        problems.push(Finding {
            code: "gate-bypass-retired",
            detail,
        });
    }
    for detail in gates.budget_errors() {
        problems.push(Finding {
            code: "gate-budget-invalid",
            detail,
        });
    }
    for conflict in gates.conflicts {
        let declarations = conflict
            .declarations
            .iter()
            .map(crate::gates::GateDeclaration::describe)
            .collect::<Vec<_>>()
            .join("; ");
        problems.push(Finding {
            code: "gate-declaration-conflict",
            detail: format!(
                "gate {:?} has conflicting declarations: {declarations}",
                conflict.name
            ),
        });
    }
}

fn inspect_debt(
    ctx: &Ctx,
    states: &BTreeMap<String, ChangeState>,
    advice: &mut Vec<Finding>,
) -> Result<()> {
    let summary = crate::commands::messaging::collect_debts(ctx, states)?;
    if !summary.is_empty() {
        advice.push(Finding {
            code: "debt-outstanding",
            detail: summary.detail(),
        });
    }
    Ok(())
}

/// Debts `review_clearance = "assessed"` leaves owed that a read would
/// discharge without it. Advice, never a problem: the policy owes them on
/// purpose, and enabling it re-judges standing without rewriting any event,
/// so this is where the debts it reopened are named with their reads.
fn inspect_clearance(states: &BTreeMap<String, ChangeState>, advice: &mut Vec<Finding>) {
    for (change_id, state) in states {
        let Some(read) = state.discharged_without_clearance() else {
            continue;
        };
        let debt = state.debt.as_ref().expect("an owed debt");
        let findings = debt
            .findings_reads
            .iter()
            .map(|read| format!("{} ({})", read.event_id, read.reason))
            .collect::<Vec<_>>();
        advice.push(Finding {
            code: "clearance-keeps-debt-owed",
            detail: format!(
                "{change_id}: debt {} stays owed under review_clearance = \"assessed\"; \
                 without it read {read} would discharge it; findings reads: {}",
                debt.event_id,
                if findings.is_empty() {
                    "none".to_string()
                } else {
                    findings.join(", ")
                }
            ),
        });
    }
}

/// A release that names no active hold. Replay ignores it so the ledger stays
/// readable; this is where it becomes visible, because it means either a hold
/// was released twice or the events do not say what somebody thought.
///
/// It reads the events the change inspection already read. A change whose
/// inspection reported a problem is not checked: its problem is the finding.
fn inspect_hold_releases(readable: &BTreeMap<String, Vec<Event>>, advice: &mut Vec<Finding>) {
    for (change_id, events) in readable {
        let mut active: BTreeSet<String> = BTreeSet::new();
        for event in events {
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
fn inspect_repository_events(cwd: &Path, store: &Store, problems: &mut Vec<Finding>) {
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
    // Two events, each a valid mapping on its own, can still claim different
    // successors for one revision. No per-event check sees that, and every
    // projection refuses until it is resolved, so this is the report the
    // refusal sends the reader to.
    if let Err(error) = store
        .load_repository_events()
        .and_then(|events| crate::rewrite::RewriteMap::from_events(events.iter()))
    {
        let enriched =
            crate::rewrite::format_rewrite_error(error, Some(cwd), Some(store), &BTreeSet::new());
        problems.push(Finding {
            code: "invalid-rewrite-mapping",
            detail: enriched.to_string(),
        });
    }
}

fn inspect_closed_worktrees(
    cwd: &Path,
    states: &BTreeMap<String, ChangeState>,
    advice: &mut Vec<Finding>,
) -> Result<()> {
    let inventory = match gitio::git(cwd, &["worktree", "list", "--porcelain"]) {
        Ok(inventory) => inventory,
        Err(error) => {
            advice.push(Finding {
                code: "worktree-inventory-unknown",
                detail: error.to_string(),
            });
            return Ok(());
        }
    };
    let registered = inventory
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    // A change opened in place records the primary checkout, which is
    // registered for as long as the repository is and is not a worktree
    // anybody removes.
    let primary = gitio::primary_worktree(cwd)
        .ok()
        .map(|p| config::resolved(&p));
    for (change_id, state) in states {
        let (Some(closure), Some(worktree)) = (&state.closure, &state.worktree) else {
            continue;
        };
        if primary.as_deref() == Some(config::resolved(Path::new(worktree)).as_path()) {
            continue;
        }
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

/// What the open changes' and the forks' worktrees occupy.
///
/// Disk is the resource `begin` spends and nothing reported: a full
/// filesystem fails as exit 0 elsewhere, empty reports and bare exit codes,
/// which reads as nothing-to-do. That failure is exactly why this is advice
/// rather than silence — the number is a fact about state arc created, which
/// is the same reason it reports open changes.
///
/// Forks are reported under their own codes and never folded into the change
/// total. Nothing in the change lifecycle retires a fork's checkout, so a
/// reader deciding what to reclaim needs the two answers apart — and needs to
/// know that both are apparent size rather than physical cost.
fn inspect_worktree_accounting(
    cwd: &Path,
    states: &BTreeMap<String, ChangeState>,
    forks: &[crate::worktree_usage::ForkWorktree],
    advice: &mut Vec<Finding>,
) {
    let accounting = crate::worktree_usage::measure(cwd, states, forks);
    if accounting.is_empty() {
        return;
    }
    let caveat = accounting.measurement.caveat();
    for usage in &accounting.changes {
        let size = usage
            .bytes
            .map(crate::worktree_usage::human)
            .unwrap_or_else(|| "size unknown".to_string());
        advice.push(Finding {
            code: "open-worktree-usage",
            detail: format!(
                "{}: {} at {}",
                usage
                    .owners
                    .iter()
                    .map(|owner| owner.change_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                size,
                usage.path
            ),
        });
    }
    for usage in &accounting.unknown {
        advice.push(Finding {
            code: "open-worktree-usage-unknown",
            detail: format!(
                "{}: unknown at {} ({})",
                usage.change_id, usage.path, usage.reason
            ),
        });
    }
    if let Some(total) = accounting.total_bytes {
        advice.push(Finding {
            code: "open-worktree-usage-total",
            detail: format!(
                "{} across {} open worktree(s) [{caveat}]; build output in a clean \
                 worktree is reproducible and removable",
                crate::worktree_usage::human(total),
                accounting.changes.len()
            ),
        });
    }
    for usage in &accounting.forks {
        let size = usage
            .bytes
            .map(crate::worktree_usage::human)
            .unwrap_or_else(|| "size unknown".to_string());
        advice.push(Finding {
            code: "fork-worktree-usage",
            detail: format!("{}: {} at {}", usage.slug, size, usage.path),
        });
    }
    if let Some(total) = accounting.fork_total_bytes {
        advice.push(Finding {
            code: "fork-worktree-usage-total",
            detail: format!(
                "{} across {} fork worktree(s) [{caveat}]; a fork's checkout goes \
                 away when `arc fork retire <slug> <outcome>` records its \
                 disposition, never on its own",
                crate::worktree_usage::human(total),
                accounting.forks.len()
            ),
        });
    }
    if let Some(root) = accounting.measurement.root_line() {
        advice.push(Finding {
            code: "worktree-root-filesystem",
            detail: root,
        });
    }
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
        "open-worktree-usage" => format!(
            "{count} open changes hold worktree storage; \
             run arc doctor --verbose to list change/path/size triples"
        ),
        _ => return None,
    };
    Some(summary)
}
