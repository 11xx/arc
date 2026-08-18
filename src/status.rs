use crate::gates::GatesFile;
use crate::gitio;
use crate::model::{MessageSeverity, MessageType, ProbePhase, Verdict, VerifyResult};
use crate::policy::PolicyFile;
use crate::state::{self, ChangeState, ClaimIdentity, GitIdentity};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

pub const STATUS_SCHEMA: &str = "arc-status/7";
pub const BLOCKER_STATUS_SCHEMA: &str = "arc-blocker-status/1";
pub const SELF_APPROVAL_REASON: &str = "approval rejected by policy: self-approval";
/// Two identities arc assumed cannot establish that two people acted. The
/// self-approval guard compares effective authors, so an assumed identity on
/// both sides makes the comparison meaningless rather than passing.
pub const UNDECLARED_APPROVAL_REASON: &str =
    "approval rejected by policy: arc assumed the reviewing or the authoring identity from \
     git config, so independence is unproven (pass --actor or set ARC_ACTOR)";

/// Typed integration blockers, ordered by exit-code precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Blocker {
    Closed,
    BranchMissing,
    BlockedByChanges,
    NeedsRebase,
    BlockingFindings,
    NoValidApproval,
    GatesNotGreen,
    AcceptanceProbesNotGreen,
    HoldActive,
}

