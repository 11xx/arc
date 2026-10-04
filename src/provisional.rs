//! Provisional deliveries are retained facts with obligations independent of closure.
use crate::model::*;
use crate::state::ChangeState;
use anyhow::{bail, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Obligation {
    pub change_id: String,
    pub event_id: String,
    pub source_replica: String,
    pub actor: String,
    pub recipient: ProvisionalRecipient,
    pub source_patchset_id: String,
    pub source_head: String,
    pub publication_target: String,
    pub staging_branch: String,
    pub provisional_commit: String,
    pub local_merge: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub age_seconds: u64,
    pub disposition: String,
    pub reason: Option<String>,
    pub refusal_event_id: Option<String>,
    pub integration_event_id: Option<String>,
    pub accepted_patchset_id: Option<String>,
    pub resolution_kind: Option<ProvisionalResolutionKind>,
}

pub fn project_obligations(state: &ChangeState) -> Vec<Obligation> {
    state
        .provisional_integrations
        .iter()
        .filter_map(|event| {
            let Payload::ChangeProvisionallyIntegrated {
                source_patchset_id,
                source_head,
                publication_target,
                staging_branch,
                provisional_commit,
                recipient,
                authority_observed,
                ..
            } = &event.payload
            else {
                return None;
            };
            let mut row = Obligation {
                change_id: state.change_id.clone(),
                event_id: event.event_id.clone(),
                source_replica: event.repository_id.clone(),
                actor: event.actor.clone(),
                recipient: recipient.clone(),
                source_patchset_id: source_patchset_id.clone(),
                source_head: source_head.clone(),
                publication_target: publication_target.clone(),
                staging_branch: staging_branch.clone(),
                provisional_commit: provisional_commit.clone(),
                local_merge: state
                    .provisional_signature_successors
                    .get(provisional_commit)
                    .cloned()
                    .unwrap_or_else(|| provisional_commit.clone()),
                created_at: event.created_at,
                age_seconds: 0,
                disposition: "pending".into(),
                reason: None,
                refusal_event_id: None,
                integration_event_id: None,
                accepted_patchset_id: None,
                resolution_kind: None,
            };
            if state.provisional_integrations.iter().any(|other| {
                other.event_id > event.event_id && other.repository_id == event.repository_id
            }) {
                row.disposition = "superseded".into();
                row.reason =
                    Some("another local preparation exists; acceptance remains owed".into());
            }
            let holder = authority_observed
                .as_ref()
                .and_then(|authority| authority.holder_repository_id.as_ref())
                .or_else(|| {
                    state
                        .privilege_scope
                        .as_ref()
                        .and_then(|scope| scope.authority_holder.as_ref())
                });
            if let Some(event_id) =
                authority_refusal(state, holder.map(String::as_str), source_patchset_id)
            {
                row.disposition = "authority-refused".into();
                row.reason = Some(format!(
                    "changes requested on the delivered patchset by authority verdict {event_id}"
                ));
                row.refusal_event_id = Some(event_id);
            }
            if let Some(closure) = &state.closure {
                row.reason.get_or_insert_with(|| {
                    format!(
                        "closure {} requires explicit reconciliation",
                        closure.event_id
                    )
                });
                if row.disposition != "authority-refused" {
                    row.disposition = "requires-reconciliation".into();
                }
            }
            for resolution in &state.provisional_resolutions {
                if state.privilege_scope.as_ref().is_some_and(|scope| {
                    !scope.permits(
                        &resolution.event_id,
                        &resolution.repository_id,
                        crate::import_authorization::Role::Integration,
                    )
                }) {
                    continue;
                }
                let accepted = match &resolution.payload {
                    Payload::ChangeIntegrated {
                        provisional_resolutions,
                        ..
                    } => provisional_resolutions
                        .iter()
                        .find(|r| r.provisional_event_id == event.event_id)
                        .map(|r| {
                            (
                                resolution.event_id.clone(),
                                r.accepted_patchset_id.clone(),
                                r.kind,
                                None,
                            )
                        }),
                    Payload::ProvisionalIntegrationResolved {
                        provisional_event_id,
                        integration_event_id,
                        accepted_patchset_id,
                        kind,
                        reason,
                        ..
                    } if *provisional_event_id == event.event_id => Some((
                        integration_event_id.clone(),
                        accepted_patchset_id.clone(),
                        *kind,
                        Some(reason.clone()),
                    )),
                    _ => None,
                };
                if let Some((integration, patchset, kind, reason)) = accepted {
                    row.disposition = if matches!(recipient, ProvisionalRecipient::Forge { .. }) {
                        "resolved-by-assertion"
                    } else {
                        "resolved"
                    }
                    .into();
                    row.reason = reason;
                    row.integration_event_id = Some(integration);
                    row.accepted_patchset_id = Some(patchset);
                    row.resolution_kind = Some(kind);
                }
            }
            Some(row)
        })
        .collect()
}

// An authority refusal is a negative fact; it grants no review or publication permission.
fn authority_refusal(state: &ChangeState, holder: Option<&str>, patchset: &str) -> Option<String> {
    let history: Vec<_> = state
        .verdicts
        .iter()
        .filter(|verdict| {
            (match &verdict.authority_observed {
                Some(observed) => {
                    observed.holder_repository_id.as_deref() == Some(&verdict.origin_replica_id)
                        && state.privilege_scope.as_ref().is_some_and(|scope| {
                            scope.authority_grant_holders.get(&observed.grant_event_id)
                                == Some(&verdict.origin_replica_id)
                        })
                }
                None => holder == Some(verdict.origin_replica_id.as_str()),
            }) && state.privilege_scope.as_ref().is_none_or(|scope| {
                verdict.origin_replica_id == scope.local_origin
                    && !scope.imported_event_ids.contains(&verdict.event_id)
                    || scope
                        .admission_for(&verdict.event_id)
                        .is_some_and(|receipt| {
                            receipt.authorized_roles.iter().any(|role| {
                                matches!(
                                    role,
                                    crate::import_authorization::Role::ReviewInput
                                        | crate::import_authorization::Role::ReviewAuthority
                                )
                            })
                        })
            })
        })
        .cloned()
        .collect();
    history
        .iter()
        .find(|refusal| {
            refusal.verdict == Verdict::ChangesRequested
                && refusal.patchset_id == patchset
                && !history.iter().any(|judgment| {
                    judgment.verdict == Verdict::Approved
                        && judgment.patchset_id == refusal.patchset_id
                        && judgment.authority_observed.is_some()
                        && explicitly_supersedes(
                            judgment,
                            &refusal.event_id,
                            &history,
                            &mut Default::default(),
                        )
                })
        })
        .map(|verdict| verdict.event_id.clone())
}

fn explicitly_supersedes(
    judgment: &crate::state::VerdictEntry,
    refusal: &str,
    history: &[crate::state::VerdictEntry],
    seen: &mut std::collections::BTreeSet<String>,
) -> bool {
    if !seen.insert(judgment.event_id.clone()) {
        return false;
    }
    let Some(VerdictRelation::Supersedes { observed }) = &judgment.relation else {
        return false;
    };
    observed.iter().any(|id| {
        id == refusal
            || history
                .iter()
                .find(|prior| prior.event_id == *id && prior.patchset_id == judgment.patchset_id)
                .is_some_and(|prior| explicitly_supersedes(prior, refusal, history, seen))
    })
}

pub fn project_obligations_at(
    state: &ChangeState,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Obligation> {
    let mut rows = project_obligations(state);
    for row in &mut rows {
        row.age_seconds = u64::try_from((now - row.created_at).num_seconds()).unwrap_or(0);
    }
    rows
}

pub fn unresolved_at(state: &ChangeState, now: chrono::DateTime<chrono::Utc>) -> Vec<Obligation> {
    project_obligations_at(state, now)
        .into_iter()
        .filter(|row| !row.disposition.starts_with("resolved"))
        .collect()
}

pub fn unresolved(state: &ChangeState) -> Vec<Obligation> {
    project_obligations(state)
        .into_iter()
        .filter(|row| !row.disposition.starts_with("resolved"))
        .collect()
}

pub fn satisfies_local_prerequisite(
    cwd: &Path,
    state: &ChangeState,
    local: &str,
    staging_head: &str,
) -> Result<Option<ProvisionalPrerequisite>> {
    if state.closure.is_some() {
        return Ok(None);
    }
    let Some(latest) = state.latest_patchset() else {
        return Ok(None);
    };
    for row in project_obligations(state).into_iter().rev() {
        if state.privilege_scope.as_ref().is_some_and(|scope| {
            !scope.permits(
                &row.event_id,
                &row.source_replica,
                crate::import_authorization::Role::ProvisionalIntegration,
            )
        }) {
            continue;
        }
        if row.source_replica != local
            || row.source_patchset_id != latest.id
            || row.disposition == "authority-refused"
            || (row.source_head != latest.head
                && state.provisional_signature_successors.get(&row.source_head)
                    != Some(&latest.head))
        {
            continue;
        }
        if crate::gitio::read_commit(cwd, &row.local_merge).is_err() {
            continue;
        }
        if crate::gitio::is_ancestor(cwd, &row.local_merge, staging_head)? {
            return Ok(Some(ProvisionalPrerequisite {
                change_id: state.change_id.clone(),
                provisional_event_id: row.event_id,
                source_patchset_id: row.source_patchset_id,
                provisional_commit: row.local_merge,
            }));
        }
    }
    Ok(None)
}

pub fn acceptance_kind(
    state: &ChangeState,
    delivered: &str,
    accepted_head: &str,
) -> ProvisionalResolutionKind {
    if delivered == accepted_head
        || state
            .provisional_signature_successors
            .get(delivered)
            .is_some_and(|head| head == accepted_head)
    {
        ProvisionalResolutionKind::SameHead
    } else {
        ProvisionalResolutionKind::RevisedHead
    }
}

pub fn acceptances(
    state: &ChangeState,
    accepted: &str,
    head: &str,
) -> Result<Vec<ProvisionalResolution>> {
    Ok(unresolved(state)
        .into_iter()
        .filter(|row| matches!(&row.recipient, ProvisionalRecipient::Replica { repository_id } if !repository_id.trim().is_empty()))
        .map(|row| ProvisionalResolution {
            provisional_event_id: row.event_id,
            delivered_patchset_id: row.source_patchset_id,
            accepted_patchset_id: accepted.into(),
            kind: acceptance_kind(state, &row.source_head, head),
        })
        .collect())
}

pub fn validate_resolution(
    state: &ChangeState,
    provisional: &str,
    integration: &Event,
    recipient: &str,
) -> Result<(String, ProvisionalResolutionKind)> {
    let row = project_obligations(state)
        .into_iter()
        .find(|row| row.event_id == provisional)
        .ok_or_else(|| anyhow::anyhow!("missing provisional resolution reference {provisional}"))?;
    if row
        .integration_event_id
        .as_deref()
        .is_some_and(|held| held != integration.event_id)
    {
        bail!(
            "conflicting resolution for {provisional}: {:?}, not {}",
            row.integration_event_id,
            integration.event_id
        );
    }
    let (patchset, head) = match (&row.recipient, &integration.payload) {
        (ProvisionalRecipient::Replica { repository_id }, Payload::ChangeIntegrated { source_patchset_id, source_head, .. }) if !repository_id.trim().is_empty() && integration.repository_id == recipient => (source_patchset_id, source_head),
        (ProvisionalRecipient::Forge { reference }, Payload::IntegrationAsserted { external_reference: Some(held), source_patchset_id, source_head, .. }) if reference == held => (source_patchset_id, source_head),
        _ => bail!("integration {} does not provide the required recipient/integration evidence for {provisional}", integration.event_id),
    };
    let accepted_head = state
        .closure
        .as_ref()
        .filter(|closure| closure.event_id == integration.event_id)
        .and_then(|closure| closure.source_head.as_deref())
        .unwrap_or(head);
    if integration.change_id != state.change_id
        || !state
            .patchsets
            .iter()
            .any(|p| p.id == *patchset && p.head == accepted_head)
    {
        bail!(
            "integration {} has no valid accepted patchset in {}",
            integration.event_id,
            state.change_id
        );
    }
    let kind = acceptance_kind(state, &row.source_head, head);
    if row.integration_event_id.is_some()
        && (row.accepted_patchset_id.as_deref() != Some(patchset)
            || row.resolution_kind != Some(kind))
    {
        bail!("conflicting accepted patchset or resolution kind for {provisional}");
    }
    Ok((patchset.clone(), kind))
}

pub fn retention_ref(change: &str, event: &str) -> String {
    format!("refs/arc/provisional/{change}/{event}")
}

pub fn obsolete_ancestry(
    cwd: &Path,
    state: &ChangeState,
    states: &std::collections::BTreeMap<String, ChangeState>,
) -> Result<Vec<String>> {
    if state.is_closed() {
        return Ok(Vec::new());
    }
    let Some(patchset) = state.latest_patchset() else {
        return Ok(Vec::new());
    };
    let Ok(target) = crate::gitio::branch_head(cwd, &state.target_branch) else {
        return Ok(Vec::new());
    };
    let mut merges = Vec::new();
    for prerequisite in states.values() {
        for row in project_obligations(prerequisite) {
            for merge in [row.provisional_commit, row.local_merge] {
                if crate::gitio::commit_exists(cwd, &merge)?
                    && crate::gitio::is_ancestor(cwd, &merge, &patchset.head)?
                    && !crate::gitio::is_ancestor(cwd, &merge, &target)?
                {
                    merges.push(merge);
                }
            }
        }
    }
    merges.sort();
    merges.dedup();
    Ok(merges)
}

/// Every settlement identifies held delivery and acceptance evidence without mutating closure.
pub fn validate_resolution_history(
    state: &ChangeState,
    events: &[Event],
    store: Option<&crate::store::Store>,
) -> Result<()> {
    let mut settled =
        std::collections::BTreeMap::<String, (String, String, ProvisionalResolutionKind)>::new();
    for event in events {
        let resolutions = match &event.payload {
            Payload::ChangeIntegrated {
                source_patchset_id,
                authority_grant_event_id,
                provisional_resolutions,
                ..
            } => {
                if !provisional_resolutions.is_empty() {
                    let grant = authority_grant_event_id.as_deref().ok_or_else(|| anyhow::anyhow!("integration {} resolves provisional delivery without an authority grant", event.event_id))?;
                    if let Some(store) = store {
                        if crate::replica::grant_holder(store, grant)?.as_deref()
                            != Some(&event.repository_id)
                        {
                            bail!(
                                "integration {} has invalid authority grant {grant}",
                                event.event_id
                            );
                        }
                    }
                }
                let mut rows = Vec::new();
                for row in provisional_resolutions {
                    let delivery = state
                        .provisional_integrations
                        .iter()
                        .find(|delivery| delivery.event_id == row.provisional_event_id)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "missing provisional resolution reference {}",
                                row.provisional_event_id
                            )
                        })?;
                    let Payload::ChangeProvisionallyIntegrated {
                        source_patchset_id: delivered,
                        ..
                    } = &delivery.payload
                    else {
                        unreachable!()
                    };
                    if *delivered != row.delivered_patchset_id
                        || *source_patchset_id != row.accepted_patchset_id
                    {
                        bail!("integration {} has conflicting delivered/accepted patchset resolution for {}", event.event_id, row.provisional_event_id);
                    }
                    rows.push((
                        row.provisional_event_id.clone(),
                        event,
                        row.accepted_patchset_id.clone(),
                        row.kind,
                    ));
                }
                rows
            }
            Payload::ProvisionalIntegrationResolved {
                provisional_event_id,
                integration_event_id,
                accepted_patchset_id,
                authority_grant_event_id,
                kind,
                reason,
            } => {
                if reason.trim().is_empty() {
                    bail!(
                        "provisional resolution {} has a blank reason",
                        event.event_id
                    );
                }
                if let Some(store) = store {
                    if crate::replica::grant_holder(store, authority_grant_event_id)?.as_deref()
                        != Some(&event.repository_id)
                    {
                        bail!("provisional resolution {} has invalid authority grant {authority_grant_event_id}", event.event_id);
                    }
                }
                let integration = events
                    .iter()
                    .find(|event| event.event_id == *integration_event_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "missing integration resolution reference {integration_event_id}"
                        )
                    })?;
                vec![(
                    provisional_event_id.clone(),
                    integration,
                    accepted_patchset_id.clone(),
                    *kind,
                )]
            }
            _ => Vec::new(),
        };
        for (id, integration, patchset, kind) in resolutions {
            let mut unprojected = state.clone();
            unprojected.provisional_resolutions.clear();
            let (accepted, expected_kind) =
                validate_resolution(&unprojected, &id, integration, &integration.repository_id)?;
            if patchset != accepted || kind != expected_kind {
                bail!("conflicting patchset or resolution kind for {id}");
            }
            let resolution = (integration.event_id.clone(), patchset, kind);
            if settled
                .insert(id.clone(), resolution.clone())
                .is_some_and(|prior| prior != resolution)
            {
                bail!("conflicting resolutions for provisional delivery {id}");
            }
        }
    }
    Ok(())
}

