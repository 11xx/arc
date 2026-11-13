//! Patchset-bound proposals to increase repository budget constraints.

use crate::{gate_budget, gates, gitio, model::*, state::ChangeState};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const ALGORITHM: &str = "arc-budget-growth/2";
const PROJECT: &str = ".arc/gates.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountedFile {
    pub source: Option<String>,
    pub path: String,
    pub before: u64,
    pub after: u64,
    pub added: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Increase {
    /// None identifies the integration total.
    pub gate: Option<String>,
    pub old_ms: u64,
    pub new_ms: u64,
    pub added: u64,
    pub allowance_ms: u64,
    pub files: Vec<CountedFile>,
    pub paths: Vec<String>,
    pub inert: Vec<String>,
    pub excluded: Vec<String>,
    pub execution: Option<String>,
    pub declaration_sum_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Raise {
    pub algorithm: String,
    pub change_id: String,
    pub target_branch: String,
    pub target_revision: String,
    pub head: String,
    pub tree: String,
    pub rules_digest: String,
    pub raise_millionths: u64,
    pub raise_per: u64,
    pub operator_total_ceiling_ms: Option<u64>,
    pub increases: Vec<Increase>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub event_id: String,
    pub raise: Raise,
}

#[derive(Debug, Clone, Serialize)]
pub struct Applicability {
    pub record: Record,
    pub applicable: bool,
    pub reason: Option<String>,
}

pub fn digest(value: &impl Serialize) -> Result<String> {
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(value)?))
    ))
}

pub fn project(cwd: &Path, revision: &str) -> Result<gates::GatesFile> {
    gitio::file_at(cwd, revision, PROJECT)?
        .map(|text| toml::from_str(&text).context("malformed committed gate declaration"))
        .transpose()
        .map(Option::unwrap_or_default)
}

pub fn operator(cwd: &Path) -> Result<gates::GatesFile> {
    match std::fs::read_to_string(crate::policy::operator_path(cwd)?) {
        Ok(text) => Ok(toml::from_str(&text)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(e.into()),
    }
}

pub fn ceiling_warning(cwd: &Path, declared: &gates::GatesFile) -> Result<Option<String>> {
    Ok(
        (declared.has_budgets() && operator(cwd)?.budget.and_then(|b| b.total).is_none()).then(
            || "budget: no operator [budget] total ceiling; nothing bounds repeated raises".into(),
        ),
    )
}

/// A percentage is stored in millionths, preserving exact decimal arithmetic.
pub fn percentage<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> std::result::Result<Option<u64>, D::Error> {
    use serde::de::Error;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Percent {
        Text(String),
        Millionths(u64),
    }
    let text = Option::<Percent>::deserialize(de)?;
    text.map(|value| {
        let text = match value {
            Percent::Text(text) => text,
            Percent::Millionths(n) => return Ok(n),
        };
        let number = text
            .strip_suffix('%')
            .ok_or_else(|| D::Error::custom("raise needs a percentage"))?;
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        if whole.is_empty()
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || fraction.len() > 4
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(D::Error::custom(
                "raise needs a nonnegative percentage with at most four decimals",
            ));
        }
        let whole: u64 = whole.parse().map_err(D::Error::custom)?;
        let fraction: u64 = format!("{fraction:0<4}")
            .parse()
            .map_err(D::Error::custom)?;
        whole
            .checked_mul(10_000)
            .and_then(|w| w.checked_add(fraction))
            .ok_or_else(|| D::Error::custom("raise percentage overflow"))
    })
    .transpose()
}

pub fn positive<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> std::result::Result<Option<u64>, D::Error> {
    use serde::de::Error;
    let value = Option::<u64>::deserialize(de)?;
    if value == Some(0) {
        return Err(D::Error::custom("raise_per must be positive"));
    }
    Ok(value)
}

fn entitlement(
    target: &gates::GatesFile,
    own: &gates::GatesFile,
    op: &gates::GatesFile,
) -> (u64, u64) {
    let rules = [
        target.budget.as_ref(),
        own.budget.as_ref(),
        op.budget.as_ref(),
    ];
    let trusted_percent = [rules[0], rules[2]]
        .into_iter()
        .flatten()
        .filter_map(|r| r.raise)
        .min()
        .unwrap_or(200_000);
    let trusted_per = [rules[0], rules[2]]
        .into_iter()
        .flatten()
        .filter_map(|r| r.raise_per)
        .max()
        .unwrap_or(500);
    (
        trusted_percent.min(rules[1].and_then(|r| r.raise).unwrap_or(trusted_percent)),
        trusted_per.max(rules[1].and_then(|r| r.raise_per).unwrap_or(trusted_per)),
    )
}

