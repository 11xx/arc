use crate::gates::GatesFile;
use crate::gitio;
use crate::model::{
    Falsification, MessageSeverity, MessageType, ProbePhase, Verdict, VerifyResult,
};
use crate::policy::PolicyFile;
use crate::state::{self, ChangeState, ClaimIdentity, GitIdentity};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

pub const STATUS_SCHEMA: &str = "arc-status/16";
pub const BLOCKER_STATUS_SCHEMA: &str = "arc-blocker-status/1";
pub const SELF_APPROVAL_REASON: &str = "approval rejected by policy: self-approval";
/// Two identities arc assumed cannot establish that two people acted. The
/// self-approval guard compares effective authors, so an assumed identity on
/// both sides makes the comparison meaningless rather than passing.
/// A verdict graph with several tips has no authority to report, so the
/// change reads as unreviewed unless the blocker says which it is: nobody
/// reviewed, or several reviewers each replaced the same verdict.
pub const CONTESTED_VERDICT_REASON: &str =
    "verdicts replace the same earlier verdict, so none is authoritative; record a verdict \
     superseding all of them";
pub const UNDECLARED_APPROVAL_REASON: &str =
    "approval rejected by policy: arc assumed the reviewing or the authoring identity from \
     git config, so independence is unproven (pass --actor or set ARC_ACTOR)";

/// Typed integration blockers, ordered by exit-code precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Blocker {
    Closed,
    BranchMissing,
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

