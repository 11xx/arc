//! The integration blockers, and the one pure function that derives them from
//! facts about a change already read from Git, the ledger, and policy.
//!
//! [`PRIORITY`] is the only statement of blocker order. The derivation lists
//! blockers in that order, and `arc check` exits with the code of the first one
//! present, so the list a report prints and the code a caller branches on can
//! never disagree. Nothing here reads Git, the store, or the clock: every input
//! is a fact the caller has already computed, and the same facts always yield
//! the same blockers.

use serde::Serialize;

/// A reason a change cannot integrate. Declared in [`PRIORITY`] order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Blocker {
    Closed,
    BranchMissing,
    TargetUnreadable,
    ForkBranch,
    Iterating,
    BlockedByChanges,
    NeedsRebase,
    MergedTreeUnevaluated,
    BlockingFindings,
    NoValidApproval,
    GatesNotGreen,
    AcceptanceProbesNotGreen,
    HoldActive,
}

/// Every blocker, highest precedence first.
pub const PRIORITY: [Blocker; 13] = [
    Blocker::Closed,
    Blocker::BranchMissing,
    Blocker::TargetUnreadable,
    Blocker::ForkBranch,
    Blocker::Iterating,
    Blocker::BlockedByChanges,
    Blocker::NeedsRebase,
    Blocker::MergedTreeUnevaluated,
    Blocker::BlockingFindings,
    Blocker::NoValidApproval,
    Blocker::GatesNotGreen,
    Blocker::AcceptanceProbesNotGreen,
    Blocker::HoldActive,
];

impl Blocker {
    pub fn exit_code(self) -> i32 {
        match self {
            Blocker::Closed | Blocker::BranchMissing | Blocker::TargetUnreadable => 6,
            Blocker::ForkBranch => 15,
            Blocker::Iterating => 13,
            Blocker::BlockedByChanges => 7,
            Blocker::NeedsRebase => 11,
            Blocker::MergedTreeUnevaluated => 14,
            Blocker::BlockingFindings => 2,
            Blocker::NoValidApproval => 3,
            Blocker::GatesNotGreen => 5,
            Blocker::AcceptanceProbesNotGreen => 12,
            Blocker::HoldActive => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Blocker::Closed => "closed",
            Blocker::BranchMissing => "branch-missing",
            Blocker::TargetUnreadable => "target-unreadable",
            Blocker::ForkBranch => "fork-branch",
            Blocker::Iterating => "iterating",
            Blocker::BlockedByChanges => "blocked-by-changes",
            Blocker::NeedsRebase => "needs-rebase",
            Blocker::MergedTreeUnevaluated => "merged-tree-unevaluated",
            Blocker::BlockingFindings => "blocking-findings",
            Blocker::NoValidApproval => "no-valid-approval",
            Blocker::GatesNotGreen => "gates-not-green",
            Blocker::AcceptanceProbesNotGreen => "acceptance-probes-not-green",
            Blocker::HoldActive => "hold-active",
        }
    }
}

/// What the derivation reads of one declared gate.
#[derive(Debug, Clone, Copy)]
pub struct GateFact {
    pub green_at_head: bool,
    /// Some run is counted for this gate at the tree it answers for.
    pub evidence_recorded: bool,
}

/// The facts a change's blockers follow from.
#[derive(Debug, Clone)]
pub struct BlockerFacts {
    pub closed: bool,
    pub branch_missing: bool,
    /// The change's target branch cannot be resolved, so the declarations it
    /// is judged by cannot be read either.
    pub target_unreadable: bool,
    /// The change's branch is a fork's, where work stays unintegrated on
    /// purpose.
    pub on_fork: bool,
    pub iterating: bool,
    pub blocked_by_changes: bool,
    pub needs_rebase: bool,
    /// The gates answer for a merged tree rather than the head's own.
    pub merged_tree_evaluated: bool,
    pub gates: Vec<GateFact>,
    pub open_blocking_findings: bool,
    pub approval_valid: bool,
    pub waiver_satisfies_approval: bool,
    /// Whether each acceptance probe discriminates at the head.
    pub probes_discriminating: Vec<bool>,
    pub hold_active: bool,
}

