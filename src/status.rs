use crate::gates::GatesFile;
use crate::gitio;
use crate::model::Verdict;
use crate::state::{self, ChangeState, ClaimIdentity, GitIdentity};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

pub const STATUS_SCHEMA: &str = "arc-status/3";
pub const BLOCKER_STATUS_SCHEMA: &str = "arc-blocker-status/1";

/// Typed integration blockers, ordered by exit-code precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Blocker {
    Closed,
    BranchMissing,
    BlockedByChanges,
    BlockingFindings,
    NoValidApproval,
    GatesNotGreen,
    HoldActive,
}

impl Blocker {
    pub fn exit_code(self) -> i32 {
        match self {
            Blocker::Closed | Blocker::BranchMissing => 6,
            Blocker::BlockedByChanges => 7,
            Blocker::BlockingFindings => 2,
            Blocker::NoValidApproval => 3,
            Blocker::GatesNotGreen => 5,
            Blocker::HoldActive => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Blocker::Closed => "closed",
            Blocker::BranchMissing => "branch-missing",
            Blocker::BlockedByChanges => "blocked-by-changes",
            Blocker::BlockingFindings => "blocking-findings",
            Blocker::NoValidApproval => "no-valid-approval",
            Blocker::GatesNotGreen => "gates-not-green",
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
}

#[derive(Debug, Serialize)]
pub struct FindingSummary {
    pub id: String,
    pub blocking: bool,
    pub severity: crate::model::Severity,
    pub summary: String,
    pub status: String,
    pub contested: bool,
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
    pub stage_started_at: DateTime<Utc>,
    pub age_seconds: u64,
    pub budget_seconds: Option<u64>,
    pub stage_budgets: BTreeMap<crate::model::StageBudget, u64>,
    pub snapshot_author: Option<GitIdentity>,
    pub snapshot_committer: Option<GitIdentity>,
    pub snapshot_claim_actor: Option<String>,
    pub provenance_mismatch: Option<bool>,
}

/// The versioned machine-readable contract the /arc skill programs
/// against. Everything here is derivable from the ledger plus Git.
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
    pub blocks: Vec<String>,
    pub blocker_status: BlockerStatus,
    pub claim: Option<ClaimStatus>,
    pub current_head: Option<String>,
    pub latest_patchset: Option<crate::state::Patchset>,
    pub head_matches_latest_patchset: bool,
    pub verdict: Option<VerdictStatus>,
    pub findings: Vec<FindingSummary>,
    pub open_blocking_findings: Vec<String>,
    pub hold: Option<String>,
    pub gates: Vec<GateStatus>,
    pub blocker_summary: BlockerSummary,
    pub next_action: String,
    pub ready_reason: String,
    pub ready_to_integrate: bool,
    /// Backward-compatible spelling retained from arc-status/1.
    pub integrate_ready: bool,
    pub blockers: Vec<Blocker>,
    pub closure: Option<crate::state::ClosureState>,
}

#[derive(Debug, Serialize)]
pub struct VerdictStatus {
    pub verdict: Verdict,
    pub patchset_id: String,
    pub actor: String,
    pub valid_for_current_head: bool,
}

/// Build the status report: replayed ledger state joined with live Git
/// facts, dependency state, and the declared gate policy.
pub fn build(
    state: &ChangeState,
    cwd: &Path,
    gates: &GatesFile,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
) -> Result<StatusReport> {
    build_at(state, cwd, gates, dependency_status, blocks, Utc::now())
}

pub fn build_at(
    state: &ChangeState,
    cwd: &Path,
    gates: &GatesFile,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
    now: DateTime<Utc>,
) -> Result<StatusReport> {
    let current_head = gitio::branch_head(cwd, &state.branch).ok();

    let latest_patchset = state.latest_patchset().cloned();
    let head_matches = match (&current_head, &latest_patchset) {
        (Some(h), Some(p)) => *h == p.head,
        _ => false,
    };

    let verdict = state.latest_verdict().map(|v| {
        let valid = v.verdict == Verdict::Approved
            && latest_patchset
                .as_ref()
                .map(|p| p.id == v.patchset_id)
                .unwrap_or(false)
            && head_matches;
        VerdictStatus {
            verdict: v.verdict,
            patchset_id: v.patchset_id.clone(),
            actor: v.actor.clone(),
            valid_for_current_head: valid,
        }
    });

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
            let result = current_head
                .as_deref()
                .and_then(|head| state.gate_result_at(name, head));
            GateStatus {
                name: name.clone(),
                command: gate.command.clone(),
                result: match result {
                    Some(crate::model::VerifyResult::Pass) => "pass",
                    Some(crate::model::VerifyResult::Fail) => "fail",
                    None => "pending",
                }
                .into(),
                green_at_head: result == Some(crate::model::VerifyResult::Pass),
            }
        })
        .collect();

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
    } else if !head_matches {
        "snapshot".into()
    } else if !open_blocking.is_empty() {
        "resolve_findings".into()
    } else if state.hold.is_some() {
        "release_hold".into()
    } else if let Some(gate) = gate_statuses.iter().find(|gate| !gate.green_at_head) {
        format!("run_gate:{}", gate.name)
    } else if !verdict
        .as_ref()
        .map(|v| v.valid_for_current_head)
        .unwrap_or(false)
    {
        "request_review".into()
    } else {
        "integrate".into()
    };

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
        blocks,
        blocker_status: dependency_status,
        claim: claim_status_at(state, now),
        current_head,
        latest_patchset,
        head_matches_latest_patchset: head_matches,
        verdict,
        findings,
        open_blocking_findings: open_blocking,
        hold: state.hold.clone(),
        gates: gate_statuses,
        blocker_summary,
        next_action,
        ready_reason,
        ready_to_integrate: ready,
        integrate_ready: ready,
        blockers,
        closure: state.closure.clone(),
    })
}

pub fn claim_status_at(state: &ChangeState, now: DateTime<Utc>) -> Option<ClaimStatus> {
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
        stage_started_at: timing.stage_started_at,
        age_seconds: timing.age_seconds,
        budget_seconds: timing.budget_seconds,
        stage_budgets: claim.stage_budgets.clone(),
        snapshot_author: snapshot.and_then(|patchset| patchset.author.clone()),
        snapshot_committer: snapshot.and_then(|patchset| patchset.committer.clone()),
        snapshot_claim_actor: snapshot.and_then(|patchset| patchset.claim_actor.clone()),
        provenance_mismatch: snapshot.and_then(|patchset| patchset.provenance_mismatch),
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
        Blocker::BlockingFindings,
        Blocker::NoValidApproval,
        Blocker::GatesNotGreen,
        Blocker::HoldActive,
    ] {
        if report.blockers.contains(&blocker) {
            return blocker.exit_code();
        }
    }
    6
}
