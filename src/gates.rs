//! Verification gates from project and operator declarations.
//!
//! Gates committed in `.arc/gates.toml` apply alongside gates in the
//! repository's operator policy. A conflicting command for one gate name is
//! retained for diagnostics and refused by gate-dependent operations.

use crate::commands::parse_duration;
use crate::model::{DeltaSelection, GateEvent};
use crate::perf::Counted;
use crate::process_group::{
    join_before_deadline, kill_process_group, read_tail, STDERR_TAIL_BYTES,
};
use anyhow::{bail, Context, Result};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
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

pub const BYPASS_RETIRED: &str = "--bypass is retired; make the gate fit its budget (`arc gates over`), or integrate with recorded debt";

/// Declared verification gates. A gate with no `profiles` list is required
/// for every profile. This is the local analogue of required CI checks.
#[derive(Debug, Default, Deserialize)]
pub struct GatesFile {
    #[serde(skip)]
    pub invalid_budget_removals: BTreeSet<Option<String>>,
    #[serde(skip)]
    pub pending_raises: BTreeSet<Option<String>>,
    #[serde(skip)]
    pub raise_snapshot_missing: bool,
    #[serde(skip)]
    pub raises: Vec<crate::gate_raise::Applicability>,
    #[serde(default)]
    pub selection: PathSelection,
    #[serde(skip)]
    pub dangerous: Vec<String>,
    #[serde(default)]
    pub budget: Option<crate::gate_budget::BudgetRules>,
    #[serde(default)]
    pub gates: BTreeMap<String, Gate>,
    #[serde(skip)]
    pub conflicts: Vec<GateConflict>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PathSelection {
    #[serde(default)]
    pub base_passes: bool,
    #[serde(default)]
    pub inert: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Coverage {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selection_inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub base_passes: bool,
    pub dangerous: Vec<String>,
    pub inert: Vec<String>,
    pub gates: BTreeMap<String, Vec<String>>,
}

pub const PATH_SELECTOR: &str = "arc:paths";

impl Coverage {
    pub fn is_inert(&self, path: &str) -> bool {
        self.inert
            .iter()
            .any(|pattern| crate::policy::glob_match(pattern, path))
    }

    pub fn is_named(&self, path: &str) -> bool {
        self.gates
            .values()
            .flatten()
            .any(|pattern| crate::policy::glob_match(pattern, path))
    }

    pub fn affects(&self, name: &str, path: &str) -> bool {
        if self.base_passes
            && (crate::base_pass::protected(path)
                || self
                    .selection_inputs
                    .iter()
                    .any(|command| command.contains(path)))
        {
            return true;
        }
        if self
            .dangerous
            .iter()
            .any(|pattern| crate::policy::glob_match(pattern, path))
        {
            return true;
        }
        if self.is_inert(path) {
            return false;
        }
        if !self.is_named(path) {
            return true;
        }
        self.gates.get(name).is_none_or(|patterns| {
            patterns.is_empty()
                || patterns
                    .iter()
                    .any(|pattern| crate::policy::glob_match(pattern, path))
        })
    }

    pub fn enabled(&self) -> bool {
        self.base_passes
            || !self.inert.is_empty()
            || self.gates.values().any(|paths| !paths.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Gate {
    pub command: String,
    #[serde(default, deserialize_with = "crate::gate_budget::deserialize_ms")]
    pub budget: Option<u64>,
    #[serde(skip)]
    pub budget_sources: Vec<crate::gate_budget::BudgetSource>,
    #[serde(skip)]
    pub raise_allowance_ms: Option<u64>,
    #[serde(skip)]
    pub raise_budget_max_ms: Option<u64>,
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
    #[serde(default)]
    pub events: Vec<GateEvent>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(skip)]
    pub coverage: Coverage,
    #[serde(default, deserialize_with = "deserialize_timeout")]
    pub timeout: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_exclusive")]
    pub exclusive: Option<String>,
    #[serde(default, deserialize_with = "deserialize_admission")]
    pub admission: Option<Duration>,
    /// An inert legacy key retained for declaration diagnostics.
    #[serde(default)]
    pub bypass: Option<String>,
    /// What one pass answers for. It changes what a pass means, so it is
    /// part of the declaration's identity.
    #[serde(default)]
    pub binds: Binding,
    #[serde(skip, default)]
    pub declared_by: Vec<String>,
}

/// What a gate's pass answers for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Binding {
    /// The tree it ran against, whichever commit holds that tree.
    #[default]
    Tree,
    /// The commit it ran on, for a command that reads commit messages or
    /// history, which a commit with the same tree need not share.
    Commit,
}

impl Binding {
    pub fn is_tree(&self) -> bool {
        *self == Binding::Tree
    }
}

#[derive(Debug, Clone)]
pub struct GateConflict {
    pub name: String,
    pub declarations: Vec<GateDeclaration>,
}

#[derive(Debug, Clone)]
pub struct GateDeclaration {
    pub paths: Vec<String>,
    pub inert: Vec<String>,
    pub exclusive: Option<String>,
    pub selector: Option<String>,
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
            paths: gate.paths.clone(),
            inert: gate.coverage.inert.clone(),
            exclusive: gate.exclusive.clone(),
            selector: gate.selector.clone(),
            command: gate.command.clone(),
            environment: gate.environment.clone(),
            source: source.to_string(),
        }
    }

    /// One declaration as a conflict report names it.
    pub fn describe(&self) -> String {
        let description = match &self.environment {
            Some(probe) => format!(
                "{} declares {:?} with environment probe {:?}",
                self.source, self.command, probe
            ),
            None => format!("{} declares {:?}", self.source, self.command),
        };
        let description = match &self.exclusive {
            Some(name) => format!("{description} with exclusive lock {name:?}"),
            None => description,
        };
        let description = match &self.selector {
            Some(selector) => format!("{description} with delta selector {selector:?}"),
            None => description,
        };
        format!(
            "{description} with paths {:?} and inert {:?}",
            self.paths, self.inert
        )
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

fn deserialize_exclusive<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let name = Option::<String>::deserialize(deserializer)?;
    if name.as_ref().is_some_and(|name| name.trim().is_empty()) {
        return Err(D::Error::custom("exclusive must name a lock"));
    }
    Ok(name)
}

fn deserialize_admission<'de, D>(deserializer: D) -> std::result::Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(|raw| parse_gate_duration(&raw).map_err(D::Error::custom))
        .transpose()
}

/// A positive duration with millisecond, second, minute or hour units.
pub fn parse_gate_duration(raw: &str) -> Result<Duration> {
    let (number, scale) = [("ms", 0.001), ("s", 1.0), ("m", 60.0), ("h", 3600.0)]
        .into_iter()
        .find_map(|(unit, scale)| raw.strip_suffix(unit).map(|number| (number, scale)))
        .context("duration needs ms, s, m or h units")?;
    if number.is_empty()
        || !number
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        bail!("duration must be a positive number with units");
    }
    let duration = Duration::try_from_secs_f64(number.parse::<f64>()? * scale)?;
    if duration.is_zero() {
        bail!("duration must be positive");
    }
    Ok(duration)
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
/// `DEFAULT_PROBE_TIMEOUT_SECONDS`. Whether it finishes or overruns, its
/// process group is killed before it returns, so nothing it started outlives
/// it. The digest streams, so a probe that prints without bound cannot
/// exhaust memory; the stderr kept for the failure is a bounded tail.
pub fn environment_probe(cwd: &Path, probe: &str, timeout: Option<u64>) -> Result<ProbeRun> {
    let timeout = timeout.unwrap_or(DEFAULT_PROBE_TIMEOUT_SECONDS);
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(timeout))
        .context("environment probe timeout is too large")?;
    let mut shell = match crate::slots::shell(cwd) {
        Ok(shell) => shell,
        Err(error) => {
            return Ok(ProbeRun {
                identity: None,
                failure: Some(ProbeFailure {
                    exit_code: None,
                    timed_out: false,
                    detail: format!("slot unavailable: {error:#}"),
                }),
            });
        }
    };
    let command = &mut shell.command;
    command
        .arg("-c")
        .arg(probe)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let _preparation = match crate::warm_worktree::configure_command(command, cwd) {
        Ok(lease) => lease,
        Err(error) => {
            return Ok(ProbeRun {
                identity: None,
                failure: Some(ProbeFailure {
                    exit_code: None,
                    timed_out: false,
                    detail: format!("worktree storage unavailable: {error:#}"),
                }),
            });
        }
    };
    let _watch = crate::interruption::Watch::start().context("cannot watch for interruption")?;
    crate::interruption::check()?;
    let mut child = match shell.spawn() {
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
        .child()
        .stdout
        .take()
        .context("environment probe stdout pipe unavailable")?;
    let stderr = child
        .child()
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

    let mut exited = false;
    let timed_out = loop {
        if !exited {
            exited = child
                .leader_exited()
                .context("failed to wait for environment probe")?;
        }
        if (exited && stdout_reader.is_finished() && stderr_reader.is_finished())
            || crate::interruption::check().is_err()
        {
            break false;
        }
        if Instant::now() >= deadline {
            break true;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let status = child
        .stop()
        .context("failed to stop the environment probe's process group")?;
    crate::interruption::check()?;
    let (stdout_digest, printed) = join_before_deadline(
        stdout_reader,
        Some(deadline),
        "environment probe stdout reader",
    )?
    .transpose()
    .context("failed to read environment probe stdout")?
    .unwrap_or_default();
    let stderr_tail = join_before_deadline(
        stderr_reader,
        Some(deadline),
        "environment probe stderr reader",
    )?
    .transpose()
    .context("failed to read environment probe stderr")?
    .unwrap_or_default();
    let exit_code = status.code();
    Ok(if timed_out || !status.success() || !printed {
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
    let mut gates = layered(repo_toplevel, project)?;
    gates.set_danger(
        crate::policy::danger_paths(repo_toplevel, None).unwrap_or_else(|_| vec!["**".into()]),
    );
    Ok(gates)
}

/// Load both declarations with the project layer read from the tree committed
/// at `revision`, wherever `cwd` stands in the repository.
pub fn inspect_at(cwd: &Path, revision: &str) -> Result<GatesFile> {
    let project = crate::gitio::file_at(cwd, revision, PROJECT_SOURCE)?
        .map(|text| (format!("{PROJECT_SOURCE} at {revision}"), text));
    let mut gates = layered(cwd, project)?;
    gates.set_danger(
        crate::policy::danger_paths(cwd, Some(revision)).unwrap_or_else(|_| vec!["**".into()]),
    );
    Ok(gates)
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
        let mut layer =
            toml::from_str::<GatesFile>(&text).with_context(|| format!("malformed {origin}"))?;
        let source = source.to_string();
        layer.name_budget_sources(&origin);
        crate::gate_budget::merge_rules(&mut merged.budget, layer.budget.take());

        if source == PROJECT_SOURCE {
            merged.selection.base_passes = layer.selection.base_passes;
        }
        merged.selection.inert.extend(layer.selection.inert);
        for (name, mut gate) in layer.gates {
            gate.declared_by.push(source.clone());
            match merged.gates.get_mut(&name) {
                None => {
                    merged.gates.insert(name, gate);
                }
                // Layers must agree on the check and any required lock name.
                Some(current) if !current.same_check(&gate) => {
                    current.bypass = current.bypass.take().or_else(|| gate.bypass.clone());
                    merged.conflicts.push(GateConflict {
                        name,
                        declarations: vec![
                            GateDeclaration {
                                paths: current.paths.clone(),
                                inert: current.coverage.inert.clone(),
                                exclusive: current.exclusive.clone(),
                                selector: current.selector.clone(),
                                command: current.command.clone(),
                                environment: current.environment.clone(),
                                source: current.declared_by[0].clone(),
                            },
                            GateDeclaration {
                                paths: gate.paths.clone(),
                                inert: gate.coverage.inert.clone(),
                                exclusive: gate.exclusive,
                                selector: gate.selector,
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

    merged.selection.inert.sort();
    merged.selection.inert.dedup();
    for gate in merged.gates.values_mut() {
        gate.coverage.inert = merged.selection.inert.clone();
    }
    merged.refresh_coverage();
    Ok(merged)
}

impl Gate {
    /// Whether declarations name the same check and compatible lock requirements.
    fn same_check(&self, other: &Gate) -> bool {
        self.command == other.command
            && self.environment == other.environment
            && self.selector == other.selector
            && self.paths == other.paths
            && self.coverage.inert == other.coverage.inert
            && (self.exclusive.is_none()
                || other.exclusive.is_none()
                || self.exclusive == other.exclusive)
    }

    /// Fold another declaration of the same check into this one. Profiles
    /// and events widen and the stricter timeout and budget win, so one declaration cannot loosen
    /// a gate the other bounds. It binds to commits if either says so. Either
    /// declaration may require a lock; the shortest admission limit applies.
    fn absorb(&mut self, other: Gate) {
        self.absorb_budget(&other);
        let left = self
            .admission
            .or_else(|| self.exclusive.as_ref().map(|_| Duration::from_secs(60)));
        let right = other
            .admission
            .or_else(|| other.exclusive.as_ref().map(|_| Duration::from_secs(60)));
        self.admission = match (left, right) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, right) => left.or(right),
        };
        if self.exclusive.is_none() {
            self.exclusive = other.exclusive;
        }
        self.bypass = self.bypass.take().or(other.bypass);
        if other.binds == Binding::Commit {
            self.binds = Binding::Commit;
        }
        if self.profiles.is_empty() || other.profiles.is_empty() {
            self.profiles.clear();
        } else {
            self.profiles.extend(other.profiles);
            let profiles: BTreeSet<_> = self.profiles.drain(..).collect();
            self.profiles.extend(profiles);
        }
        if self.events.is_empty() || other.events.is_empty() {
            self.events.clear();
        } else {
            self.events.extend(other.events);
            self.events.sort();
            self.events.dedup();
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
    pub fn retired_bypass_keys(&self) -> Vec<String> {
        self.gates
            .iter()
            .filter(|(_, gate)| gate.bypass.is_some())
            .map(|(name, _)| format!("gate {name}: `bypass` has no effect; remove the key"))
            .collect()
    }

    fn set_danger(&mut self, paths: Vec<String>) {
        self.dangerous = paths.clone();
        for gate in self.gates.values_mut() {
            gate.coverage.dangerous = paths.clone();
        }
    }
    fn refresh_coverage(&mut self) {
        let coverage: BTreeMap<_, _> = self
            .gates
            .iter()
            .map(|(name, gate)| (name.clone(), gate.paths.clone()))
            .collect();
        let mut selection_inputs = self
            .gates
            .values()
            .flat_map(|gate| {
                std::iter::once(gate.command.clone())
                    .chain(gate.environment.clone())
                    .chain(gate.selector.clone())
            })
            .collect::<Vec<_>>();
        selection_inputs.sort();
        selection_inputs.dedup();
        for gate in self.gates.values_mut() {
            if self.selection.base_passes {
                gate.coverage.selection_inputs = selection_inputs.clone();
            }
            gate.coverage.gates = coverage.clone();
            gate.coverage.base_passes = self.selection.base_passes;
        }
    }

    /// Add what a change's own declarations require on top of this set, the
    /// target's. A gate the change adds is owed. A gate the target already
    /// declares stays the target's check: the change can widen its profiles
    /// or tighten its timeout or budget and require a lock but cannot substitute another command or
    /// environment, and such a substitution is returned so it can be reported.
    /// `target_label` and `change_label` name the two sides in that report.
    pub fn owe_also(
        &mut self,
        own: GatesFile,
        target_label: &str,
        change_label: &str,
    ) -> Vec<GateDivergence> {
        crate::gate_budget::merge_rules(&mut self.budget, own.budget);
        let mut divergences = Vec::new();
        for (name, gate) in own.gates {
            match self.gates.get_mut(&name) {
                None => {
                    self.gates.insert(name, gate);
                }
                Some(current) if current.same_check(&gate) => current.absorb(gate),
                Some(current) => {
                    current.bypass = current.bypass.take().or_else(|| gate.bypass.clone());
                    current.absorb_budget(&gate);
                    divergences.push(GateDivergence {
                        target: GateDeclaration::of(current, target_label),
                        change: GateDeclaration::of(&gate, change_label),
                        name,
                    });
                }
            }
        }
        self.set_danger(self.dangerous.clone());
        self.refresh_coverage();
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
        self.required_for_event(profile, GateEvent::Integration)
    }

    pub fn required_for_event<'a>(
        &'a self,
        profile: &str,
        event: GateEvent,
    ) -> Vec<(&'a String, &'a Gate)> {
        self.gates
            .iter()
            .filter(|(_, gate)| {
                (gate.profiles.is_empty() || gate.profiles.iter().any(|item| item == profile))
                    && (gate.events.is_empty() || gate.events.contains(&event))
            })
            .collect()
    }
}

/// Identity of the gate declaration whose evidence can be reused.
///
/// Default bindings and absent coverage settings add no identity fields.
/// Scoped declarations bind the complete coverage map and dangerous patterns,
/// since they determine the fallback for a path no gate names.
pub fn declaration_identity(gate: &Gate) -> String {
    crate::base_pass::digest(&declaration_value(gate))
}

pub(crate) fn declaration_value(gate: &Gate) -> serde_json::Value {
    let mut value = serde_json::json!({
        "command": gate.command, "environment": gate.environment,
        "profiles": gate.profiles, "events": gate.events,
        "timeout": gate.timeout, "selector": gate.selector,
    });
    if gate.coverage.enabled() {
        value["paths"] = serde_json::json!(gate.paths);
        value["coverage"] = serde_json::json!(gate.coverage);
    }
    if gate.binds == Binding::Commit {
        value["binds"] = serde_json::json!("commit");
    }
    value
}

/// Retain structural identity for coverage and budget-raise execution proofs.
pub fn coverage_identity(gate: &Gate) -> Option<String> {
    (gate.coverage.enabled() || gate.budget.is_some()).then(|| declaration_identity(gate))
}

/// Read both sides of renames once for each source and destination tree pair.
pub fn delta_paths(cwd: &Path, source_tree: &str, target_tree: &str) -> Option<Vec<String>> {
    let raw = crate::gitio::git(
        cwd,
        &[
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            source_tree,
            target_tree,
            "--",
        ],
    )
    .ok()?;
    if raw.len() > 65536 || raw.contains('\u{fffd}') {
        return None;
    }
    let paths = raw
        .split('\0')
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect();
    Some(paths)
}

/// Resolve path coverage, then ask a declared selector for an affected gate. Any incomplete,
/// malformed, failed, oversized or timed-out answer leaves the gate to run.
pub fn select_delta(
    cwd: &Path,
    gate: &Gate,
    name: &str,
    source_tree: &str,
    target_tree: &str,
    declared_gates: &[String],
    paths: &[String],
) -> Option<DeltaSelection> {
    let selector = gate.selector.as_deref();
    let request = serde_json::json!({
        "schema": "arc-gate-delta/1", "source_tree": source_tree,
        "target_tree": target_tree, "paths": paths, "gates": declared_gates,
    });
    let protected = paths.iter().any(|path| {
        (gate.coverage.base_passes
            && (crate::base_pass::protected(path)
                || gate
                    .coverage
                    .selection_inputs
                    .iter()
                    .any(|command| command.contains(path))))
            || gate
                .coverage
                .dangerous
                .iter()
                .any(|pattern| crate::policy::glob_match(pattern, path))
            || (!gate.coverage.is_inert(path) && !gate.coverage.is_named(path))
    });
    if protected && gate.coverage.enabled() {
        return None;
    }
    let (selector, output) =
        if gate.coverage.enabled() && !paths.iter().any(|path| gate.coverage.affects(name, path)) {
            let selected: Vec<_> = declared_gates
                .iter()
                .filter(|name| paths.iter().any(|path| gate.coverage.affects(name, path)))
                .collect();
            (
                PATH_SELECTOR,
                serde_json::to_vec(&serde_json::json!({
                    "schema": "arc-gate-selection/1", "complete": true,
                    "request": request, "gates": selected,
                }))
                .ok()?,
            )
        } else {
            let selector = selector?;
            (
                selector,
                selector_output(cwd, selector, &request.to_string(), gate.timeout)
                    .ok()
                    .flatten()?,
            )
        };
    if output.len() > 65536 {
        return None;
    }
    let answer: serde_json::Value = serde_json::from_slice(&output).ok()?;
    if answer.get("schema")?.as_str()? != "arc-gate-selection/1"
        || !answer.get("complete")?.as_bool()?
        || answer.get("request")? != &request
    {
        return None;
    }
    let selected: Vec<String> = answer
        .get("gates")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect::<Option<_>>()?;
    if selected.iter().any(|item| !declared_gates.contains(item))
        || selected.iter().any(|item| item == name)
    {
        return None;
    }
    Some(DeltaSelection {
        against_target: None,
        source_tree: source_tree.to_owned(),
        target_tree: target_tree.to_owned(),
        declaration: declaration_identity(gate),
        selector: selector.to_owned(),
        changed_paths: paths.to_vec(),
        selected_gates: selected,
        output_sha256: format!("sha256:{}", hex::encode(Sha256::digest(&output))),
        output: String::from_utf8(output).ok()?,
    })
}

struct SelectorChild(std::process::Child);

impl Drop for SelectorChild {
    fn drop(&mut self) {
        let _ = kill_process_group(self.0.id());
        let _ = self.0.wait();
    }
}

fn selector_exit_without_reaping(child: &std::process::Child) -> Result<Option<bool>> {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: waitid initializes info; WNOWAIT retains ownership of the PID
    // until SelectorChild stops its group and reaps the leader.
    if unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("cannot observe gate selector");
    }
    // SAFETY: a successful waitid initialized info and the WEXITED fields.
    let info = unsafe { info.assume_init() };
    Ok(unsafe {
        (info.si_pid() != 0).then(|| info.si_code == libc::CLD_EXITED && info.si_status() == 0)
    })
}

fn selector_output(
    cwd: &Path,
    selector: &str,
    input: &str,
    timeout: Option<u64>,
) -> Result<Option<Vec<u8>>> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(
            timeout.unwrap_or(DEFAULT_PROBE_TIMEOUT_SECONDS),
        ))
        .context("gate selector timeout is too large")?;
    let child = Command::new("sh")
        .arg("-c")
        .arg(selector)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .counted_spawn()?;
    let mut child = SelectorChild(child);
    let mut stdin = child
        .0
        .stdin
        .take()
        .context("gate selector stdin unavailable")?;
    let input = input.to_owned();
    let writer =
        std::thread::spawn(move || crate::process_group::write_input(&mut stdin, input.as_bytes()));
    let mut stdout = child
        .0
        .stdout
        .take()
        .context("gate selector stdout unavailable")?;
    let reader = std::thread::spawn(move || -> std::io::Result<(Vec<u8>, bool)> {
        let mut output = Vec::new();
        let mut buffer = [0; 8192];
        let mut oversized = false;
        loop {
            let count = stdout.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            if output.len() + count > 65536 {
                oversized = true;
            }
            let keep = count.min(65536usize.saturating_sub(output.len()));
            output.extend_from_slice(&buffer[..keep]);
        }
        Ok((output, oversized))
    });
    let stderr = child
        .0
        .stderr
        .take()
        .context("gate selector stderr unavailable")?;
    let errors = std::thread::spawn(move || read_tail(stderr, STDERR_TAIL_BYTES));
    let mut status = None;
    loop {
        if status.is_none() {
            status = selector_exit_without_reaping(&child.0)?;
            if status.is_some() {
                kill_process_group(child.0.id())?;
            }
        }
        if status.is_some() && reader.is_finished() && writer.is_finished() && errors.is_finished()
        {
            break;
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output =
        join_before_deadline(reader, Some(deadline), "gate selector stdout")?.transpose()?;
    // A successful write can queue unread bytes; select_delta also requires
    // the answer to acknowledge the complete request.
    let written = join_before_deadline(writer, Some(deadline), "gate selector stdin")?;
    let _ = join_before_deadline(errors, Some(deadline), "gate selector stderr")?.transpose()?;
    match output {
        Some((output, false)) if matches!(written, Some(Ok(()))) && status == Some(true) => {
            Ok(Some(output))
        }
        _ => Ok(None),
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

    /// A change cannot loosen a commit binding its target declares, and a
    /// commit binding is a different declaration from a tree binding while
    /// the default leaves every existing identity as it was.
    #[test]
    fn a_commit_binding_is_kept_by_either_declaration_and_changes_the_identity() {
        let tree = "[gates.log]\ncommand = \"t\"\n";
        let commit = "[gates.log]\ncommand = \"t\"\nbinds = \"commit\"\n";
        for (target, own) in [(tree, commit), (commit, tree)] {
            let mut target = declared(target);
            target.owe_also(declared(own), "master", "change");
            assert_eq!(target.gates["log"].binds, Binding::Commit);
        }

        let tree_gate = &declared(tree).gates["log"];
        assert_eq!(
            declaration_identity(tree_gate),
            declaration_identity(
                &declared("[gates.log]\ncommand = \"t\"\nbinds = \"tree\"\n").gates["log"]
            )
        );
        assert_ne!(
            declaration_identity(tree_gate),
            declaration_identity(&declared(commit).gates["log"])
        );
        assert!(
            toml::from_str::<GatesFile>("[gates.log]\ncommand = \"t\"\nbinds = \"head\"\n")
                .is_err()
        );
    }
}

#[cfg(test)]
mod pipe_deadline_tests {
    use super::*;

    /// A gate selector that exits without reading its whole delta has not
    /// answered for that delta, so the gate runs. Rejects the input writer
    /// meeting the pipe the selector closed and raising SIGPIPE, which under
    /// the disposition `main` restores ends arc silently mid-integration.
    #[test]
    fn a_selector_that_skips_its_delta_leaves_the_gate_to_run() {
        crate::process_group::test_support::under_default_sigpipe(
            "gates::pipe_deadline_tests::a_selector_that_skips_its_delta_leaves_the_gate_to_run",
            || {
                let cwd = tempfile::tempdir().unwrap();
                let delta = "x".repeat(1024 * 1024);
                let output = selector_output(
                    cwd.path(),
                    "printf '{\"schema\":\"arc-gate-selection/1\",\"complete\":true,\"gates\":[]}'",
                    &delta,
                    Some(crate::process_group::test_support::HANG_BOUND.as_secs()),
                )
                .unwrap();
                assert!(
                    output.is_none(),
                    "a selection that never read its delta was used"
                );
            },
        );
    }

    #[test]
    fn inherited_environment_probe_pipes_obey_the_deadline() {
        use crate::process_group::test_support::{HANG_BOUND, OUTLIVING_SECS};
        let cwd = tempfile::tempdir().unwrap();
        for probe in [
            format!("printf identity; sleep {OUTLIVING_SECS} & exit 0"),
            format!("printf identity; sleep {OUTLIVING_SECS} >&2 & exit 0"),
        ] {
            let started = Instant::now();
            let run = environment_probe(cwd.path(), &probe, Some(1)).unwrap();
            assert!(started.elapsed() < HANG_BOUND);
            assert!(run.identity.is_none());
            assert!(run.failure.unwrap().timed_out);
        }
    }

    /// A probe that exits successfully must not leave a descendant running:
    /// arc stops the whole group it started before it returns the identity.
    /// Rejects returning as soon as the shell exits and its pipes close while
    /// a redirected background process lives on.
    #[test]
    fn a_successful_probe_leaves_no_descendant_running() {
        let cwd = tempfile::tempdir().unwrap();
        use crate::process_group::test_support::{ends_within, read_pid, HANG_BOUND};
        let run = environment_probe(
            cwd.path(),
            &format!(
                "sleep {} >/dev/null 2>&1 & echo $! > descendant.pid; printf identity",
                crate::process_group::test_support::OUTLIVING_SECS
            ),
            Some(HANG_BOUND.as_secs()),
        )
        .unwrap();
        let descendant = read_pid(cwd.path(), "descendant.pid");
        let ended = ends_within(descendant, HANG_BOUND);
        if !ended {
            crate::process_group::test_support::kill(descendant);
        }
        assert!(run.identity.is_some());
        assert!(ended, "probe descendant {descendant} outlived the probe");
    }

    /// The probe's group ID stays reserved by its unreaped leader until arc
    /// has signalled the group. Rejects reaping the leader as soon as it
    /// exits: with the group's only other member escaped into its own
    /// session, the reaped ID is free for reuse, and a later kill of the
    /// group would reach whatever process group next takes that number.
    #[test]
    fn the_probe_leader_stays_reserved_until_its_group_is_stopped() {
        use crate::process_group::test_support::{leader_script, LeaderObservation, HANG_BOUND};
        let cwd = tempfile::tempdir().unwrap();
        let path = cwd.path().to_owned();
        let probe = std::thread::spawn(move || {
            environment_probe(
                &path,
                &format!("{}; printf identity", leader_script()),
                Some(HANG_BOUND.as_secs()),
            )
        });
        let observation = LeaderObservation::new(cwd.path());
        let reserved = observation.reserved_after_exit();
        observation.kill_escaped();
        let run = probe.join().unwrap().unwrap();
        assert!(
            reserved,
            "probe leader was reaped while its group could still be signalled"
        );
        assert!(run.identity.is_some());
    }
}
