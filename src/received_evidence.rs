//! Execution evidence received from outside this ledger.
//!
//! A document here is what an external run says it did: the commits it
//! started from, tested and reported on, who and what produced them, the
//! commands it ran and what it saw. Arc records those claims and the
//! references to the receipts behind them, and keeps them apart from its own
//! evaluations. Nothing in a document is a gate result, so nothing in one can
//! satisfy a requirement that asks for an evaluation of the shipped tree.
//!
//! Identities stay as observed. An agent role, a forge account and a Git
//! identity are separate facts of one provenance entry, and no rule here
//! derives one from another: two roles behind one account are two entries.
use crate::forge_observation::RetainedContentRef;
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "arc-received-evidence/1";

/// The evidence sections an empty value must be named for in `omissions`.
const SECTIONS: [&str; 4] = ["provenance", "receipts", "commands", "environment"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceivedEvidence {
    pub schema: String,
    /// The execution's private handle. Opaque to arc.
    pub run_handle: String,
    /// The packet or brief the execution was handed, as its sender names it.
    pub packet: String,
    pub heads: Heads,
    #[serde(default)]
    pub provenance: Vec<Provenance>,
    /// References to the receipts behind the claims. A reference is a claim
    /// about bytes until custody holds them.
    #[serde(default)]
    pub receipts: Vec<RetainedContentRef>,
    #[serde(default)]
    pub commands: Vec<Command>,
    #[serde(default)]
    pub environment: Option<Environment>,
    pub applicability: Applicability,
    /// What the evidence does not carry. Required, so an absence is stated
    /// and never implied; `[]` states there is none.
    pub omissions: Vec<Omission>,
}

impl ReceivedEvidence {
    /// SHA256 of the typed document serialized with sorted object keys.
    pub fn sha256(&self) -> Result<String> {
        use sha2::{Digest, Sha256};
        let mut value = serde_json::to_value(self)?;
        value.sort_all_objects();
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)))
    }

    /// Validate claims against held raw objects without fetching absent heads.
    pub fn validate_held_heads(&self, cwd: &std::path::Path) -> Result<()> {
        self.validate()?;
        for (role, head) in self.heads.each() {
            let Some(kind) = crate::gitio::object_type(cwd, &head.commit)? else {
                continue;
            };
            ensure!(
                kind == "commit",
                "evidence-head-type: the {} head {} is a held {}, not a commit",
                role.as_str(),
                head.commit,
                kind
            );
            let held = crate::gitio::resolve_tree(cwd, &head.commit)?;
            ensure!(held.as_deref() == Some(head.tree.as_str()),
                "evidence-tree-mismatch: the {} head {} is held here with tree {}, not the claimed {}",
                role.as_str(), head.commit, held.as_deref().unwrap_or("none"), head.tree);
        }
        Ok(())
    }
}

/// The commits an execution names, each with its tree. They are claims: the
/// tested head is not the source head because a report was added after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heads {
    pub source: Head,
    pub tested: Head,
    pub report: Head,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    /// The full object ID as the execution reported it.
    pub commit: String,
    pub tree: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HeadRole {
    Source,
    Tested,
    Report,
}

impl HeadRole {
    pub fn as_str(self) -> &'static str {
        match self {
            HeadRole::Source => "source",
            HeadRole::Tested => "tested",
            HeadRole::Report => "report",
        }
    }
}

impl Heads {
    pub fn each(&self) -> [(HeadRole, &Head); 3] {
        [
            (HeadRole::Source, &self.source),
            (HeadRole::Tested, &self.tested),
            (HeadRole::Report, &self.report),
        ]
    }
}

/// What an observer saw of one agent role. Each identity field is its own
/// fact; none stands in for another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub role: AgentRole,
    /// The producer the provider names natively.
    #[serde(default)]
    pub native: Option<NativeProducer>,
    #[serde(default)]
    pub model: Option<ModelObservation>,
    /// The forge account the role acted through.
    #[serde(default)]
    pub forge_account: Option<ForgeAccount>,
    /// The forge account that sent a review.
    #[serde(default)]
    pub review_sender: Option<ForgeAccount>,
    #[serde(default)]
    pub git_author: Option<GitIdentity>,
    #[serde(default)]
    pub git_committer: Option<GitIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentRole {
    Producer,
    Collector,
    Reviewer,
}

impl AgentRole {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentRole::Producer => "producer",
            AgentRole::Collector => "collector",
            AgentRole::Reviewer => "reviewer",
        }
    }
}

