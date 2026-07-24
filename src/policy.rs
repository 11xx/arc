//! Repository-declared integration policy.
//!
//! Policy is loaded from `.arc/policy.toml`. An absent file preserves the
//! default behavior, with every policy disabled.

use crate::config::ProvenanceBehavior;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Integration policy committed at `.arc/policy.toml` in the repository.
#[derive(Debug, Default, Deserialize)]
pub struct PolicyFile {
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub review: Review,
    #[serde(default)]
    pub provenance: ProvenanceBehavior,
}

#[derive(Debug, Default, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub forbid_self_approval: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct Review {
    #[serde(default)]
    pub checklist: Vec<String>,
}

pub fn load(repo_toplevel: &Path) -> Result<PolicyFile> {
    let path = repo_toplevel.join(".arc").join("policy.toml");
    if !path.is_file() {
        return Ok(PolicyFile {
            provenance: ProvenanceBehavior {
                git_identity: crate::config::load()?.provenance_git_identity,
            },
            ..PolicyFile::default()
        });
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let mut policy: PolicyFile =
        toml::from_str(&text).with_context(|| format!("malformed {}", path.display()))?;
    let tables: toml::Table =
        toml::from_str(&text).with_context(|| format!("malformed {}", path.display()))?;
    if !tables.contains_key("provenance") {
        policy.provenance.git_identity = crate::config::load()?.provenance_git_identity;
    }
    Ok(policy)
}
