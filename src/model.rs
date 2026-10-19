use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One operation's outcome, including whether its durable admission published.
#[derive(Debug, Clone, Serialize)]
pub struct OperationResult<T> {
    pub exit_code: i32,
    pub published: bool,
    pub output: T,
}
impl<T: Default> OperationResult<T> {
    pub fn unpublished(exit_code: i32) -> Self {
        Self {
            exit_code,
            published: false,
            output: T::default(),
        }
    }
}

/// A failed durable operation reports whether publication occurred.
#[derive(Debug)]
pub struct OperationFailure {
    pub published: bool,
    pub message: String,
}
impl std::fmt::Display for OperationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "publication published={}: {}",
            self.published, self.message
        )
    }
}
impl std::error::Error for OperationFailure {}

/// The store format this build writes and understands. A build refuses a
/// store stamped newer than this, because the alternative is what silently
/// went wrong before: an older binary skipping event types it does not know,
/// concluding the change is still open, and closing it a second way.
pub const SCHEMA_VERSION: u32 = 7;

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
    /// Which framing operation the link came from. Absent on links recorded
    /// before arc named their source, which says nothing about how they were
    /// supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<JournalRefVia>,
}

/// Where a patchset's journal link came from: the artifact the change was
/// opened from, the plan its brief names, or an explicit `--journal-ref`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum JournalRefVia {
    Begin,
    Brief,
    Flag,
}

impl JournalRefVia {
    pub fn as_str(self) -> &'static str {
        match self {
            JournalRefVia::Begin => "begin",
            JournalRefVia::Brief => "brief",
            JournalRefVia::Flag => "flag",
        }
    }
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
    Finding {
        finding_id: String,
    },
    Verdict {
        event_id: String,
    },
    BlockedOnStage {
        event_id: String,
    },
    External {
        summary: String,
    },
    Forge {
        source: crate::forge_observation::ForgeSource,
    },
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
    pub(crate) fn validate_coordinates(&self, acting_session: Option<&str>) -> anyhow::Result<()> {
        use crate::coordinate::{actor_defect, is_placeholder, model_defect};
        let coordinates = [
            ("actor", &self.actor),
            ("harness", &self.harness),
            ("session", &self.session),
            ("model", &self.model),
        ];
        if coordinates.iter().all(|(_, value)| value.is_none()) {
            anyhow::bail!("planner metadata must name at least one coordinate");
        }
        for (field, value) in coordinates {
            if value
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                anyhow::bail!("planner {field} must be a nonempty string");
            }
        }
        let defect = [("harness", &self.harness), ("session", &self.session)]
            .into_iter()
            .find_map(|(field, value)| {
                value
                    .as_deref()
                    .filter(|value| is_placeholder(value))
                    .map(|value| (field, value, "a placeholder is not a coordinate"))
            })
            .or_else(|| {
                let actor = self.actor.as_deref()?;
                actor_defect(actor).map(|defect| ("actor", actor, defect.reason()))
            })
            .or_else(|| {
                let model = self.model.as_deref()?;
                model_defect(model, self.session.as_deref())
                    .or_else(|| model_defect(model, acting_session))
                    .map(|defect| ("model", model, defect.reason()))
            });
        if let Some((field, value, reason)) = defect {
            anyhow::bail!("{field} {value:?}: {reason}");
        }
        Ok(())
    }

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
    pub(crate) fn validated(&self) -> Self {
        let mut source = self.clone();
        if source
            .planners
            .iter()
            .any(|planner| planner.validate_coordinates(None).is_err())
        {
            source.planners.clear();
            source.planner_status = "malformed".to_string();
        }
        source
    }

    pub fn planner_trailers(&self) -> Vec<String> {
        if !matches!(self.planner_status.as_str(), "ok" | "corrected")
            || self
                .planners
                .iter()
                .any(|planner| planner.validate_coordinates(None).is_err())
        {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub model_provenance: ModelProvenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Private acting-session link. Ledger and journal records may carry it;
    /// Git commits, changelogs, trailers, and forge text must not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_link: Option<String>,
    /// What the harness's own store said about `session` when the event was
    /// written, whether detection supplied the session or the caller declared
    /// it. A session id arrives from the environment or a flag and can name
    /// no session the harness ever wrote; this is the record that the store
    /// backed it, or did not. Absent when the event names no harness and
    /// session, and on events from builds that recorded it only for detected
    /// sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_resolution: Option<SessionResolution>,
    /// Why the store did not corroborate `session`, as one bounded line.
    /// Present exactly beside an `uncorroborated` or `unresolved` verdict, so
    /// a failed lookup is never read as a recording the store lacks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_resolution_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(flatten)]
    pub payload: Payload,
}

/// Model identity and the evidence observed when an event was written.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelAttribution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub provenance: ModelProvenance,
    /// The store's verdict on the session the model was resolved against, as
    /// the resolving write saw it; `None` when no harness and session were
    /// asked about. An event keeps the verdict in its own session fields, so
    /// it is no part of this value's serialized form and a value rebuilt
    /// from an event carries none.
    #[serde(skip)]
    pub session_verdict: Option<SessionVerdict>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_source: Option<ModelSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_observation: Option<ModelObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_disagreement: Option<ModelDisagreement>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ModelSource {
    Flag,
    Env,
    Resolved,
}

impl ModelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Env => "env",
            Self::Resolved => "resolved",
        }
    }
}

/// A selection endpoint from the recording, with its bounded-read coverage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelObservation {
    pub observed: String,
    pub timestamp: Option<String>,
    pub native_id: Option<String>,
    pub head_read: bool,
    pub turn_start: Option<String>,
    pub turn_native_id: Option<String>,
    pub turn_relation: ModelTurnRelation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ModelTurnRelation {
    Inside,
    Before,
    Unknown,
}

impl ModelObservation {
    pub fn line(&self, session: Option<&str>) -> String {
        let relation = match self.turn_relation {
            ModelTurnRelation::Inside => "inside the acting turn",
            ModelTurnRelation::Before => "from an earlier turn; effort may have changed since",
            ModelTurnRelation::Unknown => {
                "observation timestamp or acting turn boundary unavailable in the bounded read"
            }
        };
        let coverage = if self.head_read {
            ""
        } else {
            "; recording head not read; earlier selections may be outside the read"
        };
        format!(
            "observed: {} at {} id {} ({relation}{coverage})",
            crate::coordinate::display_model(
                None,
                Some(&self.observed),
                session,
                crate::coordinate::ModelStyle::Coordinate
            ),
            self.timestamp.as_deref().unwrap_or("unknown timestamp"),
            self.native_id.as_deref().unwrap_or("unknown native id"),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelDisagreement {
    pub declared: String,
    pub observed: String,
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

/// The store's answer for one session id, carrying why it fell short of
/// corroboration wherever it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionVerdict {
    Corroborated,
    Uncorroborated(String),
    Unresolved(String),
}

impl SessionVerdict {
    pub fn resolution(&self) -> SessionResolution {
        match self {
            Self::Corroborated => SessionResolution::Corroborated,
            Self::Uncorroborated(_) => SessionResolution::Uncorroborated,
            Self::Unresolved(_) => SessionResolution::Unresolved,
        }
    }

    /// Why the store did not corroborate the id; `None` when it did.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Corroborated => None,
            Self::Uncorroborated(reason) | Self::Unresolved(reason) => Some(reason),
        }
    }
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
    /// Nobody declared one, no harness session was known, and Git has no
    /// `user.name`: no actor was established. An event records an actor, so a
    /// write refuses before it records one; this value is never on an event.
    Absent,
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
            Self::Absent => None,
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
    /// A read happened, by a reviewer routing does not count as independent
    /// of the author; the record names the read and why.
    IneligibleReview,
    /// A read by somebody independent, which nobody supplied. Also what an
    /// obligation recorded before kinds existed means.
    IndependentReview,
}

impl DebtMissing {
    /// Every kind, in severity order. A count split over this covers every
    /// obligation exactly once, so a split and a total cannot disagree.
    pub const ALL: [DebtMissing; 6] = [
        DebtMissing::NothingRead,
        DebtMissing::MergeResolutionUnread,
        DebtMissing::RepairUnread,
        DebtMissing::ContributorOnly,
        DebtMissing::IneligibleReview,
        DebtMissing::IndependentReview,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NothingRead => "nothing-read",
            Self::MergeResolutionUnread => "merge-resolution-unread",
            Self::RepairUnread => "repair-unread",
            Self::ContributorOnly => "contributor-only",
            Self::IneligibleReview => "ineligible-review",
            Self::IndependentReview => "independent-review",
        }
    }
}