impl Blocker {
    pub fn exit_code(self) -> i32 {
        match self {
            Blocker::Closed | Blocker::BranchMissing => 6,
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

/// Whether a gate's passing evidence was ever shown capable of failing.
///
/// A pass alone cannot say. `Discriminating` means the same check was recorded
/// failing at an earlier revision of this change, for a reason predicted
/// before it ran, and this run answers that failure. `Undiscriminated` means
/// no such failure was recorded — the usual case, and not a defect.
///
/// Advisory throughout: nothing here participates in readiness, gate results,
/// or exit codes.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Discrimination {
    Discriminating,
    Undiscriminated,
}

#[derive(Debug, Serialize)]
pub struct GateStatus {
    pub name: String,
    pub command: String,
    pub result: String,
    pub green_at_head: bool,
    /// Passing evidence exists at this head, but for a different command than
    /// the gate now declares. Not green: the declaration is the check.
    #[serde(skip_serializing_if = "is_false")]
    pub declaration_changed: bool,
    /// The head evidence for this gate is attested (arc did not run it), so a
    /// lead can apply stricter judgment even though it counts for green-ness.
    pub attested: bool,
    /// The tree this gate's readiness was read at, named only where it is not
    /// the head's own content — that is, where the change is behind its target
    /// and a merge would ship something neither branch committed. Absent means
    /// the head's content is what ships, and the gate answers for it.
    /// Additive in `arc-status/15`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluated_tree: Option<String>,
    /// The revision the counted evidence was recorded at, named only where it
    /// is not the current head. A gate answers for content, so a run against
    /// another commit holding the tree being evaluated answers for this one;
    /// this says which run that was, so a green gate nobody ran at this head
    /// is never mistaken for one that was never keyed at all.
    /// Additive in `arc-status/16`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherited_from: Option<String>,
    /// The tree the command ran against, and whether it differed from the
    /// revision's. Absent is unknown, never a claim the tree was clean.
    /// Additive in `arc-status/6`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tested_tree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_dirty: Option<bool>,
    /// Which kind of dirt, so a waiver reason is self-evident at the moment of
    /// waiving. Absent on evidence recorded before the split, which is not the
    /// same as clean. Additive in `arc-status/10`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_dirty_tracked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_dirty_untracked: Option<bool>,
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
    /// Whether this gate was ever shown capable of failing at this revision.
    /// Absent when the counted evidence is not a pass: discrimination is a
    /// property of a pass, and a failure or a missing run has none to report.
    /// Additive in `arc-status/14`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discrimination: Option<Discrimination>,
    /// The failure that was answered, when one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub falsification: Option<Falsification>,
    /// The evidence carrying that reference, named only when it is not the
    /// evidence that counts. A rerun at the same revision appends newer
    /// passing evidence without a reference, and the gate stays
    /// discriminating; the two ids then say which run proved what.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discrimination_event_id: Option<String>,
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
        // Before the provenance checks: this evidence may be perfectly good
        // provenance for a command the gate no longer declares, and saying
        // its tree was unrecorded would send the reader to the wrong repair.
        if self.declaration_changed {
            return Some("the gate declaration changed since this evidence was recorded");
        }
        Some(match self.result.as_str() {
            "pending" if self.evaluated_tree.is_some() => "no evidence at the merged tree",
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
        // While the tree is dirty, no local run can produce evidence that
        // counts — attested evidence carries its own execution context and is
        // unaffected — including a rerun of a gate that failed, and including
        // a gate that failed *because* of the uncommitted file. Cleaning is not claimed to
        // fix a failure; it is the precondition for any run whose result is
        // usable. It also cannot loop where the live tree is known: this reads
        // it, so once the tree is clean the advice becomes the rerun. Where it
        // is unknown — a change with no worktree — the advice is the rerun,
        // and whether such a change can gate at all is its own question.
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
    /// Every active hold, in event-ID order. Holds are independent, so a
    /// caller that acts on one has not cleared the others.
    pub reasons: Vec<HoldEntry>,
}

fn hold_entries(state: &ChangeState) -> Vec<HoldEntry> {
    state
        .holds
        .values()
        .map(|hold| HoldEntry {
            hold_event_id: hold.hold_event_id.clone(),
            reason: hold.reason.clone(),
            held_by: hold.held_by.clone(),
        })
        .collect()
}

/// One active hold, named by the event that set it so a release can lift
/// exactly that one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoldEntry {
    pub hold_event_id: String,
    pub reason: String,
    pub held_by: String,
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
    /// Why the claim this one displaced had its lease cut short, as the
    /// taker stated it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub displaced_reason: Option<String>,
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
    /// This reviewer matches a contributor on the patchset it last saw, so its
    /// verdict carries no independence.
    pub is_author: bool,
    /// The contributor matched by this reviewer, when the review is
    /// non-independent for that patchset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_contributor: Option<String>,
    /// Whether the patchset used its compatibility attribution or an explicit
    /// contributor declaration.
    pub contributors_source: &'static str,
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
    pub iterating: bool,
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
    /// The tree a merge into the target would ship, which is what the required
    /// gates have to answer for. It is the head's own tree while the change is
    /// rebased, and content neither branch committed once it is behind. `None`
    /// where no single tree exists to name: no branch, no target, a textual
    /// conflict, or a report derived from the ledger alone. Additive in
    /// `arc-status/15`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_tree: Option<String>,
    pub latest_patchset: Option<crate::state::Patchset>,
    pub brief: Option<BriefStatus>,
    pub head_matches_latest_patchset: bool,
    pub worktree_dirty: Option<bool>,
    pub verdict: Option<VerdictStatus>,
    /// Who reviewed what, newest coverage first. Additive in arc-status/6.
    pub review_map: Vec<ReviewerCoverage>,
    /// What a lead should know before integrating and arc will not refuse for:
    /// thin or stale review coverage, review only by the brief's author, an
    /// undischarged audit obligation. Never affects readiness or exit status.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub advisories: Vec<Advisory>,
    pub findings: Vec<FindingSummary>,
    pub open_blocking_findings: Vec<String>,
    pub holds: Vec<HoldEntry>,
    pub gates: Vec<GateStatus>,
    pub probes: Vec<ProbeStatus>,
    pub blocker_summary: BlockerSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_rejection_reason: Option<String>,
    /// Several verdicts replace the same earlier verdict, so none of them is
    /// authoritative and the change carries no usable verdict until one
    /// supersedes them all. Reported because the alternative reads as though
    /// nobody reviewed the change.
    #[serde(skip_serializing_if = "is_false")]
    pub verdict_contested: bool,
    /// A declared debt supplied a missing verdict or let a self-approval
    /// stand. Only then is the waiver an authorization input.
    #[serde(skip_serializing_if = "is_false")]
    pub approval_waived_by_debt: bool,
    pub next_action: String,
    /// Facts sessions kept while working, oldest first. Carried on the report
    /// so `resume` and `status --json` hand them back without a second read.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<crate::state::KeptContext>,
    /// Whether this change is in scope for the independent-review rule, and
    /// which declaration put it there.
    pub danger: DangerScope,
    pub ready_reason: String,
    pub ready_to_integrate: bool,
    /// Backward-compatible spelling retained from arc-status/1.
    pub integrate_ready: bool,
    pub blockers: Vec<Blocker>,
    pub closure: Option<crate::state::ClosureState>,
    /// A declared review obligation with no audit answering it. Additive in
    /// arc-status/6.
    pub debt_outstanding: bool,
    /// The change's gating approval was recorded as owed corroboration, and
    /// no audit has supplied it. Reported beside `debt_outstanding`
    /// because it is the same obligation seen one step earlier: debt says a
    /// review never happened, this says one happened and is not yet trusted.
    /// Additive in `arc-status/9`.
    pub provisional_approval_outstanding: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debt: Option<crate::state::Debt>,
    /// The dirty-tree waiver in force at the current head, if one is. A waiver
    /// naming an earlier revision is spent, and is not reported as though it
    /// still excused anything. Additive in `arc-status/10`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty_tree_waiver: Option<crate::state::DirtyTreeWaiver>,
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
    /// How far the brief's base revision sits from the change head, so a
    /// reader knows whether the brief's citations still describe the tree.
    /// Additive in `arc-status/11`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_drift: Option<BriefBaseDrift>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub acceptance_probes: Vec<crate::model::AcceptanceProbe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_slice: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum BriefBaseDrift {
    /// The brief was checked against the revision the change now heads.
    Current,
    /// The head descends from the base by this many commits.
    Behind { commits: usize },
    /// The base is not in the change's history at all — rebased or rewound —
    /// so it describes a tree this change no longer contains.
    Detached,
}

impl BriefBaseDrift {
    /// The clause a brief-printing surface appends after the base revision.
    /// `None` when the base is the head and there is nothing to say. One
    /// definition, because four surfaces print it and a wording that lived in
    /// four places would go stale in three of them.
    pub fn annotation(&self) -> Option<String> {
        match self {
            BriefBaseDrift::Current => None,
            BriefBaseDrift::Behind { commits } => Some(format!(
                " — **{commits} commits behind the change head**; line citations may have decayed"
            )),
            BriefBaseDrift::Detached => Some(
                " — **not in this change's history** (rebased or rewound); line citations describe a tree this change no longer contains"
                    .to_string(),
            ),
        }
    }
}

pub fn brief_base_drift(
    cwd: &Path,
    base_revision: Option<&str>,
    head: Option<&str>,
) -> Option<BriefBaseDrift> {
    let (Some(base), Some(head)) = (base_revision, head) else {
        return None;
    };
    if base == head {
        return Some(BriefBaseDrift::Current);
    }
    if !gitio::is_ancestor(cwd, base, head).ok()? {
        return Some(BriefBaseDrift::Detached);
    }
    Some(BriefBaseDrift::Behind {
        commits: gitio::commit_count(cwd, base, head).ok()?,
    })
}

#[derive(Debug, Serialize)]
pub struct VerdictStatus {
    pub verdict: Verdict,
    pub patchset_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Why this verdict is owed corroboration, when the reviewer said it is.
    /// Additive in `arc-status/9`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provisional: Option<String>,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    /// Whether arc invented the identity this verdict is attributed to rather
    /// than someone declaring it. Additive in `arc-status/6`.
    pub author_assumed: bool,
    pub valid_for_current_head: bool,
}

/// Whether an approval fails the self-approval guard: the reviewer matches a
/// contributor, or an identity arc assumed rather than one somebody declared.
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
    patchset.contributor_match(verdict_author).is_some()
        || patchset.author_assumed()
        || verdict.author_assumed()
}

/// Build the status report: replayed ledger state joined with live Git
/// facts, dependency state, and the declared gate policy.
pub use crate::model::{DangerRule, DangerScope};

impl DangerScope {
    /// Independent review is owed when the repository forbids self-approval
    /// *and* this change is in scope for that rule.
    pub fn requires_independent_review(&self, policy: &PolicyFile) -> bool {
        policy.policy.forbid_self_approval && self.dangerous
    }

    /// One line naming why the gate landed where it did.
    pub fn explain(&self) -> String {
        match self.rule {
            DangerRule::NotDeclared => {
                "the project declares no dangerous surfaces, so every change needs one".into()
            }
            DangerRule::Escalated => "the change was opened --dangerous".into(),
            DangerRule::DeclaredPath => {
                format!("it touches declared surfaces: {}", self.paths.join(", "))
            }
            DangerRule::Untouched => "it touches no declared dangerous surface".into(),
            DangerRule::Undetermined => {
                "the touched paths could not be established, so it is assumed dangerous".into()
            }
        }
    }

    /// No working tree to diff. An unknown surface is the one case where
    /// guessing wrong must not lower the bar.
    pub fn undetermined(state: &ChangeState) -> Self {
        DangerScope {
            dangerous: true,
            rule: if state.dangerous {
                DangerRule::Escalated
            } else {
                DangerRule::Undetermined
            },
            paths: Vec::new(),
        }
    }

    pub(crate) fn resolve(
        state: &ChangeState,
        policy: &PolicyFile,
        cwd: &Path,
        head: Option<&str>,
    ) -> Self {
        if state.dangerous {
            return DangerScope {
                dangerous: true,
                rule: DangerRule::Escalated,
                paths: Vec::new(),
            };
        }
        // An undeclared list keeps the previous uniform behaviour, so
        // adopting the feature is opt-in rather than a silent loosening.
        if !policy.danger.is_declared() {
            return DangerScope {
                dangerous: true,
                rule: DangerRule::NotDeclared,
                paths: Vec::new(),
            };
        }
        let Some(head) = head else {
            return DangerScope::undetermined(state);
        };
        let Ok(changed) = gitio::changed_paths(cwd, &state.base, head) else {
            return DangerScope::undetermined(state);
        };
        let paths = policy.danger.matching(changed.iter().map(String::as_str));
        DangerScope {
            dangerous: !paths.is_empty(),
            rule: if paths.is_empty() {
                DangerRule::Untouched
            } else {
                DangerRule::DeclaredPath
            },
            paths,
        }
    }
}

/// The tree each revision names, for evidence recorded before arc kept the
/// tree beside it.
///
/// Resolved once per distinct revision rather than per gate, and only where a
/// tree-keyed reading is what the report needs. A revision whose commit is
/// gone is absent here, and evidence naming it then matches no tree.
pub(crate) fn legacy_evidence_trees(state: &ChangeState, cwd: &Path) -> BTreeMap<String, String> {
    state
        .verifications
        .iter()
        .filter(|entry| entry.tree.is_none())
        .map(|entry| entry.revision.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|revision| {
            Some((
                revision.to_string(),
                gitio::commit_tree(cwd, revision).ok()?,
            ))
        })
        .collect()
}

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
    // One merge-tree run answers both questions it can answer: whether the
    // text conflicts, and what a clean merge would ship.
    let merge = match (&current_head, &target_head) {
        (Some(head), Some(target)) => Some(gitio::merge_outcome(cwd, target, head)?),
        _ => None,
    };
    let needs_rebase = merge.as_ref().is_some_and(|merge| merge.conflicts);
    let merged_tree = merge.and_then(|merge| merge.tree);
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
    let danger = DangerScope::resolve(state, policy, cwd, current_head.as_deref());
    build_report(
        state,
        gates,
        policy,
        dependency_status,
        blocks,
        now,
        Some(cwd),
        current_head,
        needs_rebase,
        merged_tree,
        worktree_dirty,
        danger,
    )
}

/// Build a report from a state replayed to a past event. Live Git facts are
/// not consulted: the derived latest-patchset head is taken as the head that
/// was current then, and rebase state is not simulated. Used by `--at`.
/// A report derived from the ledger alone, for callers with no working tree.
///
/// `repo` is any path inside the repository. It is not a working tree and is
/// not read as one: resolving the danger scope needs the recorded base and
/// patchset head plus the objects they name, which every clone already has.
/// Passing `None` leaves the scope undetermined, which assumes dangerous.
pub fn build_as_of(
    state: &ChangeState,
    gates: &GatesFile,
    policy: &PolicyFile,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
    now: DateTime<Utc>,
    repo: Option<&Path>,
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
        repo,
        current_head.clone(),
        false,
        None,
        None,
        match repo {
            Some(repo) => DangerScope::resolve(state, policy, repo, current_head.as_deref()),
            None => DangerScope::undetermined(state),
        },
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
    cwd: Option<&Path>,
    current_head: Option<String>,
    needs_rebase: bool,
    merged_tree: Option<String>,
    worktree_dirty: Option<bool>,
    danger: DangerScope,
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
    let base_drift = cwd.and_then(|cwd| {
        state.latest_brief().and_then(|brief| {
            brief_base_drift(cwd, brief.base_revision.as_deref(), current_head.as_deref())
        })
    });

    let mut waiver_authorized_approval = false;
    let verdict = state.latest_verdict().map(|v| {
        let approved_patchset = latest_patchset
            .as_ref()
            .filter(|patchset| patchset.id == v.patchset_id);
        // Self-approval compares the reviewer with every contributor on the
        // patchset, so a lead may review work recorded for an executor unless
        // the lead is also in the declared contributor set.
        // A declared debt converts the absent or policy-rejected review
        // into a recorded obligation. The requirement is carried forward where
        // a query can find it when no independent reviewer is reachable.
        let would_reject_self_approval = danger.requires_independent_review(policy)
            && approved_patchset
                .is_some_and(|patchset| undeclared_or_self(patchset, v.effective_author(), v));
        // The waiver only authorizes anything when it is what let the approval
        // stand. Recording it otherwise would claim a merge rested on a waiver
        // that changed nothing.
        // Only an approval can be waived into validity. A waiver declared
        // beside a changes-requested or comment-only verdict authorized
        // nothing, and saying otherwise would report an approval that does
        // not exist.
        let debt_waives_current_head = head_matches && state.debt_waives_latest_patchset();
        waiver_authorized_approval = v.verdict == Verdict::Approved
            && would_reject_self_approval
            && debt_waives_current_head;
        let rejected_self_approval = would_reject_self_approval && !debt_waives_current_head;
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
            provisional: v.provisional.clone(),
            actor: v.actor.clone(),
            on_behalf_of: v.on_behalf_of.clone(),
            author_assumed: v.author_assumed(),
            valid_for_current_head: valid,
        }
    });
    // True whenever the waiver is load-bearing: it rescued a self-approval, or
    // it stood in for a verdict that was never recorded. Reporting it only in
    // the first case would let the second merge look independently approved.
    // Computed before the report takes ownership of the head it is compared
    // against. A waiver covers exactly the revision it names.
    let dirty_tree_waiver_in_force = state
        .dirty_tree_waiver
        .clone()
        .filter(|waiver| Some(&waiver.revision) == current_head.as_ref());
    let debt_waives_current_head = head_matches && state.debt_waives_latest_patchset();
    let approval_waived_by_debt = waiver_authorized_approval
        || (debt_waives_current_head
            && !verdict
                .as_ref()
                .map(|v| v.valid_for_current_head)
                .unwrap_or(false)
            && !verdict.as_ref().is_some_and(|v| {
                v.verdict != Verdict::Approved
                    && latest_patchset
                        .as_ref()
                        .is_some_and(|patchset| patchset.id == v.patchset_id)
            }));
    let approval_rejection_reason = verdict.as_ref().and_then(|verdict| {
        (!verdict.valid_for_current_head
            && verdict.verdict == Verdict::Approved
            && latest_patchset.as_ref().is_some_and(|patchset| {
                let verdict_author = verdict
                    .on_behalf_of
                    .as_deref()
                    .unwrap_or(verdict.actor.as_str());
                danger.requires_independent_review(policy)
                    && !debt_waives_current_head
                    && patchset.id == verdict.patchset_id
                    && (patchset.contributor_match(verdict_author).is_some()
                        || patchset.author_assumed()
                        || verdict.author_assumed)
            }))
        .then(|| {
            let matched_contributor = latest_patchset.as_ref().and_then(|patchset| {
                let verdict_author = verdict
                    .on_behalf_of
                    .as_deref()
                    .unwrap_or(verdict.actor.as_str());
                patchset.contributor_match(verdict_author)
            });
            if let Some(contributor) = matched_contributor {
                format!("{SELF_APPROVAL_REASON}: reviewer matches contributor {contributor}")
            } else {
                UNDECLARED_APPROVAL_REASON.to_string()
            }
        })
    });
    // A contested verdict graph has no single authority, so `latest_verdict`
    // reports none and the change reads as unreviewed. Without this the
    // blocker would say nobody reviewed it, which is the opposite of what
    // happened: two reviewers did, and each replaced the same verdict.
    let verdict_contested = state.verdict_contested();
    let approval_rejection_reason = approval_rejection_reason.or_else(|| {
        verdict_contested
            .then(|| format!("{} {CONTESTED_VERDICT_REASON}", state.verdict_tips().len()))
    });