fn allowance(old: u64, added: u64, percent: u64, per: u64) -> Result<u64> {
    let increment = u128::from(old)
        .checked_mul(u128::from(percent))
        .and_then(|v| v.checked_mul(u128::from(added.min(per))))
        .context("budget allowance overflow")?
        / (1_000_000 * u128::from(per));
    u64::try_from(increment).context("budget allowance overflow")
}

fn matches(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|p| crate::policy::glob_match(p, path))
}

fn raw(cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = gitio::git_command().args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        bail!(
            "budget counting Git failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    if out.stdout.len() > 64 * 1024 * 1024 {
        bail!("budget counting output exceeds 64 MiB");
    }
    Ok(out.stdout)
}

/// Nonblank deltas of logical files; rename/copy detection is pinned.
fn growth(
    cwd: &Path,
    target: &str,
    head: &str,
    paths: &[String],
    operator_paths: &[String],
    inert: &[String],
    excluded: &[String],
) -> Result<Vec<CountedFile>> {
    type Counts = BTreeMap<String, Vec<CountedFile>>;
    static COUNTS: std::sync::OnceLock<std::sync::Mutex<Counts>> = std::sync::OnceLock::new();
    if !crate::bundle::valid_object_id(target) || !crate::bundle::valid_object_id(head) {
        bail!("budget counting needs immutable object IDs");
    }
    let key = digest(&(cwd, target, head, paths, operator_paths, inert, excluded))?;
    let cache = COUNTS.get_or_init(Default::default);
    if let Some(files) = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("budget count cache unavailable"))?
        .get(&key)
    {
        return Ok(files.clone());
    }
    let output = raw(
        cwd,
        &[
            "-c",
            "diff.renames=true",
            "-c",
            "diff.renameLimit=0",
            "diff",
            "--raw",
            "--text",
            "--no-relative",
            "-O/dev/null",
            "--diff-algorithm=myers",
            "--no-indent-heuristic",
            "--no-abbrev",
            "-z",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames=50%",
            "--find-copies=50%",
            "--find-copies-harder",
            "-l0",
            target,
            head,
            "--",
        ],
    )?;
    let mut fields = output.split(|b| *b == 0).peekable();
    let mut files = Vec::new();
    let mut blobs = None;
    let eligible = |path: &str| {
        [paths, operator_paths]
            .into_iter()
            .all(|rules| rules.is_empty() || matches(rules, path))
            && !matches(inert, path)
            && !matches(excluded, path)
    };
    while let Some(header) = fields.next().filter(|v| !v.is_empty()) {
        let header = std::str::from_utf8(header)?;
        let parts = header
            .trim_start_matches(':')
            .split_whitespace()
            .collect::<Vec<_>>();
        if parts.len() != 5 {
            bail!("invalid budget counting diff header");
        }
        let first = std::str::from_utf8(fields.next().context("missing budget counting path")?)?;
        let moved = parts[4].starts_with(['R', 'C']);
        let destination = if moved {
            std::str::from_utf8(
                fields
                    .next()
                    .context("missing budget counting destination")?,
            )?
        } else {
            first
        };
        let departure = parts[4].starts_with('R') && eligible(first) && !eligible(destination);
        let path = if departure { first } else { destination };
        let regular_before = parts[0].starts_with("100");
        let regular_after = !departure && parts[1].starts_with("100");
        if (!regular_before && !regular_after) || !eligible(path) || (moved && !eligible(first)) {
            continue;
        }
        if blobs.is_none() {
            blobs = Some(gitio::BlobReader::new(cwd)?);
        }
        let blobs = blobs.as_mut().unwrap();
        let after = if regular_after {
            blobs.read(parts[3])?
        } else {
            Vec::new()
        };
        let before = if regular_before {
            blobs.read(parts[2])?
        } else {
            Vec::new()
        };
        if after.contains(&0) || before.contains(&0) {
            continue;
        }
        let (Ok(before), Ok(after)) = (std::str::from_utf8(&before), std::str::from_utf8(&after))
        else {
            continue;
        };
        if before
            .chars()
            .filter(|c| !c.is_whitespace())
            .eq(after.chars().filter(|c| !c.is_whitespace()))
        {
            continue;
        }
        let count = |text: &str| text.lines().filter(|s| !s.trim().is_empty()).count() as u64;
        let (old, new) = (count(before), count(after));
        if new != old && (!parts[4].starts_with('C') || new > old) {
            files.push(CountedFile {
                source: (parts[0] != "000000").then(|| first.into()),
                path: path.into(),
                before: old,
                after: new,
                added: new.saturating_sub(old),
            });
        }
    }
    if !files.is_empty() {
        let paths = files
            .iter()
            .flat_map(|file| std::iter::once(file.path.clone()).chain(file.source.clone()))
            .collect::<BTreeSet<_>>();
        let binary = gitio::budget_binary_paths(cwd, target, head, &paths)?;
        files.retain(|file| {
            !binary.contains(&file.path)
                && file
                    .source
                    .as_ref()
                    .is_none_or(|source| !binary.contains(source))
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    if files.len() <= 2048 {
        let mut cache = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("budget count cache unavailable"))?;
        if cache.len() >= 32 {
            cache.pop_first();
        }
        cache.insert(key, files.clone());
    }
    Ok(files)
}

fn net_growth<'a>(mut files: impl Iterator<Item = &'a CountedFile>) -> Result<u64> {
    let (growth, shrinkage) = files
        .try_fold((0_u64, 0_u64), |(growth, shrinkage), file| {
            Some((
                growth.checked_add(file.after.saturating_sub(file.before))?,
                shrinkage.checked_add(file.before.saturating_sub(file.after))?,
            ))
        })
        .context("line count overflow")?;
    Ok(growth.saturating_sub(shrinkage))
}

