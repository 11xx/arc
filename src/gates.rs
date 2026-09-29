//! Verification gates from project and operator declarations.
//!
//! Gates committed in `.arc/gates.toml` apply alongside gates in the
//! repository's operator policy. A conflicting command for one gate name is
//! retained for diagnostics and refused by gate-dependent operations.

use crate::commands::parse_duration;
use crate::process_group::{kill_process_group, read_tail, STDERR_TAIL_BYTES};
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

/// A gate a change declares as a different check than the target's.
#[derive(Debug, Clone)]
pub struct GateDivergence {
    pub name: String,
    pub target: GateDeclaration,
    pub change: GateDeclaration,
}

impl GateDivergence {
    pub fn describe(&self) -> String {
        format!(
            "gate {:?}: {}; {}; the target's is evaluated",
            self.name,
            self.target.describe(),
            self.change.describe()
        )
    }
}

impl GateDeclaration {
    fn of(gate: &Gate, source: &str) -> Self {
        GateDeclaration {
            command: gate.command.clone(),
            environment: gate.environment.clone(),
            source: source.to_string(),
        }
    }

    /// One declaration as a conflict report names it.
    pub fn describe(&self) -> String {
        match &self.environment {
            Some(probe) => format!(
                "{} declares {:?} with environment probe {:?}",
                self.source, self.command, probe
            ),
            None => format!("{} declares {:?}", self.source, self.command),
        }
    }
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
    let stderr_reader = std::thread::spawn(move || read_tail(stderr, STDERR_TAIL_BYTES));

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
            kill_process_group(child.id())
                .context("failed to kill overrunning environment probe")?;
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

const PROJECT_SOURCE: &str = ".arc/gates.toml";
const OPERATOR_SOURCE: &str = "<git-common-dir>/arc/operator-policy.toml";

/// Validate the gate portion of an operator-policy document.
pub fn validate_operator_text(text: &str) -> Result<()> {
    toml::from_str::<GatesFile>(text).context("malformed operator policy TOML")?;
    Ok(())
}

/// Load both declarations from the checkout at `repo_toplevel`, retaining
/// conflicts for `arc doctor`.
pub fn inspect(repo_toplevel: &Path) -> Result<GatesFile> {
    let path = repo_toplevel.join(PROJECT_SOURCE);
    let project = read_optional(&path)?.map(|text| (path.display().to_string(), text));
    layered(repo_toplevel, project)
}

/// Load both declarations with the project layer read from the tree committed
/// at `revision`, wherever `cwd` stands in the repository.
pub fn inspect_at(cwd: &Path, revision: &str) -> Result<GatesFile> {
    let project = crate::gitio::file_at(cwd, revision, PROJECT_SOURCE)?
        .map(|text| (format!("{PROJECT_SOURCE} at {revision}"), text));
    layered(cwd, project)
}

/// Load declarations at `revision`, rejecting ambiguous gate commands.
pub fn load_at(cwd: &Path, revision: &str) -> Result<GatesFile> {
    let gates = inspect_at(cwd, revision)?;
    gates.ensure_unconflicted()?;
    Ok(gates)
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Layer the project declaration, given as its origin and text, under the
/// operator's. `repo` is any path inside the repository.
fn layered(repo: &Path, project: Option<(String, String)>) -> Result<GatesFile> {
    let operator = crate::policy::operator_path(repo)?;
    let operator = read_optional(&operator)?.map(|text| (operator.display().to_string(), text));
    let mut merged = GatesFile::default();

    for (source, layer) in [(PROJECT_SOURCE, project), (OPERATOR_SOURCE, operator)] {
        let Some((origin, text)) = layer else {
            continue;
        };
        let layer =
            toml::from_str::<GatesFile>(&text).with_context(|| format!("malformed {origin}"))?;
        let source = source.to_string();

        for (name, mut gate) in layer.gates {
            gate.declared_by.push(source.clone());
            match merged.gates.get_mut(&name) {
                None => {
                    merged.gates.insert(name, gate);
                }
                // A gate is its command and the environment its evidence
                // applies to; two layers disagreeing on either declare two
                // different checks under one name.
                Some(current) if !current.same_check(&gate) => {
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
                Some(current) => current.absorb(gate),
            }
        }
    }

    Ok(merged)
}

impl Gate {
    /// Whether two declarations name the same check: one command run against
    /// one environment.
    fn same_check(&self, other: &Gate) -> bool {
        self.command == other.command && self.environment == other.environment
    }

    /// Fold another declaration of the same check into this one. Profiles
    /// widen and the stricter timeout wins, so one declaration cannot loosen
    /// a gate the other bounds.
    fn absorb(&mut self, other: Gate) {
        if self.profiles.is_empty() || other.profiles.is_empty() {
            self.profiles.clear();
        } else {
            self.profiles.extend(other.profiles);
            let profiles: BTreeSet<_> = self.profiles.drain(..).collect();
            self.profiles.extend(profiles);
        }
        self.timeout = match (self.timeout, other.timeout) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, right) => left.or(right),
        };
        self.declared_by.extend(other.declared_by);
        self.declared_by.sort();
        self.declared_by.dedup();
    }
}

impl GatesFile {
    /// Add what a change's own declarations require on top of this set, the
    /// target's. A gate the change adds is owed. A gate the target already
    /// declares stays the target's check: the change can widen its profiles
    /// or tighten its timeout but cannot substitute another command or
    /// environment, and such a substitution is returned so it can be reported.
    /// `target_label` and `change_label` name the two sides in that report.
    pub fn owe_also(
        &mut self,
        own: GatesFile,
        target_label: &str,
        change_label: &str,
    ) -> Vec<GateDivergence> {
        let mut divergences = Vec::new();
        for (name, gate) in own.gates {
            match self.gates.get_mut(&name) {
                None => {
                    self.gates.insert(name, gate);
                }
                Some(current) if current.same_check(&gate) => current.absorb(gate),
                Some(current) => divergences.push(GateDivergence {
                    target: GateDeclaration::of(current, target_label),
                    change: GateDeclaration::of(&gate, change_label),
                    name,
                }),
            }
        }
        divergences
    }

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
                    .map(GateDeclaration::describe)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(text: &str) -> GatesFile {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn a_change_adds_gates_and_cannot_replace_the_targets_check() {
        let mut target =
            declared("[gates.guard]\ncommand = \"test -f A\"\n[gates.lint]\ncommand = \"lint\"\n");
        let own =
            declared("[gates.guard]\ncommand = \"true\"\n[gates.extra]\ncommand = \"test -f B\"\n");
        let divergences = target.owe_also(own, "master", "change");

        assert_eq!(target.gates["guard"].command, "test -f A");
        assert!(target.gates.contains_key("lint"));
        assert_eq!(target.gates["extra"].command, "test -f B");
        assert_eq!(divergences.len(), 1);
        assert_eq!(divergences[0].name, "guard");
        assert!(divergences[0]
            .describe()
            .contains("the target's is evaluated"));
    }

    #[test]
    fn the_same_check_declared_twice_takes_the_stricter_timeout() {
        let mut target = declared("[gates.unit]\ncommand = \"t\"\ntimeout = \"10m\"\n");
        let own = declared("[gates.unit]\ncommand = \"t\"\ntimeout = \"1m\"\n");
        assert!(target.owe_also(own, "master", "change").is_empty());
        assert_eq!(target.gates["unit"].timeout, Some(60));

        let unbounded = declared("[gates.unit]\ncommand = \"t\"\n");
        assert!(target.owe_also(unbounded, "master", "change").is_empty());
        assert_eq!(target.gates["unit"].timeout, Some(60));
    }
}
