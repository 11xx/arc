//! Whether one change may be integrated, as one pure function over facts
//! observed under the target and change locks.
//!
//! [`decide`] reads no Git, no store, and no clock. Everything it needs is in
//! [`IntegrationFacts`], so the same facts always yield the same plan or the
//! same refusal. The caller performs the merge from the [`MergePlan`] and
//! confirms the result against it; nothing about the merge is re-derived
//! after the decision.
//!
//! A caller holding the basis an earlier dry run reported passes it as the
//! prior basis. The answer is then accompanied by the coordinates that moved
//! since, which a refusal or a warning names. The prior basis never changes
//! the decision: the fresh facts alone decide.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

use crate::model::{AuthorizationBasis, ExternalVerdictBasis, NormalizedGate, NormalizedPolicy};
use crate::policy::Contribution;
use crate::state::ChangeState;
use crate::status::StatusReport;

pub const PLAN_SCHEMA: &str = "arc-integration-plan/1";

const CONFIGURATION_MOVED: &str = "gate or policy configuration changed while preparing the \
                                   merge; nothing was written — re-run once the worktree has \
                                   settled";

/// The checkout a merge into a target runs in, or a promotion moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetCheckout {
    pub path: PathBuf,
    /// The branch the checkout holds when it does not hold the target and
    /// must be moved onto it before the merge.
    pub switch_from: Option<String>,
}

/// The newest patchset: the one an integration merges, by exact head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approved {
    pub patchset_id: String,
    pub head: String,
}

/// What authorizes the newest patchset, as the readiness report reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Approval {
    pub verdict_event_id: Option<String>,
    pub verdict_provisional: Option<String>,
    pub external_verdict: Option<ExternalVerdictBasis>,
    /// Only when the waiver is what let the approval stand.
    pub audit_debt_event_id: Option<String>,
}

/// The gate and policy declarations an integration consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declarations {
    pub gates: BTreeMap<String, NormalizedGate>,
    pub policy: NormalizedPolicy,
}

/// The second reading of readiness and the basis, taken after the first so
/// configuration moving between the two is caught.
pub struct Confirmation {
    pub ready: bool,
    pub basis: anyhow::Result<AuthorizationBasis>,
}

/// The facts one integration decision follows from. Each observation that
/// can fail carries its error, which becomes the refusal only when the
/// decision reaches it.
pub struct IntegrationFacts {
    pub target: String,
    pub approved: Option<Approved>,
    /// Readiness under the locks.
    pub ready: bool,
    pub approval: Approval,
    pub declarations: Option<Declarations>,
    pub basis: anyhow::Result<AuthorizationBasis>,
    pub confirmation: anyhow::Result<Confirmation>,
    pub contribution: anyhow::Result<Option<Contribution>>,
    pub checkout: anyhow::Result<TargetCheckout>,
    /// The target checkout carries tracked modifications.
    pub tracked_dirt: anyhow::Result<bool>,
    /// The target branch's revision.
    pub target_revision: anyhow::Result<String>,
    /// The tree a clean merge of the approved head into the target yields.
    pub evaluated_tree: anyhow::Result<Option<String>>,
    /// Untracked or ignored paths in the target checkout the merge, or the
    /// switch onto the target, would write.
    pub write_collisions: anyhow::Result<Vec<String>>,
    /// Untracked or ignored paths in the target checkout.
    pub untouched: anyhow::Result<Vec<String>>,
    /// The target already contains the approved head.
    pub already_contained: anyhow::Result<bool>,
}

/// A merge the facts authorize, and everything the merge is confirmed
/// against once it has run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergePlan {
    pub target: String,
    pub old_target: String,
    pub approved: Approved,
    pub evaluated_tree: Option<String>,
    /// False when the target already contains the approved head: Git creates
    /// no merge commit, and the target revision is the integration.
    pub merge_commit: bool,
    pub authorization: AuthorizationBasis,
    pub checkout: TargetCheckout,
    pub untouched: Vec<String>,
}