/// Why routing does not count a read as independent of its author. A closed
/// set arc validates and never computes: routing assesses a reviewer against
/// the authors, and arc keeps the names it was given. Declaration order is
/// the order a record lists them in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum ReviewQualifier {
    /// The reviewer and an author are the same model, at any effort.
    SameModel,
    /// The reviewer is in an author's model family.
    SameFamily,
    /// The reviewer's review tier is below an author's floor.
    BelowFloor,
    /// An author's floor is unestablished.
    FloorUnknown,
    /// The reviewer holds no seated review tier, so no floor can place it.
    ReviewerUnseated,
}

impl ReviewQualifier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SameModel => "same-model",
            Self::SameFamily => "same-family",
            Self::BelowFloor => "below-floor",
            Self::FloorUnknown => "floor-unknown",
            Self::ReviewerUnseated => "reviewer-unseated",
        }
    }

    /// Every name the closed set accepts, for refusals that list them.
    pub const NAMES: &'static str =
        "same-model, same-family, below-floor, floor-unknown, reviewer-unseated";

    /// A qualifier list as a record keeps it: deduplicated, in set order.
    pub fn normalized(qualifiers: impl IntoIterator<Item = ReviewQualifier>) -> Vec<Self> {
        qualifiers
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Whether routing's assessment of a read came from routing or from the
/// caller's own declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssessmentBasis {
    /// The qualifiers came from an `agent-routing assess --json` file.
    Assessed,
    /// The caller named the qualifiers; nothing corroborates them.
    Declared,
}

impl AssessmentBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assessed => "assessed",
            Self::Declared => "declared",
        }
    }
}

/// Routing's reading of whether a reviewer counts as independent of the
/// authors it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Independence {
    Eligible,
    Ineligible,
}

impl Independence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Ineligible => "ineligible",
        }
    }
}

/// The read an `ineligible-review` debt names, and why routing did not count
/// it. Model strings and tiers are not here: the verdict event carries the
/// reviewer's model verbatim, and a tier is routing's reading at read time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IneligibleRead {
    /// The verdict event the read recorded.
    pub read: String,
    /// Never empty.
    pub qualifiers: Vec<ReviewQualifier>,
    /// The routing version that assessed the read, as a coordinate and
    /// nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_version: Option<String>,
    pub assessment: AssessmentBasis,
}

/// Whether an assessment describes the auditor that recorded it: the audit
/// event's model against the model strings routing accepts for the assessed
/// reviewer. Only the outcome is kept, never the strings compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditorBinding {
    /// The recorded model is one the assessed reviewer answers to, at the
    /// effort the assessment names where both sides name one.
    Bound,
    ModelMismatch,
    EffortMismatch,
    /// The audit recorded no model, as when a person audits.
    ModelUnrecorded,
    /// The assessment lists no model strings for its reviewer.
    IdentitiesUnstated,
    /// The assessment's authors do not include the model of every
    /// `Implemented-by` line in the audited range; the audit lists the ones
    /// it leaves out.
    AuthorsUncovered,
    /// The audited range's `Implemented-by` lines could not be established:
    /// the production record kept no list and the range did not read. Never
    /// treated as an empty list.
    AuthorsUnread,
}

impl AuditorBinding {
    /// The fixed sentence for a binding that failed.
    pub fn failure(self) -> Option<&'static str> {
        match self {
            Self::Bound => None,
            Self::ModelMismatch => {
                Some("the auditor's recorded model is not one the assessed reviewer answers to")
            }
            Self::EffortMismatch => {
                Some("the auditor's recorded effort differs from the assessed reviewer's")
            }
            Self::ModelUnrecorded => Some("the audit records no model to bind the assessment to"),
            Self::IdentitiesUnstated => {
                Some("the assessment names no model identities for its reviewer")
            }
            Self::AuthorsUncovered => {
                Some("the assessment's authors omit a model the range's Implemented-by lines name")
            }
            Self::AuthorsUnread => Some(
                "the range's Implemented-by lines could not be read, so its authors are unknown",
            ),
        }
    }

    /// What a binding that failed says of the assessment, as the remainder of
    /// a sentence that begins with "assessment": where the models an
    /// assessment's authors omit are known, they are named.
    pub fn failure_note(self, uncovered: &[String]) -> Option<String> {
        let reason = self.failure()?;
        Some(match self {
            Self::AuthorsUncovered if !uncovered.is_empty() => {
                format!(
                    "not bound to the audited work: {reason}: {}",
                    uncovered.join(", ")
                )
            }
            Self::AuthorsUncovered | Self::AuthorsUnread => {
                format!("not bound to the audited work: {reason}")
            }
            _ => format!("not bound to this auditor: {reason}"),
        })
    }
}

/// The slice of a routing assessment an audit keeps: enough to tell whether
/// the auditor counts as independent and whether the assessment describes it,
/// never the tiers behind the answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditAssessment {
    pub independence: Independence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualifiers: Vec<ReviewQualifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_version: Option<String>,
    /// Absent on an assessment recorded without one, which binds nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<AuditorBinding>,
    /// The models of the range's `Implemented-by` lines that no author of the
    /// assessment answers to. Empty unless the binding is `authors-uncovered`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncovered_authors: Vec<String>,
}

/// Whether `version` is a route version arc will record: one token, no
/// whitespace or control characters.
pub fn validate_route_version(version: &str) -> anyhow::Result<()> {
    if version.is_empty()
        || version.len() > 64
        || version
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        anyhow::bail!(
            "route version {version:?} is not well formed: one token of at most 64 characters, \
             without whitespace"
        );
    }
    Ok(())
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DebtCoverage {
    pub reviewer: String,
    /// The originating verdict session used to validate its recorded model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
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

impl Serialize for DebtCoverage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("reviewer", &self.reviewer)?;
        for (key, value) in [
            ("session", &self.session),
            ("model", &self.model),
            ("effort", &self.effort),
            ("route_version", &self.route_version),
        ] {
            if let Some(value) = value {
                map.serialize_entry(key, value)?;
            }
        }
        if !crate::coordinate::model_recorded(self.model.as_deref(), self.session.as_deref()) {
            map.serialize_entry("model_recorded", &false)?;
        }
        map.end()
    }
}

impl DebtCoverage {
    /// Coverage from one recorded review identity, reading the effort off the
    /// model string.
    pub fn new(
        reviewer: String,
        model: Option<String>,
        session: Option<String>,
        route_version: Option<String>,
    ) -> Self {
        let effort = model.as_deref().and_then(model_effort).map(str::to_owned);
        DebtCoverage {
            reviewer,
            session,
            model,
            effort,
            route_version,
        }
    }
}

/// One identity at the coordinates arc keeps for it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

impl Serialize for DebtIdentity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("actor", &self.actor)?;
        for (key, value) in [
            ("on_behalf_of", &self.on_behalf_of),
            ("harness", &self.harness),
            ("model", &self.model),
            ("effort", &self.effort),
            ("session", &self.session),
        ] {
            if let Some(value) = value {
                map.serialize_entry(key, value)?;
            }
        }
        if !crate::coordinate::model_recorded(self.model.as_deref(), self.session.as_deref()) {
            map.serialize_entry("model_recorded", &false)?;
        }
        map.end()
    }
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
    /// The `Implemented-by` values of the commits in the patchset's range,
    /// verbatim and without repeats. Empty records that the range names no
    /// implementer; absent records that the range was not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailer_implementers: Option<Vec<String>>,
    /// Whether a brief exists and somebody other than its author implemented
    /// it. False for unbriefed work and for work its own planner wrote.
    pub following_brief: bool,
}

impl DebtProduction {
    /// What the commits' `Implemented-by` lines say, when it is worth saying:
    /// the range names no implementer, or one other than the recorder.
    pub fn trailer_note(&self) -> Option<String> {
        let named = self.trailer_implementers.as_ref()?;
        if named.is_empty() {
            return Some("no Implemented-by trailers".to_string());
        }
        let recorder = self
            .implementer
            .harness
            .as_deref()
            .zip(self.implementer.model.as_deref())
            .map(|(harness, model)| (harness, model.split('#').next().unwrap_or(model)));
        let only_recorder = named.iter().all(|value| {
            let parts = value
                .split_once(':')
                .map(|(harness, rest)| (harness, rest.split('#').next().unwrap_or(rest)));
            parts.is_some() && parts == recorder
        });
        (!only_recorder).then(|| format!("Implemented-by trailers: {}", named.join(", ")))
    }
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

/// The topology recorded by a guarded integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationShape {
    FastForward,
    Merge,
    AlreadyContained,
}

