//! Declared gate trailers (`arc-gate-trailers/1`) and target-branch invariant lists.
//!
//! `Witness:` and `Change-size:` trailers are read over `merge-base..head` so a
//! rebased change is judged by its own commits rather than a stale recorded base.
//! Valid declarations bind into gate declaration identity (splitting verification
//! reuse whenever declarations change) and are forwarded to gate commands through
//! `ARC_WITNESSES` and `ARC_CHANGE_SIZE`. Target-branch invariant list files are
//! read from the authorizing revision and forwarded through `ARC_INVARIANTS`.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Component, Path};

/// Convention identifier for gate-facing commit trailers (`Witness:` and `Change-size:`).
pub const GATE_TRAILERS_CONVENTION: &str = "arc-gate-trailers/1";

/// Maximum number of distinct named `Witness:` declarations across `merge-base..head`.
pub const MAX_WITNESSES: usize = 3;

/// Environment variable carrying declared witnesses (newline-separated) to gates.
pub const ENV_WITNESSES: &str = "ARC_WITNESSES";

/// Environment variable carrying declared change size (e.g. `interface`) to gates.
pub const ENV_CHANGE_SIZE: &str = "ARC_CHANGE_SIZE";

/// Environment variable carrying target-branch invariant entries (newline-separated) to gates.
pub const ENV_INVARIANTS: &str = "ARC_INVARIANTS";

/// Declared change-size tier selecting a gate's larger budget when configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeSize {
    Interface,
}

impl ChangeSize {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeSize::Interface => "interface",
        }
    }
}

/// Validated `Witness:` declarations over a change's `merge-base..head` commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WitnessDeclaration {
    /// One to three distinct witness names, in first-seen order.
    Named { witnesses: Vec<String> },
    /// Explicit `Witness: none (<REASON>)` declarations, each with a non-empty
    /// reason, deduplicated in first-seen order.
    None { reasons: Vec<String> },
}

/// Gate-facing commit trailers aggregated over `merge-base..head`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateTrailers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witnesses: Option<WitnessDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_size: Option<ChangeSize>,
}

impl GateTrailers {
    pub fn is_empty(&self) -> bool {
        self.witnesses.is_none() && self.change_size.is_none()
    }

    /// Deterministic value for `ARC_WITNESSES`, or `None` when no `Witness:`
    /// trailer was declared in `merge-base..head`.
    pub fn witnesses_env(&self) -> Option<String> {
        match &self.witnesses {
            None => None,
            Some(WitnessDeclaration::Named { witnesses }) => Some(witnesses.join("\n")),
            Some(WitnessDeclaration::None { reasons }) => Some(
                reasons
                    .iter()
                    .map(|reason| format!("none ({reason})"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }

    /// Deterministic value for `ARC_CHANGE_SIZE`, or `None` when absent.
    pub fn change_size_env(&self) -> Option<&'static str> {
        self.change_size.map(ChangeSize::as_str)
    }
}

/// Target-branch invariant list file resolved at an authorizing tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedInvariants {
    /// Repository-relative path declared by the gate.
    pub path: String,
    /// `sha256:` digest of the file bytes read from the authorizing tree.
    pub sha256: String,
    /// Parsed invariant entries in file order, passed via `ARC_INVARIANTS`.
    pub entries: Vec<String>,
}

/// Environment values bound to one gate execution. Absent fields are explicitly
/// removed from the child process environment so inherited host variables cannot
/// leak into a gate command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateCommandEnv {
    pub witnesses: Option<String>,
    pub change_size: Option<String>,
    pub invariants: Option<String>,
}

impl GateCommandEnv {
    pub fn for_gate(gate: &crate::gates::Gate) -> Self {
        Self {
            witnesses: gate.gate_trailers.witnesses_env(),
            change_size: gate.gate_trailers.change_size_env().map(str::to_owned),
            invariants: gate
                .resolved_invariants
                .as_ref()
                .map(|list| list.entries.join("\n")),
        }
    }

