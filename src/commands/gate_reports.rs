//! Gate declarations beside observations from per-change verification events.

use super::*;
mod concurrent;
pub(super) mod landing;
use serde::Serialize;

const GATES_SCHEMA: &str = "arc-gates/1";

type BudgetSample = (chrono::DateTime<chrono::Utc>, Option<String>, u64);

#[derive(Serialize)]
pub(super) struct GateReport {
    schema: &'static str,
    pub(super) landing: landing::Report,
    unclaimed_paths: usize,
    pub(super) budget_declared: bool,
    budget_rules: Option<crate::gate_budget::BudgetRules>,
    integration_budget_sum_ms: Option<u64>,
    integration_budget_sums_ms: BTreeMap<String, Option<u64>>,
    budget_errors: Vec<String>,
    budget_warning: Option<String>,
    pub(super) gates: Vec<GateRow>,
    pub(super) failures: Vec<ReadFailure>,
}

#[derive(Serialize)]
pub(super) struct ReadFailure {
    change_id: String,
    error: String,
}

#[derive(Serialize)]
struct MeasuredAcceptance {
    #[serde(flatten)]
    reference: crate::gate_concurrency::AcceptanceRef,
    measured_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
pub(super) struct GateRow {
    pub(super) name: String,
    declaration: Declaration,
    concurrent_acceptance: Option<MeasuredAcceptance>,
    budget_ms: Option<u64>,
    budget_sources: Vec<crate::gate_budget::BudgetSource>,
    budget_share_percent: Option<f64>,
    budget_timing: Distribution,
    last_over_budget: Option<OverRun>,
    excused_runs: usize,
    last_excused: Option<OverRun>,
    history: History,
    diagnosis: crate::gate_diagnostics::Diagnosis,
    checks: crate::gate_diagnostics::Checks,
}

#[derive(Serialize)]
struct OverRun {
    change_id: String,
    event_id: String,
    at: chrono::DateTime<chrono::Utc>,
    run_ms: u64,
    hostname: String,
    timing: crate::verification_timing::VerificationTiming,
    output_artifact: Option<crate::verification_output::OutputArtifact>,
}

#[derive(Serialize)]
struct Declaration {
    timings: Option<String>,
    concurrent: bool,
    paths: Vec<String>,
    inert: Vec<String>,
    command: String,
    events: Vec<GateEvent>,
    profiles: Vec<String>,
    timeout_seconds: Option<u64>,
    exclusive: Option<String>,
    admission_ms: Option<u128>,
    selector: Option<String>,
    environment: Option<String>,
}

#[derive(Default, Serialize)]
struct History {
    runs: usize,
    reused: usize,
    selected_out: usize,
    base_passes: usize,
    attested: usize,
    not_admitted: usize,
    ever_failed: bool,
    last_failed_first: Option<Failure>,
    timing: Distribution,
}

#[derive(Serialize)]
struct Failure {
    change_id: String,
    event_id: String,
    at: chrono::DateTime<chrono::Utc>,
}

#[derive(Default, Serialize)]
struct Distribution {
    samples: usize,
    legacy_duration_samples: usize,
    median_ms: Option<u64>,
    p90_ms: Option<u64>,
    worst_ms: Option<u64>,
}

impl Distribution {
    fn of(values: &mut [u64], legacy: usize) -> Self {
        values.sort_unstable();
        let n = values.len();
        Self {
            samples: n,
            legacy_duration_samples: legacy,
            median_ms: values.get(n.saturating_sub(1) / 2).copied(),
            p90_ms: values.get((n * 9).div_ceil(10).saturating_sub(1)).copied(),
            worst_ms: values.last().copied(),
        }
    }
}

pub(super) fn collect(cwd: &Path, store: Option<&Store>) -> Result<GateReport> {
    let repository = super::workspace::has_git_repository(cwd)?;
    let declared = if repository {
        gates::inspect(cwd)?
    } else {
        match std::fs::read_to_string(cwd.join(".arc/gates.toml")) {
            Ok(text) => {
                let mut declared: gates::GatesFile =
                    toml::from_str(&text).context("malformed gate declarations")?;
                declared.name_budget_sources(".arc/gates.toml");
                declared
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                gates::GatesFile::default()
            }
            Err(error) => return Err(error.into()),
        }
    };
    collect_declarations(cwd, store, declared, repository)
}

fn collect_declarations(
    cwd: &Path,
    store: Option<&Store>,
    declared: gates::GatesFile,
    repository: bool,
) -> Result<GateReport> {
    declared.ensure_unconflicted()?;
    let unclaimed_paths = if repository {
        tracked_paths(cwd)?
            .iter()
            .filter(|path| {
                !declared
                    .selection
                    .inert
                    .iter()
                    .any(|pattern| crate::policy::glob_match(pattern, path))
                    && !declared.gates.values().any(|gate| {
                        gate.paths
                            .iter()
                            .any(|pattern| crate::policy::glob_match(pattern, path))
                    })
            })
            .count()
    } else {
        0
    };
    let budget_warning = if repository {
        crate::gate_raise::ceiling_warning(cwd, &declared)?
    } else {
        None
    };
    let budget_declared = declared.has_budgets();
    let budget_rules = declared.budget.clone();
    let budget_errors = declared.budget_errors();
    let integration_budget_sum_ms = crate::gate_concurrency::total(&declared.required_for(""));
    let profiles = std::iter::once(String::new())
        .chain(declared.gates.values().flat_map(|g| g.profiles.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    let integration_budget_sums_ms = profiles
        .into_iter()
        .map(|profile| {
            let sum = crate::gate_concurrency::total(&declared.required_for(&profile));
            (profile, sum)
        })
        .collect();
    let mut observations: BTreeMap<String, crate::gate_diagnostics::Observations> = BTreeMap::new();
    let mut budget_samples: BTreeMap<String, Vec<BudgetSample>> = BTreeMap::new();
    let mut rows: BTreeMap<_, _> = declared
        .gates
        .into_iter()
        .map(|(name, gate)| {
            let request = crate::exclusive::Request::for_gate(&gate);
            let admission_ms = gate
                .admission
                .or(request.map(|request| request.admission))
                .map(|duration| duration.as_millis());
            let row = GateRow {
                name: name.clone(),
                concurrent_acceptance: gate.concurrent_acceptance.as_ref().map(|acceptance| {
                    MeasuredAcceptance {
                        reference: acceptance.reference(),
                        measured_at: acceptance.measured_at,
                    }
                }),
                declaration: Declaration {
                    timings: gate.timings,
                    concurrent: gate.concurrent,
                    paths: gate.paths,
                    inert: declared.selection.inert.clone(),
                    command: gate.command,
                    events: if gate.events.is_empty() {
                        vec![GateEvent::Integration, GateEvent::Publication]
                    } else {
                        gate.events
                    },
                    profiles: gate.profiles,
                    timeout_seconds: gate.timeout,
                    exclusive: gate.exclusive,
                    admission_ms,
                    selector: gate.selector,
                    environment: gate.environment,
                },
                budget_ms: gate.budget,
                budget_sources: gate.budget_sources,
                budget_share_percent: None,
                budget_timing: Distribution::default(),
                last_over_budget: None,
                excused_runs: 0,
                last_excused: None,
                history: History::default(),
                diagnosis: Default::default(),
                checks: Default::default(),
            };
            (name, (row, Vec::new(), 0))
        })
        .collect();
    let mut failures = Vec::new();
    let mut integrated_changes = 0;
    let mut landing_costs = landing::Costs::new(cwd, repository)?;
    if let Some(store) = store {
        let read = store.read()?;
        for id in read.change_ids()? {
            let events = match read.held_events(&id) {
                Ok(events) => events,
                Err(error) => {
                    failures.push(ReadFailure {
                        change_id: id,
                        error: format!("{error:#}"),
                    });
                    continue;
                }
            };
            let integrated_at = events
                .iter()
                .chain(read.landings()?.derived_events(&id).iter())
                .filter(|e| {
                    matches!(
                        e.payload,
                        Payload::ChangeIntegrated { .. }
                            | Payload::IntegrationAsserted { .. }
                            | Payload::ChangeClosed {
                                outcome: crate::model::Closure::Integrated,
                                ..
                            }
                    )
                })
                .map(|e| e.created_at)
                .max();
            landing_costs.observe(&id, &events, integrated_at);
            let integrated = integrated_at.is_some();
            integrated_changes += usize::from(integrated);
            let mut failed_first = false;
            for event in events.iter() {
                match &event.payload {
                    Payload::VerificationRecorded {
                        gate,
                        result,
                        duration_ms,
                        command,
                        timeout_seconds,
                        environment,
                        hostname,
                        output_artifact,
                        timing,
                        attested,
                        ..
                    } => {
                        let first_failure =
                            !attested && *result == VerifyResult::Fail && !failed_first;
                        failed_first |= !attested && *result == VerifyResult::Fail;
                        let Some((row, samples, legacy)) =
                            gate.as_ref().and_then(|name| rows.get_mut(name))
                        else {
                            continue;
                        };
                        let history = &mut row.history;
                        if *attested {
                            history.attested += 1;
                            continue;
                        }
                        if *result == VerifyResult::NotAdmitted {
                            history.not_admitted += 1;
                            continue;
                        }
                        history.runs += 1;
                        history.ever_failed |= *result == VerifyResult::Fail;
                        if first_failure
                            && history
                                .last_failed_first
                                .as_ref()
                                .is_none_or(|last| event.created_at > last.at)
                        {
                            history.last_failed_first = Some(Failure {
                                change_id: id.clone(),
                                event_id: event.event_id.clone(),
                                at: event.created_at,
                            });
                        }
                        if command == &row.declaration.command
                            && timeout_seconds == &row.declaration.timeout_seconds
                        {
                            observations.entry(row.name.clone()).or_default().record(
                                &id,
                                integrated,
                                event.created_at,
                                environment.as_ref().map(|e| e.identity.clone()),
                                timing,
                            );
                            if *result == VerifyResult::Pass
                                && crate::gate_work::was_excused(timing)
                            {
                                row.excused_runs += 1;
                                if row
                                    .last_excused
                                    .as_ref()
                                    .is_none_or(|r| event.created_at > r.at)
                                {
                                    row.last_excused = Some(OverRun {
                                        change_id: id.clone(),
                                        event_id: event.event_id.clone(),
                                        at: event.created_at,
                                        run_ms: timing.run_ms.unwrap(),
                                        hostname: hostname.clone(),
                                        timing: timing.clone(),
                                        output_artifact: output_artifact.clone(),
                                    });
                                }
                            }
                            if let Some(ms) = timing.run_ms {
                                budget_samples.entry(row.name.clone()).or_default().push((
                                    event.created_at,
                                    environment.as_ref().map(|e| e.identity.clone()),
                                    ms,
                                ));
                                if crate::gate_budget::judge(
                                    row.budget_ms,
                                    &row.budget_sources,
                                    timing,
                                )
                                .verdict
                                    == crate::gate_budget::BudgetVerdict::OverBudget
                                    && row
                                        .last_over_budget
                                        .as_ref()
                                        .is_none_or(|r| event.created_at > r.at)
                                {
                                    row.last_over_budget = Some(OverRun {
                                        change_id: id.clone(),
                                        event_id: event.event_id.clone(),
                                        at: event.created_at,
                                        run_ms: ms,
                                        hostname: hostname.clone(),
                                        timing: timing.clone(),
                                        output_artifact: output_artifact.clone(),
                                    });
                                }
                            }
                        }
                        if let Some(ms) = timing.run_ms.or(*duration_ms) {
                            samples.push(ms);
                            *legacy += usize::from(timing.run_ms.is_none());
                        }
                    }
                    Payload::VerificationBaseReused { gate, .. } => {
                        if let Some((row, _, _)) = rows.get_mut(gate) {
                            row.history.base_passes += 1;
                        }
                    }
                    Payload::VerificationReused { gate, delta, .. } => {
                        if let Some((row, _, _)) = rows.get_mut(gate) {
                            if delta.is_some() {
                                row.history.selected_out += 1;
                            } else {
                                row.history.reused += 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let gates = rows
        .into_values()
        .map(|(mut row, mut samples, legacy)| {
            row.history.timing = Distribution::of(&mut samples, legacy);
            (row.diagnosis, row.checks) = observations
                .remove(&row.name)
                .unwrap_or_default()
                .summarize(integrated_changes);
            let mut observations = budget_samples.remove(&row.name).unwrap_or_default();
            observations.sort_by_key(|s| s.0);
            let environment = observations.last().and_then(|s| s.1.clone());
            let mut recent = observations
                .iter()
                .rev()
                .filter(|s| s.1 == environment)
                .take(20)
                .map(|s| s.2)
                .collect::<Vec<_>>();
            row.budget_timing = Distribution::of(&mut recent, 0);
            row.budget_share_percent = row
                .budget_ms
                .zip(row.budget_timing.median_ms)
                .map(|(budget, median)| median as f64 * 100.0 / budget as f64);
            row
        })
        .collect();
    Ok(GateReport {
        schema: GATES_SCHEMA,
        landing: landing_costs.summarize(cwd, repository, chrono::Utc::now())?,
        unclaimed_paths,
        budget_declared,
        budget_warning,
        budget_rules,
        budget_errors,
        integration_budget_sum_ms,
        integration_budget_sums_ms,
        gates,
        failures,
    })
}

pub fn gate_report(ctx: &Ctx, json: bool) -> Result<i32> {
    let root = gitio::toplevel(&ctx.cwd)?;
    let store = Store::open_at(&Store::resolve_root(&ctx.cwd)?)?;
    let report = collect(&root, store.as_ref())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render(&report);
    }
    for failure in &report.failures {
        eprintln!(
            "cannot read change {}: {}",
            failure.change_id, failure.error
        );
    }
    Ok(if report.failures.is_empty() { 0 } else { 16 })
}

fn ms(value: Option<u64>) -> String {
    value.map_or_else(|| "-".into(), |ms| format!("{:.3}s", ms as f64 / 1000.0))
}

pub(super) fn render(report: &GateReport) {
    report.landing.render();
    match report.budget_rules.as_ref().and_then(|b| b.total) {
        Some(total) => println!(
            "budget total: {}; unqualified integration sum {}",
            crate::gate_budget::duration(total),
            ms(report.integration_budget_sum_ms)
        ),
        None if report.budget_declared => println!("budget: gates declared; total undeclared"),
        None => println!("budget: none declared"),
    }
    for (profile, sum) in report
        .integration_budget_sums_ms
        .iter()
        .filter(|(p, _)| !p.is_empty())
    {
        println!("integration budget sum for {profile}: {}", ms(*sum));
    }
    if let Some(warning) = &report.budget_warning {
        println!("{warning}");
    }
    for error in &report.budget_errors {
        println!("{error}");
    }
    println!("tracked paths nobody names: {}", report.unclaimed_paths);
    println!(
        "gate  runs  base-passes  reused  selected-out  median  p90  worst  ever-failed  last-failed-first"
    );
    for gate in &report.gates {
        if gate.declaration.concurrent {
            match &gate.concurrent_acceptance {
                Some(acceptance) => {
                    println!(
                        "  concurrent: accepted {} at {} on host {}",
                        acceptance.reference.id, acceptance.measured_at, acceptance.reference.host
                    );
                    for (name, identity) in &acceptance.reference.members {
                        println!("    {name}: {identity}");
                    }
                }
                None => println!("  concurrent: not accepted on this store and host; runs alone"),
            }
        }
        let h = &gate.history;
        if let Some(budget) = gate.budget_ms {
            println!(
                "{} budget {}; median share {}; sources {}",
                gate.name,
                crate::gate_budget::duration(budget),
                gate.budget_share_percent.map_or_else(
                    || "unmeasured".into(),
                    |p| format!("{p:.1}%{}", if p > 80.0 { " (above 80%)" } else { "" })
                ),
                crate::gate_budget::sources(&gate.budget_sources)
            );
            if let Some(run) = &gate.last_over_budget {
                println!(
                    "  last over-budget: {} {} ran {} on {}",
                    run.change_id,
                    run.event_id,
                    ms(Some(run.run_ms)),
                    run.hostname
                );
            }
        }
        println!("  excused runs: {}", gate.excused_runs);
        if let Some(run) = &gate.last_excused {
            println!(
                "  last excused: {} {} ran {} on {}",
                run.change_id,
                run.event_id,
                ms(Some(run.run_ms)),
                run.hostname
            );
        }
        let timing = &h.timing;
        let failure = h.last_failed_first.as_ref().map_or_else(
            || "-".into(),
            |f| format!("{} ({})", f.at.to_rfc3339(), f.change_id),
        );
        println!(
            "{}  {}  {}  {}  {}  {}  {}  {}  {}  {}",
            gate.name,
            h.runs,
            h.base_passes,
            h.reused,
            h.selected_out,
            ms(timing.median_ms),
            ms(timing.p90_ms),
            ms(timing.worst_ms),
            h.ever_failed,
            failure
        );
        gate.diagnosis.render(&gate.name);
        gate.checks.render();
        let d = &gate.declaration;
        println!("  command: {}", d.command);
        println!(
            "  timings: {}",
            d.timings.as_deref().unwrap_or("undeclared")
        );
        println!(
            "  paths: {}; inert: {}",
            if d.paths.is_empty() {
                "all".into()
            } else {
                d.paths.join(", ")
            },
            if d.inert.is_empty() {
                "none".into()
            } else {
                d.inert.join(", ")
            }
        );
        println!(
            "  events: {}; profiles: {}; timeout: {}; selector: {}",
            d.events
                .iter()
                .map(|event| match event {
                    GateEvent::Integration => "integration",
                    GateEvent::Publication => "publication",
                })
                .collect::<Vec<_>>()
                .join(", "),
            if d.profiles.is_empty() {
                "all".into()
            } else {
                d.profiles.join(", ")
            },
            d.timeout_seconds
                .map_or_else(|| "unbounded".into(), |s| format!("{s}s")),
            d.selector.as_deref().unwrap_or("none")
        );
        println!(
            "  exclusive: {}; admission: {}",
            d.exclusive.as_deref().unwrap_or("none"),
            d.admission_ms
                .map_or_else(|| "none".into(), |ms| format!("{:.3}s", ms as f64 / 1000.0))
        );
        println!("  timing: {} samples; {} legacy duration_ms samples (includes preparation and waiting); attested: {}; not-admitted: {}", timing.samples, timing.legacy_duration_samples, h.attested, h.not_admitted);
    }
}

pub fn measure_gates(
    ctx: &Ctx,
    runs: u32,
    selected: Option<&str>,
    concurrent: bool,
) -> Result<i32> {
    if concurrent {
        return concurrent::measure(ctx, runs, selected);
    }
    let host = crate::verification_timing::host_at_start();
    println!(
        "host cores: physical {}; logical {}",
        crate::verification_timing::physical_core_count()
            .map_or_else(|| "unmeasured".into(), |n| n.to_string()),
        host.core_count
            .map_or_else(|| "unmeasured".into(), |n| n.to_string())
    );
    let root = gitio::toplevel(&ctx.cwd)?;
    let declared = gates::inspect(&root)?;
    declared.ensure_unconflicted()?;
    if let Some(name) = selected {
        anyhow::ensure!(
            declared.gates.contains_key(name),
            "undeclared gate {name:?}"
        );
    }
    let mut successful = true;
    for (name, gate) in declared
        .gates
        .iter()
        .filter(|(name, _)| selected.is_none_or(|selected| selected == name.as_str()))
    {
        let mut wall = Vec::new();
        let mut cpu = Vec::new();
        let mut not_measured = 0;
        for round in 1..=runs {
            let run = super::gatekeeping::measure_gate(gate, name, &root)?;
            let result =
                VerifyResult::observed(run.status.success(), run.status.code(), run.timed_out);
            successful &= result == VerifyResult::Pass;
            let timing = run.timing;
            if result == VerifyResult::NotAdmitted {
                not_measured += 1;
                println!(
                    "{name} {round}/{runs}: not measured (not-admitted); wait {}",
                    ms(timing.wait_ms)
                );
                if let Some(tail) = run.output_tail {
                    eprintln!("{name}: {tail}");
                }
                continue;
            }
            let cpu_ms = crate::gate_work::observed_tree_cpu_ms(&timing);
            println!(
                "{name} {round}/{runs}: wall {}; CPU {}; wait {}; {}{}",
                ms(timing.run_ms),
                ms(cpu_ms),
                ms(timing.wait_ms),
                result.as_str(),
                if run.timed_out { " (timed out)" } else { "" }
            );
            println!(
                "  instructions {}; other processes {} of host CPU capacity",
                timing
                    .instructions
                    .map_or_else(|| "unmeasured".into(), |n| n.to_string()),
                crate::gate_work::other_cpu_basis_points(&timing).map_or_else(
                    || "unmeasured".into(),
                    |n| format!("{:.2}%", n as f64 / 100.0)
                )
            );
            wall.extend(timing.run_ms);
            cpu.extend(cpu_ms);
            if result != VerifyResult::Pass {
                if let Some(tail) = run.output_tail {
                    eprintln!("{name}: {tail}");
                }
            }
        }
        let distribution = Distribution::of(&mut wall, 0);
        let cpu_distribution = Distribution::of(&mut cpu, 0);
        let spread = wall.last().zip(wall.first()).map(|(max, min)| max - min);
        println!(
            "{name}: wall median {}, min {}, max {}, spread {}; CPU median {}; measured {}/{}; not measured {}",
            ms(distribution.median_ms),
            ms(wall.first().copied()),
            ms(distribution.worst_ms),
            ms(spread),
            ms(cpu_distribution.median_ms),
            runs - not_measured, runs, not_measured,
        );
    }
    println!("Admission wait is separate from command wall time. Wall far above CPU indicates waiting inside the command; a wide spread indicates load sensitivity.");
    Ok(if successful { 0 } else { 1 })
}

fn tracked_paths(cwd: &Path) -> Result<Vec<String>> {
    let raw = gitio::git(cwd, &["ls-files", "-z"])?;
    anyhow::ensure!(!raw.contains('\u{fffd}'), "tracked paths must be UTF-8");
    let mut paths: Vec<_> = raw
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

#[derive(Serialize)]
struct PathRow {
    path: String,
    gates: Vec<String>,
    inert: bool,
    unclaimed: bool,
    all_gates_reason: Option<gates::AllGatesReason>,
}

#[derive(Serialize)]
struct PathsReport {
    schema: &'static str,
    paths: Vec<PathRow>,
    unclaimed_paths: Vec<String>,
}

/// The tracked inventory uses the same coverage rules as delta selection.
pub fn gate_paths(ctx: &Ctx, json: bool) -> Result<i32> {
    let root = gitio::toplevel(&ctx.cwd)?;
    let declared = gates::inspect(&root)?;
    declared.ensure_unconflicted()?;
    let mut unclaimed_paths = Vec::new();
    let mut paths = Vec::new();
    for path in tracked_paths(&root)? {
        let inert = declared
            .selection
            .inert
            .iter()
            .any(|pattern| crate::policy::glob_match(pattern, &path));
        let all_gates_reason = declared
            .gates
            .values()
            .next()
            .and_then(|gate| gate.coverage.all_gates_reason(&path));
        let named = declared.gates.values().any(|gate| {
            gate.paths
                .iter()
                .any(|pattern| crate::policy::glob_match(pattern, &path))
        });
        let unclaimed = !inert && !named;
        if unclaimed {
            unclaimed_paths.push(path.clone());
        }
        let affected = declared
            .gates
            .iter()
            .filter(|(name, gate)| gate.coverage.affects(name, &path))
            .map(|(name, _)| name.clone())
            .collect();
        paths.push(PathRow {
            path,
            gates: affected,
            inert: inert && all_gates_reason.is_none(),
            unclaimed,
            all_gates_reason,
        });
    }
    let report = PathsReport {
        schema: "arc-gate-paths/1",
        paths,
        unclaimed_paths,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for row in report.paths {
            println!(
                "{}: {}{}",
                serde_json::to_string(&row.path)?,
                if row.gates.is_empty() {
                    "none".into()
                } else {
                    row.gates.join(", ")
                },
                row.all_gates_reason
                    .map(|reason| format!(" ({})", reason.description()))
                    .unwrap_or_default()
            );
        }
        println!(
            "tracked paths nobody names: {}",
            report.unclaimed_paths.len()
        );
    }
    Ok(0)
}

fn declaration_text(file: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(file) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", file.display())),
    }
}

fn initial_budget_ms(min: u64, worst: u64) -> Result<u64> {
    let worst = u128::from(worst);
    let spread = worst - u128::from(min);
    let budget = (worst + spread).max((worst * 105).div_ceil(100)).max(1);
    let mut quantum = 1;
    while budget / quantum >= 100 {
        quantum *= 10;
    }
    u64::try_from(budget.div_ceil(quantum) * quantum).context("gate budget overflow")
}

/// Insert a field into its closest explicitly written table without rendering
/// any existing text. Source spans also cover multiline values and dotted keys.
fn budget_insertion(
    document: &toml_edit::ImDocument<&str>,
    path: &[&str],
    value: &str,
    newline: &str,
) -> Result<(usize, String)> {
    let source = document.raw();
    for depth in (0..path.len()).rev() {
        let Some(parent) = path[..depth]
            .iter()
            .try_fold(document.as_item(), |item, key| item.get(*key))
        else {
            continue;
        };
        let key = path[depth..]
            .iter()
            .map(|key| toml_edit::Key::new(*key).to_string())
            .collect::<Vec<_>>()
            .join(".");
        if let Some(table) = parent.as_inline_table() {
            let end = table
                .span()
                .context("inline table source span unavailable")?
                .end;
            let separator = if table.is_empty() { "" } else { ", " };
            return Ok((end - 1, format!("{separator}{key} = {value}")));
        }
        if let Some(table) = parent.as_table().filter(|table| !table.is_dotted()) {
            if let Some(span) = table.span() {
                let offset = if span.end == 0 {
                    0
                } else {
                    source[span.end..]
                        .find('\n')
                        .map_or(source.len(), |n| span.end + n + 1)
                };
                let separator = if offset > 0 && !source[..offset].ends_with('\n') {
                    newline
                } else {
                    ""
                };
                return Ok((offset, format!("{separator}{key} = {value}{newline}")));
            }
        }
    }
    bail!("gate declaration has no writable table")
}

pub fn init_budgets(ctx: &Ctx, runs: u32) -> Result<i32> {
    anyhow::ensure!(runs > 0, "measurement rounds must be positive");
    let root = gitio::toplevel(&ctx.cwd)?;
    let file = root.join(".arc/gates.toml");
    let original = declaration_text(&file)?;
    let operator_file = crate::policy::operator_path(&root)?;
    let operator_text = declaration_text(&operator_file)?;
    let declared = gates::inspect(&root)?;
    declared.ensure_unconflicted()?;
    anyhow::ensure!(
        !declared.has_budgets(),
        "refusing to replace an existing budget"
    );
    anyhow::ensure!(!declared.gates.is_empty(), "no gates declared");
    let source = original.as_deref().unwrap_or("");
    let document = toml_edit::ImDocument::parse(source)?;
    let operator: toml_edit::DocumentMut = operator_text.as_deref().unwrap_or("").parse()?;
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut measurements: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for round in 1..=runs {
        for (name, gate) in &declared.gates {
            let run = super::gatekeeping::measure_gate(gate, name, &root)?;
            anyhow::ensure!(
                run.status.success() && !run.timed_out && !run.admission_expired,
                "gate {name} round {round}/{runs} was not measured successfully; no budgets written: {}",
                run.output_tail.unwrap_or_default()
            );
            let measured = run
                .timing
                .run_ms
                .context("command run_ms unavailable; no budgets written")?;
            println!("gate {name} {round}/{runs}: run_ms={measured}");
            measurements.entry(name.clone()).or_default().push(measured);
        }
    }
    let mut total = 0_u64;
    let mut insertions = Vec::new();
    for (name, mut samples) in measurements {
        samples.sort_unstable();
        let min = samples[0];
        let max = samples[samples.len() - 1];
        let measured = initial_budget_ms(min, max)?;
        total = total
            .checked_add(measured)
            .context("budget total overflow")?;
        let value = toml_edit::Value::from(format!("{measured}ms"));
        if document.get("gates").and_then(|g| g.get(&name)).is_some() {
            insertions.push(budget_insertion(
                &document,
                &["gates", &name, "budget"],
                &value.to_string(),
                newline,
            )?);
        } else {
            let mut gate = operator
                .get("gates")
                .and_then(|g| g.get(&name))
                .context("operator gate declaration unavailable")?
                .clone()
                .into_value()
                .map_err(|_| anyhow::anyhow!("operator gate must be a table"))?;
            gate.as_inline_table_mut()
                .context("operator gate must be a table")?
                .insert("budget", value);
            insertions.push(budget_insertion(
                &document,
                &["gates", &name],
                &gate.to_string(),
                newline,
            )?);
        }
        println!(
            "gate {name}: budget {} from {runs} rounds; min {min}ms, worst {max}ms, spread {}ms",
            crate::gate_budget::duration(measured),
            max - min
        );
    }
    let mut text = source.to_owned();
    insertions.sort_by_key(|(offset, _)| std::cmp::Reverse(*offset));
    for (offset, insertion) in insertions {
        text.insert_str(offset, &insertion);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push_str(newline);
    }
    text.push_str(&format!("[budget]{newline}total = \"{total}ms\"{newline}"));
    let _: gates::GatesFile = toml::from_str(&text)
        .context("invalid initialized gate declarations; no budgets written")?;
    anyhow::ensure!(
        declaration_text(&file)? == original && declaration_text(&operator_file)? == operator_text,
        "gate declaration changed during measurement"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = match std::fs::metadata(&file) {
        Ok(meta) => FileMode::Keep(meta.permissions().mode()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileMode::Create(0o644),
        Err(error) => return Err(error.into()),
    };
    std::fs::create_dir_all(file.parent().context("gate file has no parent")?)?;
    write_atomically(&file, text.as_bytes(), mode)?;
    println!(
        "budget total: {}; wrote .arc/gates.toml",
        crate::gate_budget::duration(total)
    );
    Ok(0)
}

pub fn over_budgets(ctx: &Ctx, reference: Option<&str>, selected: Option<&str>) -> Result<i32> {
    let store = Store::discover_for_reading(&ctx.cwd)?;
    let states = match reference {
        Some(reference) => vec![store.state(&store.resolve_change(reference)?)?],
        None => store.readable_states()?.into_values().collect(),
    };
    let mut runs = Vec::new();
    let mut latest_declarations = None;
    let mut latest_at = None;
    for mut state in states {
        state.target_branch = crate::declarations::accepted_target(&state).to_owned();
        let target = gitio::branch_head(&ctx.cwd, &state.target_branch)?;
        let mut declared = gates::load_at(&ctx.cwd, &target)?;
        let head = if state.is_closed() {
            state.closed_head().map(str::to_owned)
        } else {
            gitio::branch_head(&ctx.cwd, &state.branch).ok()
        };
        if let Some(head) = &head {
            declared.owe_also(
                gates::inspect_at(&ctx.cwd, head)?,
                "destination",
                "change head",
            );
        }
        if let Some(head) = head {
            let _ = crate::gate_raise::advise(&ctx.cwd, &state, &target, &head, &mut declared);
        }
        let first_run = runs.len();
        for entry in &state.verifications {
            let Some(name) = entry
                .gate
                .as_deref()
                .filter(|n| selected.is_none_or(|s| s == *n))
            else {
                continue;
            };
            let Some(gate) = declared.gates.get(name) else {
                continue;
            };
            if !status::matches_declaration(entry, gate) {
                continue;
            }
            let judgement = gate.budget_judgement(&entry.timing);
            if judgement.verdict == crate::gate_budget::BudgetVerdict::OverBudget {
                runs.push((
                    entry.created_at,
                    state.change_id.clone(),
                    name.to_owned(),
                    entry.clone(),
                    judgement,
                ));
            }
        }
        if let Some(at) = runs[first_run..].iter().map(|r| r.0).max() {
            if latest_at.is_none_or(|latest| at >= latest) {
                latest_at = Some(at);
                latest_declarations = Some(declared);
            }
        }
    }
    runs.sort_by_key(|r| r.0);
    if let Some((_, change, gate, entry, judgement)) = runs.last() {
        println!(
            "{change}: gate {gate}: {}",
            judgement
                .reason(&entry.timing, &entry.hostname)
                .unwrap_or_default()
        );
        println!(
            "evidence {}; output {:?}; declared budget history {:?}",
            entry.event_id, entry.output_artifact, judgement.sources
        );
        println!(
            "budget recorded with run: {}",
            entry
                .timing
                .budget_at_run
                .as_ref()
                .map(|b| serde_json::to_string(b).unwrap_or_default())
                .unwrap_or_else(|| "unrecorded".into())
        );
    } else {
        println!("no over-budget run found");
    }
    let root = gitio::toplevel(&ctx.cwd)?;
    let report = match latest_declarations {
        Some(declared) => collect_declarations(&root, Some(&store), declared, true)?,
        None => collect(&root, Some(&store))?,
    };
    for row in report
        .gates
        .iter()
        .filter(|row| selected.is_none_or(|name| row.name == name))
    {
        row.diagnosis.render(&row.name);
        row.checks.render();
    }
    println!("Order of work: get the number first; measure serially; remove waste; rank checks by what they guard and cut from the bottom, never by cost alone; make what matters fit; lower concurrency until nothing fails; stop at a named floor; raise the budget last. See arc guide gate-rounds for the method and arc guide gate-costs for the levers.");
    for failure in &report.failures {
        eprintln!(
            "cannot read change {}: {}",
            failure.change_id, failure.error
        );
    }
    Ok(if report.failures.is_empty() { 0 } else { 16 })
}

#[cfg(test)]
mod init_tests {
    use super::initial_budget_ms;

    /// Claim: initial budgets cover the spread and five-percent floor, rounded
    /// upward to two significant digits. Rejects truncated floors, downward
    /// rounding, missing spread, and overflowing budgets.
    #[test]
    fn headroom_arithmetic_boundaries() {
        for (min, worst, budget) in [
            (8363, 9522, 11000),
            (0, 0, 1),
            (1, 1, 2),
            (94, 94, 99),
            (95, 95, 100),
            (100, 100, 110),
            (1000, 1000, 1100),
            (100, 150, 200),
            (100, 151, 210),
            (900, 999, 1100),
        ] {
            assert_eq!(
                initial_budget_ms(min, worst).unwrap(),
                budget,
                "min {min}, worst {worst}"
            );
        }
        assert!(initial_budget_ms(u64::MAX, u64::MAX).is_err());
        assert!(initial_budget_ms(0, u64::MAX).is_err());
    }
}