/// A contributed change the facts authorize recording ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendPlan {
    pub target: String,
    pub target_revision: String,
    pub approved: Approved,
    pub contribution: Contribution,
    pub authorization: AuthorizationBasis,
}

#[derive(Debug)]
pub enum Refusal {
    /// The readiness report blocks; it explains why.
    NotReady,
    /// Readiness or the basis differed between two readings.
    ConfigurationMoved,
    TrackedDirt {
        checkout: PathBuf,
    },
    WriteCollision {
        checkout: PathBuf,
        paths: Vec<String>,
    },
    /// An observation the decision needed could not be made, or refused on
    /// its own terms.
    Failed(anyhow::Error),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotReady => f.write_str("the change is not ready to integrate"),
            Refusal::ConfigurationMoved => f.write_str(CONFIGURATION_MOVED),
            Refusal::TrackedDirt { checkout } => write!(
                f,
                "worktree {} carries tracked modifications, staged or unstaged; commit the work in progress or copy it aside first",
                checkout.display()
            ),
            Refusal::WriteCollision { checkout, paths } => write!(
                f,
                "the update would write over paths the worktree {} holds untracked or ignored: \
                 {}",
                checkout.display(),
                paths.join(", ")
            ),
            Refusal::Failed(error) => write!(f, "{error:#}"),
        }
    }
}

impl Refusal {
    pub fn into_error(self) -> anyhow::Error {
        match self {
            Refusal::Failed(error) => error,
            other => anyhow!(other.to_string()),
        }
    }
}

#[derive(Debug)]
pub enum Decision {
    Merge(MergePlan),
    ReadyToSend(SendPlan),
    Refused(Refusal),
}

/// One coordinate of a prior basis that the fresh facts no longer match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moved {
    ApprovedHead { from: String, to: String },
    Target { from: String, to: String },
    Gates { changed: Vec<String> },
    Policy,
    Approval { from: String, to: String },
}

impl fmt::Display for Moved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Moved::ApprovedHead { from, to } => {
                write!(f, "the approved head you checked moved from {from} to {to}")
            }
            Moved::Target { from, to } => {
                write!(f, "the target you checked moved from {from} to {to}")
            }
            Moved::Gates { changed } => write!(
                f,
                "the gate declarations you checked changed: {}",
                changed.join(", ")
            ),
            Moved::Policy => f.write_str("the policy you checked changed"),
            Moved::Approval { from, to } => {
                write!(f, "the approval you checked moved from {from} to {to}")
            }
        }
    }
}

pub struct IntegrationOutcome {
    pub decision: Decision,
    /// The coordinates of the prior basis the fresh facts no longer match.
    /// Empty without a prior basis, and for a coordinate the facts could not
    /// observe.
    pub moved: Vec<Moved>,
}

/// One line naming everything that moved, or `None` when nothing did.
pub fn describe_moved(moved: &[Moved]) -> Option<String> {
    (!moved.is_empty()).then(|| {
        moved
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })
}

/// The basis an integration would record, and the target revision it was
/// decided at, as `arc integrate --dry-run --json` prints it and
/// `--expect-basis` reads it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationPlan {
    pub schema: String,
    pub change_id: String,
    /// `change-integrated`, or `ready-to-send` for a contributed change.
    pub records: String,
    pub target: String,
    pub target_revision: String,
    pub approved_patchset_id: String,
    pub approved_head: String,
    pub merge_commit: bool,
    pub evaluated_tree: Option<String>,
    pub authorization: AuthorizationBasis,
}

impl IntegrationPlan {
    pub fn of_merge(change_id: &str, plan: &MergePlan) -> Self {
        IntegrationPlan {
            schema: PLAN_SCHEMA.to_string(),
            change_id: change_id.to_string(),
            records: "change-integrated".to_string(),
            target: plan.target.clone(),
            target_revision: plan.old_target.clone(),
            approved_patchset_id: plan.approved.patchset_id.clone(),
            approved_head: plan.approved.head.clone(),
            merge_commit: plan.merge_commit,
            evaluated_tree: plan.evaluated_tree.clone(),
            authorization: plan.authorization.clone(),
        }
    }