impl BlockerFacts {
    fn raises(&self, blocker: Blocker) -> bool {
        match blocker {
            Blocker::Closed => self.closed,
            Blocker::BranchMissing => self.branch_missing,
            Blocker::TargetUnreadable => self.target_unreadable,
            Blocker::ForkBranch => self.on_fork,
            Blocker::Iterating => self.iterating,
            Blocker::BlockedByChanges => self.blocked_by_changes,
            Blocker::NeedsRebase => self.needs_rebase,
            // Nothing has been run against the content that would ship. A
            // tree some gates have answered for and others have not is an
            // ordinary red gate; this is the case where the whole evaluation
            // is missing, and where running a gate at the head would record
            // it against the wrong tree.
            Blocker::MergedTreeUnevaluated => {
                self.merged_tree_evaluated
                    && !self.gates.is_empty()
                    && !self.gates.iter().any(|gate| gate.evidence_recorded)
            }
            Blocker::BlockingFindings => self.open_blocking_findings,
            Blocker::NoValidApproval => {
                !self.iterating && !self.approval_valid && !self.waiver_satisfies_approval
            }
            Blocker::GatesNotGreen => self.gates.iter().any(|gate| !gate.green_at_head),
            Blocker::AcceptanceProbesNotGreen => {
                self.probes_discriminating.iter().any(|probe| !probe)
            }
            Blocker::HoldActive => self.hold_active,
        }
    }
}

/// Every blocker the facts raise, in [`PRIORITY`] order.
pub fn derive(facts: &BlockerFacts) -> Vec<Blocker> {
    PRIORITY
        .into_iter()
        .filter(|blocker| facts.raises(*blocker))
        .collect()
}