/// Substitution of a staging merge with an accepting integration requires fresh review.
pub fn retain_signature_only(
    cwd: &Path,
    store: Option<&crate::store::Store>,
    extra: &[Event],
    rewrites: &crate::rewrite::RewriteMap,
    judged: Vec<String>,
) -> Result<Vec<String>> {
    let mut provisional = std::collections::BTreeSet::new();
    let mut canonical = std::collections::BTreeSet::new();
    if let Some(store) = store {
        for state in store.readable_states()?.values() {
            for row in &state.provisional_integrations {
                if let Payload::ChangeProvisionallyIntegrated {
                    provisional_commit, ..
                } = &row.payload
                {
                    provisional.insert(provisional_commit.clone());
                    if let Some(effective) = state
                        .provisional_signature_successors
                        .get(provisional_commit)
                    {
                        provisional.insert(effective.clone());
                    }
                }
            }
            if let Some(commit) = state
                .closure
                .as_ref()
                .filter(|closure| closure.outcome == Closure::Integrated)
                .and_then(|closure| closure.integrated_commit.clone())
            {
                canonical.insert(commit);
            }
        }
    }
    for event in extra {
        match &event.payload {
            Payload::ChangeProvisionallyIntegrated {
                provisional_commit, ..
            } => {
                provisional.insert(provisional_commit.clone());
            }
            Payload::ChangeIntegrated {
                integrated_commit, ..
            }
            | Payload::IntegrationAsserted {
                integrated_commit, ..
            } => {
                canonical.insert(integrated_commit.clone());
            }
            _ => (),
        }
    }
    let originals = provisional.clone();
    for original in originals {
        for candidate in rewrites.mapped_revisions() {
            if candidate != original
                && !canonical.contains(candidate)
                && rewrites.same(&original, candidate)?
            {
                let mapping = std::collections::BTreeMap::from([(
                    original.clone(),
                    Some(candidate.to_string()),
                )]);
                if !crate::rewrite::signature_only_successors(cwd, &mapping, rewrites).is_empty() {
                    provisional.insert(candidate.to_string());
                }
            }
        }
    }
    let substitutions: Vec<_> = provisional
        .into_iter()
        .filter_map(|old| {
            let new = rewrites.current(&old).ok()?;
            (old != new && canonical.contains(&new)).then_some((old, new))
        })
        .collect();
    let mut retained = Vec::new();
    for old in judged {
        let new = rewrites.current(&old)?;
        let substituted = substitutions.iter().any(|(before, after)| {
            crate::gitio::is_ancestor(cwd, before, &old).unwrap_or(true)
                && crate::gitio::is_ancestor(cwd, after, &new).unwrap_or(true)
        });
        if !substituted {
            retained.push(old);
        }
    }
    Ok(retained)
}

pub fn needs_replay_snapshot(
    cwd: &Path,
    store: &crate::store::Store,
    state: &ChangeState,
) -> Result<bool> {
    let Some(patchset) = state
        .latest_patchset()
        .filter(|patchset| patchset.head != patchset.approved_head)
    else {
        return Ok(false);
    };
    Ok(retain_signature_only(
        cwd,
        Some(store),
        &[],
        &store.rewrites()?,
        vec![patchset.approved_head.clone()],
    )?
    .is_empty())
}