    pub fn of_send(change_id: &str, plan: &SendPlan) -> Self {
        IntegrationPlan {
            schema: PLAN_SCHEMA.to_string(),
            change_id: change_id.to_string(),
            records: "ready-to-send".to_string(),
            target: plan.target.clone(),
            target_revision: plan.target_revision.clone(),
            approved_patchset_id: plan.approved.patchset_id.clone(),
            approved_head: plan.approved.head.clone(),
            merge_commit: false,
            evaluated_tree: None,
            authorization: plan.authorization.clone(),
        }
    }
}

/// What authorizes the approved patchset: a verdict that approved it, an
/// external approval gating the current head, and the debt whose waiver let
/// the approval stand.
pub fn approval(st: &ChangeState, report: &StatusReport, approved_patchset_id: &str) -> Approval {
    let verdict_authorized_locally = report
        .verdict
        .as_ref()
        .is_some_and(|verdict| verdict.valid_for_current_head || report.approval_waived_by_debt);
    let verdict = verdict_authorized_locally
        .then(|| {
            st.verdicts.iter().rev().find(|verdict| {
                verdict.patchset_id == approved_patchset_id
                    && verdict.verdict == crate::model::Verdict::Approved
            })
        })
        .flatten();
    let external_verdict = report
        .external_verdicts
        .iter()
        .find(|external| {
            external.gates_current_head
                && external.verdict == crate::model::ExternalVerdict::Approved
        })
        .map(|external| ExternalVerdictBasis {
            event_id: external.event_id.clone(),
            revision: external.revision.clone(),
            verdict: external.verdict,
            decided_by: external.decided_by.clone(),
            reference: external.reference.clone(),
        });
    Approval {
        verdict_event_id: verdict.map(|verdict| verdict.event_id.clone()),
        verdict_provisional: verdict.and_then(|verdict| verdict.provisional.clone()),
        external_verdict,
        // A debt declared beside an approval that needed no waiver authorized
        // nothing, and recording it would claim the merge rested on
        // something it did not.
        audit_debt_event_id: st
            .debt
            .as_ref()
            .filter(|_| report.approval_waived_by_debt)
            .map(|debt| debt.event_id.clone()),
    }
}

/// Refuse a write into a checkout carrying tracked modifications, staged or
/// unstaged. A merge or a promotion beside uncommitted work writes into a
/// tree nobody can name afterwards.
pub fn guard_tracked_dirt(checkout: &Path, tracked_dirt: bool) -> Result<(), Refusal> {
    if tracked_dirt {
        return Err(Refusal::TrackedDirt {
            checkout: checkout.to_path_buf(),
        });
    }
    Ok(())
}

/// Refuse a write over paths the checkout holds untracked or ignored, given
/// the collisions between those paths and what the write would produce.
pub fn guard_writes(checkout: &Path, collisions: Vec<String>) -> Result<(), Refusal> {
    if !collisions.is_empty() {
        return Err(Refusal::WriteCollision {
            checkout: checkout.to_path_buf(),
            paths: collisions,
        });
    }
    Ok(())
}

pub fn decide(facts: IntegrationFacts, prior: Option<&IntegrationPlan>) -> IntegrationOutcome {
    let moved = prior.map(|prior| moved(prior, &facts)).unwrap_or_default();
    IntegrationOutcome {
        decision: decision(facts),
        moved,
    }
}

