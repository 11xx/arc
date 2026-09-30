use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The store format this build writes and understands. A build refuses a
/// store stamped newer than this, because the alternative is what silently
/// went wrong before: an older binary skipping event types it does not know,
/// concluding the change is still open, and closing it a second way.
pub const SCHEMA_VERSION: u32 = 6;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisplacedClaim {
    pub claim_id: String,
    pub actor: String,
    pub harness: String,
    pub session: String,
    pub stage: String,
    /// Why the lease was cut short, as the caller wrote it. Present when a
    /// takeover displaced a claim the rules would otherwise have protected;
    /// absent when the claim was already stale or expired and no reason was
    /// given. Arc records the text and verifies nothing: the evidence it
    /// names lives outside the ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BriefRef {
    pub event_id: String,
}

/// A journal artifact a patchset was framed by.
///
/// The link carries identifiers and a digest, never the artifact's text: a
/// reader follows the filename to the store that holds it, and the digest
/// says whether the body is still the one that framed the work. Nothing here
/// promises the file survives — keeping a recording is not arc's promise.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalArtifactRef {
    /// The artifact's filename inside the journal directory.
    pub file: String,
    /// `sha256:` over the body read when the link was recorded.
    pub digest: String,
}

/// Where work was discussed, as identifiers arc stores and never resolves.
///
/// A thread belongs to some external system — a remote UI, a chat service, a
/// forge issue tracker. Arc records the scheme and the id so a reader can
/// take them to that system, and makes no network call, fetches no URL, and
/// reads no transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalThreadRef {
    /// The system the thread belongs to, e.g. `t3` or `github`.
    pub scheme: String,
    /// The thread's identifier within that system, opaque to arc.
    pub id: String,
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

/// A planner coordinate carried by a portable plan header and its typed
/// journal event. Missing coordinates are genuinely unavailable and are
/// omitted rather than replaced with an invented identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannerIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl PlannerIdentity {
    pub fn key(
        &self,
    ) -> (
        &Option<String>,
        &Option<String>,
        &Option<String>,
        &Option<String>,
    ) {
        (&self.actor, &self.harness, &self.session, &self.model)
    }
}

/// The immutable plan bytes and planner metadata captured by a selected
/// brief. A later edit to the source plan cannot change this snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSource {
    pub anchor: Option<String>,
    pub journal_dir: String,
    pub filename: String,
    pub slice: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub planners: Vec<PlannerIdentity>,
    pub planner_status: String,
    pub provenance_basis: String,
}

impl PlanSource {
    pub fn planner_trailers(&self) -> Vec<String> {
        if !matches!(self.planner_status.as_str(), "ok" | "corrected") {
            return Vec::new();
        }
        let mut lines: Vec<_> = self
            .planners
            .iter()
            .filter_map(|p| {
                let harness = p.harness.as_deref()?;
                let model = p.model.as_deref()?;
                if harness.contains(['\n', '\r']) || model.contains(['\n', '\r']) {
                    return None;
                }
                Some(format!("Assisted-by: {harness}:{model} (planner)"))
            })
            .collect();
        lines.sort();
        lines.dedup();
        lines
    }
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
    /// The checkout's `git config user.name` when `actor` was derived from the
    /// harness session: whoever configured the checkout the agent ran in, not
    /// a claim about who acted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    /// The subject an action is performed for when a lead runs delegated
    /// ceremony. `actor` stays the invoker who ran the command; the effective
    /// author of the event is `on_behalf_of.unwrap_or(actor)`. Additive:
    /// serialized only when set, so old events and bundles round-trip intact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    /// Model identity declared for the invocation. Missing on events written
    /// before model identity was recorded, which remains unrecorded rather
    /// than becoming an inferred value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Private acting-session link. Ledger and journal records may carry it;
    /// Git commits, changelogs, trailers, and forge text must not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_link: Option<String>,
    /// What the harness's own store said about `session` when the event was
    /// written. A detected session id arrives from the environment and can
    /// name no session the harness ever wrote; this is the record that the
    /// store backed it, or did not. Absent when detection did not run, which
    /// is the absence of a lookup rather than a claim about the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_resolution: Option<SessionResolution>,
    pub created_at: DateTime<Utc>,
    #[serde(flatten)]
    pub payload: Payload,
}

/// What a harness's own session store said about a session id.
///
/// A session id reaches arc through the environment, so it can be a stale
/// export, a nested harness, or a wrapper passing its environment down. The
/// store is the only witness that the id names a session the harness wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionResolution {
    /// The store resolved a recording for the id.
    Corroborated,
    /// No recording for the id resolved in the store.
    Uncorroborated,
    /// The lookup or read could not decide whether the store holds the id.
    Unresolved,
}