    pub fn apply(&self, command: &mut crate::project_command::Command) {
        match &self.witnesses {
            Some(value) => {
                command.env(ENV_WITNESSES, value);
            }
            None => {
                command.env_remove(ENV_WITNESSES);
            }
        }
        match &self.change_size {
            Some(value) => {
                command.env(ENV_CHANGE_SIZE, value);
            }
            None => {
                command.env_remove(ENV_CHANGE_SIZE);
            }
        }
        match &self.invariants {
            Some(value) => {
                command.env(ENV_INVARIANTS, value);
            }
            None => {
                command.env_remove(ENV_INVARIANTS);
            }
        }
    }
}

fn short_commit(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// Whether `value` attempts the `Witness: none (<REASON>)` form rather than a
/// named witness token (so `none-handler` stays a valid witness name while
/// `none`, `NONE`, `none ()`, `none docs`, and `none(docs)` are validated as
/// the `none` form).
fn is_none_witness_form(value: &str) -> bool {
    if value.eq_ignore_ascii_case("none") {
        return true;
    }
    let Some(prefix) = value.get(..5) else {
        return false;
    };
    prefix.eq_ignore_ascii_case("none ")
        || prefix.eq_ignore_ascii_case("none(")
        || prefix.eq_ignore_ascii_case("none\t")
}

/// Parse `none (<REASON>)`, requiring exact lowercase `none (`, closing `)`,
/// and a non-empty single-line reason free of control characters.
fn parse_none_reason(value: &str) -> Option<String> {
    let inner = value.strip_prefix("none (")?.strip_suffix(')')?;
    let trimmed = inner.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }
    Some(trimmed.to_string())
}

/// Parse and validate `Witness:` and `Change-size:` trailers across an ordered
/// slice of `(commit_sha, raw_message)` pairs (oldest first, matching
/// `gitio::range_messages`).
pub fn parse_messages(
    messages: &[(String, Vec<u8>)],
) -> std::result::Result<GateTrailers, Vec<String>> {
    let mut named: Vec<String> = Vec::new();
    let mut none_reasons: Vec<String> = Vec::new();
    let mut change_size: Option<ChangeSize> = None;
    let mut errors: Vec<String> = Vec::new();

    for (sha, message) in messages {
        let label = short_commit(sha);
        for (key, value, has_continuation) in crate::trailers::block_entries(message) {
            if key.eq_ignore_ascii_case("Witness") {
                if has_continuation || value.contains(['\n', '\r']) {
                    errors.push(format!(
                        "gate trailers ({label}): Witness: value must be a single line"
                    ));
                    continue;
                }
                let value = value.trim();
                if value.is_empty() {
                    errors.push(format!(
                        "gate trailers ({label}): Witness: value must not be empty"
                    ));
                } else if is_none_witness_form(value) {
                    match parse_none_reason(value) {
                        Some(reason) => {
                            if !none_reasons.contains(&reason) {
                                none_reasons.push(reason);
                            }
                        }
                        None => errors.push(format!(
                            "gate trailers ({label}): invalid Witness: {value:?}; \
                             use `Witness: none (<REASON>)` with a non-empty parenthesised reason"
                        )),
                    }
                } else if value
                    .chars()
                    .any(|ch| ch.is_whitespace() || ch.is_control())
                {
                    errors.push(format!(
                        "gate trailers ({label}): invalid Witness: {value:?}; \
                         witness names must be single tokens without whitespace \
                         (or use `Witness: none (<REASON>)`)"
                    ));
                } else if !named.iter().any(|existing| existing == value) {
                    named.push(value.to_string());
                }
            } else if key.eq_ignore_ascii_case("Change-size") {
                if has_continuation || value.contains(['\n', '\r']) {
                    errors.push(format!(
                        "gate trailers ({label}): Change-size: value must be a single line"
                    ));
                    continue;
                }
                let value = value.trim();
                if value == ChangeSize::Interface.as_str() {
                    change_size = Some(ChangeSize::Interface);
                } else {
                    errors.push(format!(
                        "gate trailers ({GATE_TRAILERS_CONVENTION}, {label}): invalid Change-size: {value:?}; \
                         expected \"interface\""
                    ));
                }
            }
        }
    }

    if !named.is_empty() && !none_reasons.is_empty() {
        errors.push(format!(
            "gate trailers: contradictory Witness: declarations in merge-base..head: \
             cannot mix `Witness: none (<REASON>)` with named witnesses ({})",
            named.join(", ")
        ));
    }
    if named.len() > MAX_WITNESSES {
        errors.push(format!(
            "gate trailers: declared {} distinct Witness: values (maximum is {MAX_WITNESSES}): {}",
            named.len(),
            named.join(", ")
        ));
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let witnesses = if !named.is_empty() {
        Some(WitnessDeclaration::Named { witnesses: named })
    } else if !none_reasons.is_empty() {
        Some(WitnessDeclaration::None {
            reasons: none_reasons,
        })
    } else {
        None
    };

    Ok(GateTrailers {
        witnesses,
        change_size,
    })
}

/// Read and validate `Witness:` and `Change-size:` trailers over the change's
/// commits since the merge base with `target_head` (`merge-base..change_head`).
pub fn read_range(
    cwd: &Path,
    target_head: &str,
    change_head: &str,
) -> std::result::Result<GateTrailers, Vec<String>> {
    let base = match crate::gitio::merge_base(cwd, target_head, change_head) {
        Ok(base) => base,
        Err(error) => {
            return Err(vec![format!(
                "gate trailers: cannot resolve merge-base between {target_head} and {change_head}: {error:#}"
            )]);
        }
    };
    let messages = match crate::gitio::range_messages(cwd, &base, change_head) {
        Ok(messages) => messages,
        Err(error) => {
            return Err(vec![format!(
                "gate trailers: cannot read commits in {base}..{change_head}: {error:#}"
            )]);
        }
    };
    parse_messages(&messages)
}

/// Validate a repository-relative invariant list path declared in `.arc/gates.toml`.
pub fn validate_invariants_path(raw: &str) -> Result<String> {
    let path = raw.trim();
    if path.is_empty() {
        bail!("invariants must name a repository-relative file path");
    }
    if path.contains(['\0', '\n', '\r', '\\']) {
        bail!("invariants path {raw:?} contains invalid characters");
    }
    let parsed = Path::new(path);
    if parsed.is_absolute() {
        bail!("invariants path {raw:?} must be relative to the repository root");
    }
    let mut normalized = Vec::new();
    for component in parsed.components() {
        match component {
            Component::Normal(part) => normalized.push(
                part.to_str()
                    .context("invariants path must be valid UTF-8")?,
            ),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                bail!(
                    "invariants path {raw:?} must be a clean repository-relative path without `.` or `..`"
                );
            }
        }
    }
    if normalized.is_empty() {
        bail!("invariants must name a repository-relative file path");
    }
    Ok(normalized.join("/"))
}

/// Parse an invariant list file read from the authorizing tree.
///
/// Supported formats:
/// - `.json`: non-empty JSON array of non-empty single-line strings or objects
///   carrying a non-empty single-line `"check"` (or `"name"`) string, with unique
///   `"name"` values when present.
/// - `.tsv`: tab-separated table (skipping blank lines and `#` comments). When
///   the first row is a header containing `"test"`, extracts `<target>::<test>`
///   (if a `"target"` column is present) or `<test>`; otherwise extracts the
///   first column of each row.
/// - Other extensions: line-oriented list of non-empty entries, ignoring blank
///   lines and `#` comments.
pub fn parse_invariants_file(path: &str, content: &str) -> Result<ResolvedInvariants> {
    let sha256 = format!("sha256:{}", hex::encode(Sha256::digest(content.as_bytes())));
    let lower = path.to_ascii_lowercase();
    let entries = if lower.ends_with(".json") {
        parse_json_invariants(path, content)?
    } else if lower.ends_with(".tsv") {
        parse_tsv_invariants(path, content)?
    } else {
        parse_line_invariants(content)?
    };
    if entries.is_empty() {
        bail!("invariant list {path:?} contains no entries");
    }
    Ok(ResolvedInvariants {
        path: path.to_string(),
        sha256,
        entries,
    })
}

fn validate_entry(raw: &str, path: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains(['\n', '\r']) || trimmed.chars().any(char::is_control)
    {
        bail!("invariant list {path:?} contains an empty or multiline entry");
    }
    Ok(trimmed.to_string())
}

fn parse_json_invariants(path: &str, content: &str) -> Result<Vec<String>> {
    let value: serde_json::Value =
        serde_json::from_str(content).with_context(|| format!("invalid JSON in {path:?}"))?;
    let rows = value
        .as_array()
        .with_context(|| format!("invariant list {path:?} must be a JSON array"))?;
    let mut entries = Vec::with_capacity(rows.len());
    let mut names = BTreeSet::new();
    for (index, row) in rows.iter().enumerate() {
        match row {
            serde_json::Value::String(item) => {
                entries.push(validate_entry(item, path)?);
            }
            serde_json::Value::Object(map) => {
                if let Some(name_value) = map.get("name") {
                    let name = name_value.as_str().with_context(|| {
                        format!("invariant list {path:?}[{index}].name must be a string")
                    })?;
                    let name = validate_entry(name, path)?;
                    if !names.insert(name.clone()) {
                        bail!("invariant list {path:?} has duplicate invariant name {name:?}");
                    }
                }
                let check = map
                    .get("check")
                    .or_else(|| map.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .with_context(|| {
                        format!(
                            "invariant list {path:?}[{index}] must provide a non-empty string \"check\" or \"name\""
                        )
                    })?;
                entries.push(validate_entry(check, path)?);
            }
            _ => bail!("invariant list {path:?}[{index}] must be a string or object"),
        }
    }
    Ok(entries)
}

fn parse_tsv_invariants(path: &str, content: &str) -> Result<Vec<String>> {
    let rows: Vec<&str> = content
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .collect();
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let header_cols: Vec<&str> = rows[0].split('\t').map(str::trim).collect();
    if let Some(test_col) = header_cols.iter().position(|col| *col == "test") {
        let target_col = header_cols.iter().position(|col| *col == "target");
        let mut entries = Vec::with_capacity(rows.len().saturating_sub(1));
        for (index, row) in rows[1..].iter().enumerate() {
            let cols: Vec<&str> = row.split('\t').map(str::trim).collect();
            let test = cols.get(test_col).copied().unwrap_or_default();
            if test.is_empty() {
                bail!(
                    "invariant list {path:?} row {} has an empty test column",
                    index + 2
                );
            }
            let entry = match target_col {
                Some(idx) => {
                    let target = cols.get(idx).copied().unwrap_or_default();
                    if target.is_empty() {
                        bail!(
                            "invariant list {path:?} row {} has an empty target column",
                            index + 2
                        );
                    }
                    format!("{target}::{test}")
                }
                None => test.to_string(),
            };
            entries.push(validate_entry(&entry, path)?);
        }
        return Ok(entries);
    }
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let first = row.split('\t').next().unwrap_or_default();
        entries.push(validate_entry(first, path)?);
    }
    Ok(entries)
}

fn parse_line_invariants(content: &str) -> Result<Vec<String>> {
    let mut entries = Vec::new();
    for line in content.lines() {
        let trimmed = line.strip_suffix('\r').unwrap_or(line).trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.chars().any(char::is_control) {
            bail!("invariant list contains control characters");
        }
        entries.push(trimmed.to_string());
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(sha: &str, text: &str) -> (String, Vec<u8>) {
        (sha.to_string(), text.as_bytes().to_vec())
    }

    #[test]
    fn parses_one_to_three_distinct_witnesses_and_deduplicates_across_commits() {
        let parsed = parse_messages(&[
            msg(
                "1111111111111111",
                "feat: first\n\nWitness: unit:alpha\nWitness: unit:beta\n",
            ),
            msg(
                "2222222222222222",
                "feat: second\n\nWitness: unit:beta\nWitness: script:tests/check.py::Suite.test_x\n",
            ),
        ])
        .unwrap();
        assert_eq!(
            parsed.witnesses,
            Some(WitnessDeclaration::Named {
                witnesses: vec![
                    "unit:alpha".into(),
                    "unit:beta".into(),
                    "script:tests/check.py::Suite.test_x".into(),
                ],
            })
        );
        assert_eq!(
            parsed.witnesses_env().as_deref(),
            Some("unit:alpha\nunit:beta\nscript:tests/check.py::Suite.test_x")
        );
        assert_eq!(parsed.change_size, None);
        assert_eq!(parsed.change_size_env(), None);
    }

    #[test]
    fn refuses_fourth_distinct_witness() {
        let errors = parse_messages(&[
            msg(
                "1111111111111111",
                "feat: first\n\nWitness: a\nWitness: b\nWitness: c\n",
            ),
            msg("2222222222222222", "feat: second\n\nWitness: d\n"),
        ])
        .unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("maximum is 3")),
            "{errors:?}"
        );
    }

