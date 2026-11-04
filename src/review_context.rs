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
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewContext {
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
    Some(ReviewContext {
        authorizing,
        accepted,
        authors,
        landed_authors,
    })
}

impl Registry {
    fn read(cwd: &Path, target: &str, revision: anyhow::Result<String>) -> Self {
        let mut reading = Registry {
            target: target.to_string(),
            revision: None,
            version_ids: Vec::new(),
            unavailable: None,
            ladder: None,
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
                reading.ladder = ladder;
            }
            Err(error) => reading.unavailable = Some(format!("{error:#}")),
        }
        reading
    }
}