impl IntegrationShape {
    pub fn from_legacy(already_contained: bool) -> Self {
        if already_contained {
            Self::AlreadyContained
        } else {
            Self::Merge
        }
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
        #[serde(default, skip_serializing_if = "is_false")]
        worktree_created: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_owner_token: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blocked_by: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
        /// Journal artifact filename this change was opened from, if any.
        /// Additive: absent for changes not begun via `--from-journal`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        journal_ref: Option<String>,
        /// `sha256:` over that artifact's body read when the change opened.
        /// Absent on changes opened before arc recorded one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        journal_ref_digest: Option<String>,
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
    ChangeCheckoutBound {
        branch: String,
        worktree: String,
        worktree_created: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_owner_token: Option<String>,
    },
    ChangeRolesAsserted {
        asserted: BTreeMap<String, Vec<String>>,
        withdrawn: BTreeMap<String, Vec<String>>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery: Option<crate::forge_round_trip::ForgeDelivery>,
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
        /// Reads the contract requires of whoever answers it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        must_read: Vec<ReadRequirement>,
    },
    /// The change's release copy, or the record that it needs none.
    ChangelogRecorded {
        /// Empty on a record that the change needs no entry.
        #[serde(alias = "section")]
        category: String,
        /// Empty on a record that the change needs no entry.
        body: String,
        /// The changelog entry this one replaces, when it supersedes an
        /// earlier projection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supersedes: Option<String>,
        /// Why the change needs no entry. Present only on a record saying
        /// so, which projects nothing; a reader of an older store format
        /// would project it as an entry, so recording it stamps format 7.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        no_entry_reason: Option<String>,
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
        /// The candidate a promotion recorded this patchset from, and the
        /// selection that permitted it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        candidate: Option<PatchsetCandidate>,
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
        /// Events on the same change the fact rests on, each checked to exist
        /// and to be of a citable kind when the fact was kept. A citation
        /// names a record; it does not upgrade the fact's kind.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cites: Vec<String>,
    },
    /// A reviewer began reading the change, so its share of the reviewer's
    /// session runs from here to its last event on the change.
    ReviewStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patchset_id: Option<String>,
    },
    CommentAdded {
        body: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        patchset_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        anchor: Option<Anchor>,
    },
    FindingAdded {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        forge_sources: Vec<crate::forge_observation::ForgeSource>,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        forge_sources: Vec<crate::forge_observation::ForgeSource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forge_frontier: Option<Vec<String>>,
        patchset_id: String,
        verdict: Verdict,
        /// Publication authority observed while recording the judgment; grants no review eligibility.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority_observed: Option<AuthorityObserved>,
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
        /// The read an `ineligible-review` debt names. Present exactly when
        /// `missing` is that kind and the debt was declared with its read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ineligible: Option<IneligibleRead>,
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
        /// Routing's assessment of this auditor against the authors, when the
        /// caller supplied one. An `ineligible-review` debt is discharged only
        /// by an audit whose assessment is eligible.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assessment: Option<AuditAssessment>,
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
    /// A disposition recorded after integration against a review finding
    /// that was open when the change shipped.
    ///
    /// It sits beside the finding's dispositions, never among them, so the
    /// finding state the change shipped with stays as it shipped. It is not
    /// a verdict and discharges no review debt.
    PostIntegrationDispositionRecorded {
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_artifact: Option<crate::verification_output::OutputArtifact>,
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
        /// The tree this reuse answers for. Absent events bind to revision.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tree: Option<String>,
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
    /// An integration arc performed under its own guard, binding the approved
    /// head, evaluated tree, target revision and authorization. A fast-forward
    /// lands at the approved head; a merge verifies both parents; an
    /// already-contained integration leaves the target revision unchanged.
    ChangeProvisionallyIntegrated {
        project_id: String,
        source_patchset_id: String,
        source_head: String,
        publication_target: String,
        staging_branch: String,
        staging_before: String,
        provisional_commit: String,
        evaluated_tree: String,
        recipient: ProvisionalRecipient,
        authority_observed: Option<AuthorityObserved>,
        authorization: AuthorizationBasis,
    },
    ProvisionalIntegrationResolved {
        provisional_event_id: String,
        integration_event_id: String,
        accepted_patchset_id: String,
        authority_grant_event_id: String,
        kind: ProvisionalResolutionKind,
        reason: String,
    },
    ChangeIntegrated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authority_grant_event_id: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        provisional_resolutions: Vec<ProvisionalResolution>,
        integrated_commit: String,
        source_patchset_id: String,
        source_head: String,
        target_branch: String,
        target_before: String,
        /// Everything the guard consumed to authorize this one irreversible
        /// decision. Absent only on events written before arc recorded it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authorization: Option<AuthorizationBasis>,
        /// The target already held the approved head, so `target_before`
        /// equals `integrated_commit`. An advancing fast-forward is false.
        #[serde(default, skip_serializing_if = "is_false")]
        already_contained: bool,
        /// Absent fields derive from `already_contained`: true means
        /// already-contained, false means merge.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shape: Option<IntegrationShape>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_revision: Option<String>,
        authorization: AuthorizationBasis,
    },
    IntegrationAsserted {
        integrated_commit: String,
        source_patchset_id: String,
        source_head: String,
        target_branch: String,
        /// Where the target stood before: an explicitly asserted ancestor,
        /// or a merge head's first parent when no ancestor was named.
        /// Absent for other heads when no ancestor was named.
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
    /// Adapter-reported acquisition and generation claims; no readiness
    /// or integration authority follows from admission.
    ForgeObservationRecorded {
        observation: crate::forge_observation::ForgeObservation,
    },
    /// Observed PR tuple validated against its declaration and policy.
    ForgeLink {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observation: Option<String>,
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
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        binding_recorded: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link_event_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observation: Option<String>,
        pr_head: String,
        state: crate::forge::ForgeCheckState,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Observed PR lifecycle, bound to the exact link and head it was read
    /// at. `merged` carries the actual merge commit. The binding fields are
    /// absent only on events written before lifecycle facts were bound.
    ForgePrState {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observation: Option<String>,
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
        /// Ref moves performed by the rewrite. External maps and records
        /// without this field have no recorded ref-move information.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        refs: BTreeMap<String, crate::rewrite::RefMove>,
        /// The refs the rewrite created to keep a replaced commit reachable
        /// — each a patchset head or a commit a verdict was recorded on —
        /// with the commit it holds. Records without this field retained
        /// nothing.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        retained: BTreeMap<String, String>,
    },
    /// Withdraw one repository rewrite map without undoing any Git ref moves.
    HistoryRewriteWithdrawn {
        rewrite_event_id: String,
        reason: String,
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
    /// on a fork, which is outside the lifecycle by design, on a bare commit
    /// range in a repository whose ledger holds nothing yet, or on a journal
    /// artifact that no change has been begun from. A record that could bind
    /// only to a change would be unavailable to exactly the work that most
    /// needs it.
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
        /// The journal artifact this run was dispatched against.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        journal: Option<RunJournalSubject>,
        #[serde(skip_serializing_if = "Option::is_none")]
        brief_event_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// A caller told arc which backend session executed a dispatched run.
    ///
    /// The caller learns the executor's session only after launch, so the
    /// binding is its own fact, written once per dispatch at any time after
    /// it, an ending included. A follow-up or a resume is its own dispatch
    /// with its own binding: two dispatches bound to one executor session are
    /// two spans of one measurement scope, never one run.
    RunBound {
        dispatch_event_id: String,
        /// The harness that ran the executor session.
        executor_harness: String,
        /// The executor's native session id in that harness.
        executor_session: String,
        /// The delegate's handle for the run, when the caller has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        /// Session in the executor's harness whose history it began as a copy of.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forked_from: Option<String>,
        /// The session that spawned the executor as its native child: a Claude
        /// subagent's parent, whose recording holds the subagent's as
        /// `<parent>::<agent>`, or the session another harness's child names
        /// as its parent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
        /// The role the executor served in, `implementer` or `reviewer`;
        /// absent reads as `implementer`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
    },
    /// Normalized consumption facts arc read from one executor or participant
    /// session, kept so the cost view's arithmetic replays after the source
    /// rotates. The facts name the reader and calculation that produced them and the
    /// source frontier and digest they describe; the source bytes stay in the
    /// harness's own store. A later retention for the same session supersedes
    /// nothing: each is an observation at its frontier, never added to
    /// another.
    CostFactsRetained {
        executor_harness: String,
        executor_session: String,
        facts: Box<CostFacts>,
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
    /// A registered alternative answer to a brief: content, the contract it
    /// answers, who produced it, and where it came from. Repository-scoped.
    /// It opens no change and creates no patchset, and no later event alters
    /// it.
    CandidateRegistered {
        candidate_id: String,
        /// The registered content. A commit given at registration is
        /// recorded as its tree.
        tree: String,
        brief: CandidateBriefRef,
        /// Who produced the content. Never empty.
        producers: Vec<String>,
        /// Registrations this one continues. Each names the same brief
        /// version.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        parents: Vec<String>,
        /// Registrations whose content this one carries into its own
        /// contract. The producers include every producer along each
        /// adopted registration's parent chain.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        adopts: Vec<String>,
        /// Claims recorded on the brief's change that the work ran under.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        episodes: Vec<String>,
        /// Digest of the received evidence requested by this registration.
        /// Absence establishes no recoverable evidence request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requested_evidence_sha256: Option<String>,
    },
    /// A declarant's judgement of a candidate. It changes no registration.
    CandidateJudged {
        candidate_id: String,
        judgement: CandidateJudgement,
        reason: String,
    },
    /// A candidate's pin was deleted on the operator's command while no root
    /// reached it. The registration stands.
    CandidateRetired {
        candidate_id: String,
    },
    /// Execution evidence received from outside this ledger, recorded for a
    /// registered candidate. It is the sender's claim, written once beside the
    /// registration, and it is no evaluation: no gate, selection or
    /// promotion reads it as one. Repository-scoped.
    CandidateEvidenceReceived {
        candidate_id: String,
        evidence: crate::received_evidence::ReceivedEvidence,
    },
    /// A required gate run against a candidate's tree in a scratch checkout.
    /// Repository-scoped, keyed by the tree, and carrying the declaration it
    /// ran under as consumed, so a later reader compares coordinates rather
    /// than trusting a name.
    CandidateVerified {
        candidate_id: String,
        tree: String,
        gate: String,
        /// The gate's command as declared when it ran.
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_seconds: Option<u64>,
        /// The gate's environment probe as declared when it ran.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment_probe: Option<String>,
        /// What that probe yielded in the checkout. Absent when the gate
        /// declares no probe or the probe yielded no identity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment: Option<EnvironmentEvidence>,
        result: VerifyResult,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_artifact: Option<crate::verification_output::OutputArtifact>,
        #[serde(default, skip_serializing_if = "is_false")]
        timed_out: bool,
        hostname: String,
    },
    /// A named choice validated against every ground and permitted: the
    /// basis a promotion rests on. It records no effect; a promotion is its
    /// own event.
    CandidateSelected {
        /// The chosen registration.
        candidate_id: String,
        /// The destination change, whose brief the registration answers.
        destination: String,
        /// The destination's branch head the selection was validated at.
        head: String,
        target_branch: String,
        /// The target head the selection was validated at.
        target: String,
        /// The shipped tree: the chosen registration's.
        tree: String,
        /// The evaluation answering each required gate.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evaluations: Vec<SelectedEvaluation>,
        /// The read meeting each requirement of the brief version.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reads: Vec<SelectedRead>,
        reuse: crate::policy::EvaluationReuse,
        /// The producers along the chosen registration's parent chain.
        contributors: Vec<String>,
        selector: String,
        rationale: String,
        /// An earlier selection for the same destination, never promoted,
        /// that this one stands in place of.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supersedes: Option<String>,
    },
    /// A selection's effect, observed: the destination branch moved to
    /// `revision` and the patchset recorded there.
    CandidatePromoted {
        selection: String,
        candidate_id: String,
        destination: String,
        patchset_id: String,
        revision: String,
    },
    /// A tool's record of a read that succeeded: which bytes it returned for
    /// which path and range, within one episode of work on a subject. The
    /// read's version is the digest of the returned bytes.
    ContextRead {
        subject: RelationSubject,
        /// The claim the read happened under, recorded on the subject's
        /// change (a candidate's brief change).
        episode: String,
        /// The tool record's stable identifier. One per subject.
        record: String,
        /// The path as the tool recorded it.
        path: String,
        /// `sha256:` over the bytes the tool returned.
        digest: String,
        coverage: ReadCoverage,
        /// The revision the returned bytes were compared against, when one
        /// was named.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compared_at: Option<String>,
        /// The blob the bytes equal at `compared_at`. An inference, present
        /// only when the bytes matched.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blob: Option<InferredBlob>,
        /// The journal artifact the path names, when it is inside the
        /// journal, hot or cold.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<ReadArtifact>,
        /// The tool record document the read was taken from, when it came
        /// from one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    /// A declarant's claim that a subject cites, relies on, or considers a
    /// piece of context. It is never a read.
    ContextDeclared {
        subject: RelationSubject,
        relation: DeclaredRelation,
        target: DeclaredTarget,
        /// A read record on the same subject the claim points at.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        citation: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disclosure: Option<crate::forge_round_trip::Disclosure>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<crate::forge_round_trip::SourceGeneration>,
    },
    /// What the provider reported for the retention of a read record's
    /// recording. A claim about retention, attributed to its declarant.
    ContextCaptureReported {
        subject: RelationSubject,
        record: String,
        capture: CaptureState,
    },
    /// An event whose `event_type` this build does not recognize (e.g. one
    /// imported from a newer arc). Typed loading skips these entries; the
    /// underlying files and raw export preserve their original bytes intact.
    #[serde(other)]
    Unknown,
}

/// The contract a candidate answers: one version of one change's brief, and
/// the digest of that version's body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CandidateBriefRef {
    pub change_id: String,
    pub brief_event_id: String,
    /// `sha256:` over the brief body.
    pub digest: String,
}

/// A read a brief requires: a journal artifact at one body digest, or a
/// file's blob at a revision, whole or a line range.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ReadRequirement {
    Artifact {
        /// The file name in this project's journal, or
        /// `<journal-dir>::<file>` in another project's.
        file: String,
        /// `sha256:` over the artifact body.
        digest: String,
    },
    File {
        revision: String,
        path: String,
        blob: String,
        extent: RequiredExtent,
    },
}

impl std::fmt::Display for ReadRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Artifact { file, digest } => write!(f, "{file}@{digest}"),
            Self::File {
                revision,
                path,
                extent: RequiredExtent::Whole,
                ..
            } => write!(f, "{revision}:{path}"),
            Self::File {
                revision,
                path,
                extent: RequiredExtent::Lines { from, to },
                ..
            } => write!(f, "{revision}:{path}:{from}-{to}"),
        }
    }
}

/// How much of a required file must have been read.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RequiredExtent {
    Whole,
    /// One-based, inclusive.
    Lines {
        from: u64,
        to: u64,
    },
}

