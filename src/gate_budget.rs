//! Cost judgements use command observations without changing evidence identity.

use crate::gate_suspension::Suspension;
use crate::gates::{Gate, GatesFile};
use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetSource {
    pub milliseconds: u64,
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetRules {
    #[serde(default, deserialize_with = "crate::gate_raise::percentage")]
    pub raise: Option<u64>,
    #[serde(default, deserialize_with = "crate::gate_raise::positive")]
    pub raise_per: Option<u64>,
    /// Generated and vendored paths excluded from allowance counting.
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_ms")]
    pub total: Option<u64>,
    #[serde(default)]
    pub sources: Vec<BudgetSource>,
}

pub fn deserialize_ms<'de, D: Deserializer<'de>>(
    de: D,
) -> std::result::Result<Option<u64>, D::Error> {
    use serde::de::Error;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value {
        Text(String),
        Milliseconds(u64),
    }
    Option::<Value>::deserialize(de)?
        .map(|v| match v {
            Value::Text(s) => crate::commands::parse_duration_ms(&s).map_err(D::Error::custom),
            Value::Milliseconds(ms) if ms > 0 => Ok(ms),
            _ => Err(D::Error::custom("budget must be positive")),
        })
        .transpose()
}

fn budgeted_authorization(basis: &crate::model::AuthorizationBasis) -> bool {
    basis.budget_rules.is_some() || basis.gates.values().any(|g| g.budget_ms.is_some())
}

pub fn budgeted_payload(payload: &crate::model::Payload) -> bool {
    use crate::model::Payload;
    match payload {
        Payload::VerificationRecorded { timing, .. }
        | Payload::CandidateVerified { timing, .. } => timing.budget_at_run.is_some(),
        Payload::PublicationVerified {
            gates,
            budget_accepted,
            ..
        } => {
            budget_accepted.is_some()
                || gates
                    .values()
                    .any(|receipt| receipt.timing.budget_at_run.is_some())
        }
        Payload::ChangeIntegrated {
            authorization: Some(basis),
            ..
        }
        | Payload::ChangeProvisionallyIntegrated {
            authorization: basis,
            ..
        }
        | Payload::ReadyToSend {
            authorization: basis,
            ..
        } => budgeted_authorization(basis),
        Payload::LandingCompleted { members, .. } => members
            .iter()
            .any(|m| budgeted_authorization(&m.authorization)),
        Payload::GateBudgetRaised { .. } => true,
        Payload::PatchsetAdded {
            budget_raise_ids, ..
        } => !budget_raise_ids.is_empty(),
        _ => false,
    }
}

fn minimum(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

pub fn removal_error(gate: Option<&str>) -> String {
    match gate {
        Some(name) => format!("budget: gate {name} keeps its declaration but removes its budget; restore the budget or remove the gate"),
        None => "budget: cannot remove [budget] total while gates remain; restore the total or remove every gate".into(),
    }
}

pub fn merge_rules(into: &mut Option<BudgetRules>, other: Option<BudgetRules>) {
    if let Some(other) = other {
        let current = into.get_or_insert_with(BudgetRules::default);
        current.total = minimum(current.total, other.total);
        current.raise = minimum(current.raise, other.raise);
        current.raise_per = current.raise_per.into_iter().chain(other.raise_per).max();
        current.exclude.extend(other.exclude);
        current.exclude.sort();
        current.exclude.dedup();
        current.sources.extend(other.sources);
        current
            .sources
            .sort_by(|a, b| (&a.source, a.milliseconds).cmp(&(&b.source, b.milliseconds)));
        current.sources.dedup();
    }
}

impl Gate {
    pub fn effective_budget(&self) -> Option<u64> {
        if self.gate_trailers.change_size == Some(crate::gate_trailers::ChangeSize::Interface) {
            self.interface_budget.or(self.budget)
        } else {
            self.budget
        }
    }

    pub fn effective_budget_sources(&self) -> &[BudgetSource] {
        if self.gate_trailers.change_size == Some(crate::gate_trailers::ChangeSize::Interface)
            && self.interface_budget.is_some()
        {
            &self.interface_budget_sources
        } else {
            &self.budget_sources
        }
    }

    pub fn absorb_budget(&mut self, other: &Gate) {
        let self_had_budget = self.budget.is_some();
        self.budget = minimum(self.budget, other.budget);
        self.budget_sources.extend(other.budget_sources.clone());
        self.budget_sources
            .sort_by(|a, b| (&a.source, a.milliseconds).cmp(&(&b.source, b.milliseconds)));
        self.budget_sources.dedup();
        // A layer that already caps a gate with `budget` and declares no
        // `interface_budget` cannot be loosened by another layer adding one.
        if !self_had_budget || self.interface_budget.is_some() {
            self.interface_budget = minimum(self.interface_budget, other.interface_budget);
            self.interface_budget_sources
                .extend(other.interface_budget_sources.clone());
            self.interface_budget_sources
                .sort_by(|a, b| (&a.source, a.milliseconds).cmp(&(&b.source, b.milliseconds)));
            self.interface_budget_sources.dedup();
        }
    }

    pub fn budget_judgement(
        &self,
        timing: &crate::verification_timing::VerificationTiming,
    ) -> BudgetJudgement {
        let mut judgement = judge(
            self.effective_budget(),
            self.effective_budget_sources(),
            crate::gate_suspension::current(self.budget_suspension.as_deref()),
            timing,
        );
        judgement.raise_allowance_ms = self.raise_allowance_ms;
        judgement.raise_budget_max_ms = self.raise_budget_max_ms;
        judgement
    }

    /// What a run started now is judged by once it ends.
    pub fn declared_budget(&self) -> Declared {
        Declared {
            budget_ms: self.effective_budget(),
            sources: self.effective_budget_sources().to_vec(),
            suspension: self.budget_suspension.clone(),
        }
    }

    pub fn execution_limit(&self) -> Result<(Option<u64>, Option<String>)> {
        if let Some(seconds) = self.timeout {
            Ok((
                Some(
                    seconds
                        .checked_mul(1000)
                        .ok_or_else(|| anyhow::anyhow!("gate timeout overflow"))?,
                ),
                Some("timeout".into()),
            ))
        } else if let Some(budget) = self.effective_budget() {
            Ok((
                Some(
                    budget
                        .checked_mul(10)
                        .ok_or_else(|| anyhow::anyhow!("ten-times budget overflow"))?,
                ),
                Some("budget-times-ten".into()),
            ))
        } else {
            Ok((None, None))
        }
    }
}

impl GatesFile {
    pub fn name_budget_sources(&mut self, origin: &str) {
        for gate in self.gates.values_mut() {
            if let Some(milliseconds) = gate.budget {
                gate.budget_sources.push(BudgetSource {
                    milliseconds,
                    source: origin.into(),
                });
            }
            if let Some(milliseconds) = gate.interface_budget {
                gate.interface_budget_sources.push(BudgetSource {
                    milliseconds,
                    source: origin.into(),
                });
            }
        }
        if let Some(rules) = &mut self.budget {
            rules.sources.clear();
            if let Some(milliseconds) = rules.total {
                rules.sources.push(BudgetSource {
                    milliseconds,
                    source: origin.into(),
                });
            }
        }
    }

    pub fn has_budgets(&self) -> bool {
        self.budget.is_some()
            || self
                .gates
                .values()
                .any(|g| g.budget.is_some() || g.interface_budget.is_some())
    }

    /// Validate the complete effective suite, including profiles other than the active one.
    pub fn budget_errors(&self) -> Vec<String> {
        let mut errors = self.pending_raises.iter().map(|gate| {
            let name = gate.as_deref().unwrap_or("total");
            format!("budget: {name} declaration increase requires an applicable accepted raise record")
        }).collect::<Vec<_>>();
        errors.extend(
            self.invalid_budget_removals
                .iter()
                .map(|gate| removal_error(gate.as_deref())),
        );
        if self.raise_snapshot_missing {
            errors
                .push("budget raise set needs a fresh snapshot and review or recorded debt".into());
        }
        for (name, gate) in &self.gates {
            match (gate.budget, gate.interface_budget) {
                (None, Some(_)) => errors.push(format!(
                    "budget: gate {name} declares interface_budget without budget"
                )),
                (Some(budget), Some(interface_budget)) if interface_budget < budget => {
                    errors.push(format!(
                        "budget: gate {name} interface_budget {} is less than budget {}",
                        duration(interface_budget),
                        duration(budget)
                    ));
                }
                _ => {}
            }
        }
        errors.extend(crate::gate_concurrency::lock_errors(self.gates.iter()));
        let Some(rules) = &self.budget else {
            return errors;
        };
        let mut profiles = std::collections::BTreeSet::from([String::new()]);
        profiles.extend(self.gates.values().flat_map(|g| g.profiles.clone()));
        for profile in profiles {
            let label = if profile.is_empty() {
                "unqualified"
            } else {
                &profile
            };
            let required = self.required_for(&profile);
            let sum = crate::gate_concurrency::total(&required);
            let missing_budget = required.iter().any(|(_, gate)| gate.budget.is_none());
            for (name, gate) in required {
                if gate.budget.is_none() {
                    errors.push(format!(
                        "budget: integration gate {name} needs a budget (profile {label}){}",
                        if gate.head_removes {
                            "; the head removes it and the destination still declares it, so \
                             land the gate replacement first with no total declared, then \
                             declare budgets and the total in a following change, once the \
                             destination holds only the gates it keeps"
                        } else {
                            ""
                        }
                    ));
                }
            }
            match (sum, rules.total) {
                (None, _) if !missing_budget => {
                    errors.push(format!("budget sum overflow (profile {label})"))
                }
                (Some(sum), Some(total)) if sum > total => errors.push(format!(
                    "budget: integration sum {} exceeds total {} (profile {label}; {})",
                    duration(sum),
                    duration(total),
                    sources(&rules.sources)
                )),
                _ => {}
            }
        }
        errors
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BudgetVerdict {
    Undeclared,
    WithinBudget,
    OverBudget,
    Excused,
    /// Over budget, accepted because a suspension covered the gate when judged.
    Suspended,
    Unmeasured,
}

/// The budget, its provenance and any covering suspension, taken before a run.
#[derive(Debug, Clone)]
pub struct Declared {
    pub budget_ms: Option<u64>,
    pub sources: Vec<BudgetSource>,
    pub suspension: Option<std::sync::Arc<Suspension>>,
}

impl Declared {
    pub fn judge(
        &self,
        timing: &crate::verification_timing::VerificationTiming,
    ) -> BudgetJudgement {
        judge(
            self.budget_ms,
            &self.sources,
            crate::gate_suspension::current(self.suspension.as_deref()),
            timing,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetJudgement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excuse: Option<Box<crate::gate_work::Excuse>>,
    /// The suspension a `suspended` verdict rests on, and present only with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspension: Option<Box<Suspension>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raise_allowance_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raise_budget_max_ms: Option<u64>,
    pub verdict: BudgetVerdict,
    pub budget_ms: Option<u64>,
    pub run_ms: Option<u64>,
    pub excess_ms: Option<u64>,
    pub sources: Vec<BudgetSource>,
}

/// Decide whether a command time satisfies a budget. `suspension` is the one
/// covering the gate at this moment, on the ledger being judged; it accepts
/// an overrun nothing else does and changes no other verdict. A caller that
/// passes none judges strictly.
pub fn judge(
    budget_ms: Option<u64>,
    provenance: &[BudgetSource],
    suspension: Option<&Suspension>,
    timing: &crate::verification_timing::VerificationTiming,
) -> BudgetJudgement {
    let run_ms = timing.run_ms;
    let excuse = budget_ms.and_then(|b| crate::gate_work::excuse(timing, b));
    let verdict = match (budget_ms, run_ms) {
        (None, _) => BudgetVerdict::Undeclared,
        (Some(_), None) => BudgetVerdict::Unmeasured,
        (Some(b), Some(r)) if r > b && excuse.is_some() => BudgetVerdict::Excused,
        (Some(b), Some(r)) if r > b && suspension.is_some() => BudgetVerdict::Suspended,
        (Some(b), Some(r)) if r > b => BudgetVerdict::OverBudget,
        _ => BudgetVerdict::WithinBudget,
    };
    BudgetJudgement {
        excuse: if verdict == BudgetVerdict::Excused {
            excuse.map(Box::new)
        } else {
            None
        },
        suspension: suspension
            .filter(|_| verdict == BudgetVerdict::Suspended)
            .cloned()
            .map(Box::new),
        raise_allowance_ms: None,
        raise_budget_max_ms: None,
        verdict,
        budget_ms,
        run_ms,
        excess_ms: run_ms.zip(budget_ms).map(|(r, b)| r.saturating_sub(b)),
        sources: provenance.to_vec(),
    }
}

impl BudgetJudgement {
    pub fn accepted(&self) -> bool {
        self.fits()
            || matches!(
                self.verdict,
                BudgetVerdict::Excused | BudgetVerdict::Suspended
            )
    }

    /// No budget, or a command time inside it: the only runs later work may
    /// be measured against or inherit.
    pub fn fits(&self) -> bool {
        matches!(
            self.verdict,
            BudgetVerdict::Undeclared | BudgetVerdict::WithinBudget
        )
    }

    pub fn reason(
        &self,
        timing: &crate::verification_timing::VerificationTiming,
        host: &str,
    ) -> Option<String> {
        if self.accepted() {
            return None;
        }
        let budget = duration(self.budget_ms.expect("declared budget"));
        let reason = match self.run_ms {
            Some(run) => format!(
                "over budget, ran {} of {budget}; excess {}",
                duration(run),
                duration(self.excess_ms.unwrap_or_default())
            ),
            None => format!("unmeasured: run_ms is missing for budget {budget}"),
        };
        let mut reason = format!("{reason}; budget source {}; host {host}; load {:?}; cores {:?}; CPU user {:?}ms system {:?}ms; excluded wait {:?}ms preparation {:?}ms", sources(&self.sources), timing.load_average, timing.core_count, timing.cpu_user_ms, timing.cpu_system_ms, timing.wait_ms, timing.prepare_ms);
        if timing
            .load_average
            .zip(timing.core_count)
            .is_some_and(|(load, cores)| load[0] > cores as f64)
        {
            reason.push_str("; possible host contention");
        }
        reason.push_str("; inspect arc gates over, make it faster, remove checks worth less, then arc gates raise GATE; allowance ");
        reason.push_str(
            &self
                .raise_allowance_ms
                .map(duration)
                .unwrap_or_else(|| "unavailable for this subject".into()),
        );
        if let Some(maximum) = self.raise_budget_max_ms {
            reason.push_str(&format!("; maximum {}", duration(maximum)));
        }
        Some(reason)
    }
}

/// Why an over-budget pass was accepted: the suspension or the busy host.
pub fn acceptance_line(judgement: &BudgetJudgement) -> Option<String> {
    if let Some(suspension) = &judgement.suspension {
        return Some(format!(
            "accepted over budget, ran {} of {}: budgets suspended until {} by suspension {}: {}",
            duration(judgement.run_ms?),
            duration(judgement.budget_ms?),
            suspension
                .until
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            suspension.event_id,
            suspension.reason
        ));
    }
    let work = judgement.excuse.as_ref()?;
    Some(format!("accepted over budget: work {} instructions against reference {} ({} instructions); other processes {:.2}% of host CPU capacity",
        work.instructions, work.reference_event_id, work.reference_instructions, work.other_cpu_basis_points as f64 / 100.0))
}

pub fn sources(values: &[BudgetSource]) -> String {
    if values.is_empty() {
        return "declaration".into();
    }
    values
        .iter()
        .map(|s| format!("{} ({})", s.source, duration(s.milliseconds)))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn duration(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!(
            "{}s",
            format!("{:.3}", ms as f64 / 1000.0).trim_end_matches('0')
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claim: budgets parse exactly to milliseconds and refuse overflow or lost
    /// precision. Rejects floating rounding and truncated fractional execution limits.
    #[test]
    fn checked_millisecond_parser_and_execution_limit() {
        for (raw, ms) in [
            ("10s", 10000),
            ("2.5s", 2500),
            ("500ms", 500),
            ("2m", 120000),
            ("0.001s", 1),
        ] {
            assert_eq!(crate::commands::parse_duration_ms(raw).unwrap(), ms);
        }
        for raw in [
            "0ms",
            "1.1ms",
            "18446744073709551615s",
            "1e3s",
            "1.s",
            "-1s",
        ] {
            assert!(crate::commands::parse_duration_ms(raw).is_err(), "{raw}");
        }
        let gates: GatesFile =
            toml::from_str("[gates.unit]\ncommand='true'\nbudget='2.5s'\n").unwrap();
        assert_eq!(
            gates.gates["unit"].execution_limit().unwrap().0,
            Some(25000)
        );
    }

    /// Claim: every target, change and operator minimum survives omissions, additions
    /// and staging unions. Rejects a missing declaration relaxing any existing ceiling.
    #[test]
    fn minima_and_effective_totals_survive_missing_declarations_and_additions() {
        let mut required: GatesFile =
            toml::from_str("[budget]\ntotal='10s'\n[gates.unit]\ncommand='true'\nbudget='8s'\n")
                .unwrap();
        required.name_budget_sources("target");
        for text in [
            "[gates.unit]\ncommand='true'\n",
            "[budget]\ntotal='20s'\n[gates.unit]\ncommand='true'\nbudget='9s'\n",
            "[gates.extra]\ncommand='true'\nbudget='3s'\n",
        ] {
            let mut own: GatesFile = toml::from_str(text).unwrap();
            own.name_budget_sources("head or staging");
            required.owe_also(own, "target", "head");
        }
        assert_eq!(required.gates["unit"].budget, Some(8000));
        assert_eq!(required.budget.as_ref().unwrap().total, Some(10000));
        assert!(!required.budget_errors().is_empty());
        let identity = crate::gates::declaration_identity(&required.gates["unit"]);
        required.gates.get_mut("unit").unwrap().budget = Some(7000);
        assert_eq!(
            identity,
            crate::gates::declaration_identity(&required.gates["unit"])
        );
    }

    /// Claim: a suspension accepts an overrun only while it is in force at
    /// the judgement and changes no other verdict. Rejects a run that began
    /// under a suspension being accepted after it ended, and a suspension
    /// loosening an unmeasured run or labelling one that fits.
    #[test]
    fn a_suspension_accepts_only_an_overrun_judged_while_it_is_in_force() {
        let now = chrono::Utc::now();
        let declared = |until| Declared {
            budget_ms: Some(1000),
            sources: Vec::new(),
            suspension: Some(std::sync::Arc::new(Suspension {
                event_id: "01".into(),
                actor: "operator".into(),
                reason: "loaded host".into(),
                since: now - chrono::Duration::hours(2),
                until,
                gates: Vec::new(),
            })),
        };
        let ran = |ms| crate::verification_timing::VerificationTiming {
            run_ms: Some(ms),
            ..Default::default()
        };
        let live = declared(now + chrono::Duration::hours(1));
        let over = live.judge(&ran(1001));
        assert_eq!(over.verdict, BudgetVerdict::Suspended);
        assert!(over.accepted() && !over.fits() && over.suspension.is_some());
        let inside = live.judge(&ran(1000));
        assert_eq!(inside.verdict, BudgetVerdict::WithinBudget);
        assert!(inside.suspension.is_none());
        assert_eq!(
            live.judge(&Default::default()).verdict,
            BudgetVerdict::Unmeasured
        );
        let ended = declared(now - chrono::Duration::hours(1));
        assert_eq!(ended.judge(&ran(1001)).verdict, BudgetVerdict::OverBudget);
    }

    /// Claim: equality passes, one millisecond over fails, missing timing is unmeasured,
    /// and load only changes diagnostics. Rejects tolerance and load-dependent acceptance.
    #[test]
    fn boundary_and_load_do_not_change_the_judgement() {
        fn observed(ms: u64) -> crate::verification_timing::VerificationTiming {
            crate::verification_timing::VerificationTiming {
                run_ms: Some(ms),
                ..Default::default()
            }
        }
        assert!(judge(Some(10000), &[], None, &observed(10000)).accepted());
        assert!(!judge(Some(10000), &[], None, &observed(10001)).accepted());
        assert_eq!(
            judge(Some(10000), &[], None, &Default::default()).verdict,
            BudgetVerdict::Unmeasured
        );
        let b = judge(Some(10000), &[], None, &observed(11000));
        let low = crate::verification_timing::VerificationTiming {
            load_average: Some([0., 0., 0.]),
            core_count: Some(2),
            ..Default::default()
        };
        let high = crate::verification_timing::VerificationTiming {
            load_average: Some([10., 10., 10.]),
            ..low.clone()
        };
        assert!(!b
            .reason(&low, "host")
            .unwrap()
            .contains("possible host contention"));
        assert!(b
            .reason(&high, "host")
            .unwrap()
            .contains("possible host contention"));
        assert!(!b.accepted());
    }

    #[test]
    fn interface_budget_validation_selection_and_protection() {
        let mut gates: GatesFile =
            toml::from_str("[gates.unit]\ncommand='true'\nbudget='10s'\ninterface_budget='30s'\n")
                .unwrap();
        assert!(gates.budget_errors().is_empty());
        assert_eq!(gates.gates["unit"].effective_budget(), Some(10_000));
        gates.set_gate_trailers(crate::gate_trailers::GateTrailers {
            witnesses: None,
            change_size: Some(crate::gate_trailers::ChangeSize::Interface),
        });
        assert_eq!(gates.gates["unit"].effective_budget(), Some(30_000));

        let no_base: GatesFile =
            toml::from_str("[gates.unit]\ncommand='true'\ninterface_budget='30s'\n").unwrap();
        assert!(no_base
            .budget_errors()
            .iter()
            .any(|e| e.contains("interface_budget without budget")));

        let smaller: GatesFile =
            toml::from_str("[gates.unit]\ncommand='true'\nbudget='30s'\ninterface_budget='10s'\n")
                .unwrap();
        assert!(smaller
            .budget_errors()
            .iter()
            .any(|e| e.contains("is less than budget")));

        // Target with budget and no interface_budget cannot be widened by change.
        let mut target: GatesFile =
            toml::from_str("[gates.unit]\ncommand='true'\nbudget='10s'\n").unwrap();
        let own: GatesFile =
            toml::from_str("[gates.unit]\ncommand='true'\nbudget='10s'\ninterface_budget='30s'\n")
                .unwrap();
        target.owe_also(own, "target", "change");
        assert_eq!(target.gates["unit"].interface_budget, None);
    }
}