fn decision(facts: IntegrationFacts) -> Decision {
    macro_rules! observed {
        ($fact:expr) => {
            match $fact {
                Ok(value) => value,
                Err(error) => return Decision::Refused(Refusal::Failed(error)),
            }
        };
    }
    if !facts.ready {
        return Decision::Refused(Refusal::NotReady);
    }
    let Some(approved) = facts.approved else {
        return Decision::Refused(Refusal::Failed(anyhow!("no patchset recorded")));
    };
    let authorization = observed!(facts.basis);
    // Configuration files are not under any lock arc holds, so readiness and
    // the basis are two reads of something that can move between them.
    // Agreement between the two readings is what keeps the merge from
    // proceeding under one configuration and recording another.
    let confirmation = observed!(facts.confirmation);
    let confirmed = observed!(confirmation.basis);
    if confirmed != authorization || !confirmation.ready {
        return Decision::Refused(Refusal::ConfigurationMoved);
    }

    if let Some(contribution) = observed!(facts.contribution) {
        return Decision::ReadyToSend(SendPlan {
            target: facts.target,
            target_revision: observed!(facts.target_revision),
            approved,
            contribution,
            authorization,
        });
    }

    let checkout = observed!(facts.checkout);
    if let Err(refusal) = guard_tracked_dirt(&checkout.path, observed!(facts.tracked_dirt)) {
        return Decision::Refused(refusal);
    }
    let old_target = observed!(facts.target_revision);
    let evaluated_tree = observed!(facts.evaluated_tree);
    if let Err(refusal) = guard_writes(&checkout.path, observed!(facts.write_collisions)) {
        return Decision::Refused(refusal);
    }
    let untouched = observed!(facts.untouched);
    let already_contained = observed!(facts.already_contained);
    Decision::Merge(MergePlan {
        target: facts.target,
        old_target,
        approved,
        evaluated_tree,
        merge_commit: !already_contained,
        authorization,
        checkout,
        untouched,
    })
}

/// The coordinates of `prior` the fresh facts contradict. A coordinate the
/// facts could not observe is not reported: unknown is not moved.
fn moved(prior: &IntegrationPlan, facts: &IntegrationFacts) -> Vec<Moved> {
    let mut moved = Vec::new();

    let prior_head = format!("{} ({})", prior.approved_patchset_id, prior.approved_head);
    match &facts.approved {
        Some(approved)
            if approved.patchset_id == prior.approved_patchset_id
                && approved.head == prior.approved_head => {}
        Some(approved) => moved.push(Moved::ApprovedHead {
            from: prior_head,
            to: format!("{} ({})", approved.patchset_id, approved.head),
        }),
        None => moved.push(Moved::ApprovedHead {
            from: prior_head,
            to: "no patchset".to_string(),
        }),
    }

    if let Ok(revision) = &facts.target_revision {
        if prior.target != facts.target {
            moved.push(Moved::Target {
                from: format!("{} at {}", prior.target, prior.target_revision),
                to: format!("{} at {revision}", facts.target),
            });
        } else if &prior.target_revision != revision {
            moved.push(Moved::Target {
                from: prior.target_revision.clone(),
                to: revision.clone(),
            });
        }
    }

    if let Some(declarations) = &facts.declarations {
        let before = &prior.authorization.gates;
        let after = &declarations.gates;
        let changed: Vec<String> = before
            .keys()
            .chain(after.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|name| before.get(*name) != after.get(*name))
            .cloned()
            .collect();
        if !changed.is_empty() {
            moved.push(Moved::Gates { changed });
        }
        if prior.authorization.policy != declarations.policy {
            moved.push(Moved::Policy);
        }
    }

    let prior_approval = approval_label(
        prior.authorization.verdict_event_id.as_deref(),
        prior.authorization.external_verdict.as_ref(),
        prior.authorization.audit_debt_event_id.as_deref(),
    );
    let fresh_approval = approval_label(
        facts.approval.verdict_event_id.as_deref(),
        facts.approval.external_verdict.as_ref(),
        facts.approval.audit_debt_event_id.as_deref(),
    );
    if prior_approval != fresh_approval {
        moved.push(Moved::Approval {
            from: prior_approval,
            to: fresh_approval,
        });
    }
    moved
}

