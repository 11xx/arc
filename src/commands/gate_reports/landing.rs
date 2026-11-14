//! Command time paid by integrated changes, counted once at their closure,
//! with the preparation the same receipts paid beside it.

use super::*;
use chrono::{DateTime, Duration, Utc};

const WINDOW: usize = 100;

#[derive(Serialize)]
pub(in crate::commands) struct ChangeCost {
    change_id: String,
    integrated_at: DateTime<Utc>,
    executions: usize,
    run_ms: Option<u64>,
    measured_ms: u64,
    prepare_ms: Option<u64>,
    measured_prepare_ms: u64,
}

#[derive(Default, Serialize)]
pub(in crate::commands) struct Period {
    pub(in crate::commands) landings: usize,
    pub(in crate::commands) measured_ms: u64,
    unmeasured: usize,
    paid_ms: Option<u64>,
    measured_prepare_ms: u64,
    prepare_unmeasured: usize,
    prepare_paid_ms: Option<u64>,
}

impl Period {
    fn of(changes: &[ChangeCost], since: DateTime<Utc>, until: DateTime<Utc>) -> Self {
        let mut period = Self::default();
        for change in changes
            .iter()
            .filter(|c| c.integrated_at >= since && c.integrated_at <= until)
        {
            period.landings += 1;
            period.measured_ms = period.measured_ms.saturating_add(change.measured_ms);
            period.unmeasured += usize::from(change.run_ms.is_none());
            period.measured_prepare_ms = period
                .measured_prepare_ms
                .saturating_add(change.measured_prepare_ms);
            period.prepare_unmeasured += usize::from(change.prepare_ms.is_none());
        }
        period.paid_ms = (period.unmeasured == 0).then_some(period.measured_ms);
        period.prepare_paid_ms =
            (period.prepare_unmeasured == 0).then_some(period.measured_prepare_ms);
        period
    }
}

#[derive(Serialize)]
pub(in crate::commands) struct Report {
    window: usize,
    budget_ms: Option<u64>,
    budget_destination: Option<String>,
    budget_warning: Option<String>,
    landings: usize,
    unmeasured: usize,
    zero_execution_share: Option<f64>,
    timing: Distribution,
    prepare_unmeasured: usize,
    preparation: Distribution,
    #[serde(skip_serializing_if = "Option::is_none")]
    median_exceeds_budget: Option<bool>,
    pub(in crate::commands) last_7_days: Period,
    pub(in crate::commands) last_30_days: Period,
    changes: Vec<ChangeCost>,
}

pub(in crate::commands) struct Costs {
    branch: Option<String>,
    destination: Option<(DateTime<Utc>, String)>,
    changes: Vec<ChangeCost>,
}

impl Costs {
    pub(in crate::commands) fn new(cwd: &Path, repository: bool) -> Result<Self> {
        Ok(Self {
            branch: if repository {
                gitio::current_branch(cwd)?
            } else {
                None
            },
            destination: None,
            changes: Vec::new(),
        })
    }

    pub(in crate::commands) fn observe(
        &mut self,
        id: &str,
        events: &[Event],
        integrated_at: Option<DateTime<Utc>>,
    ) {
        let mut run_events = BTreeMap::new();
        let mut cost = integrated_at.map(|integrated_at| ChangeCost {
            change_id: id.into(),
            integrated_at,
            executions: 0,
            run_ms: Some(0),
            measured_ms: 0,
            prepare_ms: Some(0),
            measured_prepare_ms: 0,
        });
        for event in events {
            match &event.payload {
                Payload::ChangeOpened {
                    branch,
                    target_branch,
                    ..
                } if Some(branch) == self.branch.as_ref()
                    && self
                        .destination
                        .as_ref()
                        .is_none_or(|(at, _)| event.created_at > *at) =>
                {
                    self.destination = Some((event.created_at, target_branch.clone()));
                }
                Payload::VerificationRunStarted { event: kind, .. } if cost.is_some() => {
                    run_events.insert(event.event_id.as_str(), *kind);
                }
                Payload::VerificationRecorded {
                    gate: Some(_),
                    attested: false,
                    result,
                    run_id,
                    timing,
                    ..
                } => {
                    let Some(cost) = &mut cost else {
                        continue;
                    };
                    if event.created_at > cost.integrated_at
                        || *result == VerifyResult::NotAdmitted
                        || run_id
                            .as_deref()
                            .and_then(|id| run_events.get(id))
                            .copied()
                            .unwrap_or_default()
                            != GateEvent::Integration
                    {
                        continue;
                    }
                    cost.executions += 1;
                    cost.run_ms = cost
                        .run_ms
                        .zip(timing.run_ms)
                        .and_then(|(a, b)| a.checked_add(b));
                    cost.measured_ms = cost.measured_ms.saturating_add(timing.run_ms.unwrap_or(0));
                    cost.prepare_ms = cost
                        .prepare_ms
                        .zip(timing.prepare_ms)
                        .and_then(|(a, b)| a.checked_add(b));
                    cost.measured_prepare_ms = cost
                        .measured_prepare_ms
                        .saturating_add(timing.prepare_ms.unwrap_or(0));
                }
                _ => {}
            }
        }
        self.changes.extend(cost);
    }

