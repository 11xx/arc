use crate::model::*;
use crate::rewrite::RewriteMap;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub const CHANGE_STATE_SCHEMA: &str = "arc-state/3";

#[derive(Debug, Clone, Serialize)]
pub struct Patchset {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub budget_raise_ids: Vec<String>,
    pub id: String,
    pub actor: String,
    pub model: Option<String>,
    pub harness: Option<String>,
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_link: Option<String>,
    /// Where `actor` came from. `None` on patchsets recorded before arc kept
    /// the provenance, which is unknown rather than declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_source: Option<ActorSource>,
    /// Subject the snapshot was taken for, when a lead ran delegated ceremony.
    pub on_behalf_of: Option<String>,
    /// Author coordinates carried from a mechanical replay or derived
    /// from completed, bound implementation runs, model included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited_authors: Vec<crate::model::DebtIdentity>,
    pub base: String,
    pub head: String,
    /// The commit an approval of this patchset still stands on: `head` as
    /// recorded, followed only through rewrites that changed a signature and
    /// nothing else. It differs from `head` once a recorded rewrite changed
    /// content, which is what leaves an approval stale.
    #[serde(skip)]
    pub approved_head: String,
    pub merge_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief_ref: Option<BriefRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief_version: Option<usize>,
    pub author: Option<GitIdentity>,
    pub committer: Option<GitIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributors: Vec<String>,
    pub claim_id: Option<String>,
    pub claim_actor: Option<String>,
    /// Journal artifacts the patchset was recorded as being framed by, each
    /// with the body digest read at record time. Empty on patchsets recorded
    /// without links, which is not a claim that none framed the work.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub journal_refs: Vec<crate::model::JournalArtifactRef>,
    /// The external thread the work belongs to, as a scheme and an id.
    /// `None` when none was recorded; arc never infers one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<crate::model::ExternalThreadRef>,
    /// The candidate a promotion recorded this patchset from, with the
    /// selection that permitted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<crate::model::PatchsetCandidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_mismatch: Option<bool>,
    pub created_at: DateTime<Utc>,
}

impl Patchset {
    /// The author policy attributes this snapshot to: the subject when taken on
    /// behalf of one, otherwise the invoker.
    pub fn effective_author(&self) -> &str {
        self.on_behalf_of.as_deref().unwrap_or(&self.actor)
    }

    /// The coordinate this recording made for its own recorder. The model,
    /// harness and session beside a patchset's actor describe that session,
    /// not an `--on-behalf-of` subject, so the subject is left empty here.
    fn recorded_coordinate(&self) -> DebtIdentity {
        DebtIdentity::new(
            self.actor.clone(),
            None,
            self.harness.clone(),
            self.model.clone(),
            self.session.clone(),
        )
    }

    /// The contributor that matches a reviewer, using the effective author as
    /// the compatibility fallback for patchsets without a contributor set.
    pub fn contributor_match(&self, reviewer: &str) -> Option<&str> {
        if self.contributors.is_empty() {
            (self.effective_author() == reviewer).then_some(self.effective_author())
        } else {
            self.contributors
                .iter()
                .find(|contributor| contributor == &reviewer)
                .map(String::as_str)
        }
    }

    /// Whether the identity used by the compatibility fallback was assumed by
    /// arc rather than declared by the caller.
    pub fn author_assumed(&self) -> bool {
        self.contributors.is_empty()
            && author_assumed(self.on_behalf_of.as_deref(), self.actor_source)
    }