fn approval_label(
    verdict: Option<&str>,
    external: Option<&ExternalVerdictBasis>,
    debt: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(verdict) = verdict {
        parts.push(format!("verdict {verdict}"));
    }
    if let Some(external) = external {
        parts.push(format!("external verdict {}", external.event_id));
    }
    if let Some(debt) = debt {
        parts.push(format!("debt {debt}"));
    }
    if parts.is_empty() {
        "nothing".to_string()
    } else {
        parts.join(" with ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD_TARGET: &str = "1111111111111111111111111111111111111111";
    const NEW_TARGET: &str = "2222222222222222222222222222222222222222";
    const HEAD: &str = "3333333333333333333333333333333333333333";
    const TREE: &str = "4444444444444444444444444444444444444444";

    fn policy() -> NormalizedPolicy {
        NormalizedPolicy {
            forbid_self_approval: false,
            require_declared_actor: false,
            provenance_git_identity: "shared".to_string(),
            declared_by: BTreeMap::new(),
        }
    }

    fn gates() -> BTreeMap<String, NormalizedGate> {
        BTreeMap::from([(
            "build".to_string(),
            NormalizedGate {
                command: "make build".to_string(),
                profiles: Vec::new(),
                timeout: None,
                declared_by: vec!["project".to_string()],
            },
        )])
    }

    fn basis() -> AuthorizationBasis {
        AuthorizationBasis {
            verdict_event_id: Some("verdict-1".to_string()),
            external_verdict: None,
            gate_evidence: BTreeMap::from([("build".to_string(), "evidence-1".to_string())]),
            prerequisites: Vec::new(),
            blocking_findings: Vec::new(),
            holds: Vec::new(),
            gates: gates(),
            policy: policy(),
            danger: None,
            audit_debt_event_id: None,
            verdict_provisional: None,
        }
    }

    fn checkout() -> TargetCheckout {
        TargetCheckout {
            path: PathBuf::from("/checkout"),
            switch_from: None,
        }
    }

    /// Facts that authorize a clean merge of `HEAD` into `main` at
    /// `OLD_TARGET`.
    fn ready() -> IntegrationFacts {
        IntegrationFacts {
            target: "main".to_string(),
            approved: Some(Approved {
                patchset_id: "ps-1".to_string(),
                head: HEAD.to_string(),
            }),
            ready: true,
            approval: Approval {
                verdict_event_id: Some("verdict-1".to_string()),
                ..Approval::default()
            },
            declarations: Some(Declarations {
                gates: gates(),
                policy: policy(),
            }),
            basis: Ok(basis()),
            confirmation: Ok(Confirmation {
                ready: true,
                basis: Ok(basis()),
            }),
            contribution: Ok(None),
            checkout: Ok(checkout()),
            tracked_dirt: Ok(false),
            target_revision: Ok(OLD_TARGET.to_string()),
            evaluated_tree: Ok(Some(TREE.to_string())),
            write_collisions: Ok(Vec::new()),
            untouched: Ok(Vec::new()),
            already_contained: Ok(false),
        }
    }

    /// The plan a dry run over [`ready`] prints.
    fn prior() -> IntegrationPlan {
        let Decision::Merge(plan) = decide(ready(), None).decision else {
            panic!("ready facts must plan a merge");
        };
        IntegrationPlan::of_merge("change-1", &plan)
    }

    fn refusal(facts: IntegrationFacts) -> Refusal {
        match decide(facts, None).decision {
            Decision::Refused(refusal) => refusal,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn ready_facts_plan_a_merge_of_the_approved_head_at_the_observed_target() {
        let outcome = decide(ready(), None);
        let Decision::Merge(plan) = outcome.decision else {
            panic!("expected a merge plan");
        };
        assert_eq!(plan.target, "main");
        assert_eq!(plan.old_target, OLD_TARGET);
        assert_eq!(plan.approved.head, HEAD);
        assert_eq!(plan.evaluated_tree.as_deref(), Some(TREE));
        assert!(plan.merge_commit);
        assert_eq!(plan.authorization, basis());
        assert!(outcome.moved.is_empty());
    }

    #[test]
    fn an_already_contained_head_plans_no_merge_commit() {
        let facts = IntegrationFacts {
            already_contained: Ok(true),
            ..ready()
        };
        let Decision::Merge(plan) = decide(facts, None).decision else {
            panic!("expected a merge plan");
        };
        assert!(!plan.merge_commit);
        assert_eq!(plan.old_target, OLD_TARGET);
    }

    #[test]
    fn a_contributed_change_plans_ready_to_send_before_any_checkout_fact() {
        let facts = IntegrationFacts {
            contribution: Ok(Some(Contribution::default())),
            checkout: Err(anyhow!("no worktree has \"main\" checked out")),
            ..ready()
        };
        let Decision::ReadyToSend(send) = decide(facts, None).decision else {
            panic!("expected ready to send");
        };
        assert_eq!(send.approved.head, HEAD);
        assert_eq!(send.target_revision, OLD_TARGET);
    }

    #[test]
    fn readiness_refuses_before_any_later_fact_is_consulted() {
        let facts = IntegrationFacts {
            ready: false,
            basis: Err(anyhow!("nothing authorizes the merged patchset")),
            checkout: Err(anyhow!("no worktree")),
            ..ready()
        };
        assert!(matches!(refusal(facts), Refusal::NotReady));
    }

    #[test]
    fn a_basis_the_second_reading_disagrees_with_refuses() {
        let mut moved_basis = basis();
        moved_basis.policy.forbid_self_approval = true;
        let differs = IntegrationFacts {
            confirmation: Ok(Confirmation {
                ready: true,
                basis: Ok(moved_basis),
            }),
            ..ready()
        };
        assert!(matches!(refusal(differs), Refusal::ConfigurationMoved));
        let unready = IntegrationFacts {
            confirmation: Ok(Confirmation {
                ready: false,
                basis: Ok(basis()),
            }),
            ..ready()
        };
        let refused = refusal(unready);
        assert!(matches!(refused, Refusal::ConfigurationMoved));
        assert!(refused.to_string().contains("nothing was written"));
    }

    #[test]
    fn an_unobservable_fact_refuses_with_its_own_error() {
        let facts = IntegrationFacts {
            checkout: Err(anyhow!(
                "no worktree has \"main\" checked out; check it out first"
            )),
            ..ready()
        };
        let refused = refusal(facts);
        assert!(matches!(refused, Refusal::Failed(_)));
        assert!(refused.to_string().contains("check it out first"));
        let unapproved = IntegrationFacts {
            basis: Err(anyhow!("nothing authorizes the merged patchset")),
            ..ready()
        };
        assert!(refusal(unapproved)
            .to_string()
            .contains("nothing authorizes"));
    }

    #[test]
    fn tracked_dirt_in_the_target_checkout_refuses_by_its_own_reason() {
        let facts = IntegrationFacts {
            tracked_dirt: Ok(true),
            ..ready()
        };
        let refused = refusal(facts);
        assert!(matches!(refused, Refusal::TrackedDirt { .. }));
        assert!(refused
            .to_string()
            .contains("carries tracked modifications"));
    }

    #[test]
    fn a_write_over_an_untracked_path_refuses_naming_it() {
        let facts = IntegrationFacts {
            write_collisions: Ok(vec!["notes.txt".to_string()]),
            ..ready()
        };
        let refused = refusal(facts);
        assert!(matches!(refused, Refusal::WriteCollision { .. }));
        assert!(refused
            .to_string()
            .ends_with("untracked or ignored: notes.txt"));
    }

    #[test]
    fn the_checkout_guards_word_a_refusal_the_same_for_every_caller() {
        let checkout = Path::new("/repo");
        assert!(guard_tracked_dirt(checkout, false).is_ok());
        assert_eq!(
            guard_tracked_dirt(checkout, true).unwrap_err().to_string(),
            "worktree /repo carries tracked modifications, staged or unstaged; commit the work in progress or copy it aside first"
        );
        assert!(guard_writes(checkout, Vec::new()).is_ok());
        let collisions = vec!["notes.txt".to_string(), "out/".to_string()];
        assert_eq!(
            guard_writes(checkout, collisions).unwrap_err().to_string(),
            "the update would write over paths the worktree /repo holds untracked or ignored: \
             notes.txt, out/"
        );
    }

    #[test]
    fn an_unchanged_prior_basis_moves_nothing() {
        let outcome = decide(ready(), Some(&prior()));
        assert!(outcome.moved.is_empty());
        assert_eq!(describe_moved(&outcome.moved), None);
    }

    #[test]
    fn a_moved_target_is_named_without_changing_a_permitting_decision() {
        let facts = IntegrationFacts {
            target_revision: Ok(NEW_TARGET.to_string()),
            ..ready()
        };
        let outcome = decide(facts, Some(&prior()));
        assert!(matches!(outcome.decision, Decision::Merge(_)));
        assert_eq!(
            outcome.moved,
            vec![Moved::Target {
                from: OLD_TARGET.to_string(),
                to: NEW_TARGET.to_string(),
            }]
        );
        assert_eq!(
            describe_moved(&outcome.moved).unwrap(),
            format!("the target you checked moved from {OLD_TARGET} to {NEW_TARGET}")
        );
    }

    #[test]
    fn a_moved_target_is_named_beside_a_refusal_it_does_not_cause() {
        let facts = IntegrationFacts {
            ready: false,
            target_revision: Ok(NEW_TARGET.to_string()),
            ..ready()
        };
        let outcome = decide(facts, Some(&prior()));
        assert!(matches!(
            outcome.decision,
            Decision::Refused(Refusal::NotReady)
        ));
        assert!(matches!(outcome.moved.as_slice(), [Moved::Target { .. }]));
    }

    #[test]
    fn moved_gate_and_policy_declarations_are_named() {
        let mut changed_gates = gates();
        changed_gates.get_mut("build").unwrap().command = "make release".to_string();
        changed_gates.insert(
            "lint".to_string(),
            NormalizedGate {
                command: "make lint".to_string(),
                profiles: Vec::new(),
                timeout: None,
                declared_by: Vec::new(),
            },
        );
        let mut changed_policy = policy();
        changed_policy.forbid_self_approval = true;
        let facts = IntegrationFacts {
            declarations: Some(Declarations {
                gates: changed_gates,
                policy: changed_policy,
            }),
            ..ready()
        };
        let outcome = decide(facts, Some(&prior()));
        assert!(matches!(outcome.decision, Decision::Merge(_)));
        assert_eq!(
            outcome.moved,
            vec![
                Moved::Gates {
                    changed: vec!["build".to_string(), "lint".to_string()],
                },
                Moved::Policy,
            ]
        );
    }

    #[test]
    fn a_moved_head_and_a_withdrawn_approval_are_named() {
        let facts = IntegrationFacts {
            ready: false,
            approved: Some(Approved {
                patchset_id: "ps-2".to_string(),
                head: NEW_TARGET.to_string(),
            }),
            approval: Approval::default(),
            ..ready()
        };
        let outcome = decide(facts, Some(&prior()));
        assert_eq!(
            outcome.moved,
            vec![
                Moved::ApprovedHead {
                    from: format!("ps-1 ({HEAD})"),
                    to: format!("ps-2 ({NEW_TARGET})"),
                },
                Moved::Approval {
                    from: "verdict verdict-1".to_string(),
                    to: "nothing".to_string(),
                },
            ]
        );
    }

    #[test]
    fn an_unobserved_coordinate_is_not_reported_as_moved() {
        let facts = IntegrationFacts {
            target_revision: Err(anyhow!("target branch unreadable")),
            declarations: None,
            ..ready()
        };
        assert!(decide(facts, Some(&prior())).moved.is_empty());
    }
}
