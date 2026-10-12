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
    /// [`Self::validate`] and the rules a document must also meet to be
    /// admitted: a coordinate or an identity is a value, never a placeholder,
    /// and a model, effort or source coordinate is no session id. Absence is
    /// a field left out. A recorded document reduces under `validate` alone,
    /// so a record written earlier stays readable.
    pub fn validate_admission(&self) -> Result<()> {
        self.validate()?;
        for (index, entry) in self.provenance.iter().enumerate() {
            entry.validate_admission(index)?;
        }
        if let Some(environment) = &self.environment {
            established("environment identity", &environment.identity)?;
        }
        Ok(())
    }

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
        match self.head_mismatches(cwd)?.into_iter().next() {
            Some(mismatch) => bail!("{}: {}", mismatch.code(), mismatch.detail()),
            None => Ok(()),
        }
    }

    /// The heads whose commit this repository holds and contradicts: held as
    /// something other than a commit, or with a tree other than the claimed
    /// one. An absent commit contradicts nothing.
    pub fn head_mismatches(&self, cwd: &std::path::Path) -> Result<Vec<HeadMismatch>> {
        let mut found = Vec::new();
        for (role, head) in self.heads.each() {
            let Some(kind) = crate::gitio::object_type(cwd, &head.commit)? else {
                continue;
            };
            if kind != "commit" {
                found.push(HeadMismatch {
                    role,
                    commit: head.commit.clone(),
                    problem: HeadProblem::NotACommit { held: kind },
                });
                continue;
            }
            let held = crate::gitio::resolve_tree(cwd, &head.commit)?;
            if held.as_deref() != Some(head.tree.as_str()) {
                found.push(HeadMismatch {
                    role,
                    commit: head.commit.clone(),
                    problem: HeadProblem::TreeDiffers {
                        held,
                        claimed: head.tree.clone(),
                    },
                });
            }
        }
        Ok(found)
    }
}

/// A claimed head that the objects held here contradict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadMismatch {
    pub role: HeadRole,
    pub commit: String,
    pub problem: HeadProblem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadProblem {
    NotACommit {
        held: String,
    },
    TreeDiffers {
        held: Option<String>,
        claimed: String,
    },
}