impl Blocker {
    pub fn exit_code(self) -> i32 {
        match self {
            Blocker::Closed | Blocker::BranchMissing => 6,
            Blocker::BlockedByChanges => 7,
            Blocker::NeedsRebase => 11,
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
            Blocker::BlockedByChanges => "blocked-by-changes",
            Blocker::NeedsRebase => "needs-rebase",
            Blocker::BlockingFindings => "blocking-findings",
            Blocker::NoValidApproval => "no-valid-approval",
            Blocker::GatesNotGreen => "gates-not-green",
            Blocker::AcceptanceProbesNotGreen => "acceptance-probes-not-green",
            Blocker::HoldActive => "hold-active",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencyChangeStatus {
    pub change_id: String,
    pub slug: String,
    pub status: String,
    pub integrated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockerStatus {
    pub schema: &'static str,
    pub blocked: bool,
    /// Status of every declared prerequisite. The historical field name is
    /// retained because orchestrators already consume it from the design spec.
    pub blockers_ready: Vec<DependencyChangeStatus>,
}

#[derive(Debug, Serialize)]
pub struct GateStatus {
    pub name: String,
    pub command: String,
    pub result: String,
    pub green_at_head: bool,
    /// The head evidence for this gate is attested (arc did not run it), so a
    /// lead can apply stricter judgment even though it counts for green-ness.
    pub attested: bool,
    /// The tree the command ran against, and whether it differed from the
    /// revision's. Absent is unknown, never a claim the tree was clean.
    /// Additive in `arc-status/6`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tested_tree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_dirty: Option<bool>,
    /// The worktree changed while the command ran, so this evidence describes
    /// no single tree.
    #[serde(skip_serializing_if = "is_false")]
    pub tree_moved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tail: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub timed_out: bool,
}

impl GateStatus {
    /// Why this gate is not green at head, in the caller's terms.
    ///
    /// A gate whose evidence passed but cannot be reused says so here; the
    /// bare result would read `pass` and contradict every readiness check.
    /// `None` means the gate is green.
    pub fn not_green_reason(&self) -> Option<&'static str> {
        if self.green_at_head {
            return None;
        }
        Some(match self.result.as_str() {
            "pending" => "no evidence at head",
            "fail" => "the gate failed",
            _ if self.tree_moved => "the worktree changed while the gate ran",
            _ if self.worktree_dirty == Some(true) => {
                "evidence recorded on a dirty worktree, so no checkout of this revision \
                 reproduces it"
            }
            // Parallel gates share one worktree, so no boundary comparison can
            // prove that a gate did not change a tracked file and restore it.
            // The tree was recorded; what is missing is whether it was clean.
            _ if self.tested_tree.is_some() => {
                "the worktree's cleanliness was not recorded, which is what a shared-worktree \
                 parallel run can never establish"
            }
            _ => "the tested tree was not recorded, so the evidence has no provenance",
        })
    }

    /// What actually clears this gate, given the state of the worktree now.
    ///
    /// Re-running a gate against a still-dirty tree records the same unusable
    /// evidence, so while the tree is dirty the only step that makes progress
    /// is cleaning it. Once it is clean the stale evidence can only be
    /// replaced by a rerun — the historical dirty flag on evidence already
    /// recorded is not something cleaning can change.
    pub fn clearing_action(&self, worktree_dirty: Option<bool>) -> String {
        if self.green_at_head {
            return "integrate".into();
        }
        // While the tree is dirty, no run can produce evidence that counts —
        // including a rerun of a gate that failed, and including a gate that
        // failed *because* of the uncommitted file. Cleaning is not claimed to
        // fix a failure; it is the precondition for any run whose result is
        // usable. It also cannot loop: this reads the live tree, so once the
        // tree is clean the advice becomes the rerun.
        if worktree_dirty == Some(true) {
            return format!("clean_worktree:{}", self.name);
        }
        format!("run_gate:{}", self.name)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeStatus {
    pub name: String,
    pub command: String,
    pub brief_version: usize,
    pub baseline_revision: String,
    pub baseline_result: String,
    pub baseline_attested: bool,
    pub final_revision: String,
    pub final_result: String,
    pub final_attested: bool,
    pub discriminating_at_head: bool,
    /// The brief's base is the head under review, so the probe would have to
    /// fail and pass at one revision. No run can discharge it; only a brief
    /// based on the revision the work started from can. Additive in
    /// `arc-status/6`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub undischargeable: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Serialize)]
pub struct FindingSummary {
    pub id: String,
    pub blocking: bool,
    pub severity: crate::model::Severity,
    pub summary: String,
    pub status: String,
    pub contested: bool,
    /// What the finding actually says, and where. A one-line summary is enough
    /// to count findings and not enough to act on one, which is the position a
    /// reader inheriting a change is in. Additive in `arc-status/6`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<crate::model::Anchor>,
    /// The patchset the finding was filed against. A finding predating the
    /// patchset under review is a different kind of fact from one filed
    /// against it, and the count alone cannot tell them apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patchset_id: Option<String>,
    pub reported_by: String,
}

#[derive(Debug, Serialize)]
pub struct HoldSummary {
    pub active: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BlockerSummary {
    pub open_findings: usize,
    pub blocking_findings: usize,
    pub gate_status: BTreeMap<String, String>,
    pub hold: HoldSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaimStatus {
    pub claim_id: String,
    pub owner: ClaimIdentity,
    pub active: bool,
    pub expired: bool,
    pub stale: bool,
    pub ttl_seconds: u64,
    pub claimed_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub stage: String,
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<crate::model::BlockerRef>,
    pub stage_started_at: DateTime<Utc>,
    pub age_seconds: u64,
    pub budget_seconds: Option<u64>,
    pub stage_budgets: BTreeMap<crate::model::StageBudget, u64>,
    pub snapshot_author: Option<GitIdentity>,
    pub snapshot_committer: Option<GitIdentity>,
    pub snapshot_claim_actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_mismatch: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct MessageStatus {
    pub event_id: String,
    pub message_type: MessageType,
    pub severity: MessageSeverity,
    pub summary: String,
    pub detail: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub actor: String,
    pub harness: Option<String>,
    pub session: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<&crate::state::MessageEntry> for MessageStatus {
    fn from(message: &crate::state::MessageEntry) -> Self {
        Self {
            event_id: message.event_id.clone(),
            message_type: message.message_type,
            severity: message.severity,
            summary: message.summary.clone(),
            detail: message.detail.clone(),
            metadata: message.metadata.clone(),
            actor: message.actor.clone(),
            harness: message.harness.clone(),
            session: message.session.clone(),
            created_at: message.created_at,
        }
    }
}

/// One reviewer identity's coverage of the change.
///
/// "Somebody reviewed this change" and "somebody reviewed what is about to
/// ship" are different claims, and only the second one is worth anything at
/// integration time. A panel can run correctly for ten rounds and still let
/// the final corrections ship unseen, which is why coverage is measured
/// against the final patchset rather than against participation.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewerCoverage {
    /// The effective author of this reviewer's verdicts and findings.
    pub reviewer: String,
    /// The newest patchset this reviewer filed a verdict or finding against.
    pub last_patchset: String,
    pub verdicts: usize,
    pub findings: usize,
    /// This reviewer saw the patchset that is about to ship.
    pub covers_final: bool,
    /// This reviewer is also the final patchset's effective author, so its
    /// verdict carries no independence.
    pub is_author: bool,
    /// The reviewer is indistinguishable from the snapshot actor because no
    /// `--on-behalf-of` was recorded on either side. Reported as unknown
    /// rather than as self-review, which it may or may not be.
    pub attribution_unknown: bool,
}

/// The versioned machine-readable contract agent harnesses program against.
/// Everything here is derivable from the ledger plus Git.
#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub schema: &'static str,
    pub change_id: String,
    pub slug: String,
    pub title: String,
    pub profile: String,
    pub state: String,
    pub target_branch: String,
    pub branch: String,
    pub base: String,
    pub worktree: Option<String>,
    pub opened_by: String,
    pub opened_harness: Option<String>,
    pub tags: Vec<String>,
    pub blocked_by: Vec<String>,
    pub assigned_to: Option<String>,
    pub priority: i32,
    pub messages: Vec<MessageStatus>,
    pub blocks: Vec<String>,
    pub blocker_status: BlockerStatus,
    pub claim: Option<ClaimStatus>,
    pub current_head: Option<String>,
    pub needs_rebase: bool,
    pub latest_patchset: Option<crate::state::Patchset>,
    pub brief: Option<BriefStatus>,
    pub head_matches_latest_patchset: bool,
    pub worktree_dirty: Option<bool>,
    pub verdict: Option<VerdictStatus>,
    /// Who reviewed what, newest coverage first. Additive in arc-status/6.
    pub review_map: Vec<ReviewerCoverage>,
    /// Warnings about review coverage of the final patchset. Advisory: plenty
    /// of changes legitimately ship with one reviewer, so these never block.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub coverage_warnings: Vec<String>,
    pub findings: Vec<FindingSummary>,
    pub open_blocking_findings: Vec<String>,
    pub hold: Option<String>,
    pub gates: Vec<GateStatus>,
    pub probes: Vec<ProbeStatus>,
    pub blocker_summary: BlockerSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_rejection_reason: Option<String>,
    pub next_action: String,
    pub ready_reason: String,
    pub ready_to_integrate: bool,
    /// Backward-compatible spelling retained from arc-status/1.
    pub integrate_ready: bool,
    pub blockers: Vec<Blocker>,
    pub closure: Option<crate::state::ClosureState>,
    /// A declared review obligation with no audit answering it. Additive in
    /// arc-status/6.
    pub audit_debt_outstanding: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_debt: Option<crate::state::AuditDebt>,
    /// Reviews recorded after integration, never mixed into `verdict`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub audit_verdicts: Vec<crate::state::AuditVerdictEntry>,
    /// Observed forge (hosted-PR) facts. Absent for changes with no forge
    /// events that are not on the `forge` profile. Additive in arc-status/5.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forge: Option<crate::forge::ForgeStatus>,
    #[serde(skip)]
    pub provenance_check_enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct BriefStatus {
    pub version: usize,
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub acceptance_probes: Vec<crate::model::AcceptanceProbe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_slice: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct VerdictStatus {
    pub verdict: Verdict,
    pub patchset_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    /// Whether arc invented the identity this verdict is attributed to rather
    /// than someone declaring it. Additive in `arc-status/6`.
    pub author_assumed: bool,
    pub valid_for_current_head: bool,
}

/// Whether an approval fails the self-approval guard: the same effective
/// author on both sides, or an identity arc assumed rather than one somebody
/// declared.
///
/// An assumed identity is one arc invented from `git config user.name`. Two of
/// those, or one of those beside a declared name, do not establish that two
/// people acted. Provenance recorded before arc kept it is *unknown*, not
/// assumed, and is compared by name as it always was — otherwise upgrading arc
/// would strand every existing ledger that uses this policy.
fn undeclared_or_self(
    patchset: &crate::state::Patchset,
    verdict_author: &str,
    verdict: &crate::state::VerdictEntry,
) -> bool {
    patchset.effective_author() == verdict_author
        || patchset.author_assumed()
        || verdict.author_assumed()
}

/// Build the status report: replayed ledger state joined with live Git
/// facts, dependency state, and the declared gate policy.
pub fn build(
    state: &ChangeState,
    cwd: &Path,
    gates: &GatesFile,
    policy: &PolicyFile,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
) -> Result<StatusReport> {
    build_at(
        state,
        cwd,
        gates,
        policy,
        dependency_status,
        blocks,
        Utc::now(),
    )
}

pub fn build_at(
    state: &ChangeState,
    cwd: &Path,
    gates: &GatesFile,
    policy: &PolicyFile,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
    now: DateTime<Utc>,
) -> Result<StatusReport> {
    let current_head = gitio::branch_head(cwd, &state.branch).ok();
    let target_head = gitio::branch_head(cwd, &state.target_branch).ok();
    let needs_rebase = match (&current_head, &target_head) {
        (Some(head), Some(target)) => gitio::merge_conflicts(cwd, target, head)?,
        _ => false,
    };
    let worktree_dirty = if state.is_closed() {
        None
    } else {
        state
            .worktree
            .as_deref()
            .map(Path::new)
            .filter(|worktree| worktree.exists())
            .map(|worktree| gitio::is_clean(worktree).map(|clean| !clean))
            .transpose()?
    };
    build_report(
        state,
        gates,
        policy,
        dependency_status,
        blocks,
        now,
        current_head,
        needs_rebase,
        worktree_dirty,
    )
}

/// Build a report from a state replayed to a past event. Live Git facts are
/// not consulted: the derived latest-patchset head is taken as the head that
/// was current then, and rebase state is not simulated. Used by `--at`.
pub fn build_as_of(
    state: &ChangeState,
    gates: &GatesFile,
    policy: &PolicyFile,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
    now: DateTime<Utc>,
) -> Result<StatusReport> {
    let current_head = state
        .latest_patchset()
        .map(|patchset| patchset.head.clone());
    build_report(
        state,
        gates,
        policy,
        dependency_status,
        blocks,
        now,
        current_head,
        false,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_report(
    state: &ChangeState,
    gates: &GatesFile,
    policy: &PolicyFile,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
    now: DateTime<Utc>,
    current_head: Option<String>,
    needs_rebase: bool,
    worktree_dirty: Option<bool>,
) -> Result<StatusReport> {
    let provenance_mode = policy.provenance.git_identity;
    let provenance_check_enabled = provenance_mode == crate::config::GitIdentityMode::PerActor;
    let mut latest_patchset = state.latest_patchset().cloned();
    if let Some(patchset) = &mut latest_patchset {
        patchset.provenance_mismatch = state::provenance_mismatch(
            provenance_mode,
            patchset.claim_actor.as_deref(),
            patchset.author.as_ref(),
            patchset.committer.as_ref(),
        );
    }
    let head_matches = match (&current_head, &latest_patchset) {
        (Some(h), Some(p)) => *h == p.head,
        _ => false,
    };

    let verdict = state.latest_verdict().map(|v| {
        let approved_patchset = latest_patchset
            .as_ref()
            .filter(|patchset| patchset.id == v.patchset_id);
        // Self-approval compares effective authors, so a lead snapshotting for
        // an executor and then approving as itself is not self-approval, while
        // approving --on-behalf-of that executor is.
        // A declared audit debt converts the self-approval refusal into a
        // recorded obligation. The requirement is not waived — it is carried
        // forward where a query can find it — which is the only way a
        // single-operator change can ship at all when no independent reviewer
        // is reachable.
        let rejected_self_approval = policy.policy.forbid_self_approval
            && !state.audit_debt_waives_current_head()
            && approved_patchset
                .is_some_and(|patchset| undeclared_or_self(patchset, v.effective_author(), v));
        let valid = v.verdict == Verdict::Approved
            && latest_patchset
                .as_ref()
                .map(|p| p.id == v.patchset_id)
                .unwrap_or(false)
            && head_matches
            && !rejected_self_approval;
        VerdictStatus {
            verdict: v.verdict,
            patchset_id: v.patchset_id.clone(),
            body: v.body.clone(),
            actor: v.actor.clone(),
            on_behalf_of: v.on_behalf_of.clone(),
            author_assumed: v.author_assumed(),
            valid_for_current_head: valid,
        }
    });
    let approval_rejection_reason = verdict.as_ref().and_then(|verdict| {
        (!verdict.valid_for_current_head
            && verdict.verdict == Verdict::Approved
            && latest_patchset.as_ref().is_some_and(|patchset| {
                let verdict_author = verdict
                    .on_behalf_of
                    .as_deref()
                    .unwrap_or(verdict.actor.as_str());
                policy.policy.forbid_self_approval
                    && !state.audit_debt_waives_current_head()
                    && patchset.id == verdict.patchset_id
                    && (patchset.effective_author() == verdict_author
                        || patchset.author_assumed()
                        || verdict.author_assumed)
            }))
        .then(|| {
            // Naming the same author is the more specific fact, so it is the
            // one reported when both are true.
            let same_author = latest_patchset.as_ref().is_some_and(|patchset| {
                patchset.effective_author()
                    == verdict
                        .on_behalf_of
                        .as_deref()
                        .unwrap_or(verdict.actor.as_str())
            });
            if same_author {
                SELF_APPROVAL_REASON.to_string()
            } else {
                UNDECLARED_APPROVAL_REASON.to_string()
            }
        })
    });

    let review_map = reviewer_coverage(state);
    let coverage_warnings = coverage_warnings(&review_map, latest_patchset.as_ref());

    let findings: Vec<FindingSummary> = state
        .findings
        .values()
        .map(|f| FindingSummary {
            id: f.id.clone(),
            blocking: f.blocking,
            severity: f.severity,
            summary: f.summary.clone(),
            status: f
                .effective_status()
                .map(|s| format!("{s:?}").to_lowercase())
                .unwrap_or_else(|| {
                    if f.contested() {
                        "contested".into()
                    } else {
                        "open".into()
                    }
                }),
            contested: f.contested(),
            body: f.body.clone(),
            anchor: f.anchor.clone(),
            patchset_id: f.patchset_id.clone(),
            reported_by: f.reported_by.clone(),
        })
        .collect();

    let open_blocking: Vec<String> = state
        .open_blocking_findings()
        .iter()
        .map(|f| f.id.clone())
        .collect();

    let gate_statuses: Vec<GateStatus> = gates
        .required_for(&state.profile)
        .into_iter()
        .map(|(name, gate)| {
            let evidence = current_head
                .as_deref()
                .and_then(|head| state.gate_evidence_at(name, head));
            let result = evidence.map(|e| e.result);
            GateStatus {
                name: name.clone(),
                command: gate.command.clone(),
                result: match result {
                    Some(crate::model::VerifyResult::Pass) => "pass",
                    Some(crate::model::VerifyResult::Fail) => "fail",
                    None => "pending",
                }
                .into(),
                // Evidence produced on a dirty tree describes something no
                // checkout of this revision reproduces, and evidence whose
                // tree moved mid-run describes no single tree at all. Both
                // are displayed and neither counts as green — throwing them
                // away would push loops toward not recording at all, which is
                // worse than recording them honestly.
                green_at_head: evidence.is_some_and(|e| e.green_at_head()),
                attested: evidence.is_some_and(|e| e.attested),
                tested_tree: evidence.and_then(|e| e.tested_tree.clone()),
                worktree_dirty: evidence.and_then(|e| e.worktree_dirty),
                tree_moved: evidence.is_some_and(|e| e.tree_moved),
                evidence_event_id: evidence.map(|e| e.event_id.clone()),
                revision: evidence.map(|e| e.revision.clone()),
                hostname: evidence.map(|e| e.hostname.clone()),
                runner: evidence.and_then(|e| e.runner.clone()),
                output_tail: evidence
                    .filter(|e| e.result == crate::model::VerifyResult::Fail)
                    .and_then(|e| e.output_tail.clone()),
                timed_out: evidence
                    .is_some_and(|e| e.result == crate::model::VerifyResult::Fail && e.timed_out),
            }
        })
        .collect();
    let probe_statuses = latest_patchset
        .as_ref()
        .and_then(|patchset| {
            let brief_ref = patchset.brief_ref.as_ref()?;
            let brief_version = patchset.brief_version?;
            let brief = state
                .briefs
                .iter()
                .find(|brief| brief.event_id == brief_ref.event_id)?;
            let baseline_revision = brief.base_revision.as_deref().unwrap_or("");
            Some(
                brief
                    .acceptance_probes
                    .iter()
                    .map(|probe| {
                        let evidence = |phase, revision: &str| {
                            state.verifications.iter().rev().find(|entry| {
                                entry.revision == revision
                                    && entry.probe.as_ref().is_some_and(|evidence| {
                                        evidence.brief_event_id == brief.event_id
                                            && evidence.name == probe.name
                                            && evidence.phase == phase
                                    })
                            })
                        };
                        let baseline = evidence(ProbePhase::Baseline, baseline_revision);
                        let final_evidence = evidence(ProbePhase::Final, &patchset.head);
                        // Either the baseline and the head are one revision,
                        // or the brief predates base revisions and there is no
                        // revision to fail at — `verify --probe-phase baseline`
                        // refuses that outright.
                        let undischargeable =
                            baseline_revision.is_empty() || baseline_revision == patchset.head;
                        ProbeStatus {
                            name: probe.name.clone(),
                            command: probe.command.clone(),
                            brief_version,
                            baseline_revision: baseline_revision.to_owned(),
                            baseline_result: verification_result_label(
                                baseline.map(|entry| entry.result),
                            ),
                            baseline_attested: baseline.is_some_and(|entry| entry.attested),
                            final_revision: patchset.head.clone(),
                            final_result: verification_result_label(
                                final_evidence.map(|entry| entry.result),
                            ),
                            final_attested: final_evidence.is_some_and(|entry| entry.attested),
                            // Fail and Pass recorded at one revision is
                            // contradictory evidence, not a discharged probe,
                            // so an undischargeable probe is never green.
                            discriminating_at_head: !undischargeable
                                && baseline.is_some_and(|entry| entry.result == VerifyResult::Fail)
                                && final_evidence
                                    .is_some_and(|entry| entry.result == VerifyResult::Pass),
                            undischargeable,
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();

    let mut blockers = Vec::new();
    if state.is_closed() {
        blockers.push(Blocker::Closed);
    }
    if current_head.is_none() {
        blockers.push(Blocker::BranchMissing);
    }
    if dependency_status.blocked {
        blockers.push(Blocker::BlockedByChanges);
    }
    if needs_rebase {
        blockers.push(Blocker::NeedsRebase);
    }
    if !open_blocking.is_empty() {
        blockers.push(Blocker::BlockingFindings);
    }
    if !verdict
        .as_ref()
        .map(|v| v.valid_for_current_head)
        .unwrap_or(false)
    {
        blockers.push(Blocker::NoValidApproval);
    }
    if gate_statuses.iter().any(|g| !g.green_at_head) {
        blockers.push(Blocker::GatesNotGreen);
    }
    if probe_statuses
        .iter()
        .any(|probe| !probe.discriminating_at_head)
    {
        blockers.push(Blocker::AcceptanceProbesNotGreen);
    }
    if state.hold.is_some() {
        blockers.push(Blocker::HoldActive);
    }

    let gate_summary = gate_statuses
        .iter()
        .map(|gate| (gate.name.clone(), gate.result.clone()))
        .collect();
    let open_findings = findings
        .iter()
        .filter(|finding| {
            !matches!(
                finding.status.as_str(),
                "resolved" | "acceptedrisk" | "obsolete"
            )
        })
        .count();
    let blocker_summary = BlockerSummary {
        open_findings,
        blocking_findings: open_blocking.len(),
        gate_status: gate_summary,
        hold: HoldSummary {
            active: state.hold.is_some(),
            reason: state.hold.clone(),
        },
        approval_reason: approval_rejection_reason.clone(),
    };

    let next_action = if state.is_closed() {
        "none:closed".into()
    } else if current_head.is_none() {
        "restore_branch".into()
    } else if dependency_status
        .blockers_ready
        .iter()
        .any(|dependency| dependency.status == "wedged")
    {
        "repair_blockers:metadata".into()
    } else if dependency_status.blocked {
        "wait_for:blockers".into()
    } else if needs_rebase {
        "rebase".into()
    } else if !head_matches {
        "snapshot".into()
    } else if !open_blocking.is_empty() {
        "resolve_findings".into()
    } else if state.hold.is_some() {
        "release_hold".into()
    } else if let Some(gate) = gate_statuses.iter().find(|gate| !gate.green_at_head) {
        gate.clearing_action(worktree_dirty)
    } else if let Some(probe) = probe_statuses
        .iter()
        .find(|probe| !probe.discriminating_at_head)
    {
        format!("run_probe:{}", probe.name)
    } else if let Some(reason) = approval_rejection_reason.as_ref() {
        reason.clone()
    } else if !verdict
        .as_ref()
        .map(|v| v.valid_for_current_head)
        .unwrap_or(false)
    {
        "request_review".into()
    } else {
        "integrate".into()
    };

    let forge = crate::forge::build_status(
        &state.forge,
        &state.profile,
        latest_patchset
            .as_ref()
            .map(|patchset| patchset.head.as_str()),
        state.hold.is_some(),
    );

    let ready = blockers.is_empty();
    let ready_reason = if ready {
        "all integration gates pass".into()
    } else {
        blockers
            .iter()
            .map(|blocker| blocker.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };

    Ok(StatusReport {
        schema: STATUS_SCHEMA,
        change_id: state.change_id.clone(),
        slug: state.slug.clone(),
        title: state.title.clone(),
        profile: state.profile.clone(),
        state: if state.is_closed() {
            "closed".into()
        } else {
            "open".into()
        },
        target_branch: state.target_branch.clone(),
        branch: state.branch.clone(),
        base: state.base.clone(),
        worktree: state.worktree.clone(),
        opened_by: state.opened_by.clone(),
        opened_harness: state.opened_harness.clone(),
        tags: state.tags.clone(),
        blocked_by: state.blocked_by.clone(),
        assigned_to: state.assigned_to.clone(),
        priority: state.priority,
        messages: state.messages.iter().map(MessageStatus::from).collect(),
        blocks,
        blocker_status: dependency_status,
        claim: claim_status_at(state, now, provenance_mode),
        current_head,
        needs_rebase,
        latest_patchset,
        brief: state.latest_brief().map(|brief| BriefStatus {
            version: state.briefs.len(),
            title: brief.title.clone(),
            base_revision: brief.base_revision.clone(),
            acceptance_probes: brief.acceptance_probes.clone(),
            plan_ref: brief.plan_ref.clone(),
            plan_slice: brief.plan_slice.clone(),
            recorded_at: brief.ts,
        }),
        head_matches_latest_patchset: head_matches,
        worktree_dirty,
        verdict,
        review_map,
        coverage_warnings,
        findings,
        open_blocking_findings: open_blocking,
        hold: state.hold.clone(),
        gates: gate_statuses,
        probes: probe_statuses,
        blocker_summary,
        approval_rejection_reason,
        next_action,
        ready_reason,
        ready_to_integrate: ready,
        integrate_ready: ready,
        blockers,
        closure: state.closure.clone(),
        audit_debt_outstanding: state.audit_debt_outstanding(),
        audit_debt: state.audit_debt.clone(),
        audit_verdicts: state.audit_verdicts.clone(),
        forge,
        provenance_check_enabled,
    })
}

pub fn claim_status_at(
    state: &ChangeState,
    now: DateTime<Utc>,
    provenance_mode: crate::config::GitIdentityMode,
) -> Option<ClaimStatus> {
    let claim = state.claim.as_ref()?;
    let timing = state::claim_timing_at(claim, now);
    let snapshot = state.latest_patchset().filter(|patchset| {
        timing.stage == "snapshotted"
            && patchset.claim_id.as_deref() == Some(claim.claim_id.as_str())
    });
    Some(ClaimStatus {
        claim_id: claim.claim_id.clone(),
        owner: claim.owner.clone(),
        active: timing.active,
        expired: timing.expired,
        stale: timing.stale,
        ttl_seconds: claim.ttl_seconds,
        claimed_at: claim.claimed_at,
        last_activity_at: claim.last_activity_at,
        expires_at: timing.expires_at,
        stage: timing.stage,
        note: claim
            .progress
            .as_ref()
            .and_then(|progress| progress.note.clone()),
        blocker: claim
            .progress
            .as_ref()
            .and_then(|progress| progress.blocker.clone()),
        stage_started_at: timing.stage_started_at,
        age_seconds: timing.age_seconds,
        budget_seconds: timing.budget_seconds,
        stage_budgets: claim.stage_budgets.clone(),
        snapshot_author: snapshot.and_then(|patchset| patchset.author.clone()),
        snapshot_committer: snapshot.and_then(|patchset| patchset.committer.clone()),
        snapshot_claim_actor: snapshot.and_then(|patchset| patchset.claim_actor.clone()),
        provenance_mismatch: snapshot.and_then(|patchset| {
            state::provenance_mismatch(
                provenance_mode,
                patchset.claim_actor.as_deref(),
                patchset.author.as_ref(),
                patchset.committer.as_ref(),
            )
        }),
    })
}

/// Exit code for `arc check`: 0 when integrate-ready, else the code of
/// the highest-precedence blocker.
pub fn check_exit_code(report: &StatusReport) -> i32 {
    if report.integrate_ready {
        return 0;
    }
    for blocker in [
        Blocker::Closed,
        Blocker::BranchMissing,
        Blocker::BlockedByChanges,
        Blocker::NeedsRebase,
        Blocker::BlockingFindings,
        Blocker::NoValidApproval,
        Blocker::GatesNotGreen,
        Blocker::AcceptanceProbesNotGreen,
        Blocker::HoldActive,
    ] {
        if report.blockers.contains(&blocker) {
            return blocker.exit_code();
        }
    }
    6
}

fn verification_result_label(result: Option<VerifyResult>) -> String {
    match result {
        Some(VerifyResult::Pass) => "pass",
        Some(VerifyResult::Fail) => "fail",
        None => "pending",
    }
    .into()
}

/// Walk every verdict and finding, grouping by the identity policy attributes
/// them to, and record the newest patchset each one saw.
///
/// Ordering is by patchset position in the change's own snapshot sequence, not
/// by timestamp: a verdict filed late against an old patchset reviewed the old
/// patchset. Reviewers with no patchset attribution at all are omitted rather
/// than credited with covering something.
pub fn reviewer_coverage(state: &ChangeState) -> Vec<ReviewerCoverage> {
    let position = |patchset_id: &str| {
        state
            .patchsets
            .iter()
            .position(|patchset| patchset.id == patchset_id)
    };
    let final_patchset = state.latest_patchset();

    struct Tally {
        best: usize,
        best_id: String,
        verdicts: usize,
        findings: usize,
        attributed: bool,
    }
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let mut record = |reviewer: &str, patchset_id: &str, attributed: bool, is_verdict: bool| {
        let Some(index) = position(patchset_id) else {
            return;
        };
        let entry = tallies.entry(reviewer.to_string()).or_insert(Tally {
            best: index,
            best_id: patchset_id.to_string(),
            verdicts: 0,
            findings: 0,
            attributed,
        });
        if index >= entry.best {
            entry.best = index;
            entry.best_id = patchset_id.to_string();
        }
        // Any explicit attribution anywhere makes this identity legible.
        entry.attributed |= attributed;
        if is_verdict {
            entry.verdicts += 1;
        } else {
            entry.findings += 1;
        }
    };

    for verdict in &state.verdicts {
        record(
            verdict.effective_author(),
            &verdict.patchset_id,
            verdict.on_behalf_of.is_some(),
            true,
        );
    }
    for finding in state.findings.values() {
        if let Some(patchset_id) = finding.patchset_id.as_deref() {
            record(
                finding.effective_author(),
                patchset_id,
                finding.on_behalf_of.is_some(),
                false,
            );
        }
    }

    let mut rows: Vec<ReviewerCoverage> = tallies
        .into_iter()
        .map(|(reviewer, tally)| {
            let is_author =
                final_patchset.is_some_and(|patchset| patchset.effective_author() == reviewer);
            ReviewerCoverage {
                covers_final: final_patchset.is_some_and(|patchset| patchset.id == tally.best_id),
                // Indistinguishable rather than independent: the identity
                // matches the snapshot's and neither side declared a subject.
                attribution_unknown: is_author
                    && !tally.attributed
                    && final_patchset.is_some_and(|patchset| patchset.on_behalf_of.is_none()),
                is_author,
                reviewer,
                last_patchset: tally.best_id,
                verdicts: tally.verdicts,
                findings: tally.findings,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.covers_final
            .cmp(&a.covers_final)
            .then_with(|| a.reviewer.cmp(&b.reviewer))
    });
    rows
}

/// Advisory coverage warnings for `arc check`. Never blockers: a change with
/// one reviewer is normal, and refusing it would make the tool unusable for
/// the single-operator case it is most often run in.
pub fn coverage_warnings(
    review_map: &[ReviewerCoverage],
    final_patchset: Option<&crate::state::Patchset>,
) -> Vec<String> {
    let Some(final_patchset) = final_patchset else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for row in review_map {
        if !row.covers_final {
            warnings.push(format!(
                "{} last saw {}; integrating {}",
                row.reviewer, row.last_patchset, final_patchset.id
            ));
        }
    }
    let independent = review_map
        .iter()
        .any(|row| row.covers_final && !row.is_author && !row.attribution_unknown);
    if !independent {
        let unknown = review_map
            .iter()
            .any(|row| row.covers_final && row.attribution_unknown);
        warnings.push(if unknown {
            format!(
                "no reviewer of {} is distinguishable from its author; \
                 record --on-behalf-of to make attribution legible",
                final_patchset.id
            )
        } else {
            format!("no independent reviewer covers {}", final_patchset.id)
        });
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codes 8, 9 and 10 are refusals that any command can raise — claim or
    /// stage conflict, execution-role refusal, and forge-link refusal — so a
    /// caller reads them the same way whichever verb produced them. Blocker
    /// codes are `check`/`integrate` verdicts and must not shadow those, or a
    /// caller branching on `check` misreads a documented meaning. Codes that
    /// are genuinely per-command, such as `watch`'s timeout, are outside this
    /// rule and deliberately not listed.
    const CROSS_COMMAND_REFUSALS: [i32; 3] = [8, 9, 10];

    #[test]
    fn blocker_exit_codes_are_distinct_and_avoid_cross_command_refusals() {
        let blockers = [
            Blocker::Closed,
            Blocker::BranchMissing,
            Blocker::BlockedByChanges,
            Blocker::NeedsRebase,
            Blocker::BlockingFindings,
            Blocker::NoValidApproval,
            Blocker::GatesNotGreen,
            Blocker::AcceptanceProbesNotGreen,
            Blocker::HoldActive,
        ];
        let mut seen: BTreeMap<i32, Vec<&'static str>> = BTreeMap::new();
        for blocker in blockers {
            seen.entry(blocker.exit_code())
                .or_default()
                .push(blocker.as_str());
        }
        // Closed and BranchMissing deliberately share 6: both mean the change
        // is not in a workable state and a caller acts identically on them.
        for (code, names) in &seen {
            assert!(
                names.len() == 1 || *code == 6,
                "exit {code} is shared by {names:?}; only 6 may be shared"
            );
            assert!(
                !CROSS_COMMAND_REFUSALS.contains(code),
                "blocker {names:?} took exit {code}, which is a cross-command refusal"
            );
        }
        assert_eq!(
            Blocker::AcceptanceProbesNotGreen.exit_code(),
            12,
            "the probe blocker must keep its own code"
        );
    }
}
