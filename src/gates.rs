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
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a declared probe may run when its gate declares no timeout.
///
/// `status` runs probes under `catchup`, `inbox`, `check`, and the workspace
/// views, so a probe that waits on an unreachable host or container runtime
/// would stall every one of them. A gate that needs longer declares its own
/// timeout, which bounds the probe too.
pub const DEFAULT_PROBE_TIMEOUT_SECONDS: u64 = 30;

/// Bound on the probe stderr a failure warning carries.
const PROBE_STDERR_BYTES: usize = 4096;
const SIGKILL: i32 = 9;

extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

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
    /// applies to. The identity is a digest of the probe's stdout, and only a
    /// successful run that prints something yields one. `verify` records that
    /// identity beside the gate evidence, and readiness counts the evidence
    /// only where the same probe yields the same identity. A gate with no
    /// probe takes evidence produced in any environment.
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

/// The result of running one declared environment probe.
#[derive(Debug, Default)]
pub struct ProbeRun {
    /// `sha256:` over stdout, present only when the probe exited successfully
    /// and printed something.
    pub identity: Option<String>,
    /// Why no identity was read, absent when `identity` is present.
    pub failure: Option<ProbeFailure>,
}

/// Why a declared probe yielded no identity.
#[derive(Debug, Default)]
pub struct ProbeFailure {
    /// The exit code the probe returned, absent when it could not be started
    /// or was killed at its bound.
    pub exit_code: Option<i32>,
    /// The probe overran its time bound and was killed.
    pub timed_out: bool,
    /// Final bytes of the probe's stderr, or the start error. Empty when it
    /// wrote nothing there.
    pub detail: String,
}

impl ProbeFailure {
    /// One line naming the failure, for a warning or a gate reason.
    pub fn describe(&self) -> String {
        let mut description = if self.timed_out {
            "the probe overran its time bound and was killed".to_string()
        } else {
            match self.exit_code {
                Some(0) => "the probe exited 0 without printing an identity".to_string(),
                Some(code) => format!("the probe exited {code}"),
                None => "the probe could not be started".to_string(),
            }
        };
        if !self.detail.is_empty() {
            description.push_str(&format!(": {}", self.detail));
        }
        description
    }
}

/// Run a declared probe in `cwd` and report the identity it yields, if any.
///
/// The probe runs through the same shell a gate does, from the same
/// directory, so it describes the environment the gate runs in. The identity
/// is a digest of its stdout, and only a run that exits successfully and
/// prints something yields one. A probe that fails, cannot start, prints
/// nothing, or overruns its bound yields no identity: two environments in
/// which the probe fails are not thereby the same environment, and evidence
/// from one must not answer for the other. Incidental stderr is not part of
/// the identity either.
///
/// The bound is the gate's declared timeout when it has one, otherwise
/// `DEFAULT_PROBE_TIMEOUT_SECONDS`. An overrunning probe is killed with its
/// process group. The digest streams, so a probe that prints without bound
/// cannot exhaust memory; the stderr kept for the failure is a bounded tail.
pub fn environment_probe(cwd: &Path, probe: &str, timeout: Option<u64>) -> Result<ProbeRun> {
    let timeout = timeout.unwrap_or(DEFAULT_PROBE_TIMEOUT_SECONDS);
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(timeout))
        .context("environment probe timeout is too large")?;
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(probe)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return Ok(ProbeRun {
                identity: None,
                failure: Some(ProbeFailure {
                    exit_code: None,
                    timed_out: false,
                    detail: error.to_string(),
                }),
            })
        }
    };
    let stdout = child
        .stdout
        .take()
        .context("environment probe stdout pipe unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("environment probe stderr pipe unavailable")?;
    let stdout_reader = std::thread::spawn(move || -> std::io::Result<(String, bool)> {
        let mut digest = Sha256::new();
        let mut printed = false;
        let mut buffer = [0_u8; 8192];
        let mut stdout = stdout;
        loop {
            let read = stdout.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
            if buffer[..read]
                .iter()
                .any(|byte| !byte.is_ascii_whitespace())
            {
                printed = true;
            }
        }
        Ok((hex::encode(digest.finalize()), printed))
    });
    let stderr_reader = std::thread::spawn(move || read_tail(stderr, PROBE_STDERR_BYTES));

    let mut status = None;
    let mut timed_out = false;
    loop {
        if status.is_none() {
            status = child
                .try_wait()
                .context("failed to wait for environment probe")?;
        }
        if status.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            kill_process_group(child.id())?;
            status = Some(
                child
                    .wait()
                    .context("failed to reap overrunning environment probe")?,
            );
            timed_out = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let (stdout_digest, printed) = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("environment probe stdout reader panicked"))?
        .context("failed to read environment probe stdout")?;
    let stderr_tail = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("environment probe stderr reader panicked"))?
        .context("failed to read environment probe stderr")?;
    let exit_code = status.and_then(|status| status.code());
    let succeeded = status.is_some_and(|status| status.success());
    Ok(if timed_out || !succeeded || !printed {
        ProbeRun {
            identity: None,
            failure: Some(ProbeFailure {
                exit_code,
                timed_out,
                detail: stderr_tail,
            }),
        }
    } else {
        ProbeRun {
            identity: Some(format!("sha256:{stdout_digest}")),
            failure: None,
        }
    })
}

/// The final `limit` bytes of a reader, for a bounded diagnostic.
fn read_tail(mut reader: impl Read, limit: usize) -> std::io::Result<String> {
    let mut tail = Vec::with_capacity(limit);
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            return Ok(String::from_utf8_lossy(&tail).into_owned());
        }
        if read >= limit {
            tail.clear();
            tail.extend_from_slice(&chunk[read - limit..read]);
            continue;
        }
        let overflow = tail.len().saturating_add(read).saturating_sub(limit);
        if overflow > 0 {
            tail.drain(..overflow);
        }
        tail.extend_from_slice(&chunk[..read]);
    }
}

fn kill_process_group(pid: u32) -> Result<()> {
    let pid =
        i32::try_from(pid).map_err(|_| anyhow::anyhow!("environment probe pid exceeds i32"))?;
    // SAFETY: `kill` is called with a negated child PID created as the leader
    // of its own process group; SIGKILL requires no borrowed memory contract.
    if unsafe { kill(-pid, SIGKILL) } == -1 {
        let error = std::io::Error::last_os_error();
        // The group may have exited between the completion poll and kill(2);
        // that race is not a failure to reap it.
        if error.raw_os_error() != Some(3) {
            return Err(error).context("failed to kill overrunning environment probe");
        }
    }
    Ok(())
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