impl HeadMismatch {
    pub fn code(&self) -> &'static str {
        match self.problem {
            HeadProblem::NotACommit { .. } => "evidence-head-type",
            HeadProblem::TreeDiffers { .. } => "evidence-tree-mismatch",
        }
    }

    /// What the objects held here say against the claim. Object IDs and
    /// kinds only.
    pub fn detail(&self) -> String {
        let role = self.role.as_str();
        match &self.problem {
            HeadProblem::NotACommit { held } => {
                format!(
                    "the {role} head {} is a held {held}, not a commit",
                    self.commit
                )
            }
            HeadProblem::TreeDiffers { held, claimed } => format!(
                "the {role} head {} is held here with tree {}, not the claimed {claimed}",
                self.commit,
                held.as_deref().unwrap_or("none"),
            ),
        }
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

/// A value that names something: no placeholder spelling.
fn established(label: &str, text: &str) -> Result<()> {
    ensure!(
        !crate::coordinate::is_placeholder(text),
        "{label} {text:?} is a placeholder; leave the field out when it is not established"
    );
    Ok(())
}

/// A model, effort or source coordinate: established and no session id.
fn coordinate(label: &str, text: &str) -> Result<()> {
    established(label, text)?;
    ensure!(
        !crate::coordinate::is_session_id(text, None),
        "{label} {text:?} is a session id, not a coordinate"
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

impl SourceCoordinate {
    fn validate_admission(&self, label: &str) -> Result<()> {
        coordinate(&format!("{label} source kind"), &self.kind)?;
        coordinate(&format!("{label} source reference"), &self.reference)
    }
}

impl Provenance {
    fn validate_admission(&self, index: usize) -> Result<()> {
        let label = format!("provenance {index} ({})", self.role.as_str());
        if let Some(native) = &self.native {
            established(&format!("{label} native provider"), &native.provider)?;
            established(&format!("{label} native identity"), &native.identity)?;
            native
                .source
                .validate_admission(&format!("{label} native"))?;
        }
        if let Some(model) = &self.model {
            coordinate(&format!("{label} model"), &model.name)?;
            if let Some(effort) = &model.effort {
                coordinate(&format!("{label} model effort"), effort)?;
            }
            model.source.validate_admission(&format!("{label} model"))?;
        }
        for (what, account) in [
            ("forge account", &self.forge_account),
            ("review sender", &self.review_sender),
        ] {
            if let Some(account) = account {
                established(&format!("{label} {what} provider"), &account.provider)?;
                established(&format!("{label} {what} host"), &account.host)?;
                established(&format!("{label} {what} account"), &account.account)?;
            }
        }
        for (what, identity) in [
            ("git author", &self.git_author),
            ("git committer", &self.git_committer),
        ] {
            if let Some(identity) = identity {
                established(&format!("{label} {what} name"), &identity.name)?;
                established(&format!("{label} {what} email"), &identity.email)?;
            }
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

    fn with_provenance(entry: Provenance) -> ReceivedEvidence {
        let mut document = evidence();
        document
            .omissions
            .retain(|omission| omission.what != "provenance");
        document.provenance.push(entry);
        document
    }

    fn observed_model(name: &str, effort: Option<&str>, kind: &str, reference: &str) -> Provenance {
        Provenance {
            role: AgentRole::Producer,
            native: None,
            model: Some(ModelObservation {
                name: name.into(),
                effort: effort.map(Into::into),
                source: SourceCoordinate {
                    kind: kind.into(),
                    reference: reference.into(),
                },
            }),
            forge_account: None,
            review_sender: None,
            git_author: None,
            git_committer: None,
        }
    }

    #[test]
    fn admission_refuses_placeholder_and_session_coordinates() {
        let session = "019f0000-0000-7000-8000-00000000000a";
        let good = observed_model("model-a", Some("high"), "session-file", "run-1");
        with_provenance(good).validate_admission().unwrap();
        for entry in [
            observed_model("unknown", Some("high"), "session-file", "run-1"),
            observed_model(session, Some("high"), "session-file", "run-1"),
            observed_model("model-a", Some("n/a"), "session-file", "run-1"),
            observed_model("model-a", Some(session), "session-file", "run-1"),
            observed_model("model-a", Some("high"), "none", "run-1"),
            observed_model("model-a", Some("high"), "session-file", "?"),
            observed_model("model-a", Some("high"), "session-file", session),
        ] {
            let document = with_provenance(entry);
            document.validate().unwrap();
            assert!(document.validate_admission().is_err());
        }
        let mut native = observed_model("model-a", None, "session-file", "run-1");
        native.native = Some(NativeProducer {
            provider: "unknown".into(),
            identity: session.into(),
            source: SourceCoordinate {
                kind: "session-file".into(),
                reference: "run-1".into(),
            },
        });
        assert!(with_provenance(native.clone())
            .validate_admission()
            .is_err());
        native.native.as_mut().unwrap().provider = "codex".into();
        with_provenance(native).validate_admission().unwrap();
    }

    #[test]
    fn admission_refuses_placeholder_attribution() {
        let mut entry = observed_model("model-a", None, "session-file", "run-1");
        entry.model = None;
        entry.git_author = Some(GitIdentity {
            name: "Unknown".into(),
            email: "a@example.invalid".into(),
        });
        assert!(with_provenance(entry.clone()).validate_admission().is_err());
        entry.git_author = None;
        entry.forge_account = Some(ForgeAccount {
            provider: "forge".into(),
            host: "example.invalid".into(),
            account: "none".into(),
        });
        assert!(with_provenance(entry).validate_admission().is_err());
        let mut document = evidence();
        document.omissions.retain(|o| o.what != "environment");
        document.environment = Some(Environment {
            identity: "n/a".into(),
            detail: None,
        });
        assert!(document.validate_admission().is_err());
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