/// The evaluation a selection counted for one required gate, and the
/// registration it was recorded on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectedEvaluation {
    pub gate: String,
    pub event_id: String,
    pub candidate_id: String,
}

/// The read a selection counted for one read requirement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelectedRead {
    pub requirement: ReadRequirement,
    pub candidate_id: String,
    pub record: String,
    pub event_id: String,
}

/// A patchset's link to the candidate promoted into it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchsetCandidate {
    pub candidate_id: String,
    pub selection: String,
}

/// What a relation attaches to: a change, or a registered candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RelationSubject {
    Change { change_id: String },
    Candidate { candidate_id: String },
}

impl RelationSubject {
    pub fn label(&self) -> String {
        match self {
            Self::Change { change_id } => format!("change {change_id}"),
            Self::Candidate { candidate_id } => format!("candidate {candidate_id}"),
        }
    }
}

/// The extent a tool recorded for a read. `unknown` is never `whole`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ReadCoverage {
    Whole,
    /// One-based, inclusive.
    Lines {
        from: u64,
        to: u64,
    },
    Unknown,
}

impl std::fmt::Display for ReadCoverage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Whole => write!(f, "whole"),
            Self::Lines { from, to } => write!(f, "lines {from}-{to}"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// A blob arc inferred for a read: the returned bytes equal the blob's bytes
/// for the read's path and range at `revision`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferredBlob {
    pub revision: String,
    /// The repository-relative path at `revision`.
    pub path: String,
    pub blob: String,
    /// What the inference rests on: `content-matches-revision`.
    pub inference: String,
}

pub const CONTENT_MATCHES_REVISION: &str = "content-matches-revision";

/// The journal artifact a read's path names, with the artifact body's digest
/// as it stood when the read was recorded. The read's own digest is compared
/// with it, never assumed equal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadArtifact {
    /// The file name in this project's journal, or `<journal-dir>::<file>`
    /// in another project's.
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_digest: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DeclaredRelation {
    Cites,
    ReliesOn,
    Considers,
}

impl DeclaredRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cites => "cites",
            Self::ReliesOn => "relies-on",
            Self::Considers => "considers",
        }
    }
}

/// What a declaration points at: a path, optionally at a revision, or a
/// journal artifact by its file name or qualified `<journal-dir>::<file>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DeclaredTarget {
    Path {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
    },
    Artifact {
        file: String,
    },
}

impl std::fmt::Display for DeclaredTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path { path, at: Some(at) } => write!(f, "{path} at {at}"),
            Self::Path { path, at: None } => write!(f, "{path}"),
            Self::Artifact { file } => write!(f, "artifact {file}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureState {
    Pinned,
    Unpinned,
}

impl CaptureState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Unpinned => "unpinned",
        }
    }
}

