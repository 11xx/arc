use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisplacedClaim {
    pub claim_id: String,
    pub actor: String,
    pub harness: String,
    pub session: String,
    pub stage: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BriefRef {
    pub event_id: String,
}

/// The earlier ledger fact that caused a brief version. Each variant names a
/// specific object rather than a generic edge, so replay can validate the
/// reference against the kind it claims to be.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum BriefCause {
    Finding { finding_id: String },
    Verdict { event_id: String },
    BlockedOnStage { event_id: String },
    External { summary: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceptanceProbe {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum BlockerRef {
    Brief { brief_event_id: String },
    Finding { finding_id: String },
    Change { change_id: String },
    External,
}

/// One append-only ledger entry. The envelope is common to every event;
/// the payload is internally tagged by `event_type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: u32,
    pub event_id: String,
    pub repository_id: String,
    pub change_id: String,
    pub actor: String,
    /// Where `actor` came from. An identity nobody claimed is not evidence of
    /// who acted, and once appended it can never be corrected, so a derived
    /// view has to be able to tell a declared identity from an assumed one.
    /// Absent on events written before this was recorded, which is `unknown`
    /// rather than declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_source: Option<ActorSource>,
    /// The subject an action is performed for when a lead runs delegated
    /// ceremony. `actor` stays the invoker who ran the command; the effective
    /// author of the event is `on_behalf_of.unwrap_or(actor)`. Additive:
    /// serialized only when set, so old events and bundles round-trip intact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(flatten)]
    pub payload: Payload,
}

/// How the acting identity on an event was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActorSource {
    /// Declared on the command line.
    Flag,
    /// Declared through `ARC_ACTOR`.
    Env,
    /// Nobody declared one, so arc used `git config user.name`. This is the
    /// Git identity of whoever configured the checkout, not a claim about who
    /// acted.
    GitFallback,
}

impl ActorSource {
    /// Whether someone offered this identity, as opposed to arc inventing it. An assumed one names a person
    /// who never said they did anything.
    pub fn declared(self) -> bool {
        !matches!(self, Self::GitFallback)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "kebab-case")]
pub enum Payload {
    ChangeOpened {
        slug: String,
        title: String,
        profile: String,
        target_branch: String,
        branch: String,
        base: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        worktree: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blocked_by: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
        /// Journal artifact filename this change was opened from, if any.
        /// Additive: absent for changes not begun via `--from-journal`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        journal_ref: Option<String>,
    },
    MetadataUpdated {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        add_blocked_by: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remove_blocked_by: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        add_tags: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remove_tags: Vec<String>,
        /// Assignment update (latest wins). `Some("")` clears; `None` leaves
        /// the current assignment untouched. Advisory only — never enforced.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assign: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<i32>,
    },
    /// A structured cross-change announcement. Messages are announcements,
    /// never policy input: `check`/`integrate` ignore them entirely.
    Message {
        message_type: MessageType,
        severity: MessageSeverity,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },
    BriefRecorded {
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        body: String,
        /// Why this version exists. Empty on v1; required from v2, because a
        /// renegotiated contract without a recorded cause is indistinguishable
        /// from a proactive rewrite.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        caused_by: Vec<BriefCause>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_revision: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        acceptance_probes: Vec<AcceptanceProbe>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_slice: Option<String>,
    },
    ChangelogRecorded {
        #[serde(alias = "section")]
        category: String,
        body: String,
    },
    PatchsetAdded {
        patchset_id: String,
        base: String,
        head: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        merge_base: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brief_ref: Option<BriefRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        author_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        author_email: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        committer_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        committer_email: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim_actor: Option<String>,
    },
    ClaimSet {
        claim_id: String,
        ttl_seconds: u64,
        stage_budgets: BTreeMap<StageBudget, u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        displaced: Option<DisplacedClaim>,
    },
    ClaimReleased {
        claim_id: String,
    },
    StageSet {
        claim_id: String,
        stage: ClaimStage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocker: Option<BlockerRef>,
    },
    CommentAdded {
        body: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        patchset_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        anchor: Option<Anchor>,
    },
    FindingAdded {
        finding_id: String,
        blocking: bool,
        severity: Severity,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        patchset_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        anchor: Option<Anchor>,
    },
    ReplyAdded {
        parent_event_id: String,
        body: String,
    },
    DispositionRecorded {
        finding_id: String,
        status: DispositionStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        evidence: Option<String>,
        /// Event IDs of the disposition tips this one observed and replaces.
        /// Two dispositions superseding the same tip fork the chain; the
        /// finding is contested until a later disposition supersedes all tips.
        #[serde(default)]
        supersedes: Vec<String>,
    },
    VerdictRecorded {
        patchset_id: String,
        verdict: Verdict,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        causes: Vec<ReviewCause>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        /// Findings recorded atomically with the verdict (one review, one event).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        findings: Vec<InlineFinding>,
    },
    /// A review obligation this change carries but has not discharged.
    ///
    /// Declaring it is what lets a change integrate without an independent
    /// verdict: the requirement is not waived, it is recorded as debt that
    /// `arc query --audit-debt` can find after the reviewer becomes available.
    AuditDebtDeclared {
        reason: String,
        /// The patchset the waiver applies to. A waiver that stands in for an
        /// approval binds the way an approval binds: to one patchset, so a
        /// later commit invalidates it instead of excusing everything that
        /// follows. Absent when the debt is discovered after integration,
        /// where there is no gate left to waive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patchset_id: Option<String>,
    },
    /// A review performed after integration.
    ///
    /// Deliberately not a late `VerdictRecorded`. Sharing the event would make
    /// every consumer filter by closure timestamp to answer "what shipped with
    /// what review", and one that forgot would silently credit a change with
    /// review it did not have. The separation lives in the event model, where
    /// it cannot be forgotten.
    AuditVerdictRecorded {
        /// The integrated revision audited, not a patchset.
        revision: String,
        verdict: Verdict,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        findings: Vec<InlineFinding>,
    },
    /// A finding raised by a post-integration audit. An audit that could only
    /// say approved-or-not would be a rubber stamp.
    AuditFindingAdded {
        finding_id: String,
        blocking: bool,
        severity: Severity,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        anchor: Option<Anchor>,
    },
    /// A disposition recorded against a post-integration audit finding.
    ///
    /// This stays distinct from `DispositionRecorded`: allowing that
    /// open-change event after integration would also let later ceremony
    /// rewrite the finding state that existed when the change shipped.
    AuditDispositionRecorded {
        finding_id: String,
        status: DispositionStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        commit: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        evidence: Option<String>,
        #[serde(default)]
        supersedes: Vec<String>,
    },
    VerificationRunStarted {
        revision: String,
        mode: VerificationRunMode,
        skip_green: bool,
        gates: Vec<VerificationRunGate>,
    },
    VerificationRecorded {
        #[serde(skip_serializing_if = "Option::is_none")]
        gate: Option<String>,
        command: String,
        revision: String,
        result: VerifyResult,
        /// Absent when arc did not execute the command. Existing ledger events
        /// carry this field and round-trip byte-identically as `Some`.
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Absent when arc did not execute the command. Existing ledger events
        /// carry this field and round-trip byte-identically as `Some`.
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// Final bytes of combined stdout and stderr observed by arc.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tail: Option<String>,
        /// Arc terminated the gate after its declared timeout elapsed.
        #[serde(default, skip_serializing_if = "is_false")]
        timed_out: bool,
        hostname: String,
        /// Evidence that arc did not execute itself (e.g. a sandboxed
        /// executor ran the gate, or the result comes from another host).
        /// Absent = arc ran the command and observed the result, so existing
        /// ledgers and bundles serialize and replay byte-identically.
        #[serde(default, skip_serializing_if = "is_false")]
        attested: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        probe: Option<ProbeEvidenceRef>,
        /// Stable external runner identity for attested evidence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runner: Option<String>,
        /// Optional free-form note recorded alongside the evidence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// The tree arc actually ran against, written into the object database
        /// and pinned by a ref, so a recorded tree is one that is still there.
        /// A revision alone describes a tree no checkout reproduces whenever
        /// the worktree carried uncommitted work, which is the ordinary shape
        /// of agent execution. Absent on attested evidence, on events written
        /// before arc recorded it, and on a run whose tree could not be
        /// pinned: unknown, not clean.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tested_tree: Option<String>,
        /// Whether that tree differed from the revision's own. Absent means
        /// unknown for the same reasons.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_dirty: Option<bool>,
        /// The worktree changed while the command ran, so the evidence
        /// describes no single tree.
        #[serde(default, skip_serializing_if = "is_false")]
        tree_moved: bool,
    },
    VerificationReused {
        run_id: String,
        gate: String,
        revision: String,
        evidence_event_id: String,
    },
    HoldSet {
        reason: String,
    },
    HoldReleased {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    ChangeClosed {
        outcome: Closure,
        #[serde(skip_serializing_if = "Option::is_none")]
        integrated_commit: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        superseded_by: Option<String>,
    },
    /// Declared forge projection: the explicit repository tuple plus policy
    /// a later observed link is validated against. Latest declaration wins.
    ForgeProjection {
        host: String,
        base_repo: String,
        base_ref: String,
        head_repo: String,
        head_ref: String,
        policy: crate::forge::ForgePolicy,
    },
    /// Observed post-creation PR tuple, read back from the forge by the
    /// agent. Only appended after fail-closed validation against the
    /// declaration; recording one asserts the tuple matched.
    ForgeLink {
        pr_number: u64,
        url: String,
        base_repo: String,
        base_ref: String,
        head_repo: String,
        head_ref: String,
        head_sha: String,
    },
    /// Observed hosted-check rollup at an exact PR head. Zero checks is
    /// `not-configured`/`not-triggered`, never `passed`.
    ForgeChecks {
        pr_head: String,
        state: crate::forge::ForgeCheckState,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Observed PR lifecycle, bound to the exact link and head it was read
    /// at. `merged` carries the actual merge commit. The binding fields are
    /// absent only on events written before lifecycle facts were bound.
    ForgePrState {
        state: crate::forge::ForgePrState,
        #[serde(skip_serializing_if = "Option::is_none")]
        merge_sha: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        link_event_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pr_head: Option<String>,
    },
    /// An event whose `event_type` this build does not recognize (e.g. one
    /// imported from a newer arc). Typed loading skips these entries; the
    /// underlying files and raw export preserve their original bytes intact.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendPermission {
    OpenOnly,
    AnyPhaseFact,
    OpenOrIntegratedFact,
    IntegratedOnlyFact,
    LifecycleOwned,
    OpaqueImported,
}

/// Classify every ledger payload by the lifecycle phases in which commands
/// may append it. New payload variants must make an explicit lifecycle choice.
pub fn append_permission(payload: &Payload) -> AppendPermission {
    match payload {
        Payload::MetadataUpdated { .. }
        | Payload::BriefRecorded { .. }
        | Payload::PatchsetAdded { .. }
        | Payload::ClaimSet { .. }
        | Payload::StageSet { .. }
        | Payload::FindingAdded { .. }
        | Payload::DispositionRecorded { .. }
        | Payload::VerdictRecorded { .. }
        | Payload::VerificationRunStarted { .. }
        | Payload::VerificationRecorded { .. }
        | Payload::VerificationReused { .. }
        | Payload::HoldSet { .. }
        | Payload::ForgeProjection { .. } => AppendPermission::OpenOnly,
        Payload::Message { .. }
        | Payload::ClaimReleased { .. }
        | Payload::CommentAdded { .. }
        | Payload::ReplyAdded { .. }
        | Payload::HoldReleased { .. }
        | Payload::ForgeLink { .. }
        | Payload::ForgeChecks { .. }
        | Payload::ForgePrState { .. } => AppendPermission::AnyPhaseFact,
        Payload::ChangelogRecorded { .. } | Payload::AuditDebtDeclared { .. } => {
            AppendPermission::OpenOrIntegratedFact
        }
        Payload::AuditVerdictRecorded { .. }
        | Payload::AuditFindingAdded { .. }
        | Payload::AuditDispositionRecorded { .. } => AppendPermission::IntegratedOnlyFact,
        Payload::ChangeOpened { .. } | Payload::ChangeClosed { .. } => {
            AppendPermission::LifecycleOwned
        }
        Payload::Unknown => AppendPermission::OpaqueImported,
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The announcement class of a `message` event. Deliberately excludes
/// `verdict` and `gate-result`: those already have native, policy-bearing
/// events, and duplicating them as messages would create two sources of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum MessageType {
    Status,
    Discovery,
    Note,
}

impl MessageType {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageType::Status => "status",
            MessageType::Discovery => "discovery",
            MessageType::Note => "note",
        }
    }
}

/// Advisory severity of a `message` event. Distinct from finding `Severity`:
/// a message announces, it never blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum MessageSeverity {
    Info,
    Warning,
    Error,
}

impl MessageSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageSeverity::Info => "info",
            MessageSeverity::Warning => "warning",
            MessageSeverity::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StageBudget {
    Launch,
    Started,
    SpecRead,
    Implementing,
    Verifying,
    BlockedOn,
    Snapshotted,
}

impl StageBudget {
    pub fn as_str(self) -> &'static str {
        match self {
            StageBudget::Launch => "launch",
            StageBudget::Started => "started",
            StageBudget::SpecRead => "spec-read",
            StageBudget::Implementing => "implementing",
            StageBudget::Verifying => "verifying",
            StageBudget::BlockedOn => "blocked-on",
            StageBudget::Snapshotted => "snapshotted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimStage {
    Started,
    SpecRead,
    Implementing,
    Verifying,
    BlockedOn,
    Snapshotted,
}

impl ClaimStage {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaimStage::Started => "started",
            ClaimStage::SpecRead => "spec-read",
            ClaimStage::Implementing => "implementing",
            ClaimStage::Verifying => "verifying",
            ClaimStage::BlockedOn => "blocked-on",
            ClaimStage::Snapshotted => "snapshotted",
        }
    }

    pub fn budget_key(self) -> StageBudget {
        match self {
            ClaimStage::Started => StageBudget::Started,
            ClaimStage::SpecRead => StageBudget::SpecRead,
            ClaimStage::Implementing => StageBudget::Implementing,
            ClaimStage::Verifying => StageBudget::Verifying,
            ClaimStage::BlockedOn => StageBudget::BlockedOn,
            ClaimStage::Snapshotted => StageBudget::Snapshotted,
        }
    }
}

