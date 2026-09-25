//! Verification gates from project and operator declarations.
//!
//! Gates committed in `.arc/gates.toml` apply alongside gates in the
//! repository's operator policy. A conflicting command for one gate name is
//! retained for diagnostics and refused by gate-dependent operations.

use crate::commands::parse_duration;
use anyhow::{bail, Context, Result};
use serde::{de::Error as _, Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
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

/// Declared verification gates. A gate with no `profiles` list is required
/// for every profile. This is the local analogue of required CI checks.
#[derive(Debug, Default, Deserialize)]
pub struct GatesFile {
    #[serde(default)]
    pub gates: BTreeMap<String, Gate>,
    #[serde(skip)]
    pub conflicts: Vec<GateConflict>,
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
    #[serde(skip, default)]
    pub declared_by: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GateConflict {
    pub name: String,
    pub declarations: Vec<GateDeclaration>,
}

#[derive(Debug, Clone)]
pub struct GateDeclaration {
    pub command: String,
    pub environment: Option<String>,
    pub source: String,
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

fn source_name(path: &Path, repo_toplevel: &Path) -> String {
    if path == repo_toplevel.join(".arc/gates.toml") {
        ".arc/gates.toml".to_string()
    } else {
        "<git-common-dir>/arc/operator-policy.toml".to_string()
    }
}

/// Validate the gate portion of an operator-policy document.
pub fn validate_operator_text(text: &str) -> Result<()> {
    toml::from_str::<GatesFile>(text).context("malformed operator policy TOML")?;
    Ok(())
}

/// Load both declarations while retaining conflicts for `arc doctor`.
pub fn inspect(repo_toplevel: &Path) -> Result<GatesFile> {
    let in_tree = repo_toplevel.join(".arc/gates.toml");
    let operator = crate::policy::operator_path(repo_toplevel)?;
    let mut merged = GatesFile::default();

    for path in [in_tree, operator] {
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()))
            }
        };
        let layer = toml::from_str::<GatesFile>(&text)
            .with_context(|| format!("malformed {}", path.display()))?;
        let source = source_name(&path, repo_toplevel);

        for (name, mut gate) in layer.gates {
            gate.declared_by.push(source.clone());
            match merged.gates.get_mut(&name) {
                None => {
                    merged.gates.insert(name, gate);
                }
                // A gate is its command and the environment its evidence
                // applies to; two layers disagreeing on either declare two
                // different checks under one name.
                Some(current)
                    if current.command != gate.command
                        || current.environment != gate.environment =>
                {
                    merged.conflicts.push(GateConflict {
                        name,
                        declarations: vec![
                            GateDeclaration {
                                command: current.command.clone(),
                                environment: current.environment.clone(),
                                source: current.declared_by[0].clone(),
                            },
                            GateDeclaration {
                                command: gate.command,
                                environment: gate.environment,
                                source: source.clone(),
                            },
                        ],
                    });
                    current.declared_by.extend(gate.declared_by);
                    current.declared_by.sort();
                    current.declared_by.dedup();
                }
                Some(current) => {
                    if current.profiles.is_empty() || gate.profiles.is_empty() {
                        current.profiles.clear();
                    } else {
                        current.profiles.extend(gate.profiles);
                        let profiles: BTreeSet<_> = current.profiles.drain(..).collect();
                        current.profiles.extend(profiles);
                    }
                    // The stricter bound wins, as every other layered rule
                    // does: one layer cannot loosen a gate the other bounds.
                    current.timeout = match (current.timeout, gate.timeout) {
                        (Some(left), Some(right)) => Some(left.min(right)),
                        (left, right) => left.or(right),
                    };
                    current.declared_by.extend(gate.declared_by);
                    current.declared_by.sort();
                    current.declared_by.dedup();
                }
            }
        }
    }

    Ok(merged)
}

/// Load declarations for operations that must reject ambiguous gate commands.
pub fn load(repo_toplevel: &Path) -> Result<GatesFile> {
    let gates = inspect(repo_toplevel)?;
    gates.ensure_unconflicted()?;
    Ok(gates)
}

impl GatesFile {
    pub fn ensure_unconflicted(&self) -> Result<()> {
        if self.conflicts.is_empty() {
            return Ok(());
        }
        let details = self
            .conflicts
            .iter()
            .map(|conflict| {
                let declarations = conflict
                    .declarations
                    .iter()
                    .map(|decl| match &decl.environment {
                        Some(probe) => format!(
                            "{} declares {:?} with environment probe {:?}",
                            decl.source, decl.command, probe
                        ),
                        None => format!("{} declares {:?}", decl.source, decl.command),
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                format!("gate {:?} conflicts: {declarations}", conflict.name)
            })
            .collect::<Vec<_>>()
            .join("; ");
        bail!("conflicting gate declarations: {details}")
    }

    pub fn required_for<'a>(&'a self, profile: &str) -> Vec<(&'a String, &'a Gate)> {
        self.gates
            .iter()
            .filter(|(_, gate)| {
                gate.profiles.is_empty() || gate.profiles.iter().any(|item| item == profile)
            })
            .collect()
    }
}