    pub fn contributors_source(&self) -> &'static str {
        if self.contributors.is_empty() {
            "declared-by-invoker"
        } else {
            "declared"
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Brief {
    pub event_id: String,
    pub ts: DateTime<Utc>,
    pub actor: String,
    /// Subject a lead recorded this brief for. The brief's effective author is
    /// this when present, matching how a verdict's identity is read — the two
    /// must agree, or comparing them compares different things.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    /// Coordinates of the identity that recorded this version, so a reader can
    /// tell what produced the contract and not only who signed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_link: Option<String>,
    pub title: Option<String>,
    pub body: String,
    pub caused_by: Vec<BriefCause>,
    pub base_revision: Option<String>,
    pub acceptance_probes: Vec<AcceptanceProbe>,
    pub plan_ref: Option<String>,
    pub plan_slice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_source: Option<PlanSource>,
    /// Reads the contract requires of whoever answers it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub must_read: Vec<crate::model::ReadRequirement>,
}

/// The change's latest changelog record: its release copy, or the record
/// that it needs none.
#[derive(Debug, Clone, Serialize)]
pub struct ChangelogEntry {
    pub event_id: String,
    /// Empty when the record says the change needs no entry.
    pub category: String,
    /// Empty when the record says the change needs no entry.
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    /// Why the change needs no entry, when that is what was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_entry_reason: Option<String>,
    pub actor: String,
    pub on_behalf_of: Option<String>,
    pub harness: Option<String>,
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_link: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ChangelogEntry {
    pub fn effective_author(&self) -> &str {
        self.on_behalf_of.as_deref().unwrap_or(&self.actor)
    }

    /// Whether the record is release copy the projection emits, rather than
    /// the record that the change needs none.
    pub fn is_entry(&self) -> bool {
        self.no_entry_reason.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitIdentity {
    pub name: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimIdentity {
    pub actor: String,
    pub harness: String,
    pub session: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageProgress {
    pub stage: ClaimStage,
    pub note: Option<String>,
    pub blocker: Option<BlockerRef>,
    pub changed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaimState {
    pub claim_id: String,
    pub owner: ClaimIdentity,
    pub ttl_seconds: u64,
    pub stage_budgets: BTreeMap<StageBudget, u64>,
    pub claimed_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    pub progress: Option<StageProgress>,
    /// The claim this one displaced when it was acquired, kept so a reader
    /// of the live claim can see whose lease it ended and why. A renewal
    /// carries it forward: it describes the acquisition, not the last write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub displaced: Option<DisplacedClaim>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimTiming {
    pub active: bool,
    pub expired: bool,
    pub stale: bool,
    pub expires_at: DateTime<Utc>,
    pub stage: String,
    pub stage_started_at: DateTime<Utc>,
    pub age_seconds: u64,
    pub budget_seconds: Option<u64>,
}

/// Derive every clock-sensitive claim property from one injected instant.
/// Command checks, status, alternatives, watch, and replay all share this
/// helper so recorded events remain deterministic while wall-clock views do
/// not drift into subtly different definitions.
pub fn claim_timing_at(claim: &ClaimState, now: DateTime<Utc>) -> ClaimTiming {
    let expires_at = claim
        .last_activity_at
        .checked_add_signed(seconds_delta(claim.ttl_seconds))
        .unwrap_or(DateTime::<Utc>::MAX_UTC);
    let expired = now >= expires_at;
    let (stage, stage_started_at, budget_seconds) = match &claim.progress {
        Some(progress) => (
            progress.stage.as_str().to_string(),
            progress.changed_at,
            claim
                .stage_budgets
                .get(&progress.stage.budget_key())
                .copied(),
        ),
        None => (
            StageBudget::Launch.as_str().to_string(),
            claim.claimed_at,
            claim.stage_budgets.get(&StageBudget::Launch).copied(),
        ),
    };
    let age_seconds = elapsed_seconds(stage_started_at, now);
    let stale = !expired && budget_seconds.is_some_and(|budget| age_seconds > budget);
    ClaimTiming {
        active: !expired,
        expired,
        stale,
        expires_at,
        stage,
        stage_started_at,
        age_seconds,
        budget_seconds,
    }
}

fn elapsed_seconds(since: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    now.signed_duration_since(since).num_seconds().max(0) as u64
}

fn seconds_delta(seconds: u64) -> TimeDelta {
    let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
    TimeDelta::try_seconds(seconds).unwrap_or(TimeDelta::MAX)
}

#[derive(Debug, Clone, Serialize)]
pub struct DispositionEntry {
    pub origin_replica_id: String,
    pub gating_eligible: bool,
    pub event_id: String,
    pub status: DispositionStatus,
    pub commit: Option<String>,
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_event_id: Option<String>,
    pub actor: String,
    pub supersedes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindingState {
    pub forge_sources: Vec<crate::forge_observation::ForgeSource>,
    pub id: String,
    pub blocking: bool,
    pub severity: Severity,
    pub summary: String,
    pub body: Option<String>,
    pub patchset_id: Option<String>,
    pub anchor: Option<Anchor>,
    pub origin_event: String,
    pub reported_by: String,
    /// Subject the finding was filed for when a lead ran the ceremony.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    /// Where `reported_by` came from. `None` on findings recorded before arc
    /// kept the provenance, which is unknown rather than declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_source: Option<ActorSource>,
    /// Dispositions recorded while the change is open. On an integrated
    /// change they are the finding state it shipped with.
    pub dispositions: Vec<DispositionEntry>,
    /// Dispositions recorded after integration, for a finding open when the
    /// change shipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after_integration: Vec<DispositionEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<ReplyEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplyEntry {
    pub event_id: String,
    pub actor: String,
    pub body: String,
}

impl FindingState {
    pub fn effective_author(&self) -> &str {
        self.on_behalf_of.as_deref().unwrap_or(&self.reported_by)
    }

    pub fn author_assumed(&self) -> bool {
        author_assumed(self.on_behalf_of.as_deref(), self.actor_source)
    }

    /// Disposition tips: dispositions not superseded by any later one.
    /// One tip = its status governs; several = contested.
    pub fn tips(&self) -> Vec<&DispositionEntry> {
        eligible_disposition_tips(&self.dispositions)
    }

    pub fn contested(&self) -> bool {
        self.tips().len() > 1
    }

    pub fn effective_status(&self) -> Option<DispositionStatus> {
        let tips = self.tips();
        if tips.len() == 1 {
            Some(tips[0].status)
        } else {
            None
        }
    }

    /// The tips among the dispositions recorded after integration.
    pub fn after_integration_tips(&self) -> Vec<&DispositionEntry> {
        eligible_disposition_tips(&self.after_integration)
    }

    /// Whether the finding's own dispositions release it. For a review
    /// finding on an integrated change they are the state it shipped with.
    pub fn released(&self) -> bool {
        history_releases(&self.dispositions)
    }

    /// Whether the finding's applicable history releases it: the dispositions
    /// recorded after integration when there are any, otherwise its own.
    pub fn currently_released(&self) -> bool {
        if self.after_integration_tips().is_empty() {
            self.released()
        } else {
            history_releases(&self.after_integration)
        }
    }

    /// How the finding stands after integration, as readers print it: the
    /// single tip's status, `contested` for several, `None` for none.
    pub fn after_integration_status(&self) -> Option<String> {
        match self.after_integration_tips().as_slice() {
            [] => None,
            [tip] => Some(format!("{:?}", tip.status).to_lowercase()),
            _ => Some("contested".into()),
        }
    }

    pub fn blocks_integration(&self) -> bool {
        if !self.blocking {
            return false;
        }
        match self.effective_status() {
            Some(status) => !status.releases_block(),
            None => true, // open or contested
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VerdictEntry {
    #[serde(flatten)]
    pub basis: crate::review_clearance::Basis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiebreak_reviewer: Option<crate::policy::TiebreakReviewer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiebreak_target: Option<String>,
    pub forge_sources: Vec<crate::forge_observation::ForgeSource>,
    pub forge_frontier: Option<Vec<String>>,
    pub origin_replica_id: String,
    pub event_id: String,
    pub patchset_id: String,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_observed: Option<crate::model::AuthorityObserved>,
    pub causes: Vec<ReviewCause>,
    pub body: Option<String>,
    /// Model identity recorded on the verdict event, when one was declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The model the reviewer's session store observed where it
    /// contradicted the recorded one. Independence judges the verdict under
    /// both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_model: Option<String>,
    /// The originating session used to validate the recorded model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Why this verdict is owed corroboration, when the caller said it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisional: Option<String>,
    /// The routing version that selected the reviewer. Absent means unrouted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_version: Option<String>,
    /// Routing's assessment of the reviewer, when the verdict carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<AuditAssessment>,
    pub actor: String,
    /// Subject the verdict was cast for, when a lead reviewed on behalf of one.
    pub on_behalf_of: Option<String>,
    /// Where `actor` came from. `None` on verdicts recorded before arc kept
    /// the provenance, which is unknown rather than declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_source: Option<ActorSource>,
    /// How this verdict relates to the verdict tips it observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<VerdictRelation>,
    /// The receipt item this verdict was ingested from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<crate::model::ReceiptReference>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl VerdictEntry {
    /// The author policy attributes this verdict to: the subject when cast on
    /// behalf of one, otherwise the invoker.
    pub fn effective_author(&self) -> &str {
        self.on_behalf_of.as_deref().unwrap_or(&self.actor)
    }

    /// The models independence judges this verdict under.
    pub fn judged_models(&self) -> Vec<crate::model::ReaderModel> {
        crate::model::ReaderModel::judged(self.model.as_deref(), self.observed_model.as_deref())
    }

    /// Whether arc invented the identity this verdict is attributed to. A
    /// delegated subject is always somebody's claim; an invoker is assumed
    /// only when arc took it from git config or the harness session with
    /// nobody offering one.
    pub fn author_assumed(&self) -> bool {
        author_assumed(self.on_behalf_of.as_deref(), self.actor_source)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadyToSendEntry {
    pub event_id: String,
    pub patchset_id: String,
    pub head: String,
    pub history: String,
    pub target_branch: Option<String>,
    pub target_revision: Option<String>,
    pub authorization: crate::model::AuthorizationBasis,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExternalVerdictEntry {
    pub origin_replica_id: String,
    pub source: &'static str,
    pub event_id: String,
    pub revision: String,
    pub verdict: crate::model::ExternalVerdict,
    pub decided_by: String,
    pub reference: String,
    pub findings: Vec<crate::model::ExternalFinding>,
    pub recorded_by: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

trait TipEntry {
    fn event_id(&self) -> &str;
    fn supersedes(&self) -> &[String];
    fn counts_as_tip(&self) -> bool {
        true
    }
}

/// Append the dispositions a verdict or audit event carries to the findings
/// they name, as separate disposition events of that kind would append them.
/// A finding the event does not know, or one named twice, refuses the event.
fn apply_inline_dispositions(
    findings: &mut BTreeMap<String, FindingState>,
    ev: &Event,
    dispositions: &[crate::model::InlineDisposition],
    kind: &str,
) -> Result<()> {
    let mut named = BTreeSet::new();
    for inline in dispositions {
        if !named.insert(inline.finding_id.as_str()) {
            bail!(
                "event {} disposes {kind} {:?} twice",
                ev.event_id,
                inline.finding_id
            );
        }
        let Some(finding) = findings.get_mut(&inline.finding_id) else {
            bail!(
                "event {} disposes unknown {kind} {:?}",
                ev.event_id,
                inline.finding_id
            );
        };
        finding.dispositions.push(DispositionEntry {
            origin_replica_id: ev.repository_id.clone(),
            gating_eligible: true,
            event_id: ev.event_id.clone(),
            status: inline.status,
            commit: None,
            evidence: inline.evidence.clone(),
            evidence_event_id: None,
            actor: ev.actor.clone(),
            supersedes: inline.supersedes.clone(),
        });
    }
    Ok(())
}

/// Whether a disposition history releases its finding: a single tip that is
/// resolved, accepted-risk, or obsolete. Several tips are contested and
/// release nothing.
fn eligible_disposition_tips(entries: &[DispositionEntry]) -> Vec<&DispositionEntry> {
    let eligible: Vec<_> = entries
        .iter()
        .filter(|entry| entry.gating_eligible)
        .collect();
    let superseded: BTreeSet<_> = eligible
        .iter()
        .flat_map(|entry| entry.supersedes.iter().map(String::as_str))
        .collect();
    eligible
        .into_iter()
        .filter(|entry| !superseded.contains(entry.event_id.as_str()))
        .collect()
}

fn history_releases(entries: &[DispositionEntry]) -> bool {
    matches!(eligible_disposition_tips(entries).as_slice(), [tip] if tip.status.releases_block())
}

fn observed_tips<T: TipEntry>(entries: &[T]) -> Vec<&T> {
    let superseded: Vec<&str> = entries
        .iter()
        .flat_map(|entry| entry.supersedes().iter().map(String::as_str))
        .collect();
    entries
        .iter()
        .filter(|entry| entry.counts_as_tip() && !superseded.contains(&entry.event_id()))
        .collect()
}

impl TipEntry for DispositionEntry {
    fn event_id(&self) -> &str {
        &self.event_id
    }

    fn supersedes(&self) -> &[String] {
        &self.supersedes
    }
}

impl TipEntry for VerdictEntry {
    fn event_id(&self) -> &str {
        &self.event_id
    }

    fn supersedes(&self) -> &[String] {
        match self.relation.as_ref() {
            Some(VerdictRelation::Supersedes { observed }) => observed,
            _ => &[],
        }
    }

    fn counts_as_tip(&self) -> bool {
        !matches!(
            self.relation.as_ref(),
            Some(VerdictRelation::Corroborates { .. })
        )
    }
}

/// An effective author is assumed when nobody declared the actor — arc took it
/// from git config or the harness session — and nobody supplied a subject. Provenance recorded before arc kept it is *unknown*
/// rather than assumed: an old event says nothing either way, and treating
/// silence as an invention would retroactively invalidate approvals that were
/// valid when they were made.
fn author_assumed(on_behalf_of: Option<&str>, source: Option<ActorSource>) -> bool {
    on_behalf_of.is_none() && source.is_some_and(|source| !source.declared())
}

/// A role assertion as an identity. A human value is its own actor, and an
/// agent coordinate keeps its harness and model beside the value as the
/// actor it was declared as.
pub(crate) fn identity_from_role_value(value: &str) -> DebtIdentity {
    let value = value.trim();
    if !value.contains('<') {
        if let Some((harness, rest)) = value.split_once(':') {
            if !harness.trim().is_empty() && !rest.trim().is_empty() {
                return DebtIdentity::new(
                    value.to_string(),
                    None,
                    Some(harness.trim().to_string()),
                    Some(rest.trim().to_string()),
                    None,
                );
            }
        }
    }
    DebtIdentity::new(value.to_string(), None, None, None, None)
}

/// A plan header's planner as an identity, with a session or model standing
/// for an actor the header did not name.
fn identity_from_planner(planner: &PlannerIdentity) -> DebtIdentity {
    let actor = planner
        .actor
        .clone()
        .or_else(|| planner.session.clone())
        .or_else(|| planner.model.clone())
        .unwrap_or_default();
    DebtIdentity::new(
        actor,
        None,
        planner.harness.clone(),
        planner.model.clone(),
        planner.session.clone(),
    )
}

/// Whether an identity is the reviewer `actor` at any of `models`, compared
/// the way independence compares identities: the actor whole, or a model
/// without its effort.
fn identity_names(identity: &DebtIdentity, actor: &str, models: &[&str]) -> bool {
    if identity.effective_actor() == actor {
        return true;
    }
    let Some(recorded) = identity.model.as_deref() else {
        return false;
    };
    let recorded = recorded.split('#').next().unwrap_or(recorded).trim();
    !recorded.is_empty()
        && models
            .iter()
            .any(|model| model.split('#').next().unwrap_or(model).trim() == recorded)
}

/// Where a declared review obligation stands.
///
/// Distinct from readiness: `ChangeState::debt_outstanding` is true only for
/// [`DebtStanding::Owed`], so a debt on a change that has not integrated is
/// neither owed nor discharged. It is declared, and no review has answered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DebtStanding {
    /// Declared on a change that has not integrated.
    Declared,
    /// The change integrated and no eligible review has answered the debt.
    Owed,
    /// The change integrated and an eligible review answered the debt;
    /// `Debt::discharged_by` names it.
    Discharged,
}

impl DebtStanding {
    /// The one line a presentation gives the standing of a debt.
    pub fn label(self) -> &'static str {
        match self {
            DebtStanding::Declared => "declared, not yet integrated",
            DebtStanding::Owed => "owed",
            DebtStanding::Discharged => "discharged",
        }
    }
}

/// A declared review obligation and where it stands.
#[derive(Debug, Clone, Serialize)]
pub struct Debt {
    pub origin_replica_id: String,
    pub event_id: String,
    pub reason: String,
    /// The patchset this waiver was declared against, if it waived anything.
    pub patchset_id: Option<String>,
    pub actor: String,
    pub on_behalf_of: Option<String>,
    pub model: Option<String>,
    pub harness: Option<String>,
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_link: Option<String>,
    pub declared_at: chrono::DateTime<chrono::Utc>,
    /// What the versioned obligation says was missing. Absent on the
    /// pre-versioned obligation, whose meaning is legacy independent-review
    /// debt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<DebtMissing>,
    /// Verdict coverage captured when the versioned obligation was declared.
    /// An empty array means the bound patchset had no verdicts; absence is the
    /// legacy shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Vec<DebtCoverage>>,
    /// Who planned and who implemented the work the obligation covers. Absent
    /// when nothing was snapshotted, and on the legacy shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production: Option<DebtProduction>,
    /// The read an `ineligible-review` obligation names, with the reviewer
    /// read off that verdict or audit event and the audits since that did not clear
    /// it. Absent on every other kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ineligible: Option<DebtRead>,
    /// The approval and bases a `replayed-unread` obligation names. Absent on
    /// every other kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replayed: Option<crate::model::ReplayedRead>,
    /// What the owed review must cover, as declared. Absent, the
    /// obligation's extent is not established.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extent: Option<crate::extent::Extent>,
    /// Under `review_clearance = "assessed"`, the reads since the
    /// declaration that find without clearing, listed beside the obligation
    /// and never counted against it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub findings_reads: Vec<FindingsRead>,
    /// The reads since the declaration that would discharge the obligation
    /// but for their extent: listed beside it as partial coverage, never
    /// combined, never counted against it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub partial_coverage: Vec<PartialCoverage>,
    /// The first verdict or audit no [`DischargeBar`] bars, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discharged_by: Option<DebtCoverage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discharged_by_event_id: Option<String>,
    /// How the discharging read covers the obligation: `covered` when its
    /// extent covers the obligation's, `not-established` when neither
    /// recorded one. Absent while nothing discharges it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discharge_extent: Option<&'static str>,
    /// Where the obligation stands, read from the closure and the discharge
    /// predicate once every event is in. `discharged_by` is the discharger
    /// only when this is [`DebtStanding::Discharged`].
    pub standing: DebtStanding,
}

/// The read an `ineligible-review` debt names, as the ledger reads it now.
///
/// `record` is what was declared. The reviewer and the model are copied from
/// the verdict or audit event it names, so they are as recorded there and never a
/// tier or a family. `later_reads` lists what has been recorded since without
/// clearing the obligation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DebtRead {
    #[serde(flatten)]
    pub record: IneligibleRead,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer_model: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub later_reads: Vec<LaterRead>,
}

impl DebtRead {
    /// One line naming what the read was and what routing said of it.
    pub fn label(&self) -> String {
        let reviewer = match (&self.reviewer, &self.reviewer_model) {
            (Some(reviewer), Some(model)) => format!("{reviewer} ({model})"),
            (Some(reviewer), None) => format!("{reviewer} (model unrecorded)"),
            (None, _) => "reviewer unavailable".to_string(),
        };
        let mut line = format!(
            "ineligible read: {}; reviewer {reviewer}; {}",
            self.record.reasons(),
            self.record.assessment.as_str()
        );
        if let Some(version) = &self.record.route_version {
            line.push_str(&format!("; route {version}"));
        }
        if !self.later_reads.is_empty() {
            line.push_str(&format!(
                "; {} later read{} did not clear it",
                self.later_reads.len(),
                if self.later_reads.len() == 1 { "" } else { "s" }
            ));
        }
        line
    }

    /// Triage within the kind: a read below the author's floor is the weakest.
    pub fn below_floor(&self) -> bool {
        self.record
            .qualifiers
            .contains(&ReviewQualifier::BelowFloor)
    }
}

/// A verdict or audit that judged the work an obligation covers but, under
/// `review_clearance = "assessed"`, does not clear it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindingsRead {
    /// Always `findings`: the kind of read this is.
    pub kind: &'static str,
    pub event_id: String,
    /// `verdict` or `audit`.
    pub source: &'static str,
    pub verdict: Verdict,
    pub reviewer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Why it does not clear, a stable name.
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub qualifiers: Vec<ReviewQualifier>,
    /// The command whose answer would let a read clear.
    pub remedy: &'static str,
}

impl FindingsRead {
    /// One line naming the read and why it does not clear.
    pub fn label(&self) -> String {
        let model = self.model.as_deref().unwrap_or("model unrecorded");
        format!(
            "findings read {} ({} {:?} by {} ({model})): {}; remedy `{}`",
            self.event_id, self.source, self.verdict, self.reviewer, self.reason, self.remedy
        )
    }
}

/// A read that would discharge an obligation but for its extent: a scoped
/// read of a whole obligation, a read of another subject, or one whose
/// entries leave part of the obligation unread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PartialCoverage {
    /// Always `partial`: the kind of coverage this read gives.
    pub kind: &'static str,
    pub event_id: String,
    /// `verdict` or `audit`.
    pub source: &'static str,
    pub verdict: Verdict,
    pub reviewer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What the read covered. Absent, its extent is not established.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extent: Option<crate::extent::Extent>,
    /// Why it does not cover the obligation, a stable name.
    pub reason: &'static str,
    /// The obligation paths the read leaves uncovered or excludes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

impl PartialCoverage {
    /// One line naming the read and what it leaves uncovered.
    pub fn label(&self) -> String {
        let model = self.model.as_deref().unwrap_or("model unrecorded");
        let extent = self
            .extent
            .as_ref()
            .map(crate::extent::Extent::label)
            .unwrap_or_else(|| "extent not established".into());
        format!(
            "partial coverage {} ({} {:?} by {} ({model}); {extent}): {}",
            self.event_id, self.source, self.verdict, self.reviewer, self.reason
        )
    }
}

/// An audit recorded against an `ineligible-review` debt that did not clear
/// it: listed beside the obligation, never counted against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaterRead {
    pub event_id: String,
    pub reviewer: String,
    /// The model the audit recorded, kept raw; `model_recorded` is `false`
    /// beside it when it is missing, a placeholder or the audit's session id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_recorded: Option<bool>,
    /// The audit's session, which a model is judged against.
    #[serde(skip)]
    pub session: Option<String>,
    /// Routing's reading, when the audit carried an assessment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub independence: Option<Independence>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub qualifiers: Vec<ReviewQualifier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_version: Option<String>,
    /// Whether the assessment describes the auditor that recorded it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<AuditorBinding>,
    /// The implementer models the assessment leaves out.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncovered_authors: Vec<String>,
}

impl Debt {
    /// Whether `event` declares this undischarged obligation again: the same
    /// patchset, reason, kind, coverage, and production, by the same declarer.
    pub fn restated_by(&self, event: &Event) -> bool {
        let Payload::DebtDeclared {
            reason,
            patchset_id,
            missing,
            coverage,
            production,
            ineligible,
            replayed,
            extent,
        } = &event.payload
        else {
            return false;
        };
        self.discharged_by.is_none()
            && self.reason == *reason
            && self.patchset_id == *patchset_id
            && self.missing == Some(*missing)
            && self.coverage.as_ref() == Some(coverage)
            && self.production == *production
            && self.ineligible.as_ref().map(|read| &read.record) == ineligible.as_ref()
            && self.replayed == *replayed
            && self.extent == *extent
            && self.actor == event.actor
            && self.on_behalf_of == event.on_behalf_of
    }
}

/// Permission for dirty evidence to count, at one revision.
///
/// Bound the way the thing it excuses is bound: gate evidence counts only at
/// the change's own head, so the waiver covers that revision and dies at the
/// next commit rather than standing as an open exemption.
#[derive(Debug, Clone, Serialize)]
pub struct DirtyTreeWaiver {
    pub origin_replica_id: String,
    pub event_id: String,
    pub reason: String,
    /// The head this was declared at, and the only revision it covers.
    pub revision: String,
    pub actor: String,
    pub declared_at: chrono::DateTime<chrono::Utc>,
}

/// One fact a session judged load-bearing mid-work.
#[derive(Debug, Clone, Serialize)]
pub struct KeptContext {
    pub event_id: String,
    pub kind: KeptKind,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// Events on this change the fact cites as its grounds. Additive in
    /// `arc-state/3` and `arc-status/27`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cites: Vec<String>,
    pub actor: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditVerdictEntry {
    #[serde(flatten)]
    pub basis: crate::review_clearance::Basis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiebreak_reviewer: Option<crate::policy::TiebreakReviewer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiebreak_target: Option<String>,
    pub origin_replica_id: String,
    pub event_id: String,
    /// The integrated revision reviewed.
    pub revision: String,
    pub verdict: Verdict,
    pub body: Option<String>,
    /// Model identity recorded on the audit event, when one was declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The model the auditor's session store observed where it contradicted
    /// the recorded one. Independence judges the audit under both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_model: Option<String>,
    /// The originating session used to validate the recorded model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The routing version that selected the auditor. Absent means unrouted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_version: Option<String>,
    /// Routing's assessment of the auditor, when the caller supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<AuditAssessment>,
    /// The non-authoring roles the auditor held on the audited work.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held_roles: Vec<crate::model::ReviewRole>,
    pub actor: String,
    pub on_behalf_of: Option<String>,
    /// Where `actor` came from. `None` on audits recorded before arc kept the
    /// provenance, which is unknown rather than declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_source: Option<ActorSource>,
    /// The receipt item this audit was ingested from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<crate::model::ReceiptReference>,
    /// What the auditor read. Absent, its extent is not established.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extent: Option<crate::extent::Extent>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl AuditVerdictEntry {
    pub fn effective_author(&self) -> &str {
        self.on_behalf_of.as_deref().unwrap_or(&self.actor)
    }

    /// The models independence judges this audit under.
    pub fn judged_models(&self) -> Vec<crate::model::ReaderModel> {
        crate::model::ReaderModel::judged(self.model.as_deref(), self.observed_model.as_deref())
    }

    /// Whether arc invented the identity this audit is attributed to rather
    /// than somebody declaring it.
    pub fn author_assumed(&self) -> bool {
        author_assumed(self.on_behalf_of.as_deref(), self.actor_source)
    }
}

/// A recorded reference replay could not follow because the only map that
/// carried it to the revision it names was withdrawn. The event stays in the
/// ledger and the change stays readable; the reference counts for nothing
/// until a map carrying it is recorded again.
#[derive(Debug, Clone, Serialize)]
pub struct SetAsideReference {
    /// The event whose reference is set aside.
    pub event_id: String,
    /// `post-integration-verification` sets the whole receipt aside;
    /// `falsification` sets aside only the passing run's falsification claim.
    pub reference: &'static str,
    /// The revision the reference names.
    pub revision: String,
    /// The revision it has to follow from: the integrated commit, or the
    /// failure's recorded revision.
    pub follows: String,
}

/// A check run against the integrated tree after the change integrated.
///
/// Kept apart from `verifications`, which hold the gate evidence a decision
/// can count, so a receipt never reads as evidence the integration had.
#[derive(Debug, Clone, Serialize)]
pub struct PostIntegrationVerificationEntry {
    pub event_id: String,
    /// Always true: every reader that labels evidence by when it was observed
    /// finds it here without knowing which list it came from.
    pub after_integration: bool,
    pub gate: Option<String>,
    pub command: String,
    /// The revision the producer tested.
    pub revision: String,
    /// That revision's tree, which equalled the integrated commit's tree.
    pub tree: String,
    pub integrated_commit: String,
    pub result: VerifyResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    pub hostname: String,
    pub runner: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<crate::model::EnvironmentEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// When the receipt was recorded, which is when arc observed it.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl PostIntegrationVerificationEntry {
    pub fn effective_producer(&self) -> &str {
        self.on_behalf_of.as_deref().unwrap_or(&self.actor)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_pass: Option<crate::base_pass::Accounting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaration: Option<String>,
    pub event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<crate::model::DeltaSelection>,
    pub run_id: Option<String>,
    pub probe: Option<ProbeEvidenceRef>,
    pub gate: Option<String>,
    /// The command that ran. Empty, and omitted, on a bypass, where nothing
    /// ran.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// The timeout the gate declared when this ran. `None` predates the field
    /// or means the gate declared none — unknown, not unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    pub revision: String,
    /// The commit the evidence was recorded at. `revision` follows recorded
    /// rewrites to the successor; this stays where the run or judgement was
    /// recorded.
    #[serde(skip)]
    pub recorded_revision: String,
    pub result: VerifyResult,
    pub attested: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub timing: crate::verification_timing::VerificationTiming,
    pub output_tail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_artifact: Option<crate::verification_output::OutputArtifact>,
    pub timed_out: bool,
    /// Where the command ran. Empty, and omitted, on a bypass.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hostname: String,
    pub runner: Option<String>,
    /// The judgement recorded in place of a run, present exactly when
    /// `result` is `bypassed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bypass: Option<BypassEntry>,
    /// The tree the command ran against, and whether it differed from the
    /// revision's own. `None` is unknown — attested evidence, or an event
    /// written before arc recorded it — never a claim that the tree was clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested_tree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_dirty: Option<bool>,
    /// Which kind of dirt the tree carried. Absent on evidence recorded
    /// before the split, which is not the same as clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_dirty_tracked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_dirty_untracked: Option<bool>,
    #[serde(default, skip_serializing_if = "is_false_ref")]
    pub tree_moved: bool,
    /// The tree of the revision this evidence names, which is the content the
    /// gate holds for. `None` on evidence written before arc recorded it; the
    /// revision resolves it while the commit is reachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    /// The target head this evidence's tree was merged against, on evidence
    /// for a synthesized merge. A target that has moved since makes a
    /// different merge, and this evidence is about the earlier one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against_target: Option<String>,
    /// The recorded failure this evidence answers, absent when the check was
    /// never seen to fail before it passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub falsification: Option<Falsification>,
    /// The failure arc derived this pass follows, kept apart from the declared
    /// `falsification` because it decides nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub falsification_inferred: Option<InferredFalsification>,
    /// The environment this evidence was produced in, when the gate declared
    /// an environment probe. `None` on evidence written before arc recorded
    /// environments, which satisfies only probe-less gates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<crate::model::EnvironmentEvidence>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Who judged a gate not to need its run, and why.
#[derive(Debug, Clone, Serialize)]
pub struct BypassEntry {
    pub actor: String,
    pub reason: String,
    /// Historical qualification text recorded with the bypass.
    pub qualification: String,
}

/// Why a falsification reference cannot stand, phrased for whoever offered it.
///
/// A reference that names a pass, a different check, or a revision other than
/// the one the referenced failure was recorded at asserts a discrimination
/// that did not happen; admitting it would make the record weaker than no
/// record, because it would read as established. `None` means it holds.
///
/// `prior` is the evidence recorded on this change before the evidence being
/// judged, so a reference can only ever point backwards.
pub fn falsification_mismatch(
    prior: &[VerificationEntry],
    gate: Option<&str>,
    command: &str,
    falsification: &Falsification,
) -> Option<String> {
    let Some(referenced) = prior
        .iter()
        .find(|entry| entry.event_id == falsification.event_id)
    else {
        return Some(format!(
            "{} names no earlier verification on this change",
            falsification.event_id
        ));
    };
    if referenced.result != VerifyResult::Fail {
        return Some(format!(
            "{} recorded a pass; only an observed failure can be answered",
            falsification.event_id
        ));
    }
    // A gate keeps its identity when its command is edited, so a name match is
    // enough; two ad hoc runs have no name and must agree on the command.
    let same_check = matches!(
        (gate, referenced.gate.as_deref()),
        (Some(offered), Some(referenced)) if offered == referenced
    ) || command == referenced.command;
    if !same_check {
        return Some(format!(
            "{} recorded {}, not {}",
            falsification.event_id,
            check_label(referenced.gate.as_deref(), &referenced.command),
            check_label(gate, command)
        ));
    }
    if falsification.revision != referenced.revision {
        return Some(format!(
            "{} was recorded at {}, not at {}",
            falsification.event_id, referenced.revision, falsification.revision
        ));
    }
    None
}

/// The failure a pass of this check follows, derived from `prior`: the newest
/// failing gate evidence of the same gate, or of the same command when the
/// check is unnamed. Probe evidence answers to its own baseline and final
/// contract, so it is never a source.
pub fn infer_falsification(
    prior: &[VerificationEntry],
    gate: Option<&str>,
    command: &str,
) -> Option<InferredFalsification> {
    prior
        .iter()
        .rev()
        .filter(|entry| entry.result == VerifyResult::Fail && entry.probe.is_none())
        .find(|entry| match gate {
            Some(gate) => entry.gate.as_deref() == Some(gate),
            None => entry.command == command,
        })
        .map(|entry| InferredFalsification {
            event_id: entry.event_id.clone(),
            revision: entry.revision.clone(),
            source: InferenceSource::PriorFailureSameChange,
        })
}

fn check_label(gate: Option<&str>, command: &str) -> String {
    match gate {
        Some(gate) => format!("gate {gate:?}"),
        None => format!("command {command:?}"),
    }
}

impl VerificationEntry {
    /// Whether this evidence can satisfy a gate at its recorded revision.
    /// Passing output is not reproducible when local provenance is unknown,
    /// the tested tree was not retained, the worktree was dirty, or it
    /// changed while the command ran. Attested evidence carries an external
    /// execution context instead of local tree provenance, so a passing
    /// attestation remains eligible.
    ///
    /// A waiver excuses dirt that was *observed*, and only at the revision it
    /// names. Cleanliness recorded as unknown is not dirt somebody looked at
    /// and accepted — the parallel path records it that way precisely because
    /// a shared batch cannot tell whether a gate changed and restored a file
    /// while another was still running. Letting a waiver cover that would
    /// vouch for a tree nobody saw. A tree that moved mid-run is never
    /// excused either: that evidence describes no single tree, so there is
    /// nothing to vouch for.
    pub fn green_at_head(&self, waiver: Option<&DirtyTreeWaiver>) -> bool {
        let dirt_excused = match self.worktree_dirty {
            Some(false) => true,
            Some(true) => waiver.is_some_and(|waiver| waiver.revision == self.revision),
            None => false,
        };
        self.result == VerifyResult::Pass
            && self.timing.concurrent_batch.as_ref().is_none_or(|batch| {
                self.worktree_dirty == Some(false)
                    && !self.attested
                    && crate::gate_concurrency::receipt_matches(
                        batch,
                        self.gate.as_deref(),
                        self.declaration.as_deref(),
                    )
            })
            && !self.tree_moved
            && (self.attested || (self.tested_tree.is_some() && dirt_excused))
    }

    /// The content key available to replay without consulting Git.
    pub fn recorded_tree(&self) -> Option<&str> {
        self.tree.as_deref().or(self.tested_tree.as_deref())
    }

    /// Whether this evidence is about the given tree.
    ///
    /// Evidence recorded before arc kept the tree beside the revision still
    /// describes one, and `resolve` reads it back from the revision. A
    /// revision whose commit is gone answers `None` and matches nothing.
    pub fn describes_tree(&self, tree: &str, resolve: &dyn Fn(&str) -> Option<String>) -> bool {
        match self.tree.as_deref() {
            Some(recorded) => recorded == tree,
            None => resolve(&self.revision).as_deref() == Some(tree),
        }
    }
}

fn valid_delta_selection(delta: &crate::model::DeltaSelection) -> bool {
    use sha2::{Digest, Sha256};
    let Ok(answer) = serde_json::from_str::<serde_json::Value>(&delta.output) else {
        return false;
    };
    let selected = answer
        .get("gates")
        .and_then(serde_json::Value::as_array)
        .and_then(|gates| {
            gates
                .iter()
                .map(|name| name.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        });
    delta.output.len() <= 65536
        && answer.get("schema").and_then(serde_json::Value::as_str) == Some("arc-gate-selection/1")
        && answer.get("complete").and_then(serde_json::Value::as_bool) == Some(true)
        && answer.get("request").is_some_and(|request| {
            request.get("schema").and_then(serde_json::Value::as_str) == Some("arc-gate-delta/1")
                && request
                    .get("source_tree")
                    .and_then(serde_json::Value::as_str)
                    == Some(delta.source_tree.as_str())
                && request
                    .get("target_tree")
                    .and_then(serde_json::Value::as_str)
                    == Some(delta.target_tree.as_str())
                && request.get("paths") == Some(&serde_json::json!(delta.changed_paths))
                && request
                    .get("gates")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|gates| {
                        gates.iter().all(serde_json::Value::is_string)
                            && delta
                                .selected_gates
                                .iter()
                                .all(|gate| gates.contains(&serde_json::json!(gate)))
                    })
        })
        && selected.as_deref() == Some(delta.selected_gates.as_slice())
        && delta.output_sha256
            == format!(
                "sha256:{}",
                hex::encode(Sha256::digest(delta.output.as_bytes()))
            )
}

pub fn verification_event(
    state: &ChangeState,
    evidence: &VerificationEntry,
) -> crate::model::GateEvent {
    evidence
        .run_id
        .as_deref()
        .and_then(|id| state.verification_runs.iter().find(|run| run.run_id == id))
        .map(|run| run.event)
        .unwrap_or_default()
}

fn is_false_ref(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationRunEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub run_id: String,
    pub event: crate::model::GateEvent,
    pub revision: String,
    pub mode: VerificationRunMode,
    pub skip_green: bool,
    pub gates: Vec<VerificationRunGate>,
    pub terminals: Vec<VerificationRunTerminal>,
    pub missing_gates: Vec<String>,
    pub complete: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum VerificationRunTerminal {
    Recorded {
        gate: String,
        evidence_event_id: String,
        result: VerifyResult,
    },
    Reused {
        gate: String,
        evidence_event_id: String,
        reuse_event_id: String,
    },
}

impl VerificationRunTerminal {
    fn gate(&self) -> &str {
        match self {
            Self::Recorded { gate, .. } | Self::Reused { gate, .. } => gate,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CommentEntry {
    pub event_id: String,
    pub actor: String,
    pub body: String,
    pub patchset_id: Option<String>,
    pub anchor: Option<Anchor>,
    pub replies: Vec<(String, String, String)>, // (event_id, actor, body)
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageEntry {
    pub event_id: String,
    pub message_type: MessageType,
    pub severity: MessageSeverity,
    pub summary: String,
    pub detail: Option<String>,
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<crate::forge_round_trip::ForgeDelivery>,
    pub actor: String,
    pub harness: Option<String>,
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_link: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One active hold, identified by the event that set it. Holds accumulate:
/// releasing one leaves every other in place.
#[derive(Debug, Clone, Serialize)]
pub struct HoldState {
    pub hold_event_id: String,
    pub reason: String,
    pub held_by: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClosureState {
    pub origin_replica_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<crate::model::IntegrationShape>,
    pub outcome: Closure,
    pub integrated_commit: Option<String>,
    pub superseded_by: Option<String>,
    /// How the integration happened, for an integrated closure: `guarded`
    /// when arc performed and guarded the merge, `asserted` when somebody
    /// performed it elsewhere and said so, `legacy-unclassified` for closures
    /// written before arc could tell those apart. Absent when the change did
    /// not integrate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration: Option<IntegrationKind>,
    /// The patchset and head that were merged, the branch merged into, and
    /// where that branch stood first. Absent on legacy closures, which
    /// recorded none of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_patchset_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_before: Option<String>,
    /// What the guard consumed to authorize a guarded merge. Absent on an
    /// asserted integration, which arc did not authorize, and on guarded
    /// events written before arc recorded it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization: Option<crate::model::AuthorizationBasis>,
    /// External reference associated with a recorded receiver outcome.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_reference: Option<String>,
    /// The session that performed the integration. Integrating somebody
    /// else's work is its own role, never a contribution to the work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrator: Option<crate::model::DebtIdentity>,
    /// The start of this change's own landed range, when a landing closed
    /// it: the parent of its first landed commit. `target_before` stays the
    /// destination revision that governed the landing, which every
    /// declaration and policy reader keeps reading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_base: Option<String>,
    /// The landing this closure derives from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landing: Option<ClosureLanding>,
    /// For an asserted integration whose commits landed as other commits,
    /// each source commit and the commit asserted to be it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub correspondence: Vec<crate::model::CorrespondedCommit>,
    pub event_id: String,
    #[serde(skip)]
    pub created_at: DateTime<Utc>,
}

/// The landing an integrated closure derives from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClosureLanding {
    pub attempt: String,
    pub completion_event_id: String,
    pub target_after: String,
}

impl ClosureState {
    /// Where this change's own integrated range starts: its landed range's
    /// base, else where the target stood before.
    pub fn range_base(&self) -> Option<&str> {
        self.range_base.as_deref().or(self.target_before.as_deref())
    }
}

/// The closure an integration event reduces to. Both kinds record the same
/// facts; they differ in whether arc guarded the merge.
#[allow(clippy::too_many_arguments)]
fn integrated_closure(
    ev: &Event,
    integration: IntegrationKind,
    integrated_commit: &str,
    source_patchset_id: &str,
    source_head: &str,
    target_branch: &str,
    target_before: Option<String>,
) -> ClosureState {
    ClosureState {
        origin_replica_id: ev.repository_id.clone(),
        shape: None,
        outcome: Closure::Integrated,
        integrated_commit: Some(integrated_commit.to_string()),
        superseded_by: None,
        integration: Some(integration),
        source_patchset_id: Some(source_patchset_id.to_string()),
        source_head: Some(source_head.to_string()),
        target_branch: Some(target_branch.to_string()),
        target_before,
        authorization: None,
        external_reference: None,
        integrator: Some(crate::model::DebtIdentity::new(
            ev.actor.clone(),
            None,
            ev.harness.clone(),
            ev.model.clone(),
            ev.session.clone(),
        )),
        range_base: None,
        landing: None,
        correspondence: Vec::new(),
        event_id: ev.event_id.clone(),
        created_at: ev.created_at,
    }
}

/// How an integrated change reached its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationKind {
    /// arc performed the merge and verified its own preconditions.
    Guarded,
    /// Somebody else performed it; arc recorded the assertion.
    Asserted,
    /// Written before the two were distinguishable. arc cannot infer which
    /// this was, and says so rather than guessing.
    LegacyUnclassified,
}

/// The coverage a debt declaration captured, with each entry's session read
/// from the verdict it was captured from when the declaration predates the
/// field. A session is what tells a model from the key a recording was filed
/// under, so an entry without one cannot be validated.
///
/// Declarations capture every verdict on the bound patchset in order, so an
/// entry takes the session of the verdict at its position when that verdict
/// has the same identity, and otherwise the one session every matching verdict
/// shares. An entry that neither settles stays as recorded.
fn captured_coverage(
    captured: &[DebtCoverage],
    verdicts: &[VerdictEntry],
    patchset_id: Option<&str>,
) -> Vec<DebtCoverage> {
    let Some(patchset_id) = patchset_id else {
        return captured.to_vec();
    };
    let held: Vec<DebtCoverage> = verdicts
        .iter()
        .filter(|verdict| verdict.patchset_id == patchset_id)
        .map(|verdict| {
            DebtCoverage::new(
                verdict.effective_author().to_string(),
                verdict.model.clone(),
                verdict.session.clone(),
                verdict.route_version.clone(),
            )
        })
        .collect();
    let same_identity = |entry: &DebtCoverage, verdict: &DebtCoverage| {
        entry.reviewer == verdict.reviewer
            && entry.model == verdict.model
            && entry.route_version == verdict.route_version
    };
    captured
        .iter()
        .enumerate()
        .map(|(position, entry)| {
            let mut entry = entry.clone();
            if entry.session.is_some() {
                return entry;
            }
            let positional = (captured.len() == held.len())
                .then(|| &held[position])
                .filter(|verdict| same_identity(&entry, verdict))
                .and_then(|verdict| verdict.session.clone());
            let sessions: BTreeSet<&String> = held
                .iter()
                .filter(|verdict| same_identity(&entry, verdict))
                .filter_map(|verdict| verdict.session.as_ref())
                .collect();
            entry.session = positional.or_else(|| match sessions.len() {
                1 => sessions.into_iter().next().cloned(),
                _ => None,
            });
            entry
        })
        .collect()
}

/// A change's lease on a pool slot. The checkout at `path` outlives the
/// change; `released` records that the lease ended.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SlotRecord {
    pub root: String,
    pub slot: String,
    pub path: String,
    pub token: String,
    pub released: bool,
}

impl SlotRecord {
    /// The lease, while it holds.
    pub fn held(&self) -> bool {
        !self.released
    }
}

/// The current state of one change, derived by replaying its events in
/// ULID order. The event ledger is authoritative; this is a view.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeState {
    pub budget_raises: Vec<crate::gate_raise::Record>,
    #[serde(skip)]
    pub direct_executions: BTreeMap<String, Option<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_context: Option<crate::review_context::ReviewContext>,
    #[serde(skip)]
    pub privilege_scope: Option<std::sync::Arc<crate::import_authorization::PrivilegeScope>>,
    /// The clearance rule this projection judged reads by: the policy in
    /// force where the ledger was read, never a recorded fact.
    #[serde(skip)]
    pub review_clearance: crate::policy::ReviewClearance,
    pub schema: &'static str,
    #[serde(skip)]
    pub provisional_signature_successors: BTreeMap<String, String>,
    /// The revisions falsification references named as recorded, where
    /// identity recovery replaced them with the failure's own. Projection
    /// shows the recovered identity; a checker that asks what the ledger
    /// cites still needs the recorded one.
    #[serde(skip)]
    pub(crate) recovered_falsification_revisions: BTreeSet<String>,
    pub provisional_integrations: Vec<Event>,
    pub provisional_resolutions: Vec<Event>,
    /// Write-time model evidence, keyed by the ledger event that carries it.
    pub model_attributions: BTreeMap<String, ModelAttribution>,
    pub change_id: String,
    /// Explicit contribution assertions in force, independent of event ancestry.
    pub contribution_roles: BTreeMap<String, Vec<String>>,
    pub slug: String,
    pub title: String,
    pub profile: String,
    #[serde(default)]
    pub iterating: bool,
    pub target_branch: String,
    pub branch: String,
    pub base: String,
    pub worktree: Option<String>,
    #[serde(default)]
    pub worktree_created: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_owner_token: Option<String>,
    /// The slot lease the change's checkout holds or held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<SlotRecord>,
    pub opened_by: String,
    pub opened_on_behalf_of: Option<String>,
    pub opened_harness: Option<String>,
    pub opened_model: Option<String>,
    pub opened_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opened_session_link: Option<String>,
    /// Journal artifact this change was opened from, if any.
    pub journal_ref: Option<String>,
    /// `sha256:` over that artifact's body read when the change opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_ref_digest: Option<String>,
    /// The fork `begin --from-fork` promoted work from, when one did. A
    /// recorded link: no review credit, obligation, or lifecycle state
    /// crosses it, and the fork survives the promotion untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_fork: Option<crate::model::ForkProvenance>,
    pub blocked_by: Vec<String>,
    pub tags: Vec<String>,
    pub assigned_to: Option<String>,
    pub priority: i32,
    pub opened_at: chrono::DateTime<chrono::Utc>,
    pub patchsets: Vec<Patchset>,
    pub briefs: Vec<Brief>,
    /// The latest changelog record, release copy or the record that the
    /// change needs none; [`ChangeState::changelog_entry`] yields only copy.
    pub changelog: Option<ChangelogEntry>,
    pub messages: Vec<MessageEntry>,
    pub comments: Vec<CommentEntry>,
    pub findings: BTreeMap<String, FindingState>,
    pub verdicts: Vec<VerdictEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_verdicts: Vec<ExternalVerdictEntry>,
    /// The latest head a contribution was checked against and found ready
    /// to send. Absent until `integrate` runs under a contribution policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_to_send: Option<ReadyToSendEntry>,
    /// Post-integration audits, deliberately separate from `verdicts` so that
    /// "what shipped with what review" cannot be rewritten after the fact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit_verdicts: Vec<AuditVerdictEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub audit_findings: BTreeMap<String, FindingState>,
    /// The latest declared review obligation, if one was ever declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debt: Option<Debt>,
    /// The latest dirty-tree waiver, which counts only while it names the
    /// current head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty_tree_waiver: Option<DirtyTreeWaiver>,
    /// Facts a session kept while the work was happening, oldest first, so
    /// `resume` can hand them back to a compacted or cold successor. Skipped
    /// when empty so outputs that serialize the state whole keep their shape
    /// for changes that never kept anything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<KeptContext>,
    /// Raised at `begin` to demand an independent verdict regardless of what
    /// the change touches. One-way: a change may raise itself, never lower
    /// itself below what the project declared.
    #[serde(default)]
    pub dangerous: bool,
    /// Event IDs of every `blocked-on` stage, so a later brief can name the
    /// block that caused it without rescanning the event log.
    pub blocked_on_stages: Vec<String>,
    /// The other members of this change's must-land-together component.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lands_with: Vec<String>,
    /// The prepared, unfinished landing attempt this change is a member of.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_landing: Option<String>,
    pub verifications: Vec<VerificationEntry>,
    /// Checks observed after integration, never counted as gate evidence.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub post_integration_verifications: Vec<PostIntegrationVerificationEntry>,
    /// References a withdrawn rewrite map stranded, reported rather than
    /// replayed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub set_aside: Vec<SetAsideReference>,
    pub verification_runs: Vec<VerificationRunEntry>,
    pub claim: Option<ClaimState>,
    #[serde(skip)]
    pub(crate) retired_claim_ids: BTreeSet<String>,
    /// Active holds, keyed by the `HoldSet` event that set them. Two
    /// collaborators hold independently: neither replaces the other's, and a
    /// release names the one it lifts.
    pub holds: BTreeMap<String, HoldState>,
    pub closure: Option<ClosureState>,
    pub forge: crate::forge::ForgeState,
    pub round_trip: crate::forge_round_trip::RoundTrip,
}

impl ChangeState {
    /// Every commit this view names, each with what names it.
    ///
    /// The enumeration lives beside the projection it enumerates, so a field
    /// added to one is added to the other rather than to whichever reader
    /// somebody remembered. A revision nothing can resolve is the difference
    /// between evidence and a claim, and a checker that silently skipped a
    /// field would report a ledger sound in exactly the place it is not.
    ///
    /// Trees and blobs are not here. They are content, and content survives a
    /// rewrite that only renames the commits holding it.
    pub fn recorded_revisions(&self) -> Vec<(String, String)> {
        let mut found: Vec<(String, String)> = Vec::new();
        let mut note = |revision: &str, what: String| {
            let revision = revision.trim();
            if !revision.is_empty() && revision.chars().all(|c| c.is_ascii_hexdigit()) {
                found.push((revision.to_string(), what));
            }
        };
        note(&self.base, "base".to_string());
        for record in &self.budget_raises {
            note(&record.raise.head, "budget raise head".into());
            note(
                &record.raise.target_revision,
                "budget raise destination".into(),
            );
        }
        for patchset in &self.patchsets {
            note(&patchset.head, format!("{} head", patchset.id));
            note(&patchset.base, format!("{} base", patchset.id));
            if let Some(merge_base) = patchset.merge_base.as_deref() {
                note(merge_base, format!("{} merge-base", patchset.id));
            }
        }
        for brief in &self.briefs {
            if let Some(base) = brief.base_revision.as_deref() {
                note(base, "brief base".to_string());
            }
        }
        for verification in &self.verifications {
            note(&verification.revision, "verification".to_string());
            if let Some(target) = verification.against_target.as_deref() {
                note(target, "verification target".to_string());
            }
            if let Some(falsification) = &verification.falsification {
                note(&falsification.revision, "falsification".to_string());
            }
            if let Some(inferred) = &verification.falsification_inferred {
                note(&inferred.revision, "inferred falsification".to_string());
            }
        }
        for revision in &self.recovered_falsification_revisions {
            note(revision, "falsification".to_string());
        }
        for run in &self.verification_runs {
            note(&run.revision, "verification run".to_string());
        }
        for receipt in &self.post_integration_verifications {
            note(
                &receipt.revision,
                "verification after integration".to_string(),
            );
        }
        for audit in &self.audit_verdicts {
            note(&audit.revision, "audit".to_string());
        }
        for external in &self.external_verdicts {
            note(&external.revision, "external verdict".to_string());
        }
        if let Some(waiver) = &self.dirty_tree_waiver {
            note(&waiver.revision, "dirty-tree waiver".to_string());
        }
        for finding in self.findings.values().chain(self.audit_findings.values()) {
            for disposition in &finding.dispositions {
                if let Some(commit) = disposition.commit.as_deref() {
                    note(commit, format!("{} disposition", finding.id));
                }
            }
        }
        if let Some(closure) = &self.closure {
            if let Some(commit) = closure.integrated_commit.as_deref() {
                note(commit, "integration".to_string());
            }
            if let Some(head) = closure.source_head.as_deref() {
                note(head, "integrated source head".to_string());
            }
            if let Some(before) = closure.target_before.as_deref() {
                note(before, "integration target".to_string());
            }
            if let Some(base) = closure.range_base.as_deref() {
                note(base, "landed range base".to_string());
            }
            if let Some(authorization) = &closure.authorization {
                for prerequisite in &authorization.prerequisites {
                    if let Some(commit) = prerequisite.integrated_commit.as_deref() {
                        note(commit, format!("prerequisite {}", prerequisite.change_id));
                    }
                }
            }
        }
        // A forge records revisions the same way, and a rewritten branch
        // strands them the same way. Every observation is kept rather than the
        // newest: an earlier head is still recorded, and still named something
        // that was supposed to exist.
        for link in &self.forge.links {
            note(&link.observed_head_sha, "forge link head".to_string());
        }
        for checks in &self.forge.checks_history {
            note(&checks.observed_pr_head, "forge checks head".to_string());
        }
        for pr_state in &self.forge.pr_states {
            if let Some(merge_sha) = pr_state.observed_merge_sha.as_deref() {
                note(merge_sha, "forge merge".to_string());
            }
            if let Some(pr_head) = pr_state.observed_pr_head.as_deref() {
                note(pr_head, "forge pr head".to_string());
            }
        }
        found
    }

    /// Follow every revision this view holds forward through the recorded
    /// rewrites, so each one names the commit it names in this repository.
    ///
    /// Every revision follows a rewrite except the one an approval stands on,
    /// which follows only a rewrite that changed a signature and nothing else.
    ///
    /// Trees and blobs are deliberately untouched. A rewrite that preserves
    /// content produces new commits over the same trees, so tree-keyed gate
    /// evidence must count identically across it; a rewrite that changes
    /// content produces trees no recorded map describes, and inventing a
    /// successor for one would vouch for content nobody evaluated.
    pub fn follow_rewrites(&mut self, rewrites: &RewriteMap) -> Result<()> {
        if rewrites.is_empty() {
            return Ok(());
        }
        for event in &self.provisional_integrations {
            if let Payload::ChangeProvisionallyIntegrated {
                source_head,
                provisional_commit,
                ..
            } = &event.payload
            {
                self.provisional_signature_successors.insert(
                    source_head.clone(),
                    rewrites.signature_successor(source_head)?,
                );
                self.provisional_signature_successors.insert(
                    provisional_commit.clone(),
                    rewrites.signature_successor(provisional_commit)?,
                );
            }
        }
        rewrites.advance(&mut self.base)?;
        for patchset in &mut self.patchsets {
            rewrites.advance(&mut patchset.base)?;
            rewrites.advance(&mut patchset.head)?;
            // Once a change has closed, its approval is the record of what
            // authorized the close rather than a gate, and follows every
            // rewrite as the rest of the record does.
            patchset.approved_head = if self.closure.is_some() {
                patchset.head.clone()
            } else {
                rewrites.approved_successor(&patchset.approved_head)?
            };
            rewrites.advance_opt(&mut patchset.merge_base)?;
        }
        for brief in &mut self.briefs {
            rewrites.advance_opt(&mut brief.base_revision)?;
        }
        for finding in self
            .findings
            .values_mut()
            .chain(self.audit_findings.values_mut())
        {
            for disposition in &mut finding.dispositions {
                rewrites.advance_opt(&mut disposition.commit)?;
            }
        }
        if let Some(waiver) = &mut self.dirty_tree_waiver {
            rewrites.advance(&mut waiver.revision)?;
        }
        for audit in &mut self.audit_verdicts {
            rewrites.advance(&mut audit.revision)?;
        }
        // An extent names what was read, so it follows only the rewrites an
        // approval follows: a successor that changed content is not what
        // either side covered.
        let extents = self
            .audit_verdicts
            .iter_mut()
            .filter_map(|audit| audit.extent.as_mut())
            .chain(self.debt.as_mut().and_then(|debt| debt.extent.as_mut()));
        for extent in extents {
            for revision in extent.subject.revisions_mut() {
                *revision = rewrites.approved_successor(revision)?;
            }
        }
        for verification in &mut self.verifications {
            rewrites.advance(&mut verification.revision)?;
            rewrites.advance_opt(&mut verification.against_target)?;
            if let Some(falsification) = &mut verification.falsification {
                rewrites.advance(&mut falsification.revision)?;
            }
            if let Some(inferred) = &mut verification.falsification_inferred {
                rewrites.advance(&mut inferred.revision)?;
            }
        }
        for run in &mut self.verification_runs {
            rewrites.advance(&mut run.revision)?;
        }
        for receipt in &mut self.post_integration_verifications {
            rewrites.advance(&mut receipt.revision)?;
            rewrites.advance(&mut receipt.integrated_commit)?;
        }
        if let Some(closure) = &mut self.closure {
            rewrites.advance_opt(&mut closure.integrated_commit)?;
            rewrites.advance_opt(&mut closure.source_head)?;
            rewrites.advance_opt(&mut closure.target_before)?;
            rewrites.advance_opt(&mut closure.range_base)?;
            if let Some(landing) = &mut closure.landing {
                rewrites.advance(&mut landing.target_after)?;
            }
            if let Some(authorization) = &mut closure.authorization {
                for prerequisite in &mut authorization.prerequisites {
                    rewrites.advance_opt(&mut prerequisite.integrated_commit)?;
                }
            }
        }
        self.forge.follow_rewrites(rewrites)
    }

    pub fn latest_patchset(&self) -> Option<&Patchset> {
        self.patchsets.last()
    }

    /// The brief a patchset was built from, which is not always the newest
    /// one: recording a new brief without re-snapshotting leaves the patchset
    /// bound to the version the work was actually done against.
    pub fn brief_for(&self, patchset: &Patchset) -> Option<&Brief> {
        let brief_ref = patchset.brief_ref.as_ref()?;
        self.briefs
            .iter()
            .find(|brief| brief.event_id == brief_ref.event_id)
    }

    pub fn latest_brief(&self) -> Option<&Brief> {
        self.briefs.last()
    }

    /// The release copy the projection emits for this change: the latest
    /// changelog record, unless that record says the change needs none.
    pub fn changelog_entry(&self) -> Option<&ChangelogEntry> {
        self.changelog.as_ref().filter(|entry| entry.is_entry())
    }

    /// Who wrote the brief a patchset was built from, read the same way a
    /// verdict's identity is: a lead acting for an executor is that executor
    /// on both sides, or the comparison compares different things.
    ///
    /// Bound to the patchset rather than to the newest brief, because a brief
    /// version recorded after the snapshot describes work this patchset is
    /// not.
    pub fn brief_author_for(&self, patchset: &Patchset) -> Option<&str> {
        self.brief_for(patchset)
            .map(|brief| brief.on_behalf_of.as_deref().unwrap_or(&brief.actor))
    }

    /// Whether every verdict identity in some window came from the brief's
    /// author. `None` when there is no brief, or no verdict to attribute.
    ///
    /// This asserts nothing about independence — arc cannot know that a
    /// reviewer directed the work. It reports that the identity which wrote
    /// the brief is the only one that recorded a verdict, and the window the
    /// caller chose decides which verdicts that covers.
    pub fn reviewed_only_by_brief_author<'a>(
        &self,
        patchset: &Patchset,
        identities: impl IntoIterator<Item = &'a str>,
    ) -> Option<bool> {
        let author = self.brief_author_for(patchset)?;
        let mut identities = identities.into_iter().peekable();
        identities.peek()?;
        Some(identities.all(|identity| identity == author))
    }

    /// Verdict tips: corroborations support the observed tip without becoming
    /// another authority, while supersessions replace the observed tips.
    /// A legacy verdict without a relation replaces the earlier verdict
    /// history for compatibility with the former latest-wins projection.
    pub fn verdict_tips(&self) -> Vec<&VerdictEntry> {
        let start = self
            .verdicts
            .iter()
            .rposition(|verdict| verdict.relation.is_none())
            .unwrap_or(0);
        observed_tips(&self.verdicts[start..])
    }

    pub fn review_eligible(&self, event_id: &str, origin: &str) -> bool {
        self.privilege_scope.as_ref().is_none_or(|scope| {
            scope.permits(
                event_id,
                origin,
                crate::import_authorization::Role::ReviewAuthority,
            )
        })
    }

    /// The receiver's verdict history: local judgments take precedence over
    /// foreign review even when the foreign events have review permission.
    fn authoritative_verdict_history(&self) -> Vec<&VerdictEntry> {
        let eligible: Vec<_> = self
            .verdicts
            .iter()
            .filter(|verdict| self.review_eligible(&verdict.event_id, &verdict.origin_replica_id))
            .collect();
        let Some(scope) = &self.privilege_scope else {
            return eligible;
        };
        let local: Vec<_> = eligible
            .iter()
            .copied()
            .filter(|verdict| verdict.origin_replica_id == scope.local_origin)
            .collect();
        if local.is_empty() {
            eligible
        } else {
            local
        }
    }

    pub fn eligible_verdict_tips(&self) -> Vec<&VerdictEntry> {
        let selected: Vec<_> = self
            .authoritative_verdict_history()
            .into_iter()
            .cloned()
            .collect();
        let start = selected
            .iter()
            .rposition(|verdict| verdict.relation.is_none())
            .unwrap_or(0);
        let ids: BTreeSet<_> = observed_tips(&selected[start..])
            .iter()
            .map(|verdict| verdict.event_id.as_str())
            .collect();
        self.verdicts
            .iter()
            .filter(|verdict| ids.contains(verdict.event_id.as_str()))
            .collect()
    }

    pub fn apply_privilege_scope(
        &mut self,
        scope: std::sync::Arc<crate::import_authorization::PrivilegeScope>,
        events: &[Event],
    ) {
        for finding in self
            .findings
            .values_mut()
            .chain(self.audit_findings.values_mut())
        {
            for entry in finding
                .dispositions
                .iter_mut()
                .chain(&mut finding.after_integration)
            {
                let role = if entry.status == DispositionStatus::AcceptedRisk {
                    crate::import_authorization::Role::Waiver
                } else {
                    crate::import_authorization::Role::ReviewAuthority
                };
                entry.gating_eligible = !entry.status.releases_block()
                    || scope.permits(&entry.event_id, &entry.origin_replica_id, role);
            }
        }
        self.holds.clear();
        for event in events {
            match &event.payload {
                Payload::HoldSet { reason } => {
                    self.holds.insert(
                        event.event_id.clone(),
                        HoldState {
                            hold_event_id: event.event_id.clone(),
                            reason: reason.clone(),
                            held_by: event.actor.clone(),
                            created_at: event.created_at,
                        },
                    );
                }
                Payload::HoldReleased { hold_event_id, .. }
                    if scope.permits(
                        &event.event_id,
                        &event.repository_id,
                        crate::import_authorization::Role::Waiver,
                    ) =>
                {
                    match hold_event_id {
                        Some(id) => {
                            self.holds.remove(id);
                        }
                        None => self.holds.clear(),
                    }
                }
                _ => {}
            }
        }
        self.privilege_scope = Some(scope);
        self.settle_debt();
    }

    pub fn verdict_contested(&self) -> bool {
        self.eligible_verdict_tips().len() > 1
    }

    /// The sole authoritative verdict, when the observed verdict graph has
    /// one tip. Validity against the current head is a Git-time question
    /// answered by the status layer.
    pub fn latest_verdict(&self) -> Option<&VerdictEntry> {
        let tips = self.eligible_verdict_tips();
        match tips.as_slice() {
            [tip] => Some(*tip),
            _ => None,
        }
    }

    /// Most recent external decision that names this exact revision.
    pub fn latest_external_verdict_for_revision(
        &self,
        revision: &str,
    ) -> Option<&ExternalVerdictEntry> {
        self.external_verdicts
            .iter()
            .rev()
            .find(|verdict| verdict.revision == revision)
    }

    pub fn open_blocking_findings(&self) -> Vec<&FindingState> {
        self.findings
            .values()
            .filter(|f| f.blocks_integration())
            .collect()
    }

    pub fn is_closed(&self) -> bool {
        self.closure.is_some()
    }

    /// Latest matching verification evidence per gate name at an exact revision.
    pub fn gate_evidence_at_matching(
        &self,
        gate: &str,
        revision: &str,
        matches: impl Fn(&VerificationEntry) -> bool,
    ) -> Option<&VerificationEntry> {
        self.verifications
            .iter()
            .rfind(|v| v.gate.as_deref() == Some(gate) && v.revision == revision && matches(v))
    }

    /// Latest matching verification evidence per gate name at an exact tree.
    ///
    /// A gate holds for content, so the tree is the key wherever what ships is
    /// the content of no commit either side made. Evidence written before arc
    /// recorded the tree names only a revision; `resolve` turns that revision
    /// into the tree it points at, and a revision resolving to nothing matches
    /// no tree, which is unknown rather than a match.
    pub fn gate_evidence_at_tree_matching(
        &self,
        gate: &str,
        tree: &str,
        resolve: &dyn Fn(&str) -> Option<String>,
        matches: impl Fn(&VerificationEntry) -> bool,
    ) -> Option<&VerificationEntry> {
        self.verifications.iter().rfind(|v| {
            v.gate.as_deref() == Some(gate) && v.describes_tree(tree, resolve) && matches(v)
        })
    }

    /// The latest passing evidence that showed this gate able to fail at this
    /// tree, keyed the way the evidence that counts there is keyed.
    pub fn gate_falsification_at_tree(
        &self,
        gate: &str,
        tree: &str,
        resolve: &dyn Fn(&str) -> Option<String>,
    ) -> Option<&VerificationEntry> {
        self.verifications.iter().rfind(|v| {
            v.gate.as_deref() == Some(gate)
                && v.result == VerifyResult::Pass
                && v.falsification.is_some()
                && v.describes_tree(tree, resolve)
        })
    }

    /// The latest passing evidence that showed this gate able to fail at this
    /// revision.
    ///
    /// Falsification is a fact about the gate at a revision, not about one run
    /// of it: once a check has been watched to fail and then pass against this
    /// tree, running it again does not unlearn that. Reading only the newest
    /// evidence would let any rerun — `verify --all`, and so every `done` —
    /// silently retract the finding.
    pub fn gate_falsification_at(&self, gate: &str, revision: &str) -> Option<&VerificationEntry> {
        self.verifications.iter().rfind(|v| {
            v.gate.as_deref() == Some(gate)
                && v.revision == revision
                && v.result == VerifyResult::Pass
                && v.falsification.is_some()
        })
    }

    pub fn resolve_finding_id(&self, needle: &str) -> Result<String> {
        resolve_unique_id(self.findings.keys().map(String::as_str), needle, "finding")
    }
}

/// Resolve a prefix to exactly one identifier, preferring an exact match.
/// An ambiguous prefix refuses rather than picking the first candidate: the
/// resolved id is written canonically into an append-only event, so guessing
/// wrong is both permanent and silent.
pub fn resolve_unique_id<'a>(
    candidates: impl Iterator<Item = &'a str>,
    needle: &str,
    noun: &str,
) -> Result<String> {
    let mut prefixed = Vec::new();
    for candidate in candidates {
        if candidate == needle {
            return Ok(candidate.to_string());
        }
        if candidate.starts_with(needle) {
            prefixed.push(candidate);
        }
    }
    match prefixed.len() {
        0 => bail!("no {noun} matches {needle:?}"),
        1 => Ok(prefixed[0].to_string()),
        _ => bail!("ambiguous {noun} {needle:?}"),
    }
}

/// The same reduction, with every recorded revision followed forward through
/// the repository's recorded history rewrites.
///
/// The ledger is append-only and stays exactly as written; this is the view,
/// and a view's job is to answer in terms of the repository the reader has. A
/// projection that repeated pre-rewrite revisions would report a change whose
/// head does not match its own patchset, whose gates ran somewhere else, and
/// whose waiver covers a commit nobody can name — all of it derived from a
/// rewrite that changed no content.
#[cfg(test)]
pub fn reduce_following(events: &[Event], rewrites: &RewriteMap) -> Result<ChangeState> {
    reduce_following_with_repository(events, rewrites, &[])
}

pub fn reduce_following_with_repository(
    events: &[Event],
    rewrites: &RewriteMap,
    repository: &[Event],
) -> Result<ChangeState> {
    let mut state = reduce_with_repository(events, rewrites, repository)?;
    state.follow_rewrites(rewrites)?;
    Ok(state)
}

/// Replay recorded structure without deciding which claims can authorize
/// this receiver. Eligibility belongs to `apply_privilege_scope`; changing
/// receiver trust cannot invalidate recorded references or later facts.
/// Without a rewrite map, a change that needs one does not replay, so every
/// production reader supplies repository facts to resolve baseline references.
#[cfg(test)]
pub fn reduce(events: &[Event]) -> Result<ChangeState> {
    reduce_with_rewrites(events, &RewriteMap::default())
}

/// Replay at recorded revisions, using active rewrites only to establish a
/// falsification reference's identity. The failure event remains authoritative.
#[cfg(test)]
pub fn reduce_with_rewrites(events: &[Event], rewrites: &RewriteMap) -> Result<ChangeState> {
    reduce_with_repository(events, rewrites, &[])
}

pub fn reduce_with_repository(
    events: &[Event],
    rewrites: &RewriteMap,
    repository: &[Event],
) -> Result<ChangeState> {
    let baselines: BTreeMap<_, _> = if events
        .iter()
        .any(|event| matches!(event.payload, Payload::VerificationBaseReused { .. }))
    {
        repository
            .iter()
            .filter_map(|event| match &event.payload {
                Payload::GateBaselineRecorded { baseline } => {
                    Some((event.event_id.as_str(), baseline.as_ref()))
                }
                _ => None,
            })
            .collect()
    } else {
        BTreeMap::new()
    };
    let replies = events
        .iter()
        .filter(|event| matches!(event.payload, Payload::ReplyAdded { .. }))
        .collect::<Vec<_>>();
    let mut iter = events
        .iter()
        .filter(|event| !matches!(event.payload, Payload::ReplyAdded { .. }));
    let first = iter.next();
    let (mut state, first_event) = match first {
        Some(ev) => match &ev.payload {
            Payload::ChangeOpened {
                slug,
                title,
                profile,
                target_branch,
                branch,
                base,
                worktree,
                worktree_created,
                worktree_owner_token,
                blocked_by,
                tags,
                journal_ref,
                journal_ref_digest,
                from_fork,
                dangerous,
            } => (
                ChangeState {
                    budget_raises: Vec::new(),
                    direct_executions: BTreeMap::new(),
                    provisional_signature_successors: BTreeMap::new(),
                    recovered_falsification_revisions: BTreeSet::new(),
                    provisional_integrations: Vec::new(),
                    provisional_resolutions: Vec::new(),
                    privilege_scope: None,
                    review_clearance: Default::default(),
                    review_context: None,
                    schema: CHANGE_STATE_SCHEMA,
                    model_attributions: BTreeMap::new(),
                    contribution_roles: BTreeMap::new(),
                    dangerous: *dangerous,
                    dirty_tree_waiver: None,
                    iterating: false,
                    change_id: ev.change_id.clone(),
                    slug: slug.clone(),
                    title: title.clone(),
                    profile: profile.clone(),
                    target_branch: target_branch.clone(),
                    branch: branch.clone(),
                    base: base.clone(),
                    worktree: worktree.clone(),
                    worktree_created: *worktree_created,
                    worktree_owner_token: worktree_owner_token.clone(),
                    slot: None,
                    opened_by: ev.actor.clone(),
                    opened_on_behalf_of: ev.on_behalf_of.clone(),
                    opened_harness: ev.harness.clone(),
                    opened_model: ev.model.clone(),
                    opened_session: ev.session.clone(),
                    opened_session_link: ev.session_link.clone(),
                    journal_ref: journal_ref.clone(),
                    journal_ref_digest: journal_ref_digest.clone(),
                    from_fork: from_fork.clone(),
                    blocked_by: blocked_by.clone(),
                    tags: tags.clone(),
                    assigned_to: None,
                    priority: 0,
                    opened_at: ev.created_at,
                    patchsets: Vec::new(),
                    briefs: Vec::new(),
                    changelog: None,
                    messages: Vec::new(),
                    comments: Vec::new(),
                    findings: BTreeMap::new(),
                    verdicts: Vec::new(),
                    external_verdicts: Vec::new(),
                    ready_to_send: None,
                    audit_verdicts: Vec::new(),
                    audit_findings: BTreeMap::new(),
                    debt: None,
                    blocked_on_stages: Vec::new(),
                    lands_with: Vec::new(),
                    pending_landing: None,
                    verifications: Vec::new(),
                    post_integration_verifications: Vec::new(),
                    set_aside: Vec::new(),
                    verification_runs: Vec::new(),
                    claim: None,
                    retired_claim_ids: BTreeSet::new(),
                    holds: BTreeMap::new(),
                    closure: None,
                    kept: Vec::new(),
                    forge: crate::forge::ForgeState::default(),
                    round_trip: crate::forge_round_trip::RoundTrip::default(),
                },
                ev,
            ),
            _ => bail!(
                "change {} ledger does not start with change-opened",
                ev.change_id
            ),
        },
        None => bail!("empty event ledger"),
    };
    let _ = first_event;
    state.model_attributions = events
        .iter()
        .filter(|event| event.model_provenance.model_source.is_some())
        .map(|event| {
            (
                event.event_id.clone(),
                ModelAttribution {
                    model: event.model.clone(),
                    provenance: event.model_provenance.clone(),
                    session_verdict: None,
                },
            )
        })
        .collect();

    let recording_origins: BTreeMap<&str, &str> = events
        .iter()
        .map(|event| (event.event_id.as_str(), event.repository_id.as_str()))
        .collect();
    let legacy_link = |links: &[crate::forge::ForgeLinkRecord], event: &Event| {
        links
            .iter()
            .rev()
            .find(|link| {
                recording_origins.get(link.event_id.as_str()).copied()
                    == Some(event.repository_id.as_str())
            })
            .map(|link| link.event_id.clone())
    };
    let mut read_documents = BTreeMap::new();
    let mut conflicting_reads = BTreeSet::new();
    for event in events {
        if let Payload::ForgeObservationRecorded { observation } = &event.payload {
            if read_documents
                .insert(observation.read_id.as_str(), observation)
                .is_some_and(|old| old != observation)
            {
                conflicting_reads.insert(observation.read_id.as_str());
            }
        }
    }
    let mut legacy_checks_binding = false;
    for ev in iter {
        crate::gate_work::validate_event(ev)?;
        let projection_error =
            crate::forge_observation::projection_error(&ev.payload, &state.forge);
        if let Some(message) = &projection_error {
            state
                .forge
                .diagnostics
                .push(crate::forge_observation::ForgeDiagnostic {
                    event_id: ev.event_id.clone(),
                    kind: "projection-unresolved",
                    message: message.clone(),
                });
        }
        match &ev.payload {
            Payload::ChangeOpened { .. } => {
                bail!("duplicate change-opened event {}", ev.event_id)
            }
            Payload::ChangeCheckoutBound {
                branch,
                worktree,
                worktree_created,
                worktree_owner_token,
            } => {
                if branch != &state.branch {
                    bail!(
                        "checkout binding branch {branch:?} differs from change branch {:?}",
                        state.branch
                    );
                }
                state.worktree = Some(worktree.clone());
                state.worktree_created = *worktree_created;
                state.worktree_owner_token = worktree_owner_token.clone();
            }
            Payload::SlotLeased {
                root,
                slot,
                path,
                token,
            } => {
                state.slot = Some(SlotRecord {
                    root: root.clone(),
                    slot: slot.clone(),
                    path: path.clone(),
                    token: token.clone(),
                    released: false,
                });
            }
            Payload::SlotReleased { slot, token } => {
                if let Some(record) = state
                    .slot
                    .as_mut()
                    .filter(|record| &record.slot == slot && &record.token == token)
                {
                    record.released = true;
                }
            }
            Payload::ChangeRolesAsserted {
                asserted,
                withdrawn,
            } => {
                crate::trailers::validate_roles(asserted)?;
                crate::trailers::validate_roles(withdrawn)?;
                for (role, values) in withdrawn {
                    if let Some(current) = state.contribution_roles.get_mut(role) {
                        current.retain(|value| !values.contains(value));
                    }
                }
                for (role, values) in asserted {
                    let current = state.contribution_roles.entry(role.clone()).or_default();
                    for value in values {
                        if !current.contains(value) {
                            current.push(value.clone());
                        }
                    }
                }
                state
                    .contribution_roles
                    .retain(|_, values| !values.is_empty());
            }
            Payload::IterationScopeSet { iterating } => {
                state.iterating = *iterating;
            }
            Payload::MetadataUpdated {
                add_blocked_by,
                remove_blocked_by,
                add_tags,
                remove_tags,
                assign,
                priority,
            } => {
                state
                    .blocked_by
                    .retain(|id| !remove_blocked_by.contains(id));
                for id in add_blocked_by {
                    if !state.blocked_by.contains(id) {
                        state.blocked_by.push(id.clone());
                    }
                }
                state.tags.retain(|tag| !remove_tags.contains(tag));
                for tag in add_tags {
                    if !state.tags.contains(tag) {
                        state.tags.push(tag.clone());
                    }
                }
                state.blocked_by.sort();
                state.tags.sort();
                // Latest assignment wins; an empty value clears it.
                if let Some(assign) = assign {
                    let trimmed = assign.trim();
                    state.assigned_to = if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    };
                }
                if let Some(priority) = priority {
                    state.priority = *priority;
                }
            }
            Payload::Message {
                message_type,
                severity,
                summary,
                detail,
                metadata,
                delivery,
            } => state.messages.push(MessageEntry {
                event_id: ev.event_id.clone(),
                message_type: *message_type,
                severity: *severity,
                summary: summary.clone(),
                detail: detail.clone(),
                metadata: metadata.clone(),
                delivery: delivery.clone(),
                actor: ev.actor.clone(),
                harness: ev.harness.clone(),
                session: ev.session.clone(),
                session_link: ev.session_link.clone(),
                created_at: ev.created_at,
            }),
            Payload::BriefRecorded {
                title,
                body,
                caused_by,
                base_revision,
                acceptance_probes,
                plan_ref,
                plan_slice,
                plan_source,
                must_read,
            } => state.briefs.push(Brief {
                event_id: ev.event_id.clone(),
                ts: ev.created_at,
                actor: ev.actor.clone(),
                on_behalf_of: ev.on_behalf_of.clone(),
                harness: ev.harness.clone(),
                model: ev.model.clone(),
                session: ev.session.clone(),
                session_link: ev.session_link.clone(),
                title: title.clone(),
                body: body.clone(),
                caused_by: caused_by.clone(),
                base_revision: base_revision.clone(),
                acceptance_probes: acceptance_probes.clone(),
                plan_ref: plan_ref.clone(),
                plan_slice: plan_slice.clone(),
                plan_source: plan_source.as_ref().map(PlanSource::validated),
                must_read: must_read.clone(),
            }),
            Payload::ChangelogRecorded {
                category,
                body,
                supersedes,
                no_entry_reason,
            } => {
                state.changelog = Some(ChangelogEntry {
                    event_id: ev.event_id.clone(),
                    category: category.clone(),
                    body: body.clone(),
                    supersedes: supersedes.clone(),
                    no_entry_reason: no_entry_reason.clone(),
                    actor: ev.actor.clone(),
                    on_behalf_of: ev.on_behalf_of.clone(),
                    harness: ev.harness.clone(),
                    session: ev.session.clone(),
                    session_link: ev.session_link.clone(),
                    created_at: ev.created_at,
                });
            }
            Payload::PatchsetAdded {
                budget_raise_ids,
                patchset_id,
                base,
                head,
                merge_base,
                brief_ref,
                author_name,
                author_email,
                committer_name,
                committer_email,
                contributors,
                claim_id,
                claim_actor,
                journal_refs,
                thread,
                candidate,
                inherited_authors,
                rebase_started_at: _,
            } => {
                if let Some(claim_id) = claim_id {
                    crate::ids::validate_id_component(claim_id)?;
                }
                if let Some(claim) = state
                    .claim
                    .as_mut()
                    .filter(|claim| claim_id.as_deref() == Some(claim.claim_id.as_str()))
                {
                    claim.last_activity_at = ev.created_at;
                    claim.progress = Some(StageProgress {
                        stage: ClaimStage::Snapshotted,
                        note: None,
                        blocker: None,
                        changed_at: ev.created_at,
                    });
                }
                let author = git_identity(author_name, author_email);
                let committer = git_identity(committer_name, committer_email);
                let brief_version = brief_ref
                    .as_ref()
                    .map(|brief_ref| {
                        state
                            .briefs
                            .iter()
                            .position(|brief| brief.event_id == brief_ref.event_id)
                            .map(|index| index + 1)
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "patchset {patchset_id} references unknown or later brief {}",
                                    brief_ref.event_id
                                )
                            })
                    })
                    .transpose()?;
                let provenance_mismatch = provenance_mismatch(
                    crate::config::GitIdentityMode::PerActor,
                    claim_actor.as_deref(),
                    author.as_ref(),
                    committer.as_ref(),
                );
                state.patchsets.push(Patchset {
                    budget_raise_ids: budget_raise_ids.clone(),
                    id: patchset_id.clone(),
                    actor: ev.actor.clone(),
                    model: ev.model.clone(),
                    harness: ev.harness.clone(),
                    session: ev.session.clone(),
                    session_link: ev.session_link.clone(),
                    actor_source: ev.actor_source,
                    on_behalf_of: ev.on_behalf_of.clone(),
                    inherited_authors: inherited_authors.clone(),
                    base: base.clone(),
                    head: head.clone(),
                    approved_head: head.clone(),
                    merge_base: merge_base.clone(),
                    brief_ref: brief_ref.clone(),
                    brief_version,
                    author,
                    committer,
                    contributors: contributors.clone(),
                    claim_id: claim_id.clone(),
                    claim_actor: claim_actor.clone(),
                    journal_refs: journal_refs.clone(),
                    thread: thread.clone(),
                    candidate: candidate.clone(),
                    provenance_mismatch,
                    created_at: ev.created_at,
                });
            }
            Payload::PatchsetAttributionAmended {
                patchset_id,
                contributors,
            } => {
                crate::ids::validate_id_component(patchset_id)?;
                if contributors.is_empty() {
                    bail!(
                        "patchset attribution amendment {} must name at least one contributor",
                        ev.event_id
                    );
                }
                let Some(patchset) = state
                    .patchsets
                    .iter_mut()
                    .find(|patchset| patchset.id == *patchset_id)
                else {
                    bail!(
                        "patchset attribution amendment {} references unknown patchset {patchset_id}",
                        ev.event_id
                    );
                };
                if let Some(verdict) = state
                    .verdicts
                    .iter()
                    .find(|verdict| verdict.patchset_id == *patchset_id)
                {
                    bail!(
                        "patchset {patchset_id} attribution cannot be amended after verdict {}",
                        verdict.event_id
                    );
                }
                patchset.contributors = contributors.clone();
            }
            Payload::ClaimSet {
                claim_id,
                ttl_seconds,
                stage_budgets,
                displaced,
            } => {
                let harness = ev
                    .harness
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("claim event {} has no harness", ev.event_id))?;
                let session = ev
                    .session
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("claim event {} has no session", ev.event_id))?;
                let owner = ClaimIdentity {
                    actor: ev.actor.clone(),
                    harness,
                    session,
                };
                crate::ids::validate_id_component(claim_id)?;
                if let Some(displaced) = displaced {
                    crate::ids::validate_id_component(&displaced.claim_id)?;
                    state.retired_claim_ids.insert(displaced.claim_id.clone());
                    if state
                        .claim
                        .as_ref()
                        .is_some_and(|claim| claim.claim_id == displaced.claim_id)
                    {
                        state.claim = None;
                    }
                }
                let claim_id = claim_id.clone();
                if state.retired_claim_ids.contains(&claim_id) {
                    continue;
                }
                // Histories produced before transition locking (or merged by
                // import) may contain two acquisitions that both observed no
                // live owner. The first replayed owner wins until release or
                // expiry; the stale contender remains observable in the raw
                // ledger without taking over the typed view.
                if let Some(current) = state.claim.as_ref() {
                    let active = claim_timing_at(current, ev.created_at).active;
                    if current.claim_id == claim_id {
                        if current.owner != owner || !active {
                            if !active {
                                state.retired_claim_ids.insert(claim_id);
                                state.claim = None;
                            }
                            continue;
                        }
                    } else if active {
                        state.retired_claim_ids.insert(claim_id);
                        continue;
                    } else {
                        state.retired_claim_ids.insert(current.claim_id.clone());
                    }
                }
                let renewal = state.claim.as_ref().filter(|claim| {
                    claim.claim_id == claim_id
                        && claim.owner == owner
                        && claim_timing_at(claim, ev.created_at).active
                });
                let claimed_at = renewal.map_or(ev.created_at, |claim| claim.claimed_at);
                let progress = renewal.and_then(|claim| claim.progress.clone());
                let carried = renewal.and_then(|claim| claim.displaced.clone());
                state.claim = Some(ClaimState {
                    claim_id,
                    owner,
                    ttl_seconds: *ttl_seconds,
                    stage_budgets: stage_budgets.clone(),
                    claimed_at,
                    last_activity_at: ev.created_at,
                    progress,
                    displaced: displaced.clone().or(carried),
                });
            }
            Payload::ClaimReleased { claim_id } => {
                crate::ids::validate_id_component(claim_id)?;
                if state
                    .claim
                    .as_ref()
                    .is_some_and(|claim| claim.claim_id == *claim_id)
                {
                    state.claim = None;
                }
                state.retired_claim_ids.insert(claim_id.clone());
            }
            Payload::StageSet {
                claim_id,
                stage,
                note,
                blocker,
            } => {
                crate::ids::validate_id_component(claim_id)?;
                if *stage == ClaimStage::BlockedOn {
                    state.blocked_on_stages.push(ev.event_id.clone());
                }
                if *stage != ClaimStage::BlockedOn && blocker.is_some() {
                    bail!("non-blocked stage {} carries a blocker", ev.event_id);
                }
                match blocker {
                    Some(BlockerRef::Brief { brief_event_id }) => {
                        crate::ids::validate_id_component(brief_event_id)?;
                        if !state
                            .briefs
                            .iter()
                            .any(|brief| brief.event_id == *brief_event_id)
                        {
                            bail!(
                                "blocked-on stage {} references unknown or later brief {brief_event_id}",
                                ev.event_id
                            );
                        }
                    }
                    Some(BlockerRef::Finding { finding_id }) => {
                        crate::ids::validate_id_component(finding_id)?;
                        if !state.findings.contains_key(finding_id) {
                            bail!(
                                "blocked-on stage {} references unknown or later finding {finding_id}",
                                ev.event_id
                            );
                        }
                    }
                    Some(BlockerRef::Change { change_id }) => {
                        crate::ids::validate_id_component(change_id)?;
                    }
                    Some(BlockerRef::External) | None => {}
                }
                if state.retired_claim_ids.contains(claim_id) {
                    continue;
                }
                let Some(claim) = state.claim.as_mut() else {
                    // A concurrently appended release can sort before an
                    // already-authorized stage event. Keep the raw transition,
                    // but do not let it make all subsequent typed replay fail.
                    continue;
                };
                if claim_id != &claim.claim_id {
                    continue;
                }
                if claim.owner.actor != ev.actor
                    || ev.harness.as_deref() != Some(claim.owner.harness.as_str())
                    || ev.session.as_deref() != Some(claim.owner.session.as_str())
                {
                    continue;
                }
                if !claim_timing_at(claim, ev.created_at).active {
                    continue;
                }
                claim.last_activity_at = ev.created_at;
                claim.progress = Some(StageProgress {
                    stage: *stage,
                    note: note.clone(),
                    blocker: blocker.clone(),
                    changed_at: ev.created_at,
                });
            }
            Payload::ContextKept {
                kind,
                body,
                evidence,
                cites,
            } => state.kept.push(KeptContext {
                event_id: ev.event_id.clone(),
                kind: *kind,
                body: body.clone(),
                evidence: evidence.clone(),
                cites: cites.clone(),
                actor: ev.actor.clone(),
                created_at: ev.created_at,
            }),
            // A review start bounds a reviewer's span in a cost view and
            // changes no state.
            Payload::ReviewStarted { .. } => {}
            Payload::CommentAdded {
                body,
                patchset_id,
                anchor,
            } => state.comments.push(CommentEntry {
                event_id: ev.event_id.clone(),
                actor: ev.actor.clone(),
                body: body.clone(),
                patchset_id: patchset_id.clone(),
                anchor: anchor.clone(),
                replies: Vec::new(),
            }),
            Payload::FindingAdded {
                forge_sources,
                finding_id,
                blocking,
                severity,
                summary,
                body,
                patchset_id,
                anchor,
            } => {
                state.findings.insert(
                    finding_id.clone(),
                    FindingState {
                        forge_sources: forge_sources.clone(),
                        id: finding_id.clone(),
                        blocking: *blocking,
                        severity: *severity,
                        summary: summary.clone(),
                        body: body.clone(),
                        patchset_id: patchset_id.clone(),
                        anchor: anchor.clone(),
                        origin_event: ev.event_id.clone(),
                        reported_by: ev.actor.clone(),
                        on_behalf_of: ev.on_behalf_of.clone(),
                        actor_source: ev.actor_source,
                        dispositions: Vec::new(),
                        after_integration: Vec::new(),
                        replies: Vec::new(),
                    },
                );
            }
            Payload::ReplyAdded {
                parent_event_id: _,
                body: _,
            } => unreachable!("reply events are replayed in the second pass"),
            Payload::DispositionRecorded {
                finding_id,
                status,
                commit,
                evidence,
                evidence_event_id,
                supersedes,
            } => {
                let Some(f) = state.findings.get_mut(finding_id) else {
                    bail!(
                        "disposition {} references unknown finding {finding_id:?}",
                        ev.event_id
                    );
                };
                f.dispositions.push(DispositionEntry {
                    origin_replica_id: ev.repository_id.clone(),
                    gating_eligible: true,
                    event_id: ev.event_id.clone(),
                    status: *status,
                    commit: commit.clone(),
                    evidence: evidence.clone(),
                    evidence_event_id: evidence_event_id.clone(),
                    actor: ev.actor.clone(),
                    supersedes: supersedes.clone(),
                });
            }
            Payload::VerdictRecorded {
                basis,
                tiebreak_reviewer,
                tiebreak_target,
                forge_sources,
                forge_frontier,
                patchset_id,
                verdict,
                authority_observed,
                causes,
                provisional,
                body,
                findings,
                relation,
                route_version,
                assessment,
                dispositions,
                receipt,
            } => {
                for inline in findings {
                    state.findings.insert(
                        inline.finding_id.clone(),
                        FindingState {
                            forge_sources: inline.forge_sources.clone(),
                            id: inline.finding_id.clone(),
                            blocking: inline.blocking,
                            severity: inline.severity,
                            summary: inline.summary.clone(),
                            body: inline.body.clone(),
                            patchset_id: Some(patchset_id.clone()),
                            anchor: inline.anchor.clone(),
                            origin_event: ev.event_id.clone(),
                            reported_by: ev.actor.clone(),
                            on_behalf_of: ev.on_behalf_of.clone(),
                            actor_source: ev.actor_source,
                            dispositions: Vec::new(),
                            after_integration: Vec::new(),
                            replies: Vec::new(),
                        },
                    );
                }
                state.verdicts.push(VerdictEntry {
                    basis: basis.clone(),
                    tiebreak_reviewer: tiebreak_reviewer.clone(),
                    tiebreak_target: tiebreak_target.clone(),
                    forge_sources: forge_sources.clone(),
                    forge_frontier: forge_frontier.clone(),
                    origin_replica_id: ev.repository_id.clone(),
                    event_id: ev.event_id.clone(),
                    patchset_id: patchset_id.clone(),
                    verdict: *verdict,
                    authority_observed: authority_observed.clone(),
                    causes: causes.clone(),
                    body: body.clone(),
                    model: ev.model.clone(),
                    observed_model: ev.observed_model(),
                    session: ev.session.clone(),
                    provisional: provisional.clone(),
                    route_version: route_version.clone(),
                    assessment: assessment.clone(),
                    actor: ev.actor.clone(),
                    on_behalf_of: ev.on_behalf_of.clone(),
                    actor_source: ev.actor_source,
                    relation: relation.clone(),
                    receipt: receipt.clone(),
                    created_at: ev.created_at,
                });
                apply_inline_dispositions(&mut state.findings, ev, dispositions, "finding")?;
            }
            Payload::ReadyToSend {
                patchset_id,
                head,
                history,
                target_branch,
                target_revision,
                authorization,
            } => {
                state.ready_to_send = Some(ReadyToSendEntry {
                    event_id: ev.event_id.clone(),
                    patchset_id: patchset_id.clone(),
                    head: head.clone(),
                    history: history.clone(),
                    target_branch: target_branch.clone(),
                    target_revision: target_revision.clone(),
                    authorization: authorization.clone(),
                    created_at: ev.created_at,
                });
            }
            Payload::ExternalVerdictRecorded {
                revision,
                verdict,
                decided_by,
                reference,
                findings,
            } => {
                let closes_rejected_patchset = *verdict == crate::model::ExternalVerdict::Rejected
                    && state
                        .latest_patchset()
                        .is_some_and(|patchset| patchset.head == *revision);
                let closed_patchset = closes_rejected_patchset
                    .then(|| state.latest_patchset().cloned())
                    .flatten();
                state.external_verdicts.push(ExternalVerdictEntry {
                    origin_replica_id: ev.repository_id.clone(),
                    source: "external",
                    event_id: ev.event_id.clone(),
                    revision: revision.clone(),
                    verdict: *verdict,
                    decided_by: decided_by.clone(),
                    reference: reference.clone(),
                    findings: findings.clone(),
                    recorded_by: ev.actor.clone(),
                    created_at: ev.created_at,
                });
                if let Some(patchset) = closed_patchset {
                    state.closure = Some(ClosureState {
                        origin_replica_id: ev.repository_id.clone(),
                        shape: None,
                        outcome: Closure::Abandoned,
                        integrated_commit: None,
                        superseded_by: None,
                        integration: None,
                        source_patchset_id: Some(patchset.id),
                        source_head: Some(revision.clone()),
                        target_branch: None,
                        target_before: None,
                        authorization: None,
                        external_reference: Some(reference.clone()),
                        integrator: None,
                        range_base: None,
                        landing: None,
                        correspondence: Vec::new(),
                        event_id: ev.event_id.clone(),
                        created_at: ev.created_at,
                    });
                }
            }
            Payload::DirtyTreeWaived { reason, revision } => {
                state.dirty_tree_waiver = Some(DirtyTreeWaiver {
                    origin_replica_id: ev.repository_id.clone(),
                    event_id: ev.event_id.clone(),
                    reason: reason.clone(),
                    revision: revision.clone(),
                    actor: ev.actor.clone(),
                    declared_at: ev.created_at,
                });
            }
            Payload::AuditDebtDeclared {
                reason,
                patchset_id,
            } => {
                state.debt = Some(Debt {
                    origin_replica_id: ev.repository_id.clone(),
                    event_id: ev.event_id.clone(),
                    reason: reason.clone(),
                    patchset_id: patchset_id.clone(),
                    actor: ev.actor.clone(),
                    on_behalf_of: ev.on_behalf_of.clone(),
                    model: ev.model.clone(),
                    harness: ev.harness.clone(),
                    session: ev.session.clone(),
                    session_link: ev.session_link.clone(),
                    declared_at: ev.created_at,
                    missing: None,
                    coverage: None,
                    production: None,
                    ineligible: None,
                    replayed: None,
                    extent: None,
                    findings_reads: Vec::new(),
                    partial_coverage: Vec::new(),
                    discharged_by: None,
                    discharged_by_event_id: None,
                    discharge_extent: None,
                    standing: DebtStanding::Declared,
                });
            }
            Payload::DebtDeclared {
                reason,
                patchset_id,
                missing,
                coverage,
                production,
                ineligible,
                replayed,
                extent,
            } => {
                state.debt = Some(Debt {
                    origin_replica_id: ev.repository_id.clone(),
                    event_id: ev.event_id.clone(),
                    reason: reason.clone(),
                    patchset_id: patchset_id.clone(),
                    actor: ev.actor.clone(),
                    on_behalf_of: ev.on_behalf_of.clone(),
                    model: ev.model.clone(),
                    harness: ev.harness.clone(),
                    session: ev.session.clone(),
                    session_link: ev.session_link.clone(),
                    declared_at: ev.created_at,
                    missing: Some(*missing),
                    coverage: Some(captured_coverage(
                        coverage,
                        &state.verdicts,
                        patchset_id.as_deref(),
                    )),
                    production: production.clone(),
                    ineligible: ineligible.clone().map(|record| DebtRead {
                        record,
                        reviewer: None,
                        reviewer_model: None,
                        later_reads: Vec::new(),
                    }),
                    replayed: replayed.clone(),
                    extent: extent.clone(),
                    findings_reads: Vec::new(),
                    partial_coverage: Vec::new(),
                    discharged_by: None,
                    discharged_by_event_id: None,
                    discharge_extent: None,
                    standing: DebtStanding::Declared,
                });
            }
            Payload::AuditVerdictRecorded {
                basis,
                tiebreak_reviewer,
                tiebreak_target,
                revision,
                verdict,
                body,
                findings,
                route_version,
                assessment,
                held_roles,
                dispositions,
                receipt,
                extent,
            } => {
                for inline in findings {
                    state.audit_findings.insert(
                        inline.finding_id.clone(),
                        FindingState {
                            forge_sources: inline.forge_sources.clone(),
                            id: inline.finding_id.clone(),
                            blocking: inline.blocking,
                            severity: inline.severity,
                            summary: inline.summary.clone(),
                            body: inline.body.clone(),
                            patchset_id: None,
                            anchor: inline.anchor.clone(),
                            origin_event: ev.event_id.clone(),
                            reported_by: ev.actor.clone(),
                            on_behalf_of: ev.on_behalf_of.clone(),
                            actor_source: ev.actor_source,
                            dispositions: Vec::new(),
                            after_integration: Vec::new(),
                            replies: Vec::new(),
                        },
                    );
                }
                state.audit_verdicts.push(AuditVerdictEntry {
                    basis: basis.clone(),
                    tiebreak_reviewer: tiebreak_reviewer.clone(),
                    tiebreak_target: tiebreak_target.clone(),
                    origin_replica_id: ev.repository_id.clone(),
                    event_id: ev.event_id.clone(),
                    revision: revision.clone(),
                    verdict: *verdict,
                    body: body.clone(),
                    model: ev.model.clone(),
                    observed_model: ev.observed_model(),
                    session: ev.session.clone(),
                    route_version: route_version.clone(),
                    assessment: assessment.clone(),
                    held_roles: held_roles.clone(),
                    actor: ev.actor.clone(),
                    on_behalf_of: ev.on_behalf_of.clone(),
                    actor_source: ev.actor_source,
                    receipt: receipt.clone(),
                    extent: extent.clone(),
                    created_at: ev.created_at,
                });
                apply_inline_dispositions(
                    &mut state.audit_findings,
                    ev,
                    dispositions,
                    "audit finding",
                )?;
            }
            Payload::AuditFindingAdded {
                finding_id,
                blocking,
                severity,
                summary,
                body,
                anchor,
            } => {
                state.audit_findings.insert(
                    finding_id.clone(),
                    FindingState {
                        forge_sources: Vec::new(),
                        id: finding_id.clone(),
                        blocking: *blocking,
                        severity: *severity,
                        summary: summary.clone(),
                        body: body.clone(),
                        patchset_id: None,
                        anchor: anchor.clone(),
                        origin_event: ev.event_id.clone(),
                        reported_by: ev.actor.clone(),
                        on_behalf_of: ev.on_behalf_of.clone(),
                        actor_source: ev.actor_source,
                        dispositions: Vec::new(),
                        after_integration: Vec::new(),
                        replies: Vec::new(),
                    },
                );
            }
            Payload::AuditDispositionRecorded {
                finding_id,
                status,
                commit,
                evidence,
                evidence_event_id,
                supersedes,
            } => {
                let Some(finding) = state.audit_findings.get_mut(finding_id) else {
                    bail!(
                        "audit disposition {} references unknown audit finding {finding_id:?}",
                        ev.event_id
                    );
                };
                finding.dispositions.push(DispositionEntry {
                    origin_replica_id: ev.repository_id.clone(),
                    gating_eligible: true,
                    event_id: ev.event_id.clone(),
                    status: *status,
                    commit: commit.clone(),
                    evidence: evidence.clone(),
                    evidence_event_id: evidence_event_id.clone(),
                    actor: ev.actor.clone(),
                    supersedes: supersedes.clone(),
                });
            }
            Payload::PostIntegrationDispositionRecorded {
                finding_id,
                status,
                commit,
                evidence,
                evidence_event_id,
                supersedes,
            } => {
                if !state
                    .closure
                    .as_ref()
                    .is_some_and(|closure| closure.outcome == Closure::Integrated)
                {
                    bail!(
                        "post-integration disposition {} comes before change {} integrated",
                        ev.event_id,
                        state.change_id
                    );
                }
                let Some(finding) = state.findings.get_mut(finding_id) else {
                    bail!(
                        "post-integration disposition {} references unknown finding {finding_id:?}",
                        ev.event_id
                    );
                };
                finding.after_integration.push(DispositionEntry {
                    origin_replica_id: ev.repository_id.clone(),
                    gating_eligible: true,
                    event_id: ev.event_id.clone(),
                    status: *status,
                    commit: commit.clone(),
                    evidence: evidence.clone(),
                    evidence_event_id: evidence_event_id.clone(),
                    actor: ev.actor.clone(),
                    supersedes: supersedes.clone(),
                });
            }
            Payload::PostIntegrationVerificationRecorded {
                gate,
                command,
                revision,
                tree,
                integrated_commit,
                result,
                timeout_seconds,
                hostname,
                runner,
                environment,
                note,
            } => {
                let shipped = state
                    .closure
                    .as_ref()
                    .filter(|closure| closure.outcome == Closure::Integrated)
                    .and_then(|closure| closure.integrated_commit.as_deref());
                match shipped {
                    None => bail!(
                        "post-integration verification {} comes before change {} integrated",
                        ev.event_id,
                        state.change_id
                    ),
                    // Recorded after a rewrite, the receipt names the
                    // integrated commit's successor.
                    Some(shipped)
                        if shipped != integrated_commit
                            && !rewrites.rewrites_to(shipped, integrated_commit)? =>
                    {
                        if !rewrites.withdrawn_rewrites_to(shipped, integrated_commit) {
                            bail!(
                                "post-integration verification {} names {integrated_commit}, \
                                 but change {} integrated {shipped}",
                                ev.event_id,
                                state.change_id
                            );
                        }
                        state.set_aside.push(SetAsideReference {
                            event_id: ev.event_id.clone(),
                            reference: "post-integration-verification",
                            revision: integrated_commit.clone(),
                            follows: shipped.to_string(),
                        });
                        continue;
                    }
                    Some(_) => {}
                }
                state
                    .post_integration_verifications
                    .push(PostIntegrationVerificationEntry {
                        event_id: ev.event_id.clone(),
                        after_integration: true,
                        gate: gate.clone(),
                        command: command.clone(),
                        revision: revision.clone(),
                        tree: tree.clone(),
                        integrated_commit: integrated_commit.clone(),
                        result: *result,
                        timeout_seconds: *timeout_seconds,
                        hostname: hostname.clone(),
                        runner: runner.clone(),
                        environment: environment.clone(),
                        note: note.clone(),
                        actor: ev.actor.clone(),
                        on_behalf_of: ev.on_behalf_of.clone(),
                        harness: ev.harness.clone(),
                        model: ev.model.clone(),
                        session: ev.session.clone(),
                        created_at: ev.created_at,
                    });
            }
            Payload::VerificationRunStarted {
                profile,
                event,
                revision,
                mode,
                skip_green,
                gates,
                ..
            } => {
                let gate_names = gates
                    .iter()
                    .map(|gate| gate.name.clone())
                    .collect::<BTreeSet<_>>();
                if gate_names.len() != gates.len() || gates.is_empty() {
                    bail!(
                        "verification run {} must declare a nonempty unique gate set",
                        ev.event_id
                    );
                }
                state.verification_runs.push(VerificationRunEntry {
                    profile: profile.clone(),
                    run_id: ev.event_id.clone(),
                    event: *event,
                    revision: revision.clone(),
                    mode: *mode,
                    skip_green: *skip_green,
                    gates: gates.clone(),
                    terminals: Vec::new(),
                    missing_gates: gates.iter().map(|gate| gate.name.clone()).collect(),
                    complete: false,
                    created_at: ev.created_at,
                });
            }
            Payload::GateBudgetRaised { raise } => {
                if raise.change_id != state.change_id || raise.increases.is_empty() {
                    bail!("invalid budget raise subject");
                }
                state.budget_raises.push(crate::gate_raise::Record {
                    event_id: ev.event_id.clone(),
                    raise: raise.clone(),
                });
            }
            Payload::VerificationRecorded {
                exit_code,
                declaration,
                gate,
                command,
                duration_ms,
                timing,
                timeout_seconds,
                revision,
                result,
                hostname,
                attested,
                run_id,
                probe,
                runner,
                environment,
                output_tail,
                output_artifact,
                timed_out,
                tested_tree,
                worktree_dirty,
                worktree_dirty_tracked,
                worktree_dirty_untracked,
                tree_moved,
                tree,
                against_target,
                falsification,
                falsification_inferred,
                ..
            } => {
                if let Some(batch) = &timing.concurrent_batch {
                    if batch.acceptance.producing_store != ev.repository_id
                        || !crate::gate_concurrency::receipt_matches(
                            batch,
                            gate.as_deref(),
                            declaration.as_deref(),
                        )
                    {
                        bail!(
                            "verification {} has invalid concurrent batch provenance",
                            ev.event_id
                        );
                    }
                }
                state
                    .direct_executions
                    .insert(ev.event_id.clone(), *exit_code);
                let mut falsification = falsification.clone();
                if let Some(claimed) = &falsification {
                    if let Some(failure) = state
                        .verifications
                        .iter()
                        .find(|entry| entry.event_id == claimed.event_id)
                    {
                        if claimed.revision != failure.revision
                            && !rewrites.rewrites_to(&failure.revision, &claimed.revision)?
                            && rewrites.withdrawn_rewrites_to(&failure.revision, &claimed.revision)
                        {
                            state.set_aside.push(SetAsideReference {
                                event_id: ev.event_id.clone(),
                                reference: "falsification",
                                revision: claimed.revision.clone(),
                                follows: failure.revision.clone(),
                            });
                            falsification = None;
                        }
                    }
                }
                if let Some(falsification) = &mut falsification {
                    if let Some(failure) = state
                        .verifications
                        .iter()
                        .find(|entry| entry.event_id == falsification.event_id)
                    {
                        if falsification.revision != failure.revision
                            && rewrites.rewrites_to(&failure.revision, &falsification.revision)?
                        {
                            state
                                .recovered_falsification_revisions
                                .insert(falsification.revision.clone());
                            falsification.revision = failure.revision.clone();
                        }
                    }
                    if let Some(mismatch) = falsification_mismatch(
                        &state.verifications,
                        gate.as_deref(),
                        command,
                        falsification,
                    ) {
                        bail!(
                            "verification {} claims falsification by {mismatch}",
                            ev.event_id
                        );
                    }
                }
                if gate.is_some() && probe.is_some() {
                    bail!(
                        "verification {} cannot be both a gate and an acceptance probe",
                        ev.event_id
                    );
                }
                if *result == VerifyResult::Bypassed {
                    bail!(
                        "verification {} records a run as bypassed; a bypass is its own event",
                        ev.event_id
                    );
                }
                if let Some(probe) = probe {
                    let brief = state
                        .briefs
                        .iter()
                        .find(|brief| brief.event_id == probe.brief_event_id)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "verification {} references unknown or later brief {}",
                                ev.event_id,
                                probe.brief_event_id
                            )
                        })?;
                    let declaration = brief
                        .acceptance_probes
                        .iter()
                        .find(|declared| declared.name == probe.name)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "verification {} references undeclared probe {}",
                                ev.event_id,
                                probe.name
                            )
                        })?;
                    if declaration.command != *command {
                        bail!(
                            "verification {} command does not match declared probe {}",
                            ev.event_id,
                            probe.name
                        );
                    }
                }
                if let Some(run_id) = run_id {
                    let gate = gate.as_deref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "verification {} belongs to run {run_id} but has no gate",
                            ev.event_id
                        )
                    })?;
                    add_run_terminal(
                        &mut state.verification_runs,
                        run_id,
                        revision,
                        VerificationRunTerminal::Recorded {
                            gate: gate.to_owned(),
                            evidence_event_id: ev.event_id.clone(),
                            result: *result,
                        },
                    )?;
                }
                state.verifications.push(VerificationEntry {
                    base_pass: None,
                    declaration: declaration.clone(),
                    event_id: ev.event_id.clone(),
                    delta: None,
                    timeout_seconds: *timeout_seconds,
                    tested_tree: tested_tree.clone(),
                    worktree_dirty: *worktree_dirty,
                    worktree_dirty_tracked: *worktree_dirty_tracked,
                    worktree_dirty_untracked: *worktree_dirty_untracked,
                    tree_moved: *tree_moved,
                    tree: tree.clone(),
                    against_target: against_target.clone(),
                    run_id: run_id.clone(),
                    probe: probe.clone(),
                    gate: gate.clone(),
                    command: command.clone(),
                    revision: revision.clone(),
                    recorded_revision: revision.clone(),
                    result: *result,
                    attested: *attested,
                    duration_ms: *duration_ms,
                    timing: timing.clone(),
                    output_tail: output_tail.clone(),
                    output_artifact: output_artifact.clone(),
                    timed_out: *timed_out,
                    hostname: hostname.clone(),
                    runner: runner.clone(),
                    bypass: None,
                    environment: environment.clone(),
                    falsification,
                    falsification_inferred: falsification_inferred.clone(),
                    created_at: ev.created_at,
                });
            }
            Payload::VerificationBypassed {
                gate,
                revision,
                tree,
                reason,
                qualification,
            } => {
                state.verifications.push(VerificationEntry {
                    base_pass: None,
                    declaration: None,
                    event_id: ev.event_id.clone(),
                    delta: None,
                    run_id: None,
                    probe: None,
                    gate: Some(gate.clone()),
                    command: String::new(),
                    timeout_seconds: None,
                    revision: revision.clone(),
                    recorded_revision: revision.clone(),
                    result: VerifyResult::Bypassed,
                    attested: false,
                    duration_ms: None,
                    timing: Default::default(),
                    output_tail: None,
                    output_artifact: None,
                    timed_out: false,
                    hostname: String::new(),
                    runner: None,
                    bypass: Some(BypassEntry {
                        actor: ev.actor.clone(),
                        reason: reason.clone(),
                        qualification: qualification.clone(),
                    }),
                    tested_tree: None,
                    worktree_dirty: None,
                    worktree_dirty_tracked: None,
                    worktree_dirty_untracked: None,
                    tree_moved: false,
                    tree: Some(tree.clone()),
                    against_target: None,
                    falsification: None,
                    falsification_inferred: None,
                    environment: None,
                    created_at: ev.created_at,
                });
            }
            Payload::VerificationBaseReused {
                run_id,
                gate,
                revision,
                tree,
                witness,
            } => {
                if !crate::base_pass::unobserved(&state, gate) {
                    bail!("base pass cannot displace another verification observation");
                }
                let run = state
                    .verification_runs
                    .iter()
                    .find(|run| &run.run_id == run_id)
                    .ok_or_else(|| anyhow::anyhow!("base pass names no verification run"))?;
                let baseline = baselines
                    .get(witness.baseline_event_id.as_str())
                    .context("base pass has no authenticated integration baseline")?;
                let profile = &baseline.profile;
                witness.validate(baseline, profile)?;
                if run.event != crate::model::GateEvent::Integration
                    || run.profile.as_deref() != Some(profile)
                    || state.profile != *profile
                    || &witness.gate != gate
                    || &witness.revision != revision
                    || &witness.tree != tree
                {
                    bail!("base pass destination mismatch");
                }
                let derived = crate::base_pass::derive(witness, baseline, ev, run_id)?;
                if !run.gates.iter().any(|declared| {
                    declared.name == *gate
                        && declared.command == derived.command
                        && declared.timeout_seconds == derived.timeout_seconds
                }) {
                    bail!("base pass does not match its destination run declaration");
                }
                add_run_terminal(
                    &mut state.verification_runs,
                    run_id,
                    revision,
                    VerificationRunTerminal::Reused {
                        gate: gate.clone(),
                        evidence_event_id: baseline.gates[gate].execution.event_id.clone(),
                        reuse_event_id: ev.event_id.clone(),
                    },
                )?;
                state.verifications.push(derived);
            }
            Payload::GateBaselineRecorded { baseline } => {
                baseline.validate()?;
            }
            Payload::VerificationReused {
                run_id,
                gate,
                revision,
                tree,
                evidence_event_id,
                delta,
            } => {
                let evidence = state
                    .verifications
                    .iter()
                    .find(|entry| entry.event_id == *evidence_event_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "verification reuse {} references unknown or later evidence {}",
                            ev.event_id,
                            evidence_event_id
                        )
                    })?;
                let source_event = verification_event(&state, evidence);
                let destination_event = state
                    .verification_runs
                    .iter()
                    .find(|run| run.run_id == *run_id)
                    .map(|run| run.event)
                    .ok_or_else(|| anyhow::anyhow!("verification reuse names no run"))?;
                let tree_matches = match delta {
                    Some(delta) => {
                        valid_delta_selection(delta)
                            && tree.as_deref() == Some(delta.target_tree.as_str())
                            && evidence.recorded_tree() == Some(delta.source_tree.as_str())
                            && !delta.selected_gates.contains(gate)
                            && !delta.selector.is_empty()
                            && !delta.declaration.is_empty()
                            && evidence
                                .declaration
                                .as_ref()
                                .map_or(delta.selector != crate::gates::PATH_SELECTOR, |id| {
                                    id == &delta.declaration
                                })
                            && evidence.green_at_head(state.eligible_dirty_tree_waiver())
                    }
                    None => match tree {
                        Some(tree) => evidence.recorded_tree() == Some(tree.as_str()),
                        None => evidence.revision == *revision,
                    },
                };
                if evidence.gate.as_deref() != Some(gate)
                    || !tree_matches
                    || evidence.result != VerifyResult::Pass
                    || source_event != destination_event
                {
                    bail!("verification reuse {} does not match passing {gate} evidence at {revision}", ev.event_id);
                }
                let derived = delta.as_ref().map(|delta| {
                    let mut derived = evidence.clone();
                    derived.event_id = ev.event_id.clone();
                    derived.run_id = Some(run_id.clone());
                    derived.revision = revision.clone();
                    derived.recorded_revision = revision.clone();
                    derived.tree = tree.clone();
                    derived.against_target = delta.against_target.clone();
                    derived.delta = Some(delta.clone());
                    derived.created_at = ev.created_at;
                    derived.falsification = None;
                    derived.falsification_inferred = None;
                    derived
                });
                add_run_terminal(
                    &mut state.verification_runs,
                    run_id,
                    revision,
                    VerificationRunTerminal::Reused {
                        gate: gate.clone(),
                        evidence_event_id: evidence_event_id.clone(),
                        reuse_event_id: ev.event_id.clone(),
                    },
                )?;
                if let Some(derived) = derived {
                    state.verifications.push(derived);
                }
            }
            Payload::HoldSet { reason } => {
                state.holds.insert(
                    ev.event_id.clone(),
                    HoldState {
                        hold_event_id: ev.event_id.clone(),
                        reason: reason.clone(),
                        held_by: ev.actor.clone(),
                        created_at: ev.created_at,
                    },
                );
            }
            // A release naming no hold predates hold identity, where releasing
            // meant releasing everything. Honouring that keeps replay of old
            // ledgers truthful rather than leaving holds nobody can lift.
            // A release names the hold it lifts; one that names a hold no
            // longer active is a no-op rather than a replay failure. Two
            // collaborators can independently release the same hold, and a
            // ledger nobody can reduce is a ledger nobody can repair — the
            // contradiction belongs in `doctor`, which reports it, not in the
            // reducer, which would make every derived view unreadable.
            Payload::HoldReleased { hold_event_id, .. } => match hold_event_id {
                Some(id) => {
                    state.holds.remove(id);
                }
                None => state.holds.clear(),
            },
            Payload::ChangeClosed {
                outcome,
                integrated_commit,
                superseded_by,
            } => {
                state.closure = Some(ClosureState {
                    origin_replica_id: ev.repository_id.clone(),
                    shape: None,
                    outcome: *outcome,
                    integrated_commit: integrated_commit.clone(),
                    superseded_by: superseded_by.clone(),
                    // An integrated `ChangeClosed` predates the distinction,
                    // and arc cannot truthfully infer how the merge happened.
                    integration: (*outcome == Closure::Integrated)
                        .then_some(IntegrationKind::LegacyUnclassified),
                    source_patchset_id: None,
                    source_head: None,
                    target_branch: None,
                    integrator: None,
                    target_before: None,
                    authorization: None,
                    external_reference: None,
                    range_base: None,
                    landing: None,
                    correspondence: Vec::new(),
                    event_id: ev.event_id.clone(),
                    created_at: ev.created_at,
                });
            }
            Payload::ChangeProvisionallyIntegrated { .. } => {
                state.provisional_integrations.push(ev.clone())
            }
            Payload::ProvisionalIntegrationResolved { .. } => {
                state.provisional_resolutions.push(ev.clone())
            }
            Payload::ChangeIntegrated {
                provisional_resolutions,
                authority_grant_event_id: _,
                integrated_commit,
                source_patchset_id,
                source_head,
                target_branch,
                target_before,
                authorization,
                already_contained,
                shape,
                started_at: _,
                landing,
            } => {
                let mut closure = integrated_closure(
                    ev,
                    IntegrationKind::Guarded,
                    integrated_commit,
                    source_patchset_id,
                    source_head,
                    target_branch,
                    Some(target_before.clone()),
                );
                closure.shape = Some(shape.unwrap_or_else(|| {
                    crate::model::IntegrationShape::from_legacy(*already_contained)
                }));
                if !provisional_resolutions.is_empty() {
                    state.provisional_resolutions.push(ev.clone());
                }
                closure.authorization = authorization.clone();
                closure.range_base = landing.as_ref().map(|landing| landing.range_base.clone());
                closure.landing = landing.as_ref().map(|landing| ClosureLanding {
                    attempt: landing.attempt.clone(),
                    completion_event_id: landing.completion_event_id.clone(),
                    target_after: landing.target_after.clone(),
                });
                closure.external_reference = authorization
                    .as_ref()
                    .and_then(|basis| basis.external_verdict.as_ref())
                    .map(|verdict| verdict.reference.clone());
                state.closure = Some(closure);
            }
            Payload::IntegrationAsserted {
                integrated_commit,
                source_patchset_id,
                source_head,
                target_branch,
                target_before,
                external_reference,
                correspondence,
            } => {
                let mut closure = integrated_closure(
                    ev,
                    IntegrationKind::Asserted,
                    integrated_commit,
                    source_patchset_id,
                    source_head,
                    target_branch,
                    target_before.clone(),
                );
                closure.external_reference = external_reference.clone();
                closure.correspondence = correspondence.clone();
                state.closure = Some(closure);
            }
            Payload::ForgeProjection {
                host,
                base_repo,
                base_ref,
                head_repo,
                head_ref,
                policy,
            } => {
                state.forge.projection = Some(crate::forge::ForgeProjectionRecord {
                    host: host.clone(),
                    base_repo: base_repo.clone(),
                    base_ref: base_ref.clone(),
                    head_repo: head_repo.clone(),
                    head_ref: head_ref.clone(),
                    policy: policy.clone(),
                });
            }
            Payload::ForgeObservationRecorded { observation } => {
                crate::forge_observation::replay_observation(
                    &mut state.forge,
                    ev,
                    observation,
                    conflicting_reads.contains(observation.read_id.as_str()),
                );
            }
            Payload::ForgeLink {
                observation,
                pr_number,
                url,
                base_repo,
                base_ref,
                head_repo,
                head_ref,
                head_sha,
            } => {
                let record = crate::forge::ForgeLinkRecord {
                    provider_identity: observation
                        .as_ref()
                        .and_then(|id| {
                            state
                                .forge
                                .observations
                                .iter()
                                .find(|r| r.names_event(id) && r.usable())
                        })
                        .map(|r| r.document.provider_identity.clone()),
                    unresolved_binding: projection_error.is_some(),
                    observation: observation.clone(),
                    acquisition: if observation.is_some() {
                        "retention-reference"
                    } else {
                        "unretained-assertion"
                    },
                    read_at: observation
                        .as_ref()
                        .and_then(|id| {
                            state
                                .forge
                                .observations
                                .iter()
                                .find(|r| r.names_event(id) && r.usable())
                        })
                        .map(|r| r.document.read_at),
                    event_id: ev.event_id.clone(),
                    pr_number: *pr_number,
                    url: url.clone(),
                    base_repo: base_repo.clone(),
                    base_ref: base_ref.clone(),
                    head_repo: head_repo.clone(),
                    head_ref: head_ref.clone(),
                    observed_head_sha: head_sha.clone(),
                    resolved_head_sha: Some(head_sha.clone()),
                };
                state.forge.links.push(record.clone());
                state.forge.link = Some(record);
            }
            Payload::ForgeChecks {
                binding_recorded,
                link_event_id,
                observation,
                pr_head,
                state: check_state,
                detail,
            } => {
                legacy_checks_binding = !binding_recorded && link_event_id.is_none();
                let record = crate::forge::ForgeChecksRecord {
                    unresolved_binding: projection_error.is_some(),
                    observation: observation.clone(),
                    acquisition: if observation.is_some() {
                        "retention-reference"
                    } else {
                        "unretained-assertion"
                    },
                    read_at: observation
                        .as_ref()
                        .and_then(|id| {
                            state
                                .forge
                                .observations
                                .iter()
                                .find(|r| r.names_event(id) && r.usable())
                        })
                        .map(|r| r.document.read_at),
                    observed_pr_head: pr_head.clone(),
                    resolved_pr_head: Some(pr_head.clone()),
                    link_event_id: if legacy_checks_binding {
                        legacy_link(&state.forge.links, ev)
                    } else {
                        link_event_id.clone()
                    },
                    state: *check_state,
                    detail: detail.clone(),
                };
                state.forge.checks_history.push(record.clone());
                state.forge.checks = Some(record);
            }
            Payload::ForgePrState {
                observation,
                state: pr_state,
                merge_sha,
                link_event_id,
                pr_head,
            } => {
                // Explicit bindings name a recorded link and head. Legacy
                // readings need a prior link from their recording repository.
                let unresolved_binding = projection_error.is_some()
                    || match (link_event_id, pr_head) {
                        (Some(id), Some(_)) => {
                            !state.forge.links.iter().any(|link| link.event_id == *id)
                        }
                        (None, None) => legacy_link(&state.forge.links, ev).is_none(),
                        _ => true,
                    };
                state
                    .forge
                    .pr_states
                    .push(crate::forge::ForgePrStateRecord {
                        observation: observation.clone(),
                        acquisition: if observation.is_some() {
                            "retention-reference"
                        } else {
                            "unretained-assertion"
                        },
                        read_at: observation
                            .as_ref()
                            .and_then(|id| {
                                state
                                    .forge
                                    .observations
                                    .iter()
                                    .find(|r| r.names_event(id) && r.usable())
                            })
                            .map(|r| r.document.read_at),
                        state: *pr_state,
                        observed_merge_sha: merge_sha.clone(),
                        resolved_merge_sha: merge_sha.clone(),
                        link_event_id: link_event_id.clone(),
                        observed_pr_head: pr_head.clone(),
                        resolved_pr_head: pr_head.clone(),
                        unresolved_binding,
                    });
            }
            // Repository-scoped, so none is ever in a change's log; if one
            // arrives by import it says nothing about this change.
            Payload::HistoryRewritten { .. }
            | Payload::HistoryRewriteWithdrawn { .. }
            | Payload::ReviewPassOpened { .. }
            | Payload::ReviewPassCompleted { .. }
            | Payload::ReviewPassAbandoned { .. }
            | Payload::LandsWithDeclared { .. }
            | Payload::LandsWithWithdrawn { .. }
            | Payload::LandingPrepared { .. }
            | Payload::LandingCompleted { .. }
            | Payload::LandingAbandoned { .. }
            | Payload::CheckpointRecorded { .. }
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
            | Payload::ContextCaptureReported { .. } => {}
            // An event this build does not recognize. Typed loading skips
            // unknown events before replay, so this arm is defensive: keep the
            // raw history intact without mutating the derived view.
            Payload::ContextDeclared { .. } => state.round_trip.observe(&ev.payload),
            Payload::PublicationVerified { .. } => {}
            Payload::Unknown => {}
        }
    }
    let unusable_source = |id: &str| {
        state
            .forge
            .observations
            .iter()
            .find(|r| r.names_event(id))
            .is_none_or(|r| !r.usable())
    };
    for link in &mut state.forge.links {
        link.unresolved_binding |= link.observation.as_deref().is_some_and(&unusable_source);
        if link.unresolved_binding {
            link.provider_identity = None;
            link.read_at = None;
            link.resolved_head_sha = None;
        }
    }
    state.forge.link = state.forge.links.last().cloned();
    let bad_link = |id: &str| {
        state
            .forge
            .links
            .iter()
            .find(|link| link.event_id == id)
            .is_none_or(|link| link.unresolved_binding)
    };
    for checks in state
        .forge
        .checks
        .iter_mut()
        .chain(&mut state.forge.checks_history)
    {
        checks.unresolved_binding |= checks.observation.as_deref().is_some_and(&unusable_source)
            || checks.link_event_id.as_deref().is_some_and(&bad_link);
        if checks.unresolved_binding {
            checks.read_at = None;
            checks.resolved_pr_head = None;
        }
    }
    for lifecycle in &mut state.forge.pr_states {
        lifecycle.unresolved_binding |= lifecycle
            .observation
            .as_deref()
            .is_some_and(&unusable_source)
            || lifecycle.link_event_id.as_deref().is_some_and(&bad_link);
        if lifecycle.unresolved_binding {
            lifecycle.read_at = None;
            lifecycle.resolved_pr_head = None;
            lifecycle.resolved_merge_sha = None;
        }
    }
    for record in &state.forge.observations {
        for (kind, message) in [
            (
                "observation-conflict",
                record
                    .conflict
                    .then(|| "read_id has conflicting documents".to_string()),
            ),
            ("observation-invalid", record.invalid.clone()),
        ] {
            if let Some(message) = message {
                state
                    .forge
                    .diagnostics
                    .push(crate::forge_observation::ForgeDiagnostic {
                        event_id: record.event_id.clone(),
                        kind,
                        message,
                    });
            }
        }
    }
    for event in events {
        let source = match &event.payload {
            Payload::ForgeLink { observation, .. }
            | Payload::ForgeChecks { observation, .. }
            | Payload::ForgePrState { observation, .. } => observation,
            _ => continue,
        };
        if source.as_deref().is_some_and(&unusable_source)
            && !state
                .forge
                .diagnostics
                .iter()
                .any(|d| d.event_id == event.event_id)
        {
            state
                .forge
                .diagnostics
                .push(crate::forge_observation::ForgeDiagnostic {
                    event_id: event.event_id.clone(),
                    kind: "projection-unresolved",
                    message: "observation unavailable, conflicting or invalid".into(),
                });
        }
    }
    if legacy_checks_binding && state.forge.relinked() {
        for checks in state
            .forge
            .checks
            .iter_mut()
            .chain(state.forge.checks_history.last_mut())
        {
            checks.link_event_id = None;
        }
    }
    for reply in replies {
        attach_reply(&mut state, reply);
    }
    state.settle_debt();
    Ok(state)
}