    pub(in crate::commands) fn summarize(
        mut self,
        cwd: &Path,
        repository: bool,
        now: DateTime<Utc>,
    ) -> Result<Report> {
        let destination = self.destination.map(|(_, branch)| branch).or(self.branch);
        let (budget_ms, budget_warning) = if repository {
            match destination
                .as_deref()
                .map(|branch| {
                    gitio::branch_head(cwd, branch).and_then(|head| {
                        let project = crate::gate_raise::project(cwd, &head)?;
                        let operator = crate::gate_raise::operator(cwd)?;
                        Ok(project
                            .landing_budget_ms
                            .into_iter()
                            .chain(operator.landing_budget_ms)
                            .min())
                    })
                })
                .transpose()
            {
                Ok(Some(budget)) => (budget, None),
                Ok(None) => (
                    None,
                    Some("landing budget unobserved: no destination branch".into()),
                ),
                Err(error) => (None, Some(format!("landing budget unobserved: {error:#}"))),
            }
        } else {
            (
                None,
                Some("landing budget unobserved: no repository".into()),
            )
        };
        let last_7_days = Period::of(&self.changes, now - Duration::days(7), now);
        let last_30_days = Period::of(&self.changes, now - Duration::days(30), now);
        self.changes.sort_by(|a, b| {
            b.integrated_at
                .cmp(&a.integrated_at)
                .then_with(|| a.change_id.cmp(&b.change_id))
        });
        self.changes.truncate(WINDOW);
        let landings = self.changes.len();
        let mut samples = self
            .changes
            .iter()
            .filter_map(|c| c.run_ms)
            .collect::<Vec<_>>();
        let unmeasured = landings - samples.len();
        let timing = window_timing(&mut samples, unmeasured);
        let mut preparation = self
            .changes
            .iter()
            .filter_map(|c| c.prepare_ms)
            .collect::<Vec<_>>();
        let prepare_unmeasured = landings - preparation.len();
        let preparation = window_timing(&mut preparation, prepare_unmeasured);
        Ok(Report {
            window: WINDOW,
            budget_ms,
            budget_destination: destination,
            budget_warning,
            landings,
            unmeasured,
            zero_execution_share: (landings > 0).then(|| {
                self.changes.iter().filter(|c| c.executions == 0).count() as f64 / landings as f64
            }),
            median_exceeds_budget: timing
                .median_ms
                .zip(budget_ms)
                .map(|(median, budget)| median > budget),
            timing,
            prepare_unmeasured,
            preparation,
            last_7_days,
            last_30_days,
            changes: self.changes,
        })
    }
}

/// A percentile over `samples` and `unmeasured` landings, stated only when no
/// value the unmeasured landings could hold would change it.
pub(super) fn window_timing(samples: &mut [u64], unmeasured: usize) -> Distribution {
    let mut timing = Distribution::of(samples, 0);
    if unmeasured > 0 {
        let landings = samples.len() + unmeasured;
        let determined = |rank: usize| {
            let lower = if rank < unmeasured {
                0
            } else {
                samples[rank - unmeasured]
            };
            samples.get(rank).copied().filter(|upper| *upper == lower)
        };
        timing.median_ms = determined((landings - 1) / 2);
        timing.p90_ms = determined((landings * 9).div_ceil(10) - 1);
        timing.worst_ms = None;
    }
    timing
}

impl Report {
    pub(in crate::commands) fn over_budget(&self) -> Option<String> {
        self.median_exceeds_budget.filter(|over| *over).map(|_| format!(
            "landing median {} exceeds advisory budget {} over {} measured of {} recent landings (window {})",
            ms(self.timing.median_ms), ms(self.budget_ms), self.timing.samples, self.landings, self.window
        ))
    }

    pub(in crate::commands) fn render(&self) {
        let statistic =
            |value: Option<u64>| value.map_or_else(|| "unmeasured".into(), |value| ms(Some(value)));
        println!("landing: latest {} changes; {} landings, {} unmeasured; median {}, p90 {}; budget {}{}; zero executions {}",
            self.window, self.landings, self.unmeasured, statistic(self.timing.median_ms), statistic(self.timing.p90_ms), ms(self.budget_ms),
            if self.median_exceeds_budget == Some(true) { " (median exceeds advisory)" } else { "" },
            self.zero_execution_share.map_or_else(|| "unmeasured".into(), |s| format!("{:.1}%", s*100.0)));
        println!(
            "  preparation: {} unmeasured; median {}, p90 {}; outside the landing budget, which judges gate time",
            self.prepare_unmeasured,
            statistic(self.preparation.median_ms),
            statistic(self.preparation.p90_ms)
        );
        if let Some(warning) = &self.budget_warning {
            println!("{warning}");
        }
        for (days, period) in [(7, &self.last_7_days), (30, &self.last_30_days)] {
            println!(
                "  {days} days: {} landings; paid {}; measured {}; {} unmeasured landings; preparation paid {}; measured {}; {} unmeasured",
                period.landings,
                ms(period.paid_ms),
                ms(Some(period.measured_ms)),
                period.unmeasured,
                ms(period.prepare_paid_ms),
                ms(Some(period.measured_prepare_ms)),
                period.prepare_unmeasured
            );
        }
    }
}
