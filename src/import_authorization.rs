//! Receiver-local publishing capabilities and admission evidence.
use crate::model::{DispositionStatus, ExternalVerdict, Payload};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug)]
pub struct Refusal(pub String);
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for Refusal {}
fn refuse(message: String) -> anyhow::Error {
    Refusal(message).into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Facts,
    ReviewInput,
    ReviewAuthority,
    DebtInput,
    Waiver,
    Authority,
    Rewrite,
    Integration,
    ProvisionalIntegration,
    ExternalDecision,
}
impl Role {
    fn privileged(self) -> bool {
        matches!(
            self,
            Self::ReviewAuthority
                | Self::Waiver
                | Self::Authority
                | Self::Integration
                | Self::ExternalDecision
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplicaPolicy {
    #[serde(default)]
    pub trusted_review_replicas: Vec<String>,
    #[serde(default)]
    pub publishers: Vec<Publisher>,
    #[serde(default)]
    pub provisional_enabled: Option<bool>,
    #[serde(default)]
    pub protected_targets: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    pub principal: String,
    pub source_replica_id: String,
    #[serde(default)]
    pub origins: Vec<Origin>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub replica_id: String,
    pub roles: Vec<Role>,
    #[serde(default)]
    pub actors: Vec<String>,
}
impl ReplicaPolicy {
    pub fn validate(&self) -> Result<()> {
        let nonblank = |value: &str, key: &str| -> Result<()> {
            if value.trim().is_empty() {
                bail!("replica.{key}: blank identifier");
            }
            Ok(())
        };
        let mut principals = BTreeSet::new();
        for publisher in &self.publishers {
            nonblank(&publisher.principal, "publishers.principal")?;
            nonblank(&publisher.source_replica_id, "publishers.source_replica_id")?;
            if !principals.insert(&publisher.principal) {
                bail!(
                    "replica.publishers: duplicate principal {}",
                    publisher.principal
                );
            }
            let mut origins = BTreeSet::new();
            for origin in &publisher.origins {
                nonblank(&origin.replica_id, "publishers.origins.replica_id")?;
                if !origins.insert(&origin.replica_id) {
                    bail!(
                        "replica.publishers.origins: duplicate origin {}",
                        origin.replica_id
                    );
                }
                if origin.roles.iter().any(|role| role.privileged()) && origin.actors.is_empty() {
                    bail!("replica.publishers.origins.actors: privileged roles require an explicit actor allowlist");
                }
                let mut actors = BTreeSet::new();
                for actor in &origin.actors {
                    nonblank(actor, "publishers.origins.actors")?;
                    if !actors.insert(actor) {
                        bail!("replica.publishers.origins.actors: duplicate actor {actor}");
                    }
                }
                if origin.roles.iter().copied().collect::<BTreeSet<_>>().len() != origin.roles.len()
                {
                    bail!("replica.publishers.origins.roles: duplicate capability");
                }
            }
        }
        let mut trusted = BTreeSet::new();
        for origin in &self.trusted_review_replicas {
            nonblank(origin, "trusted_review_replicas")?;
            if !trusted.insert(origin) {
                bail!("replica.trusted_review_replicas: duplicate origin {origin}");
            }
        }
        Ok(())
    }
}

/// Alternatives within a group; every group requires one granted capability.
pub fn required_roles(payload: &Payload) -> Option<Vec<Vec<Role>>> {
    use Payload::*;
    use Role::*;
    Some(match payload {
        VerdictRecorded { .. } | AuditVerdictRecorded { .. } => {
            vec![vec![ReviewInput, ReviewAuthority]]
        }
        DebtDeclared { .. } | AuditDebtDeclared { .. } => vec![vec![DebtInput, Waiver]],
        DirtyTreeWaived { .. } | HoldReleased { .. } => vec![vec![Waiver]],
        DispositionRecorded { status, .. }
        | AuditDispositionRecorded { status, .. }
        | PostIntegrationDispositionRecorded { status, .. } => match status {
            DispositionStatus::AcceptedRisk => vec![vec![Waiver]],
            DispositionStatus::Resolved | DispositionStatus::Obsolete => {
                vec![vec![ReviewAuthority]]
            }
            DispositionStatus::StillOpen | DispositionStatus::Disputed => vec![vec![Facts]],
        },
        ExternalVerdictRecorded { verdict, .. } => {
            let mut roles = vec![vec![ExternalDecision]];
            if *verdict == ExternalVerdict::Rejected {
                roles.push(vec![Integration]);
            }
            roles
        }
        HistoryRewritten { .. } | HistoryRewriteWithdrawn { .. } => vec![vec![Rewrite]],
        ChangeProvisionallyIntegrated { .. } => vec![vec![ProvisionalIntegration]],
        ProvisionalIntegrationResolved { .. } => vec![vec![Integration]],
        ChangeIntegrated { .. }
        | IntegrationAsserted { .. }
        | ChangeClosed { .. }
        | ReadyToSend { .. } => vec![vec![Integration]],
        ChangeCheckoutBound { .. }
        | ChangeOpened { .. }
        | ChangeRolesAsserted { .. }
        | MetadataUpdated { .. }
        | Message { .. }
        | BriefRecorded { .. }
        | ChangelogRecorded { .. }
        | PatchsetAdded { .. }
        | PatchsetAttributionAmended { .. }
        | ClaimSet { .. }
        | ClaimReleased { .. }
        | StageSet { .. }
        | ContextKept { .. }
        | CommentAdded { .. }
        | FindingAdded { .. }
        | ReplyAdded { .. }
        | AuditFindingAdded { .. }
        | VerificationRunStarted { .. }
        | VerificationRecorded { .. }
        | VerificationReused { .. }
        | HoldSet { .. }
        | IterationScopeSet { .. }
        | ForgeProjection { .. }
        | ForgeLink { .. }
        | ForgeChecks { .. }
        | ForgePrState { .. }
        | ReviewPassOpened { .. }
        | ReviewPassCompleted { .. }
        | ReviewPassAbandoned { .. }
        | RunDispatched { .. }
        | RunEnded { .. }
        | CandidateRegistered { .. }
        | CandidateJudged { .. }
        | CandidateRetired { .. }
        | CandidateVerified { .. }
        | CandidateSelected { .. }
        | CandidatePromoted { .. }
        | ContextRead { .. }
        | ContextDeclared { .. }
        | ContextCaptureReported { .. } => vec![vec![Facts]],
        Unknown => return None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportAuthorization {
    pub principal: String,
    pub authentication: String,
    pub received_sha256: String,
    pub source_replica_id: String,
    pub project_id: Option<String>,
    pub policy_sha256: String,
    pub events: Vec<EventAuthorization>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventAuthorization {
    pub event_id: String,
    pub scope: String,
    pub origin_replica_id: String,
    pub event_sha256: String,
    pub authorized_roles: Vec<Role>,
}
pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
/// Bind the exact received input before interpreting its contents.
pub fn verify_received_digest(bytes: &[u8], expected: Option<&str>) -> Result<()> {
    if let Some(expected) = expected {
        let actual = digest(bytes);
        if expected != actual {
            return Err(refuse(format!(
                "received-byte digest mismatch: expected {expected}, actual {actual}"
            )));
        }
    }
    Ok(())
}

pub fn policy_digest(policy: &ReplicaPolicy) -> Result<String> {
    Ok(digest(&serde_json::to_vec(policy)?))
}

/// The caller supplies only newly introduced events, after conflict detection.
pub struct ImportRequest<'a> {
    pub bytes: &'a [u8],
    pub principal: Option<&'a str>,
    pub received_sha256: Option<&'a str>,
    pub source_replica_id: Option<&'a str>,
    pub project_id: Option<&'a str>,
    pub receiver_id: &'a str,
    pub receiver_project: Option<&'a str>,
    pub events: &'a [(String, Value)],
}
pub fn plan_import(
    policy: &ReplicaPolicy,
    source: &Path,
    request: ImportRequest<'_>,
) -> Result<Option<ImportAuthorization>> {
    let enforced = request.receiver_project.is_some()
        || !policy.publishers.is_empty()
        || request.events.iter().any(|(_, event)| {
            matches!(
                event["event_type"].as_str(),
                Some("change-provisionally-integrated" | "provisional-integration-resolved")
            )
        });
    if request.principal.is_none() && request.received_sha256.is_none() && !enforced {
        return Ok(None);
    }
    let (Some(principal), Some(expected)) = (request.principal, request.received_sha256) else {
        return Err(refuse(format!(
            "receiving replica {} requires --publisher PRINCIPAL and --received-sha256 SHA256",
            request.receiver_id
        )));
    };
    let actual = digest(request.bytes);
    if expected != actual {
        return Err(refuse(format!(
            "received-byte digest mismatch: expected {expected}, actual {actual}"
        )));
    }
    let publisher = policy
        .publishers
        .iter()
        .find(|publisher| publisher.principal == principal)
        .ok_or_else(|| {
            refuse(format!(
                "unknown principal {principal}; declared exporter {:?}; enrollment source {}",
                request.source_replica_id,
                source.display()
            ))
        })?;
    if request.source_replica_id != Some(publisher.source_replica_id.as_str()) {
        return Err(refuse(format!("principal {principal}: declared exporter {:?} differs from enrolled exporter {}; enrollment source {}", request.source_replica_id, publisher.source_replica_id, source.display())));
    }
    if request.receiver_project.is_some() && request.project_id != request.receiver_project {
        let witness = request
            .events
            .first()
            .map(|(_, event)| {
                format!(
                    "event {}, recorded origin {}",
                    event["event_id"], event["repository_id"]
                )
            })
            .unwrap_or_else(|| "no newly introduced events".into());
        return Err(refuse(format!(
            "wrong project {:?} for receiving replica {} (project {:?}); {witness}",
            request.project_id, request.receiver_id, request.receiver_project
        )));
    }
    let mut events = Vec::new();
    for (scope, value) in request.events {
        let event_id = value["event_id"]
            .as_str()
            .context("event has no event_id")?;
        let origin_id = value["repository_id"]
            .as_str()
            .context("event has no recorded origin")?;
        let origin = publisher
            .origins
            .iter()
            .find(|origin| origin.replica_id == origin_id)
            .ok_or_else(|| {
                refuse(format!(
                    "project {:?}: event {event_id} has unenrolled origin {origin_id}",
                    request.project_id
                ))
            })?;
        let requirements = if scope == "replica" {
            let event: crate::replica::ReplicaEvent = serde_json::from_value(value.clone())?;
            required_replica_roles(&event.payload)
        } else {
            let event: crate::model::Event = serde_json::from_value(value.clone())?;
            required_roles(&event.payload).ok_or_else(|| {
                refuse(format!(
                    "unclassified payload {} in event {event_id}",
                    value["event_type"]
                ))
            })?
        };
        let mut authorized_roles = Vec::new();
        for group in requirements {
            let granted: Vec<_> = group
                .iter()
                .filter(|role| origin.roles.contains(role))
                .copied()
                .collect();
            if granted.is_empty() {
                return Err(refuse(format!("event {event_id}, origin {origin_id}: missing capability {group:?}; claimed actor {}, subject {}", value["actor"], value["on_behalf_of"])));
            }
            for role in granted {
                if role.privileged() {
                    let actor = if scope == "replica" {
                        value["recorded_by"].as_str()
                    } else {
                        value["actor"].as_str()
                    };
                    let subject = value["on_behalf_of"].as_str();
                    if actor
                        .is_none_or(|actor| !origin.actors.iter().any(|allowed| allowed == actor))
                        || subject.is_some_and(|subject| {
                            !origin.actors.iter().any(|allowed| allowed == subject)
                        })
                    {
                        return Err(refuse(format!("event {event_id}, origin {origin_id}, role {role:?}: forbidden privileged actor {actor:?}, subject {subject:?}")));
                    }
                }
                authorized_roles.push(role);
            }
        }
        events.push(EventAuthorization {
            event_id: event_id.into(),
            scope: scope.clone(),
            origin_replica_id: origin_id.into(),
            event_sha256: canonical_event_digest(value)?,
            authorized_roles,
        });
    }
    Ok(Some(ImportAuthorization {
        principal: principal.into(),
        authentication: "transport-asserted".into(),
        received_sha256: actual,
        source_replica_id: publisher.source_replica_id.clone(),
        project_id: request.project_id.map(str::to_string),
        policy_sha256: policy_digest(policy)?,
        events,
    }))
}

/// Canonical content includes the complete envelope and every payload field.
pub fn canonical_event_digest(value: &Value) -> Result<String> {
    Ok(digest(&serde_json::to_vec(value)?))
}

pub fn validate_receipt_events(
    receipt: &ImportAuthorization,
    values: &[(String, Value)],
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for grant in &receipt.events {
        if !seen.insert(&grant.event_id) {
            bail!("conflicting receipt identity {}", grant.event_id);
        }
        let value = values
            .iter()
            .find(|(scope, value)| {
                scope == &grant.scope && value["event_id"].as_str() == Some(&grant.event_id)
            })
            .map(|(_, value)| value)
            .with_context(|| {
                format!(
                    "receipt event {} has no matching scope {}",
                    grant.event_id, grant.scope
                )
            })?;
        if value["repository_id"].as_str() != Some(grant.origin_replica_id.as_str())
            || canonical_event_digest(value)? != grant.event_sha256
        {
            bail!(
                "receipt content digest or origin mismatch for {}",
                grant.event_id
            );
        }
    }
    Ok(())
}

/// Admission cannot introduce an ID already held with another scope or content.
pub fn validate_import_identities(
    store: &crate::store::Store,
    values: &[(String, Value)],
) -> Result<()> {
    let mut held = std::collections::BTreeMap::<String, BTreeSet<(String, String)>>::new();
    for change in store.list_change_ids()? {
        for (id, value) in store.raw_events(&change)? {
            held.entry(id)
                .or_default()
                .insert(("change".into(), canonical_event_digest(&value)?));
        }
    }
    for (id, value) in store.raw_repository_events_unseen(&BTreeSet::new())? {
        held.entry(id)
            .or_default()
            .insert(("repository".into(), canonical_event_digest(&value)?));
    }
    for (scope, value) in values {
        let id = value["event_id"]
            .as_str()
            .context("import event has no ID")?;
        let identity = (scope.clone(), canonical_event_digest(value)?);
        let identities = held.entry(id.into()).or_default();
        if identities.iter().any(|existing| existing != &identity) {
            bail!("conflicting event identity {id}: scope or canonical content differs");
        }
        identities.insert(identity);
    }
    Ok(())
}

/// Receiver permissions remain separate from the recorded verdict graph.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PrivilegeScope {
    pub local_origin: String,
    pub authority_holder: Option<String>,
    pub authority_grant_holders: std::collections::BTreeMap<String, String>,
    pub origin_names: std::collections::BTreeMap<String, String>,
    pub trusted_review_replicas: Vec<String>,
    pub admissions: std::collections::BTreeMap<String, EventAuthorization>,
    pub imported_event_ids: BTreeSet<String>,
    pub bound_events: std::collections::BTreeMap<String, (String, String, String)>,
    pub ambiguous_event_ids: BTreeSet<String>,
}
impl PrivilegeScope {
    pub fn for_store(store: &crate::store::Store) -> Result<Self> {
        let policy = match &store.operator_policy_path {
            Some(path) => crate::policy::replica_at(path)?,
            None => ReplicaPolicy::default(),
        };
        let mut admissions = std::collections::BTreeMap::new();
        let mut imported_event_ids = BTreeSet::new();
        for batch in crate::store::import_batches(&store.root)? {
            for value in batch.events.iter().chain(&batch.repository_events) {
                imported_event_ids.insert(
                    value["event_id"]
                        .as_str()
                        .context("imported event has no ID")?
                        .to_string(),
                );
            }
            if let Some(receipt) = batch.authorization {
                for event in receipt.events {
                    if admissions.insert(event.event_id.clone(), event).is_some() {
                        bail!("duplicate event authorization receipt");
                    }
                }
            }
        }
        let mut bound_events = std::collections::BTreeMap::new();
        let mut ambiguous_event_ids = BTreeSet::new();
        let mut bind = |scope: &str, id: String, value: Value| -> Result<()> {
            let identity = (
                scope.to_string(),
                value["repository_id"]
                    .as_str()
                    .context("event has no origin")?
                    .to_string(),
                canonical_event_digest(&value)?,
            );
            if let Some(held) = bound_events.insert(id.clone(), identity.clone()) {
                if held != identity {
                    ambiguous_event_ids.insert(id);
                }
            }
            Ok(())
        };
        if !admissions.is_empty() {
            for change in store.list_change_ids()? {
                if let Ok(values) = store.raw_events(&change) {
                    for (id, value) in values {
                        bind("change", id, value)?;
                    }
                }
            }
            if let Ok(values) = store.raw_repository_events_unseen(&BTreeSet::new()) {
                for (id, value) in values {
                    bind("repository", id, value)?;
                }
            }
        }
        admissions.retain(|id, receipt| {
            !ambiguous_event_ids.contains(id)
                && bound_events.get(id).is_some_and(|(scope, origin, hash)| {
                    scope == &receipt.scope
                        && origin == &receipt.origin_replica_id
                        && hash == &receipt.event_sha256
                })
        });
        let replica = crate::replica::status(store)?;
        let authority_holder = replica
            .authority
            .as_ref()
            .and_then(|authority| authority.holder.as_ref())
            .map(|holder| holder.repository_id.clone());
        let origin_names = replica
            .local
            .into_iter()
            .chain(replica.peers)
            .map(|identity| (identity.repository_id, identity.name))
            .collect();
        Ok(Self {
            authority_holder,
            authority_grant_holders: crate::replica::grant_holders(store)?,
            origin_names,
            local_origin: store.repository_id.clone(),
            trusted_review_replicas: policy.trusted_review_replicas,
            admissions,
            imported_event_ids,
            bound_events,
            ambiguous_event_ids,
        })
    }
    pub fn admission_for(&self, event_id: &str) -> Option<&EventAuthorization> {
        if self.ambiguous_event_ids.contains(event_id) {
            return None;
        }
        let receipt = self.admissions.get(event_id)?;
        let (scope, origin, hash) = self.bound_events.get(event_id)?;
        (scope == &receipt.scope
            && origin == &receipt.origin_replica_id
            && hash == &receipt.event_sha256)
            .then_some(receipt)
    }

    pub fn permits(&self, event_id: &str, origin: &str, role: Role) -> bool {
        if self.ambiguous_event_ids.contains(event_id) {
            return false;
        }
        let capability = match self.admission_for(event_id) {
            Some(receipt) => {
                receipt.origin_replica_id == origin && receipt.authorized_roles.contains(&role)
            }
            None => origin == self.local_origin && !self.imported_event_ids.contains(event_id),
        };
        capability
            && (role != Role::ReviewAuthority
                || origin == self.local_origin
                || self
                    .trusted_review_replicas
                    .iter()
                    .any(|trusted| trusted == origin))
    }
    pub fn provenance(&self, event_id: &str) -> &'static str {
        if self.admission_for(event_id).is_some() {
            "transport-asserted"
        } else {
            "legacy-unknown"
        }
    }
    pub fn rejection(&self, event_id: &str, origin: &str) -> Option<String> {
        if self.permits(event_id, origin, Role::ReviewAuthority) {
            None
        } else if self
            .admission_for(event_id)
            .is_none_or(|event| !event.authorized_roles.contains(&Role::ReviewAuthority))
        {
            Some(format!(
                "origin {origin} has no admitted review-authority; local verdict owed"
            ))
        } else {
            Some(format!(
                "origin {origin} is not trusted for review; local verdict owed"
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedVerdictProvenance {
    pub origin_replica_id: String,
    pub admission_provenance: String,
    pub receipt: Option<EventAuthorization>,
}

/// Every replica payload is an authority assertion, including membership.
pub fn required_replica_roles(payload: &crate::replica::ReplicaPayload) -> Vec<Vec<Role>> {
    use crate::replica::ReplicaPayload::*;
    match payload {
        Initialized
        | Paired { .. }
        | AuthorityOffered { .. }
        | AuthorityAcquired { .. }
        | AuthorityReclaimed { .. }
        | AuthorityReclaimRequested { .. }
        | AuthorityReturnConfirmed { .. } => vec![vec![Role::Authority]],
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportSummary {
    pub introduced_event_ids: Vec<String>,
    pub authorization: Option<ImportAuthorization>,
}
pub type ImportResult = crate::model::OperationResult<ImportSummary>;
