use crate::model::Verdict;
use crate::state::{ChangeState, ClaimIdentity};
use crate::status::{ClaimStatus, StatusReport};
use serde::Serialize;

pub const INBOX_SCHEMA: &str = "arc-inbox/3";

/// The journal's actionable backlog, carried beside the ledger buckets.
///
/// An inbox reporting only ledger state answers "what is in flight" and
/// silently implies nothing else is waiting, when the queue a session should
/// actually pull from may live entirely in the journal. The rollup names the
/// tiers and previews the primary one; `arc journal open` renders it in full.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JournalBacklog {
    pub dir: String,
    pub open: usize,
    pub later: usize,
    #[serde(rename = "feature-requests")]
    pub feature_requests: usize,
    /// Newest primary-tier items, as `(file, kind, heading)`.
    pub preview: Vec<JournalBacklogRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct JournalBacklogRow {
    pub file: String,
    pub kind: String,
    pub heading: String,
}

/// One change's line within an inbox bucket.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InboxRow {
    pub change_id: String,
    pub title: String,
    pub priority: i32,
    /// Who should act next on this change while it sits in this bucket.
    pub next_actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    /// The active claim owner when this is a claim-backed row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<ClaimIdentity>,
    /// The active claim stage when this is a claim-backed row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// Seconds elapsed in the active claim stage when this is a claim-backed row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
}

/// The `arc-inbox/3` rollup: a lead-facing queue derived entirely from
/// existing ledger + Git state. A change may appear in more than one bucket
/// when it is genuinely in more than one actionable state (e.g. blocked and
/// awaiting review); each bucket is computed independently.
#[derive(Debug, Clone, Serialize)]
pub struct Inbox {
    pub schema: &'static str,
    /// The `--assigned-to` filter applied, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    #[serde(rename = "needs-review")]
    pub needs_review: Vec<InboxRow>,
    #[serde(rename = "changes-requested")]
    pub changes_requested: Vec<InboxRow>,
    #[serde(rename = "ready-to-integrate")]
    pub ready_to_integrate: Vec<InboxRow>,
    pub blocked: Vec<InboxRow>,
    pub held: Vec<InboxRow>,
    #[serde(rename = "in-progress")]
    pub in_progress: Vec<InboxRow>,
    pub stalled: Vec<InboxRow>,
    /// Changes owing a review nobody has recorded. The only bucket holding
    /// integrated changes: the obligation outlives the change, and a queue
    /// that dropped it at closure would lose exactly the work it exists to
    /// track.
    #[serde(rename = "audit-owed")]
    pub audit_owed: Vec<InboxRow>,
    /// Absent when the journal directory could not be resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<JournalBacklog>,
}

impl Inbox {
    pub fn new(assigned_to: Option<String>) -> Self {
        Inbox {
            schema: INBOX_SCHEMA,
            assigned_to,
            journal: None,
            needs_review: Vec::new(),
            changes_requested: Vec::new(),
            ready_to_integrate: Vec::new(),
            blocked: Vec::new(),
            held: Vec::new(),
            in_progress: Vec::new(),
            stalled: Vec::new(),
            audit_owed: Vec::new(),
        }
    }

