use crate::gates::GatesFile;
use crate::gitio;
use crate::model::Verdict;
use crate::state::ChangeState;
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

pub const STATUS_SCHEMA: &str = "arc-status/1";

/// Typed integration blockers, ordered by exit-code precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Blocker {
    Closed,
    BlockingFindings,
    NoValidApproval,
    GatesNotGreen,
    HoldActive,
    BranchMissing,
}

impl Blocker {
    pub fn exit_code(self) -> i32 {
        match self {
            Blocker::Closed => 6,
            Blocker::BlockingFindings => 2,
            Blocker::NoValidApproval => 3,
            Blocker::GatesNotGreen => 5,
            Blocker::HoldActive => 4,
            Blocker::BranchMissing => 6,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct GateStatus {
    pub name: String,
    pub command: String,
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
    pub current_head: Option<String>,
    pub latest_patchset: Option<crate::state::Patchset>,
    pub head_matches_latest_patchset: bool,
    pub verdict: Option<VerdictStatus>,
    pub findings: Vec<FindingSummary>,
    pub open_blocking_findings: Vec<String>,
    pub hold: Option<String>,
    pub gates: Vec<GateStatus>,
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
/// facts (branch head) and the declared gate policy.
pub fn build(state: &ChangeState, cwd: &Path, gates: &GatesFile) -> Result<StatusReport> {
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
        .map(|(name, gate)| GateStatus {
            name: name.clone(),
            command: gate.command.clone(),
            green_at_head: current_head
                .as_deref()
                .map(|h| state.gate_passed_at(name, h))
                .unwrap_or(false),
        })
        .collect();

    let mut blockers = Vec::new();
    if state.is_closed() {
        blockers.push(Blocker::Closed);
    }
    if current_head.is_none() {
        blockers.push(Blocker::BranchMissing);
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
        current_head,
        latest_patchset,
        head_matches_latest_patchset: head_matches,
        verdict,
        findings,
        open_blocking_findings: open_blocking,
        hold: state.hold.clone(),
        gates: gate_statuses,
        integrate_ready: blockers.is_empty(),
        blockers,
        closure: state.closure.clone(),
    })
}

/// Exit code for `arc check`: 0 when integrate-ready, else the code of
/// the highest-precedence blocker.
pub fn check_exit_code(report: &StatusReport) -> i32 {
    if report.integrate_ready {
        return 0;
    }
    for b in [
        Blocker::Closed,
        Blocker::BranchMissing,
        Blocker::BlockingFindings,
        Blocker::NoValidApproval,
        Blocker::GatesNotGreen,
        Blocker::HoldActive,
    ] {
        if report.blockers.contains(&b) {
            return b.exit_code();
        }
    }
    6
}
