//! Validating a named selection: every ground that stands against it, or
//! the basis it rests on.
//!
//! The choice is the caller's. `evaluate` never picks a candidate and never
//! writes; it takes the proposal, the facts observed now, and the ledgers,
//! and answers with every failing ground rather than the first. Reading the
//! object store, the policy, and the destination is the command's work, so
//! the rules here are judged the same way whoever gathered the facts.

use crate::candidate::{Evaluation, Ledger, Registration};
use crate::model::{
    ReadCoverage, ReadRequirement, RelationSubject, RequiredExtent, SelectedEvaluation,
    SelectedRead, VerifyResult,
};
use crate::policy::EvaluationReuse;
use crate::relations::{line_range, Read, Relations};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;

/// A required gate as the destination declares it now, with the environment
/// its probe yields now.
#[derive(Debug, Clone)]
pub struct RequiredGate {
    pub name: String,
    pub command: String,
    pub timeout: Option<u64>,
    pub environment_probe: Option<String>,
    /// What the probe yielded. `None` for a gate declaring no probe, and for
    /// a probe that yielded no identity.
    pub environment: Option<String>,
}

/// A read requirement, with the required blob's bytes for a file.
#[derive(Debug, Clone)]
pub struct RequiredRead {
    pub requirement: ReadRequirement,
    pub bytes: Option<Vec<u8>>,
}

/// What the caller names. arc validates it and adds nothing to it.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub chosen: String,
    pub change_id: String,
    /// The target head the caller decided against.
    pub target: String,
    pub evaluations: Vec<String>,
}

/// The facts a proposal is judged against, observed now.
#[derive(Debug, Clone)]
pub struct Observations {
    pub reuse: Option<EvaluationReuse>,
    /// The destination's target head.
    pub target: String,
    pub destination_open: bool,
    pub gates: Vec<RequiredGate>,
    pub reads: Vec<RequiredRead>,
    /// Journal artifacts the contract itself supplied: the brief's plan and
    /// the artifact the change was opened from. Supplied is not read.
    pub supplied: Vec<String>,
}

/// Where the command resolves what the ledgers record only by name.
pub trait Resolve {
    /// The repository path a read names: its inferred blob's path, or the
    /// tool's path taken inside the repository.
    fn read_path(&self, read: &Read) -> Option<String>;
}

/// What a permitted selection rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Basis {
    pub tree: String,
    pub reuse: EvaluationReuse,
    pub evaluations: Vec<SelectedEvaluation>,
    pub reads: Vec<SelectedRead>,
    pub contributors: Vec<String>,
}

/// Why one named evaluation does not answer for a required gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceShortfall {
    Unrecorded,
    OtherRegistration(String),
    OtherTree(String),
    OtherDeclaration(String),
    EnvironmentUnrecorded,
    EnvironmentOther(String),
    Failed,
}

impl EvidenceShortfall {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unrecorded => "unrecorded",
            Self::OtherRegistration(_) => "other-registration",
            Self::OtherTree(_) => "other-tree",
            Self::OtherDeclaration(_) => "other-declaration",
            Self::EnvironmentUnrecorded => "environment-unrecorded",
            Self::EnvironmentOther(_) => "environment-other",
            Self::Failed => "failed",
        }
    }
}

/// Why a read requirement is not met, carrying what was found instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadShortfall {
    Partial(Vec<String>),
    UnknownCoverage(Vec<String>),
    OnlyDeclared(Vec<String>),
    OnlySupplied,
    NotRead,
}

impl ReadShortfall {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Partial(_) => "partial",
            Self::UnknownCoverage(_) => "unknown-coverage",
            Self::OnlyDeclared(_) => "only-declared",
            Self::OnlySupplied => "only-supplied",
            Self::NotRead => "not-read",
        }
    }
}

