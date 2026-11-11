//! Gate declarations beside observations from per-change verification events.

use super::*;
use serde::Serialize;

const GATES_SCHEMA: &str = "arc-gates/1";

#[derive(Serialize)]
pub(super) struct GateReport {
    schema: &'static str,
    pub(super) budget_declared: bool,
    pub(super) gates: Vec<GateRow>,
    pub(super) failures: Vec<ReadFailure>,
}

#[derive(Serialize)]
pub(super) struct ReadFailure {
    change_id: String,
    error: String,
}

#[derive(Serialize)]
pub(super) struct GateRow {
    pub(super) name: String,
    declaration: Declaration,
    budget_ms: Option<u64>,
    history: History,
}

#[derive(Serialize)]
struct Declaration {
    command: String,
    events: Vec<GateEvent>,
    profiles: Vec<String>,
    timeout_seconds: Option<u64>,
    selector: Option<String>,
    environment: Option<String>,
}

#[derive(Default, Serialize)]
struct History {
    runs: usize,
    reused: usize,
    selected_out: usize,
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
    let declared = if super::workspace::has_git_repository(cwd)? {
        gates::inspect(cwd)?
    } else {
        match std::fs::read_to_string(cwd.join(".arc/gates.toml")) {
            Ok(text) => toml::from_str(&text).context("malformed gate declarations")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                gates::GatesFile::default()
            }
            Err(error) => return Err(error.into()),
        }
    };
    declared.ensure_unconflicted()?;
    let mut rows: BTreeMap<_, _> = declared
        .gates
        .into_iter()
        .map(|(name, gate)| {
            let row = GateRow {
                name: name.clone(),
                declaration: Declaration {
                    command: gate.command,
                    events: if gate.events.is_empty() {
                        vec![GateEvent::Integration, GateEvent::Publication]
                    } else {
                        gate.events
                    },
                    profiles: gate.profiles,
                    timeout_seconds: gate.timeout,
                    selector: gate.selector,
                    environment: gate.environment,
                },
                budget_ms: None,
                history: History::default(),
            };
            (name, (row, Vec::new(), 0))
        })
        .collect();
    let mut failures = Vec::new();
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
            let mut failed_first = false;
            for event in events.iter() {
                match &event.payload {
                    Payload::VerificationRecorded {
                        gate,
                        result,
                        duration_ms,
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
                        if let Some(ms) = timing.run_ms.or(*duration_ms) {
                            samples.push(ms);
                            *legacy += usize::from(timing.run_ms.is_none());
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
            row
        })
        .collect();
    Ok(GateReport {
        schema: GATES_SCHEMA,
        budget_declared: false,
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
    println!("budget: none declared");
    println!(
        "gate  runs  reused  selected-out  median  p90  worst  ever-failed  last-failed-first"
    );
    for gate in &report.gates {
        let h = &gate.history;
        let timing = &h.timing;
        let failure = h.last_failed_first.as_ref().map_or_else(
            || "-".into(),
            |f| format!("{} ({})", f.at.to_rfc3339(), f.change_id),
        );
        println!(
            "{}  {}  {}  {}  {}  {}  {}  {}  {}",
            gate.name,
            h.runs,
            h.reused,
            h.selected_out,
            ms(timing.median_ms),
            ms(timing.p90_ms),
            ms(timing.worst_ms),
            h.ever_failed,
            failure
        );
        let d = &gate.declaration;
        println!("  command: {}", d.command);
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
        println!("  timing: {} samples; {} legacy duration_ms samples (includes preparation and waiting); attested: {}; not-admitted: {}", timing.samples, timing.legacy_duration_samples, h.attested, h.not_admitted);
    }
}

pub fn measure_gates(ctx: &Ctx, runs: u32, selected: Option<&str>) -> Result<i32> {
    let root = gitio::toplevel(&ctx.cwd)?;
    let declared = gates::inspect(&root)?;
    declared.ensure_unconflicted()?;
    if let Some(name) = selected {
        anyhow::ensure!(
            declared.gates.contains_key(name),
            "undeclared gate {name:?}"
        );
    }
    let mut passed = true;
    for (name, gate) in declared
        .gates
        .iter()
        .filter(|(name, _)| selected.is_none_or(|selected| selected == name.as_str()))
    {
        let mut wall = Vec::new();
        let mut cpu = Vec::new();
        for round in 1..=runs {
            let run = super::gatekeeping::measure_gate(&gate.command, &root, gate.timeout)?;
            let result =
                VerifyResult::observed(run.status.success(), run.status.code(), run.timed_out);
            passed &= result == VerifyResult::Pass;
            let timing = run.timing;
            let cpu_ms = timing
                .cpu_user_ms
                .zip(timing.cpu_system_ms)
                .map(|(u, s)| u.saturating_add(s));
            println!(
                "{name} {round}/{runs}: wall {}; CPU {}; {}{}",
                ms(timing.run_ms),
                ms(cpu_ms),
                result.as_str(),
                if run.timed_out { " (timed out)" } else { "" }
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
            "{name}: wall median {}, min {}, max {}, spread {}; CPU median {}",
            ms(distribution.median_ms),
            ms(wall.first().copied()),
            ms(distribution.worst_ms),
            ms(spread),
            ms(cpu_distribution.median_ms)
        );
    }
    println!("Wall far above CPU indicates waiting; a wide spread indicates load sensitivity.");
    Ok(if passed { 0 } else { 1 })
}