    let review_map = reviewer_coverage(state);
    let advisories = advisories(&review_map, latest_patchset.as_ref(), state, &danger);

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

    // The tree the gates must answer for, named only where it is not the
    // head's own — that is, where a merge would ship content neither branch
    // committed.
    let head_tree = cwd
        .zip(current_head.as_deref())
        .and_then(|(cwd, head)| gitio::commit_tree(cwd, head).ok());
    let evaluated_tree = merged_tree
        .clone()
        .filter(|merged| head_tree.as_ref() != Some(merged));
    // A gate answers for content, so content is the key everywhere it is
    // known: the merge's tree where the change is behind its target, the
    // head's own otherwise. A rebase that moves the base without touching the
    // diff produces a new commit holding a tree some run already answered
    // for, and that answer still describes what ships. Only a head whose tree
    // cannot be read falls back to the revision.
    let lookup_tree = evaluated_tree.clone().or_else(|| head_tree.clone());
    let legacy_trees = match (&lookup_tree, cwd) {
        (Some(_), Some(cwd)) => legacy_evidence_trees(state, cwd),
        _ => BTreeMap::new(),
    };
    let resolve_tree = |revision: &str| legacy_trees.get(revision).cloned();

    let gate_statuses: Vec<GateStatus> = gates
        .required_for(&state.profile)
        .into_iter()
        .map(|(name, gate)| {
            let evidence = match (&lookup_tree, current_head.as_deref()) {
                (Some(tree), _) => state.gate_evidence_at_tree(name, tree, &resolve_tree),
                (None, Some(head)) => state.gate_evidence_at(name, head),
                (None, None) => None,
            };
            let result = evidence.map(|e| e.result);
            // Discrimination is asked of the gate at this revision, not of the
            // newest run, so a rerun that appends a plain pass cannot retract
            // what an earlier run established against the same tree.
            let counted_pass = evidence.filter(|e| e.result == crate::model::VerifyResult::Pass);
            let discriminating = counted_pass.and_then(|e| match &lookup_tree {
                Some(tree) => state.gate_falsification_at_tree(name, tree, &resolve_tree),
                None => state.gate_falsification_at(name, &e.revision),
            });
            GateStatus {
                name: name.clone(),
                command: gate.command.clone(),
                evaluated_tree: evaluated_tree.clone(),
                inherited_from: evidence
                    .map(|e| e.revision.clone())
                    .filter(|revision| Some(revision.as_str()) != current_head.as_deref()),
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
                // Evidence is green for the command it ran, not for the
                // gate's name. A declaration edited after the run describes a
                // different check, and counting the old pass would let a gate
                // nobody has run authorize a merge.
                // The whole declaration, not just the command: a run under a
                // laxer timeout is not evidence for a stricter one, and a run
                // whose declared timeout is unknown cannot be shown to satisfy
                // a declaration that has one.
                green_at_head: evidence.is_some_and(|e| {
                    e.green_at_head(state.dirty_tree_waiver.as_ref())
                        && matches_declaration(e, gate)
                }),
                declaration_changed: evidence.is_some_and(|e| {
                    e.green_at_head(state.dirty_tree_waiver.as_ref())
                        && !matches_declaration(e, gate)
                }),
                attested: evidence.is_some_and(|e| e.attested),
                tested_tree: evidence.and_then(|e| e.tested_tree.clone()),
                worktree_dirty: evidence.and_then(|e| e.worktree_dirty),
                worktree_dirty_tracked: evidence.and_then(|e| e.worktree_dirty_tracked),
                worktree_dirty_untracked: evidence.and_then(|e| e.worktree_dirty_untracked),
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
                discrimination: counted_pass.map(|_| match discriminating {
                    Some(_) => Discrimination::Discriminating,
                    None => Discrimination::Undiscriminated,
                }),
                falsification: discriminating.and_then(|e| e.falsification.clone()),
                discrimination_event_id: discriminating
                    .map(|e| e.event_id.clone())
                    .filter(|id| Some(id.as_str()) != counted_pass.map(|e| e.event_id.as_str())),
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
    if state.iterating {
        blockers.push(Blocker::Iterating);
    }
    if dependency_status.blocked {
        blockers.push(Blocker::BlockedByChanges);
    }
    if needs_rebase {
        blockers.push(Blocker::NeedsRebase);
    }
    // Nothing has been run against the content that would ship. A tree some
    // gates have answered for and others have not is an ordinary red gate;
    // this is the case where the whole evaluation is missing, and where
    // running a gate at the head would record it against the wrong tree.
    let merged_tree_unevaluated = evaluated_tree.is_some()
        && !gate_statuses.is_empty()
        && !gate_statuses
            .iter()
            .any(|gate| gate.evidence_event_id.is_some());
    if merged_tree_unevaluated {
        blockers.push(Blocker::MergedTreeUnevaluated);
    }
    if !open_blocking.is_empty() {
        blockers.push(Blocker::BlockingFindings);
    }
    // A waiver stands in for a verdict nobody recorded. It does not stand over
    // one that refused: a reviewer who read this patchset and asked for changes
    // has said something a waiver has no business overriding, and letting the
    // author waive past it would make the mechanism a way to ignore review
    // rather than a way to defer it.
    //
    // So the waiver satisfies this gate exactly when the gate is unmet for want
    // of a verdict — none recorded, or one that only policy's self-approval rule
    // rejects. The obligation itself is untouched and stays where
    // `arc query --debt` finds it.
    let verdict_refuses_this_head = verdict.as_ref().is_some_and(|v| {
        v.verdict != Verdict::Approved
            && latest_patchset
                .as_ref()
                .is_some_and(|patchset| patchset.id == v.patchset_id)
    });
    let approval_valid = verdict
        .as_ref()
        .map(|v| v.valid_for_current_head)
        .unwrap_or(false);
    let waiver_satisfies_approval = debt_waives_current_head && !verdict_refuses_this_head;
    if !state.iterating && !approval_valid && !waiver_satisfies_approval {
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
    if !state.holds.is_empty() {
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
            active: !state.holds.is_empty(),
            reasons: hold_entries(state),
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
    } else if let Some(hold) = state.holds.values().next() {
        format!("release_hold:{}", hold.hold_event_id)
    } else if let Some(gate) = gate_statuses.iter().find(|gate| !gate.green_at_head) {
        // A gate read at a merged tree is not repaired by running it at the
        // head: that records evidence for content the merge discards. The
        // merge is what has to be evaluated, and re-evaluated once the work
        // that makes it pass lands.
        if evaluated_tree.is_some() {
            format!("verify_against:{}", state.target_branch)
        } else {
            gate.clearing_action(worktree_dirty)
        }
    } else if let Some(probe) = probe_statuses
        .iter()
        .find(|probe| !probe.discriminating_at_head)
    {
        format!("run_probe:{}", probe.name)
    } else if state.iterating {
        // An iterating change owes declared debt rather than a verdict,
        // so it never reaches `request_review`. It reaches this arm only once
        // findings, holds, gates and probes are clear, because those are real
        // work whether or not integration is the goal.
        if state.debt.is_none() && state.latest_patchset().is_some() {
            "declare_debt".into()
        } else {
            "iterating:clear".into()
        }
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
        !state.holds.is_empty(),
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
        iterating: state.iterating,
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
        merged_tree,
        latest_patchset,
        brief: state.latest_brief().map(|brief| BriefStatus {
            version: state.briefs.len(),
            title: brief.title.clone(),
            base_revision: brief.base_revision.clone(),
            base_drift,
            acceptance_probes: brief.acceptance_probes.clone(),
            plan_ref: brief.plan_ref.clone(),
            plan_slice: brief.plan_slice.clone(),
            recorded_at: brief.ts,
        }),
        head_matches_latest_patchset: head_matches,
        worktree_dirty,
        verdict,
        review_map,
        advisories,
        findings,
        open_blocking_findings: open_blocking,
        holds: hold_entries(state),
        gates: gate_statuses,
        probes: probe_statuses,
        blocker_summary,
        approval_rejection_reason,
        verdict_contested,
        approval_waived_by_debt,
        next_action,
        kept: state.kept.clone(),
        danger,
        ready_reason,
        ready_to_integrate: ready,
        integrate_ready: ready,
        blockers,
        closure: state.closure.clone(),
        debt_outstanding: state.debt_outstanding(),
        provisional_approval_outstanding: state.provisional_approval_outstanding(),
        debt: state.debt.clone(),
        dirty_tree_waiver: dirty_tree_waiver_in_force,
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
        displaced_reason: claim
            .displaced
            .as_ref()
            .and_then(|displaced| displaced.reason.clone()),
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
        Blocker::Iterating,
        Blocker::BlockedByChanges,
        Blocker::NeedsRebase,
        Blocker::MergedTreeUnevaluated,
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
            // Independence is a fact about the patchset this reviewer
            // actually read, not about whatever happens to be newest now.
            // Judging it against the final patchset lets a later snapshot by
            // a different author retroactively make a reviewer independent,
            // or a self-review look independent once someone else pushes.
            let reviewed = state
                .patchsets
                .iter()
                .find(|patchset| patchset.id == tally.best_id);
            let matched_contributor = reviewed
                .and_then(|patchset| patchset.contributor_match(&reviewer))
                .map(str::to_string);
            let is_author = matched_contributor.is_some();
            ReviewerCoverage {
                covers_final: final_patchset.is_some_and(|patchset| patchset.id == tally.best_id),
                // Indistinguishable rather than independent: the identity
                // matches the snapshot's and neither side declared a subject.
                attribution_unknown: is_author
                    && !tally.attributed
                    && reviewed.is_some_and(|patchset| {
                        patchset.on_behalf_of.is_none() && patchset.contributors.is_empty()
                    }),
                is_author,
                matched_contributor,
                contributors_source: reviewed
                    .map(crate::state::Patchset::contributors_source)
                    .unwrap_or("unknown"),
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

/// Whether recorded evidence ran the gate as it is declared now.
pub fn matches_declaration(
    evidence: &crate::state::VerificationEntry,
    gate: &crate::gates::Gate,
) -> bool {
    evidence.command == gate.command && evidence.timeout_seconds == gate.timeout
}

/// One advisory: something a lead should know before integrating, which is
/// deliberately not a blocker. An orchestrator's review is a valid review
/// unless a project's policy says otherwise, so arc reports the shape of the
/// review and lets the project judge it.
#[derive(Debug, Clone, Serialize)]
pub struct Advisory {
    /// Stable machine-readable kind, so a consumer can act on one advisory
    /// without parsing prose.
    pub code: &'static str,
    pub detail: String,
}

/// Advisories for `arc check`. Never blockers: a change with one reviewer is
/// normal, and refusing it would make the tool unusable for the
/// single-operator case it is most often run in.
pub fn advisories(
    review_map: &[ReviewerCoverage],
    final_patchset: Option<&crate::state::Patchset>,
    state: &ChangeState,
    danger: &DangerScope,
) -> Vec<Advisory> {
    let Some(final_patchset) = final_patchset else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for row in review_map {
        if !row.covers_final {
            warnings.push(Advisory {
                code: "reviewer-behind-final-patchset",
                detail: format!(
                    "{} last saw {}; integrating {}",
                    row.reviewer, row.last_patchset, final_patchset.id
                ),
            });
        }
    }
    // An unproven reviewer is still not the author, so a provisional verdict
    // satisfies independence and is reported on its own axis. Collapsing the
    // two would make "nobody independent read this" and "somebody read it
    // whose judgment is not yet trusted" the same state, which is the
    // conflation this advisory exists to end.
    if let Some(reason) = state
        .outstanding_provisional_approval()
        .and_then(|verdict| verdict.provisional.as_deref())
    {
        warnings.push(Advisory {
            code: "provisional-approval",
            detail: format!(
                "the verdict covering {} is owed corroboration: {reason}. Discharge it with an \
                 independent review of this patchset, or `arc audit` after it lands",
                final_patchset.id
            ),
        });
    }
    let independent = review_map
        .iter()
        .any(|row| row.covers_final && !row.is_author && !row.attribution_unknown);
    let matched = review_map
        .iter()
        .filter(|row| row.covers_final)
        .filter_map(|row| {
            row.matched_contributor
                .as_deref()
                .map(|contributor| format!("{} matches contributor {contributor}", row.reviewer))
        })
        .collect::<Vec<_>>();
    if !independent && !danger.dangerous {
        let detail = if matched.is_empty() {
            format!(
                "no independent reviewer covers {}, and none is required: {}",
                final_patchset.id,
                danger.explain()
            )
        } else {
            format!(
                "no independent reviewer covers {}; {}; none is required: {}",
                final_patchset.id,
                matched.join(", "),
                danger.explain()
            )
        };
        warnings.push(Advisory {
            code: "self-verdict-permitted",
            detail,
        });
    }
    if !independent && danger.dangerous {
        let unknown = review_map
            .iter()
            .any(|row| row.covers_final && row.attribution_unknown);
        warnings.push(if unknown {
            Advisory {
                code: "reviewer-attribution-unknown",
                detail: format!(
                    "no reviewer of {} is distinguishable from its author; \
                     record --on-behalf-of to make attribution legible{}",
                    final_patchset.id,
                    if matched.is_empty() {
                        String::new()
                    } else {
                        format!("; {}", matched.join(", "))
                    }
                ),
            }
        } else {
            let detail = if matched.is_empty() {
                format!("no independent reviewer covers {}", final_patchset.id)
            } else {
                format!(
                    "no independent reviewer covers {}; {}",
                    final_patchset.id,
                    matched.join(", ")
                )
            };
            Advisory {
                code: "no-independent-reviewer",
                detail,
            }
        });
    }
    // The review map makes brief-author-only review visible after the fact.
    // Saying it before integration is the point of an advisory: arc reports
    // that the identity which briefed the work is the only one that approved
    // it, and infers nothing about whether that was independent.
    // Verdicts on the patchset that is shipping — not the review map, whose
    // rows count a reviewer's whole history. A reviewer who approved an
    // earlier patchset and only filed a finding on this one has approved
    // nothing here, and one who filed only findings has approved nothing at
    // all; counting either would answer a question about the wrong window.
    let covering_reviewers = state
        .verdicts
        .iter()
        .filter(|verdict| verdict.patchset_id == final_patchset.id)
        .map(|verdict| {
            verdict
                .on_behalf_of
                .as_deref()
                .unwrap_or(verdict.actor.as_str())
        });
    if let (Some(author), Some(true)) = (
        state.brief_author_for(final_patchset),
        state.reviewed_only_by_brief_author(final_patchset, covering_reviewers),
    ) {
        warnings.push(Advisory {
            code: "brief-author-only-review",
            detail: format!(
                "every verdict on {} came from {author}, who wrote the brief",
                final_patchset.id
            ),
        });
    }
    if state.debt_outstanding() {
        warnings.push(Advisory {
            code: "debt-outstanding",
            detail: "a review obligation was recorded at integration and is not discharged"
                .to_string(),
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
            Blocker::Iterating,
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