/// One ground standing against a proposal. Each carries its stable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    UnknownCandidate(String),
    Retired(String),
    ReusePolicyUndeclared,
    DestinationClosed(String),
    DestinationOtherContract {
        destination: String,
        contract: String,
    },
    TargetMoved {
        proposed: String,
        observed: String,
    },
    EnvironmentUnobserved {
        gate: String,
    },
    NoEvaluation {
        gate: String,
    },
    Evidence {
        gate: String,
        evaluation: String,
        shortfall: EvidenceShortfall,
    },
    Read {
        requirement: ReadRequirement,
        shortfall: ReadShortfall,
    },
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownCandidate(_) => "unknown-candidate",
            Self::Retired(_) => "candidate-retired",
            Self::ReusePolicyUndeclared => "reuse-policy-undeclared",
            Self::DestinationClosed(_) => "destination-closed",
            Self::DestinationOtherContract { .. } => "destination-other-contract",
            Self::TargetMoved { .. } => "target-moved",
            Self::EnvironmentUnobserved { .. } => "environment-unobserved",
            Self::NoEvaluation { .. } => "no-evaluation",
            Self::Evidence { shortfall, .. } => shortfall.code(),
            Self::Read { shortfall, .. } => shortfall.code(),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.code())?;
        match self {
            Self::UnknownCandidate(id) => write!(f, "no candidate {id} is registered"),
            Self::Retired(id) => write!(f, "candidate {id} is retired; its pin is gone"),
            Self::ReusePolicyUndeclared => write!(
                f,
                "[candidates] evaluation_reuse is not declared; declare \
                 \"matching-coordinates\" or \"never\""
            ),
            Self::DestinationClosed(change) => write!(f, "change {change} is not open"),
            Self::DestinationOtherContract {
                destination,
                contract,
            } => write!(
                f,
                "the chosen registration answers the brief of {contract}, not of {destination}"
            ),
            Self::TargetMoved { proposed, observed } => write!(
                f,
                "the proposal names target {proposed}; the target head is {observed}"
            ),
            Self::EnvironmentUnobserved { gate } => write!(
                f,
                "gate {gate}: its environment probe yielded no identity, so no evaluation \
                 can be compared with this environment"
            ),
            Self::NoEvaluation { gate } => {
                write!(f, "gate {gate}: no named evaluation answers it")
            }
            Self::Evidence {
                gate,
                evaluation,
                shortfall,
            } => {
                write!(f, "gate {gate}: evaluation {evaluation} ")?;
                match shortfall {
                    EvidenceShortfall::Unrecorded => {
                        write!(f, "is not a recorded candidate evaluation")
                    }
                    EvidenceShortfall::OtherRegistration(candidate) => write!(
                        f,
                        "was recorded on {candidate}, and evaluation_reuse is \"never\""
                    ),
                    EvidenceShortfall::OtherTree(tree) => {
                        write!(f, "ran at tree {tree}, not the shipped tree")
                    }
                    EvidenceShortfall::OtherDeclaration(detail) => {
                        write!(f, "ran under another declaration: {detail}")
                    }
                    EvidenceShortfall::EnvironmentUnrecorded => write!(
                        f,
                        "recorded no environment, and the gate declares an environment probe"
                    ),
                    EvidenceShortfall::EnvironmentOther(identity) => {
                        write!(f, "ran in environment {identity}, not the one observed now")
                    }
                    EvidenceShortfall::Failed => write!(f, "did not pass"),
                }
            }
            Self::Read {
                requirement,
                shortfall,
            } => {
                write!(f, "must-read {requirement}: ")?;
                match shortfall {
                    ReadShortfall::Partial(records) => write!(
                        f,
                        "read at the required version by {} without covering the required \
                         extent",
                        records.join(", ")
                    ),
                    ReadShortfall::UnknownCoverage(records) => write!(
                        f,
                        "read at the required version by {}, which recorded no coverage",
                        records.join(", ")
                    ),
                    ReadShortfall::OnlyDeclared(declarants) => write!(
                        f,
                        "only declared, by {}; a declaration is not a read",
                        declarants.join(", ")
                    ),
                    ReadShortfall::OnlySupplied => {
                        write!(f, "only supplied by the contract; supplied is not read")
                    }
                    ReadShortfall::NotRead => write!(
                        f,
                        "no tool read at the required version by an episode the chosen \
                         registration or its parents cite"
                    ),
                }
            }
        }
    }
}