/// How the acting identity on an event was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActorSource {
    /// Declared on the command line.
    Flag,
    /// Declared through `ARC_ACTOR`.
    Env,
    /// Nobody declared one and no harness session was known, so arc used
    /// `git config user.name`. This is the Git identity of whoever configured
    /// the checkout, not a claim about who acted.
    GitFallback,
    /// Nobody declared one, so arc named the harness session the command ran
    /// in, as `<harness>:<session>`. It names the acting agent session, but
    /// it is arc's inference from the environment, not anybody's claim.
    Derived,
}

impl ActorSource {
    /// Whether someone offered this identity, as opposed to arc inventing it. An assumed one names a person
    /// who never said they did anything.
    pub fn declared(self) -> bool {
        matches!(self, Self::Flag | Self::Env)
    }

    /// Where arc took an identity nobody declared, for the lines that announce
    /// or refuse one.
    pub fn assumed_from(self) -> Option<&'static str> {
        match self {
            Self::Flag | Self::Env => None,
            Self::GitFallback => Some("git config user.name"),
            Self::Derived => Some("the harness session"),
        }
    }
}

/// What kind of fact was kept. The distinction is the point: these decay
/// differently and are worth different amounts on resume. A rejected approach
/// is the highest-value one and the least likely to be re-derived, because a
/// cold session will cheerfully try it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeptKind {
    /// A premise checked rather than assumed.
    Verified,
    /// An approach tried and abandoned, and why.
    Rejected,
    /// Something the work discovered it must respect.
    Constraint,
    /// Believed but not established. Recorded as a guess, so a later reader
    /// cannot mistake it for a finding.
    Hypothesis,
}

impl KeptKind {
    pub fn as_str(self) -> &'static str {
        match self {
            KeptKind::Verified => "verified",
            KeptKind::Rejected => "rejected",
            KeptKind::Constraint => "constraint",
            KeptKind::Hypothesis => "hypothesis",
        }
    }
}

/// What a declared debt says was missing.
///
/// The kind is the weight, carried as a label rather than a number: a deficit
/// is a different obligation depending on what was read, and one count over
/// every debt says only how many exist. Declaration order is severity order,
/// least covered first, which is the order a queue works through them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum DebtMissing {
    /// No verdict on any patchset of the change.
    NothingRead,
    /// An approved patchset, then a merge or rebase resolution nobody read.
    MergeResolutionUnread,
    /// An approved patchset, then authored work nobody read.
    RepairUnread,
    /// Verdicts on the shipped patchset, all of them from its contributors.
    ContributorOnly,
    /// A read by somebody independent, which nobody supplied. Also what an
    /// obligation recorded before kinds existed means.
    IndependentReview,
}

impl DebtMissing {
    /// Every kind, in severity order. A count split over this covers every
    /// obligation exactly once, so a split and a total cannot disagree.
    pub const ALL: [DebtMissing; 5] = [
        DebtMissing::NothingRead,
        DebtMissing::MergeResolutionUnread,
        DebtMissing::RepairUnread,
        DebtMissing::ContributorOnly,
        DebtMissing::IndependentReview,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NothingRead => "nothing-read",
            Self::MergeResolutionUnread => "merge-resolution-unread",
            Self::RepairUnread => "repair-unread",
            Self::ContributorOnly => "contributor-only",
            Self::IndependentReview => "independent-review",
        }
    }
}

/// The effort a model string names in a trailing `#suffix`, when it names one.
///
/// A reading of the string, never a replacement for it: routing writes model
/// and effort as one token, and the whole token is what was recorded.
pub fn model_effort(model: &str) -> Option<&str> {
    model
        .rsplit_once('#')
        .map(|(_, effort)| effort)
        .filter(|effort| !effort.is_empty())
}

/// One verdict's recorded review identity and the coordinates it was cast at.
///
/// Coordinates only: no routing tier is derived from the identity or from any
/// configuration, and no ordering is implied between two of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebtCoverage {
    pub reviewer: String,
    /// Absent when the verdict event recorded no model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The effort named by the model string's trailing `#suffix`, when it
    /// carries one. The model string above stays whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The routing version that selected the reviewer. Absent means unrouted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_version: Option<String>,
}

impl DebtCoverage {
    /// Coverage from one recorded review identity, reading the effort off the
    /// model string.
    pub fn new(reviewer: String, model: Option<String>, route_version: Option<String>) -> Self {
        let effort = model.as_deref().and_then(model_effort).map(str::to_owned);
        DebtCoverage {
            reviewer,
            model,
            effort,
            route_version,
        }
    }
}

