use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u32 = 1;

/// One append-only ledger entry. The envelope is common to every event;
/// the payload is internally tagged by `event_type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: u32,
    pub event_id: String,
    pub repository_id: String,
    pub change_id: String,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(flatten)]
    pub payload: Payload,
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
    },
    PatchsetAdded {
        patchset_id: String,
        base: String,
        head: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        merge_base: Option<String>,
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
    },
    ClaimReleased {
        claim_id: String,
    },
    StageSet {
        claim_id: String,
        stage: ClaimStage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
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
        /// Findings recorded atomically with the verdict (one review, one event).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        findings: Vec<InlineFinding>,
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
        /// Optional free-form note recorded alongside the evidence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
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
    /// Observed PR lifecycle. `merged` carries the actual merge commit.
    ForgePrState {
        state: crate::forge::ForgePrState,
        #[serde(skip_serializing_if = "Option::is_none")]
        merge_sha: Option<String>,
    },
    /// An event whose `event_type` this build does not recognize (e.g. one
    /// imported from a newer arc). Typed loading skips these entries; the
    /// underlying files and raw export preserve their original bytes intact.
    #[serde(other)]
    Unknown,
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
}

impl StageBudget {
    pub fn as_str(self) -> &'static str {
        match self {
            StageBudget::Launch => "launch",
            StageBudget::Started => "started",
            StageBudget::SpecRead => "spec-read",
            StageBudget::Implementing => "implementing",
            StageBudget::Verifying => "verifying",
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

    pub fn budget_key(self) -> Option<StageBudget> {
        match self {
            ClaimStage::Started => Some(StageBudget::Started),
            ClaimStage::SpecRead => Some(StageBudget::SpecRead),
            ClaimStage::Implementing => Some(StageBudget::Implementing),
            ClaimStage::Verifying => Some(StageBudget::Verifying),
            ClaimStage::BlockedOn | ClaimStage::Snapshotted => None,
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