fn scope(
    target: &gates::GatesFile,
    op: &gates::GatesFile,
    gate: Option<&str>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut inert = target.selection.inert.clone();
    inert.extend(op.selection.inert.clone());
    inert.sort();
    inert.dedup();
    let paths = gate
        .and_then(|name| target.gates.get(name))
        .map(|g| g.paths.clone())
        .unwrap_or_default();
    (
        paths,
        inert,
        op.budget
            .as_ref()
            .map(|b| b.exclude.clone())
            .unwrap_or_default(),
    )
}

fn gate_paths<'a>(file: &'a gates::GatesFile, gate: Option<&str>) -> &'a [String] {
    gate.and_then(|name| file.gates.get(name))
        .map(|gate| gate.paths.as_slice())
        .unwrap_or_default()
}

fn head_credits(file: &CountedFile, own: &gates::GatesFile, gate: Option<&str>) -> bool {
    let paths = gate_paths(own, gate);
    file.after < file.before
        || (!matches(&own.selection.inert, &file.path)
            && (paths.is_empty() || matches(paths, &file.path)))
}

fn total(file: &gates::GatesFile) -> Option<u64> {
    file.budget.as_ref().and_then(|b| b.total)
}

fn transitions(
    target: &gates::GatesFile,
    own: &gates::GatesFile,
) -> Vec<(Option<String>, u64, u64)> {
    let mut found = Vec::new();
    for (name, gate) in &target.gates {
        if let Some(old) = gate.budget {
            let new = own.gates.get(name).and_then(|g| g.budget);
            if let Some(new) = new.filter(|new| *new > old) {
                found.push((Some(name.clone()), old, new));
            }
        }
    }
    if let Some(old) = total(target) {
        let new = total(own);
        if let Some(new) = new.filter(|new| *new > old) {
            found.push((None, old, new));
        }
    }
    found
}

fn invalid_removals(target: &gates::GatesFile, own: &gates::GatesFile) -> BTreeSet<Option<String>> {
    let mut removed = target
        .gates
        .iter()
        .filter(|&(name, gate)| {
            gate.budget.is_some()
                && own
                    .gates
                    .get(name)
                    .is_some_and(|gate| gate.budget.is_none())
        })
        .map(|(name, _)| Some(name.clone()))
        .collect::<BTreeSet<_>>();
    if total(target).is_some() && total(own).is_none() && !own.gates.is_empty() {
        removed.insert(None);
    }
    removed
}