/// The exit code of the highest-precedence blocker present, or `None` when
/// there is none.
pub fn exit_code(blockers: &[Blocker]) -> Option<i32> {
    PRIORITY
        .into_iter()
        .find(|blocker| blockers.contains(blocker))
        .map(Blocker::exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear() -> BlockerFacts {
        BlockerFacts {
            closed: false,
            branch_missing: false,
            target_unreadable: false,
            on_fork: false,
            iterating: false,
            blocked_by_changes: false,
            needs_rebase: false,
            merged_tree_evaluated: false,
            gates: Vec::new(),
            open_blocking_findings: false,
            approval_valid: true,
            waiver_satisfies_approval: false,
            probes_discriminating: Vec::new(),
            hold_active: false,
        }
    }

    const GREEN_WITH_EVIDENCE: GateFact = GateFact {
        green_at_head: true,
        evidence_recorded: true,
    };

    #[test]
    fn clear_facts_raise_nothing() {
        assert_eq!(derive(&clear()), Vec::<Blocker>::new());
    }

    type Raise = fn(&mut BlockerFacts);

    #[test]
    fn each_fact_alone_raises_its_blocker() {
        let cases: [(Raise, Blocker); 13] = [
            (|f| f.closed = true, Blocker::Closed),
            (|f| f.branch_missing = true, Blocker::BranchMissing),
            (|f| f.target_unreadable = true, Blocker::TargetUnreadable),
            (|f| f.on_fork = true, Blocker::ForkBranch),
            (|f| f.iterating = true, Blocker::Iterating),
            (|f| f.blocked_by_changes = true, Blocker::BlockedByChanges),
            (|f| f.needs_rebase = true, Blocker::NeedsRebase),
            (
                |f| {
                    f.merged_tree_evaluated = true;
                    f.gates = vec![GateFact {
                        green_at_head: true,
                        evidence_recorded: false,
                    }];
                },
                Blocker::MergedTreeUnevaluated,
            ),
            (
                |f| f.open_blocking_findings = true,
                Blocker::BlockingFindings,
            ),
            (|f| f.approval_valid = false, Blocker::NoValidApproval),
            (
                |f| {
                    f.gates = vec![
                        GREEN_WITH_EVIDENCE,
                        GateFact {
                            green_at_head: false,
                            evidence_recorded: true,
                        },
                    ]
                },
                Blocker::GatesNotGreen,
            ),
            (
                |f| f.probes_discriminating = vec![true, false],
                Blocker::AcceptanceProbesNotGreen,
            ),
            (|f| f.hold_active = true, Blocker::HoldActive),
        ];
        for (raise, expected) in cases {
            let mut facts = clear();
            raise(&mut facts);
            assert_eq!(derive(&facts), vec![expected]);
        }
    }

    #[test]
    fn all_facts_raise_every_blocker_in_priority_order() {
        let facts = BlockerFacts {
            closed: true,
            branch_missing: true,
            target_unreadable: true,
            on_fork: true,
            iterating: false,
            blocked_by_changes: true,
            needs_rebase: true,
            merged_tree_evaluated: true,
            gates: vec![GateFact {
                green_at_head: false,
                evidence_recorded: false,
            }],
            open_blocking_findings: true,
            approval_valid: false,
            waiver_satisfies_approval: false,
            probes_discriminating: vec![false],
            hold_active: true,
        };
        // Iterating suspends the approval requirement, so the two cannot be
        // raised together; each is shown in its own list.
        let without_iterating: Vec<Blocker> = PRIORITY
            .into_iter()
            .filter(|blocker| *blocker != Blocker::Iterating)
            .collect();
        assert_eq!(derive(&facts), without_iterating);

        let iterating = BlockerFacts {
            iterating: true,
            ..facts
        };
        let without_approval: Vec<Blocker> = PRIORITY
            .into_iter()
            .filter(|blocker| *blocker != Blocker::NoValidApproval)
            .collect();
        assert_eq!(derive(&iterating), without_approval);
    }

    #[test]
    fn priority_names_every_blocker_once() {
        let mut seen = PRIORITY.to_vec();
        seen.sort_by_key(|blocker| blocker.as_str());
        seen.dedup();
        assert_eq!(seen.len(), PRIORITY.len());
        assert_eq!(PRIORITY.len(), 13);
    }

    #[test]
    fn waiver_satisfies_a_missing_approval() {
        let facts = BlockerFacts {
            approval_valid: false,
            waiver_satisfies_approval: true,
            ..clear()
        };
        assert_eq!(derive(&facts), Vec::<Blocker>::new());
    }

    #[test]
    fn merged_tree_is_evaluated_once_any_gate_has_evidence() {
        let unevaluated = GateFact {
            green_at_head: true,
            evidence_recorded: false,
        };
        let facts = BlockerFacts {
            merged_tree_evaluated: true,
            gates: vec![unevaluated, GREEN_WITH_EVIDENCE],
            ..clear()
        };
        assert_eq!(derive(&facts), Vec::<Blocker>::new());

        // No declared gate leaves nothing to evaluate.
        let no_gates = BlockerFacts {
            merged_tree_evaluated: true,
            ..clear()
        };
        assert_eq!(derive(&no_gates), Vec::<Blocker>::new());

        // The head's own tree is what ships, so missing evidence there is a
        // red gate, not an unevaluated merge.
        let at_head = BlockerFacts {
            gates: vec![unevaluated],
            ..clear()
        };
        assert_eq!(derive(&at_head), Vec::<Blocker>::new());
    }

    #[test]
    fn exit_code_is_the_first_blocker_in_priority_order() {
        let facts = BlockerFacts {
            needs_rebase: true,
            open_blocking_findings: true,
            approval_valid: false,
            hold_active: true,
            ..clear()
        };
        let blockers = derive(&facts);
        assert_eq!(
            blockers,
            vec![
                Blocker::NeedsRebase,
                Blocker::BlockingFindings,
                Blocker::NoValidApproval,
                Blocker::HoldActive,
            ]
        );
        assert_eq!(exit_code(&blockers), Some(Blocker::NeedsRebase.exit_code()));
        assert_eq!(exit_code(&[Blocker::HoldActive, Blocker::Closed]), Some(6));
        assert_eq!(exit_code(&[]), None);
    }
}
