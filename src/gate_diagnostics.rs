//! Receipt indicators for choosing a gate's next measurement.

use crate::verification_timing::VerificationTiming;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub struct Observations {
    runs: Vec<(
        chrono::DateTime<chrono::Utc>,
        Option<String>,
        String,
        VerificationTiming,
    )>,
    integrated: BTreeMap<String, usize>,
}

#[derive(Default, Serialize)]
pub struct Measurement {
    samples: usize,
    median_ms: Option<u64>,
}

impl Measurement {
    fn of(values: impl Iterator<Item = u64>) -> Self {
        let mut values = values.collect::<Vec<_>>();
        values.sort_unstable();
        Self {
            samples: values.len(),
            median_ms: values.get(values.len().saturating_sub(1) / 2).copied(),
        }
    }
}

#[derive(Default, Serialize)]
pub struct Diagnosis {
    kinds: Vec<&'static str>,
    unmeasured: Vec<&'static str>,
    wall: Measurement,
    cpu: Measurement,
    wait: Measurement,
    preparation: Measurement,
    integrated_changes: usize,
    integrated_runs: usize,
    runs_per_integrated_change: Option<f64>,
}

#[derive(Serialize)]
pub struct CheckCost {
    name: String,
    median_ms: u64,
    runs: usize,
    failed_on: BTreeSet<String>,
}

#[derive(Default, Serialize)]
pub struct Checks {
    measured_runs: usize,
    unmeasured_runs: usize,
    invalid_runs: usize,
    dropped_checks: usize,
    slowest: Vec<CheckCost>,
}

impl Observations {
    pub fn record(
        &mut self,
        change: &str,
        integrated: bool,
        at: chrono::DateTime<chrono::Utc>,
        environment: Option<String>,
        timing: &VerificationTiming,
    ) {
        if integrated {
            *self.integrated.entry(change.into()).or_default() += 1;
        }
        self.runs
            .push((at, environment, change.into(), timing.clone()));
    }

