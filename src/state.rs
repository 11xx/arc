use crate::model::*;
use anyhow::{bail, Result};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize)]
pub struct Patchset {
    pub id: String,
    pub actor: String,
    /// Where `actor` came from. `None` on patchsets recorded before arc kept
    /// the provenance, which is unknown rather than declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_source: Option<ActorSource>,
    /// Subject the snapshot was taken for, when a lead ran delegated ceremony.
    pub on_behalf_of: Option<String>,
    pub base: String,
    pub head: String,
    pub merge_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief_ref: Option<BriefRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief_version: Option<usize>,
    pub author: Option<GitIdentity>,
    pub committer: Option<GitIdentity>,
    pub claim_id: Option<String>,
    pub claim_actor: Option<String>,
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

    /// Whether arc invented the identity this patchset is attributed to.
    pub fn author_assumed(&self) -> bool {
        author_assumed(self.on_behalf_of.as_deref(), self.actor_source)
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
    pub title: Option<String>,
    pub body: String,
    pub caused_by: Vec<BriefCause>,
    pub base_revision: Option<String>,
    pub acceptance_probes: Vec<AcceptanceProbe>,
    pub plan_ref: Option<String>,
    pub plan_slice: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangelogEntry {
    pub event_id: String,
    pub category: String,
    pub body: String,
    pub actor: String,
    pub on_behalf_of: Option<String>,
    pub harness: Option<String>,
    pub session: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ChangelogEntry {
    pub fn effective_author(&self) -> &str {
        self.on_behalf_of.as_deref().unwrap_or(&self.actor)
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
    pub event_id: String,
    pub status: DispositionStatus,
    pub commit: Option<String>,
    pub evidence: Option<String>,
    pub actor: String,
    pub supersedes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindingState {
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
    pub dispositions: Vec<DispositionEntry>,
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

    /// Disposition tips: dispositions not superseded by any later one.
    /// One tip = its status governs; several = contested.
    pub fn tips(&self) -> Vec<&DispositionEntry> {
        let superseded: Vec<&str> = self
            .dispositions
            .iter()
            .flat_map(|d| d.supersedes.iter().map(String::as_str))
            .collect();
        self.dispositions
            .iter()
            .filter(|d| !superseded.contains(&d.event_id.as_str()))
            .collect()
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
    pub event_id: String,
    pub patchset_id: String,
    pub verdict: Verdict,
    pub causes: Vec<ReviewCause>,
    pub body: Option<String>,
    /// Why this verdict is owed corroboration, when the caller said it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisional: Option<String>,
    pub actor: String,
    /// Subject the verdict was cast for, when a lead reviewed on behalf of one.
    pub on_behalf_of: Option<String>,
    /// Where `actor` came from. `None` on verdicts recorded before arc kept
    /// the provenance, which is unknown rather than declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_source: Option<ActorSource>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl VerdictEntry {
    /// The author policy attributes this verdict to: the subject when cast on
    /// behalf of one, otherwise the invoker.
    pub fn effective_author(&self) -> &str {
        self.on_behalf_of.as_deref().unwrap_or(&self.actor)
    }

    /// Whether arc invented the identity this verdict is attributed to. A
    /// delegated subject is always somebody's claim; an invoker is assumed
    /// only when arc took it from git config with nobody offering one.
    pub fn author_assumed(&self) -> bool {
        author_assumed(self.on_behalf_of.as_deref(), self.actor_source)
    }
}

/// An effective author is assumed when arc took it from git config and nobody
/// supplied a subject. Provenance recorded before arc kept it is *unknown*
/// rather than assumed: an old event says nothing either way, and treating
/// silence as an invention would retroactively invalidate approvals that were
/// valid when they were made.
fn author_assumed(on_behalf_of: Option<&str>, source: Option<ActorSource>) -> bool {
    on_behalf_of.is_none() && source == Some(ActorSource::GitFallback)
}

/// A declared, not-yet-discharged review obligation.
#[derive(Debug, Clone, Serialize)]
pub struct AuditDebt {
    pub event_id: String,
    pub reason: String,
    /// The patchset this waiver was declared against, if it waived anything.
    pub patchset_id: Option<String>,
    pub actor: String,
    pub declared_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditVerdictEntry {
    pub event_id: String,
    /// The integrated revision reviewed.
    pub revision: String,
    pub verdict: Verdict,
    pub body: Option<String>,
    pub actor: String,
    pub on_behalf_of: Option<String>,
    /// Where `actor` came from. `None` on audits recorded before arc kept the
    /// provenance, which is unknown rather than declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_source: Option<ActorSource>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl AuditVerdictEntry {
    pub fn effective_author(&self) -> &str {
        self.on_behalf_of.as_deref().unwrap_or(&self.actor)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationEntry {
    pub event_id: String,
    pub run_id: Option<String>,
    pub probe: Option<ProbeEvidenceRef>,
    pub gate: Option<String>,
    pub command: String,
    /// The timeout the gate declared when this ran. `None` predates the field
    /// or means the gate declared none — unknown, not unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    pub revision: String,
    pub result: VerifyResult,
    pub attested: bool,
    pub output_tail: Option<String>,
    pub timed_out: bool,
    pub hostname: String,
    pub runner: Option<String>,
    /// The tree the command ran against, and whether it differed from the
    /// revision's own. `None` is unknown — attested evidence, or an event
    /// written before arc recorded it — never a claim that the tree was clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested_tree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_dirty: Option<bool>,
    #[serde(default, skip_serializing_if = "is_false_ref")]
    pub tree_moved: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl VerificationEntry {
    /// Whether this evidence can satisfy a gate at its recorded revision.
    /// Passing output is not reproducible when local provenance is unknown,
    /// the tested tree was not retained, the worktree was dirty, or it
    /// changed while the command ran. Attested evidence carries an external
    /// execution context instead of local tree provenance, so a passing
    /// attestation remains eligible.
    pub fn green_at_head(&self) -> bool {
        self.result == VerifyResult::Pass
            && !self.tree_moved
            && (self.attested || (self.tested_tree.is_some() && self.worktree_dirty == Some(false)))
    }
}

fn is_false_ref(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationRunEntry {
    pub run_id: String,
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
    pub actor: String,
    pub harness: Option<String>,
    pub session: Option<String>,
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
    pub event_id: String,
    #[serde(skip)]
    pub created_at: DateTime<Utc>,
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
        outcome: Closure::Integrated,
        integrated_commit: Some(integrated_commit.to_string()),
        superseded_by: None,
        integration: Some(integration),
        source_patchset_id: Some(source_patchset_id.to_string()),
        source_head: Some(source_head.to_string()),
        target_branch: Some(target_branch.to_string()),
        target_before,
        authorization: None,
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

/// The current state of one change, derived by replaying its events in
/// ULID order. The event ledger is authoritative; this is a view.
#[derive(Debug, Clone, Serialize)]
pub struct ChangeState {
    pub change_id: String,
    pub slug: String,
    pub title: String,
    pub profile: String,
    pub target_branch: String,
    pub branch: String,
    pub base: String,
    pub worktree: Option<String>,
    pub opened_by: String,
    pub opened_harness: Option<String>,
    /// Journal artifact this change was opened from, if any.
    pub journal_ref: Option<String>,
    pub blocked_by: Vec<String>,
    pub tags: Vec<String>,
    pub assigned_to: Option<String>,
    pub priority: i32,
    pub opened_at: chrono::DateTime<chrono::Utc>,
    pub patchsets: Vec<Patchset>,
    pub briefs: Vec<Brief>,
    pub changelog: Option<ChangelogEntry>,
    pub messages: Vec<MessageEntry>,
    pub comments: Vec<CommentEntry>,
    pub findings: BTreeMap<String, FindingState>,
    pub verdicts: Vec<VerdictEntry>,
    /// Post-integration audits, deliberately separate from `verdicts` so that
    /// "what shipped with what review" cannot be rewritten after the fact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit_verdicts: Vec<AuditVerdictEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub audit_findings: BTreeMap<String, FindingState>,
    /// The latest declared review obligation, if one was ever declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_debt: Option<AuditDebt>,
    /// Raised at `begin` to demand an independent verdict regardless of what
    /// the change touches. One-way: a change may raise itself, never lower
    /// itself below what the project declared.
    #[serde(default)]
    pub dangerous: bool,
    /// Event IDs of every `blocked-on` stage, so a later brief can name the
    /// block that caused it without rescanning the event log.
    pub blocked_on_stages: Vec<String>,
    pub verifications: Vec<VerificationEntry>,
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
}

impl ChangeState {
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

    /// The latest verdict overall; validity against the current head is
    /// a Git-time question answered by the status layer.
    pub fn latest_verdict(&self) -> Option<&VerdictEntry> {
        self.verdicts.last()
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

    /// Latest verification evidence per gate name at an exact revision.
    pub fn gate_evidence_at(&self, gate: &str, revision: &str) -> Option<&VerificationEntry> {
        self.verifications
            .iter()
            .rfind(|v| v.gate.as_deref() == Some(gate) && v.revision == revision)
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

pub fn reduce(events: &[Event]) -> Result<ChangeState> {
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
                blocked_by,
                tags,
                journal_ref,
                dangerous,
            } => (
                ChangeState {
                    dangerous: *dangerous,
                    change_id: ev.change_id.clone(),
                    slug: slug.clone(),
                    title: title.clone(),
                    profile: profile.clone(),
                    target_branch: target_branch.clone(),
                    branch: branch.clone(),
                    base: base.clone(),
                    worktree: worktree.clone(),
                    opened_by: ev.actor.clone(),
                    opened_harness: ev.harness.clone(),
                    journal_ref: journal_ref.clone(),
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
                    audit_verdicts: Vec::new(),
                    audit_findings: BTreeMap::new(),
                    audit_debt: None,
                    blocked_on_stages: Vec::new(),
                    verifications: Vec::new(),
                    verification_runs: Vec::new(),
                    claim: None,
                    retired_claim_ids: BTreeSet::new(),
                    holds: BTreeMap::new(),
                    closure: None,
                    forge: crate::forge::ForgeState::default(),
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

    for ev in iter {
        match &ev.payload {
            Payload::ChangeOpened { .. } => {
                bail!("duplicate change-opened event {}", ev.event_id)
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
            } => state.messages.push(MessageEntry {
                event_id: ev.event_id.clone(),
                message_type: *message_type,
                severity: *severity,
                summary: summary.clone(),
                detail: detail.clone(),
                metadata: metadata.clone(),
                actor: ev.actor.clone(),
                harness: ev.harness.clone(),
                session: ev.session.clone(),
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
            } => state.briefs.push(Brief {
                event_id: ev.event_id.clone(),
                ts: ev.created_at,
                actor: ev.actor.clone(),
                on_behalf_of: ev.on_behalf_of.clone(),
                title: title.clone(),
                body: body.clone(),
                caused_by: caused_by.clone(),
                base_revision: base_revision.clone(),
                acceptance_probes: acceptance_probes.clone(),
                plan_ref: plan_ref.clone(),
                plan_slice: plan_slice.clone(),
            }),
            Payload::ChangelogRecorded { category, body } => {
                state.changelog = Some(ChangelogEntry {
                    event_id: ev.event_id.clone(),
                    category: category.clone(),
                    body: body.clone(),
                    actor: ev.actor.clone(),
                    on_behalf_of: ev.on_behalf_of.clone(),
                    harness: ev.harness.clone(),
                    session: ev.session.clone(),
                    created_at: ev.created_at,
                });
            }
            Payload::PatchsetAdded {
                patchset_id,
                base,
                head,
                merge_base,
                brief_ref,
                author_name,
                author_email,
                committer_name,
                committer_email,
                claim_id,
                claim_actor,
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
                    id: patchset_id.clone(),
                    actor: ev.actor.clone(),
                    actor_source: ev.actor_source,
                    on_behalf_of: ev.on_behalf_of.clone(),
                    base: base.clone(),
                    head: head.clone(),
                    merge_base: merge_base.clone(),
                    brief_ref: brief_ref.clone(),
                    brief_version,
                    author,
                    committer,
                    claim_id: claim_id.clone(),
                    claim_actor: claim_actor.clone(),
                    provenance_mismatch,
                    created_at: ev.created_at,
                });
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
                state.claim = Some(ClaimState {
                    claim_id,
                    owner,
                    ttl_seconds: *ttl_seconds,
                    stage_budgets: stage_budgets.clone(),
                    claimed_at,
                    last_activity_at: ev.created_at,
                    progress,
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
                        dispositions: Vec::new(),
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
                supersedes,
            } => {
                let Some(f) = state.findings.get_mut(finding_id) else {
                    bail!(
                        "disposition {} references unknown finding {finding_id:?}",
                        ev.event_id
                    );
                };
                f.dispositions.push(DispositionEntry {
                    event_id: ev.event_id.clone(),
                    status: *status,
                    commit: commit.clone(),
                    evidence: evidence.clone(),
                    actor: ev.actor.clone(),
                    supersedes: supersedes.clone(),
                });
            }
            Payload::VerdictRecorded {
                patchset_id,
                verdict,
                causes,
                provisional,
                body,
                findings,
            } => {
                for inline in findings {
                    state.findings.insert(
                        inline.finding_id.clone(),
                        FindingState {
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
                            dispositions: Vec::new(),
                            replies: Vec::new(),
                        },
                    );
                }
                state.verdicts.push(VerdictEntry {
                    event_id: ev.event_id.clone(),
                    patchset_id: patchset_id.clone(),
                    verdict: *verdict,
                    causes: causes.clone(),
                    body: body.clone(),
                    provisional: provisional.clone(),
                    actor: ev.actor.clone(),
                    on_behalf_of: ev.on_behalf_of.clone(),
                    actor_source: ev.actor_source,
                    created_at: ev.created_at,
                });
            }
            Payload::AuditDebtDeclared {
                reason,
                patchset_id,
            } => {
                state.audit_debt = Some(AuditDebt {
                    event_id: ev.event_id.clone(),
                    reason: reason.clone(),
                    patchset_id: patchset_id.clone(),
                    actor: ev.actor.clone(),
                    declared_at: ev.created_at,
                });
            }
            Payload::AuditVerdictRecorded {
                revision,
                verdict,
                body,
                findings,
            } => {
                for inline in findings {
                    state.audit_findings.insert(
                        inline.finding_id.clone(),
                        FindingState {
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
                            dispositions: Vec::new(),
                            replies: Vec::new(),
                        },
                    );
                }
                state.audit_verdicts.push(AuditVerdictEntry {
                    event_id: ev.event_id.clone(),
                    revision: revision.clone(),
                    verdict: *verdict,
                    body: body.clone(),
                    actor: ev.actor.clone(),
                    on_behalf_of: ev.on_behalf_of.clone(),
                    actor_source: ev.actor_source,
                    created_at: ev.created_at,
                });
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
                        dispositions: Vec::new(),
                        replies: Vec::new(),
                    },
                );
            }
            Payload::AuditDispositionRecorded {
                finding_id,
                status,
                commit,
                evidence,
                supersedes,
            } => {
                let Some(finding) = state.audit_findings.get_mut(finding_id) else {
                    bail!(
                        "audit disposition {} references unknown audit finding {finding_id:?}",
                        ev.event_id
                    );
                };
                finding.dispositions.push(DispositionEntry {
                    event_id: ev.event_id.clone(),
                    status: *status,
                    commit: commit.clone(),
                    evidence: evidence.clone(),
                    actor: ev.actor.clone(),
                    supersedes: supersedes.clone(),
                });
            }
            Payload::VerificationRunStarted {
                revision,
                mode,
                skip_green,
                gates,
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
                    run_id: ev.event_id.clone(),
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
            Payload::VerificationRecorded {
                gate,
                command,
                timeout_seconds,
                revision,
                result,
                hostname,
                attested,
                run_id,
                probe,
                runner,
                output_tail,
                timed_out,
                tested_tree,
                worktree_dirty,
                tree_moved,
                ..
            } => {
                if gate.is_some() && probe.is_some() {
                    bail!(
                        "verification {} cannot be both a gate and an acceptance probe",
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
                    event_id: ev.event_id.clone(),
                    timeout_seconds: *timeout_seconds,
                    tested_tree: tested_tree.clone(),
                    worktree_dirty: *worktree_dirty,
                    tree_moved: *tree_moved,
                    run_id: run_id.clone(),
                    probe: probe.clone(),
                    gate: gate.clone(),
                    command: command.clone(),
                    revision: revision.clone(),
                    result: *result,
                    attested: *attested,
                    output_tail: output_tail.clone(),
                    timed_out: *timed_out,
                    hostname: hostname.clone(),
                    runner: runner.clone(),
                    created_at: ev.created_at,
                });
            }
            Payload::VerificationReused {
                run_id,
                gate,
                revision,
                evidence_event_id,
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
                if evidence.gate.as_deref() != Some(gate)
                    || evidence.revision != *revision
                    || evidence.result != VerifyResult::Pass
                {
                    bail!(
                        "verification reuse {} does not match passing {gate} evidence at {revision}",
                        ev.event_id
                    );
                }
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
                    target_before: None,
                    authorization: None,
                    event_id: ev.event_id.clone(),
                    created_at: ev.created_at,
                });
            }
            Payload::ChangeIntegrated {
                integrated_commit,
                source_patchset_id,
                source_head,
                target_branch,
                target_before,
                authorization,
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
                closure.authorization = authorization.clone();
                state.closure = Some(closure);
            }
            Payload::IntegrationAsserted {
                integrated_commit,
                source_patchset_id,
                source_head,
                target_branch,
                target_before,
            } => {
                state.closure = Some(integrated_closure(
                    ev,
                    IntegrationKind::Asserted,
                    integrated_commit,
                    source_patchset_id,
                    source_head,
                    target_branch,
                    target_before.clone(),
                ));
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
            Payload::ForgeLink {
                pr_number,
                url,
                base_repo,
                base_ref,
                head_repo,
                head_ref,
                head_sha,
            } => {
                let record = crate::forge::ForgeLinkRecord {
                    event_id: ev.event_id.clone(),
                    pr_number: *pr_number,
                    url: url.clone(),
                    base_repo: base_repo.clone(),
                    base_ref: base_ref.clone(),
                    head_repo: head_repo.clone(),
                    head_ref: head_ref.clone(),
                    head_sha: head_sha.clone(),
                };
                state.forge.links.push(record.clone());
                state.forge.link = Some(record);
            }
            Payload::ForgeChecks {
                pr_head,
                state: check_state,
                detail,
            } => {
                state.forge.checks = Some(crate::forge::ForgeChecksRecord {
                    pr_head: pr_head.clone(),
                    state: *check_state,
                    detail: detail.clone(),
                });
            }
            Payload::ForgePrState {
                state: pr_state,
                merge_sha,
                link_event_id,
                pr_head,
            } => {
                // A binding naming a link this change has not recorded yet
                // cannot describe any PR the ledger knows about. In a
                // well-formed ledger that is impossible — events replay in ID
                // order — so it means an imported or hand-edited history, and
                // the honest reading is that the fact binds to nothing.
                let unresolved_binding = match (link_event_id, pr_head) {
                    (Some(id), Some(_)) => {
                        !state.forge.links.iter().any(|link| link.event_id == *id)
                    }
                    (None, None) => false,
                    _ => true,
                };
                state
                    .forge
                    .pr_states
                    .push(crate::forge::ForgePrStateRecord {
                        state: *pr_state,
                        merge_sha: merge_sha.clone(),
                        link_event_id: link_event_id.clone(),
                        pr_head: pr_head.clone(),
                        unresolved_binding,
                    });
            }
            // Repository-scoped, so it is never in a change's log; if one
            // arrives by import it says nothing about this change.
            Payload::HistoryRewritten { .. } => {}
            // An event this build does not recognize. Typed loading skips
            // unknown events before replay, so this arm is defensive: keep the
            // raw history intact without mutating the derived view.
            Payload::Unknown => {}
        }
    }
    for reply in replies {
        attach_reply(&mut state, reply);
    }
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
    /// `arc query --audit-debt` finds it once a reviewer is available again.
    /// Whether the declared debt still authorizes its recorded patchset.
    ///
    /// Only while it names the patchset that is about to ship. Any newer
    /// snapshot leaves the waiver behind exactly as it leaves an approval
    /// behind, so re-declaring is a deliberate act rather than a thing that
    /// happened once and never expired.
    pub fn audit_debt_waives_latest_patchset(&self) -> bool {
        let Some(debt) = &self.audit_debt else {
            return false;
        };
        let Some(declared_for) = debt.patchset_id.as_deref() else {
            return false;
        };
        self.latest_patchset()
            .is_some_and(|patchset| patchset.id == declared_for)
    }

    /// A review this change owes and nobody has recorded, by any route.
    ///
    /// Scoped to integrated changes because that is when the obligation is
    /// actionable: an audit reviews a revision that shipped, so a debt on an
    /// open change is a pending waiver rather than owed work. Queueing it
    /// earlier would offer a reviewer an item `arc audit` then refuses.
    /// Whether a verdict the caller marked as owed corroboration is still
    /// the change's gating approval, with no audit having supplied it.
    ///
    /// Independent of integration, unlike audit debt: a provisional approval
    /// is an open obligation the moment it is recorded, because the whole
    /// point is to see it before the merge rather than after.
    pub fn provisional_approval_outstanding(&self) -> bool {
        let Some(verdict) = self.latest_verdict() else {
            return false;
        };
        if verdict.provisional.is_none() || verdict.verdict != Verdict::Approved {
            return false;
        }
        !self.corroborated_after(verdict.created_at)
    }

    /// Whether an independent audit verdict was recorded after this instant.
    /// An audit is the discharge for both obligations, so the two share it.
    fn corroborated_after(&self, when: chrono::DateTime<chrono::Utc>) -> bool {
        self.audit_verdicts
            .iter()
            .any(|audit| audit.created_at > when)
    }

    pub fn audit_debt_outstanding(&self) -> bool {
        let Some(debt) = &self.audit_debt else {
            return false;
        };
        if !self
            .closure
            .as_ref()
            .is_some_and(|closure| closure.outcome == Closure::Integrated)
        {
            return false;
        }
        !self.audit_debt_discharged(debt)
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
    fn audit_debt_discharged(&self, debt: &AuditDebt) -> bool {
        if self
            .audit_verdicts
            .iter()
            .any(|audit| audit.created_at >= debt.declared_at)
        {
            return true;
        }
        // Only a verdict on the revision that actually shipped counts; a
        // verdict on an earlier draft judged something else.
        let Some(shipped) = self
            .closure
            .as_ref()
            .and_then(|closure| closure.source_patchset_id.as_deref())
        else {
            return false;
        };
        let Some(author) = self
            .patchsets
            .iter()
            .find(|patchset| patchset.id == shipped)
            .map(Patchset::effective_author)
        else {
            return false;
        };
        self.verdicts.iter().any(|verdict| {
            verdict.created_at >= debt.declared_at
                && verdict.patchset_id == shipped
                && verdict.effective_author() != author
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn ev(change: &str, payload: Payload) -> Event {
        Event {
            schema_version: SCHEMA_VERSION,
            event_id: crate::ids::new_event_id(),
            repository_id: "repo".into(),
            change_id: change.into(),
            actor: "tester".into(),
            actor_source: Some(ActorSource::Flag),
            on_behalf_of: None,
            harness: None,
            session: None,
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
                blocked_by: Vec::new(),
                tags: Vec::new(),
                journal_ref: None,
            },
        )
    }

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

    #[test]
    fn legacy_reuse_of_non_green_passing_evidence_still_replays() {
        let change = "legacy-reuse";
        let mut events = vec![opened(change)];
        let run = ev(
            change,
            Payload::VerificationRunStarted {
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
                timeout_seconds: None,
                gate: Some("unit".into()),
                command: "true".into(),
                revision: "head".into(),
                result: VerifyResult::Pass,
                exit_code: Some(0),
                duration_ms: Some(1),
                output_tail: None,
                timed_out: false,
                hostname: "test".into(),
                attested: false,
                run_id: Some(run_id),
                probe: None,
                runner: None,
                note: None,
                tested_tree: Some("dirty-tree".into()),
                worktree_dirty: Some(true),
                tree_moved: true,
            },
        );
        let evidence_event_id = evidence.event_id.clone();
        events.push(evidence);
        let reuse_run = ev(
            change,
            Payload::VerificationRunStarted {
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
                run_id: reuse_run_id,
                gate: "unit".into(),
                revision: "head".into(),
                evidence_event_id,
            },
        ));

        let state = reduce(&events).unwrap();
        assert!(state.verification_runs.last().unwrap().complete);
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
}