/// What a declarant concluded about a candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CandidateJudgement {
    /// A considered alternative not taken.
    Rejected,
    /// Another registration answers in this one's place.
    SupersededBy { candidate_id: String },
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_verdict_provenance: Option<crate::import_authorization::SelectedVerdictProvenance>,
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
    #[serde(default)]
    pub provisional_prerequisites: Vec<ProvisionalPrerequisite>,
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
    #[serde(default)]
    pub replica: crate::import_authorization::ReplicaPolicy,
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
        Payload::ChangeCheckoutBound { .. }
        | Payload::MetadataUpdated { .. }
        | Payload::BriefRecorded { .. }
        | Payload::ChangeRolesAsserted { .. }
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
        Payload::ChangeProvisionallyIntegrated { .. }
        | Payload::ProvisionalIntegrationResolved { .. }
        | Payload::Message { .. }
        | Payload::ClaimReleased { .. }
        | Payload::ContextKept { .. }
        | Payload::CommentAdded { .. }
        | Payload::ReviewStarted { .. }
        | Payload::ReplyAdded { .. }
        | Payload::HoldReleased { .. }
        | Payload::ForgeObservationRecorded { .. }
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
        | Payload::AuditDispositionRecorded { .. }
        | Payload::PostIntegrationDispositionRecorded { .. } => {
            AppendPermission::IntegratedOnlyFact
        }
        Payload::ChangeOpened { .. }
        | Payload::ChangeClosed { .. }
        | Payload::ChangeIntegrated { .. }
        | Payload::IntegrationAsserted { .. }
        | Payload::IterationScopeSet { .. } => AppendPermission::LifecycleOwned,
        // Repository-scoped: it is never appended to a change's log, so no
        // change-phase policy applies to it.
        Payload::HistoryRewritten { .. }
        | Payload::HistoryRewriteWithdrawn { .. }
        | Payload::ReviewPassOpened { .. }
        | Payload::ReviewPassCompleted { .. }
        | Payload::ReviewPassAbandoned { .. } => AppendPermission::AnyPhaseFact,
        | Payload::RunDispatched { .. }
        | Payload::RunBound { .. }
        | Payload::RunEnded { .. }
        | Payload::CostFactsRetained { .. }
        | Payload::CandidateRegistered { .. }
        | Payload::CandidateJudged { .. }
        | Payload::CandidateRetired { .. }
        | Payload::CandidateEvidenceReceived { .. }
        | Payload::CandidateVerified { .. }
        | Payload::CandidateSelected { .. }
        | Payload::CandidatePromoted { .. }
        | Payload::ContextRead { .. }
        | Payload::ContextDeclared { .. }
        | Payload::ContextCaptureReported { .. } => AppendPermission::AnyPhaseFact,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forge_sources: Vec<crate::forge_observation::ForgeSource>,
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
    pub forge_sources: Vec<String>,
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
    pub fn as_str(self) -> &'static str {
        match self {
            DispositionStatus::Resolved => "resolved",
            DispositionStatus::AcceptedRisk => "accepted-risk",
            DispositionStatus::Obsolete => "obsolete",
            DispositionStatus::StillOpen => "still-open",
            DispositionStatus::Disputed => "disputed",
        }
    }

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

/// One session's consumption as one read measured it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostFacts {
    /// The reader that measured the session: tapes' core library and the
    /// revision arc pins it at.
    pub reader: String,
    /// The normalization arc applied to the reader's answer.
    pub calculation: String,
    /// Bytes of the source the read observed.
    pub frontier: u64,
    /// `sha256:` over the observed source prefix, when the reader pinned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// What the counters are, as tapes names it: `recorded-total` or
    /// `summed-requests`. Absent when the source recorded no counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    /// How much of the session the counters cover: `session`, `read-window`
    /// or `since-reset`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<String>,
    /// Earliest observed source timestamp, including copied recording turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_started_at: Option<DateTime<Utc>>,
    /// The read could not establish the source's complete opening boundary.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_start_uncertain: bool,
    /// Completeness and validity gaps per counter category and `usd`.
    /// Absence means category coverage was not retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_gaps: Option<BTreeMap<String, Vec<String>>>,
    /// Token counters by tapes' categories. Reasoning is a subset of output.
    #[serde(default)]
    pub counters: CostCounters,
    /// Recorded relatives and any uncertainty in the lineage read.
    /// Absence means lineage was not retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<CostLineage>,
    /// The dollar figure the harness wrote: a harness-calculated estimate
    /// with an unknown pricing basis, never a charge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_estimate_usd: Option<f64>,
    /// Why the measurement is incomplete. Empty when it covers the session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete: Vec<String>,
    /// Which records the counters select where a recording keeps more than
    /// one conversation path: `active-branch` or `retained-branches`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
    /// The counters restated in tapes' disjoint categories, which add across
    /// harnesses. Absence means they were not retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disjoint: Option<CostDisjoint>,
    /// Pricing completeness of the harness-written dollar figures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_coverage: Option<CostPricing>,
    /// The consumption events a request sum counted, with per-quantity
    /// coverage and how each event is identified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<CostEvents>,
    /// The recorded total reconciled per counter with the requests its
    /// recording family identifies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residual: Option<CostResidual>,
    /// Whether the session was running when these facts were retained, as
    /// agent-status answered: `live`, `ended` or `unknown`. A fresh read
    /// carries none; absence reads as unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liveness: Option<String>,
    /// The one model the recording says served the session, as
    /// `model#variant` when it names a variant. Absent when it names none or
    /// several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_model: Option<String>,
    /// The disjoint categories of each model the figure is priced by, share
    /// by share: where the recording names several models, counts events on
    /// branches besides the active one, or, under uncertain attribution,
    /// leaves part of the figure to no model. Empty where `served_model`
    /// prices the figure whole.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_model: Vec<CostModelShare>,
    /// The session read per span, when the ledger gives it more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spans: Option<CostSpans>,
    /// The list-price estimate under the price table a view loads. It is
    /// priced from these facts when they are shown and is never retained.
    #[serde(skip)]
    pub list_price: Option<CostListPrice>,
    /// What price terms set by the request read, as the read that measured
    /// the figure kept it. Never retained, so retained facts hold none.
    #[serde(skip)]
    pub request_terms: CostRequestTerms,
}

/// The requests a figure counted, one by one, and the billing its
/// recording establishes, which a price table's terms set by the request
/// read. It describes how the figure is priced, not what it measures, so
/// any two compare equal.
#[derive(Debug, Clone, Default)]
pub struct CostRequestTerms {
    /// Absent when the read did not keep the figure's requests one by one.
    pub requests: Option<agent_tapes_core::pricing::RequestUses>,
    pub billing: Option<agent_tapes_core::pricing::Billing>,
    /// The first and last activity the figure spans, which bound the
    /// validity windows its requests could fall in.
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

impl PartialEq for CostRequestTerms {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// One model's part of a figure, by its model id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostModelShare {
    pub model: String,
    pub disjoint: CostDisjoint,
}

/// A session read once per disjoint segment of its timeline. Segments are
/// cut at every boundary of the spans the ledger gives the session, so no
/// request is in two of them; the segments and the remainder add up to
/// `whole` per counter, within `whole`'s accounting basis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostSpans {
    /// Why the read could not split the session: `span reading unsupported`
    /// when the reader refuses to divide it. The segments are empty then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsplit: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<CostSegment>,
    /// Requests no segment holds: those whose time the recording leaves
    /// unrecorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remainder: Option<Box<CostFacts>>,
    #[serde(default)]
    pub remainder_requests: u64,
    /// The whole session in the segments' accounting basis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whole: Option<Box<CostFacts>>,
    /// How a participant's unattributed segments were estimated, when the
    /// read estimated them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<CostEstimate>,
}

/// The working-stretch estimate of a participant's unattributed segments:
/// its basis, the idle gap that cuts a stretch, and why no part was
/// estimated when none was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub basis: String,
    pub idle_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withheld: Option<String>,
}

/// One part of an unattributed segment, cut by the participant's working
/// stretches and its own events in them, and the item the estimate gives
/// it: none for a stretch without an event of the participant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostEstimatePart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work: Option<String>,
    pub requests: u64,
    pub facts: Box<CostFacts>,
}

/// One segment `[from, to)` of a session's timeline, the work items and runs
/// whose spans cover it, and what the session consumed inside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostSegment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<DateTime<Utc>>,
    /// The covering work items, by their view labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub work: Vec<String>,
    /// The covering runs, by dispatch event id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<String>,
    /// Counted requests the segment holds.
    pub requests: u64,
    pub facts: Box<CostFacts>,
    /// For a segment no single item covers in a participant's read, the
    /// parts the working-stretch estimate cuts it into, which together hold
    /// every timed request the segment holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub estimates: Vec<CostEstimatePart>,
}

/// What defines a segment: its bounds, covering work items and runs.
pub(crate) type SegmentDefinition<'a> = (
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    &'a [String],
    &'a [String],
);

impl CostSegment {
    /// The segment's definition, without what was measured inside it.
    pub(crate) fn bounds(&self) -> SegmentDefinition<'_> {
        (self.from, self.to, &self.work, &self.runs)
    }
}