fn attach_reply(state: &mut ChangeState, reply: &Event) {
    let Payload::ReplyAdded {
        parent_event_id,
        body,
    } = &reply.payload
    else {
        return;
    };
    if let Some(comment) = state
        .comments
        .iter_mut()
        .find(|comment| &comment.event_id == parent_event_id)
    {
        comment
            .replies
            .push((reply.event_id.clone(), reply.actor.clone(), body.clone()));
        return;
    }
    if let Some(finding) = state.findings.get_mut(parent_event_id) {
        finding.replies.push(ReplyEntry {
            event_id: reply.event_id.clone(),
            actor: reply.actor.clone(),
            body: body.clone(),
        });
        return;
    }
    if let Some(finding) = state.audit_findings.get_mut(parent_event_id) {
        finding.replies.push(ReplyEntry {
            event_id: reply.event_id.clone(),
            actor: reply.actor.clone(),
            body: body.clone(),
        });
        return;
    }
    let mut matches = state
        .findings
        .values()
        .filter(|finding| &finding.origin_event == parent_event_id)
        .map(|finding| finding.id.clone());
    if let Some(finding_id) = matches.next() {
        if matches.next().is_some() {
            return;
        }
        if let Some(finding) = state.findings.get_mut(&finding_id) {
            finding.replies.push(ReplyEntry {
                event_id: reply.event_id.clone(),
                actor: reply.actor.clone(),
                body: body.clone(),
            });
        }
        return;
    }
    let mut audit_matches = state
        .audit_findings
        .values()
        .filter(|finding| &finding.origin_event == parent_event_id)
        .map(|finding| finding.id.clone());
    let Some(finding_id) = audit_matches.next() else {
        return;
    };
    if audit_matches.next().is_some() {
        return;
    }
    if let Some(finding) = state.audit_findings.get_mut(&finding_id) {
        finding.replies.push(ReplyEntry {
            event_id: reply.event_id.clone(),
            actor: reply.actor.clone(),
            body: body.clone(),
        });
    }
}