    /// Bucket names paired with their rows, in rendering order.
    pub fn sections(&self) -> [(&'static str, &Vec<InboxRow>); 8] {
        [
            ("needs-review", &self.needs_review),
            ("changes-requested", &self.changes_requested),
            ("ready-to-integrate", &self.ready_to_integrate),
            ("blocked", &self.blocked),
            ("held", &self.held),
            ("in-progress", &self.in_progress),
            ("stalled", &self.stalled),
            ("audit-owed", &self.audit_owed),
        ]
    }

    /// Classify one open change into every bucket it qualifies for. The
    /// report carries the reused readiness/blocker/claim derivations so no
    /// policy is reimplemented here.
    pub fn absorb(&mut self, state: &ChangeState, report: &StatusReport) {
        let row = |next_actor: &str| InboxRow {
            change_id: state.change_id.clone(),
            title: state.title.clone(),
            priority: state.priority,
            next_actor: next_actor.to_string(),
            assigned_to: state.assigned_to.clone(),
            owner: None,
            stage: None,
            age_seconds: None,
        };
        let claim_row = |claim: &ClaimStatus| {
            let next_actor = if claim.owner.harness.is_empty() {
                "implementer"
            } else {
                &claim.owner.harness
            };
            InboxRow {
                change_id: state.change_id.clone(),
                title: state.title.clone(),
                priority: state.priority,
                next_actor: next_actor.to_string(),
                assigned_to: state.assigned_to.clone(),
                owner: Some(claim.owner.clone()),
                stage: Some(claim.stage.clone()),
                age_seconds: Some(claim.age_seconds),
            }
        };

        if !report.holds.is_empty() {
            self.held.push(row("lead"));
        }
        if report.blocker_status.blocked {
            self.blocked.push(row("wait"));
        }
        if let Some(claim) = report.claim.as_ref().filter(|claim| claim.active) {
            if claim.stale {
                self.stalled.push(claim_row(claim));
            } else {
                self.in_progress.push(claim_row(claim));
            }
        }
        if needs_review(state) {
            let actor = if state.latest_patchset().is_none() {
                "implementer"
            } else {
                "reviewer"
            };
            self.needs_review.push(row(actor));
        }
        if changes_requested(state) {
            self.changes_requested.push(row("implementer"));
        }
        if report.ready_to_integrate {
            self.ready_to_integrate.push(row("lead"));
        }
    }

    /// Order every queue bucket by descending scheduling priority.
    pub fn sort_by_priority(&mut self) {
        self.needs_review
            .sort_by_key(|row| std::cmp::Reverse(row.priority));
        self.changes_requested
            .sort_by_key(|row| std::cmp::Reverse(row.priority));
        self.ready_to_integrate
            .sort_by_key(|row| std::cmp::Reverse(row.priority));
        self.blocked
            .sort_by_key(|row| std::cmp::Reverse(row.priority));
        self.held.sort_by_key(|row| std::cmp::Reverse(row.priority));
        self.in_progress
            .sort_by_key(|row| std::cmp::Reverse(row.priority));
        self.stalled
            .sort_by_key(|row| std::cmp::Reverse(row.priority));
        self.audit_owed
            .sort_by_key(|row| std::cmp::Reverse(row.priority));
    }

    /// Record a change that owes a review. Called for closed changes too, so
    /// it stands apart from `absorb`, which only sees open work.
    pub fn absorb_audit_debt(&mut self, state: &ChangeState) {
        if !state.audit_debt_outstanding() {
            return;
        }
        self.audit_owed.push(InboxRow {
            change_id: state.change_id.clone(),
            title: state.title.clone(),
            priority: state.priority,
            next_actor: "reviewer".to_string(),
            assigned_to: state.assigned_to.clone(),
            owner: None,
            stage: None,
            age_seconds: None,
        });
    }
}

/// A new patchset has landed since the last verdict (or the change was never
/// reviewed): the reviewer — or the implementer, if nothing is snapshotted
/// yet — owns the next move.
pub fn needs_review(state: &ChangeState) -> bool {
    match state.latest_verdict() {
        None => true,
        Some(verdict) => state
            .latest_patchset()
            .is_some_and(|patchset| verdict.created_at < patchset.created_at),
    }
}

/// The latest verdict asked for changes and no newer patchset has answered it.
pub fn changes_requested(state: &ChangeState) -> bool {
    let Some(verdict) = state.latest_verdict() else {
        return false;
    };
    if verdict.verdict != Verdict::ChangesRequested {
        return false;
    }
    // No patchset newer than the verdict has superseded the request.
    state
        .latest_patchset()
        .is_none_or(|patchset| patchset.created_at <= verdict.created_at)
}
