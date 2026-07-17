use crate::model::*;
use anyhow::{bail, Result};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
pub struct Patchset {
    pub id: String,
    pub base: String,
    pub head: String,
    pub merge_base: Option<String>,
    pub author: Option<GitIdentity>,
    pub committer: Option<GitIdentity>,
    pub claim_actor: Option<String>,
    pub provenance_mismatch: Option<bool>,
    pub created_at: DateTime<Utc>,
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
    pub changed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaimState {
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
    let expires_at = claim.last_activity_at + seconds_delta(claim.ttl_seconds);
    let expired = now >= expires_at;
    let (stage, stage_started_at, budget_seconds) = match &claim.progress {
        Some(progress) => (
            progress.stage.as_str().to_string(),
            progress.changed_at,
            progress
                .stage
                .budget_key()
                .and_then(|key| claim.stage_budgets.get(&key).copied()),
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
    pub dispositions: Vec<DispositionEntry>,
}

impl FindingState {
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
    pub actor: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationEntry {
    pub event_id: String,
    pub gate: Option<String>,
    pub command: String,
    pub revision: String,
    pub result: VerifyResult,
    pub hostname: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
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
pub struct ClosureState {
    pub outcome: Closure,
    pub integrated_commit: Option<String>,
    pub superseded_by: Option<String>,
    pub event_id: String,
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
    pub blocked_by: Vec<String>,
    pub tags: Vec<String>,
    pub opened_at: chrono::DateTime<chrono::Utc>,
    pub patchsets: Vec<Patchset>,
    pub comments: Vec<CommentEntry>,
    pub findings: BTreeMap<String, FindingState>,
    pub verdicts: Vec<VerdictEntry>,
    pub verifications: Vec<VerificationEntry>,
    pub claim: Option<ClaimState>,
    pub hold: Option<String>,
    pub closure: Option<ClosureState>,
}

impl ChangeState {
    pub fn latest_patchset(&self) -> Option<&Patchset> {
        self.patchsets.last()
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

    /// Latest verification result per gate name at an exact revision.
    pub fn gate_result_at(&self, gate: &str, revision: &str) -> Option<VerifyResult> {
        self.verifications
            .iter()
            .rfind(|v| v.gate.as_deref() == Some(gate) && v.revision == revision)
            .map(|v| v.result)
    }

    pub fn resolve_finding_id(&self, needle: &str) -> Result<String> {
        if self.findings.contains_key(needle) {
            return Ok(needle.to_string());
        }
        let matches: Vec<&String> = self
            .findings
            .keys()
            .filter(|k| k.starts_with(needle))
            .collect();
        match matches.len() {
            0 => bail!("no finding matches {needle:?}"),
            1 => Ok(matches[0].clone()),
            _ => bail!("ambiguous finding {needle:?}"),
        }
    }
}

pub fn reduce(events: &[Event]) -> Result<ChangeState> {
    let mut iter = events.iter();
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
            } => (
                ChangeState {
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
                    blocked_by: blocked_by.clone(),
                    tags: tags.clone(),
                    opened_at: ev.created_at,
                    patchsets: Vec::new(),
                    comments: Vec::new(),
                    findings: BTreeMap::new(),
                    verdicts: Vec::new(),
                    verifications: Vec::new(),
                    claim: None,
                    hold: None,
                    closure: None,
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
            }
            Payload::PatchsetAdded {
                patchset_id,
                base,
                head,
                merge_base,
                author_name,
                author_email,
                committer_name,
                committer_email,
            } => {
                let claim_actor = state.claim.as_ref().and_then(|claim| {
                    claim_timing_at(claim, ev.created_at)
                        .active
                        .then(|| claim.owner.actor.clone())
                });
                if claim_actor.is_some() {
                    let claim = state.claim.as_mut().expect("claim was present");
                    claim.last_activity_at = ev.created_at;
                    claim.progress = Some(StageProgress {
                        stage: ClaimStage::Snapshotted,
                        note: None,
                        changed_at: ev.created_at,
                    });
                }
                let author = git_identity(author_name, author_email);
                let committer = git_identity(committer_name, committer_email);
                let provenance_mismatch = claim_actor
                    .as_ref()
                    .zip(author.as_ref())
                    .map(|(actor, author)| actor != &author.name);
                state.patchsets.push(Patchset {
                    id: patchset_id.clone(),
                    base: base.clone(),
                    head: head.clone(),
                    merge_base: merge_base.clone(),
                    author,
                    committer,
                    claim_actor,
                    provenance_mismatch,
                    created_at: ev.created_at,
                });
            }
            Payload::ClaimSet {
                ttl_seconds,
                stage_budgets,
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
                let renewal = state.claim.as_ref().filter(|claim| {
                    claim.owner == owner && claim_timing_at(claim, ev.created_at).active
                });
                let claimed_at = renewal.map_or(ev.created_at, |claim| claim.claimed_at);
                let progress = renewal.and_then(|claim| claim.progress.clone());
                state.claim = Some(ClaimState {
                    owner,
                    ttl_seconds: *ttl_seconds,
                    stage_budgets: stage_budgets.clone(),
                    claimed_at,
                    last_activity_at: ev.created_at,
                    progress,
                });
            }
            Payload::ClaimReleased => state.claim = None,
            Payload::StageSet { stage, note } => {
                let claim = state
                    .claim
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("stage event {} has no claim", ev.event_id))?;
                if claim.owner.actor != ev.actor
                    || ev.harness.as_deref() != Some(claim.owner.harness.as_str())
                    || ev.session.as_deref() != Some(claim.owner.session.as_str())
                {
                    bail!("stage event {} is not owned by the live claim", ev.event_id);
                }
                if !claim_timing_at(claim, ev.created_at).active {
                    bail!("stage event {} follows an expired claim", ev.event_id);
                }
                claim.last_activity_at = ev.created_at;
                claim.progress = Some(StageProgress {
                    stage: *stage,
                    note: note.clone(),
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
                        dispositions: Vec::new(),
                    },
                );
            }
            Payload::ReplyAdded {
                parent_event_id,
                body,
            } => {
                if let Some(c) = state
                    .comments
                    .iter_mut()
                    .find(|c| &c.event_id == parent_event_id)
                {
                    c.replies
                        .push((ev.event_id.clone(), ev.actor.clone(), body.clone()));
                }
                // Replies to findings/other events are kept in the ledger and
                // shown by render; no state transition needed here.
            }
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
                            dispositions: Vec::new(),
                        },
                    );
                }
                state.verdicts.push(VerdictEntry {
                    event_id: ev.event_id.clone(),
                    patchset_id: patchset_id.clone(),
                    verdict: *verdict,
                    actor: ev.actor.clone(),
                    created_at: ev.created_at,
                });
            }
            Payload::VerificationRecorded {
                gate,
                command,
                revision,
                result,
                hostname,
                ..
            } => state.verifications.push(VerificationEntry {
                event_id: ev.event_id.clone(),
                gate: gate.clone(),
                command: command.clone(),
                revision: revision.clone(),
                result: *result,
                hostname: hostname.clone(),
                created_at: ev.created_at,
            }),
            Payload::HoldSet { reason } => state.hold = Some(reason.clone()),
            Payload::HoldReleased { .. } => state.hold = None,
            Payload::ChangeClosed {
                outcome,
                integrated_commit,
                superseded_by,
            } => {
                state.closure = Some(ClosureState {
                    outcome: *outcome,
                    integrated_commit: integrated_commit.clone(),
                    superseded_by: superseded_by.clone(),
                    event_id: ev.event_id.clone(),
                });
            }
        }
    }
    Ok(state)
}

fn git_identity(name: &Option<String>, email: &Option<String>) -> Option<GitIdentity> {
    name.as_ref().map(|name| GitIdentity {
        name: name.clone(),
        email: email.clone(),
    })
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
                slug: "fix".into(),
                title: "Fix".into(),
                profile: "local".into(),
                target_branch: "master".into(),
                branch: "arc/fix".into(),
                base: "b0".into(),
                worktree: None,
                blocked_by: Vec::new(),
                tags: Vec::new(),
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

    #[test]
    fn hold_toggles() {
        let change = "fix-abc123";
        let mut events = vec![opened(change)];
        events.push(ev(
            change,
            Payload::HoldSet {
                reason: "testing".into(),
            },
        ));
        assert!(reduce(&events).unwrap().hold.is_some());
        events.push(ev(change, Payload::HoldReleased { reason: None }));
        assert!(reduce(&events).unwrap().hold.is_none());
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