fn add_run_terminal(
    runs: &mut [VerificationRunEntry],
    run_id: &str,
    revision: &str,
    terminal: VerificationRunTerminal,
) -> Result<()> {
    let run = runs
        .iter_mut()
        .find(|run| run.run_id == run_id)
        .ok_or_else(|| anyhow::anyhow!("verification references unknown or later run {run_id}"))?;
    if run.revision != revision {
        bail!(
            "verification run {run_id} is at {} but terminal is at {revision}",
            run.revision
        );
    }
    let gate = terminal.gate();
    if !run.gates.iter().any(|declared| declared.name == gate) {
        bail!("verification run {run_id} does not declare gate {gate}");
    }
    if run.terminals.iter().any(|existing| existing.gate() == gate) {
        bail!("verification run {run_id} has duplicate terminal edge for gate {gate}");
    }
    run.terminals.push(terminal);
    run.missing_gates = run
        .gates
        .iter()
        .filter(|declared| {
            !run.terminals
                .iter()
                .any(|terminal| terminal.gate() == declared.name)
        })
        .map(|declared| declared.name.clone())
        .collect();
    run.complete = run.missing_gates.is_empty();
    Ok(())
}

fn git_identity(name: &Option<String>, email: &Option<String>) -> Option<GitIdentity> {
    name.as_ref().map(|name| GitIdentity {
        name: name.clone(),
        email: email.clone(),
    })
}