/// Every ground standing against the proposal, or the basis it rests on.
pub fn evaluate(
    ledger: &Ledger,
    relations: &Relations,
    resolve: &dyn Resolve,
    proposal: &Proposal,
    observed: &Observations,
) -> Result<Basis, Vec<Refusal>> {
    let Some(chosen) = ledger.registration(&proposal.chosen) else {
        return Err(vec![Refusal::UnknownCandidate(proposal.chosen.clone())]);
    };
    let mut refusals = Vec::new();
    if ledger.retirement(&chosen.candidate_id).is_some() {
        refusals.push(Refusal::Retired(chosen.candidate_id.clone()));
    }
    if observed.reuse.is_none() {
        refusals.push(Refusal::ReusePolicyUndeclared);
    }
    if chosen.brief.change_id != proposal.change_id {
        refusals.push(Refusal::DestinationOtherContract {
            destination: proposal.change_id.clone(),
            contract: chosen.brief.change_id.clone(),
        });
    }
    if !observed.destination_open {
        refusals.push(Refusal::DestinationClosed(proposal.change_id.clone()));
    }
    if proposal.target != observed.target {
        refusals.push(Refusal::TargetMoved {
            proposed: proposal.target.clone(),
            observed: observed.target.clone(),
        });
    }

    // Under an undeclared policy nothing may be reused, so the gates are
    // judged as under `never` and the undeclared policy is its own ground.
    let reuse = observed.reuse.unwrap_or(EvaluationReuse::Never);
    let mut evaluations = Vec::new();
    for gate in &observed.gates {
        if gate.environment_probe.is_some() && gate.environment.is_none() {
            refusals.push(Refusal::EnvironmentUnobserved {
                gate: gate.name.clone(),
            });
            continue;
        }
        match gate_answer(ledger, chosen, reuse, gate, &proposal.evaluations) {
            Ok(evaluation) => evaluations.push(SelectedEvaluation {
                gate: gate.name.clone(),
                event_id: evaluation.event_id.clone(),
                candidate_id: evaluation.candidate_id.clone(),
            }),
            Err(mut grounds) => refusals.append(&mut grounds),
        }
    }

    let mut reads = Vec::new();
    for required in &observed.reads {
        match read_answer(ledger, relations, resolve, chosen, required, observed) {
            Ok(read) => reads.push(SelectedRead {
                requirement: required.requirement.clone(),
                candidate_id: match &read.subject {
                    RelationSubject::Candidate { candidate_id } => candidate_id.clone(),
                    RelationSubject::Change { change_id } => change_id.clone(),
                },
                record: read.record.clone(),
                event_id: read.event_id.clone(),
            }),
            Err(shortfall) => refusals.push(Refusal::Read {
                requirement: required.requirement.clone(),
                shortfall,
            }),
        }
    }

    if !refusals.is_empty() {
        return Err(refusals);
    }
    Ok(Basis {
        tree: chosen.tree.clone(),
        reuse,
        evaluations,
        reads,
        contributors: ledger.contributors_of(chosen),
    })
}

/// The first named evaluation answering a gate, or every shortfall of the
/// ones named for it. An evaluation the ledger does not hold could be for
/// any gate, so it is reported under each.
fn gate_answer<'a>(
    ledger: &'a Ledger,
    chosen: &Registration,
    reuse: EvaluationReuse,
    gate: &RequiredGate,
    named: &[String],
) -> Result<&'a Evaluation, Vec<Refusal>> {
    let mut grounds = Vec::new();
    for event_id in named {
        let evaluation = ledger.evaluation(event_id);
        if evaluation.is_some_and(|evaluation| evaluation.gate != gate.name) {
            continue;
        }
        match evaluation.map_or(Err(EvidenceShortfall::Unrecorded), |evaluation| {
            evidence_shortfall(chosen, reuse, gate, evaluation).map_or(Ok(evaluation), Err)
        }) {
            Ok(evaluation) => return Ok(evaluation),
            Err(shortfall) => grounds.push(Refusal::Evidence {
                gate: gate.name.clone(),
                evaluation: event_id.clone(),
                shortfall,
            }),
        }
    }
    if grounds.is_empty() {
        grounds.push(Refusal::NoEvaluation {
            gate: gate.name.clone(),
        });
    }
    Err(grounds)
}

