//! Non-authorizing delivery, disclosure and comment-act evidence.
use crate::forge_observation::RetainedContentRef;
use crate::model::{Event, Payload};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A single-line value naming a participant: no placeholder, no session id,
/// and no `harness:model` form carrying either. Absence is `None`, never a
/// placeholder.
fn participant(value: &str) -> Result<()> {
    named(value)?;
    ensure!(
        !crate::coordinate::is_session_id(value, None)
            && crate::coordinate::actor_defect(value).is_none(),
        "a placeholder or session id names no participant"
    );
    Ok(())
}

/// A single-line coordinate that is no placeholder.
fn established(value: &str) -> Result<()> {
    named(value)?;
    ensure!(
        !crate::coordinate::is_placeholder(value),
        "a placeholder is not a coordinate"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceGeneration {
    pub change_id: String,
    pub observation: String,
    pub generation: String,
}
impl SourceGeneration {
    pub fn validate(&self) -> Result<()> {
        crate::ids::validate_id_component(&self.change_id)?;
        named(&self.observation)?;
        named(&self.generation)
    }
    pub fn resolve<'a>(&self, events: &'a [Event]) -> Result<&'a RetainedContentRef> {
        self.validate()?;
        let source_history: Vec<_> = events
            .iter()
            .filter(|e| e.change_id == self.change_id)
            .cloned()
            .collect();
        let projected = crate::state::reduce(&source_history)?;
        let resolved =
            crate::forge_observation::find_observation(&projected.forge, &self.observation)?;
        ensure!(
            resolved
                .document
                .generations
                .iter()
                .any(|g| g.generation_id == self.generation),
            "generation is not on the exact observation"
        );
        let observation = events
            .iter()
            .find_map(|e| {
                if e.change_id == self.change_id && e.event_id == resolved.event_id {
                    if let Payload::ForgeObservationRecorded { observation } = &e.payload {
                        return Some(observation);
                    }
                }
                None
            })
            .context("source observation is not recorded on the named change")?;
        let item = observation
            .items
            .iter()
            .find(|item| item.generation_id == self.generation)
            .context("source generation is not present in the exact observation")?;
        Ok(&item.content)
    }
}
fn named(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && !value.contains(['\n', '\r']),
        "expected a nonempty single-line value"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservationStanding {
    Observed,
    Uncertain,
    NotObserved,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeObservation {
    pub standing: ObservationStanding,
    pub detail: String,
    #[serde(default)]
    pub evidence: Vec<RetainedContentRef>,
    #[serde(default)]
    pub sources: Vec<SourceGeneration>,
}
impl OutcomeObservation {
    fn validate(&self) -> Result<()> {
        named(&self.detail)?;
        for reference in &self.evidence {
            reference.validate()?;
        }
        for source in &self.sources {
            source.validate()?;
        }
        ensure!(
            self.standing != ObservationStanding::Observed
                || !self.evidence.is_empty()
                || !self.sources.is_empty(),
            "an observed outcome needs evidence; use uncertain or not-observed otherwise"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeDelivery {
    pub schema: String,
    pub message_id: String,
    pub attempt_id: String,
    /// A separately identified observation of this attempt's outcome.
    pub evidence_id: String,
    #[serde(default)]
    pub previous_evidence: Option<String>,
    pub run: String,
    pub transport_request: String,
    pub sources: Vec<SourceGeneration>,
    pub delivery: OutcomeObservation,
    pub acknowledgement: OutcomeObservation,
    pub acted_on: OutcomeObservation,
}
impl ForgeDelivery {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == "arc-forge-delivery/1",
            "unsupported delivery schema"
        );
        for name in [
            &self.message_id,
            &self.attempt_id,
            &self.evidence_id,
            &self.run,
            &self.transport_request,
        ] {
            named(name)?;
        }
        if let Some(previous) = &self.previous_evidence {
            named(previous)?;
            ensure!(
                previous != &self.evidence_id,
                "outcome evidence cannot name itself"
            );
        }
        for source in &self.sources {
            source.validate()?;
        }
        self.delivery.validate()?;
        self.acknowledgement.validate()?;
        self.acted_on.validate()
    }
    pub fn sources(&self) -> impl Iterator<Item = &SourceGeneration> {
        self.sources
            .iter()
            .chain(&self.delivery.sources)
            .chain(&self.acknowledgement.sources)
            .chain(&self.acted_on.sources)
    }
    pub fn contents(&self) -> impl Iterator<Item = &RetainedContentRef> {
        self.delivery
            .evidence
            .iter()
            .chain(&self.acknowledgement.evidence)
            .chain(&self.acted_on.evidence)
    }
    pub fn same_attempt(&self, other: &Self) -> bool {
        self.message_id == other.message_id
            && self.attempt_id == other.attempt_id
            && self.run == other.run
            && self.transport_request == other.transport_request
            && self.sources == other.sources
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentAuthor {
    pub name: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Coordinates of the original material authorship act, independent of publication.
    pub act: String,
    pub model_observation: Option<String>,
}
impl CommentAuthor {
    fn validate(&self) -> Result<()> {
        established(&self.act)?;
        ensure!(
            self.name.is_none()
                || (self.harness.is_none() && self.model.is_none() && self.effort.is_none()),
            "human and agent authorship coordinates are separate"
        );
        for value in [
            &self.name,
            &self.harness,
            &self.model,
            &self.effort,
            &self.model_observation,
        ]
        .into_iter()
        .flatten()
        {
            named(value)?;
        }
        if let Some(effort) = &self.effort {
            ensure!(
                !crate::coordinate::is_placeholder(effort)
                    && effort
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "invalid authorship effort"
            );
        }
        if let Some(observation) = &self.model_observation {
            established(observation)?;
        }
        if let Some(name) = &self.name {
            participant(name)?;
            ensure!(
                !name.contains(['/', '\\', ':', '@']),
                "human author needs a name, not a trace or URL"
            );
        }
        if let Some(harness) = &self.harness {
            ensure!(
                !crate::coordinate::is_placeholder(harness)
                    && harness
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "invalid authorship harness"
            );
        }
        if let Some(model) = &self.model {
            ensure!(
                crate::coordinate::model_defect(model, None).is_none()
                    && !model.contains(['#', ':'])
                    && !model.starts_with('/'),
                "invalid authorship model"
            );
            if let Some(effort) = &self.effort {
                ensure!(
                    !crate::coordinate::is_placeholder(effort)
                        && effort
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                    "invalid authorship effort"
                );
            }
            if let Some(harness) = &self.harness {
                let coordinate = format!(
                    "{harness}:{model}{}",
                    self.effort
                        .as_ref()
                        .map(|e| format!("#{e}"))
                        .unwrap_or_default()
                );
                ensure!(
                    crate::coordinate::coordinate_defect(&coordinate, None).is_none(),
                    "invalid authorship coordinate"
                );
            }
        }
        Ok(())
    }
    fn coordinate(&self) -> String {
        if self.validate().is_err() {
            return "model not recorded".into();
        }
        if let Some(name) = &self.name {
            return name.clone();
        }
        match (&self.harness, &self.model) {
            (Some(harness), Some(model)) if self.model_observation.is_some() => format!(
                "{harness}:{model}{}",
                self.effort
                    .as_ref()
                    .map(|e| format!("#{e}"))
                    .unwrap_or_default()
            ),
            _ => "model not recorded".into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostReference {
    pub reference: String,
    pub as_of: String,
    pub scope: String,
    pub basis: String,
    pub coverage: String,
    pub source_frontier: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleAttribution {
    pub role: String,
    pub participant: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentProvenance {
    pub comment_id: String,
    pub act_id: String,
    pub authors: Vec<CommentAuthor>,
    #[serde(default)]
    pub source_frontiers: Vec<String>,
    #[serde(default)]
    pub source_statements: Vec<String>,
    pub renderer_version: String,
    #[serde(default)]
    pub roles: Vec<RoleAttribution>,
    #[serde(default)]
    pub cost: Option<CostReference>,
}
impl CommentProvenance {
    fn validate(&self) -> Result<()> {
        named(&self.comment_id)?;
        named(&self.act_id)?;
        named(&self.renderer_version)?;
        for author in &self.authors {
            author.validate()?;
        }
        ensure!(
            self.compact_line().chars().count() <= 512,
            "compact provenance exceeds 512 characters; use separate comments"
        );
        let mut authors = BTreeSet::new();
        for author in &self.authors {
            ensure!(
                authors.insert((&author.act, author.coordinate())),
                "duplicate material author"
            );
        }
        ensure!(
            self.authors.len() < 2 || !self.source_statements.is_empty(),
            "material synthesis needs source statements"
        );
        for value in self.source_frontiers.iter().chain(&self.source_statements) {
            named(value)?;
        }
        for role in &self.roles {
            named(&role.role)?;
            participant(&role.participant)?;
        }
        if let Some(cost) = &self.cost {
            ensure!(cost.scope == "review-exchange", "comment cost must be scoped to review-exchange, never implementation or whole-session");
            for value in [&cost.reference, &cost.as_of, &cost.basis, &cost.coverage] {
                named(value)?;
            }
            ensure!(
                !cost.source_frontier.is_empty(),
                "cost reference needs its as-of source frontier"
            );
            for value in &cost.source_frontier {
                named(value)?;
            }
        }
        Ok(())
    }
    pub fn compact_line(&self) -> String {
        let authors = if self.authors.is_empty() {
            "model not recorded".into()
        } else {
            self.authors
                .iter()
                .map(CommentAuthor::coordinate)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let cost = match &self.cost {
            Some(cost) => format!("{} as of {}", cost.basis, cost.as_of),
            None => "unmeasured".into(),
        };
        format!(
            "{}: {authors} · Review cost: {cost}.",
            if self.authors.len() > 1 {
                "Authors"
            } else {
                "Author"
            }
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccessBasis {
    AccessibleGit,
    AccessibleForge,
    WithheldPrivateAssertion,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DisclosureSource {
    Forge(SourceGeneration),
    Ledger(LedgerSource),
    Git(GitSource),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerSource {
    pub change_id: String,
    pub event: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSource {
    pub change_id: String,
    pub commit: String,
}
impl DisclosureSource {
    fn change_id(&self) -> &str {
        match self {
            Self::Forge(s) => &s.change_id,
            Self::Ledger(s) => &s.change_id,
            Self::Git(s) => &s.change_id,
        }
    }
    fn validate(&self) -> Result<()> {
        crate::ids::validate_id_component(self.change_id())?;
        match self {
            Self::Forge(s) => s.validate(),
            Self::Ledger(s) => named(&s.event),
            Self::Git(s) => {
                ensure!(
                    crate::gitio::is_full_object_id(&s.commit),
                    "Git disclosure source needs a full lowercase object ID"
                );
                Ok(())
            }
        }
    }
    fn resolve(&self, events: &[Event]) -> Result<()> {
        self.validate()?;
        match self {
            Self::Forge(s) => {
                s.resolve(events)?;
            }
            Self::Ledger(s) => {
                ensure!(
                    events
                        .iter()
                        .any(|e| e.change_id == s.change_id && e.event_id == s.event),
                    "selected ledger source is not recorded"
                );
            }
            Self::Git(_) => {}
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosureSelection {
    pub source: DisclosureSource,
    pub fields: Vec<String>,
    pub access_basis: AccessBasis,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disclosure {
    pub schema: String,
    pub disclosure_id: String,
    #[serde(default)]
    pub previous_disclosure: Option<String>,
    pub selected: Vec<DisclosureSelection>,
    pub allowlist_version: String,
    pub validator_version: String,
    pub author: String,
    pub reviewer: Option<String>,
    pub editor: Option<String>,
    pub discloser: String,
    pub recipient_access_basis: String,
    pub omissions: Vec<String>,
    pub outgoing: RetainedContentRef,
    pub readback: SourceGeneration,
    pub comment: Option<CommentProvenance>,
}
impl Disclosure {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == "arc-disclosure/1",
            "unsupported disclosure schema"
        );
        for value in [
            &self.disclosure_id,
            &self.allowlist_version,
            &self.validator_version,
            &self.recipient_access_basis,
        ] {
            named(value)?;
        }
        for value in [&self.author, &self.discloser] {
            participant(value)?;
        }
        for value in [&self.reviewer, &self.editor].into_iter().flatten() {
            participant(value)?;
        }
        if let Some(previous) = &self.previous_disclosure {
            named(previous)?;
        }
        for selection in &self.selected {
            selection.source.validate()?;
            ensure!(
                !selection.fields.is_empty(),
                "disclosure selection needs explicit fields"
            );
            for field in &selection.fields {
                named(field)?;
            }
        }
        for omission in &self.omissions {
            named(omission)?;
        }
        self.outgoing.validate()?;
        self.readback.validate()?;
        if let Some(comment) = &self.comment {
            comment.validate()?;
        }
        Ok(())
    }
    pub fn sources(&self) -> impl Iterator<Item = &SourceGeneration> {
        self.selected
            .iter()
            .filter_map(|selection| {
                if let DisclosureSource::Forge(source) = &selection.source {
                    Some(source)
                } else {
                    None
                }
            })
            .chain(std::iter::once(&self.readback))
    }
    pub fn validate_readback(&self, events: &[Event]) -> Result<()> {
        self.validate()?;
        for selection in &self.selected {
            selection.source.resolve(events)?;
        }
        let readback = self.readback.resolve(events)?;
        ensure!(
            self.outgoing.sha256 == readback.sha256
                && self.outgoing.byte_length == readback.byte_length,
            "outgoing bytes disagree with the exact readback generation"
        );
        Ok(())
    }
}

/// Validate foreign delivery/disclosure shapes and their immutable lineage before publication.
pub fn validate_history(events: &[Event], introduced: &BTreeSet<&String>) -> Result<()> {
    let deliveries: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::Message {
                delivery: Some(delivery),
                ..
            } => Some(delivery),
            _ => None,
        })
        .collect();
    let disclosures: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::ContextDeclared {
                disclosure: Some(disclosure),
                ..
            } => Some(disclosure),
            _ => None,
        })
        .collect();
    for event in events {
        if !introduced.contains(&event.event_id) {
            continue;
        }
        if let Payload::ContextDeclared {
            origin: Some(origin),
            ..
        } = &event.payload
        {
            origin.resolve(events)?;
        }
        match &event.payload {
            Payload::Message {
                delivery: Some(delivery),
                ..
            } => {
                delivery.validate()?;
                for source in delivery.sources() {
                    source.resolve(events)?;
                }
                for old in &deliveries {
                    if old.attempt_id == delivery.attempt_id {
                        ensure!(
                            old.same_attempt(delivery),
                            "conflicting delivery attempt reuse"
                        );
                        if old.evidence_id == delivery.evidence_id {
                            ensure!(*old == delivery, "conflicting delivery evidence reuse");
                        }
                    }
                    ensure!(
                        old.transport_request != delivery.transport_request
                            || old.same_attempt(delivery),
                        "transport request reused for another attempt"
                    );
                }
                if let Some(previous) = &delivery.previous_evidence {
                    ensure!(
                        deliveries
                            .iter()
                            .any(|old| old.same_attempt(delivery) && &old.evidence_id == previous),
                        "unknown prior outcome evidence"
                    );
                } else {
                    ensure!(
                        !deliveries
                            .iter()
                            .any(|old| old.attempt_id == delivery.attempt_id
                                && old.evidence_id != delivery.evidence_id
                                && old.previous_evidence.is_none()),
                        "later outcomes need previous_evidence"
                    );
                }
                let mut seen = BTreeSet::new();
                let mut cursor = Some(delivery);
                while let Some(current) = cursor {
                    ensure!(
                        seen.insert(&current.evidence_id),
                        "cyclic delivery outcome evidence"
                    );
                    cursor = current.previous_evidence.as_ref().and_then(|previous| {
                        deliveries
                            .iter()
                            .copied()
                            .find(|old| old.same_attempt(delivery) && &old.evidence_id == previous)
                    });
                }
            }
            Payload::ContextDeclared {
                disclosure: Some(disclosure),
                ..
            } => {
                disclosure.validate_readback(events)?;
                if let Some(comment) = &disclosure.comment {
                    for old in &disclosures {
                        if let Some(original) = &old.comment {
                            if original.comment_id == comment.comment_id {
                                ensure!(
                                    original.authors == comment.authors,
                                    "comment identity cannot change original material authors"
                                );
                                ensure!(
                                    disclosure.previous_disclosure.is_some()
                                        || old.previous_disclosure.is_some()
                                        || old.disclosure_id == disclosure.disclosure_id,
                                    "later comment annotations need previous_disclosure"
                                );
                            }
                        }
                    }
                }
                if let Some(previous) = &disclosure.previous_disclosure {
                    let old = disclosures
                        .iter()
                        .find(|old| &old.disclosure_id == previous)
                        .context("unknown prior disclosure")?;
                    ensure!(
                        old.comment.is_none() || disclosure.comment.is_some(),
                        "correction cannot drop original comment provenance"
                    );
                    if let (Some(before), Some(after)) = (&old.comment, &disclosure.comment) {
                        ensure!(
                            before.comment_id == after.comment_id
                                && before.authors == after.authors,
                            "correction must retain original material authors"
                        );
                    }
                    ensure!(
                        disclosure.editor.is_some(),
                        "correction needs distinct editor attribution"
                    );
                }
                ensure!(
                    !disclosures
                        .iter()
                        .any(|old| old.disclosure_id == disclosure.disclosure_id
                            && *old != disclosure),
                    "conflicting disclosure identity reuse"
                );
                let mut seen = BTreeSet::new();
                let mut cursor = Some(disclosure);
                while let Some(current) = cursor {
                    ensure!(
                        seen.insert(&current.disclosure_id),
                        "cyclic disclosure edit lineage"
                    );
                    cursor = current.previous_disclosure.as_ref().and_then(|previous| {
                        disclosures
                            .iter()
                            .copied()
                            .find(|old| &old.disclosure_id == previous)
                    });
                }
            }
            Payload::ContextDeclared {
                origin: Some(source),
                ..
            } => {
                source.resolve(events)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Held histories, including repository-scoped candidate context.
pub fn held_events(store: &crate::store::Store) -> Result<Vec<Event>> {
    let mut events = store.load_repository_events()?;
    for change in store.list_change_ids()? {
        events.extend(store.load_events(&change)?);
    }
    events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    Ok(events)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RoundTrip {
    pub disclosures: Vec<Disclosure>,
    pub origins: Vec<SourceGeneration>,
    pub candidate_paths: Vec<CandidateOutcome>,
    pub local_integration_revision: Option<String>,
    pub accepting_patchset: Option<String>,
    pub provider_lifecycle: Option<String>,
    pub observed_merge_sha: Option<String>,
    pub disclosure_availability: Vec<DisclosureAvailability>,
}
#[derive(Debug, Clone, Serialize)]
pub struct DisclosureAvailability {
    pub disclosure_id: String,
    pub outgoing_bytes: String,
    pub compact_provenance: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct CandidateOutcome {
    pub candidate_id: String,
    pub parents: Vec<String>,
    pub adopts: Vec<String>,
    pub origin_change: String,
    pub origin_sources: Vec<SourceGeneration>,
    pub selection: String,
    pub promotion: Option<String>,
    pub patchset: Option<String>,
    pub revision: Option<String>,
}
impl RoundTrip {
    pub fn observe(&mut self, payload: &Payload) {
        if let Payload::ContextDeclared {
            disclosure, origin, ..
        } = payload
        {
            self.disclosures.extend(disclosure.clone());
            self.origins.extend(origin.clone());
        }
    }
    pub fn join(
        &mut self,
        state: &crate::state::ChangeState,
        repository: &[Event],
        custody: &crate::custody::Custody,
    ) {
        self.disclosure_availability = self
            .disclosures
            .iter()
            .map(|d| DisclosureAvailability {
                disclosure_id: d.disclosure_id.clone(),
                outgoing_bytes: custody
                    .availability(&crate::custody::Content::from(&d.outgoing))
                    .label()
                    .into(),
                compact_provenance: d.comment.as_ref().map(CommentProvenance::compact_line),
            })
            .collect();
        self.local_integration_revision = state
            .closure
            .as_ref()
            .and_then(|c| c.integrated_commit.clone());
        self.accepting_patchset = state
            .closure
            .as_ref()
            .and_then(|c| c.source_patchset_id.clone());
        if let Some(lifecycle) = state.forge.current_pr_state() {
            self.provider_lifecycle = Some(match lifecycle.state {
                crate::forge::ForgePrState::Closed => "closed-unmerged".into(),
                other => other.as_str().into(),
            });
            self.observed_merge_sha = lifecycle.observed_merge_sha.clone();
        }
        self.candidate_paths.clear();
        for event in repository {
            let Payload::CandidateSelected {
                candidate_id,
                destination,
                ..
            } = &event.payload
            else {
                continue;
            };
            if destination != &state.change_id {
                continue;
            }
            let promotion = repository.iter().find(|e| matches!(&e.payload, Payload::CandidatePromoted { selection, .. } if selection == &event.event_id));
            let mut pending = vec![candidate_id.clone()];
            let mut seen = BTreeSet::new();
            while let Some(id) = pending.pop() {
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Some((parents, adopts, brief)) = repository.iter().find_map(|e| {
                    if let Payload::CandidateRegistered {
                        candidate_id,
                        parents,
                        adopts,
                        brief,
                        ..
                    } = &e.payload
                    {
                        if candidate_id == &id {
                            return Some((parents, adopts, brief));
                        }
                    }
                    None
                }) else {
                    continue;
                };
                pending.extend(parents.iter().chain(adopts).cloned());
                let (patchset, revision) = promotion
                    .and_then(|e| {
                        if let Payload::CandidatePromoted {
                            patchset_id,
                            revision,
                            ..
                        } = &e.payload
                        {
                            Some((Some(patchset_id.clone()), Some(revision.clone())))
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                self.candidate_paths.push(CandidateOutcome {
                    candidate_id: id.clone(),
                    parents: parents.clone(),
                    adopts: adopts.clone(),
                    origin_change: brief.change_id.clone(),
                    origin_sources: repository
                        .iter()
                        .filter_map(|event| match &event.payload {
                            Payload::ContextDeclared {
                                subject: crate::model::RelationSubject::Candidate { candidate_id },
                                origin: Some(origin),
                                ..
                            } if candidate_id == &id => Some(origin.clone()),
                            _ => None,
                        })
                        .collect(),
                    selection: event.event_id.clone(),
                    promotion: promotion.map(|e| e.event_id.clone()),
                    patchset,
                    revision,
                });
            }
        }
    }
}

/// Change references needed to reconstruct a context relationship.
pub fn source_changes(payload: &Payload) -> BTreeSet<String> {
    let mut changes = BTreeSet::new();
    match payload {
        Payload::Message {
            delivery: Some(delivery),
            ..
        } => changes.extend(delivery.sources().map(|s| s.change_id.clone())),
        Payload::ContextDeclared {
            disclosure, origin, ..
        } => {
            if let Some(disclosure) = disclosure {
                changes.extend(disclosure.sources().map(|s| s.change_id.clone()));
                changes.extend(
                    disclosure
                        .selected
                        .iter()
                        .map(|s| s.source.change_id().to_string()),
                );
            }
            if let Some(origin) = origin {
                changes.insert(origin.change_id.clone());
            }
        }
        _ => {}
    }
    changes.remove(crate::store::Store::REPOSITORY_SCOPE);
    changes
}

/// Full Git support references remain separate from outgoing text custody.
pub fn disclosure_commits(payload: &Payload) -> Vec<&str> {
    match payload {
        Payload::ContextDeclared {
            disclosure: Some(disclosure),
            ..
        } => disclosure
            .selected
            .iter()
            .filter_map(|s| match &s.source {
                DisclosureSource::Git(s) => Some(s.commit.as_str()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether a local workflow carries forge-related records without a provider declaration.
/// Whether a change takes part in a forge round trip: it has the forge
/// profile, recorded forge facts, or typed local round-trip records. Only
/// such a change carries a forge view or a round-trip section.
pub fn applies(state: &crate::state::ChangeState) -> bool {
    state.profile == "forge" || !state.forge.is_empty() || has_records(state)
}

pub fn has_records(state: &crate::state::ChangeState) -> bool {
    !state.round_trip.disclosures.is_empty()
        || !state.round_trip.origins.is_empty()
        || !state.round_trip.candidate_paths.is_empty()
        || state.messages.iter().any(|m| m.delivery.is_some())
}

pub fn related_to(state: &crate::state::ChangeState) -> BTreeSet<String> {
    let mut changes = BTreeSet::new();
    if let Some(successor) = state
        .closure
        .as_ref()
        .and_then(|c| c.superseded_by.as_ref())
    {
        changes.insert(successor.clone());
    }
    changes.extend(state.round_trip.origins.iter().map(|s| s.change_id.clone()));
    changes.extend(
        state
            .round_trip
            .candidate_paths
            .iter()
            .map(|p| p.origin_change.clone()),
    );
    for disclosure in &state.round_trip.disclosures {
        changes.extend(disclosure.sources().map(|s| s.change_id.clone()));
        changes.extend(
            disclosure
                .selected
                .iter()
                .map(|s| s.source.change_id().to_string()),
        );
    }
    for delivery in state.messages.iter().filter_map(|m| m.delivery.as_ref()) {
        changes.extend(delivery.sources().map(|s| s.change_id.clone()));
    }
    changes.remove(crate::store::Store::REPOSITORY_SCOPE);
    changes
}

/// The exact source discriminator and current bytes, separate from the recorded claim.
pub fn source_view(
    source: &SourceGeneration,
    events: &[Event],
    custody: &crate::custody::Custody,
) -> serde_json::Value {
    let history: Vec<_> = events
        .iter()
        .filter(|e| e.change_id == source.change_id)
        .cloned()
        .collect();
    let projected = crate::state::reduce(&history);
    let record = projected.as_ref().ok().and_then(|state| {
        crate::forge_observation::find_observation(&state.forge, &source.observation).ok()
    });
    match (record, source.resolve(events)) {
        (Some(record), Ok(content)) => {
            serde_json::json!({"reference":source,"source_status":"valid","canonical_observation_id":record.event_id,"aliases":record.aliases,"content":content,"availability":custody.availability(&content.into()).label()})
        }
        (_, Err(error)) => {
            let conflict = projected.as_ref().ok().is_some_and(|state| {
                state.forge.observations.iter().any(|r| {
                    (r.names_event(&source.observation) || r.document.read_id == source.observation)
                        && r.conflict
                })
            });
            serde_json::json!({"reference":source,"source_status":if conflict {"conflict"} else {"invalid-or-unresolved"},"availability":"unavailable","diagnostic":error.to_string()})
        }
        _ => unreachable!("resolution and the shared source selector agree"),
    }
}

/// Explicit predecessor records can live on a different change from their source observation.
pub fn predecessor_changes(payload: &Payload, history: &[Event]) -> BTreeSet<String> {
    let mut changes = BTreeSet::new();
    for event in history {
        let references = match (payload, &event.payload) {
            (
                Payload::Message {
                    delivery: Some(current),
                    ..
                },
                Payload::Message {
                    delivery: Some(previous),
                    ..
                },
            ) => {
                current.previous_evidence.as_ref() == Some(&previous.evidence_id)
                    && current.same_attempt(previous)
            }
            (
                Payload::ContextDeclared {
                    disclosure: Some(current),
                    ..
                },
                Payload::ContextDeclared {
                    disclosure: Some(previous),
                    ..
                },
            ) => current.previous_disclosure.as_ref() == Some(&previous.disclosure_id),
            _ => false,
        };
        if references && event.change_id != crate::store::Store::REPOSITORY_SCOPE {
            changes.insert(event.change_id.clone());
        }
    }
    changes
}

pub fn has_predecessor(payload: &Payload) -> bool {
    matches!(
        payload,
        Payload::Message {
            delivery: Some(ForgeDelivery {
                previous_evidence: Some(_),
                ..
            }),
            ..
        } | Payload::ContextDeclared {
            disclosure: Some(Disclosure {
                previous_disclosure: Some(_),
                ..
            }),
            ..
        }
    )
}