/// A claim actor matches a git identity when it equals its name or email.
fn identity_matches(identity: &GitIdentity, actor: &str) -> bool {
    identity.name == actor || identity.email.as_deref() == Some(actor)
}

/// Compare a claim actor with Git identities only when the project assigns a
/// distinct Git identity to each actor.
pub(crate) fn provenance_mismatch(
    mode: crate::config::GitIdentityMode,
    claim_actor: Option<&str>,
    author: Option<&GitIdentity>,
    committer: Option<&GitIdentity>,
) -> Option<bool> {
    if mode == crate::config::GitIdentityMode::Shared {
        return None;
    }
    claim_actor.zip(author).map(|(actor, author)| {
        !(identity_matches(author, actor)
            || committer.is_some_and(|committer| identity_matches(committer, actor)))
    })
}

impl ChangeState {
    /// A declared review obligation with no audit answering it yet.
    ///
    /// The debt is what makes integration without an independent verdict
    /// honest rather than silent: it survives closure, and
    /// `arc query --debt` finds it once a reviewer is available again.
    /// Whether the declared debt still authorizes its recorded patchset.
    ///
    /// Only while it names the patchset that is about to ship. Any newer
    /// snapshot leaves the waiver behind exactly as it leaves an approval
    /// behind, so re-declaring is a deliberate act rather than a thing that
    /// happened once and never expired.
    pub fn eligible_dirty_tree_waiver(&self) -> Option<&DirtyTreeWaiver> {
        self.dirty_tree_waiver.as_ref().filter(|waiver| {
            self.privilege_scope.as_ref().is_none_or(|scope| {
                scope.permits(
                    &waiver.event_id,
                    &waiver.origin_replica_id,
                    crate::import_authorization::Role::Waiver,
                )
            })
        })
    }

    pub fn debt_waives_latest_patchset(&self) -> bool {
        let Some(debt) = &self.debt else {
            return false;
        };
        let Some(declared_for) = debt.patchset_id.as_deref() else {
            return false;
        };
        self.privilege_scope.as_ref().is_none_or(|scope| {
            scope.permits(
                &debt.event_id,
                &debt.origin_replica_id,
                crate::import_authorization::Role::Waiver,
            )
        }) && self
            .latest_patchset()
            .is_some_and(|patchset| patchset.id == declared_for)
    }

    /// A review this change owes and nobody has recorded, by any route.
    ///
    /// Scoped to integrated changes because that is when the obligation is
    /// actionable: an audit reviews a revision that shipped, so a debt on an
    /// open change is a pending waiver rather than owed work. Queueing it
    /// earlier would offer a reviewer an item `arc audit` then refuses.
    /// The provisional approval still gating this change, if one is.
    ///
    /// One derivation, so the JSON flag, `arc query --provisional`, and the
    /// `check` advisory cannot disagree about the same obligation. Reading
    /// the *latest* verdict instead would do both wrong things at once: a
    /// later verdict of any kind would mask a provisional approval that still
    /// gates, and a provisional approval left behind by a new patchset would
    /// keep reporting an obligation that gates nothing.
    ///
    /// Independent of integration, unlike debt: a provisional approval
    /// is an open obligation the moment it is recorded, because the point is
    /// to see it before the merge rather than after.
    pub fn outstanding_provisional_approval(&self) -> Option<&VerdictEntry> {
        let latest = self.latest_patchset()?;
        // A sole authoritative approval must gate this patchset before its
        // review history can owe corroboration.
        self.latest_verdict().filter(|verdict| {
            verdict.verdict == Verdict::Approved && verdict.patchset_id == latest.id
        })?;
        let provisional = self
            .authoritative_verdict_history()
            .into_iter()
            .rev()
            .find(|verdict| {
                verdict.verdict == Verdict::Approved
                    && verdict.provisional.is_some()
                    && verdict.patchset_id == latest.id
            })?;
        (!self.corroborates(latest, provisional)).then_some(provisional)
    }

    pub fn provisional_approval_outstanding(&self) -> bool {
        self.outstanding_provisional_approval().is_some()
    }

    /// Whether anything since has supplied the corroboration a provisional
    /// approval was owed.
    ///
    /// Either a later unqualified approval of the same patchset or a later
    /// audit discharges it — the obligation is for a second judgment, not for
    /// one particular command to supply it. Both must come from somebody
    /// else: a reviewer confirming its own unproven verdict is the one thing
    /// that cannot be corroboration.
    fn corroborates(&self, patchset: &Patchset, provisional: &VerdictEntry) -> bool {
        let reviewer = provisional.effective_author();
        // Neither the reviewer being corroborated nor any contributor can
        // supply it. Excluding only the reviewer would let a contributor clear
        // the obligation by approving their own change.
        let independent = |author: &str, assumed: bool| {
            author != reviewer && patchset.contributor_match(author).is_none() && !assumed
        };
        let later_clean_approval = self.verdicts.iter().any(|verdict| {
            self.review_eligible(&verdict.event_id, &verdict.origin_replica_id)
                && verdict.created_at > provisional.created_at
                && verdict.verdict == Verdict::Approved
                && verdict.provisional.is_none()
                && verdict.patchset_id == provisional.patchset_id
                && independent(verdict.effective_author(), verdict.author_assumed())
        });
        let later_audit = self.audit_verdicts.iter().any(|audit| {
            self.review_eligible(&audit.event_id, &audit.origin_replica_id)
                && audit.created_at > provisional.created_at
                && independent(audit.effective_author(), audit.author_assumed())
        });
        later_clean_approval || later_audit
    }

    /// Whether a review is still owed: the change integrated and nothing
    /// eligible has answered the debt. Readiness and waivers read this; a
    /// presentation of where the debt stands reads [`Self::debt_standing`],
    /// because "not owed yet" is not "discharged".
    pub fn debt_outstanding(&self) -> bool {
        self.debt_standing() == Some(DebtStanding::Owed)
    }

    /// Where the held debt stands, `None` when no debt was declared.
    pub fn debt_standing(&self) -> Option<DebtStanding> {
        let debt = self.debt.as_ref()?;
        let integrated = self
            .closure
            .as_ref()
            .is_some_and(|closure| closure.outcome == Closure::Integrated);
        Some(if !integrated {
            DebtStanding::Declared
        } else if self.debt_discharged(debt) {
            DebtStanding::Discharged
        } else {
            DebtStanding::Owed
        })
    }

    /// The patchset a debt is read against: the one about to ship while the
    /// change is open, and the one that did ship once it closed.
    pub fn debt_subject_patchset(&self) -> Option<&Patchset> {
        if !self.is_closed() {
            return self.latest_patchset();
        }
        self.closure_shipped_patchset()
    }

    /// The patchset a closed change shipped, read from the ledger.
    ///
    /// A closure that recorded its source names it exactly. A historical
    /// integrated closure recorded only the integrated commit, so the
    /// patchset is the one whose head that commit is, else the latest one
    /// recorded before the closure. A change that did not integrate shipped
    /// nothing.
    pub(crate) fn closure_shipped_patchset(&self) -> Option<&Patchset> {
        let closure = self.closure.as_ref()?;
        if let Some(shipped) = closure.source_patchset_id.as_deref() {
            return self
                .patchsets
                .iter()
                .find(|patchset| patchset.id == shipped);
        }
        if closure.outcome != Closure::Integrated {
            return None;
        }
        if let Some(integrated) = closure.integrated_commit.as_deref() {
            if let Some(patchset) = self
                .patchsets
                .iter()
                .find(|patchset| patchset.head == integrated)
            {
                return Some(patchset);
            }
        }
        self.patchsets
            .iter()
            .rev()
            .find(|patchset| patchset.created_at <= closure.created_at)
    }

    /// The head a closed change is judged at, read from the ledger alone: the
    /// one its closure recorded, else its newest patchset's. `None` while the
    /// change is open, whose head is its branch.
    pub fn closed_head(&self) -> Option<&str> {
        let closure = self.closure.as_ref()?;
        closure.source_head.as_deref().or_else(|| {
            self.latest_patchset()
                .map(|patchset| patchset.head.as_str())
        })
    }

    /// What kind of deficit a debt declared now would carry, read off the
    /// ledger.
    ///
    /// A caller's declared kind always wins over this. Arc cannot tell a merge
    /// resolution from a repair — both are authored work on top of an approved
    /// patchset — so `merge-resolution-unread` is only ever asserted.
    pub fn derive_debt_missing(&self, subject: Option<&Patchset>) -> DebtMissing {
        if self.verdicts.is_empty() {
            return DebtMissing::NothingRead;
        }
        let Some(subject) = subject else {
            return DebtMissing::IndependentReview;
        };
        let read_it: Vec<&VerdictEntry> = self
            .verdicts
            .iter()
            .filter(|verdict| verdict.patchset_id == subject.id)
            .collect();
        if read_it.is_empty() {
            return DebtMissing::RepairUnread;
        }
        if read_it.iter().all(|verdict| {
            subject
                .contributor_match(verdict.effective_author())
                .is_some()
        }) {
            return DebtMissing::ContributorOnly;
        }
        DebtMissing::IndependentReview
    }

    /// Why `verdict` is a findings read under `review_clearance =
    /// "assessed"`, or `None` when it clears.
    pub fn verdict_clearance_gap(&self, verdict: &VerdictEntry) -> Option<FindingsReason> {
        self.read_clearance_gap(&Read::verdict(verdict))
    }

    pub fn audit_clearance_gap(&self, audit: &AuditVerdictEntry) -> Option<FindingsReason> {
        self.read_clearance_gap(&Read::audit(audit))
    }

    pub fn verdict_ladder_clearance(&self, verdict: &VerdictEntry) -> Option<LadderClearance> {
        self.ladder_clearance(&Read::verdict(verdict))
    }

    pub fn verdict_uses_assessed(
        &self,
        verdict: &VerdictEntry,
        fallback: crate::policy::ReviewClearance,
    ) -> bool {
        self.read_uses_assessed(&Read::verdict(verdict), fallback)
    }

    pub fn audit_uses_assessed(&self, audit: &AuditVerdictEntry) -> bool {
        self.read_uses_assessed(&Read::audit(audit), self.review_clearance)
    }

    pub fn has_assessed_clearance(&self) -> bool {
        self.review_clearance.is_assessed()
            || self
                .review_context
                .as_ref()
                .is_some_and(|context| context.registry(self.is_closed()).assessed() == Some(true))
    }

    pub fn set_review_context(&mut self, context: Option<crate::review_context::ReviewContext>) {
        self.review_context = context;
        self.settle_debt();
    }

    fn read_uses_assessed(
        &self,
        read: &Read<'_>,
        fallback: crate::policy::ReviewClearance,
    ) -> bool {
        fallback.is_assessed()
            || self.review_context.as_ref().is_some_and(|context| {
                context
                    .registry(crate::review_context::after_integration(
                        self,
                        read.event_id,
                        read.audit,
                    ))
                    .assessed()
                    == Some(true)
            })
    }

    fn read_clearance_gap(&self, read: &Read<'_>) -> Option<FindingsReason> {
        self.ladder_clearance(read)
            .map(|clearance| clearance.gap)
            .unwrap_or_else(|| clearance_gap(read.author, read.model, read.assessment))
    }

    fn ladder_clearance(&self, read: &Read<'_>) -> Option<LadderClearance> {
        let context = self.review_context.as_ref()?;
        let source = context.registry(crate::review_context::after_integration(
            self,
            read.event_id,
            read.audit,
        ));
        let assessed = source.assessed()?;
        if !assessed {
            return None;
        }
        let judgment = context.judgments.get(read.event_id);
        let gap = if !self.review_eligible(read.event_id, read.origin) {
            Some(FindingsReason::Ladder {
                code: "review-authority-unavailable",
                message: "read has no receiver review authority".into(),
            })
        } else if read.author_assumed {
            Some(FindingsReason::Ladder {
                code: "reader-identity-unestablished",
                message: "reader identity was assumed rather than declared".into(),
            })
        } else {
            match judgment {
                Some(judgment) if judgment.clears == Some(true) => read
                    .assessment
                    .filter(|_| !crate::coordinate::is_person(read.author))
                    .filter(|assessment| assessment.binding != Some(AuditorBinding::Bound))
                    .map(|assessment| FindingsReason::Ladder {
                        code: "unbound",
                        message: format!(
                            "assessment does not bind the read: {}",
                            assessment
                                .binding
                                .and_then(|binding| binding.failure())
                                .unwrap_or("binding not recorded")
                        ),
                    }),
                Some(judgment) => Some(FindingsReason::Ladder {
                    code: judgment.reason.unwrap_or("not-established"),
                    message: judgment
                        .details
                        .iter()
                        .chain(&judgment.conservative_bounds)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("; "),
                }),
                None => Some(FindingsReason::Ladder {
                    code: "not-established",
                    message: "ladder judgment not established for the read".into(),
                }),
            }
        };
        Some(LadderClearance { gap })
    }