/// Where an observation was read, as its observer names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCoordinate {
    pub kind: String,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProducer {
    pub provider: String,
    pub identity: String,
    pub source: SourceCoordinate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelObservation {
    pub name: String,
    #[serde(default)]
    pub effort: Option<String>,
    pub source: SourceCoordinate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeAccount {
    pub provider: String,
    pub host: String,
    pub account: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
}

/// One command the execution reports, with the outcome it reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub command: String,
    pub result: ReportedResult,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// The index in `receipts` of the output receipt, when there is one.
    #[serde(default)]
    pub receipt: Option<usize>,
}

/// A reported outcome. It is no gate result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReportedResult {
    Pass,
    Fail,
    Error,
    NotRun,
}

impl ReportedResult {
    pub fn as_str(self) -> &'static str {
        match self {
            ReportedResult::Pass => "pass",
            ReportedResult::Fail => "fail",
            ReportedResult::Error => "error",
            ReportedResult::NotRun => "not-run",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub identity: String,
    #[serde(default)]
    pub detail: Option<String>,
}

/// Which head the reported results apply to, as the execution states it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Applicability {
    pub applies_to: HeadRole,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Omission {
    pub what: String,
    pub reason: String,
}

fn nonblank(label: &str, text: &str) -> Result<()> {
    ensure!(!text.trim().is_empty(), "{label} is blank");
    Ok(())
}

fn object_id(label: &str, text: &str) -> Result<()> {
    ensure!(
        matches!(text.len(), 40 | 64)
            && text
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "{label} {text:?} is not a full object ID"
    );
    Ok(())
}

impl SourceCoordinate {
    fn validate(&self, label: &str) -> Result<()> {
        nonblank(&format!("{label} source kind"), &self.kind)?;
        nonblank(&format!("{label} source reference"), &self.reference)
    }
}

impl ForgeAccount {
    fn validate(&self, label: &str) -> Result<()> {
        nonblank(&format!("{label} provider"), &self.provider)?;
        nonblank(&format!("{label} host"), &self.host)?;
        nonblank(&format!("{label} account"), &self.account)
    }
}

impl GitIdentity {
    fn validate(&self, label: &str) -> Result<()> {
        nonblank(&format!("{label} name"), &self.name)?;
        nonblank(&format!("{label} email"), &self.email)
    }
}

impl Provenance {
    fn validate(&self, index: usize) -> Result<()> {
        let label = format!("provenance {index} ({})", self.role.as_str());
        ensure!(
            self.native.is_some()
                || self.model.is_some()
                || self.forge_account.is_some()
                || self.review_sender.is_some()
                || self.git_author.is_some()
                || self.git_committer.is_some(),
            "{label} observes nothing"
        );
        if let Some(native) = &self.native {
            nonblank(&format!("{label} native provider"), &native.provider)?;
            nonblank(&format!("{label} native identity"), &native.identity)?;
            native.source.validate(&format!("{label} native"))?;
        }
        if let Some(model) = &self.model {
            nonblank(&format!("{label} model"), &model.name)?;
            if let Some(effort) = &model.effort {
                nonblank(&format!("{label} model effort"), effort)?;
            }
            model.source.validate(&format!("{label} model"))?;
        }
        if let Some(account) = &self.forge_account {
            account.validate(&format!("{label} forge account"))?;
        }
        if let Some(account) = &self.review_sender {
            account.validate(&format!("{label} review sender"))?;
        }
        if let Some(identity) = &self.git_author {
            identity.validate(&format!("{label} git author"))?;
        }
        if let Some(identity) = &self.git_committer {
            identity.validate(&format!("{label} git committer"))?;
        }
        Ok(())
    }
}

impl ReceivedEvidence {
    /// The rules that need no local objects: a supported schema, complete
    /// identifiers, internally consistent heads and command results, and an
    /// omission for every section left empty.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == SCHEMA,
            "unsupported received-evidence schema {:?}; expected {SCHEMA}",
            self.schema
        );
        nonblank("run handle", &self.run_handle)?;
        nonblank("packet reference", &self.packet)?;
        for (role, head) in self.heads.each() {
            object_id(&format!("{} head commit", role.as_str()), &head.commit)?;
            object_id(&format!("{} head tree", role.as_str()), &head.tree)?;
        }
        let heads = self.heads.each();
        for (i, (left_role, left)) in heads.iter().enumerate() {
            for (right_role, right) in &heads[i + 1..] {
                ensure!(
                    left.commit != right.commit || left.tree == right.tree,
                    "the {} and {} heads name commit {} with different trees",
                    left_role.as_str(),
                    right_role.as_str(),
                    left.commit
                );
            }
        }
        for (index, entry) in self.provenance.iter().enumerate() {
            entry.validate(index)?;
        }
        for (index, receipt) in self.receipts.iter().enumerate() {
            receipt
                .validate()
                .map_err(|error| anyhow::anyhow!("receipt {index}: {error}"))?;
        }
        for (index, command) in self.commands.iter().enumerate() {
            nonblank(&format!("command {index}"), &command.command)?;
            if let Some(receipt) = command.receipt {
                ensure!(
                    receipt < self.receipts.len(),
                    "command {index} names receipt {receipt}, but there are {}",
                    self.receipts.len()
                );
            }
            match (command.result, command.exit_code) {
                (ReportedResult::Pass, Some(code)) if code != 0 => {
                    bail!("command {index} reports pass with exit code {code}")
                }
                (ReportedResult::Fail, Some(0)) => {
                    bail!("command {index} reports fail with exit code 0")
                }
                (ReportedResult::NotRun, Some(_)) => {
                    bail!("command {index} reports not-run with an exit code")
                }
                _ => {}
            }
        }
        if let Some(environment) = &self.environment {
            nonblank("environment identity", &environment.identity)?;
        }
        if let Some(note) = &self.applicability.note {
            nonblank("applicability note", note)?;
        }
        for omission in &self.omissions {
            nonblank("omission", &omission.what)?;
            nonblank(
                &format!("reason for omitting {}", omission.what),
                &omission.reason,
            )?;
        }
        for section in SECTIONS {
            let held = match section {
                "provenance" => !self.provenance.is_empty(),
                "receipts" => !self.receipts.is_empty(),
                "commands" => !self.commands.is_empty(),
                _ => self.environment.is_some(),
            };
            ensure!(
                held != self.omitted(section),
                if held {
                    format!("{section} are carried and also listed as omitted")
                } else {
                    format!("{section} are absent and not listed in omissions")
                }
            );
        }
        Ok(())
    }

    pub fn omitted(&self, section: &str) -> bool {
        self.omissions
            .iter()
            .any(|omission| omission.what == section)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";
    const TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    fn head(commit: &str, tree: &str) -> Head {
        Head {
            commit: commit.into(),
            tree: tree.into(),
        }
    }

    fn evidence() -> ReceivedEvidence {
        ReceivedEvidence {
            schema: SCHEMA.into(),
            run_handle: "run-1".into(),
            packet: "packet-1".into(),
            heads: Heads {
                source: head(COMMIT, TREE),
                tested: head(COMMIT, TREE),
                report: head(OTHER, TREE),
            },
            provenance: Vec::new(),
            receipts: Vec::new(),
            commands: Vec::new(),
            environment: None,
            applicability: Applicability {
                applies_to: HeadRole::Tested,
                note: None,
            },
            omissions: SECTIONS
                .iter()
                .map(|section| Omission {
                    what: section.to_string(),
                    reason: "not supplied".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_document_that_states_every_absence_validates() {
        evidence().validate().unwrap();
    }

    #[test]
    fn an_absent_section_must_be_named_and_a_present_one_must_not() {
        let mut silent = evidence();
        silent
            .omissions
            .retain(|omission| omission.what != "environment");
        assert!(silent
            .validate()
            .unwrap_err()
            .to_string()
            .contains("environment are absent"));

        let mut contradicted = evidence();
        contradicted.environment = Some(Environment {
            identity: "image".into(),
            detail: None,
        });
        assert!(contradicted
            .validate()
            .unwrap_err()
            .to_string()
            .contains("environment are carried and also listed as omitted"));
    }

    #[test]
    fn one_commit_cannot_carry_two_trees() {
        let mut split = evidence();
        split.heads.report = head(COMMIT, "0000000000000000000000000000000000000000");
        assert!(split
            .validate()
            .unwrap_err()
            .to_string()
            .contains("different trees"));
    }

    #[test]
    fn a_command_result_cannot_contradict_its_exit_code() {
        let mut contradicted = evidence();
        contradicted
            .omissions
            .retain(|omission| omission.what != "commands");
        contradicted.commands.push(Command {
            command: "make test".into(),
            result: ReportedResult::Pass,
            exit_code: Some(2),
            duration_ms: None,
            receipt: None,
        });
        assert!(contradicted
            .validate()
            .unwrap_err()
            .to_string()
            .contains("pass with exit code 2"));
    }

    #[test]
    fn a_provenance_entry_observes_something() {
        let mut empty = evidence();
        empty
            .omissions
            .retain(|omission| omission.what != "provenance");
        empty.provenance.push(Provenance {
            role: AgentRole::Collector,
            native: None,
            model: None,
            forge_account: None,
            review_sender: None,
            git_author: None,
            git_committer: None,
        });
        assert!(empty
            .validate()
            .unwrap_err()
            .to_string()
            .contains("observes nothing"));
    }
}