fn rules_digest(
    target: &gates::GatesFile,
    own: &gates::GatesFile,
    op: &gates::GatesFile,
) -> Result<String> {
    let scopes = target
        .gates
        .keys()
        .map(|name| (name.clone(), scope(target, op, Some(name))))
        .collect::<BTreeMap<_, _>>();
    let structural = target
        .gates
        .iter()
        .map(|(name, gate)| (name, gates::declaration_identity(gate)))
        .collect::<BTreeMap<_, _>>();
    digest(&(
        ALGORITHM,
        entitlement(target, own, op),
        scopes,
        scope(target, op, None),
        &own.selection.inert,
        structural,
        total(op),
    ))
}

/// Re-derive a proposal from receiver policy, retained objects and direct executions.
pub fn propose(
    cwd: &Path,
    st: &ChangeState,
    target_revision: &str,
    head: &str,
    subjects: &[Option<String>],
    reason: &str,
) -> Result<Raise> {
    if reason.trim().is_empty() {
        bail!("budget raise needs a reason saying what was tried first");
    }
    if !gitio::is_ancestor(cwd, target_revision, head)? {
        bail!("budget raise requires rebasing onto the destination before counting");
    }
    if !gitio::is_ancestor(cwd, &st.base, target_revision)? {
        bail!("budget raise requires the destination to contain the change base; integrate prerequisites before counting");
    }
    let target = project(cwd, target_revision)?;
    let own = project(cwd, head)?;
    if let Some(gate) = invalid_removals(&target, &own).iter().next() {
        bail!("{}", gate_budget::removal_error(gate.as_deref()));
    }
    let op = operator(cwd)?;
    if let Some(ceiling) = total(&op) {
        if total(&target)
            .zip(total(&own))
            .is_some_and(|(old, new)| new > old && new > ceiling)
        {
            bail!(
                "budget raise exceeds operator total ceiling {}",
                gate_budget::duration(ceiling)
            );
        }
    }
    let mut effective = gates::load_at(cwd, target_revision)?;
    effective.owe_also(gates::load_at(cwd, head)?, "destination", "head");
    let tree = gitio::commit_tree(cwd, head)?;
    let (percent, per) = entitlement(&target, &own, &op);
    let changes = transitions(&target, &own);
    let mut increases = Vec::new();
    for subject in subjects {
        let label = subject.as_deref().unwrap_or("total");
        let (_, old, new) = changes
            .iter()
            .find(|(gate, _, _)| gate == subject)
            .with_context(|| format!("{label}: head must declare a higher existing budget"))?;
        let new = *new;
        let (paths, inert, excluded) = scope(&target, &op, subject.as_deref());
        let mut files = growth(
            cwd,
            target_revision,
            head,
            &paths,
            gate_paths(&op, subject.as_deref()),
            &inert,
            &excluded,
        )?;
        files.retain(|file| head_credits(file, &own, subject.as_deref()));
        let added = net_growth(files.iter())?;
        let allowance_ms = allowance(*old, added, percent, per)?;
        if new - old > allowance_ms {
            bail!(
                "{label}: raise exceeds allowance {} ({} eligible lines)",
                gate_budget::duration(allowance_ms),
                added
            );
        }
        let mut execution = None;
        let mut declaration_sum_ms = None;
        if let Some(name) = subject {
            let gate = effective
                .gates
                .get(name)
                .context("target gate unavailable")?;
            let entry = st
                .verifications
                .iter()
                .rev()
                .find(|e| {
                    st.direct_executions.contains_key(&e.event_id)
                        && e.gate.as_deref() == Some(name)
                        && e.recorded_revision == head
                })
                .context("budget raise needs the newest direct execution at this exact head")?;
            let run = entry
                .timing
                .run_ms
                .context("budget raise needs observed run_ms")?;
            if entry.result != VerifyResult::Pass
                || entry.attested
                || st.direct_executions.get(&entry.event_id) != Some(&Some(0))
                || (gate.environment.is_some() && entry.environment.is_none())
                || entry.probe.is_some()
                || !entry.green_at_head(None)
                || entry.worktree_dirty != Some(false)
                || entry.tested_tree.as_deref() != Some(&tree)
                || entry.against_target.is_some()
                || (!gate.events.is_empty()
                    && !gate
                        .events
                        .contains(&crate::state::verification_event(st, entry)))
                || !crate::status::matches_declaration(entry, gate)
                || entry.declaration.as_ref() != Some(&gates::declaration_identity(gate))
                || run <= *old
                || run > new
            {
                bail!("{name}: newest direct execution must be clean, passing, over the target budget and inside the raised budget under the target declaration");
            }
            execution = Some(entry.event_id.clone());
        } else {
            let mut suite = gates::load_at(cwd, head)?;
            suite.owe_also(gates::load_at(cwd, target_revision)?, "head", "target");
            for (name, gate) in &mut suite.gates {
                if let Some(new) = own.gates.get(name).and_then(|g| g.budget) {
                    gate.budget =
                        Some(new.min(op.gates.get(name).and_then(|g| g.budget).unwrap_or(new)));
                }
            }
            let profiles = std::iter::once(String::new())
                .chain(suite.gates.values().flat_map(|g| g.profiles.clone()))
                .collect::<BTreeSet<_>>();
            let max = profiles
                .iter()
                .map(|p| crate::gate_concurrency::total(&suite.required_for(p)))
                .collect::<Option<Vec<_>>>()
                .context("total raise needs budgets on the complete integration suite")?
                .into_iter()
                .max()
                .unwrap_or(0);
            if max <= *old || max > new {
                bail!("total: declaration sum must exceed the old total and fit the raised total");
            }
            declaration_sum_ms = Some(max);
        }
        increases.push(Increase {
            gate: subject.clone(),
            old_ms: *old,
            new_ms: new,
            added,
            allowance_ms,
            files,
            paths,
            inert,
            excluded,
            execution,
            declaration_sum_ms,
        });
    }
    Ok(Raise {
        algorithm: ALGORITHM.into(),
        change_id: st.change_id.clone(),
        target_branch: st.target_branch.clone(),
        target_revision: target_revision.into(),
        head: head.into(),
        tree,
        rules_digest: rules_digest(&target, &own, &op)?,
        raise_millionths: percent,
        raise_per: per,
        operator_total_ceiling_ms: total(&op),
        increases,
        reason: reason.trim().into(),
    })
}

