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
    #[serde(default)]
    pub events: Vec<GateEvent>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default, deserialize_with = "deserialize_timeout")]
    pub timeout: Option<u64>,
    /// What qualifies a change to skip this gate on an agent's judgement, as
    /// `arc verify --bypass <reason>` records. Absent, the gate cannot be
    /// bypassed. It decides nothing about what the command checks, so it is
    /// no part of the declaration's identity.
    #[serde(default, deserialize_with = "deserialize_bypass")]
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
        match &self.selector {
            Some(selector) => format!("{description} with delta selector {selector:?}"),
            None => description,
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

fn deserialize_bypass<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<String>::deserialize(deserializer)? {
        Some(qualification) if qualification.trim().is_empty() => Err(D::Error::custom(
            "bypass must name what qualifies a change to skip the gate",
        )),
        qualification => Ok(qualification.map(|text| text.trim().to_owned())),
    }
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
                                selector: current.selector.clone(),
                                command: current.command.clone(),
                                environment: current.environment.clone(),
                                source: current.declared_by[0].clone(),
                            },
                            GateDeclaration {
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

    Ok(merged)
}

impl Gate {
    /// Whether two declarations name the same check, environment and
    /// project selection contract.
    fn same_check(&self, other: &Gate) -> bool {
        self.command == other.command
            && self.environment == other.environment
            && self.selector == other.selector
    }

    /// Fold another declaration of the same check into this one. Profiles
    /// and events widen and the stricter timeout wins, so one declaration cannot loosen
    /// a gate the other bounds. The gate may be bypassed only if both allow it,
    /// on this declaration's terms, and binds to commits if either says so.
    fn absorb(&mut self, other: Gate) {
        if other.bypass.is_none() {
            self.bypass = None;
        }
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
/// A tree binding is the default and adds nothing, so the identity of every
/// declaration that does not name `binds` is the one it has always had.
pub fn declaration_identity(gate: &Gate) -> String {
    let mut value = serde_json::json!({
        "command": gate.command, "environment": gate.environment,
        "profiles": gate.profiles, "events": gate.events,
        "timeout": gate.timeout, "selector": gate.selector,
    });
    if gate.binds == Binding::Commit {
        value["binds"] = serde_json::json!("commit");
    }
    format!(
        "sha256:{}",
        hex::encode(Sha256::digest(value.to_string().as_bytes()))
    )
}

/// Ask a project selector whether the delta affects one gate. Any incomplete,
/// malformed, failed, oversized or timed-out answer leaves the gate to run.
pub fn select_delta(
    cwd: &Path,
    gate: &Gate,
    name: &str,
    source_tree: &str,
    target_tree: &str,
    declared_gates: &[String],
) -> Option<DeltaSelection> {
    let selector = gate.selector.as_deref()?;
    let raw = crate::gitio::git(
        cwd,
        &["diff", "--name-only", "-z", source_tree, target_tree, "--"],
    )
    .ok()?;
    if raw.len() > 65536 || raw.contains('\u{fffd}') {
        return None;
    }
    let paths: Vec<String> = raw
        .split('\0')
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect();
    let input = serde_json::json!({
        "schema": "arc-gate-delta/1", "source_tree": source_tree,
        "target_tree": target_tree, "paths": paths, "gates": declared_gates,
    })
    .to_string();
    let output = selector_output(cwd, selector, &input, gate.timeout)
        .ok()
        .flatten()?;
    let answer: serde_json::Value = serde_json::from_slice(&output).ok()?;
    if answer.get("schema")?.as_str()? != "arc-gate-selection/1"
        || !answer.get("complete")?.as_bool()?
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
        changed_paths: paths,
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
    use std::io::Write;
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
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
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
    let written =
        join_before_deadline(writer, Some(deadline), "gate selector stdin")?.transpose()?;
    let _ = join_before_deadline(errors, Some(deadline), "gate selector stderr")?.transpose()?;
    match output {
        Some((output, false)) if written.is_some() && status == Some(true) => Ok(Some(output)),
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

    /// A change cannot let itself skip a gate its target runs, and a
    /// declaration that omits `bypass` withdraws it.
    #[test]
    fn a_gate_may_be_bypassed_only_where_both_declarations_allow_it() {
        let strict = "[gates.docs]\ncommand = \"t\"\n";
        let lax = "[gates.docs]\ncommand = \"t\"\nbypass = \"docs only\"\n";
        let mut target = declared(strict);
        target.owe_also(declared(lax), "master", "change");
        assert_eq!(target.gates["docs"].bypass, None);

        let mut target = declared(lax);
        target.owe_also(declared(strict), "master", "change");
        assert_eq!(target.gates["docs"].bypass, None);

        let mut target = declared(lax);
        target.owe_also(
            declared("[gates.docs]\ncommand = \"t\"\nbypass = \"anything\"\n"),
            "master",
            "change",
        );
        assert_eq!(target.gates["docs"].bypass.as_deref(), Some("docs only"));

        assert!(
            toml::from_str::<GatesFile>("[gates.docs]\ncommand = \"t\"\nbypass = \" \"\n").is_err()
        );
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

    #[test]
    fn inherited_environment_probe_pipes_obey_the_deadline() {
        let cwd = tempfile::tempdir().unwrap();
        for probe in [
            "printf identity; sleep 5 & exit 0",
            "printf identity; sleep 5 >&2 & exit 0",
        ] {
            let started = Instant::now();
            let run = environment_probe(cwd.path(), probe, Some(1)).unwrap();
            assert!(started.elapsed() < Duration::from_secs(3));
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
        let run = environment_probe(
            cwd.path(),
            "sleep 30 >/dev/null 2>&1 & echo $! > descendant.pid; printf identity",
            Some(5),
        )
        .unwrap();
        let descendant = crate::process_group::test_support::read_pid(cwd.path(), "descendant.pid");
        let ended =
            crate::process_group::test_support::ends_within(descendant, Duration::from_secs(2));
        if !ended {
            crate::process_group::test_support::kill(descendant);
        }
        assert!(run.identity.is_some());
        assert!(ended, "probe descendant {descendant} outlived the probe");
    }

    /// The probe's group ID stays reserved by its unreaped leader until arc
    /// has signalled the group. Rejects reaping the leader as soon as it
    /// exits: with the group's only other member escaped into its own
    /// session, the reaped ID is free for reuse, and the deadline's kill
    /// would reach whatever process group next takes that number.
    #[test]
    fn the_probe_leader_stays_reserved_until_its_group_is_stopped() {
        let cwd = tempfile::tempdir().unwrap();
        let path = cwd.path().to_owned();
        let probe = std::thread::spawn(move || {
            environment_probe(
                &path,
                "echo $$ > leader.pid; \
                 setsid sh -c 'echo $$ > escaped.pid; exec sleep 6' & printf identity",
                Some(3),
            )
        });
        let support = &crate::process_group::test_support::LeaderObservation::new(cwd.path());
        let reserved = support.reserved_after_exit();
        let run = probe.join().unwrap().unwrap();
        support.kill_escaped();
        assert!(
            reserved,
            "probe leader was reaped while its group could still be signalled"
        );
        assert!(run.failure.unwrap().timed_out);
    }
}