impl CostSpans {
    pub(crate) fn portable(&self) -> bool {
        self.unsplit
            .as_deref()
            .is_none_or(|reason| reason == "span reading unsupported")
            && self.segments.iter().all(|segment| {
                segment
                    .work
                    .iter()
                    .chain(&segment.runs)
                    .all(|label| portable_cost_metadata(label) && !label.is_empty())
                    && segment.facts.portable()
                    && segment.estimates.iter().all(|part| {
                        part.work
                            .as_deref()
                            .is_none_or(|label| portable_cost_metadata(label) && !label.is_empty())
                            && part.facts.portable()
                    })
            })
            && self.remainder.as_deref().is_none_or(CostFacts::portable)
            && self.whole.as_deref().is_none_or(CostFacts::portable)
            && self.estimate.as_ref().is_none_or(|estimate| {
                estimate.basis == "working-stretch"
                    && estimate
                        .withheld
                        .as_deref()
                        .is_none_or(portable_cost_metadata)
            })
    }
}

/// A list-price estimate of one figure: an amount where every priced token
/// has one price, else the least and the most it can be, the table rows it
/// used, and why it is not a complete estimate.
#[derive(Debug, Clone, PartialEq)]
pub struct CostListPrice {
    pub currency: String,
    /// The schema of the table that priced it.
    pub schema: &'static str,
    /// Absent when nothing in the figure could be priced.
    pub amount: Option<f64>,
    /// The most the priced part can cost, present only where a price in
    /// doubt leaves `amount` the least it can.
    pub at_most: Option<f64>,
    pub models: Vec<String>,
    pub gaps: Vec<String>,
}

/// Token counts in categories no token belongs to twice: uncached input,
/// cache read, cache write by TTL, and output, with reasoning a subset of
/// output. Each is absent when the native counters do not determine it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostDisjoint {
    pub convention: CostConvention,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncached_input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<CostCacheWrites>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    /// Why a category, by tapes' category name, is not completely measured.
    /// A category absent from the map is complete; absence of the map means
    /// category coverage was not retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gaps: Option<BTreeMap<String, Vec<String>>>,
}

/// How the harness's native counters divide a request's tokens, as tapes
/// names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CostConvention {
    /// `excludes-cache` or `includes-cache-read`.
    pub input: String,
    /// `includes-reasoning` or `excludes-reasoning`.
    pub output: String,
}

/// Cache writes by how long the provider keeps the entry. The buckets sum to
/// the writes; `ttl_unrecorded` holds writes the source gave no TTL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostCacheWrites {
    pub ephemeral_5m: u64,
    pub ephemeral_1h: u64,
    pub ttl_unrecorded: u64,
}

impl CostCacheWrites {
    pub fn total(&self) -> Option<u64> {
        self.ephemeral_5m
            .checked_add(self.ephemeral_1h)?
            .checked_add(self.ttl_unrecorded)
    }
}

/// Pricing completeness as tapes reports it. A written zero beside nonzero
/// usage is unpriced, never a measured zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostPricing {
    pub complete: bool,
    pub zero_written_requests: u64,
    pub zero_written_observations: u64,
}

/// Consumption events a request sum counted, each once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEvents {
    pub counted: u64,
    pub active_branch: u64,
    pub other_branches: u64,
    /// Further observations of counted events; they add nothing.
    pub repeats: u64,
    /// Per counter category and `cost`: events that measured it, that did
    /// not record it, and that wrote a zero price beside usage.
    pub coverage: BTreeMap<String, CostEventCoverage>,
    pub identity: CostEventIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEventCoverage {
    pub measured: u64,
    pub missing: u64,
    pub unpriced: u64,
}

/// How counted events are keyed: by a provider response, by an entry within
/// this recording alone, or not at all. `account_namespace` is
/// `established` or `unestablished` when provider responses key events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostEventIdentity {
    pub provider_response: u64,
    pub recording_entry: u64,
    pub unidentified: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_namespace: Option<String>,
}

/// A recorded total reconciled with its recording family's identified
/// requests. `unavailable` names why no reconciliation was read; the other
/// fields are tapes' answer, with statuses and reasons as tapes names them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostResidual {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
    #[serde(default)]
    pub scope_established: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_coverage: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counters: Vec<CostCounterResidual>,
    /// Evidence that would establish the total's scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// The family recordings read, by role, frontier and digest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<CostResidualSource>,
}

/// One counter's recorded total, identified union, and signed difference
/// `recorded - identified`, never clamped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostCounterResidual {
    pub counter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identified: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difference: Option<i64>,
    /// `reconciled`, `conflict`, `discrepancy` or `withheld`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostResidualSource {
    /// `parent` or `child`.
    pub role: String,
    /// A child recording's native agent id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontier: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    pub complete: bool,
    /// The distinct requests this recording identifies.
    #[serde(default)]
    pub requests: u64,
    /// The counters of those requests, a separate observation from every
    /// other recording's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identified: Option<CostCounters>,
}

/// Native session relations used to detect potentially overlapping recordings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostLineage {
    pub parent: Option<String>,
    pub children: Vec<CostRelative>,
    pub forked_from: Option<String>,
    /// Failed or bounded reads cannot establish absence of other relatives.
    pub unknown: Vec<String>,
    /// Children whose recordings the parent's own store holds under it, such
    /// as Claude subagents, named by native agent id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_children: Vec<CostNativeChild>,
}

/// A child recording held under its parent's, named by its native agent id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostNativeChild {
    pub agent: String,
    /// The agent type the harness recorded, in its own vocabulary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Whether the child's own recording is in the store.
    pub resolved: bool,
}

/// The agent type the delegated-agents plugin records for a native subagent
/// row that mirrors an agent-delegate run: it carries no consumption of its
/// own, and the run's consumption is the delegated session's.
pub const MIRROR_AGENT_TYPE: &str = "delegated-agents:run";

impl CostNativeChild {
    pub fn mirror(&self) -> bool {
        self.role.as_deref() == Some(MIRROR_AGENT_TYPE)
    }
}

/// A session key a cost read measures: a plain native id, or a native child
/// held under its parent's recording as `<parent>::<agent>`.
pub(crate) fn native_session_key(key: &str) -> bool {
    match key.split_once("::") {
        Some((parent, agent)) => plain_session_id(parent) && plain_session_id(agent),
        None => plain_session_id(key),
    }
}

pub(crate) fn plain_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn counted_reason(reason: &str, suffix: &str) -> bool {
    reason
        .strip_suffix(suffix)
        .is_some_and(|count| count.parse::<usize>().is_ok())
}

pub(crate) fn portable_lineage_reason(reason: &str) -> bool {
    matches!(
        reason,
        "lineage read failed"
            | "lineage read bounded"
            | "lineage identity mismatch"
            | "lineage relative unresolved"
            | "lineage relative is not a plain session id"
    ) || counted_reason(reason, " lineage notes")
}

pub(crate) fn portable_cost_reason(reason: &str) -> bool {
    matches!(
        reason,
        "the source recorded no usage counters"
            | "the read was bounded and withheld part of the source"
            | "the reader established no source frontier"
            | "abandoned branch consumption is outside the pinned reader's active branch"
            | "the counters cover the read-window, not the whole session"
            | "the counters cover the since-reset, not the whole session"
            | "retained measurement gap"
    ) || counted_reason(reason, " source read gaps")
        || counted_reason(reason, " selection warnings")
}

pub(crate) fn portable_quantity_reason(reason: &str) -> bool {
    matches!(
        reason,
        "recorded category is absent"
            | "recorded zero price accompanies nonzero or unknown usage"
            | "recorded price is invalid"
            | "quantity coverage was not retained"
            | "accounting basis is unknown"
            | "some counted events did not record this category"
            | "some counted events recorded no price"
            | "a request stopped before its usage was complete"
            | "some source records could not be read"
            | "the figure covers only the part of the recording the read reached"
            | "the counters restarted inside the recording"
            | "no accounting establishes what the figure covers"
            | "some requests could not be attributed to a model"
            | "the recorded counters do not determine this category"
            | "some records carrying usage were not read"
            | "recorded total is below its identified requests"
            | "recorded cost kind is unsupported"
            | "the price table has no row for the model that served the usage"
            | "some cache writes recorded no TTL"
            | "a price term set by the request may apply to requests the figure does not hold one by one"
            | "the recording does not establish whether a long-context surcharge was billed"
            | "no price row was in effect at the request's time"
    )
}

pub(crate) fn portable_residual_reason(reason: &str) -> bool {
    matches!(
        reason,
        "reconciliation read failed"
            | "reconciliation frontier differs from the measurement"
            | "retained reconciliation gap"
    )
}

/// A category tapes names by a short kebab-case token.
fn fixed_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// A versioned rule set, `<name>/<n>`.
fn versioned_token(value: &str) -> bool {
    value.split_once('/').is_some_and(|(name, version)| {
        fixed_token(name) && !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())
    })
}