/// Why an evaluation does not answer for a gate, or `None` when it does.
/// The registration is checked under the reuse policy first, then the
/// coordinates, then the outcome.
pub fn evidence_shortfall(
    chosen: &Registration,
    reuse: EvaluationReuse,
    gate: &RequiredGate,
    evaluation: &Evaluation,
) -> Option<EvidenceShortfall> {
    if evaluation.candidate_id != chosen.candidate_id && reuse == EvaluationReuse::Never {
        return Some(EvidenceShortfall::OtherRegistration(
            evaluation.candidate_id.clone(),
        ));
    }
    if evaluation.tree != chosen.tree {
        return Some(EvidenceShortfall::OtherTree(evaluation.tree.clone()));
    }
    if let Some(detail) = declaration_difference(gate, evaluation) {
        return Some(EvidenceShortfall::OtherDeclaration(detail));
    }
    if gate.environment_probe.is_some() {
        match &evaluation.environment {
            None => return Some(EvidenceShortfall::EnvironmentUnrecorded),
            Some(recorded) if Some(&recorded.identity) != gate.environment.as_ref() => {
                return Some(EvidenceShortfall::EnvironmentOther(
                    recorded.identity.clone(),
                ))
            }
            Some(_) => {}
        }
    }
    (evaluation.result != VerifyResult::Pass).then_some(EvidenceShortfall::Failed)
}

fn declaration_difference(gate: &RequiredGate, evaluation: &Evaluation) -> Option<String> {
    if evaluation.command != gate.command {
        return Some(format!(
            "command {:?}, declared now as {:?}",
            evaluation.command, gate.command
        ));
    }
    if evaluation.timeout_seconds != gate.timeout {
        return Some(format!(
            "timeout {:?}, declared now as {:?}",
            evaluation.timeout_seconds, gate.timeout
        ));
    }
    if evaluation.environment_probe != gate.environment_probe {
        return Some(format!(
            "environment probe {:?}, declared now as {:?}",
            evaluation.environment_probe, gate.environment_probe
        ));
    }
    None
}

/// A tool read meeting a requirement, or what was found instead.
///
/// Only a read recorded on a registration along the chosen one's parent
/// chain, by an episode one of them cites, counts. An adopted registration
/// is not on the chain, so its reads do not count. A declaration or the
/// contract's own supply never meets a requirement; each is reported as what
/// it is.
fn read_answer<'a>(
    ledger: &Ledger,
    relations: &'a Relations,
    resolve: &dyn Resolve,
    chosen: &Registration,
    required: &RequiredRead,
    observed: &Observations,
) -> Result<&'a Read, ReadShortfall> {
    let chain = ledger.lineage(chosen);
    let on_chain: BTreeSet<&str> = chain
        .iter()
        .map(|registration| registration.candidate_id.as_str())
        .collect();
    let cited: BTreeSet<&str> = chain
        .iter()
        .flat_map(|registration| registration.episodes.iter().map(String::as_str))
        .collect();
    let chain_subject = |subject: &RelationSubject| match subject {
        RelationSubject::Candidate { candidate_id } => on_chain.contains(candidate_id.as_str()),
        RelationSubject::Change { .. } => false,
    };
    let found: Vec<&Read> = relations
        .reads()
        .filter(|read| chain_subject(&read.subject) && cited.contains(read.episode.as_str()))
        .filter(|read| same_version(resolve, read, required))
        .collect();
    let extent = match &required.requirement {
        ReadRequirement::Artifact { .. } => RequiredExtent::Whole,
        ReadRequirement::File { extent, .. } => *extent,
    };
    if let Some(read) = found
        .iter()
        .find(|read| covers(read.coverage, extent) == Some(true))
    {
        return Ok(read);
    }
    let records = |wanted: Option<bool>| -> Vec<String> {
        found
            .iter()
            .filter(|read| covers(read.coverage, extent) == wanted)
            .map(|read| read.record.clone())
            .collect()
    };
    let partial = records(Some(false));
    if !partial.is_empty() {
        return Err(ReadShortfall::Partial(partial));
    }
    let unknown = records(None);
    if !unknown.is_empty() {
        return Err(ReadShortfall::UnknownCoverage(unknown));
    }
    let declared: BTreeSet<String> = relations
        .declarations()
        .filter(|declaration| chain_subject(&declaration.subject))
        .filter(|declaration| declares(&declaration.target, &required.requirement))
        .map(|declaration| declaration.declarant.clone())
        .collect();
    if !declared.is_empty() {
        return Err(ReadShortfall::OnlyDeclared(declared.into_iter().collect()));
    }
    if let ReadRequirement::Artifact { file, .. } = &required.requirement {
        if observed.supplied.iter().any(|supplied| supplied == file) {
            return Err(ReadShortfall::OnlySupplied);
        }
    }
    Err(ReadShortfall::NotRead)
}

