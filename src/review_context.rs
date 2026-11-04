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
    let context = ReviewContext {
        authorizing,
        accepted,
    };
    (context.authorizing.ladder.is_some()
        || context.accepted.ladder.is_some()
        || context.authorizing.unavailable.is_some()
        || context.accepted.unavailable.is_some())
    .then_some(context)
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