/// The disjoint categories tapes qualifies.
pub(crate) const DISJOINT_CATEGORIES: [&str; 5] = [
    "uncached_input",
    "cache_read",
    "cache_write",
    "output",
    "reasoning",
];

const COUNTER_CATEGORIES: [&str; 5] = ["input", "output", "reasoning", "cache_read", "cache_write"];

pub(crate) fn portable_selection(selection: &str) -> bool {
    matches!(selection, "active-branch" | "retained-branches")
}

/// A model coordinate, `model` or `model#effort`, with at most one `#`. A
/// model is the id its provider gives it: ASCII letters, digits and `.-_:@+`,
/// in `/`-separated namespace segments (`z-ai/glm-5.3-flash`), none of them
/// empty, `.` or `..`, so no model reads as a path.
pub(crate) fn portable_coordinate(coordinate: &str) -> bool {
    let token = |part: &str| {
        !part.is_empty()
            && !matches!(part, "." | "..")
            && part.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '@' | '+')
            })
    };
    let mut parts = coordinate.split('#');
    coordinate.len() <= 256
        && parts
            .next()
            .is_some_and(|model| model.split('/').all(token))
        && parts
            .next()
            .is_none_or(|effort| !effort.contains('/') && token(effort))
        && parts.next().is_none()
}

pub(crate) fn portable_liveness(liveness: &str) -> bool {
    matches!(liveness, "live" | "ended" | "unknown")
}

impl CostDisjoint {
    pub(crate) fn portable(&self) -> bool {
        matches!(
            self.convention.input.as_str(),
            "excludes-cache" | "includes-cache-read"
        ) && matches!(
            self.convention.output.as_str(),
            "includes-reasoning" | "excludes-reasoning"
        ) && self.gaps.iter().flatten().all(|(category, reasons)| {
            DISJOINT_CATEGORIES.contains(&category.as_str())
                && reasons
                    .iter()
                    .all(|reason| portable_quantity_reason(reason))
        })
    }
}

impl CostEvents {
    pub(crate) fn portable(&self) -> bool {
        self.coverage
            .keys()
            .all(|key| key == "cost" || COUNTER_CATEGORIES.contains(&key.as_str()))
            && self
                .identity
                .account_namespace
                .as_deref()
                .is_none_or(|namespace| matches!(namespace, "established" | "unestablished"))
    }
}

impl CostResidual {
    pub(crate) fn portable(&self) -> bool {
        self.unavailable
            .as_deref()
            .is_none_or(portable_residual_reason)
            && self.rules.as_deref().is_none_or(versioned_token)
            && self.identity_coverage.as_deref().is_none_or(fixed_token)
            && self.missing.iter().all(|reason| fixed_token(reason))
            && self.counters.iter().all(|counter| {
                COUNTER_CATEGORIES.contains(&counter.counter.as_str())
                    && matches!(
                        counter.status.as_str(),
                        "reconciled" | "conflict" | "discrepancy" | "withheld"
                    )
                    && counter.reasons.iter().all(|reason| fixed_token(reason))
            })
            && self.sources.iter().all(|source| {
                matches!(source.role.as_str(), "parent" | "child")
                    && source.digest.as_deref().is_none_or(portable_cost_metadata)
                    && source.agent.as_deref().is_none_or(plain_session_id)
            })
    }
}

pub(crate) fn portable_cost_metadata(value: &str) -> bool {
    !value.contains(['/', '\\', '~']) && !value.chars().any(char::is_control)
}

pub(crate) fn portable_binding(
    harness: &str,
    session: &str,
    handle: &Option<String>,
    origin: &Option<String>,
    parent: &Option<String>,
) -> bool {
    [
        Some(harness),
        Some(session),
        handle.as_deref(),
        origin.as_deref(),
        parent.as_deref(),
    ]
    .into_iter()
    .flatten()
    .all(portable_cost_metadata)
}

impl CostFacts {
    /// Transport admits session relatives and fixed reason categories only.
    pub(crate) fn portable(&self) -> bool {
        portable_cost_metadata(&self.reader)
            && portable_cost_metadata(&self.calculation)
            && self
                .basis
                .as_deref()
                .is_none_or(|basis| matches!(basis, "recorded-total" | "summed-requests"))
            && self.coverage.as_deref().is_none_or(|coverage| {
                matches!(coverage, "session" | "read-window" | "since-reset")
            })
            && self
                .digest
                .as_ref()
                .is_none_or(|digest| portable_cost_metadata(digest))
            && self
                .harness_estimate_usd
                .is_none_or(|amount| amount.is_finite() && amount >= 0.0)
            && self.quantity_gaps.as_ref().is_none_or(|gaps| {
                gaps.iter().all(|(category, reasons)| {
                    matches!(
                        category.as_str(),
                        "input" | "output" | "reasoning" | "cache_read" | "cache_write" | "usd"
                    ) && reasons
                        .iter()
                        .all(|reason| portable_quantity_reason(reason))
                })
            })
            && self
                .incomplete
                .iter()
                .all(|reason| portable_cost_reason(reason))
            && self.lineage.as_ref().is_none_or(|lineage| {
                [&lineage.parent, &lineage.forked_from]
                    .into_iter()
                    .flatten()
                    .all(|id| plain_session_id(id))
                    && lineage.children.iter().all(|child| {
                        plain_session_id(&child.harness) && plain_session_id(&child.session)
                    })
                    && lineage
                        .unknown
                        .iter()
                        .all(|reason| portable_lineage_reason(reason))
                    && lineage.native_children.iter().all(|child| {
                        plain_session_id(&child.agent)
                            && child.role.as_deref().is_none_or(portable_cost_metadata)
                    })
            })
            && self.selection.as_deref().is_none_or(portable_selection)
            && self.liveness.as_deref().is_none_or(portable_liveness)
            && self.served_model.as_deref().is_none_or(portable_coordinate)
            && self.disjoint.as_ref().is_none_or(CostDisjoint::portable)
            && self.events.as_ref().is_none_or(CostEvents::portable)
            && self.residual.as_ref().is_none_or(CostResidual::portable)
            && self.spans.as_ref().is_none_or(CostSpans::portable)
            && self
                .by_model
                .iter()
                .all(|share| portable_coordinate(&share.model) && share.disjoint.portable())
    }
}

/// A child in its harness's native session namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostRelative {
    pub harness: String,
    pub session: String,
}

/// Token counters by tapes' categories, each absent when the source recorded
/// none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostCounters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
}