    /// The approval of `subject` that, under `review_clearance =
    /// "assessed"`, finds without clearing, as the `ineligible-review` record
    /// a debt declared for it names: the sole approving verdict tip on
    /// `subject` when it does not clear, else, with no verdict tip on
    /// `subject`, the newest external approval of its head, which carries no
    /// assessment. `None` when a clearing approval stands or nothing
    /// approves.
    pub fn findings_approval(&self, subject: &Patchset) -> Option<IneligibleRead> {
        let tips = self.eligible_verdict_tips();
        let on_subject: Vec<_> = tips
            .iter()
            .filter(|verdict| verdict.patchset_id == subject.id)
            .collect();
        if let [verdict] = on_subject.as_slice() {
            if verdict.verdict != Verdict::Approved {
                return None;
            }
            let why = self.verdict_clearance_gap(verdict)?;
            let assessment = verdict.assessment.as_ref();
            return Some(IneligibleRead {
                read: verdict.event_id.clone(),
                qualifiers: if matches!(&why, FindingsReason::Ladder { .. }) {
                    why.qualifiers()
                } else {
                    assessment
                        .map(|assessed| assessed.qualifiers.clone())
                        .unwrap_or_default()
                },
                findings: Some(why.as_str().to_string()),
                route_version: assessment
                    .and_then(|assessed| assessed.route_version.clone())
                    .or_else(|| verdict.route_version.clone()),
                assessment: if assessment.is_some() {
                    AssessmentBasis::Assessed
                } else {
                    AssessmentBasis::Unassessed
                },
            });
        }
        if !on_subject.is_empty() {
            return None;
        }
        self.external_verdicts
            .iter()
            .rev()
            .find(|external| {
                external.verdict == crate::model::ExternalVerdict::Approved
                    && (external.revision == subject.head
                        || external.revision == subject.approved_head)
            })
            .map(|external| IneligibleRead {
                read: external.event_id.clone(),
                qualifiers: Vec::new(),
                findings: Some(FindingsReason::NoAssessment.as_str().to_string()),
                route_version: None,
                assessment: AssessmentBasis::Unassessed,
            })
    }

    /// Every verdict recorded on one patchset, at the coordinates it was cast
    /// at.
    pub fn debt_coverage_for(&self, subject: &Patchset) -> Vec<DebtCoverage> {
        self.verdicts
            .iter()
            .filter(|verdict| verdict.patchset_id == subject.id)
            .map(|verdict| {
                DebtCoverage::new(
                    verdict.effective_author().to_string(),
                    verdict.model.clone(),
                    verdict.session.clone(),
                    verdict.route_version.clone(),
                )
            })
            .collect()
    }

    /// The coordinate the ledger credits to `actor` in an authoring record at
    /// or before `subject`. A later authoring record belongs to a later
    /// patchset, and planning, opening and other ceremony name no authorship,
    /// so neither can rewrite what this patchset recorded. A record that
    /// names no model establishes no coordinate and does not erase one an
    /// earlier record named.
    fn recorded_authoring_identity(&self, subject: &Patchset, actor: &str) -> Option<DebtIdentity> {
        let end = self
            .patchsets
            .iter()
            .position(|patchset| patchset.id == subject.id)
            .map_or(0, |index| index + 1);
        self.authoring_identity_before(end, actor)
    }

    /// The newest coordinate among the records before `end` that credits
    /// `actor` with authorship. Retained author evidence credits its coordinates, a
    /// declared contributor set credits its members, an `--on-behalf-of`
    /// subject credits itself, and an ordinary snapshot credits its recorder.
    /// A record that only replayed or recorded somebody else's work credits
    /// nothing, and one that names no model establishes no coordinate and does
    /// not erase one an earlier record named.
    fn authoring_identity_before(&self, end: usize, actor: &str) -> Option<DebtIdentity> {
        let mut established = None;
        for record in &self.patchsets[..end] {
            let coordinate = if let Some(inherited) = record
                .inherited_authors
                .iter()
                .find(|identity| actor == identity.actor || actor == identity.effective_actor())
            {
                inherited.clone()
            } else if !record.contributors.is_empty() {
                if !record
                    .contributors
                    .iter()
                    .any(|contributor| contributor == actor)
                {
                    continue;
                }
                if actor == record.actor {
                    record.recorded_coordinate()
                } else {
                    established
                        .clone()
                        .unwrap_or_else(|| identity_from_role_value(actor))
                }
            } else if let Some(subject) = record.on_behalf_of.as_deref() {
                if subject != actor {
                    continue;
                }
                if actor == record.actor {
                    record.recorded_coordinate()
                } else {
                    established
                        .clone()
                        .unwrap_or_else(|| identity_from_role_value(actor))
                }
            } else if actor == record.actor {
                record.recorded_coordinate()
            } else {
                continue;
            };
            if coordinate.model.is_some() {
                established = Some(coordinate);
            }
        }
        established
    }

    /// The identity a patchset's implementer is credited with: the coordinate
    /// retained for that contributor, its recorded contributor set
    /// when one was declared, its subject when one was named, else the session
    /// that recorded it.
    pub(crate) fn implementer_identity(&self, subject: &Patchset) -> DebtIdentity {
        if !subject.contributors.is_empty() {
            let contributor = subject
                .on_behalf_of
                .as_deref()
                .filter(|named| subject.contributors.iter().any(|value| value == named))
                .unwrap_or(&subject.contributors[0]);
            return self.contributor_identity(subject, contributor);
        }
        if let Some(inherited) = subject.inherited_authors.first() {
            return inherited.clone();
        }
        let Some(named) = subject.on_behalf_of.as_deref() else {
            return DebtIdentity::new(
                subject.actor.clone(),
                None,
                subject.harness.clone(),
                subject.model.clone(),
                subject.session.clone(),
            );
        };
        let parsed = identity_from_role_value(named);
        let mut identity = if named == subject.actor {
            subject.recorded_coordinate()
        } else {
            self.recorded_authoring_identity(subject, named)
                .unwrap_or(parsed)
        };
        if identity.model.is_none() {
            if let Some(parsed) = identity_from_role_value(named).model {
                identity.model = Some(parsed);
            }
        }
        identity
    }

    /// The coordinate the ledger holds for one contributor of `patchset`: a
    /// retained author coordinate when it names that contributor, this
    /// patchset's own coordinate when it names its recorder, else the
    /// coordinate that contributor's authoring record at or before this
    /// patchset names.
    fn contributor_identity(&self, patchset: &Patchset, contributor: &str) -> DebtIdentity {
        if let Some(inherited) = patchset.inherited_authors.iter().find(|identity| {
            contributor == identity.actor || contributor == identity.effective_actor()
        }) {
            return inherited.clone();
        }
        if contributor == patchset.actor {
            return patchset.recorded_coordinate();
        }
        self.recorded_authoring_identity(patchset, contributor)
            .unwrap_or_else(|| identity_from_role_value(contributor))
    }

    /// The coordinate of every author `patchset` names: one per member of a
    /// declared contributor set, else its effective author.
    pub(crate) fn authoring_coordinates(&self, patchset: &Patchset) -> Vec<DebtIdentity> {
        if patchset.contributors.is_empty() {
            return vec![self.implementer_identity(patchset)];
        }
        patchset
            .contributors
            .iter()
            .map(|contributor| self.contributor_identity(patchset, contributor))
            .collect()
    }

    /// Who set the contract one patchset answered, who answered it, who
    /// recorded it for its implementer, and who integrated it.
    ///
    /// `trailer_implementers` is what the range's commits name, or `None`
    /// when no one read them. It is a projection kept for comparison, never
    /// the source of a role.
    pub fn debt_production_for(
        &self,
        subject: &Patchset,
        trailer_implementers: Option<Vec<String>>,
    ) -> DebtProduction {
        let brief = self.brief_for(subject);
        let planner = brief
            .and_then(|brief| brief.plan_source.as_ref())
            .and_then(|source| source.planners.first())
            .map(identity_from_planner)
            .or_else(|| {
                self.contribution_roles
                    .get("Planned-by")
                    .and_then(|values| values.first())
                    .map(|value| identity_from_role_value(value))
            })
            .or_else(|| {
                brief.map(|brief| {
                    DebtIdentity::new(
                        brief.actor.clone(),
                        brief.on_behalf_of.clone(),
                        brief.harness.clone(),
                        brief.model.clone(),
                        brief.session.clone(),
                    )
                })
            });
        let implementer = self.implementer_identity(subject);
        let orchestrator = if subject.on_behalf_of.is_some() {
            Some(DebtIdentity::new(
                subject.actor.clone(),
                None,
                subject.harness.clone(),
                subject.model.clone(),
                subject.session.clone(),
            ))
        } else {
            self.contribution_roles
                .get("Orchestrated-by")
                .and_then(|values| values.first())
                .map(|value| identity_from_role_value(value))
        };
        let integrator = self
            .closure
            .as_ref()
            .and_then(|closure| closure.integrator.clone());
        let following_brief = planner
            .as_ref()
            .is_some_and(|planner| planner.effective_actor() != implementer.effective_actor());
        DebtProduction {
            planner,
            brief_version: subject.brief_version,
            implementer,
            orchestrator,
            integrator,
            trailer_implementers,
            following_brief,
        }
    }

    /// Every identity the ledger credits with authoring `patchset`: its
    /// recorded contributor set or effective author, and the production
    /// record's implementer when that identity is not excluded by an explicit
    /// contributor set. A production record supplies the only authorship a
    /// debt can compare against; without one the patchset's own identity is
    /// the ledger's whole answer.
    pub fn authoring_identities(
        &self,
        patchset: &Patchset,
        production: Option<&DebtProduction>,
    ) -> Vec<DebtIdentity> {
        let mut identities = Vec::new();
        if let Some(production) = production {
            let implementer = &production.implementer;
            let declared = patchset.contributors.is_empty()
                || patchset.contributors.iter().any(|contributor| {
                    contributor == &implementer.actor
                        || contributor == implementer.effective_actor()
                });
            if declared {
                identities.push(implementer.clone());
            }
        }
        if patchset.contributors.is_empty() {
            identities.push(self.implementer_identity(patchset));
        } else {
            for contributor in &patchset.contributors {
                // A retained author coordinate is the one the ledger
                // observed, model included. Looking the actor up across the
                // whole change instead would see the replayer's later model
                // and lose the recorded one.
                identities.push(self.contributor_identity(patchset, contributor));
            }
        }
        let mut unique = Vec::new();
        for identity in identities {
            if !unique.contains(&identity) {
                unique.push(identity);
            }
        }
        unique
    }

    /// How an approving read under `actor`, judged under each of `models`,
    /// is authorship of `patchset`, when it is: the author it names, and the
    /// observed model that named it when the recorded one did not.
    /// Authorship refuses; planning, orchestrating and integrating somebody
    /// else's work do not.
    ///
    /// Only ledger identities decide this. A model the ledger did not record
    /// for an author is not established by commit text, so a read is judged
    /// by the actor, session, harness and model the ledger holds.
    pub fn authorship_match(
        &self,
        patchset: &Patchset,
        production: Option<&DebtProduction>,
        actor: &str,
        models: &[crate::model::ReaderModel],
    ) -> Option<Authorship> {
        let identities = self.authoring_identities(patchset, production);
        if let Some(identity) = identities
            .iter()
            .find(|identity| identity.effective_actor() == actor)
        {
            return Some(Authorship {
                identity: identity.effective_actor().to_string(),
                observed: None,
            });
        }
        for reader in models {
            if let Some(identity) = identities
                .iter()
                .find(|identity| identity_names(identity, actor, &[reader.model.as_str()]))
            {
                return Some(Authorship {
                    identity: identity.effective_actor().to_string(),
                    observed: reader.declared.is_some().then(|| reader.clone()),
                });
            }
        }
        None
    }

    /// The authoring model coordinates an independence warning can name, as
    /// the ledger records them. Declared commit text is not among them.
    pub fn authoring_model_values(
        &self,
        patchset: &Patchset,
        production: Option<&DebtProduction>,
    ) -> Vec<String> {
        let mut values = Vec::new();
        for identity in self.authoring_identities(patchset, production) {
            if let Some(model) = identity.model.as_deref() {
                let value = match identity.harness.as_deref() {
                    Some(harness) => format!("{harness}:{model}"),
                    None => model.to_string(),
                };
                if !values.contains(&value) {
                    values.push(value);
                }
            }
        }
        values
    }

    /// The non-authoring roles `actor` held on `subject`, as the ledger names
    /// them. Authorship is answered separately and refuses; these roles only
    /// make the read dirty context.
    pub fn held_review_roles(
        &self,
        subject: &Patchset,
        actor: &str,
        models: &[&str],
    ) -> Vec<crate::model::ReviewRole> {
        let mut roles = std::collections::BTreeSet::new();
        let names = |identity: &DebtIdentity| identity_names(identity, actor, models);
        let mut planners: Vec<DebtIdentity> = Vec::new();
        if let Some(source) = self
            .brief_for(subject)
            .and_then(|brief| brief.plan_source.as_ref())
        {
            planners.extend(source.planners.iter().map(identity_from_planner));
        }
        if let Some(values) = self.contribution_roles.get("Planned-by") {
            planners.extend(values.iter().map(|value| identity_from_role_value(value)));
        }
        if let Some(planner) = self.debt_production_for(subject, None).planner {
            planners.push(planner);
        }
        if planners.iter().any(names) {
            roles.insert(crate::model::ReviewRole::Planner);
        }
        let production = self.debt_production_for(subject, None);
        let mut orchestrators: Vec<DebtIdentity> = production.orchestrator.into_iter().collect();
        if let Some(values) = self.contribution_roles.get("Orchestrated-by") {
            orchestrators.extend(values.iter().map(|value| identity_from_role_value(value)));
        }
        if orchestrators.iter().any(names) {
            roles.insert(crate::model::ReviewRole::Orchestrator);
        }
        crate::model::ReviewRole::normalized(roles)
    }

    /// Any independent verdict on the revision that shipped, recorded after
    /// the debt was declared, discharges it — whichever command emitted it.
    ///
    /// The debt records that no verdict existed, not that one particular
    /// command must supply it. Requiring `arc audit` specifically left an
    /// operator who reviewed *before* merging with no honest move: leave a
    /// debt standing for a review that happened, or file a post-integration
    /// audit that did not. The verdict's outcome, and anything it found, live
    /// in the verdict and its findings rather than in the debt.
    fn debt_discharged(&self, debt: &Debt) -> bool {
        self.debt_discharge(debt).is_some()
    }

    /// Settle what the ledger says of the held debt once every event is in:
    /// what discharged it, and for an `ineligible-review` debt the read it
    /// names and the audits since that left it open.
    pub(crate) fn settle_debt(&mut self) {
        let Some(debt) = self.debt.clone() else {
            return;
        };
        let discharged_by = self.debt_discharge(&debt);
        let standing = self.debt_standing().expect("held debt");
        let findings_reads = self.findings_reads(&debt);
        let partial_coverage = self.partial_coverage(&debt);
        let discharge_extent = discharged_by
            .as_ref()
            .and_then(|(event_id, _)| self.discharge_extent(&debt, event_id));
        let read = debt.ineligible.as_ref().map(|read| {
            let reviewer = self
                .verdicts
                .iter()
                .find(|verdict| verdict.event_id == read.record.read)
                .map(|verdict| (verdict.effective_author(), verdict.model.clone()))
                .or_else(|| {
                    self.audit_verdicts
                        .iter()
                        .find(|audit| audit.event_id == read.record.read)
                        .map(|audit| (audit.effective_author(), audit.model.clone()))
                });
            let cleared = discharged_by.as_ref().map(|(id, _)| id.as_str());
            DebtRead {
                record: read.record.clone(),
                reviewer: reviewer.as_ref().map(|(reviewer, _)| reviewer.to_string()),
                reviewer_model: reviewer.and_then(|(_, model)| model),
                later_reads: self
                    .audit_verdicts
                    .iter()
                    .filter(|audit| {
                        audit.created_at >= debt.declared_at
                            && Some(audit.event_id.as_str()) != cleared
                    })
                    .map(|audit| LaterRead {
                        event_id: audit.event_id.clone(),
                        reviewer: audit.effective_author().to_string(),
                        model: audit.model.clone(),
                        model_recorded: (!crate::coordinate::model_recorded(
                            audit.model.as_deref(),
                            audit.session.as_deref(),
                        ))
                        .then_some(false),
                        session: audit.session.clone(),
                        independence: audit.assessment.as_ref().map(|a| a.independence),
                        qualifiers: audit
                            .assessment
                            .as_ref()
                            .map(|a| a.qualifiers.clone())
                            .unwrap_or_default(),
                        route_version: audit
                            .assessment
                            .as_ref()
                            .and_then(|a| a.route_version.clone())
                            .or_else(|| audit.route_version.clone()),
                        binding: audit.assessment.as_ref().and_then(|a| a.binding),
                        uncovered_authors: audit
                            .assessment
                            .as_ref()
                            .map(|a| a.uncovered_authors.clone())
                            .unwrap_or_default(),
                    })
                    .collect(),
            }
        });
        let integrator = self
            .closure
            .as_ref()
            .and_then(|closure| closure.integrator.clone());
        let current = self.debt.as_mut().expect("held debt");
        if let (Some(production), Some(integrator)) = (current.production.as_mut(), integrator) {
            production.integrator = Some(integrator);
        }
        current.findings_reads = findings_reads;
        current.partial_coverage = partial_coverage;
        current.discharge_extent = discharge_extent;
        current.discharged_by_event_id = discharged_by.as_ref().map(|(id, _)| id.clone());
        current.discharged_by = discharged_by.map(|(_, coverage)| coverage);
        current.ineligible = read;
        current.standing = standing;
    }

    /// The verdict or audit that discharges `debt`: the first audit, else the
    /// first verdict on the shipped patchset, that [`DischargeBar`] does not
    /// bar. A `replayed-unread` debt owes a read of the context its patchset
    /// landed in, which only an audit of the integrated revision gives; a
    /// verdict on the patchset read the edit alone. The returned model is
    /// copied from that event and is never inferred.
    fn debt_discharge(&self, debt: &Debt) -> Option<(String, DebtCoverage)> {
        // Only a range with no ledger record at all has no authorship to
        // compare a reader against, and nothing discharges its debt.
        let shipped = self.closure_shipped_patchset()?;
        if let Some(audit) = self.audit_verdicts.iter().find(|audit| {
            self.discharge_bar(debt, shipped, &Read::audit(audit))
                .is_none()
        }) {
            return Some((
                audit.event_id.clone(),
                DebtCoverage::new(
                    audit.effective_author().to_string(),
                    audit.model.clone(),
                    audit.session.clone(),
                    audit.route_version.clone(),
                ),
            ));
        }
        if debt.missing == Some(DebtMissing::ReplayedUnread) {
            return None;
        }
        // A verdict on an earlier draft judged something else.
        self.verdicts
            .iter()
            .filter(|verdict| verdict.patchset_id == shipped.id)
            .find(|verdict| {
                self.discharge_bar(debt, shipped, &Read::verdict(verdict))
                    .is_none()
            })
            .map(|verdict| {
                (
                    verdict.event_id.clone(),
                    DebtCoverage::new(
                        verdict.effective_author().to_string(),
                        verdict.model.clone(),
                        verdict.session.clone(),
                        verdict.route_version.clone(),
                    ),
                )
            })
    }

    /// The read that would discharge the held debt were `review_clearance`
    /// absent, when the key is in force and the debt is owed under it.
    pub fn discharged_without_clearance(&self) -> Option<String> {
        if !self.has_assessed_clearance() {
            return None;
        }
        let debt = self.debt.as_ref()?;
        if debt.standing != DebtStanding::Owed {
            return None;
        }
        let mut legacy = self.clone();
        legacy.review_clearance = crate::policy::ReviewClearance::Legacy;
        legacy.review_context = None;
        legacy.debt_discharge(debt).map(|(event_id, _)| event_id)
    }

    /// The audits, then the verdicts on the shipped patchset, that `debt`
    /// bars only because they find without clearing.
    fn findings_reads(&self, debt: &Debt) -> Vec<FindingsRead> {
        let Some(shipped) = self.closure_shipped_patchset() else {
            return Vec::new();
        };
        let audits = self
            .audit_verdicts
            .iter()
            .map(|audit| ("audit", Read::audit(audit)));
        let verdicts = self
            .verdicts
            .iter()
            .filter(|verdict| verdict.patchset_id == shipped.id)
            .filter(|_| debt.missing != Some(DebtMissing::ReplayedUnread))
            .map(|verdict| ("verdict", Read::verdict(verdict)));
        audits
            .chain(verdicts)
            .filter_map(
                |(source, read)| match self.discharge_bar(debt, shipped, &read) {
                    Some(DischargeBar::FindingsRead(why)) => Some(FindingsRead {
                        kind: "findings",
                        event_id: read.event_id.to_string(),
                        source,
                        verdict: read.verdict,
                        reviewer: read.author.to_string(),
                        model: read.model.map(str::to_string),
                        reason: why.as_str(),
                        qualifiers: why.qualifiers(),
                        remedy: why.remedy(),
                    }),
                    _ => None,
                },
            )
            .collect()
    }

    /// The audits, then the verdicts on the shipped patchset, that `debt`
    /// bars only because their extent does not cover it. Each is listed on
    /// its own: several partial reads never combine into coverage.
    fn partial_coverage(&self, debt: &Debt) -> Vec<PartialCoverage> {
        let Some(shipped) = self.closure_shipped_patchset() else {
            return Vec::new();
        };
        let audits = self
            .audit_verdicts
            .iter()
            .map(|audit| ("audit", Read::audit(audit)));
        let verdicts = self
            .verdicts
            .iter()
            .filter(|verdict| verdict.patchset_id == shipped.id)
            .map(|verdict| ("verdict", Read::verdict(verdict)));
        audits
            .chain(verdicts)
            .filter_map(
                |(source, read)| match self.discharge_bar(debt, shipped, &read) {
                    Some(DischargeBar::ExtentUncovered(gap)) => Some(PartialCoverage {
                        kind: "partial",
                        event_id: read.event_id.to_string(),
                        source,
                        verdict: read.verdict,
                        reviewer: read.author.to_string(),
                        model: read.model.map(str::to_string),
                        extent: read.extent.cloned(),
                        reason: gap.as_str(),
                        paths: gap.paths().to_vec(),
                    }),
                    _ => None,
                },
            )
            .collect()
    }

    /// Whether `read` covers what `debt` requires: the debt's extent, else
    /// the whole [`extent_subject`](Self::extent_subject).
    fn coverage_of(
        &self,
        debt: &Debt,
        read: Option<&crate::extent::Extent>,
    ) -> crate::extent::Coverage {
        self.coverage_against(debt.extent.as_ref(), read)
    }

    /// Whether `read` covers an obligation naming `required`, else the
    /// whole [`extent_subject`](Self::extent_subject).
    fn coverage_against(
        &self,
        required: Option<&crate::extent::Extent>,
        read: Option<&crate::extent::Extent>,
    ) -> crate::extent::Coverage {
        let obligation = match (required, self.extent_subject()) {
            (Some(extent), _) => crate::extent::Obligation::Extent(extent),
            (None, Some((base, revision))) => {
                crate::extent::Obligation::Unestablished { base, revision }
            }
            // A replayed obligation whose landed range was not recorded has
            // no subject a stated extent could cover.
            (None, None) if read.is_some() => {
                return crate::extent::Coverage::Gap(crate::extent::CoverageGap::Revision)
            }
            (None, None) => return crate::extent::Coverage::NotEstablished,
        };
        crate::extent::covers(read, obligation)
    }

    /// The range an extent is resolved against, as `(base, revision)`: what
    /// shipped. For a member landed by replay that is the range it landed
    /// as, from the composed parent to the integrated revision, the commits
    /// the target holds and the context a `replayed-unread` obligation owes
    /// a read of; for any other change, the shipped patchset's (the latest
    /// one while it is open). `None` when there is no such range.
    pub fn extent_subject(&self) -> Option<(&str, &str)> {
        if let Some(replayed) = self.debt.as_ref().and_then(|debt| debt.replayed.as_ref()) {
            let closure = self.closure.as_ref()?;
            let base = closure.range_base.as_deref().unwrap_or(&replayed.parent);
            return Some((base, closure.integrated_commit.as_deref()?));
        }
        if let Some(landed) = self.replayed_range() {
            return Some(landed);
        }
        self.debt_subject_patchset()
            .map(|patchset| (patchset.base.as_str(), patchset.head.as_str()))
    }

    /// [`landed_range`](Self::landed_range) for a member landed by replay:
    /// one whose integrated revision is not the source head it shipped.
    pub(crate) fn replayed_range(&self) -> Option<(&str, &str)> {
        let closure = self.closure.as_ref()?;
        self.landed_range()
            .filter(|(_, revision)| closure.source_head.as_deref() != Some(revision))
    }

    /// The range a change closed by a landing occupies on its target, as
    /// `(base, revision)`: `range_base..integrated_commit`, which for a
    /// member landed by replay is the commits it landed as rather than its
    /// patchset's. `None` for any other change, whose shipped patchset's
    /// `base..head` is that range.
    pub fn landed_range(&self) -> Option<(&str, &str)> {
        let closure = self.closure.as_ref()?;
        closure.landing.as_ref()?;
        Some((
            closure.range_base.as_deref()?,
            closure.integrated_commit.as_deref()?,
        ))
    }

    /// Whether the read `event_id` discharges `debt` by an extent that
    /// covers it, or where neither side established an extent.
    fn discharge_extent(&self, debt: &Debt, event_id: &str) -> Option<&'static str> {
        let extent = self
            .audit_verdicts
            .iter()
            .find(|audit| audit.event_id == event_id)
            .and_then(|audit| audit.extent.as_ref());
        match self.coverage_of(debt, extent) {
            crate::extent::Coverage::Covered => Some("covered"),
            crate::extent::Coverage::NotEstablished => Some("not-established"),
            crate::extent::Coverage::Gap(_) => None,
        }
    }

    /// Why the audit `event_id` leaves the held debt owed, or `None` when it
    /// discharges it or there is no such audit or debt.
    pub fn audit_discharge_bar(&self, event_id: &str) -> Option<DischargeBar> {
        let debt = self.debt.as_ref()?;
        let audit = self
            .audit_verdicts
            .iter()
            .find(|audit| audit.event_id == event_id)?;
        let Some(shipped) = self.closure_shipped_patchset() else {
            return Some(DischargeBar::NoAuthorshipRecord);
        };
        self.discharge_bar(debt, shipped, &Read::audit(audit))
    }

    /// Why `read` does not discharge `debt` on the work `shipped` carries.
    /// The first bar that applies is the one named; whether the read covers
    /// the obligation's extent is asked last, of a read that otherwise
    /// discharges it.
    fn discharge_bar(
        &self,
        debt: &Debt,
        shipped: &Patchset,
        read: &Read<'_>,
    ) -> Option<DischargeBar> {
        self.reader_bar(debt, shipped, read)
            .or_else(|| match self.coverage_of(debt, read.extent) {
                crate::extent::Coverage::Covered | crate::extent::Coverage::NotEstablished => None,
                crate::extent::Coverage::Gap(gap) => Some(DischargeBar::ExtentUncovered(gap)),
            })
    }

    /// Why `read`, by who read it and when, does not discharge `debt`.
    fn reader_bar(&self, debt: &Debt, shipped: &Patchset, read: &Read<'_>) -> Option<DischargeBar> {
        if !self.review_eligible(read.event_id, read.origin) {
            return Some(DischargeBar::NoReviewAuthority);
        }
        if read.created_at < debt.declared_at {
            return Some(DischargeBar::BeforeDeclaration);
        }
        if read.verdict == Verdict::CommentOnly {
            return Some(DischargeBar::CommentOnly);
        }
        self.judged_bar(
            debt.missing,
            debt.production.as_ref(),
            shipped,
            read,
            self.review_clearance,
        )
    }

    /// Whether the read `event_id` on `shipped`, judged under `assessment`
    /// in place of the one it recorded, would discharge an
    /// `ineligible-review` debt naming `extent` under `clearance`, the
    /// destination's review clearance policy: every bar of discharge,
    /// coverage last, except that the read predates the declaration.
    /// `false` when there is no such read or its judgment is not
    /// established.
    pub fn read_would_clear(
        &self,
        event_id: &str,
        assessment: &AuditAssessment,
        shipped: &Patchset,
        production: Option<&DebtProduction>,
        extent: Option<&crate::extent::Extent>,
        clearance: crate::policy::ReviewClearance,
    ) -> bool {
        let read = self
            .verdicts
            .iter()
            .find(|verdict| verdict.event_id == event_id)
            .map(Read::verdict)
            .or_else(|| {
                self.audit_verdicts
                    .iter()
                    .find(|audit| audit.event_id == event_id)
                    .map(Read::audit)
            });
        let Some(mut read) = read else {
            return false;
        };
        read.assessment = Some(assessment);
        self.review_eligible(read.event_id, read.origin)
            && read.verdict != Verdict::CommentOnly
            && self
                .judged_bar(
                    Some(DebtMissing::IneligibleReview),
                    production,
                    shipped,
                    &read,
                    clearance,
                )
                .is_none()
            && !matches!(
                self.coverage_against(extent, read.extent),
                crate::extent::Coverage::Gap(_)
            )
    }

    /// Why `read`, judged by its reader's identity, model and assessment,
    /// does not discharge a debt of kind `missing` under `clearance`.
    fn judged_bar(
        &self,
        missing: Option<DebtMissing>,
        production: Option<&DebtProduction>,
        shipped: &Patchset,
        read: &Read<'_>,
        clearance: crate::policy::ReviewClearance,
    ) -> Option<DischargeBar> {
        let tiebreak = crate::review_independence::tiebreak(
            self,
            shipped,
            production,
            &crate::review_independence::Reader {
                actor: read.author,
                target: read.tiebreak_target,
                assumed: read.author_assumed,
                session: read.session,
                model: read.model,
                observed: read.observed,
                at: read.created_at,
                after_integration: crate::review_context::after_integration(
                    self,
                    read.event_id,
                    read.audit,
                ),
            },
            read.tiebreak_reviewer
                .map(std::slice::from_ref)
                .unwrap_or_default(),
        )
        .is_some();
        let models = if tiebreak { &[][..] } else { &read.judged };
        if self.read_uses_assessed(read, clearance) {
            if let Some(why) = self.read_clearance_gap(read) {
                return Some(DischargeBar::FindingsRead(why));
            }
            // Persons retain the eligible bound assessment requirement for
            // ineligible-review; a clearing ladder agent needs no assessment.
            let eligible = read.assessment.is_some_and(|assessment| {
                assessment.independence == Independence::Eligible
                    && assessment.binding == Some(AuditorBinding::Bound)
            });
            let ladder_agent =
                self.ladder_clearance(read).is_some() && !crate::coordinate::is_person(read.author);
            if missing == Some(DebtMissing::IneligibleReview) && !eligible && !ladder_agent {
                return Some(DischargeBar::NoEligibleAssessment);
            }
            if read.author_assumed {
                return Some(DischargeBar::AuthorAssumed);
            }
            return self
                .authorship_match(shipped, production, read.author, models)
                .map(DischargeBar::Authored);
        }
        if let Some(assessment) = read
            .assessment
            .filter(|assessment| assessment.independence == Independence::Ineligible)
        {
            return Some(DischargeBar::AssessedIneligible(
                assessment.qualifiers.clone(),
            ));
        }
        // Routing's word that the auditor is independent of the authors, in
        // an assessment that describes the model the audit recorded.
        let cleared = read.assessment.is_some_and(|assessment| {
            assessment.independence == Independence::Eligible
                && assessment.binding == Some(AuditorBinding::Bound)
        });
        if missing == Some(DebtMissing::IneligibleReview) && !cleared {
            return Some(DischargeBar::NoEligibleAssessment);
        }
        if read.author_assumed {
            return Some(DischargeBar::AuthorAssumed);
        }
        self.authorship_match(shipped, production, read.author, models)
            .map(DischargeBar::Authored)
    }
}