    #[test]
    fn parses_none_with_reason_and_refuses_empty_or_mixed_forms() {
        let parsed = parse_messages(&[
            msg(
                "1111111111111111",
                "docs: guide\n\nWitness: none (documentation only)\n",
            ),
            msg(
                "2222222222222222",
                "docs: typo\n\nWitness: none (documentation only)\n",
            ),
        ])
        .unwrap();
        assert_eq!(
            parsed.witnesses,
            Some(WitnessDeclaration::None {
                reasons: vec!["documentation only".into()],
            })
        );
        assert_eq!(
            parsed.witnesses_env().as_deref(),
            Some("none (documentation only)")
        );

        for bad in [
            "docs: x\n\nWitness: none\n",
            "docs: x\n\nWitness: none ()\n",
            "docs: x\n\nWitness: none (   )\n",
            "docs: x\n\nWitness: none docs\n",
            "docs: x\n\nWitness: none(docs)\n",
            "docs: x\n\nWitness: NONE (docs)\n",
            "docs: x\n\nWitness:\n",
            "docs: x\n\nWitness: two words\n",
        ] {
            assert!(
                parse_messages(&[msg("1111111111111111", bad)]).is_err(),
                "expected refusal for {bad:?}"
            );
        }

        // Contradictory mix within one commit or across commits is refused.
        let mixed = parse_messages(&[
            msg("1111111111111111", "docs: x\n\nWitness: none (docs)\n"),
            msg("2222222222222222", "feat: y\n\nWitness: unit:foo\n"),
        ])
        .unwrap_err();
        assert!(
            mixed.iter().any(|e| e.contains("contradictory")),
            "{mixed:?}"
        );

        // Witness names starting with `none-` or `none_` stay valid names.
        let non_exempt = parse_messages(&[msg(
            "1111111111111111",
            "feat: y\n\nWitness: none-handler\n",
        )])
        .unwrap();
        assert_eq!(
            non_exempt.witnesses,
            Some(WitnessDeclaration::Named {
                witnesses: vec!["none-handler".into()],
            })
        );
    }

