//! Repository-declared verification gates and their optional execution policy.
//!
//! Gate timeouts use the same positive `s`/`m`/`h` duration syntax as claim
//! leases. Omitting a timeout preserves unbounded execution.

use crate::commands::parse_duration;
use anyhow::{Context, Result};
use serde::{de::Error as _, Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

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
    /// A command whose output identifies the environment this gate's evidence
    /// applies to. `verify` records a digest of that output beside the gate
    /// evidence, and readiness counts the evidence only where the probe yields
    /// the same digest. A gate with no probe takes evidence produced in any
    /// environment.
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_timeout")]
    pub timeout: Option<u64>,
}

fn deserialize_timeout<'de, D>(deserializer: D) -> std::result::Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(|raw| parse_duration(&raw).map_err(D::Error::custom))
        .transpose()
}

/// The environment identity a declared probe yields in `cwd`.
///
/// The probe runs through the same shell a gate does, from the same
/// directory, so it describes the environment the gate runs in. The identity
/// is a digest of the probe's combined output; its exit status is not part of
/// it, because what the probe reports is the answer, including a report that
/// the environment is broken.
///
/// The whole output is hashed as it streams, so a probe that writes without
/// bound cannot exhaust memory. The digest is opaque to arc: a probe names
/// the environment, and only its equality to another digest is ever read.
pub fn environment_identity(cwd: &Path, probe: &str) -> Result<String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!("exec 2>&1\n{probe}"))
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to run environment probe {probe:?}"))?;
    let mut stdout = child
        .stdout
        .take()
        .context("environment probe output pipe unavailable")?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = stdout
            .read(&mut buffer)
            .with_context(|| format!("failed to read environment probe {probe:?} output"))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    child
        .wait()
        .with_context(|| format!("failed to wait for environment probe {probe:?}"))?;
    Ok(format!("sha256:{}", hex::encode(digest.finalize())))
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