/// The author the ledger credits that a read is by, and the observed model
/// that named it where the read's recorded model did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorship {
    pub identity: String,
    pub observed: Option<crate::model::ReaderModel>,
}

/// A recorded verdict or audit, as the discharge rule reads it.
struct Read<'a> {
    tiebreak_reviewer: Option<&'a crate::policy::TiebreakReviewer>,
    tiebreak_target: Option<&'a str>,
    session: Option<&'a str>,
    event_id: &'a str,
    origin: &'a str,
    created_at: chrono::DateTime<chrono::Utc>,
    verdict: Verdict,
    /// Routing's assessment of the reader, when the read recorded one.
    assessment: Option<&'a AuditAssessment>,
    author_assumed: bool,
    author: &'a str,
    model: Option<&'a str>,
    observed: Option<&'a str>,
    /// The models independence judges the read under.
    judged: Vec<crate::model::ReaderModel>,
    /// What the read covered. A verdict records no extent.
    extent: Option<&'a crate::extent::Extent>,
    audit: bool,
}

impl<'a> Read<'a> {
    fn audit(audit: &'a AuditVerdictEntry) -> Self {
        Self {
            tiebreak_reviewer: audit.tiebreak_reviewer.as_ref(),
            tiebreak_target: audit.tiebreak_target.as_deref(),
            session: audit.session.as_deref(),
            event_id: &audit.event_id,
            origin: &audit.origin_replica_id,
            created_at: audit.created_at,
            verdict: audit.verdict,
            assessment: audit.assessment.as_ref(),
            author_assumed: audit.author_assumed(),
            author: audit.effective_author(),
            model: audit.model.as_deref(),
            observed: audit.observed_model.as_deref(),
            judged: audit.judged_models(),
            extent: audit.extent.as_ref(),
            audit: true,
        }
    }

    fn verdict(verdict: &'a VerdictEntry) -> Self {
        Self {
            tiebreak_reviewer: verdict.tiebreak_reviewer.as_ref(),
            tiebreak_target: verdict.tiebreak_target.as_deref(),
            session: verdict.session.as_deref(),
            event_id: &verdict.event_id,
            origin: &verdict.origin_replica_id,
            created_at: verdict.created_at,
            verdict: verdict.verdict,
            assessment: verdict.assessment.as_ref(),
            author_assumed: verdict.author_assumed(),
            author: verdict.effective_author(),
            model: verdict.model.as_deref(),
            observed: verdict.observed_model.as_deref(),
            judged: verdict.judged_models(),
            extent: None,
            audit: false,
        }
    }
}

/// The assessment-only fallback when no applicable target ladder exists: why
/// a read by `author` at `model` under `assessment` is a findings read
/// under `review_clearance = "assessed"`, or `None` when it clears. A person
/// subject (`Name <email>`) clears on authorship alone; an agent clears only
/// with a recorded `model#effort` and a bound assessment whose `clears` is
/// true.
pub fn clearance_gap(
    author: &str,
    model: Option<&str>,
    assessment: Option<&AuditAssessment>,
) -> Option<FindingsReason> {
    if crate::coordinate::is_person(author) {
        return None;
    }
    let Some(model) = model.filter(|model| !model.trim().is_empty()) else {
        return Some(FindingsReason::ModelUnrecorded);
    };
    let Some(assessment) = assessment else {
        return Some(FindingsReason::NoAssessment);
    };
    match assessment.clears {
        None => return Some(FindingsReason::FirstSchema),
        Some(false) => return Some(FindingsReason::NotClearing(assessment.qualifiers.clone())),
        Some(true) => {}
    }
    match assessment.binding {
        Some(AuditorBinding::Bound) => {}
        binding => return Some(FindingsReason::Unbound(binding)),
    }
    if crate::model::model_effort(model).is_none() {
        return Some(FindingsReason::EffortUnrecorded);
    }
    None
}

#[derive(Debug, Clone)]
pub struct LadderClearance {
    pub gap: Option<FindingsReason>,
}

/// Why an agent's read records findings without clearing, under
/// `review_clearance = "assessed"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingsReason {
    Ladder {
        code: &'static str,
        message: String,
    },
    /// The read records no model for its agent subject.
    ModelUnrecorded,
    /// The read carries no routing assessment.
    NoAssessment,
    /// The assessment is `agent-routing-assessment/1`, which has no
    /// clearance answer.
    FirstSchema,
    /// Routing assessed the read as not clearing, for these reasons.
    NotClearing(Vec<ReviewQualifier>),
    /// The assessment does not describe this reader or its authors.
    Unbound(Option<AuditorBinding>),
    /// The recorded model names no effort, so the assessed effort is not
    /// established.
    EffortUnrecorded,
}

impl FindingsReason {
    pub fn remedy(&self) -> &'static str {
        if matches!(self, Self::Ladder { .. }) {
            "a clearing read under the target review ladder"
        } else {
            CLEARANCE_REMEDY
        }
    }

    pub fn qualifiers(&self) -> Vec<ReviewQualifier> {
        match self {
            Self::NotClearing(qualifiers) => qualifiers.clone(),
            Self::Ladder {
                code: "same-family",
                ..
            } => vec![ReviewQualifier::SameFamily],
            Self::Ladder {
                code: "below-floor",
                ..
            } => vec![ReviewQualifier::BelowFloor],
            Self::Ladder {
                code: "same-model-lower-effort",
                ..
            } => vec![ReviewQualifier::SameModel],
            _ => Vec::new(),
        }
    }

    /// The stable name a projection lists beside the read.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ladder { code, .. } => code,
            Self::ModelUnrecorded => "model-unrecorded",
            Self::NoAssessment => "no-assessment",
            Self::FirstSchema => "assessment-without-clearance",
            Self::NotClearing(_) => "not-clearing",
            Self::Unbound(_) => "unbound",
            Self::EffortUnrecorded => "effort-unrecorded",
        }
    }

    /// Why the read does not clear, phrased to follow "the read".
    pub fn reason(&self) -> String {
        match self {
            Self::Ladder { message, .. } => {
                format!("has a ladder judgment that does not clear: {message}")
            }
            Self::ModelUnrecorded => "records no model for its agent subject".into(),
            Self::NoAssessment => "carries no routing assessment".into(),
            Self::FirstSchema => {
                "carries an agent-routing-assessment/1, which answers no clearance".into()
            }
            Self::NotClearing(qualifiers) if qualifiers.is_empty() => {
                "was assessed as not clearing".into()
            }
            Self::NotClearing(qualifiers) => format!(
                "was assessed as not clearing ({})",
                qualifiers
                    .iter()
                    .map(|qualifier| qualifier.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Unbound(Some(binding)) => format!(
                "carries an assessment not bound to it: {}",
                binding.failure().unwrap_or("unbound")
            ),
            Self::Unbound(None) => "carries an assessment bound to no reader".into(),
            Self::EffortUnrecorded => {
                "records a model without an effort, so the assessed effort is unestablished".into()
            }
        }
    }
}

/// The command that answers whether a reader clears.
pub const CLEARANCE_REMEDY: &str = "agent-routing assess --purpose clearance --json";

/// Why a recorded read leaves review debt owed. A debt is discharged by a
/// read that approves or requests changes, recorded after it was declared, by an identity the ledger
/// does not credit with the work, that routing did not assess `ineligible`;
/// an `ineligible-review` debt also needs routing's eligible, bound
/// assessment on that read, an audit's or a verdict's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DischargeBar {
    /// The read came from a replica this ledger grants no review authority.
    NoReviewAuthority,
    /// The read predates the debt it would answer.
    BeforeDeclaration,
    /// The read judged nothing: `comment-only`.
    CommentOnly,
    /// Routing assessed the reader ineligible, for these reasons.
    AssessedIneligible(Vec<ReviewQualifier>),
    /// The debt is `ineligible-review` and the read carries no eligible,
    /// bound assessment.
    NoEligibleAssessment,
    /// arc assumed the reader's identity; nobody declared it.
    AuthorAssumed,
    /// The reader is this identity the ledger credits with the work.
    Authored(Authorship),
    /// Under `review_clearance = "assessed"`, the read finds but does not
    /// clear.
    FindingsRead(FindingsReason),
    /// The shipped range has no ledger authorship to compare against.
    NoAuthorshipRecord,
    /// The read's extent does not cover the obligation's.
    ExtentUncovered(crate::extent::CoverageGap),
}