    #[test]
    fn parses_change_size_interface_and_rejects_invalid_values() {
        let parsed = parse_messages(&[
            msg(
                "1111111111111111",
                "feat: api\n\nChange-size: interface\nWitness: unit:api\n",
            ),
            msg(
                "2222222222222222",
                "feat: api 2\n\nchange-size: interface\n",
            ),
        ])
        .unwrap();
        assert_eq!(parsed.change_size, Some(ChangeSize::Interface));
        assert_eq!(parsed.change_size_env(), Some("interface"));

        for bad in [
            "feat: x\n\nChange-size:\n",
            "feat: x\n\nChange-size: large\n",
            "feat: x\n\nChange-size: Interface\n",
            "feat: x\n\nChange-size: interface\n  continued\n",
        ] {
            assert!(
                parse_messages(&[msg("1111111111111111", bad)]).is_err(),
                "expected refusal for {bad:?}"
            );
        }
    }

    #[test]
    fn ignores_prose_outside_the_final_trailer_block() {
        let parsed = parse_messages(&[msg(
            "1111111111111111",
            "feat: explain\n\nWitness: invalid prose example\nChange-size: huge\n\nImplemented-by: antigravity:model\n",
        )])
        .unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn validates_invariants_paths_and_formats() {
        assert_eq!(
            validate_invariants_path(".arc/invariants.tsv").unwrap(),
            ".arc/invariants.tsv"
        );
        for bad in [
            "",
            "  ",
            "/etc/passwd",
            "../invariants.tsv",
            "./a.json",
            "a/../b",
        ] {
            assert!(validate_invariants_path(bad).is_err(), "{bad:?}");
        }

        let json = r#"[
          {"name": "one", "check": "unit:one"},
          {"name": "two", "check": "script:tests/two.py"}
        ]"#;
        let parsed_json = parse_invariants_file("scripts/check-invariants.json", json).unwrap();
        assert_eq!(parsed_json.entries, vec!["unit:one", "script:tests/two.py"]);
        assert!(parsed_json.sha256.starts_with("sha256:"));

        let dup_json = r#"[{"name": "a", "check": "x"}, {"name": "a", "check": "y"}]"#;
        assert!(parse_invariants_file("scripts/check-invariants.json", dup_json).is_err());
        assert!(parse_invariants_file("scripts/check-invariants.json", "[]").is_err());

        let tsv = "invariant\ttarget\ttest\tclaim\tboundary\nI1\tcli\talpha::beta\tc\tb\nI2\tunit\tgamma::delta\tc\tb\n";
        let parsed_tsv = parse_invariants_file(".arc/invariants.tsv", tsv).unwrap();
        assert_eq!(
            parsed_tsv.entries,
            vec!["cli::alpha::beta", "unit::gamma::delta"]
        );

        let lines = "# comment\n\nunit:first\nunit:second\n";
        let parsed_lines = parse_invariants_file(".arc/invariants.txt", lines).unwrap();
        assert_eq!(parsed_lines.entries, vec!["unit:first", "unit:second"]);
    }
}