/// One identity at the coordinates arc keeps for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebtIdentity {
    pub actor: String,
    /// Subject the work was recorded for, when a lead ran delegated ceremony.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The effort named by the model string's trailing `#suffix`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

impl DebtIdentity {
    pub fn new(
        actor: String,
        on_behalf_of: Option<String>,
        harness: Option<String>,
        model: Option<String>,
        session: Option<String>,
    ) -> Self {
        let effort = model.as_deref().and_then(model_effort).map(str::to_owned);
        DebtIdentity {
            actor,
            on_behalf_of,
            harness,
            model,
            effort,
            session,
        }
    }

    /// The identity policy attributes the work to: the subject when one was
    /// named, otherwise the invoker.
    pub fn effective_actor(&self) -> &str {
        self.on_behalf_of.as_deref().unwrap_or(&self.actor)
    }
}

/// How the work a debt covers was produced: who set the contract and who
/// answered it.
///
/// Coordinates, not a ranking. Whether a planner outranks an implementer, or
/// either model outranks the other, is a judgment arc does not make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebtProduction {
    /// Who recorded the brief version the shipped work answered. Absent when
    /// the change carries no brief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner: Option<DebtIdentity>,
    /// The brief version current at the shipped patchset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_version: Option<usize>,
    /// Who recorded the shipped patchset.
    pub implementer: DebtIdentity,
    /// Whether a brief exists and somebody other than its author implemented
    /// it. False for unbriefed work and for work its own planner wrote.
    pub following_brief: bool,
}