fn substitute(
    required: &mut gates::GatesFile,
    target_revision: &str,
    record: &Record,
) -> Result<()> {
    let source = format!("{PROJECT} at {target_revision}");
    let provenance = format!(
        "accepted budget raise {} {}",
        record.event_id,
        digest(&record.raise)?
    );
    for increase in &record.raise.increases {
        let sources = if let Some(name) = &increase.gate {
            &mut required
                .gates
                .get_mut(name)
                .context("raise gate missing")?
                .budget_sources
        } else {
            &mut required
                .budget
                .as_mut()
                .context("raise total missing")?
                .sources
        };
        for entry in sources
            .iter_mut()
            .filter(|s| s.source == source && s.milliseconds == increase.old_ms)
        {
            entry.milliseconds = increase.new_ms;
            entry.source = provenance.clone();
        }
        let minimum = sources.iter().map(|s| s.milliseconds).min();
        if let Some(name) = &increase.gate {
            required.gates.get_mut(name).unwrap().budget = minimum;
        } else {
            required.budget.as_mut().unwrap().total = minimum;
        }
    }
    Ok(())
}

/// Resolve the repository transition before folding the head's additional minima.
pub fn apply(
    cwd: &Path,
    st: &ChangeState,
    target_revision: &str,
    head: &str,
    required: &mut gates::GatesFile,
) -> Result<()> {
    let target = project(cwd, target_revision)?;
    let own = project(cwd, head)?;
    required
        .invalid_budget_removals
        .extend(invalid_removals(&target, &own));
    let changes = transitions(&target, &own);
    if changes.is_empty() && st.budget_raises.is_empty() {
        return Ok(());
    }
    let mut accepted = BTreeSet::new();
    for record in &st.budget_raises {
        let subjects = record
            .raise
            .increases
            .iter()
            .map(|i| i.gate.clone())
            .collect::<Vec<_>>();
        let result = if record.raise.head != head
            || record.raise.target_revision != target_revision
            || record.raise.target_branch != st.target_branch
            || record.raise.change_id != st.change_id
        {
            Err(anyhow::anyhow!(
                "budget raise is stale: exact head or destination changed"
            ))
        } else {
            let mut issuance = st.clone();
            issuance
                .verifications
                .retain(|e| e.event_id < record.event_id);
            propose(
                cwd,
                &issuance,
                target_revision,
                head,
                &subjects,
                &record.raise.reason,
            )
            .and_then(|fresh| {
                if fresh != record.raise {
                    bail!("budget raise counting, execution or policy basis changed");
                }
                Ok(())
            })
        };
        let applicable = result.is_ok();
        required.raises.push(Applicability {
            record: record.clone(),
            applicable,
            reason: result.err().map(|e| format!("{e:#}")),
        });
        if applicable {
            substitute(required, target_revision, record)?;
            accepted.extend(subjects);
        }
    }
    for (gate, _, _) in changes {
        if !accepted.contains(&gate) {
            required.pending_raises.insert(gate);
        }
    }
    let ids = st
        .budget_raises
        .iter()
        .map(|r| r.event_id.clone())
        .collect::<Vec<_>>();
    if !accepted.is_empty()
        && st
            .latest_patchset()
            .is_none_or(|p| p.budget_raise_ids != ids)
    {
        required.raise_snapshot_missing = true;
    }
    Ok(())
}

