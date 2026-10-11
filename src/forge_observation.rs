//! Adapter-reported forge reads and content-generation claims. References do
//! not establish that bytes are present or verified in Arc's custody.
use anyhow::{bail, ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedContentRef {
    pub provider: String,
    pub handle: String,
    pub sha256: String,
    pub byte_length: u64,
    pub media_type: String,
    pub capture: crate::model::CaptureState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderIdentity {
    pub provider: String,
    pub host: String,
    pub repository: String,
    pub repository_id: Option<String>,
    pub pr_number: u64,
    pub native_id: Option<String>,
    pub url: String,
    pub base_repo: String,
    pub base_ref: String,
    pub head_repo: String,
    pub head_ref: String,
}
impl ProviderIdentity {
    /// Immutable provider/PR coordinates, with names and numbers as fallback
    /// only when both reads lack the corresponding native identifier.
    pub fn same_pr_identity_as(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.host == other.host
            && match (&self.repository_id, &other.repository_id) {
                (Some(left), Some(right)) => left == right,
                (None, None) => self.repository == other.repository,
                _ => false,
            }
            && match (&self.native_id, &other.native_id) {
                (Some(left), Some(right)) => left == right,
                (None, None) => self.pr_number == other.pr_number,
                _ => false,
            }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CoverageState {
    Complete,
    Truncated,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub state: CoverageState,
    pub scope: String,
    pub frontier: Option<String>,
    pub failure_reason: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadCoverage {
    pub overall: Coverage,
    pub collections: BTreeMap<String, Coverage>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ItemFacts {
    Comment,
    PullRequest {
        state: crate::forge::ForgePrState,
        merge_sha: Option<String>,
    },
    Checks {
        state: crate::forge::ForgeCheckState,
        count: u64,
    },
    Absence {
        collection: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub response_id: String,
    pub sha256: String,
    pub byte_length: u64,
    pub content: RetainedContentRef,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub kind: String,
    pub node_id: String,
    pub generation_id: String,
    pub sha256: String,
    pub byte_length: u64,
    pub content: RetainedContentRef,
    pub facts: ItemFacts,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub generation_id: String,
    pub predecessor: Option<String>,
    pub kind: String,
    pub node_id: String,
    pub sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeObservation {
    pub schema: String,
    pub read_id: String,
    pub adapter: String,
    pub provider_identity: ProviderIdentity,
    pub observed_head_sha: Option<String>,
    pub observed_base_sha: Option<String>,
    pub read_at: DateTime<Utc>,
    pub provider_updated_at: Option<DateTime<Utc>>,
    pub provider_event_id: Option<String>,
    pub coverage: ReadCoverage,
    pub responses: Vec<Response>,
    pub items: Vec<Item>,
    pub generations: Vec<Generation>,
    pub previous_observation: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservationRecord {
    pub aliases: Vec<String>,
    pub conflict: bool,
    pub invalid: Option<String>,
    pub event_id: String,
    pub created_at: DateTime<Utc>,
    pub document: ForgeObservation,
}
impl ObservationRecord {
    pub fn usable(&self) -> bool {
        !self.conflict && self.invalid.is_none()
    }
    pub fn names_event(&self, id: &str) -> bool {
        self.event_id == id || self.aliases.iter().any(|alias| alias == id)
    }
    pub fn names_prefix(&self, prefix: &str) -> bool {
        self.event_id.starts_with(prefix)
            || self.aliases.iter().any(|alias| alias.starts_with(prefix))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeDiagnostic {
    pub event_id: String,
    pub kind: &'static str,
    pub message: String,
}
fn nonblank(s: &str) -> Result<()> {
    ensure!(!s.trim().is_empty(), "blank forge identifier or scope");
    Ok(())
}
fn hex(s: &str, widths: &[usize]) -> Result<()> {
    ensure!(
        widths.contains(&s.len())
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid full digest or object ID: {s}"
    );
    Ok(())
}
fn reference(content: &RetainedContentRef, digest: &str, length: u64) -> Result<()> {
    hex(digest, &[64])?;
    nonblank(&content.provider)?;
    nonblank(&content.handle)?;
    nonblank(&content.media_type)?;
    ensure!(
        content.sha256 == digest && content.byte_length == length,
        "retention reference digest/length disagreement"
    );
    Ok(())
}
impl ForgeObservation {
    pub fn validate(&self, history: &[ObservationRecord]) -> Result<()> {
        let history: Vec<_> = history.iter().filter(|r| r.usable()).collect();
        ensure!(
            self.schema == "arc-forge-observation/1",
            "unsupported forge observation schema"
        );
        nonblank(&self.read_id)?;
        nonblank(&self.adapter)?;
        let p = &self.provider_identity;
        for s in [
            &p.provider,
            &p.host,
            &p.repository,
            &p.url,
            &p.base_repo,
            &p.base_ref,
            &p.head_repo,
            &p.head_ref,
        ] {
            nonblank(s)?;
        }
        ensure!(p.pr_number > 0, "PR number must be positive");
        for s in [&p.repository_id, &p.native_id, &self.provider_event_id]
            .into_iter()
            .flatten()
        {
            nonblank(s)?;
        }
        for sha in [&self.observed_head_sha, &self.observed_base_sha]
            .into_iter()
            .flatten()
        {
            hex(sha, &[40, 64])?;
        }
        if self.coverage.overall.state != CoverageState::Failed {
            ensure!(
                self.observed_head_sha.is_some() && self.observed_base_sha.is_some(),
                "successful read requires observed head and base"
            );
        }
        for c in std::iter::once(&self.coverage.overall).chain(self.coverage.collections.values()) {
            nonblank(&c.scope)?;
            if c.state == CoverageState::Failed {
                nonblank(c.failure_reason.as_deref().unwrap_or(""))?;
            }
        }
        if self.coverage.overall.state == CoverageState::Complete {
            ensure!(
                self.coverage
                    .collections
                    .values()
                    .all(|c| c.state == CoverageState::Complete),
                "complete overall coverage contradicts collection coverage"
            );
        }
        let previous = if let Some(named) = &self.previous_observation {
            nonblank(named)?;
            let matches: Vec<_> = history
                .iter()
                .filter(|r| r.names_event(named) || r.document.read_id == *named)
                .collect();
            match matches.as_slice() {
                [record] => Some(&record.document),
                _ => bail!("previous observation must name one recorded read"),
            }
        } else {
            None
        };
        let mut ids = BTreeSet::new();
        for r in &self.responses {
            nonblank(&r.response_id)?;
            ensure!(ids.insert(&r.response_id), "duplicate response ID");
            reference(&r.content, &r.sha256, r.byte_length)?;
        }
        let prior: BTreeMap<&str, (&Generation, &ProviderIdentity)> = history
            .iter()
            .flat_map(|r| {
                r.document
                    .generations
                    .iter()
                    .map(move |g| (g.generation_id.as_str(), (g, &r.document.provider_identity)))
            })
            .collect();
        let mut gens = BTreeMap::new();
        for g in &self.generations {
            nonblank(&g.generation_id)?;
            nonblank(&g.kind)?;
            nonblank(&g.node_id)?;
            hex(&g.sha256, &[64])?;
            ensure!(
                gens.insert(g.generation_id.as_str(), g).is_none(),
                "duplicate generation ID"
            );
            if let Some((old, identity)) = prior.get(g.generation_id.as_str()) {
                ensure!(
                    *old == g && identity.same_pr_identity_as(p),
                    "generation ID conflicts with recorded content"
                );
            }
        }
        for g in &self.generations {
            if prior.contains_key(g.generation_id.as_str()) {
                continue;
            }
            if let Some(pred) = &g.predecessor {
                let Some((old, identity)) = prior.get(pred.as_str()) else {
                    bail!("generation predecessor is not recorded");
                };
                ensure!(
                    identity.same_pr_identity_as(p)
                        && old.kind == g.kind
                        && old.node_id == g.node_id,
                    "generation predecessor names another item or PR"
                );
                ensure!(
                    old.sha256 != g.sha256,
                    "unchanged bytes must reuse the existing generation"
                );
            } else {
                ensure!(
                    !prior
                        .values()
                        .any(|(old, identity)| identity.same_pr_identity_as(p)
                            && old.kind == g.kind
                            && old.node_id == g.node_id),
                    "changed item requires an explicit predecessor"
                );
            }
        }
        let mut nodes = BTreeSet::new();
        for item in &self.items {
            ensure!(
                nodes.insert((&item.kind, &item.node_id)),
                "duplicate item identity"
            );
            reference(&item.content, &item.sha256, item.byte_length)?;
            let g = gens
                .get(item.generation_id.as_str())
                .ok_or_else(|| anyhow::anyhow!("item generation missing"))?;
            ensure!(
                g.kind == item.kind && g.node_id == item.node_id && g.sha256 == item.sha256,
                "item/generation disagreement"
            );
            for old in history
                .iter()
                .flat_map(|r| &r.document.items)
                .filter(|old| old.generation_id == item.generation_id)
            {
                ensure!(
                    old.byte_length == item.byte_length,
                    "generation byte length conflicts with recorded item"
                );
            }
            if let Some(previous) = previous.filter(|d| d.provider_identity.same_pr_identity_as(p))
            {
                if let Some(old) = previous
                    .items
                    .iter()
                    .find(|old| old.kind == item.kind && old.node_id == item.node_id)
                {
                    if old.sha256 == item.sha256 {
                        ensure!(
                            old.generation_id == item.generation_id,
                            "unchanged bytes must reuse the previous generation"
                        );
                    } else {
                        ensure!(!prior.contains_key(item.generation_id.as_str()) && g.predecessor.as_deref() == Some(old.generation_id.as_str()), "changed bytes or reversion require a new generation naming the previous item");
                    }
                }
            }
            let fact_kind = match item.facts {
                ItemFacts::Comment => "comment",
                ItemFacts::PullRequest { .. } => "pull-request",
                ItemFacts::Checks { .. } => "checks",
                ItemFacts::Absence { .. } => "absence",
            };
            ensure!(
                item.kind == fact_kind,
                "item kind disagrees with typed facts"
            );
            match &item.facts {
                ItemFacts::Checks { state, count } => {
                    ensure!(
                        *state != crate::forge::ForgeCheckState::NotConfigured || *count == 0,
                        "not-configured requires zero checks"
                    );
                    ensure!(
                        *state != crate::forge::ForgeCheckState::Passed || *count > 0,
                        "zero checks cannot pass"
                    );
                    ensure!(
                        *state != crate::forge::ForgeCheckState::Passed || self.complete("checks"),
                        "partial checks cannot pass"
                    );
                    ensure!(
                        *count != 0 || self.complete("checks"),
                        "partial frontier cannot establish zero checks"
                    );
                }
                ItemFacts::Absence { collection } => ensure!(
                    self.complete(collection),
                    "partial frontier cannot establish absence"
                ),
                ItemFacts::PullRequest { state, merge_sha } => {
                    ensure!(
                        (*state == crate::forge::ForgePrState::Merged) == merge_sha.is_some(),
                        "merged state requires exactly one merge SHA"
                    );
                    if let Some(sha) = merge_sha {
                        hex(sha, &[40, 64])?;
                    }
                }
                ItemFacts::Comment => {}
            }
        }
        ensure!(
            gens.len() == self.items.len(),
            "unreferenced generations in read"
        );
        Ok(())
    }
    /// Every retained-content reference the read carries.
    pub fn contents(&self) -> impl Iterator<Item = &RetainedContentRef> {
        self.responses
            .iter()
            .map(|r| &r.content)
            .chain(self.items.iter().map(|i| &i.content))
    }
    pub fn complete(&self, collection: &str) -> bool {
        self.coverage
            .collections
            .get(collection)
            .is_some_and(|c| c.state == CoverageState::Complete)
    }
}

/// Check source-bound projections during local admission.
/// Flag assertions carry no observation binding and retain their legacy rules.
pub fn validate_projection(
    payload: &crate::model::Payload,
    forge: &crate::forge::ForgeState,
) -> Result<()> {
    validate_projection_inner(payload, forge, true)
}
pub fn projection_error(
    payload: &crate::model::Payload,
    forge: &crate::forge::ForgeState,
) -> Option<String> {
    validate_projection_inner(payload, forge, false)
        .err()
        .map(|error| error.to_string())
}
fn validate_projection_inner(
    payload: &crate::model::Payload,
    forge: &crate::forge::ForgeState,
    admission: bool,
) -> Result<()> {
    use crate::model::Payload;
    let source = match payload {
        Payload::ForgeLink { observation, .. }
        | Payload::ForgeChecks { observation, .. }
        | Payload::ForgePrState { observation, .. } => observation,
        _ => return Ok(()),
    };
    let Some(source) = source else {
        return Ok(());
    };
    let record = forge
        .observations
        .iter()
        .find(|r| r.names_event(source))
        .ok_or_else(|| anyhow::anyhow!("projection observation is not recorded"))?;
    ensure!(
        record.usable(),
        "projection observation is conflicting or invalid"
    );
    let d = &record.document;
    let p = &d.provider_identity;
    let head = d
        .observed_head_sha
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("projection observation has no raw head"))?;
    match payload {
        Payload::ForgeLink {
            pr_number,
            url,
            base_repo,
            base_ref,
            head_repo,
            head_ref,
            head_sha,
            ..
        } => {
            ensure!(
                *pr_number == p.pr_number
                    && *url == p.url
                    && *base_repo == p.base_repo
                    && *base_ref == p.base_ref
                    && *head_repo == p.head_repo
                    && *head_ref == p.head_ref
                    && head_sha == head,
                "link values disagree with observation"
            );
            if admission {
                ensure!(
                    forge
                        .projection
                        .as_ref()
                        .is_some_and(|projection| projection.host == p.host),
                    "observation host differs from declaration"
                );
                crate::forge::validate_link(
                    forge.projection.as_ref(),
                    &crate::forge::ForgeTuple {
                        base_repo: base_repo.clone(),
                        base_ref: base_ref.clone(),
                        head_repo: head_repo.clone(),
                        head_ref: head_ref.clone(),
                    },
                )
                .map_err(|e| anyhow::anyhow!("{}", e.message()))?;
            }
        }
        Payload::ForgeChecks {
            link_event_id,
            pr_head,
            state,
            detail,
            ..
        } => {
            let link = if admission {
                validate_current_identity(d, forge)?
            } else {
                let link = forge
                    .links
                    .iter()
                    .find(|link| Some(link.event_id.as_str()) == link_event_id.as_deref())
                    .ok_or_else(|| anyhow::anyhow!("projection link is not recorded"))?;
                validate_link_identity(d, forge, link, false)?;
                link
            };
            ensure!(
                link_event_id.as_deref() == Some(link.event_id.as_str()),
                "checks link binding disagrees with observation"
            );
            ensure!(
                pr_head == head && detail.is_none(),
                "checks values disagree with observation"
            );
            let facts: Vec<_> = d
                .items
                .iter()
                .filter_map(|i| {
                    if let ItemFacts::Checks { state, .. } = i.facts {
                        Some(state)
                    } else {
                        None
                    }
                })
                .collect();
            ensure!(
                facts.as_slice() == [*state],
                "checks state disagrees with observation"
            );
        }
        Payload::ForgePrState {
            state,
            merge_sha,
            link_event_id,
            pr_head,
            ..
        } => {
            let link = if admission {
                validate_current_identity(d, forge)?
            } else {
                let link = forge
                    .links
                    .iter()
                    .find(|link| Some(link.event_id.as_str()) == link_event_id.as_deref())
                    .ok_or_else(|| anyhow::anyhow!("projection link is not recorded"))?;
                validate_link_identity(d, forge, link, false)?;
                link
            };
            ensure!(
                link_event_id.as_deref() == Some(link.event_id.as_str())
                    && pr_head.as_deref() == Some(head),
                "lifecycle binding disagrees with observation"
            );
            let facts: Vec<_> = d
                .items
                .iter()
                .filter_map(|i| {
                    if let ItemFacts::PullRequest { state, merge_sha } = &i.facts {
                        Some((*state, merge_sha.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            ensure!(
                facts.as_slice() == [(*state, merge_sha.clone())],
                "lifecycle state disagrees with observation"
            );
        }
        _ => unreachable!(),
    }
    Ok(())
}

pub fn validate_current_identity<'a>(
    d: &ForgeObservation,
    forge: &'a crate::forge::ForgeState,
) -> Result<&'a crate::forge::ForgeLinkRecord> {
    let link = forge
        .link
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no forge link recorded"))?;
    validate_link_identity(d, forge, link, true)?;
    Ok(link)
}
fn validate_link_identity(
    d: &ForgeObservation,
    forge: &crate::forge::ForgeState,
    link: &crate::forge::ForgeLinkRecord,
    admission: bool,
) -> Result<()> {
    ensure!(!link.unresolved_binding, "projection link is unresolved");
    let p = &d.provider_identity;
    ensure!(
        p.pr_number == link.pr_number
            && p.url == link.url
            && p.base_repo == link.base_repo
            && p.base_ref == link.base_ref
            && p.head_repo == link.head_repo
            && p.head_ref == link.head_ref
            && d.observed_head_sha.as_deref() == Some(link.observed_head_sha.as_str()),
        "observation must name the current PR identity and raw observed head"
    );
    if let Some(source) = &link.observation {
        let original = forge
            .observations
            .iter()
            .find(|r| r.names_event(source))
            .ok_or_else(|| anyhow::anyhow!("link observation is not recorded"))?;
        ensure!(
            original.usable() && original.document.provider_identity.same_pr_identity_as(p),
            "observation provider identity differs from current PR"
        );
    } else if admission {
        ensure!(
            forge
                .projection
                .as_ref()
                .is_some_and(|projection| projection.host == p.host),
            "observation host differs from declared host"
        );
    }
    Ok(())
}

pub fn replay_observation(
    forge: &mut crate::forge::ForgeState,
    event: &crate::model::Event,
    document: &ForgeObservation,
    known_conflict: bool,
) {
    if let Some(existing) = forge
        .observations
        .iter_mut()
        .find(|r| r.document == *document)
    {
        existing.aliases.push(event.event_id.clone());
        return;
    }
    let conflict = known_conflict
        || forge
            .observations
            .iter()
            .any(|r| r.document.read_id == document.read_id);
    if conflict {
        for record in forge
            .observations
            .iter_mut()
            .filter(|r| r.document.read_id == document.read_id)
        {
            record.conflict = true;
        }
    }
    let invalid = document
        .validate(&forge.observations)
        .err()
        .map(|error| error.to_string());
    forge.observations.push(ObservationRecord {
        event_id: event.event_id.clone(),
        created_at: event.created_at,
        document: document.clone(),
        aliases: Vec::new(),
        conflict,
        invalid,
    });
}

/// Imported facts are checked in their supplying history. Independent reads
/// sharing an acquisition ID remain separate claims until merged replay.
pub fn validate_import_admission(
    events: &[crate::model::Event],
    introduced: &BTreeSet<&String>,
) -> Result<()> {
    use crate::model::Payload;
    for (index, event) in events.iter().enumerate() {
        if !introduced.contains(&event.event_id) {
            continue;
        }
        let source = match &event.payload {
            Payload::ForgeObservationRecorded { observation } => Some(observation),
            Payload::ForgeLink {
                observation: Some(id),
                ..
            }
            | Payload::ForgeChecks {
                observation: Some(id),
                ..
            }
            | Payload::ForgePrState {
                observation: Some(id),
                ..
            } => events[..index].iter().find_map(|e| {
                if e.event_id != *id {
                    return None;
                }
                if let Payload::ForgeObservationRecorded { observation } = &e.payload {
                    Some(observation)
                } else {
                    None
                }
            }),
            _ => continue,
        };
        if let Payload::ForgeObservationRecorded { observation } = &event.payload {
            ensure!(!events[..index].iter().any(|e| e.repository_id == event.repository_id && matches!(&e.payload, Payload::ForgeObservationRecorded { observation: old } if old.read_id == observation.read_id && old != observation)), "conflicting read_id in one recording repository");
        }
        let prefix: Vec<_> = events[..index].iter().filter(|e| {
            !matches!((&e.payload, source), (Payload::ForgeObservationRecorded { observation: old }, Some(doc)) if old.read_id == doc.read_id && old != doc)
        }).cloned().collect();
        let state = crate::state::reduce(&prefix)?;
        if let Some(document) = source {
            document.validate(&state.forge.observations)?;
        }
        match &event.payload {
            Payload::ForgeObservationRecorded { .. } => {}
            _ => validate_projection(&event.payload, &state.forge)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(repository_id: Option<&str>, native_id: Option<&str>) -> ProviderIdentity {
        ProviderIdentity {
            provider: "example".into(),
            host: "forge.example.invalid".into(),
            repository: "sample/project".into(),
            repository_id: repository_id.map(str::to_string),
            pr_number: 1,
            native_id: native_id.map(str::to_string),
            url: "https://forge.example.invalid/sample/project/pull/1".into(),
            base_repo: "sample/project".into(),
            base_ref: "main".into(),
            head_repo: "sample/project".into(),
            head_ref: "topic".into(),
        }
    }

    #[test]
    fn snapshot_fields_do_not_identify_a_native_pr() {
        let original = identity(Some("repository"), Some("pr"));
        let mut moved = original.clone();
        moved.repository = "renamed/project".into();
        moved.pr_number = 7;
        moved.url = "https://forge.example.invalid/renamed/project/pull/7".into();
        moved.base_repo = "renamed/project".into();
        moved.base_ref = "release".into();
        moved.head_repo = "fork/project".into();
        moved.head_ref = "other".into();
        assert!(original.same_pr_identity_as(&moved));
    }

    #[test]
    fn native_coordinates_separate_prs() {
        let original = identity(Some("repository"), Some("pr"));
        assert!(!original.same_pr_identity_as(&identity(Some("recreated"), Some("pr"))));
        assert!(!original.same_pr_identity_as(&identity(Some("repository"), Some("other"))));
        let mut other_host = original.clone();
        other_host.host = "other.example.invalid".into();
        assert!(!original.same_pr_identity_as(&other_host));
        let mut other_provider = original.clone();
        other_provider.provider = "other".into();
        assert!(!original.same_pr_identity_as(&other_provider));
    }

    #[test]
    fn a_missing_native_coordinate_matches_only_a_missing_one() {
        let native = identity(Some("repository"), Some("pr"));
        assert!(!native.same_pr_identity_as(&identity(None, None)));
        assert!(!native.same_pr_identity_as(&identity(Some("repository"), None)));
        assert!(!native.same_pr_identity_as(&identity(None, Some("pr"))));

        let named = identity(None, None);
        assert!(named.same_pr_identity_as(&identity(None, None)));
        let mut renumbered = named.clone();
        renumbered.pr_number = 2;
        assert!(!named.same_pr_identity_as(&renumbered));
        let mut renamed = named.clone();
        renamed.repository = "other/project".into();
        assert!(!named.same_pr_identity_as(&renamed));
    }
}
