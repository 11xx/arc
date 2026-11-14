//! Command time paid by integrated changes, counted once at their closure,
//! with the preparation the same receipts paid beside it, arc's own command
//! time around them, and the three summed as landing time.

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
    /// arc's own command time; see `own_ms`.
    own_ms: Option<u64>,
    /// Gate, preparation and arc's own time summed, when all three are known.
    total_ms: Option<u64>,
    /// The landing's commands by invocation, each with its largest running
    /// time; `None` when an event in the landing carries no invocation.
    #[serde(skip)]
    commands: Option<BTreeMap<String, u64>>,
}

/// One integrated change's landing time and its parts, as `arc show` states it.
#[derive(Serialize)]
pub(in crate::commands) struct Figure {
    pub(in crate::commands) gate_ms: Option<u64>,
    pub(in crate::commands) prepare_ms: Option<u64>,
    pub(in crate::commands) own_ms: Option<u64>,
    pub(in crate::commands) total_ms: Option<u64>,
}

impl Figure {
    /// The figure as one line: its parts, their sum, and what judges it.
    pub(in crate::commands) fn line(&self) -> String {
        let part = |value: Option<u64>| value.map_or_else(|| "unmeasured".into(), |v| ms(Some(v)));
        format!(
            "gate {}, preparation {}, arc's own commands {}: landing time {}; reported, not budgeted: the landing budget judges gate time",
            part(self.gate_ms),
            part(self.prepare_ms),
            part(self.own_ms),
            part(self.total_ms)
        )
    }
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
    measured_own_ms: u64,
    own_unmeasured: usize,
    own_paid_ms: Option<u64>,
    total_unmeasured: usize,
    total_paid_ms: Option<u64>,
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
            period.measured_own_ms = period
                .measured_own_ms
                .saturating_add(change.own_ms.unwrap_or(0));
            period.own_unmeasured += usize::from(change.own_ms.is_none());
            period.total_unmeasured += usize::from(change.total_ms.is_none());
        }
        period.paid_ms = (period.unmeasured == 0).then_some(period.measured_ms);
        period.prepare_paid_ms =
            (period.prepare_unmeasured == 0).then_some(period.measured_prepare_ms);
        period.own_paid_ms = (period.own_unmeasured == 0).then_some(period.measured_own_ms);
        period.total_paid_ms = sum([period.paid_ms, period.prepare_paid_ms, period.own_paid_ms]);
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
    own_unmeasured: usize,
    own: Distribution,
    total_unmeasured: usize,
    total: Distribution,
    #[serde(skip_serializing_if = "Option::is_none")]
    median_exceeds_budget: Option<bool>,
    pub(in crate::commands) last_7_days: Period,
    pub(in crate::commands) last_30_days: Period,
    changes: Vec<ChangeCost>,
}

#[derive(Default)]
pub(in crate::commands) struct Costs {
    branch: Option<String>,
    destination: Option<(DateTime<Utc>, String)>,
    changes: Vec<ChangeCost>,
    /// Gate run and preparation time each invocation recorded on the
    /// receipts gate time counts, from every change observed; `None` when
    /// one of them lacks either.
    inside: BTreeMap<String, Option<u64>>,
}

/// `a + b + c` when all are known.
fn sum(parts: [Option<u64>; 3]) -> Option<u64> {
    parts
        .into_iter()
        .try_fold(0u64, |total, part| total.checked_add(part?))
}

/// When a change was integrated: its latest integration event, its own or one
/// a landing derived for it.
pub(in crate::commands) fn integrated_at(events: &[Event]) -> Option<DateTime<Utc>> {
    events
        .iter()
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
        .max()
}

/// The invocation an event names, when it names a well-formed one with its
/// running time.
fn invocation(event: &Event) -> Option<(&str, u64)> {
    let id = event
        .invocation_id
        .as_deref()
        .filter(|id| crate::ids::is_invocation_id(id))?;
    Some((id, event.invocation_elapsed_ms?))
}

/// The commands of a change's final landing, from the one that recorded the
/// shipped patchset through the integration event, each with the largest
/// running time among its events there. `None` when the change has no
/// landing window or an event in it carries no invocation.
fn landing_commands(events: &[Event]) -> Option<BTreeMap<String, u64>> {
    let (start, end) = super::super::stats::final_landing(events)?;
    let mut commands = BTreeMap::new();
    for event in &events[start..=end] {
        let (id, elapsed) = invocation(event)?;
        let longest: &mut u64 = commands.entry(id.to_string()).or_default();
        *longest = (*longest).max(elapsed);
    }
    Some(commands)
}