fn advice(
    cwd: &Path,
    st: &ChangeState,
    name: &str,
    target_revision: &str,
    head: &str,
) -> Result<Option<(u64, u64)>> {
    if !gitio::is_ancestor(cwd, target_revision, head)?
        || !gitio::is_ancestor(cwd, &st.base, target_revision)?
    {
        return Ok(None);
    }
    let target = project(cwd, target_revision)?;
    let own = project(cwd, head)?;
    let op = operator(cwd)?;
    let Some(old) = target.gates.get(name).and_then(|g| g.budget) else {
        return Ok(None);
    };
    let (paths, inert, excluded) = scope(&target, &op, Some(name));
    let files = growth(
        cwd,
        target_revision,
        head,
        &paths,
        gate_paths(&op, Some(name)),
        &inert,
        &excluded,
    )?;
    let added = net_growth(
        files
            .iter()
            .filter(|file| head_credits(file, &own, Some(name))),
    )?;
    let (percent, per) = entitlement(&target, &own, &op);
    let increment = allowance(old, added, percent, per)?;
    Ok(Some((
        increment,
        old.checked_add(increment)
            .context("maximum budget overflow")?,
    )))
}

/// Refusal advice shares the immutable counting basis used by raise proposals.
pub fn fill_advice(
    cwd: &Path,
    st: &ChangeState,
    name: &str,
    judgement: &mut gate_budget::BudgetJudgement,
) -> Result<()> {
    if !judgement.accepted() && judgement.run_ms.is_some() && judgement.raise_allowance_ms.is_none()
    {
        let target = crate::declarations::authorizing_revision(cwd, st, None)?;
        let head = if st.is_closed() {
            st.closed_head()
                .map(str::to_owned)
                .context("closed head unavailable")?
        } else {
            gitio::branch_head(cwd, &st.branch)?
        };
        if let Some((increment, maximum)) = advice(cwd, st, name, &target, &head)? {
            judgement.raise_allowance_ms = Some(increment);
            judgement.raise_budget_max_ms = Some(maximum);
        }
    }
    Ok(())
}

/// Count only when an observed refusal needs actionable advice.
pub fn advise(
    cwd: &Path,
    st: &ChangeState,
    target_revision: &str,
    head: &str,
    required: &mut gates::GatesFile,
) -> Result<()> {
    for (name, gate) in &mut required.gates {
        let run = st
            .verifications
            .iter()
            .rev()
            .find(|e| e.gate.as_ref() == Some(name))
            .and_then(|e| e.timing.run_ms);
        let mut judgement = gate.budget_judgement(run);
        if !judgement.accepted() && judgement.run_ms.is_some() {
            if let Some((increment, maximum)) = advice(cwd, st, name, target_revision, head)? {
                judgement.raise_allowance_ms = Some(increment);
                judgement.raise_budget_max_ms = Some(maximum);
            }
        }
        gate.raise_allowance_ms = judgement.raise_allowance_ms;
        gate.raise_budget_max_ms = judgement.raise_budget_max_ms;
    }
    Ok(())
}

/// An evaluated tree without a matching change record cannot authorize a raise.
pub fn require_records(
    cwd: &Path,
    target_revision: &str,
    head: &str,
    required: &mut gates::GatesFile,
) -> Result<()> {
    let target = project(cwd, target_revision)?;
    let own = project(cwd, head)?;
    required
        .invalid_budget_removals
        .extend(invalid_removals(&target, &own));
    required.pending_raises.extend(
        transitions(&target, &own)
            .into_iter()
            .map(|(gate, _, _)| gate),
    );
    Ok(())
}