/// Where a promoted change's work came from: the fork it was taken from at
/// the moment of promotion, recorded so a later reader can tell whether the
/// fork moved afterwards. It is a link, not an obligation: no review credit
/// crosses it, and the fork is neither consumed nor obligated by it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForkProvenance {
    /// The fork's slug, as the repository recorded it.
    pub slug: String,
    /// The fork branch the work was read from.
    pub branch: String,
    /// The revision the fork's own commits start from: their merge base with
    /// the change's target when the promotion ran.
    pub base: String,
    /// The fork head the work was taken from.
    pub head: String,
    /// The tree of that head, so a reader can tell the exact content the
    /// promotion read even after the fork branch moves.
    pub tree: String,
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
        /// The fork `begin --from-fork` promoted work from. A recorded link
        /// and nothing more: it grants no review credit, creates no
        /// obligation on the fork or the change, and is not a lifecycle
        /// state. Additive: absent for changes not begun via `--from-fork`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_fork: Option<ForkProvenance>,
        /// Raised at `begin` to demand an independent verdict whatever the
        /// change turns out to touch. One-way: nothing lowers it later.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        dangerous: bool,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_source: Option<PlanSource>,
    },
    ChangelogRecorded {
        #[serde(alias = "section")]
        category: String,
        body: String,
        /// The changelog entry this one replaces, when it supersedes an
        /// earlier projection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supersedes: Option<String>,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        contributors: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim_actor: Option<String>,
        /// Journal artifacts recorded as framing this patchset, each with the
        /// body digest read when the link was recorded.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        journal_refs: Vec<JournalArtifactRef>,
        /// An external thread this work belongs to, as a scheme and an id.
        /// Arc stores the pair and never resolves or fetches it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread: Option<ExternalThreadRef>,
    },
    /// An explicit contributor declaration for one patchset. The patchset's
    /// contributor set may change only before its first verdict.
    #[serde(alias = "patchset-contributors-amended")]
    PatchsetAttributionAmended {
        patchset_id: String,
        contributors: Vec<String>,
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
    /// A fact a session judged load-bearing while the work was happening.
    ///
    /// Compaction is lossy compression chosen by something that does not know
    /// what will be needed; the session doing the work does. `arc resume`
    /// hands these back, so a compacted or cold session does not re-derive
    /// them — or, worse, re-try an approach already rejected.
    ContextKept {
        kind: KeptKind,
        body: String,
        /// What established it. Absent when the caller offered none, which is
        /// itself worth seeing: a fact with no evidence reads as a claim.
        #[serde(skip_serializing_if = "Option::is_none")]
        evidence: Option<String>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_event_id: Option<String>,
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
        /// How this verdict relates to the verdict tips it observed. A
        /// corroboration leaves those tips authoritative; a supersession
        /// replaces them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relation: Option<VerdictRelation>,
        /// Why this verdict is owed corroboration, when the caller says it is.
        ///
        /// A verdict answers "what did the reviewer conclude". This answers a
        /// different question — "should that conclusion be relied on yet" —
        /// and the two were previously collapsed, so a reviewer whose
        /// judgment had never been validated discharged the gate exactly as
        /// one whose judgment had.
        ///
        /// The caller asserts it; arc never infers it. Deciding which
        /// reviewers are proven would mean holding a roster, which is the
        /// routing opinion arc does not have.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provisional: Option<String>,
        /// The routing version that selected this reviewer, as the caller
        /// declared it. Absent means the review was unrouted: arc never infers
        /// a version, because knowing which roster produced an identity would
        /// mean holding the roster.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route_version: Option<String>,
    },
    /// A verdict copied from a decision made outside Arc, with its source and
    /// exact revision kept apart from witnessed review.
    ExternalVerdictRecorded {
        revision: String,
        verdict: ExternalVerdict,
        decided_by: String,
        reference: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        findings: Vec<ExternalFinding>,
    },
    /// A review obligation this change carries but has not discharged.
    ///
    /// Declaring it is what lets a change integrate without an independent
    /// verdict: the requirement is not waived, it is recorded as debt that
    /// `arc query --debt` can find after the reviewer becomes available.
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
    /// A debt: what review a change was missing when it shipped, and what
    /// review it did have. Named for what it records rather than for the
    /// ceremony that discharges it — an audit is one of two things that can,
    /// and the record is ordinary rather than an exception. The untyped
    /// `AuditDebtDeclared` above is what earlier builds wrote, and keeps its
    /// event type forever so nothing already recorded is lost.
    #[serde(rename = "debt-declared")]
    DebtDeclared {
        reason: String,
        /// The patchset the waiver applies to, when it was declared before
        /// integration.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patchset_id: Option<String>,
        missing: DebtMissing,
        /// Every verdict recorded on the bound patchset at declaration time.
        #[serde(default)]
        coverage: Vec<DebtCoverage>,
        /// Who planned and who implemented the work this debt covers. Absent
        /// when no patchset was bound, and on obligations recorded before
        /// production was kept.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        production: Option<DebtProduction>,
    },
    /// Dirty-tree evidence allowed to count, for one revision.
    ///
    /// Dirt stays fatal by default: evidence from a tree no checkout
    /// reproduces is recorded and declines to count. The exception is declared
    /// rather than assumed, and binds the way the thing it excuses binds —
    /// gate evidence counts only at the change's own head, so the waiver
    /// covers exactly that revision and dies at the next commit.
    DirtyTreeWaived {
        reason: String,
        /// The revision whose evidence this waives, which is the head the
        /// waiver was declared at.
        revision: String,
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
        /// The routing version that selected this auditor, as the caller
        /// declared it. Absent means the audit was unrouted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route_version: Option<String>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_event_id: Option<String>,
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
        /// The timeout the gate was declared with when this ran. A run under a
        /// laxer timeout is not evidence for a stricter one, and `None` — on
        /// events written before this was recorded, or a gate declaring no
        /// timeout — is unknown rather than unlimited.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_seconds: Option<u64>,
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
        /// The environment this evidence was produced in, when the gate
        /// declared an environment probe. Absent on a gate that declares
        /// none, and on evidence written before arc recorded environments,
        /// which satisfies only probe-less gates.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment: Option<EnvironmentEvidence>,
        /// Optional free-form note recorded alongside the evidence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// The failure this evidence answers. Absent means the check was never
        /// recorded failing before it passed — which is the ordinary case, and
        /// is unknown rather than a claim the check cannot discriminate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        falsification: Option<Falsification>,
        /// The failure arc derived this pass follows, recorded whether or not
        /// one was declared. Absent on a failure, on probe evidence, on a pass
        /// no earlier failure of the same check precedes, and on evidence
        /// written before arc derived it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        falsification_inferred: Option<InferredFalsification>,
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
        /// Which kind of dirt, recorded separately from whether there was any.
        /// One bool could not say, so the premise that this wedges
        /// overwhelmingly on untracked-only dirt was unmeasurable and a waiver
        /// reason had to be written from memory. Absent on evidence recorded
        /// before the split, which is not the same as clean.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_dirty_tracked: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_dirty_untracked: Option<bool>,
        /// The worktree changed while the command ran, so the evidence
        /// describes no single tree.
        #[serde(default, skip_serializing_if = "is_false")]
        tree_moved: bool,
        /// The tree of the revision this evidence names.
        ///
        /// A gate holds for content, and content is what a merge carries
        /// forward; the revision is the commit that happened to hold it. Two
        /// commits with one tree are one evaluation, and a merge produces a
        /// tree neither side committed. Absent on evidence written before arc
        /// recorded it, where the revision resolves it as long as the commit
        /// is reachable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tree: Option<String>,
        /// The target head the evaluated tree was computed against, on
        /// evidence for a synthesized merge. A merge with a moved target is a
        /// different merge, so naming the target says both what this evidence
        /// is about and when it stops applying.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        against_target: Option<String>,
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
        /// The `HoldSet` event this releases. Absent only on events written
        /// before holds had identity, where it keeps its historical
        /// release-everything meaning.
        #[serde(skip_serializing_if = "Option::is_none")]
        hold_event_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A terminal outcome arc did not merge itself. New writes carry only
    /// `abandoned` and `superseded`; an `integrated` outcome here is history
    /// from before arc distinguished a guarded merge from an assertion.
    ChangeClosed {
        outcome: Closure,
        #[serde(skip_serializing_if = "Option::is_none")]
        integrated_commit: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        superseded_by: Option<String>,
    },
    /// A merge arc performed under its own guard: the head equalled the
    /// approved patchset head, gates were green at it, no finding blocked and
    /// no hold was active, and — when the merge existed at all — the merge
    /// commit's parents were verified. `already_contained` marks the case
    /// where the target already held the head and no merge was made.
    ChangeIntegrated {
        integrated_commit: String,
        source_patchset_id: String,
        source_head: String,
        target_branch: String,
        target_before: String,
        /// Everything the guard consumed to authorize this one irreversible
        /// decision. Absent only on events written before arc recorded it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authorization: Option<AuthorizationBasis>,
        /// The target already contained the approved head when the guard ran,
        /// so Git created no merge commit: `integrated_commit` is the target
        /// revision that already held it, and `target_before` equals it. A
        /// reader must not look for merge parents. False means
        /// `integrated_commit` is the merge arc made.
        #[serde(default, skip_serializing_if = "is_false")]
        already_contained: bool,
    },
    /// An integration somebody performed elsewhere and asserted afterwards.
    /// Deliberately carries no authorization: arc did not guard this merge and
    /// cannot claim it was authorized, only that it was asserted.
    /// A contributed change passed every integration check at this head and
    /// is ready to go to its receiver. Nothing was merged: the receiver's
    /// decision closes the change later.
    ReadyToSend {
        patchset_id: String,
        head: String,
        /// The history shape the head was checked against.
        history: String,
        authorization: AuthorizationBasis,
    },
    IntegrationAsserted {
        integrated_commit: String,
        source_patchset_id: String,
        source_head: String,
        target_branch: String,
        /// Where the target stood first, read from the asserted revision's
        /// first parent. Absent when it has none, which is the only case
        /// where there was no prior target state to name.
        #[serde(skip_serializing_if = "Option::is_none")]
        target_before: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_reference: Option<String>,
    },
    /// The author declares whether this change is being iterated on rather
    /// than driven to a merge. An iterating change records progress with
    /// snapshots and declared debt, and integrates only once the
    /// declaration is cleared.
    IterationScopeSet {
        iterating: bool,
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
    /// A Git history rewrite this repository underwent. Nothing already
    /// written changes: an old event keeps saying exactly what it said, and
    /// readers follow a recorded revision forward through this. The mapping
    /// is either arc's own output or one the operator supplied, and both are
    /// judged by the same rules before being recorded.
    HistoryRewritten {
        /// Old revision to its replacement, or to nothing when the rewrite
        /// dropped the commit entirely. A revision the rewrite left alone is
        /// absent: that is not a move.
        mapping: std::collections::BTreeMap<String, Option<String>>,
        reason: String,
        /// What performed the rewrite, when the operator says so.
        #[serde(skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        /// The old revisions, spelled as in `mapping`, whose successor
        /// differs from them in nothing but its signature: the same tree,
        /// author, committer, message and other headers, with parents
        /// differing only as the rewrite translates them. An approval follows
        /// a rewrite only through these. It is judged by whoever records the
        /// event, while both commits exist, so an old commit that is gone
        /// leaves its entry out rather than assumed. Events written without
        /// it verify nothing.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        signature_only: Vec<String>,
    },
    /// A caller-declared review pass over exact change and patchset members.
    /// The declaration records coverage only; it grants no authority to any
    /// gate and arc does not observe the review itself.
    ReviewPassOpened {
        pass_id: String,
        members: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// A caller-declared successful ending for a review pass.
    ReviewPassCompleted {
        pass_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// A caller-declared abandoned ending for a review pass.
    ReviewPassAbandoned {
        pass_id: String,
        reason: String,
    },
    /// A caller told arc that a delegated run was dispatched through a
    /// resolved route. arc records the dispatch context but does not choose,
    /// start, or supervise the run.
    ///
    /// Exactly one subject is named. A ledger change is one shape a delegation
    /// takes, not the only one: the loop of bounded rounds runs just as often
    /// on a fork, which is outside the lifecycle by design, or on a bare
    /// commit range in a repository whose ledger holds nothing yet. A record
    /// that could bind only to a change would be unavailable to exactly the
    /// work that most needs it.
    RunDispatched {
        route: String,
        worktree: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        change: Option<String>,
        /// The fork slug this run was dispatched against.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fork: Option<String>,
        /// The commit range this run was dispatched against.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        range: Option<CommitRange>,
        #[serde(skip_serializing_if = "Option::is_none")]
        brief_event_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// A caller told arc that a dispatched run reached a terminal outcome.
    /// `unknown` records that no more specific outcome is known.
    ///
    /// A round additionally carries what it read and what it produced: the
    /// head it reviewed, the findings it raised, the findings it deliberately
    /// left, and the earlier deferrals it collected. A deferral recorded
    /// nowhere is a deferral nobody will honor, so the ending that made the
    /// decision is where it is written down.
    RunEnded {
        dispatch_event_id: String,
        outcome: RunOutcome,
        /// The revision the round reviewed, when the caller names one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reviewed_head: Option<String>,
        /// Findings the round raised for repair.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        raised: Vec<RunFinding>,
        /// Findings the round decided not to fix, each with the reason.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deferred: Vec<DeferredFinding>,
        /// Deferral IDs this round took up. A deferral is open until a later
        /// round on the same subject collects it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        collects: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// An event whose `event_type` this build does not recognize (e.g. one
    /// imported from a newer arc). Typed loading skips these entries; the
    /// underlying files and raw export preserve their original bytes intact.
    #[serde(other)]
    Unknown,
}

/// The inputs to one guarded merge, recorded on the event that performed it.
///
/// An auditor could otherwise only replay preceding events and recover
/// project policy from Git. Operator policy is local to the shared Git
/// directory, so the declaration sources used by one decision are recorded
/// beside its normalized values. This does not make the ledger a config store:
/// arc records no configuration history, only the inputs to one decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalVerdictBasis {
    pub event_id: String,
    pub revision: String,
    pub verdict: ExternalVerdict,
    pub decided_by: String,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationBasis {
    /// The verdict that approved the merged patchset, when one did. Absent
    /// when a declared debt stood in for a review nobody performed: the
    /// merge then rests on `audit_debt_event_id` alone, and saying so is more
    /// honest than naming a verdict that does not exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_event_id: Option<String>,
    /// External approval consumed for this merge, kept apart from verdicts
    /// Arc witnessed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_verdict: Option<ExternalVerdictBasis>,
    /// One passing verification per resolved required gate, by gate name.
    pub gate_evidence: std::collections::BTreeMap<String, String>,
    /// Each prerequisite change and the closure that satisfied it.
    pub prerequisites: Vec<PrerequisiteClosure>,
    /// Vectors that had to be empty for this event to be written at all.
    /// Recorded rather than implied, so an auditor reads a checked fact
    /// instead of inferring one from an absence.
    pub blocking_findings: Vec<String>,
    pub holds: Vec<String>,
    /// The normalized gate declarations consumed, by gate name.
    pub gates: std::collections::BTreeMap<String, NormalizedGate>,
    /// The normalized policy values consumed.
    pub policy: NormalizedPolicy,
    /// The danger determination consumed by the guard. Absent on events
    /// written before this fact was recorded; absence is not a safe result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub danger: Option<DangerScope>,
    /// The debt declaration that stood in for an absent verdict or made
    /// a self-approved merge eligible. It is an authorization input like the
    /// verdict: without it the merge would have been refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_debt_event_id: Option<String>,
    /// Why the authorizing verdict was owed corroboration, when it was.
    ///
    /// Without it an auditor reading this basis sees a verdict event id and
    /// concludes the merge was reviewed, with nothing to distinguish a
    /// reviewer whose judgment had been validated from one whose had not.
    /// The obligation must be legible from the record of the merge itself,
    /// not only from the verdict it points at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_provisional: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrerequisiteClosure {
    pub change_id: String,
    pub closure_event_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integrated_commit: Option<String>,
}

/// Whether a change touches a surface the project declared dangerous, and
/// therefore whether its verdict must come from somebody other than its
/// author.
///
/// A change may raise itself to dangerous and may never lower itself below
/// what config declares: escalation is a judgement anyone may make, while
/// de-escalation would let the party under shipping pressure decide its own
/// gate, which is the pressure the declaration exists to resist.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DangerScope {
    /// Whether an independent verdict is required for this change.
    pub dangerous: bool,
    /// Why, in a form `arc check` can name.
    pub rule: DangerRule,
    /// The touched paths that matched a declared pattern.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DangerRule {
    /// The project declared no dangerous surfaces, so the gate is uniform.
    NotDeclared,
    /// Touched paths matched a declared pattern.
    DeclaredPath,
    /// The change raised itself at `begin`.
    Escalated,
    /// Nothing the project declared was touched.
    Untouched,
    /// The touched set could not be established; assumed dangerous.
    Undetermined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedGate {
    pub command: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub profiles: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(default)]
    pub declared_by: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedPolicy {
    pub forbid_self_approval: bool,
    pub require_declared_actor: bool,
    pub provenance_git_identity: String,
    #[serde(default)]
    pub declared_by: std::collections::BTreeMap<String, Vec<String>>,
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
        | Payload::PatchsetAttributionAmended { .. }
        | Payload::ClaimSet { .. }
        | Payload::StageSet { .. }
        | Payload::FindingAdded { .. }
        | Payload::DispositionRecorded { .. }
        | Payload::VerdictRecorded { .. }
        | Payload::ExternalVerdictRecorded { .. }
        | Payload::VerificationRunStarted { .. }
        | Payload::VerificationRecorded { .. }
        | Payload::VerificationReused { .. }
        // Waiving dirt excuses a gate, and a closed change has no gate left
        // to excuse.
        | Payload::DirtyTreeWaived { .. }
        | Payload::HoldSet { .. }
        | Payload::ReadyToSend { .. }
        | Payload::ForgeProjection { .. } => AppendPermission::OpenOnly,
        Payload::Message { .. }
        | Payload::ClaimReleased { .. }
        | Payload::ContextKept { .. }
        | Payload::CommentAdded { .. }
        | Payload::ReplyAdded { .. }
        | Payload::HoldReleased { .. }
        | Payload::ForgeLink { .. }
        | Payload::ForgeChecks { .. }
        | Payload::ForgePrState { .. } => AppendPermission::AnyPhaseFact,
        Payload::ChangelogRecorded { .. }
        | Payload::AuditDebtDeclared { .. }
        | Payload::DebtDeclared { .. } => {
            AppendPermission::OpenOrIntegratedFact
        }
        Payload::AuditVerdictRecorded { .. }
        | Payload::AuditFindingAdded { .. }
        | Payload::AuditDispositionRecorded { .. } => AppendPermission::IntegratedOnlyFact,
        Payload::ChangeOpened { .. }
        | Payload::ChangeClosed { .. }
        | Payload::ChangeIntegrated { .. }
        | Payload::IntegrationAsserted { .. }
        | Payload::IterationScopeSet { .. } => AppendPermission::LifecycleOwned,
        // Repository-scoped: it is never appended to a change's log, so no
        // change-phase policy applies to it.
        Payload::HistoryRewritten { .. }
        | Payload::ReviewPassOpened { .. }
        | Payload::ReviewPassCompleted { .. }
        | Payload::ReviewPassAbandoned { .. } => AppendPermission::AnyPhaseFact,
        | Payload::RunDispatched { .. }
        | Payload::RunEnded { .. } => AppendPermission::AnyPhaseFact,
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

/// A finding copied from an external review record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalFinding {
    pub finding_id: String,
    pub blocking: bool,
    pub severity: Severity,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<AnchorInput>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// A decision supplied by a receiver outside the local review ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalVerdict {
    Approved,
    ChangesRequested,
    Rejected,
}

/// The relationship between a verdict and the verdicts it observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum VerdictRelationKind {
    /// Supports the observed verdicts without replacing them.
    Corroborates,
    /// Replaces the observed verdict tips.
    Supersedes,
}

/// A typed verdict edge that records both its meaning and the events it saw.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum VerdictRelation {
    Corroborates { observed: Vec<String> },
    Supersedes { observed: Vec<String> },
}

impl VerdictRelationKind {
    pub fn with_observed(self, observed: Vec<String>) -> VerdictRelation {
        match self {
            Self::Corroborates => VerdictRelation::Corroborates { observed },
            Self::Supersedes => VerdictRelation::Supersedes { observed },
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Corroborates => "corroborates",
            Self::Supersedes => "supersedes",
        }
    }
}

impl VerdictRelation {
    pub fn kind(&self) -> VerdictRelationKind {
        match self {
            Self::Corroborates { .. } => VerdictRelationKind::Corroborates,
            Self::Supersedes { .. } => VerdictRelationKind::Supersedes,
        }
    }

    pub fn observed(&self) -> &[String] {
        match self {
            Self::Corroborates { observed } | Self::Supersedes { observed } => observed,
        }
    }

    pub fn description(&self) -> String {
        let observed = self.observed();
        if observed.is_empty() {
            self.kind().as_str().to_string()
        } else {
            format!("{} {}", self.kind().as_str(), observed.join(", "))
        }
    }
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
pub enum RunOutcome {
    Completed,
    RefusedOnPremise,
    Stopped,
    Unknown,
}

impl RunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::RefusedOnPremise => "refused-on-premise",
            Self::Stopped => "stopped",
            Self::Unknown => "unknown",
        }
    }
}

/// A base and a head, as `<base>..<head>` is written on the command line.
/// Both ends are recorded verbatim: arc does not resolve, shorten, or verify
/// them, so a range naming revisions this repository never held is a record
/// that is wrong rather than one that is missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRange {
    pub base: String,
    pub head: String,
}

impl CommitRange {
    /// Parse the `<base>..<head>` form, refusing anything with an empty end.
    pub fn parse(value: &str) -> std::result::Result<Self, String> {
        let Some((base, head)) = value.split_once("..") else {
            return Err(format!("range {value:?} is not <base>..<head>"));
        };
        let (base, head) = (base.trim(), head.trim());
        if base.is_empty() || head.is_empty() {
            return Err(format!("range {value:?} needs a base and a head"));
        }
        Ok(Self {
            base: base.to_string(),
            head: head.to_string(),
        })
    }

    pub fn as_str(&self) -> String {
        format!("{}..{}", self.base, self.head)
    }
}

/// One finding a round raised. Severity is optional because a round that
/// names what it found has said the useful part; ranking it is a second
/// judgment nobody is obliged to make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFinding {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
}