/// arc's own time on a landing: each command's running time less the gate
/// run and preparation time it recorded, summed. `None` when a command's
/// receipt lacks either.
fn own_ms(commands: &BTreeMap<String, u64>, inside: &BTreeMap<String, Option<u64>>) -> Option<u64> {
    commands.iter().try_fold(0u64, |total, (id, elapsed)| {
        let gated = inside.get(id).copied().unwrap_or(Some(0))?;
        total.checked_add(elapsed.saturating_sub(gated))
    })
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
            inside: BTreeMap::new(),
        })
    }

    /// One change's landing time, reading the other members of the landing
    /// that integrated it for the gate time its command recorded on their
    /// ledgers.
    pub(in crate::commands) fn of_change(store: &Store, id: &str) -> Result<Option<Figure>> {
        let read = store.read()?;
        let events = read.events_with_landings(id)?;
        let attempt = events.iter().rev().find_map(|event| match &event.payload {
            Payload::ChangeIntegrated {
                landing: Some(landing),
                ..
            } => Some(landing.attempt.as_str()),
            _ => None,
        });
        let landings = read.landings()?;
        let completion = attempt.and_then(|attempt| landings.completion(attempt));
        let mut costs = Self::default();
        if let Some(Payload::LandingCompleted { members, .. }) = completion.map(|c| &c.payload) {
            for member in members.iter().filter(|member| member.change_id != id) {
                let theirs = read.events_with_landings(&member.change_id)?;
                costs.observe(&member.change_id, &theirs, None);
            }
        }
        costs.observe(id, &events, integrated_at(&events));
        costs.settle();
        Ok(costs.changes.pop().map(|change| Figure {
            gate_ms: change.run_ms,
            prepare_ms: change.prepare_ms,
            own_ms: change.own_ms,
            total_ms: change.total_ms,
        }))
    }

    /// Each observed landing's own and total time, once every change's
    /// receipts are in.
    fn settle(&mut self) {
        for change in &mut self.changes {
            change.own_ms = change
                .commands
                .as_ref()
                .and_then(|commands| own_ms(commands, &self.inside));
            change.total_ms = sum([change.run_ms, change.prepare_ms, change.own_ms]);
        }
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
            own_ms: None,
            total_ms: None,
            commands: landing_commands(events),
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
                Payload::VerificationRunStarted { event: kind, .. } => {
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
                    if *result == VerifyResult::NotAdmitted
                        || run_id
                            .as_deref()
                            .and_then(|id| run_events.get(id))
                            .copied()
                            .unwrap_or_default()
                            != GateEvent::Integration
                    {
                        continue;
                    }
                    if let Some((id, _)) = invocation(event) {
                        let gated = timing
                            .run_ms
                            .zip(timing.prepare_ms)
                            .and_then(|(run, prepare)| run.checked_add(prepare));
                        let inside = self.inside.entry(id.to_string()).or_insert(Some(0));
                        *inside = inside.zip(gated).and_then(|(a, b)| a.checked_add(b));
                    }
                    let Some(cost) = &mut cost else {
                        continue;
                    };
                    if event.created_at > cost.integrated_at {
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
        self.settle();
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
        let mut own = self
            .changes
            .iter()
            .filter_map(|c| c.own_ms)
            .collect::<Vec<_>>();
        let own_unmeasured = landings - own.len();
        let own = window_timing(&mut own, own_unmeasured);
        let mut total = self
            .changes
            .iter()
            .filter_map(|c| c.total_ms)
            .collect::<Vec<_>>();
        let total_unmeasured = landings - total.len();
        let total = window_timing(&mut total, total_unmeasured);
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
            own_unmeasured,
            own,
            total_unmeasured,
            total,
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
        println!(
            "  arc's own commands: {} unmeasured; median {}, p90 {}",
            self.own_unmeasured,
            statistic(self.own.median_ms),
            statistic(self.own.p90_ms)
        );
        println!(
            "  landing time (gate, preparation and arc's own): {} unmeasured; median {}, p90 {}; reported, not budgeted: the landing budget judges gate time",
            self.total_unmeasured,
            statistic(self.total.median_ms),
            statistic(self.total.p90_ms)
        );
        if let Some(warning) = &self.budget_warning {
            println!("{warning}");
        }
        for (days, period) in [(7, &self.last_7_days), (30, &self.last_30_days)] {
            println!(
                "  {days} days: {} landings; paid {}; measured {}; {} unmeasured landings; preparation paid {}; measured {}; {} unmeasured; arc's own paid {}; measured {}; {} unmeasured; landing time paid {}; {} unmeasured",
                period.landings,
                ms(period.paid_ms),
                ms(Some(period.measured_ms)),
                period.unmeasured,
                ms(period.prepare_paid_ms),
                ms(Some(period.measured_prepare_ms)),
                period.prepare_unmeasured,
                ms(period.own_paid_ms),
                ms(Some(period.measured_own_ms)),
                period.own_unmeasured,
                ms(period.total_paid_ms),
                period.total_unmeasured
            );
        }
    }
}