/// Whether a read saw the required version. An artifact's is its body
/// digest. A file's is its blob: the read's inferred blob is that blob, or
/// the bytes it returned equal the blob's over the range it recorded.
fn same_version(resolve: &dyn Resolve, read: &Read, required: &RequiredRead) -> bool {
    match &required.requirement {
        ReadRequirement::Artifact { file, digest } => {
            read.artifact
                .as_ref()
                .is_some_and(|artifact| &artifact.file == file)
                && &read.digest == digest
        }
        ReadRequirement::File { path, blob, .. } => {
            if resolve.read_path(read).as_deref() != Some(path.as_str()) {
                return false;
            }
            if read
                .blob
                .as_ref()
                .is_some_and(|inferred| &inferred.blob == blob)
            {
                return true;
            }
            let Some(bytes) = &required.bytes else {
                return false;
            };
            let returned = match read.coverage {
                ReadCoverage::Whole | ReadCoverage::Unknown => Some(bytes.as_slice()),
                ReadCoverage::Lines { from, to } => line_range(bytes, from, to),
            };
            returned.is_some_and(|returned| {
                format!("sha256:{}", hex::encode(Sha256::digest(returned))) == read.digest
            })
        }
    }
}

/// Whether a recorded coverage covers a required extent; `None` when the
/// tool recorded none, which never covers anything.
pub fn covers(coverage: ReadCoverage, extent: RequiredExtent) -> Option<bool> {
    match (coverage, extent) {
        (ReadCoverage::Unknown, _) => None,
        (ReadCoverage::Whole, _) => Some(true),
        (ReadCoverage::Lines { .. }, RequiredExtent::Whole) => Some(false),
        (
            ReadCoverage::Lines { from, to },
            RequiredExtent::Lines {
                from: need,
                to: upto,
            },
        ) => Some(from <= need && upto <= to),
    }
}

/// Whether a declaration's target names the required context. A
/// declaration carries no version, so it is matched by locator alone.
fn declares(target: &crate::model::DeclaredTarget, requirement: &ReadRequirement) -> bool {
    use crate::model::DeclaredTarget;
    match (target, requirement) {
        (DeclaredTarget::Artifact { file }, ReadRequirement::Artifact { file: wanted, .. }) => {
            file == wanted
        }
        (DeclaredTarget::Path { path, .. }, ReadRequirement::File { path: wanted, .. }) => {
            path.trim_start_matches("./") == wanted
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_coverage_never_covers_and_a_range_covers_only_inside_itself() {
        let lines = |from, to| ReadCoverage::Lines { from, to };
        assert_eq!(covers(ReadCoverage::Unknown, RequiredExtent::Whole), None);
        assert_eq!(
            covers(ReadCoverage::Whole, RequiredExtent::Whole),
            Some(true)
        );
        assert_eq!(covers(lines(1, 9), RequiredExtent::Whole), Some(false));
        let need = RequiredExtent::Lines { from: 2, to: 4 };
        assert_eq!(covers(lines(1, 9), need), Some(true));
        assert_eq!(covers(lines(3, 9), need), Some(false));
    }
}