impl DischargeBar {
    /// The bar's stable name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoReviewAuthority => "no-review-authority",
            Self::BeforeDeclaration => "before-declaration",
            Self::CommentOnly => "comment-only",
            Self::AssessedIneligible(_) => "assessed-ineligible",
            Self::NoEligibleAssessment => "no-eligible-assessment",
            Self::AuthorAssumed => "author-assumed",
            Self::Authored(_) => "authored",
            Self::FindingsRead(_) => "findings-read",
            Self::NoAuthorshipRecord => "no-authorship-record",
            Self::ExtentUncovered(_) => "extent-uncovered",
        }
    }

    /// Why the read does not discharge, phrased to follow "this read".
    pub fn reason(&self) -> String {
        match self {
            Self::NoReviewAuthority => {
                "comes from a replica this ledger grants no review authority".into()
            }
            Self::BeforeDeclaration => "was recorded before the debt was declared".into(),
            Self::CommentOnly => "is comment-only: a read that neither approves nor requests \
                                  changes discharges no debt"
                .into(),
            Self::AssessedIneligible(qualifiers) if qualifiers.is_empty() => {
                "is not independent: routing assessed this auditor ineligible".into()
            }
            Self::AssessedIneligible(qualifiers) => format!(
                "is not independent: routing assessed this auditor ineligible ({})",
                qualifiers
                    .iter()
                    .map(|qualifier| qualifier.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::NoEligibleAssessment => {
                "carries no eligible assessment bound to the model it recorded".into()
            }
            Self::AuthorAssumed => "is by an identity arc assumed rather than one declared".into(),
            Self::Authored(Authorship {
                identity,
                observed: None,
            }) => format!("is by {identity}, whom the ledger credits with the work"),
            Self::Authored(Authorship {
                identity,
                observed: Some(reader),
            }) => format!(
                "is by {identity}, whom the ledger credits with the work: the reader's session \
                 store observed model {} where the read records {}",
                reader.model,
                reader.declared.as_deref().unwrap_or_default()
            ),
            Self::NoAuthorshipRecord => {
                "cannot be compared with authorship: the shipped range has no ledger record".into()
            }
            Self::ExtentUncovered(gap) => format!(
                "is partial coverage ({}): it {}",
                gap.as_str(),
                gap.reason()
            ),
            Self::FindingsRead(why) if matches!(why, FindingsReason::Ladder { .. }) => format!(
                "is a findings read: it {}; obtain {}",
                why.reason(),
                why.remedy()
            ),
            Self::FindingsRead(why) => format!(
                "is a findings read: it {}; review_clearance = \"assessed\" clears only a bound \
                 assessment from `{CLEARANCE_REMEDY}` that says it clears",
                why.reason()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn review_projection_preserves_sessions_in_every_coverage_path() {
        let patch = ev(
            "fix",
            serde_json::from_value(serde_json::json!({
                "event_type": "patchset-added", "patchset_id": "ps-01", "base": "b0", "head": "h1"
            }))
            .unwrap(),
        );
        let debt = ev(
            "fix",
            Payload::DebtDeclared {
                reason: "review owed".into(),
                patchset_id: Some("ps-01".into()),
                missing: DebtMissing::IndependentReview,
                coverage: Vec::new(),
                production: Some(test_production()),
                ineligible: None,
                replayed: None,
                extent: None,
            },
        );
        for audit in [false, true] {
            let mut review = if audit {
                ev(
                    "fix",
                    Payload::AuditVerdictRecorded {
                        basis: Default::default(),
                        tiebreak_reviewer: None,
                        tiebreak_target: None,
                        dispositions: Vec::new(),
                        receipt: None,
                        revision: "h1".into(),
                        verdict: Verdict::Approved,
                        body: None,
                        findings: vec![],
                        route_version: None,
                        assessment: None,
                        held_roles: Vec::new(),
                        extent: None,
                    },
                )
            } else {
                verdict("fix", "ps-01", None)
            };
            review.actor = "reader".into();
            review.session = Some("fixture-recording".into());
            review.model = Some("fixture-recording#high".into());
            let integrated = ev(
                "fix",
                Payload::ChangeIntegrated {
                    authority_grant_event_id: None,
                    provisional_resolutions: Vec::new(),
                    integrated_commit: "h1".into(),
                    source_patchset_id: "ps-01".into(),
                    source_head: "h1".into(),
                    target_branch: "master".into(),
                    target_before: "b0".into(),
                    authorization: None,
                    already_contained: false,
                    shape: None,
                    started_at: None,
                    landing: None,
                },
            );
            let state = reduce(&[
                opened("fix"),
                patch.clone(),
                integrated,
                debt.clone(),
                review,
            ])
            .unwrap();
            if !audit {
                let coverage = state.debt_coverage_for(&state.patchsets[0]);
                assert_eq!(
                    serde_json::to_value(&coverage[0]).unwrap()["model_recorded"],
                    false
                );
                assert_eq!(
                    serde_json::to_value(
                        state
                            .debt_discharge(state.debt.as_ref().unwrap())
                            .unwrap()
                            .1
                    )
                    .unwrap()["model_recorded"],
                    false
                );
            } else {
                assert_eq!(
                    serde_json::to_value(
                        state
                            .debt_discharge(state.debt.as_ref().unwrap())
                            .unwrap()
                            .1
                    )
                    .unwrap()["model_recorded"],
                    false
                );
            }
        }
    }

    /// A declaration written before coverage carried the session: the
    /// recording key stands where a model would, and only the verdict it was
    /// captured from can say so.
    fn legacy_declaration(coverage: serde_json::Value) -> Event {
        let json = serde_json::json!({
            "event_type": "debt-declared",
            "reason": "review owed",
            "patchset_id": "ps-01",
            "missing": "independent-review",
            "coverage": coverage,
        });
        let payload: Payload = serde_json::from_value(json).unwrap();
        ev("fix", payload)
    }

    fn reviewed(session: &str, model: &str, actor: &str) -> Event {
        let mut review = verdict("fix", "ps-01", None);
        review.actor = actor.into();
        review.session = Some(session.into());
        review.model = Some(model.into());
        review
    }

    fn declared_coverage(state: &ChangeState) -> Vec<serde_json::Value> {
        state
            .debt
            .as_ref()
            .unwrap()
            .coverage
            .as_ref()
            .unwrap()
            .iter()
            .map(|entry| serde_json::to_value(entry).unwrap())
            .collect()
    }

    #[test]
    fn legacy_coverage_takes_its_session_from_the_verdict_it_was_captured_from() {
        let patch = ev(
            "fix",
            serde_json::from_value(serde_json::json!({
                "event_type": "patchset-added", "patchset_id": "ps-01", "base": "b0", "head": "h1"
            }))
            .unwrap(),
        );
        let events = [
            opened("fix"),
            patch,
            reviewed("recording-key", "recording-key#high", "reader"),
            reviewed("session-b", "gpt-5.6-luna#max", "reader"),
            legacy_declaration(serde_json::json!([
                {"reviewer": "reader", "model": "recording-key#high", "effort": "high"},
                {"reviewer": "reader", "model": "gpt-5.6-luna#max", "effort": "max"},
            ])),
        ];
        let state = reduce(&events).unwrap();
        let coverage = declared_coverage(&state);
        assert_eq!(coverage[0]["session"], "recording-key");
        assert_eq!(coverage[0]["model_recorded"], false, "{coverage:?}");
        assert_eq!(coverage[1]["session"], "session-b");
        assert!(coverage[1].get("model_recorded").is_none(), "{coverage:?}");
    }

    #[test]
    fn legacy_coverage_without_a_determinable_session_stays_as_recorded() {
        let patch = ev(
            "fix",
            serde_json::from_value(serde_json::json!({
                "event_type": "patchset-added", "patchset_id": "ps-01", "base": "b0", "head": "h1"
            }))
            .unwrap(),
        );
        // Two sessions reviewed as one identity, and the declaration kept
        // only one of them, so neither position nor identity settles it.
        let events = [
            opened("fix"),
            patch,
            reviewed("session-a", "gpt-5.6-luna#max", "reader"),
            reviewed("session-b", "gpt-5.6-luna#max", "reader"),
            legacy_declaration(serde_json::json!([
                {"reviewer": "reader", "model": "gpt-5.6-luna#max", "effort": "max"},
            ])),
        ];
        let coverage = declared_coverage(&reduce(&events).unwrap());
        assert!(coverage[0].get("session").is_none(), "{coverage:?}");
    }

    #[test]
    fn the_applicable_history_decides_whether_a_finding_is_released() {
        use DispositionStatus::{AcceptedRisk, Disputed, Obsolete, Resolved, StillOpen};
        let entry = |event_id: &str, status, supersedes: &[&str]| DispositionEntry {
            origin_replica_id: "repo".into(),
            gating_eligible: true,
            event_id: event_id.into(),
            status,
            commit: None,
            evidence: None,
            evidence_event_id: None,
            actor: "tester".into(),
            supersedes: supersedes.iter().map(|id| id.to_string()).collect(),
        };
        let finding = |dispositions, after_integration| FindingState {
            forge_sources: Vec::new(),
            id: "f1".into(),
            blocking: false,
            severity: Severity::Minor,
            summary: "a finding".into(),
            body: None,
            patchset_id: None,
            anchor: None,
            origin_event: "e0".into(),
            reported_by: "tester".into(),
            on_behalf_of: None,
            actor_source: None,
            dispositions,
            after_integration,
            replies: Vec::new(),
        };
        let contested = || vec![entry("e1", Resolved, &[]), entry("e2", Obsolete, &[])];
        // (own dispositions, later history, released by its own, released now)
        let cases = [
            (vec![], vec![], false, false),
            (vec![entry("e1", StillOpen, &[])], vec![], false, false),
            (vec![entry("e1", Disputed, &[])], vec![], false, false),
            (contested(), vec![], false, false),
            (vec![entry("e1", AcceptedRisk, &[])], vec![], true, true),
            (
                vec![entry("e1", Resolved, &[]), entry("e2", StillOpen, &["e1"])],
                vec![],
                false,
                false,
            ),
            (vec![], vec![entry("l1", StillOpen, &[])], false, false),
            (
                vec![entry("e1", Disputed, &[])],
                vec![entry("l1", Disputed, &[])],
                false,
                false,
            ),
            (
                vec![],
                vec![entry("l1", Resolved, &[]), entry("l2", Obsolete, &[])],
                false,
                false,
            ),
            (
                vec![entry("e1", StillOpen, &[])],
                vec![entry("l1", Resolved, &[])],
                false,
                true,
            ),
            (contested(), vec![entry("l1", Obsolete, &[])], false, true),
        ];
        for (index, (own, later, released, currently_released)) in cases.into_iter().enumerate() {
            let finding = finding(own, later);
            assert_eq!(finding.released(), released, "case {index}");
            assert_eq!(
                finding.currently_released(),
                currently_released,
                "case {index}"
            );
        }
    }

    fn ev(change: &str, payload: Payload) -> Event {
        Event {
            schema_version: SCHEMA_VERSION,
            event_id: crate::ids::new_event_id().unwrap(),
            repository_id: "repo".into(),
            change_id: change.into(),
            actor: "tester".into(),
            actor_source: Some(ActorSource::Flag),
            operator: None,
            on_behalf_of: None,
            model: None,
            model_provenance: Default::default(),
            harness: None,
            session: None,
            session_link: None,
            session_resolution: None,
            session_resolution_reason: None,
            created_at: Utc::now(),
            payload,
        }
    }

    fn opened(change: &str) -> Event {
        ev(
            change,
            Payload::ChangeOpened {
                dangerous: false,
                slug: "fix".into(),
                title: "Fix".into(),
                profile: "local".into(),
                target_branch: "master".into(),
                branch: "arc/fix".into(),
                base: "b0".into(),
                worktree: None,
                worktree_created: false,
                worktree_owner_token: None,
                blocked_by: Vec::new(),
                tags: Vec::new(),
                journal_ref: None,
                journal_ref_digest: None,
                from_fork: None,
            },
        )
    }

    /// Claim: an integration event replays to a guarded closure with its
    /// recorded shape, and one recorded before shapes existed derives merge, or
    /// already-contained when it says so. Rejects old integration events
    /// replaying to another shape or none.
    #[test]
    fn guarded_integration_shapes_reduce_and_legacy_events_derive_shape() {
        use crate::model::IntegrationShape::{AlreadyContained, FastForward, Merge};
        for (shape, contained, expected) in [
            (Some(FastForward), false, FastForward),
            (Some(Merge), false, Merge),
            (Some(AlreadyContained), true, AlreadyContained),
            (None, false, Merge),
            (None, true, AlreadyContained),
        ] {
            let integrated = ev(
                "fix",
                Payload::ChangeIntegrated {
                    authority_grant_event_id: None,
                    provisional_resolutions: Vec::new(),
                    integrated_commit: "head".into(),
                    source_patchset_id: "ps-01".into(),
                    source_head: "head".into(),
                    target_branch: "master".into(),
                    target_before: "base".into(),
                    authorization: None,
                    already_contained: contained,
                    shape,
                    started_at: None,
                    landing: None,
                },
            );
            let json = serde_json::to_value(&integrated).unwrap();
            if shape.is_none() {
                assert!(json.get("shape").is_none());
            }
            let parsed = serde_json::from_value(json).unwrap();
            let state = reduce(&[opened("fix"), parsed]).unwrap();
            let closure = state.closure.unwrap();
            assert_eq!(closure.shape, Some(expected));
            assert_eq!(closure.integration, Some(IntegrationKind::Guarded));
            assert_eq!(closure.integrated_commit, closure.source_head);
        }
    }

    fn post_integration_receipt(integrated_commit: &str) -> Event {
        ev(
            "fix",
            Payload::PostIntegrationVerificationRecorded {
                gate: Some("unit".into()),
                command: "true".into(),
                revision: integrated_commit.into(),
                tree: "t1".into(),
                integrated_commit: integrated_commit.into(),
                result: VerifyResult::Pass,
                timeout_seconds: None,
                hostname: "ci".into(),
                runner: "job".into(),
                environment: None,
                note: None,
            },
        )
    }

    fn integrated_at(commit: &str) -> Event {
        ev(
            "fix",
            Payload::ChangeIntegrated {
                authority_grant_event_id: None,
                provisional_resolutions: Vec::new(),
                integrated_commit: commit.into(),
                source_patchset_id: "ps-01".into(),
                source_head: commit.into(),
                target_branch: "master".into(),
                target_before: "base".into(),
                authorization: None,
                already_contained: false,
                shape: None,
                started_at: None,
                landing: None,
            },
        )
    }

    /// Claim: replay keeps a receipt to the integrated change it verifies.
    /// Rejects: an imported receipt recorded before integration, or naming a
    /// commit the change did not integrate, reading as verification of what
    /// shipped. It is also never gate evidence.
    #[test]
    fn a_post_integration_receipt_replays_only_against_what_shipped() {
        let state = reduce(&[
            opened("fix"),
            integrated_at("head"),
            post_integration_receipt("head"),
        ])
        .unwrap();
        assert_eq!(state.post_integration_verifications.len(), 1);
        assert!(state.post_integration_verifications[0].after_integration);
        assert!(state.verifications.is_empty());

        let early = reduce(&[opened("fix"), post_integration_receipt("head")]).unwrap_err();
        assert!(
            format!("{early:#}").contains("comes before change fix integrated"),
            "{early:#}"
        );
        let elsewhere = reduce(&[
            opened("fix"),
            integrated_at("head"),
            post_integration_receipt("other"),
        ])
        .unwrap_err();
        assert!(
            format!("{elsewhere:#}").contains("names other, but change fix integrated head"),
            "{elsewhere:#}"
        );
    }

    fn verdict(change: &str, patchset: &str, relation: Option<VerdictRelation>) -> Event {
        ev(
            change,
            Payload::VerdictRecorded {
                basis: Default::default(),
                tiebreak_reviewer: None,
                tiebreak_target: None,
                dispositions: Vec::new(),
                receipt: None,
                forge_frontier: None,
                forge_sources: Vec::new(),
                patchset_id: patchset.into(),
                verdict: Verdict::Approved,
                authority_observed: None,
                causes: Vec::new(),
                body: None,
                findings: Vec::new(),
                relation,
                provisional: None,
                route_version: None,
                assessment: None,
            },
        )
    }

    /// A corroboration supports the verdict it observed without becoming a
    /// second authority — which is what discharging a provisional approval is,
    /// and why a bare `supersedes` could not express it.
    #[test]
    fn a_corroborating_verdict_leaves_the_observed_one_authoritative() {
        let first = verdict("fix", "ps-01", None);
        let first_id = first.event_id.clone();
        let second = verdict(
            "fix",
            "ps-01",
            Some(VerdictRelation::Corroborates {
                observed: vec![first_id.clone()],
            }),
        );
        let state = reduce(&[opened("fix"), first, second]).unwrap();
        assert!(!state.verdict_contested());
        assert_eq!(state.latest_verdict().unwrap().event_id, first_id);
    }

    #[test]
    fn a_superseding_verdict_replaces_the_observed_one() {
        let first = verdict("fix", "ps-01", None);
        let first_id = first.event_id.clone();
        let second = verdict(
            "fix",
            "ps-01",
            Some(VerdictRelation::Supersedes {
                observed: vec![first_id],
            }),
        );
        let second_id = second.event_id.clone();
        let state = reduce(&[opened("fix"), first, second]).unwrap();
        assert!(!state.verdict_contested());
        assert_eq!(state.latest_verdict().unwrap().event_id, second_id);
    }

    /// Two verdicts replacing the same tip fork the chain, exactly as two
    /// dispositions superseding one tip do. Neither wins by arriving later:
    /// the change is contested until a verdict supersedes both.
    #[test]
    fn verdicts_replacing_one_tip_leave_the_change_contested() {
        let first = verdict("fix", "ps-01", None);
        let first_id = first.event_id.clone();
        let fork_a = verdict(
            "fix",
            "ps-01",
            Some(VerdictRelation::Supersedes {
                observed: vec![first_id.clone()],
            }),
        );
        let fork_b = verdict(
            "fix",
            "ps-01",
            Some(VerdictRelation::Supersedes {
                observed: vec![first_id],
            }),
        );
        let (a_id, b_id) = (fork_a.event_id.clone(), fork_b.event_id.clone());
        let state = reduce(&[opened("fix"), first, fork_a, fork_b]).unwrap();
        assert!(state.verdict_contested());
        assert!(state.latest_verdict().is_none());
        let tips: Vec<&str> = state
            .verdict_tips()
            .iter()
            .map(|tip| tip.event_id.as_str())
            .collect();
        assert_eq!(tips.len(), 2);
        assert!(tips.contains(&a_id.as_str()));
        assert!(tips.contains(&b_id.as_str()));
    }

    /// Every verdict recorded before the relation existed carries none, and a
    /// ledger of them must keep reading exactly as it did: latest wins.
    #[test]
    fn relationless_verdicts_still_resolve_to_the_latest() {
        let first = verdict("fix", "ps-01", None);
        let second = verdict("fix", "ps-01", None);
        let second_id = second.event_id.clone();
        let state = reduce(&[opened("fix"), first, second]).unwrap();
        assert!(!state.verdict_contested());
        assert_eq!(state.latest_verdict().unwrap().event_id, second_id);
    }

    #[test]
    fn a_changelog_entry_records_the_entry_it_replaced() {
        let first = ev(
            "fix",
            Payload::ChangelogRecorded {
                category: "added".into(),
                body: "first".into(),
                supersedes: None,
                no_entry_reason: None,
            },
        );
        let first_id = first.event_id.clone();
        let second = ev(
            "fix",
            Payload::ChangelogRecorded {
                category: "added".into(),
                body: "second".into(),
                supersedes: Some(first_id.clone()),
                no_entry_reason: None,
            },
        );
        let state = reduce(&[opened("fix"), first, second]).unwrap();
        let entry = state.changelog.unwrap();
        assert_eq!(entry.body, "second");
        assert_eq!(entry.supersedes.as_deref(), Some(first_id.as_str()));
    }

    /// A verdict and a changelog entry recorded before either edge existed
    /// must deserialize and replay unchanged.
    #[test]
    fn events_without_the_supersession_edges_replay_unchanged() {
        let verdict_event: Event = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "event_id": "event-verdict-old",
            "repository_id": "repo",
            "change_id": "fix",
            "actor": "tester",
            "created_at": "2026-07-16T00:00:00Z",
            "event_type": "verdict-recorded",
            "patchset_id": "ps-01",
            "verdict": "approved"
        }))
        .unwrap();
        let changelog_event: Event = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "event_id": "event-changelog-old",
            "repository_id": "repo",
            "change_id": "fix",
            "actor": "tester",
            "created_at": "2026-07-16T00:00:00Z",
            "event_type": "changelog-recorded",
            "category": "added",
            "body": "old"
        }))
        .unwrap();
        let state = reduce(&[opened("fix"), verdict_event, changelog_event]).unwrap();
        assert_eq!(
            state.latest_verdict().unwrap().event_id,
            "event-verdict-old"
        );
        assert!(state.latest_verdict().unwrap().relation.is_none());
        let entry = state.changelog.unwrap();
        assert_eq!(entry.body, "old");
        assert!(entry.supersedes.is_none());
    }

    /// Claim: a schema-1 change-opened event carrying no blockers or tags
    /// replays with both empty. Rejects an old opening that no longer reduces.
    #[test]
    fn old_opening_events_default_new_metadata() {
        let event: Event = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "event_id": "event-old",
            "repository_id": "repo",
            "change_id": "fix-old",
            "actor": "tester",
            "created_at": "2026-07-16T00:00:00Z",
            "event_type": "change-opened",
            "slug": "fix",
            "title": "Fix",
            "profile": "local",
            "target_branch": "master",
            "branch": "arc/fix",
            "base": "base"
        }))
        .unwrap();
        let state = reduce(&[event]).unwrap();
        assert!(state.blocked_by.is_empty());
        assert!(state.tags.is_empty());
    }

    #[test]
    fn contested_dispositions_block() {
        let change = "fix-abc123";
        let mut events = vec![opened(change)];
        events.push(ev(
            change,
            Payload::FindingAdded {
                forge_sources: Vec::new(),
                finding_id: "f1".into(),
                blocking: true,
                severity: Severity::Major,
                summary: "bad".into(),
                body: None,
                patchset_id: None,
                anchor: None,
            },
        ));
        // Two dispositions forked from the empty tip set: contested.
        events.push(ev(
            change,
            Payload::DispositionRecorded {
                finding_id: "f1".into(),
                status: DispositionStatus::Resolved,
                commit: None,
                evidence: None,
                evidence_event_id: None,
                supersedes: vec![],
            },
        ));
        events.push(ev(
            change,
            Payload::DispositionRecorded {
                finding_id: "f1".into(),
                status: DispositionStatus::StillOpen,
                commit: None,
                evidence: None,
                evidence_event_id: None,
                supersedes: vec![],
            },
        ));
        let state = reduce(&events).unwrap();
        let f = &state.findings["f1"];
        assert!(f.contested());
        assert!(f.blocks_integration());

        // A later disposition superseding both tips settles it.
        let tips: Vec<String> = f.tips().iter().map(|t| t.event_id.clone()).collect();
        let mut events2 = events.clone();
        events2.push(ev(
            change,
            Payload::DispositionRecorded {
                finding_id: "f1".into(),
                status: DispositionStatus::Resolved,
                commit: Some("c9".into()),
                evidence: None,
                evidence_event_id: None,
                supersedes: tips,
            },
        ));
        let state2 = reduce(&events2).unwrap();
        let f2 = &state2.findings["f1"];
        assert!(!f2.contested());
        assert!(!f2.blocks_integration());
    }

    /// Two collaborators hold independently: releasing one leaves the other
    /// in place, and only a release that names no hold — which is how every
    /// event written before holds had identity looks — clears everything.
    #[test]
    fn holds_are_independent_and_a_legacy_release_still_clears_all() {
        let change = "fix-abc123";
        let mut events = vec![opened(change)];
        events.push(ev(
            change,
            Payload::HoldSet {
                reason: "reviewer waiting on the user".into(),
            },
        ));
        events.push(ev(
            change,
            Payload::HoldSet {
                reason: "release manager waiting on a dependency".into(),
            },
        ));
        let state = reduce(&events).unwrap();
        assert_eq!(state.holds.len(), 2);
        // The identity is the setting event's own ID, not a key the reducer
        // invented: that is what makes it nameable from outside the reducer.
        let first = events[1].event_id.clone();
        let second = events[2].event_id.clone();
        assert!(state.holds.contains_key(&first));
        assert!(state.holds.contains_key(&second));

        events.push(ev(
            change,
            Payload::HoldReleased {
                hold_event_id: Some(first.clone()),
                reason: None,
            },
        ));
        let state = reduce(&events).unwrap();
        assert_eq!(state.holds.len(), 1);
        assert!(!state.holds.contains_key(&first));
        assert!(state.holds.contains_key(&second));

        // Releasing the same hold twice — two collaborators reaching the same
        // conclusion — reduces, and leaves the other hold alone.
        let mut again = events.clone();
        again.push(ev(
            change,
            Payload::HoldReleased {
                hold_event_id: Some(first.clone()),
                reason: None,
            },
        ));
        let state = reduce(&again).unwrap();
        assert_eq!(state.holds.len(), 1);
        assert!(state.holds.contains_key(&second));

        events.push(ev(
            change,
            Payload::HoldReleased {
                hold_event_id: None,
                reason: None,
            },
        ));
        assert!(reduce(&events).unwrap().holds.is_empty());
    }

    /// Claim: a recorded reuse of passing evidence from a dirty, moved tree
    /// still replays to a complete run, by revision or by tree, while a reuse
    /// whose tree, gate or result disagrees with its evidence fails replay.
    /// Rejects history that no longer replays, and a reuse standing in for
    /// evidence it does not match.
    #[test]
    fn legacy_reuse_of_non_green_passing_evidence_still_replays() {
        let change = "legacy-reuse";
        let mut events = vec![opened(change)];
        let run = ev(
            change,
            Payload::VerificationRunStarted {
                profile: None,
                event: crate::model::GateEvent::Integration,
                revision: "head".into(),
                mode: VerificationRunMode::Sequential,
                skip_green: true,
                gates: vec![VerificationRunGate {
                    name: "unit".into(),
                    command: "true".into(),
                    timeout_seconds: None,
                }],
            },
        );
        let run_id = run.event_id.clone();
        events.push(run);
        let evidence = ev(
            change,
            Payload::VerificationRecorded {
                declaration: None,
                timeout_seconds: None,
                tree: None,
                against_target: None,
                landing: None,
                gate: Some("unit".into()),
                command: "true".into(),
                revision: "head".into(),
                result: VerifyResult::Pass,
                exit_code: Some(0),
                duration_ms: Some(1),
                timing: Default::default(),
                output_tail: None,
                output_artifact: None,
                timed_out: false,
                hostname: "test".into(),
                attested: false,
                run_id: Some(run_id),
                probe: None,
                runner: None,
                environment: None,
                note: None,
                falsification: None,
                falsification_inferred: None,
                tested_tree: Some("dirty-tree".into()),
                worktree_dirty: Some(true),
                worktree_dirty_tracked: None,
                worktree_dirty_untracked: None,
                tree_moved: true,
            },
        );
        let evidence_event_id = evidence.event_id.clone();
        events.push(evidence);
        let reuse_run = ev(
            change,
            Payload::VerificationRunStarted {
                profile: None,
                event: crate::model::GateEvent::Integration,
                revision: "head".into(),
                mode: VerificationRunMode::Sequential,
                skip_green: true,
                gates: vec![VerificationRunGate {
                    name: "unit".into(),
                    command: "true".into(),
                    timeout_seconds: None,
                }],
            },
        );
        let reuse_run_id = reuse_run.event_id.clone();
        events.push(reuse_run);
        events.push(ev(
            change,
            Payload::VerificationReused {
                delta: None,
                run_id: reuse_run_id,
                gate: "unit".into(),
                revision: "head".into(),
                tree: None,
                evidence_event_id,
            },
        ));

        let state = reduce(&events).unwrap();
        assert!(state.verification_runs.last().unwrap().complete);

        let mut tree_keyed = events.clone();
        if let Payload::VerificationRecorded {
            tree, tested_tree, ..
        } = &mut tree_keyed[2].payload
        {
            *tree = Some("content".into());
            *tested_tree = Some("dirty-content".into());
        }
        if let Payload::VerificationRunStarted { revision, .. } = &mut tree_keyed[3].payload {
            *revision = "merge".into();
        }
        if let Payload::VerificationReused { revision, tree, .. } = &mut tree_keyed[4].payload {
            *revision = "merge".into();
            *tree = Some("content".into());
        }
        assert!(
            reduce(&tree_keyed)
                .unwrap()
                .verification_runs
                .last()
                .unwrap()
                .complete
        );
        for mismatch in ["tree", "gate", "result"] {
            let mut invalid = tree_keyed.clone();
            match mismatch {
                "tree" => {
                    if let Payload::VerificationReused { tree, .. } = &mut invalid[4].payload {
                        *tree = Some("other".into());
                    }
                }
                "gate" => {
                    if let Payload::VerificationReused { gate, .. } = &mut invalid[4].payload {
                        *gate = "other".into();
                    }
                }
                "result" => {
                    if let Payload::VerificationRecorded { result, .. } = &mut invalid[2].payload {
                        *result = VerifyResult::Fail;
                    }
                }
                _ => unreachable!(),
            }
            let error = reduce(&invalid).unwrap_err().to_string();
            assert!(
                error.contains("does not match passing"),
                "{mismatch}: {error}"
            );
        }
        if let Payload::VerificationRecorded {
            tree, tested_tree, ..
        } = &mut tree_keyed[2].payload
        {
            *tree = None;
            *tested_tree = Some("content".into());
        }
        assert!(reduce(&tree_keyed).is_ok());
    }

    #[test]
    fn claim_timing_distinguishes_stale_expired_and_unbudgeted_stages() {
        let claimed_at = Utc.with_ymd_and_hms(2026, 7, 17, 12, 0, 0).unwrap();
        let budgets = [
            (StageBudget::Launch, 60),
            (StageBudget::Started, 300),
            (StageBudget::SpecRead, 120),
            (StageBudget::Implementing, 1_800),
            (StageBudget::Verifying, 900),
        ]
        .into_iter()
        .collect();
        let mut claim = ClaimState {
            claim_id: "claim-1".into(),
            owner: ClaimIdentity {
                actor: "executor".into(),
                harness: "codex".into(),
                session: "session".into(),
            },
            ttl_seconds: 7_200,
            stage_budgets: budgets,
            claimed_at,
            last_activity_at: claimed_at,
            progress: None,
            displaced: None,
        };

        let launch = claim_timing_at(&claim, claimed_at + TimeDelta::seconds(61));
        assert!(launch.active);
        assert!(launch.stale);
        assert_eq!(launch.stage, "launch");
        assert_eq!(launch.age_seconds, 61);
        assert_eq!(launch.budget_seconds, Some(60));

        claim.progress = Some(StageProgress {
            stage: ClaimStage::Implementing,
            note: None,
            blocker: None,
            changed_at: claimed_at + TimeDelta::seconds(10),
        });
        claim.last_activity_at = claimed_at + TimeDelta::seconds(1_000);
        let implementing = claim_timing_at(&claim, claimed_at + TimeDelta::seconds(1_811));
        assert!(implementing.active);
        assert!(implementing.stale);
        assert_eq!(implementing.age_seconds, 1_801);

        claim.progress = Some(StageProgress {
            stage: ClaimStage::BlockedOn,
            note: Some("waiting".into()),
            blocker: None,
            changed_at: claimed_at,
        });
        let blocked = claim_timing_at(&claim, claimed_at + TimeDelta::seconds(5_000));
        assert!(blocked.active);
        assert!(!blocked.stale);
        assert_eq!(blocked.budget_seconds, None);

        let expired = claim_timing_at(&claim, claimed_at + TimeDelta::seconds(8_201));
        assert!(expired.expired);
        assert!(!expired.active);
        assert!(!expired.stale);
    }

    /// The events of a change holding one patchset, with `debt` declared on
    /// it, integrated or left open, and `reviews` recorded after.
    fn change_with_debt(debt: Event, integrated: bool, reviews: Vec<Event>) -> ChangeState {
        let patch = ev(
            "fix",
            serde_json::from_value(serde_json::json!({
                "event_type": "patchset-added", "patchset_id": "ps-01", "base": "b0", "head": "h1"
            }))
            .unwrap(),
        );
        let mut events = vec![opened("fix"), patch, debt];
        if integrated {
            events.push(ev(
                "fix",
                Payload::ChangeIntegrated {
                    authority_grant_event_id: None,
                    provisional_resolutions: Vec::new(),
                    integrated_commit: "h1".into(),
                    source_patchset_id: "ps-01".into(),
                    source_head: "h1".into(),
                    target_branch: "master".into(),
                    target_before: "b0".into(),
                    authorization: None,
                    already_contained: false,
                    shape: None,
                    started_at: None,
                    landing: None,
                },
            ));
        }
        events.extend(reviews);
        reduce(&events).unwrap()
    }

    fn independent_audit(assessment: Option<AuditAssessment>) -> Event {
        let mut audit = ev(
            "fix",
            Payload::AuditVerdictRecorded {
                basis: Default::default(),
                tiebreak_reviewer: None,
                tiebreak_target: None,
                dispositions: Vec::new(),
                receipt: None,
                revision: "h1".into(),
                verdict: Verdict::Approved,
                body: None,
                findings: vec![],
                route_version: None,
                assessment,
                held_roles: Vec::new(),
                extent: None,
            },
        );
        audit.actor = "reader".into();
        audit
    }

    fn test_production() -> DebtProduction {
        DebtProduction {
            planner: None,
            brief_version: None,
            implementer: DebtIdentity::new(
                "author".into(),
                None,
                Some("fixture".into()),
                Some("author#high".into()),
                None,
            ),
            orchestrator: None,
            integrator: None,
            trailer_implementers: Some(Vec::new()),
            following_brief: false,
        }
    }

    fn versioned_debt() -> Event {
        ev(
            "fix",
            Payload::DebtDeclared {
                reason: "review owed".into(),
                patchset_id: Some("ps-01".into()),
                missing: DebtMissing::IndependentReview,
                coverage: Vec::new(),
                production: Some(test_production()),
                ineligible: None,
                replayed: None,
                extent: None,
            },
        )
    }

    fn legacy_debt() -> Event {
        ev(
            "fix",
            Payload::AuditDebtDeclared {
                reason: "review owed".into(),
                patchset_id: Some("ps-01".into()),
            },
        )
    }

    fn standing_of(state: &ChangeState) -> Option<DebtStanding> {
        state.debt_standing()
    }

    #[test]
    fn a_debt_is_declared_until_the_change_integrates_then_owed_until_discharged() {
        let open = change_with_debt(versioned_debt(), false, vec![]);
        assert_eq!(standing_of(&open), Some(DebtStanding::Declared));
        assert!(!open.debt_outstanding());
        assert_eq!(
            open.debt.as_ref().map(|debt| debt.standing),
            Some(DebtStanding::Declared)
        );

        let owed = change_with_debt(versioned_debt(), true, vec![]);
        assert_eq!(standing_of(&owed), Some(DebtStanding::Owed));
        assert!(owed.debt_outstanding());

        let discharged = change_with_debt(versioned_debt(), true, vec![independent_audit(None)]);
        assert_eq!(standing_of(&discharged), Some(DebtStanding::Discharged));
        assert!(!discharged.debt_outstanding());
        assert_eq!(
            discharged.debt.as_ref().map(|debt| debt.standing),
            Some(DebtStanding::Discharged)
        );
    }

    #[test]
    fn a_change_with_no_debt_has_no_standing() {
        let patch = ev(
            "fix",
            serde_json::from_value(serde_json::json!({
                "event_type": "patchset-added", "patchset_id": "ps-01", "base": "b0", "head": "h1"
            }))
            .unwrap(),
        );
        let state = reduce(&[opened("fix"), patch]).unwrap();
        assert_eq!(standing_of(&state), None);
    }

    #[test]
    fn a_closed_change_that_did_not_integrate_never_owes_or_discharges() {
        let abandoned = ev(
            "fix",
            Payload::ChangeClosed {
                outcome: Closure::Abandoned,
                integrated_commit: None,
                superseded_by: None,
            },
        );
        let state = change_with_debt(legacy_debt(), false, vec![abandoned]);
        assert_eq!(standing_of(&state), Some(DebtStanding::Declared));
        assert!(!state.debt_outstanding());
    }

    #[test]
    fn a_verdict_never_clears_an_ineligible_review_debt() {
        let declared = ev(
            "fix",
            Payload::DebtDeclared {
                reason: "same-family read only".into(),
                patchset_id: Some("ps-01".into()),
                missing: DebtMissing::IneligibleReview,
                coverage: Vec::new(),
                production: None,
                ineligible: Some(IneligibleRead {
                    read: "01READ".into(),
                    qualifiers: vec![ReviewQualifier::SameFamily],
                    findings: None,
                    route_version: None,
                    assessment: AssessmentBasis::Declared,
                }),
                replayed: None,
                extent: None,
            },
        );
        let mut review = verdict("fix", "ps-01", None);
        review.actor = "reader".into();
        let owed = change_with_debt(declared.clone(), true, vec![review]);
        assert_eq!(standing_of(&owed), Some(DebtStanding::Owed));
        assert!(owed.debt_outstanding());

        let plain_audit = change_with_debt(declared.clone(), true, vec![independent_audit(None)]);
        assert_eq!(standing_of(&plain_audit), Some(DebtStanding::Owed));

        let open = change_with_debt(declared, false, vec![]);
        assert_eq!(standing_of(&open), Some(DebtStanding::Declared));
    }

    /// Rejects a verdict on the replayed patchset discharging a
    /// `replayed-unread` debt: it read the edit, and the debt owes a read of
    /// the context the edit landed in, which an audit of the integrated
    /// revision gives.
    #[test]
    fn only_an_audit_of_what_landed_clears_a_replayed_unread_debt() {
        let declared = ev(
            "fix",
            Payload::DebtDeclared {
                reason: "replayed onto a later base".into(),
                patchset_id: Some("ps-01".into()),
                missing: DebtMissing::ReplayedUnread,
                coverage: Vec::new(),
                production: Some(test_production()),
                ineligible: None,
                replayed: Some(crate::model::ReplayedRead {
                    approval: "01APPROVAL".into(),
                    base: "b0".into(),
                    parent: "p1".into(),
                }),
                extent: None,
            },
        );
        let mut review = verdict("fix", "ps-01", None);
        review.actor = "reader".into();
        let reviewed = change_with_debt(declared.clone(), true, vec![review]);
        assert_eq!(standing_of(&reviewed), Some(DebtStanding::Owed));
        assert!(reviewed.debt.as_ref().unwrap().findings_reads.is_empty());

        let audited = change_with_debt(declared, true, vec![independent_audit(None)]);
        assert_eq!(standing_of(&audited), Some(DebtStanding::Discharged));
        assert_eq!(
            audited
                .debt
                .as_ref()
                .unwrap()
                .replayed
                .as_ref()
                .unwrap()
                .approval,
            "01APPROVAL"
        );
    }

    fn with_verdict(mut event: Event, outcome: Verdict) -> Event {
        match &mut event.payload {
            Payload::AuditVerdictRecorded { verdict, .. }
            | Payload::VerdictRecorded { verdict, .. } => *verdict = outcome,
            _ => unreachable!("not a read"),
        }
        event
    }

    fn independent_verdict(outcome: Verdict) -> Event {
        let mut review = with_verdict(verdict("fix", "ps-01", None), outcome);
        review.actor = "reader".into();
        review
    }

    /// Rejects a `comment-only` read, which judged nothing, settling the debt
    /// it was supposed to answer, and a review that requested changes leaving
    /// it owed: from an audit and from a verdict alike.
    #[test]
    fn a_read_that_judges_the_work_discharges_review_debt() {
        let audit = with_verdict(independent_audit(None), Verdict::CommentOnly);
        let state = change_with_debt(versioned_debt(), true, vec![audit]);
        assert_eq!(standing_of(&state), Some(DebtStanding::Owed));
        assert_eq!(state.audit_verdicts.len(), 1, "the read stays recorded");
        let state = change_with_debt(
            versioned_debt(),
            true,
            vec![independent_verdict(Verdict::CommentOnly)],
        );
        assert_eq!(standing_of(&state), Some(DebtStanding::Owed));
        for outcome in [Verdict::Approved, Verdict::ChangesRequested] {
            let audited = change_with_debt(
                versioned_debt(),
                true,
                vec![with_verdict(independent_audit(None), outcome)],
            );
            assert_eq!(
                standing_of(&audited),
                Some(DebtStanding::Discharged),
                "{outcome:?}"
            );
            let reviewed =
                change_with_debt(versioned_debt(), true, vec![independent_verdict(outcome)]);
            assert_eq!(
                standing_of(&reviewed),
                Some(DebtStanding::Discharged),
                "{outcome:?}"
            );
        }
    }

    /// Rejects an audit routing assessed `ineligible` discharging any kind of
    /// debt because its declared actor differs from the author's; the same
    /// audit without an assessment is judged by authorship alone.
    #[test]
    fn an_ineligible_assessment_discharges_no_kind_of_debt() {
        let ineligible = AuditAssessment {
            independence: Independence::Ineligible,
            findings: None,
            clears: None,
            qualifiers: vec![ReviewQualifier::SameModel, ReviewQualifier::SameFamily],
            route_version: None,
            binding: Some(AuditorBinding::Bound),
            uncovered_authors: Vec::new(),
        };
        for missing in DebtMissing::ALL
            .into_iter()
            .filter(|missing| *missing != DebtMissing::IneligibleReview)
        {
            let debt = || {
                ev(
                    "fix",
                    Payload::DebtDeclared {
                        reason: "review owed".into(),
                        patchset_id: Some("ps-01".into()),
                        missing,
                        coverage: Vec::new(),
                        production: Some(test_production()),
                        ineligible: None,
                        replayed: None,
                        extent: None,
                    },
                )
            };
            let assessed = change_with_debt(
                debt(),
                true,
                vec![independent_audit(Some(ineligible.clone()))],
            );
            assert_eq!(
                standing_of(&assessed),
                Some(DebtStanding::Owed),
                "{missing:?}"
            );
            let unassessed = change_with_debt(debt(), true, vec![independent_audit(None)]);
            assert_eq!(
                standing_of(&unassessed),
                Some(DebtStanding::Discharged),
                "{missing:?}"
            );
        }
    }

    /// An obligation over `p` from `b0b0b0b0b0` to `a1a1a1a1a1`, declared on
    /// `ps-01`, which integrated at `a1a1a1a1a1`.
    fn extent_fixture() -> Vec<Event> {
        let patch = ev(
            "fix",
            serde_json::from_value(serde_json::json!({
                "event_type": "patchset-added", "patchset_id": "ps-01", "base": "b0b0b0b0b0", "head": "a1a1a1a1a1"
            }))
            .unwrap(),
        );
        let debt = ev(
            "fix",
            Payload::DebtDeclared {
                reason: "review owed".into(),
                patchset_id: Some("ps-01".into()),
                missing: DebtMissing::IndependentReview,
                coverage: Vec::new(),
                production: Some(test_production()),
                ineligible: None,
                replayed: None,
                extent: Some(scoped_extent("b0b0b0b0b0", "a1a1a1a1a1")),
            },
        );
        let integrated = ev(
            "fix",
            Payload::ChangeIntegrated {
                authority_grant_event_id: None,
                provisional_resolutions: Vec::new(),
                integrated_commit: "a1a1a1a1a1".into(),
                source_patchset_id: "ps-01".into(),
                source_head: "a1a1a1a1a1".into(),
                target_branch: "master".into(),
                target_before: "b0b0b0b0b0".into(),
                authorization: None,
                already_contained: false,
                shape: None,
                started_at: None,
                landing: None,
            },
        );
        vec![opened("fix"), patch, debt, integrated]
    }

    /// `p` at blob `aa` in the diff from `base` to `revision`.
    fn scoped_extent(base: &str, revision: &str) -> crate::extent::Extent {
        crate::extent::Extent {
            subject: crate::extent::ExtentSubject::Diff {
                base: base.into(),
                revision: revision.into(),
            },
            scope: Some(crate::extent::ExtentScope {
                entries: vec![crate::extent::ExtentEntry {
                    path: "p".into(),
                    content: crate::extent::EntryContent::Blob("aa".into()),
                }],
                exclusions: Vec::new(),
            }),
        }
    }

    fn extent_audit(extent: crate::extent::Extent) -> Event {
        let mut audit = ev(
            "fix",
            Payload::AuditVerdictRecorded {
                basis: Default::default(),
                tiebreak_reviewer: None,
                tiebreak_target: None,
                dispositions: Vec::new(),
                receipt: None,
                revision: "a1a1a1a1a1".into(),
                verdict: Verdict::Approved,
                body: None,
                findings: vec![],
                route_version: None,
                assessment: None,
                held_roles: Vec::new(),
                extent: Some(extent),
            },
        );
        audit.actor = "reader".into();
        audit
    }

    /// Claim: an audit listing the same final path and blob as the
    /// obligation, read from another base, does not discharge it and is
    /// listed as partial coverage; the same audit from the obligation's base
    /// does. Rejects coverage judged by the final inventory alone.
    #[test]
    fn identical_inventories_from_another_base_do_not_discharge() {
        let mut events = extent_fixture();
        let other_base = extent_audit(scoped_extent("b1b1b1b1b1", "a1a1a1a1a1"));
        let other_id = other_base.event_id.clone();
        events.push(other_base);
        let state = reduce(&events).unwrap();
        let debt = state.debt.as_ref().unwrap();
        assert_eq!(debt.standing, DebtStanding::Owed, "{debt:?}");
        assert_eq!(debt.partial_coverage.len(), 1, "{debt:?}");
        assert_eq!(debt.partial_coverage[0].event_id, other_id);
        assert_eq!(debt.partial_coverage[0].reason, "base-mismatch");

        let same_base = extent_audit(scoped_extent("b0b0b0b0b0", "a1a1a1a1a1"));
        let same_id = same_base.event_id.clone();
        events.push(same_base);
        let state = reduce(&events).unwrap();
        let debt = state.debt.as_ref().unwrap();
        assert_eq!(debt.standing, DebtStanding::Discharged, "{debt:?}");
        assert_eq!(
            debt.discharged_by_event_id.as_deref(),
            Some(same_id.as_str())
        );
        assert_eq!(debt.discharge_extent, Some("covered"));
    }

    /// Claim: an audit of a signature-only successor of the obligation's
    /// revision, from the same base, covers it, and an audit of a successor
    /// that changed content does not. Rejects an extent following a content
    /// rewrite to vouch for what nobody read.
    #[test]
    fn an_extent_follows_only_a_signature_only_successor() {
        for (signature_only, discharged) in [(true, true), (false, false)] {
            let mut events = extent_fixture();
            events.push(extent_audit(scoped_extent("b0b0b0b0b0", "a2a2a2a2a2")));
            let rewrite = ev(
                crate::store::Store::REPOSITORY_SCOPE,
                Payload::HistoryRewritten {
                    mapping: std::collections::BTreeMap::from([(
                        "a1a1a1a1a1".to_string(),
                        Some("a2a2a2a2a2".to_string()),
                    )]),
                    reason: "sign".into(),
                    tool: None,
                    signature_only: if signature_only {
                        vec!["a1a1a1a1a1".into()]
                    } else {
                        Vec::new()
                    },
                    role_additions: Vec::new(),
                    refs: std::collections::BTreeMap::new(),
                    retained: std::collections::BTreeMap::new(),
                },
            );
            let rewrites = RewriteMap::from_events(std::iter::once(&rewrite)).unwrap();
            let mut state = reduce_following(&events, &rewrites).unwrap();
            state.settle_debt();
            let debt = state.debt.as_ref().unwrap();
            assert_eq!(
                debt.standing == DebtStanding::Discharged,
                discharged,
                "signature-only {signature_only}: {debt:?}"
            );
            if !discharged {
                assert_eq!(debt.partial_coverage[0].reason, "revision-mismatch");
            }
        }
    }
}