/// The journal artifact a run was dispatched against: the reference as it
/// was given, resolved the way `begin --from-journal` resolves one, and the
/// body it named then. A kind transition retires the artifact under its old
/// name, and the `supersedes` chain leads from either name to the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunJournalSubject {
    /// A filename in this project's journal, or `<journal-dir>::<file>`.
    pub file: String,
    /// `sha256:` over the body read at dispatch.
    pub digest: String,
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
    /// A production record kept before the range's trailers were read says
    /// nothing of them: it reads as unread and writes back without the key.
    #[test]
    fn a_production_record_without_trailer_implementers_reads_and_writes_unchanged() {
        let old = serde_json::json!({
            "implementer": {"actor": "lead", "harness": "claude", "model": "opus#high"},
            "following_brief": false
        });
        let production: DebtProduction = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(production.trailer_implementers, None);
        assert_eq!(production.trailer_note(), None);
        let written = serde_json::to_value(&production).unwrap();
        assert!(written.get("trailer_implementers").is_none(), "{written}");
        assert_eq!(written["following_brief"], false);

        let mut read = production.clone();
        read.trailer_implementers = Some(Vec::new());
        assert_eq!(
            read.trailer_note().as_deref(),
            Some("no Implemented-by trailers")
        );
        read.trailer_implementers = Some(vec!["claude:opus#xhigh".into()]);
        assert_eq!(
            read.trailer_note(),
            None,
            "the recorder's own model, at another effort"
        );
        read.trailer_implementers = Some(vec!["claude:opus#xhigh".into(), "codex:sol".into()]);
        assert_eq!(
            read.trailer_note().as_deref(),
            Some("Implemented-by trailers: claude:opus#xhigh, codex:sol")
        );
    }

    /// An audit assessment recorded before the authors were compared reads as
    /// naming no uncovered author and writes back without the key.
    #[test]
    fn an_assessment_without_uncovered_authors_reads_and_writes_unchanged() {
        let old = serde_json::json!({"independence": "eligible", "binding": "bound"});
        let assessment: AuditAssessment = serde_json::from_value(old).unwrap();
        assert!(assessment.uncovered_authors.is_empty());
        let written = serde_json::to_value(&assessment).unwrap();
        assert!(written.get("uncovered_authors").is_none(), "{written}");
    }

    /// Events recorded before the read fields existed carry neither key, read
    /// as the shapes they always meant, and serialize back without them.
    #[test]
    fn events_without_the_read_fields_read_and_write_unchanged() {
        let debt = serde_json::json!({
            "event_type": "debt-declared", "reason": "owed", "patchset_id": "ps-01",
            "missing": "independent-review", "coverage": []
        });
        let Payload::DebtDeclared { ineligible, .. } =
            serde_json::from_value::<Payload>(debt.clone()).unwrap()
        else {
            panic!("not a debt");
        };
        assert_eq!(ineligible, None);
        let rewritten =
            serde_json::to_value(serde_json::from_value::<Payload>(debt).unwrap()).unwrap();
        assert!(rewritten.get("ineligible").is_none(), "{rewritten}");

        let audit = serde_json::json!({
            "event_type": "audit-verdict-recorded", "revision": "h1", "verdict": "approved"
        });
        let Payload::AuditVerdictRecorded { assessment, .. } =
            serde_json::from_value::<Payload>(audit.clone()).unwrap()
        else {
            panic!("not an audit");
        };
        assert_eq!(assessment, None);
        let rewritten =
            serde_json::to_value(serde_json::from_value::<Payload>(audit).unwrap()).unwrap();
        assert!(rewritten.get("assessment").is_none(), "{rewritten}");
    }

    #[test]
    fn the_new_kind_sits_between_contributor_only_and_independent_review() {
        let order: Vec<_> = DebtMissing::ALL.iter().map(|kind| kind.as_str()).collect();
        assert_eq!(
            order,
            [
                "nothing-read",
                "merge-resolution-unread",
                "repair-unread",
                "contributor-only",
                "ineligible-review",
                "independent-review"
            ]
        );
        assert!(DebtMissing::ALL.windows(2).all(|pair| pair[0] < pair[1]));
        for kind in DebtMissing::ALL {
            let value = serde_json::to_value(kind).unwrap();
            assert_eq!(value, kind.as_str());
        }
    }

    #[test]
    fn route_versions_are_one_token() {
        assert!(validate_route_version("2026.10.12").is_ok());
        assert!(validate_route_version("").is_err());
        assert!(validate_route_version("a b").is_err());
        assert!(validate_route_version(&"x".repeat(65)).is_err());
    }

    #[test]
    fn coverage_session_relative_model_is_unrecorded() {
        let coverage: crate::model::DebtCoverage = serde_json::from_value(serde_json::json!({
            "reviewer": "reader", "model": "fixture-recording#high",
            "session": "fixture-recording", "effort": "high"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&coverage).unwrap()["model_recorded"],
            false
        );
        let value = serde_json::to_value(&coverage).unwrap();
        assert_eq!(value["session"], "fixture-recording");
        assert_eq!(
            serde_json::from_value::<super::DebtCoverage>(value).unwrap(),
            coverage
        );
    }

    #[test]
    fn coverage_json_retains_raw_models_and_marks_defects() {
        for model in [Some("unknown"), Some("m#none"), None] {
            let coverage =
                super::DebtCoverage::new("reader".into(), model.map(str::to_string), None, None);
            let value = serde_json::to_value(&coverage).unwrap();
            assert_eq!(value["model_recorded"], false);
            assert_eq!(
                value.get("model").and_then(serde_json::Value::as_str),
                model
            );
        }
        let coverage = super::DebtCoverage::new("reader".into(), Some("m#high".into()), None, None);
        assert!(serde_json::to_value(coverage)
            .unwrap()
            .get("model_recorded")
            .is_none());
    }

    #[test]
    fn producer_json_retains_raw_models_and_marks_defects() {
        for model in [
            Some("unknown"),
            Some("m#none"),
            Some("recording-key#low"),
            Some(""),
            None,
        ] {
            let identity = super::DebtIdentity::new(
                "writer".into(),
                None,
                Some("codex".into()),
                model.map(str::to_string),
                Some("recording-key".into()),
            );
            let value = serde_json::to_value(&identity).unwrap();
            assert_eq!(value["model_recorded"], false);
            assert_eq!(
                value.get("model").and_then(serde_json::Value::as_str),
                model
            );
            let roundtrip: super::DebtIdentity = serde_json::from_value(value).unwrap();
            assert_eq!(roundtrip, identity);
        }
        let identity = super::DebtIdentity::new(
            "writer".into(),
            None,
            Some("codex".into()),
            Some("m#high".into()),
            None,
        );
        assert!(serde_json::to_value(identity)
            .unwrap()
            .get("model_recorded")
            .is_none());
    }
    use super::*;

    #[test]
    fn cost_facts_transport_admits_only_fixed_accounting_categories() {
        let facts: CostFacts = serde_json::from_value(serde_json::json!({
            "reader": "reader", "calculation": "calculation", "frontier": 10,
            "selection": "retained-branches",
            "disjoint": {"convention": {"input": "excludes-cache", "output": "includes-reasoning"},
                "cache_write": {"ephemeral_5m": 1, "ephemeral_1h": 0, "ttl_unrecorded": 2}},
            "events": {"counted": 1, "active_branch": 1, "other_branches": 0, "repeats": 0,
                "coverage": {"input": {"measured": 1, "missing": 0, "unpriced": 0}},
                "identity": {"provider_response": 1, "recording_entry": 0, "unidentified": 0,
                    "account_namespace": "unestablished"}},
            "residual": {"rules": "tapes-claude-reconciliation/1", "scope_established": false,
                "identity_coverage": "validated-family", "missing": ["recorded-total-membership"],
                "counters": [{"counter": "input", "recorded": 5, "identified": 7, "difference": -2,
                    "status": "conflict", "reasons": ["request-conflict"]}],
                "sources": [{"role": "parent", "frontier": 10, "digest": "sha256:00", "complete": true}]},
        }))
        .unwrap();
        assert!(facts.portable());
        let refused = |edit: &dyn Fn(&mut CostFacts)| {
            let mut facts = facts.clone();
            edit(&mut facts);
            !facts.portable()
        };
        assert!(refused(&|f| f.selection = Some("/home/user/branch".into())));
        assert!(refused(&|f| f.liveness = Some("running".into())));
        assert!(refused(
            &|f| f.served_model = Some("/home/user/model".into())
        ));
        assert!(refused(&|f| f.served_model = Some("a#b#c".into())));
        assert!(refused(&|f| f.served_model = Some("home/../model".into())));
        assert!(refused(&|f| f.served_model = Some("~/model".into())));
        assert!(refused(&|f| f.served_model = Some("vendor//model".into())));
        for model in [
            "z-ai/glm-5.3-flash",
            "@or:z-ai:fp8:z-ai/glm-5.3-flash",
            "MiniMax-M2#high",
            "anthropic/claude-haiku-5.5:batch",
        ] {
            assert!(
                !refused(&|f| f.served_model = Some(model.into())),
                "{model}"
            );
        }
        assert!(refused(&|f| f
            .disjoint
            .as_mut()
            .unwrap()
            .convention
            .input = "native".into()));
        assert!(refused(&|f| {
            f.events.as_mut().unwrap().identity.account_namespace = Some("acct-123".into())
        }));
        assert!(refused(&|f| {
            f.events.as_mut().unwrap().coverage.insert(
                "/tmp/x".into(),
                CostEventCoverage {
                    measured: 0,
                    missing: 0,
                    unpriced: 0,
                },
            );
        }));
        let residual = |edit: &dyn Fn(&mut CostResidual)| {
            refused(&|f: &mut CostFacts| edit(f.residual.as_mut().unwrap()))
        };
        assert!(residual(
            &|r| r.counters[0].reasons = vec!["cannot read /home/user/a.jsonl".into()]
        ));
        assert!(residual(&|r| r.counters[0].status = "clamped".into()));
        assert!(residual(&|r| r.missing = vec!["~/.claude".into()]));
        assert!(residual(&|r| r.rules = Some("../rules".into())));
        assert!(residual(
            &|r| r.sources[0].digest = Some("/home/user/a.jsonl".into())
        ));
        assert!(residual(
            &|r| r.unavailable = Some("No such file: /home/user".into())
        ));
    }

    #[test]
    fn dispositions_without_evidence_event_id_still_deserialize() {
        for event_type in [
            "disposition-recorded",
            "audit-disposition-recorded",
            "post-integration-disposition-recorded",
        ] {
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
                } | Payload::PostIntegrationDispositionRecorded {
                    evidence_event_id: None,
                    ..
                }
            ));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ProvisionalRecipient {
    Replica { repository_id: String },
    Forge { reference: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityObserved {
    pub grant_event_id: String,
    pub holder_repository_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvisionalResolutionKind {
    SameHead,
    RevisedHead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionalResolution {
    pub provisional_event_id: String,
    pub delivered_patchset_id: String,
    pub accepted_patchset_id: String,
    pub kind: ProvisionalResolutionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionalPrerequisite {
    pub change_id: String,
    pub provisional_event_id: String,
    pub source_patchset_id: String,
    pub provisional_commit: String,
}