    pub fn summarize(mut self, integrated_changes: usize) -> (Diagnosis, Checks) {
        self.runs.sort_by_key(|r| r.0);
        let environment = self.runs.last().and_then(|r| r.1.clone());
        let matching = self
            .runs
            .iter()
            .rev()
            .filter(|r| r.1 == environment)
            .collect::<Vec<_>>();
        let recent = matching.iter().take(20).map(|r| &r.3).collect::<Vec<_>>();
        let wall = Measurement::of(recent.iter().filter_map(|t| t.run_ms));
        let cpu = Measurement::of(recent.iter().filter_map(|t| {
            t.cpu_user_ms
                .zip(t.cpu_system_ms)
                .map(|(u, s)| u.saturating_add(s))
        }));
        let wait = Measurement::of(recent.iter().filter_map(|t| t.wait_ms));
        let preparation = Measurement::of(recent.iter().filter_map(|t| t.prepare_ms));
        let integrated_runs = self.integrated.values().sum();
        let runs_per_integrated_change =
            (integrated_changes > 0).then(|| integrated_runs as f64 / integrated_changes as f64);
        let mut kinds = Vec::new();
        let mut unmeasured = Vec::new();
        // Pair wall and CPU from each receipt before classifying waiting.
        let paired_wall = Measurement::of(
            recent
                .iter()
                .filter(|t| t.cpu_user_ms.is_some() && t.cpu_system_ms.is_some())
                .filter_map(|t| t.run_ms),
        );
        let paired_cpu = Measurement::of(recent.iter().filter(|t| t.run_ms.is_some()).filter_map(
            |t| {
                t.cpu_user_ms
                    .zip(t.cpu_system_ms)
                    .map(|(u, s)| u.saturating_add(s))
            },
        ));
        match paired_wall.median_ms.zip(paired_cpu.median_ms) {
            Some((w, c)) if w >= 100 && c <= w / 2 => kinds.push("waiting"),
            Some(_) => kinds.push("real-work"),
            None => unmeasured.push("waiting/real-work"),
        }
        for (name, measurement) in [("queueing", &wait), ("preparation", &preparation)] {
            match measurement.median_ms {
                Some(ms) if ms >= 100 && wall.median_ms.is_some_and(|w| ms >= w.div_ceil(10)) => {
                    kinds.push(name)
                }
                Some(_) if wall.median_ms.is_some() => {}
                _ => unmeasured.push(name),
            }
        }
        match runs_per_integrated_change {
            Some(ratio) if ratio > 1.0 => kinds.push("duplication"),
            Some(_) => {}
            None => unmeasured.push("duplication"),
        }
        let diagnosis = Diagnosis {
            kinds,
            unmeasured,
            wall,
            cpu,
            wait,
            preparation,
            integrated_changes,
            integrated_runs,
            runs_per_integrated_change,
        };
        let mut checks = Checks::default();
        let mut costs: BTreeMap<String, (Vec<u64>, BTreeSet<String>)> = BTreeMap::new();
        for (_, _, change, timing) in matching {
            let Some(receipt) = &timing.check_timings else {
                checks.unmeasured_runs += 1;
                continue;
            };
            checks.dropped_checks += receipt.dropped;
            if receipt.error.is_some() {
                checks.invalid_runs += 1;
                continue;
            }
            checks.measured_runs += 1;
            for check in &receipt.checks {
                let (times, failures) = costs.entry(check.name.clone()).or_default();
                times.push(check.ms);
                if check.failed {
                    failures.insert(change.clone());
                }
            }
        }
        checks.slowest = costs
            .into_iter()
            .map(|(name, (times, failed_on))| CheckCost {
                name,
                runs: times.len(),
                median_ms: Measurement::of(times.into_iter()).median_ms.unwrap_or(0),
                failed_on,
            })
            .collect();
        checks
            .slowest
            .sort_by(|a, b| b.median_ms.cmp(&a.median_ms).then(a.name.cmp(&b.name)));
        checks.slowest.truncate(10);
        (diagnosis, checks)
    }
}

impl Diagnosis {
    pub fn render(&self, gate: &str) {
        let value = |m: &Measurement| {
            m.median_ms.map_or_else(
                || "unmeasured".into(),
                |n| format!("{n}ms ({} samples)", m.samples),
            )
        };
        println!(
            "{gate} cost kinds: {}; unmeasured: {}",
            if self.kinds.is_empty() {
                "none above thresholds".into()
            } else {
                self.kinds.join(", ")
            },
            if self.unmeasured.is_empty() {
                "none".into()
            } else {
                self.unmeasured.join(", ")
            }
        );
        println!(
            "  wall {}; CPU {}; queueing {}; preparation {}",
            value(&self.wall),
            value(&self.cpu),
            value(&self.wait),
            value(&self.preparation)
        );
        println!(
            "  duplication: {} executed runs / {} integrated changes; runs/change {}",
            self.integrated_runs,
            self.integrated_changes,
            self.runs_per_integrated_change
                .map_or_else(|| "unmeasured".into(), |n| format!("{n:.2}"))
        );
    }
}

impl Checks {
    pub fn render(&self) {
        println!("  check timings: {} measured runs; {} unmeasured; {} invalid; {} checks dropped by receipt caps", self.measured_runs, self.unmeasured_runs, self.invalid_runs, self.dropped_checks);
        for check in &self.slowest {
            if check.failed_on.is_empty() {
                println!(
                    "  check {:?}: never failed in {} runs, costs {:.3} seconds (median)",
                    check.name,
                    check.runs,
                    check.median_ms as f64 / 1000.0
                );
            } else {
                println!(
                    "  check {:?}: costs {:.3} seconds (median), {} recorded runs; failed on {}",
                    check.name,
                    check.median_ms as f64 / 1000.0,
                    check.runs,
                    check
                        .failed_on
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
}