/// One finding a round chose not to fix.
///
/// The reason is required. A deferral without one is indistinguishable from a
/// finding that was missed, and the next round inherits no way to tell whether
/// the decision still holds. The ID is what a later round collects it by, and
/// arc mints one when the caller supplies none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredFinding {
    pub id: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
    pub why: String,
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

/// The environment a verification ran in, as its gate's declared probe
/// identifies it.
///
/// A receipt from one environment says nothing about another unless the two
/// are known to be the same. The probe's output is the identity evidence
/// carries, and the store that recorded it is provenance that travels with
/// the record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentEvidence {
    /// The digest the declared probe yielded: `sha256:` over its combined
    /// output. Opaque to arc; only equality between digests is ever read.
    pub identity: String,
    /// The `repository_id` of the store that recorded the evidence. On
    /// attested evidence this names the recording store, not a runner arc
    /// has not been told about.
    pub producing_store: String,
}

/// The observed failure that a passing verification answers.
///
/// A gate that passed is not evidence that the gate could have failed. A check
/// watched to fail for a stated reason, then to pass once that reason was
/// removed, discriminates; a check first run after the code it checks does
/// not. In a record of passes alone the two are identical, and this reference
/// is what separates them. It is advisory: no readiness decision, gate result,
/// or exit code depends on its presence.
///
/// The reason is predicted, not observed. A reason read off the failure
/// afterwards restates the output; one stated beforehand is a claim that could
/// have turned out wrong.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Falsification {
    /// A failing `VerificationRecorded` event on the same change, carrying the
    /// same gate name or the same command.
    pub event_id: String,
    /// The revision that event was recorded at. Carried here so the reference
    /// reads without resolving it, and checked against the event on the way in
    /// so the two can never disagree.
    pub revision: String,
    /// Why the check was expected to fail, stated before it ran.
    pub predicted_reason: String,
}