/// Where a comment or finding attaches. Blob OIDs anchor to immutable
/// objects; line numbers alone drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anchor {
    pub path: String,
    pub side: Side,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineFinding {
    pub finding_id: String,
    pub blocking: bool,
    pub severity: Severity,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
}

/// Shape accepted by `arc review --findings-json`: finding IDs are
/// assigned by the CLI at write time.
#[derive(Debug, Clone, Deserialize)]
pub struct FindingInput {
    #[serde(default)]
    pub blocking: bool,
    pub severity: Severity,
    pub summary: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub anchor: Option<AnchorInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnchorInput {
    pub path: String,
    #[serde(default = "default_side")]
    pub side: Side,
    #[serde(default)]
    pub line_start: Option<u32>,
    #[serde(default)]
    pub line_end: Option<u32>,
    #[serde(default)]
    pub context: Option<String>,
}

fn default_side() -> Side {
    Side::Head
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    Base,
    Head,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Critical,
    Major,
    Minor,
    Note,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Approved,
    ChangesRequested,
    CommentOnly,
}

/// Reviewer-classified root causes for a requested-rework round.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewCause {
    /// The patchset faithfully exposed a missing, false, or ambiguous premise.
    Brief,
    /// The patchset violated a correct applicable brief.
    Executor,
    /// Later target work invalidated a correct brief and implementation.
    IntegrationStaleness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum DispositionStatus {
    Resolved,
    AcceptedRisk,
    Obsolete,
    StillOpen,
    Disputed,
}

impl DispositionStatus {
    /// Statuses that release a blocking finding.
    pub fn releases_block(self) -> bool {
        matches!(
            self,
            DispositionStatus::Resolved
                | DispositionStatus::AcceptedRisk
                | DispositionStatus::Obsolete
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Closure {
    Integrated,
    Abandoned,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum VerifyResult {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationRunMode {
    Sequential,
    Parallel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationRunGate {
    pub name: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ProbePhase {
    Baseline,
    Final,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeEvidenceRef {
    pub brief_event_id: String,
    pub name: String,
    pub phase: ProbePhase,
}
