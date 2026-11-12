//! Cost judgements use command observations without changing evidence identity.

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
        _ => false,
    }
}

fn minimum(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

pub fn merge_rules(into: &mut Option<BudgetRules>, other: Option<BudgetRules>) {
    if let Some(other) = other {
        let current = into.get_or_insert_with(BudgetRules::default);
        current.total = minimum(current.total, other.total);
        current.sources.extend(other.sources);
        current
            .sources
            .sort_by(|a, b| (&a.source, a.milliseconds).cmp(&(&b.source, b.milliseconds)));
        current.sources.dedup();
    }
}

impl Gate {
    pub fn absorb_budget(&mut self, other: &Gate) {
        self.budget = minimum(self.budget, other.budget);
        self.budget_sources.extend(other.budget_sources.clone());
        self.budget_sources
            .sort_by(|a, b| (&a.source, a.milliseconds).cmp(&(&b.source, b.milliseconds)));
        self.budget_sources.dedup();
    }

    pub fn budget_judgement(&self, run_ms: Option<u64>) -> BudgetJudgement {
        judge(self.budget, &self.budget_sources, run_ms)
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
        } else if let Some(budget) = self.budget {
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
        self.budget.is_some() || self.gates.values().any(|g| g.budget.is_some())
    }

    /// Validate the complete effective suite, including profiles other than the active one.
    pub fn budget_errors(&self) -> Vec<String> {
        let Some(rules) = &self.budget else {
            return Vec::new();
        };
        let mut profiles = std::collections::BTreeSet::from([String::new()]);
        profiles.extend(self.gates.values().flat_map(|g| g.profiles.clone()));
        let mut errors = Vec::new();
        for profile in profiles {
            let label = if profile.is_empty() {
                "unqualified"
            } else {
                &profile
            };
            let mut sum = Some(0_u64);
            for (name, gate) in self.required_for(&profile) {
                match gate.budget {
                    None => errors.push(format!(
                        "budget: integration gate {name} needs a budget (profile {label})"
                    )),
                    Some(ms) => sum = sum.and_then(|sum| sum.checked_add(ms)),
                }
            }
            match (sum, rules.total) {
                (None, _) => errors.push(format!("budget sum overflow (profile {label})")),
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
    Unmeasured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetJudgement {
    pub verdict: BudgetVerdict,
    pub budget_ms: Option<u64>,
    pub run_ms: Option<u64>,
    pub excess_ms: Option<u64>,
    pub sources: Vec<BudgetSource>,
}

pub fn judge(
    budget_ms: Option<u64>,
    provenance: &[BudgetSource],
    run_ms: Option<u64>,
) -> BudgetJudgement {
    let verdict = match (budget_ms, run_ms) {
        (None, _) => BudgetVerdict::Undeclared,
        (Some(_), None) => BudgetVerdict::Unmeasured,
        (Some(b), Some(r)) if r > b => BudgetVerdict::OverBudget,
        _ => BudgetVerdict::WithinBudget,
    };
    BudgetJudgement {
        verdict,
        budget_ms,
        run_ms,
        excess_ms: run_ms.zip(budget_ms).map(|(r, b)| r.saturating_sub(b)),
        sources: provenance.to_vec(),
    }
}

impl BudgetJudgement {
    pub fn accepted(&self) -> bool {
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
        reason.push_str("; inspect arc gates over, make it faster, remove checks worth less, then consider a declared raise");
        Some(reason)
    }
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

    /// Claim: equality passes, one millisecond over fails, missing timing is unmeasured,
    /// and load only changes diagnostics. Rejects tolerance and load-dependent acceptance.
    #[test]
    fn boundary_and_load_do_not_change_the_judgement() {
        assert!(judge(Some(10000), &[], Some(10000)).accepted());
        assert!(!judge(Some(10000), &[], Some(10001)).accepted());
        assert_eq!(
            judge(Some(10000), &[], None).verdict,
            BudgetVerdict::Unmeasured
        );
        let b = judge(Some(10000), &[], Some(11000));
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
}