/// The failure a pass follows, derived by arc rather than declared.
///
/// It is provenance, never a claim: it decides no gate result, readiness,
/// discrimination, or exit code, and it never stands in for a declared
/// [`Falsification`], which alone carries a predicted reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferredFalsification {
    /// The newest failing `VerificationRecorded` event on the same change,
    /// before this one, of the same gate, or of the same command when the
    /// evidence names no gate.
    pub event_id: String,
    /// The revision that event was recorded at.
    pub revision: String,
    /// The rule that derived the reference.
    pub source: InferenceSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InferenceSource {
    /// The newest earlier failure of the same check on the same change.
    PriorFailureSameChange,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispositions_without_evidence_event_id_still_deserialize() {
        for event_type in ["disposition-recorded", "audit-disposition-recorded"] {
            let event: Event = serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "event_id": "01J00000000000000000000000",
                "repository_id": "repo",
                "change_id": "change",
                "actor": "tester",
                "created_at": "2026-08-26T00:00:00Z",
                "event_type": event_type,
                "finding_id": "f1",
                "status": "resolved",
                "commit": null,
                "evidence": null,
                "supersedes": []
            }))
            .unwrap();
            assert!(matches!(
                event.payload,
                Payload::DispositionRecorded {
                    evidence_event_id: None,
                    ..
                } | Payload::AuditDispositionRecorded {
                    evidence_event_id: None,
                    ..
                }
            ));
        }
    }
}
