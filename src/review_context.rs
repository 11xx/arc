//! Target-owned capability context shared by review and integration readers.

use crate::{review_ladder::Ladder, state::ChangeState};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct Registry {
    pub target: String,
    pub revision: Option<String>,
    pub version_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    #[serde(skip)]
    pub ladder: Option<Ladder>,
    #[serde(skip)]
    pub has_file: bool,
    #[serde(skip)]
    pub clearance: Option<crate::policy::ReviewClearance>,
    #[serde(skip)]
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip)]
    pub coordinates_always: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewContext {
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub judgments: std::collections::BTreeMap<String, crate::review_clearance::Judgment>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub authors: std::collections::BTreeMap<String, crate::review_authors::Authors>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landed_authors: Option<crate::review_authors::Authors>,
    pub authorizing: Registry,
    pub accepted: Registry,
}

pub fn load(
    cwd: &Path,
    state: &ChangeState,
    target_revision: Option<&str>,
) -> Option<ReviewContext> {
    let authorizing = Registry::read(
        cwd,
        if target_revision.is_some() {
            &state.target_branch
        } else {
            crate::declarations::accepted_target(state)
        },
        crate::declarations::authorizing_revision(cwd, state, target_revision),
    );
    let accepted_target = crate::declarations::accepted_target(state);
    let accepted = Registry::read(
        cwd,
        accepted_target,
        crate::gitio::branch_head(cwd, accepted_target),
    );
    if authorizing.ladder.is_none()
        && accepted.ladder.is_none()
        && authorizing.unavailable.is_none()
        && accepted.unavailable.is_none()
    {
        return None;
    }
    let authors = state
        .patchsets
        .iter()
        .map(|patchset| {
            (
                patchset.id.clone(),
                crate::review_authors::read(cwd, state, patchset, &patchset.base, &patchset.head),
            )
        })
        .collect();
    let landed_authors = state.replayed_range().and_then(|(base, revision)| {
        Some(crate::review_authors::read(
            cwd,
            state,
            state.closure_shipped_patchset()?,
            base,
            revision,
        ))
    });
    let mut context = ReviewContext {
        judgments: Default::default(),
        authorizing,
        accepted,
        authors,
        landed_authors,
    };
    context.judge_reads(state);
    Some(context)
}

impl Registry {
    fn read(cwd: &Path, target: &str, revision: anyhow::Result<String>) -> Self {
        let mut reading = Registry {
            target: target.to_string(),
            revision: None,
            version_ids: Vec::new(),
            unavailable: None,
            ladder: None,
            has_file: false,
            clearance: None,
            since: None,
            coordinates_always: false,
        };
        let result = revision.and_then(|revision| {
            reading.revision = Some(revision.clone());
            crate::review_ladder::load_at(cwd, &revision)
        });
        match result {
            Ok(ladder) => {
                reading.version_ids = ladder
                    .as_ref()
                    .map(|ladder| {
                        ladder
                            .version
                            .iter()
                            .map(|version| version.id.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                reading.has_file = ladder.is_some();
                reading.ladder = ladder;
            }
            Err(error) => {
                reading.has_file = reading.revision.is_some();
                reading.unavailable = Some(format!("{error:#}"));
            }
        }
        if reading.has_file {
            if let Some(revision) = reading.revision.as_deref() {
                match crate::policy::load_at(cwd, revision) {
                    Ok(policy) => {
                        reading.clearance = Some(policy.policy.review_clearance);
                        reading.since = policy.policy.review_clearance_since;
                        reading.coordinates_always = policy.policy.clearance_coordinates_always;
                    }
                    Err(error) => {
                        reading.unavailable =
                            Some(format!("clearance policy unavailable: {error:#}"))
                    }
                }
            }
        }
        reading
    }

    pub fn assessed(&self) -> Option<bool> {
        self.has_file
            .then(|| self.clearance.is_none_or(|mode| mode.is_assessed()))
    }

    pub fn coordinate_required(&self, at: chrono::DateTime<chrono::Utc>) -> bool {
        self.has_file
            && crate::policy::Policy {
                review_clearance: self
                    .clearance
                    .unwrap_or(crate::policy::ReviewClearance::Assessed),
                review_clearance_since: self.since,
                clearance_coordinates_always: self.coordinates_always,
                ..Default::default()
            }
            .clearance_coordinate_required(at)
    }
}

pub fn after_integration(state: &ChangeState, event: &str, audit: bool) -> bool {
    audit
        || state.closure.as_ref().is_some_and(|closure| {
            closure.outcome == crate::model::Closure::Integrated
                && (state
                    .privilege_scope
                    .as_ref()
                    .is_some_and(|scope| closure.origin_replica_id != scope.local_origin)
                    || event > closure.event_id.as_str())
        })
}

impl ReviewContext {
    pub fn registry(&self, after_integration: bool) -> &Registry {
        if after_integration {
            &self.accepted
        } else {
            &self.authorizing
        }
    }

    fn judge_reads(&mut self, state: &ChangeState) {
        for verdict in &state.verdicts {
            let after = after_integration(state, &verdict.event_id, false);
            let source = self.registry(after);
            if source.assessed() != Some(true) {
                continue;
            }
            let authors = if after
                && state
                    .closure_shipped_patchset()
                    .is_some_and(|patchset| patchset.id == verdict.patchset_id)
            {
                self.landed_authors
                    .as_ref()
                    .or_else(|| self.authors.get(&verdict.patchset_id))
            } else {
                self.authors.get(&verdict.patchset_id)
            };
            let judgment = crate::review_clearance::judge(crate::review_clearance::Facts {
                ladder: source.ladder.as_ref(),
                unavailable: source.unavailable.as_deref(),
                authors,
                reader: verdict.effective_author(),
                models: &crate::review_clearance::coordinates(
                    verdict.model.as_deref(),
                    verdict.observed_model.as_deref(),
                ),
                at: verdict.created_at,
                assessment: verdict.assessment.as_ref(),
            });
            self.judgments.insert(verdict.event_id.clone(), judgment);
        }
        for audit in &state.audit_verdicts {
            let source = &self.accepted;
            if source.assessed() != Some(true) {
                continue;
            }
            let authors = self
                .landed_authors
                .as_ref()
                .or_else(|| self.authors.get(&state.closure_shipped_patchset()?.id));
            let judgment = crate::review_clearance::judge(crate::review_clearance::Facts {
                ladder: source.ladder.as_ref(),
                unavailable: source.unavailable.as_deref(),
                authors,
                reader: audit.effective_author(),
                models: &crate::review_clearance::coordinates(
                    audit.model.as_deref(),
                    audit.observed_model.as_deref(),
                ),
                at: audit.created_at,
                assessment: audit.assessment.as_ref(),
            });
            self.judgments.insert(audit.event_id.clone(), judgment);
        }
    }
}
