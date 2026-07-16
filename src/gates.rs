use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Declared verification gates, committed at `.arc/gates.toml` in the
/// repository. A gate with no `profiles` list is required for every
/// profile. This file is the local analogue of required CI checks.
#[derive(Debug, Default, Deserialize)]
pub struct GatesFile {
    #[serde(default)]
    pub gates: BTreeMap<String, Gate>,
}

#[derive(Debug, Deserialize)]
pub struct Gate {
    pub command: String,
    #[serde(default)]
    pub profiles: Vec<String>,
}

pub fn load(repo_toplevel: &Path) -> Result<GatesFile> {
    let path = repo_toplevel.join(".arc").join("gates.toml");
    if !path.is_file() {
        return Ok(GatesFile::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("malformed {}", path.display()))
}

impl GatesFile {
    pub fn required_for<'a>(&'a self, profile: &str) -> Vec<(&'a String, &'a Gate)> {
        self.gates
            .iter()
            .filter(|(_, g)| g.profiles.is_empty() || g.profiles.iter().any(|p| p == profile))
            .collect()
    }
}
