use crate::approval::{self, ApprovalFacts, ApprovalOutcome};
use crate::blockers::{self, BlockerFacts, GateFact};
use crate::declarations::Declarations;
use crate::gates::GatesFile;
use crate::gitio;
use crate::model::{
    Falsification, MessageSeverity, MessageType, ProbePhase, Verdict, VerifyResult,
};
use crate::policy::PolicyFile;
use crate::state::{self, ChangeState, ClaimIdentity, GitIdentity};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub use crate::blockers::Blocker;

pub const STATUS_SCHEMA: &str = "arc-status/27";
pub const BLOCKER_STATUS_SCHEMA: &str = "arc-blocker-status/1";

#[derive(Debug, Clone, Serialize)]
pub struct DependencyChangeStatus {
    pub change_id: String,
    pub slug: String,
    pub status: String,
    pub integrated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockerStatus {
    pub schema: &'static str,
    pub blocked: bool,
    /// Status of every declared prerequisite. The historical field name is
    /// retained because orchestrators already consume it from the design spec.
    pub blockers_ready: Vec<DependencyChangeStatus>,
}

/// Whether a gate's passing evidence was ever shown capable of failing.
///
/// A pass alone cannot say. `Discriminating` means the same check was recorded
/// failing at an earlier revision of this change, for a reason predicted
/// before it ran, and this run answers that failure. `Undiscriminated` means
/// no such failure was recorded — the usual case, and not a defect.
///
/// Advisory throughout: nothing here participates in readiness, gate results,
/// or exit codes.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Discrimination {
    Discriminating,
    Undiscriminated,
}

#[derive(Debug, Serialize)]
pub struct GateStatus {
    #[serde(skip_serializing_if = "is_false")]
    pub batch_acceptance_missing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_pass: Option<crate::base_pass::Accounting>,
    pub budget: crate::gate_budget::BudgetJudgement,
    pub execution_host: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub timing: crate::verification_timing::VerificationTiming,
    pub command: String,
    /// Policy files that declared this effective gate. Additive in
    /// `arc-status/23`.
    pub declared_by: Vec<String>,
    pub result: String,
    /// Whether eligible passing evidence meets the gate's integration requirement.
    pub green_at_head: bool,
    /// Historical judgement recorded in place of a run; grants no authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bypass: Option<crate::state::BypassEntry>,
    /// Passing evidence exists at this head, but for a different command than
    /// the gate now declares. Not green: the declaration is the check.
    #[serde(skip_serializing_if = "is_false")]
    pub declaration_changed: bool,
    /// The head evidence for this gate is attested (arc did not run it), so a
    /// lead can apply stricter judgment even though it counts for green-ness.
    pub attested: bool,
    /// The tree this gate's readiness was read at, named only where it is not
    /// the head's own content — that is, where the change is behind its target
    /// and a merge would ship something neither branch committed. Absent means
    /// the head's content is what ships, and the gate answers for it.
    /// Additive in `arc-status/15`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluated_tree: Option<String>,
    /// The revision the counted evidence was recorded at, named only where it
    /// is not the current head. A gate answers for content, so a run against
    /// another commit holding the tree being evaluated answers for this one;
    /// this says which run that was, so a green gate nobody ran at this head
    /// is never mistaken for one that was never keyed at all. A bypass names
    /// the immutable head where its judgement was recorded.
    /// Additive in `arc-status/16`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherited_from: Option<String>,
    /// The tree the command ran against, and whether it differed from the
    /// revision's. Absent is unknown, never a claim the tree was clean.
    /// Additive in `arc-status/6`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tested_tree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_dirty: Option<bool>,
    /// Which kind of dirt, so a waiver reason is self-evident at the moment of
    /// waiving. Absent on evidence recorded before the split, which is not the
    /// same as clean. Additive in `arc-status/10`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_dirty_tracked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_dirty_untracked: Option<bool>,
    /// The worktree changed while the command ran, so this evidence describes
    /// no single tree.
    #[serde(skip_serializing_if = "is_false")]
    pub tree_moved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    /// Whether this gate's evidence describes the environment being
    /// evaluated, named only for a gate that declares an environment probe.
    /// Additive in `arc-status/22`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<GateEnvironmentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tail: Option<String>,
    /// Private diagnostic capture of an executed gate, passing or failing.
    /// Additive in `arc-status/27`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_artifact: Option<crate::verification_output::OutputArtifact>,
    #[serde(skip_serializing_if = "is_false")]
    pub timed_out: bool,
    /// Whether this gate was ever shown capable of failing at this revision.
    /// Absent when the counted evidence is not a pass: discrimination is a
    /// property of a pass, and a failure or a missing run has none to report.
    /// Additive in `arc-status/14`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discrimination: Option<Discrimination>,
    /// The failure that was answered, when one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub falsification: Option<Falsification>,
    /// The evidence carrying that reference, named only when it is not the
    /// evidence that counts. A rerun at the same revision appends newer
    /// passing evidence without a reference, and the gate stays
    /// discriminating; the two ids then say which run proved what.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discrimination_event_id: Option<String>,
    /// The failure arc derived the counted pass follows. It never sets
    /// `discrimination`, which only a declared falsification does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub falsification_inferred: Option<crate::model::InferredFalsification>,
}

/// Whether a gate's evidence describes the environment being evaluated.
///
/// Present only for a gate that declares an environment probe. Readiness
/// counts the evidence only when the recorded identity equals the one the
/// probe yields in a clean checkout of the content the evidence is looked up
/// at; the identities are opaque, and only equality between them is read.
#[derive(Debug, Clone, Serialize)]
pub struct GateEnvironmentStatus {
    /// The identity recorded on the evidence being counted. Absent when that
    /// evidence carries none, which is evidence written before environments
    /// were recorded or evidence whose probe could not answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// The identity the declared probe yields in a clean checkout of the
    /// evaluated content. Absent on a ledger replay, which observes no
    /// environment, where no such checkout exists, and where the probe could
    /// not answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// The evidence was produced in a different environment, so it does not
    /// answer for this one. A mismatch is reported as inapplicable rather
    /// than missing: the run happened, it just describes somewhere else.
    #[serde(skip_serializing_if = "is_false")]
    pub inapplicable: bool,
    /// The evidence carries no environment identity, so nothing says which
    /// environment produced it. Unknown, not inapplicable: there is no second
    /// identity to compare against.
    #[serde(skip_serializing_if = "is_false")]
    pub unknown: bool,
    /// The declared probe could not be read in that checkout;
    /// the run's exit, overrun, or empty output is named. Every receipt for
    /// the gate is not-green while this holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe_failed: Option<String>,
    /// No clean checkout holds the content the evidence is looked up at, so
    /// the probe ran nowhere and nothing was compared. Every receipt for the
    /// gate is not-green while this holds. Additive in `arc-status/27`.
    #[serde(skip_serializing_if = "is_false")]
    pub needs_evaluation: bool,
}

/// A checkout created to hold the content an evaluation reads, removed when
/// dropped.
pub trait EvaluationCheckout {
    fn path(&self) -> &Path;
}

/// Creates a checkout holding exactly the given tree, for a caller allowed to
/// create one.
pub type CreateEvaluationCheckout<'a> = &'a dyn Fn(&str) -> Result<Box<dyn EvaluationCheckout>>;

/// Where a report runs a gate's declared environment probe.
enum ProbeSite<'a> {
    /// A ledger replay observes no environment and takes a recorded identity
    /// at its word.
    Replay,
    /// A live report runs the probe in a clean checkout whose content is the
    /// tree the evidence is looked up at: `invoking` when it holds that tree,
    /// else the change's worktree, else one `create` makes. With none of
    /// them the probe runs nowhere.
    Live {
        invoking: &'a Path,
        create: Option<CreateEvaluationCheckout<'a>>,
    },
}

/// The checkout a report's environment probes run in, resolved once.
enum ProbeSubject {
    Checkout(PathBuf),
    Owned(Box<dyn EvaluationCheckout>),
    Unobservable,
}

impl ProbeSubject {
    fn resolve(site: &ProbeSite<'_>, tree: Option<&str>, state: &ChangeState) -> Result<Self> {
        let (ProbeSite::Live { invoking, create }, Some(tree)) = (site, tree) else {
            return Ok(Self::Unobservable);
        };
        let recorded = state
            .worktree
            .as_deref()
            .map(PathBuf::from)
            .filter(|worktree| worktree.exists());
        let holding = std::iter::once(invoking.to_path_buf())
            .chain(recorded)
            .find(|checkout| {
                gitio::commit_tree(checkout, "HEAD").is_ok_and(|head| head == tree)
                    && gitio::is_clean(checkout).unwrap_or(false)
            });
        Ok(match (holding, create) {
            (Some(checkout), _) => Self::Checkout(checkout),
            (None, Some(create)) => Self::Owned(create(tree)?),
            (None, None) => Self::Unobservable,
        })
    }

    fn path(&self) -> Option<&Path> {
        match self {
            Self::Checkout(path) => Some(path),
            Self::Owned(checkout) => Some(checkout.path()),
            Self::Unobservable => None,
        }
    }
}

impl GateStatus {
    /// Why this gate is not green at head, in the caller's terms.
    ///
    /// A gate whose evidence passed but cannot be reused says so here; the
    /// bare result would read `pass` and contradict every readiness check.
    /// `None` means the gate is green.
    pub fn not_green_reason(&self) -> Option<String> {
        if self.green_at_head {
            return None;
        }
        if self.bypass.is_some() {
            return Some(
                "recorded bypass is history and satisfies no gate requirement; run the gate".into(),
            );
        }
        if self.batch_acceptance_missing {
            return Some("concurrent batch acceptance is not current on this store and host; run the gate alone or measure the current batch".into());
        }
        if let Some(reason) = self.budget.reason(
            &self.timing,
            self.execution_host.as_deref().unwrap_or("unrecorded"),
        ) {
            return Some(reason);
        }
        // Before the provenance checks: this evidence may be perfectly good
        // provenance for a command the gate no longer declares, and saying
        // its tree was unrecorded would send the reader to the wrong repair.
        if self.declaration_changed {
            return Some("the gate declaration changed since this evidence was recorded".into());
        }
        // An environment mismatch is a fact about where the run happened, not
        // about its provenance on this checkout, so it is reported first.
        if let Some(environment) = &self.environment {
            if environment.unknown {
                return Some(
                    "the evidence records no environment identity, so no environment can be \
                     said to have produced it"
                        .into(),
                );
            }
            if environment.inapplicable {
                return Some(format!(
                    "the evidence was produced in environment {} but the declared probe yields \
                     {} for the content being evaluated",
                    environment.evidence.as_deref().unwrap_or("unknown"),
                    environment.current.as_deref().unwrap_or("unknown")
                ));
            }
            if let Some(failure) = &environment.probe_failed {
                return Some(format!(
                    "the declared environment probe could not be read here: {failure}"
                ));
            }
            if environment.needs_evaluation {
                return Some(match &self.evaluated_tree {
                    Some(tree) => format!(
                        "no clean checkout holds the merged tree {tree}, so the declared \
                         environment probe ran nowhere; arc integrate evaluates it in a \
                         checkout of its own"
                    ),
                    None => "no clean checkout holds the head's tree, so the declared \
                             environment probe ran nowhere; run this from the change's worktree \
                             with its head checked out and clean, or arc integrate evaluates it \
                             in a checkout of its own"
                        .into(),
                });
            }
        }
        Some(
            match self.result.as_str() {
                "pending" if self.evaluated_tree.is_some() => "no evidence at the merged tree",
                "pending" => "no evidence at head",
                "fail" => "the gate failed",
                "not-admitted" => {
                    "the gate was not admitted before any check ran, so nothing was evaluated; rerun it"
                }
                _ if self.tree_moved => "the worktree changed while the gate ran",
                _ if self.worktree_dirty == Some(true) => {
                    "evidence recorded on a dirty worktree, so no checkout of this revision \
                     reproduces it"
                }
                // Parallel gates share one worktree, so no boundary comparison can
                // prove that a gate did not change a tracked file and restore it.
                // The tree was recorded; what is missing is whether it was clean.
                _ if self.tested_tree.is_some() => {
                    "the worktree's cleanliness was not recorded, which is what a shared-worktree \
                     parallel run can never establish"
                }
                _ => "the tested tree was not recorded, so the evidence has no provenance",
            }
            .into(),
        )
    }

    /// What actually clears this gate, given the state of the worktree now.
    ///
    /// Re-running a gate against a still-dirty tree records the same unusable
    /// evidence, so while the tree is dirty the only step that makes progress
    /// is cleaning it. Once it is clean the stale evidence can only be
    /// replaced by a rerun — the historical dirty flag on evidence already
    /// recorded is not something cleaning can change.
    pub fn clearing_action(&self, worktree_dirty: Option<bool>) -> String {
        if self.green_at_head {
            return "integrate".into();
        }
        if !self.budget.accepted() && self.evidence_event_id.is_some() {
            return format!("inspect_gate_budget:{}", self.name);
        }
        // While the tree is dirty, no local run can produce evidence that
        // counts — attested evidence carries its own execution context and is
        // unaffected — including a rerun of a gate that failed, and including
        // a gate that failed *because* of the uncommitted file. Cleaning is not claimed to
        // fix a failure; it is the precondition for any run whose result is
        // usable. It also cannot loop where the live tree is known: this reads
        // it, so once the tree is clean the advice becomes the rerun. Where it
        // is unknown — a change with no worktree — the advice is the rerun,
        // and whether such a change can gate at all is its own question.
        if worktree_dirty == Some(true) {
            return format!("clean_worktree:{}", self.name);
        }
        format!("run_gate:{}", self.name)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeStatus {
    pub name: String,
    pub command: String,
    pub brief_version: usize,
    pub baseline_revision: String,
    pub baseline_result: String,
    pub baseline_attested: bool,
    pub final_revision: String,
    pub final_result: String,
    pub final_attested: bool,
    pub discriminating_at_head: bool,
    /// The brief's base is the head under review, so the probe would have to
    /// fail and pass at one revision. No run can discharge it; only a brief
    /// based on the revision the work started from can. Additive in
    /// `arc-status/6`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub undischargeable: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Whether a probe declared on a brief based at `base` can discharge at a
/// patchset whose head is `head`. It cannot when the base and the head are one
/// revision, or when the brief predates base revisions and there is no
/// revision to fail at — `verify --probe-phase baseline` refuses that outright.
pub fn probe_base_dischargeable(base: Option<&str>, head: &str) -> bool {
    base.is_some_and(|base| !base.is_empty() && base != head)
}

/// How a finding's cited resolution commit relates to the change's current head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionCommitStatus {
    /// No commit was cited on the disposition, or the finding is not resolved.
    Unbound,
    /// The cited commit (after following recorded rewrites) is an ancestor of
    /// `head` and its fix survives at `head`.
    Contained(String),
    /// The cited commit is not an ancestor of `head`, but a commit in
    /// `base..head` carries the exact same Git patch-id and its fix survives
    /// at `head`.
    CarriedByPatchId { recorded: String, carried: String },
    /// The cited commit (or its patch-id-carried successor) appears in history
    /// but was reverted before `head`.
    RevertedInHead { recorded: String, commit: String },
    /// The cited commit is neither an ancestor of `head` nor carried by patch-id
    /// in `base..head`.
    NotInHead(String),
}

impl ResolutionCommitStatus {
    /// Whether this status leaves the finding open at `head`.
    pub fn leaves_open(&self) -> bool {
        matches!(self, Self::NotInHead(_) | Self::RevertedInHead { .. })
    }
}

#[derive(Debug, Serialize)]
pub struct FindingSummary {
    pub id: String,
    pub blocking: bool,
    pub severity: crate::model::Severity,
    pub summary: String,
    pub status: String,
    pub contested: bool,
    /// What the finding actually says, and where. A one-line summary is enough
    /// to count findings and not enough to act on one, which is the position a
    /// reader inheriting a change is in. Additive in `arc-status/6`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<crate::model::Anchor>,
    /// The patchset the finding was filed against. A finding predating the
    /// patchset under review is a different kind of fact from one filed
    /// against it, and the count alone cannot tell them apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patchset_id: Option<String>,
    pub reported_by: String,
    /// Git-time verification of a `Resolved` disposition's cited commit against
    /// the current head and `base..head` patch-ids. Omitted from JSON to keep
    /// `arc-status/27` stable.
    #[serde(skip)]
    pub resolution_commit_status: Option<ResolutionCommitStatus>,
}

/// Verify each open finding's cited resolution commit against `current_head`
/// and the change's own commit range (`base..head`).
///
/// A `Resolved` disposition citing commit `C1` stands when:
/// - `C1` (already advanced through recorded rewrites) is an ancestor of
///   `current_head`, or
/// - `C1` is a non-merge commit with a non-empty Git patch-id and some
///   non-merge commit `C2` in `base..head` carries that exact patch-id.
///
/// When neither holds, the disposition's cited commit is missing from the
/// change's head (`ResolutionCommitStatus::NotInHead`), leaving the finding
/// open. Range patch-ids are computed lazily only when a cited commit is not
/// already an ancestor of `current_head`.
pub(crate) fn evaluate_finding_resolutions(
    cwd: Option<&Path>,
    state: &ChangeState,
    current_head: Option<&str>,
) -> BTreeMap<String, ResolutionCommitStatus> {
    let mut statuses = BTreeMap::new();
    let (Some(cwd), Some(head)) = (cwd, current_head) else {
        return statuses;
    };
    if state.is_closed() {
        return statuses;
    }
    let mut range_patch_ids: Option<Vec<(String, String)>> = None;
    for finding in state.findings.values() {
        let tips = finding.tips();
        let [tip] = tips.as_slice() else {
            continue;
        };
        if tip.status != crate::model::DispositionStatus::Resolved {
            continue;
        }
        let Some(recorded) = tip.commit.as_deref() else {
            statuses.insert(finding.id.clone(), ResolutionCommitStatus::Unbound);
            continue;
        };
        if gitio::is_ancestor(cwd, recorded, head).unwrap_or(false) {
            let status = if fix_survives_in_head(cwd, recorded, head) {
                ResolutionCommitStatus::Contained(recorded.to_string())
            } else {
                ResolutionCommitStatus::RevertedInHead {
                    recorded: recorded.to_string(),
                    commit: recorded.to_string(),
                }
            };
            statuses.insert(finding.id.clone(), status);
            continue;
        }
        let entries = range_patch_ids
            .get_or_insert_with(|| change_range_patch_ids(cwd, state, head).unwrap_or_default());
        let carried = gitio::commit_exists(cwd, recorded)
            .unwrap_or(false)
            .then(|| {
                let parents = gitio::commit_parents(cwd, recorded).ok()?;
                if parents.len() > 1 {
                    return None;
                }
                let expected_pid = gitio::patch_id(cwd, recorded).ok().flatten()?;
                entries
                    .iter()
                    .rfind(|(_, pid)| pid == &expected_pid)
                    .map(|(commit, _)| commit.clone())
            })
            .flatten();
        match carried {
            Some(carried) => {
                let status = if fix_survives_in_head(cwd, &carried, head) {
                    ResolutionCommitStatus::CarriedByPatchId {
                        recorded: recorded.to_string(),
                        carried,
                    }
                } else {
                    ResolutionCommitStatus::RevertedInHead {
                        recorded: recorded.to_string(),
                        commit: carried,
                    }
                };
                statuses.insert(finding.id.clone(), status);
            }
            None => {
                statuses.insert(
                    finding.id.clone(),
                    ResolutionCommitStatus::NotInHead(recorded.to_string()),
                );
            }
        }
    }
    statuses
}

/// Whether `fix_commit` (already known to be an ancestor of `head`) still
/// survives in `head` rather than having been reverted or partially rolled back
/// by subsequent commits.
///
/// Compares the final tree at `head` directly against `fix_commit` and its
/// pre-fix parent using raw path bytes and zero-context blob-OID diffs:
/// - Any path unchanged between `fix_commit` and `head` survives immediately.
/// - Any path whose `(mode, oid)` returned to its pre-fix state (including
///   deleted additions, restored deletions, mode rollbacks, or single-file
///   rollbacks in a multi-file fix) fails closed as reverted.
/// - For regular text blobs modified again after `fix_commit`, each hunk of
///   `diff -U0 (pre_oid, fix_oid)` is tracked to its aligned region in `head`
///   via `diff -U0 (fix_oid, head_oid)` with left-tightening minimum-displacement
///   anchor relaxation so nearby context edits, unrelated copies of old text
///   elsewhere in the file, and reverts followed by reapplication with adjacent
///   edits are all judged accurately at the affected region.
fn fix_survives_in_head(cwd: &Path, fix_commit: &str, head: &str) -> bool {
    if fix_commit == head {
        return true;
    }
    let Ok(fix_entries) = gitio::commit_raw_tree_diff(cwd, fix_commit) else {
        return false;
    };
    if fix_entries.is_empty() {
        return true;
    }
    let Ok(head_diff_entries) = gitio::raw_tree_diff_between(cwd, fix_commit, head) else {
        return false;
    };
    if head_diff_entries.is_empty() {
        return true;
    }
    let head_by_path: std::collections::HashMap<&[u8], (&str, &str)> = head_diff_entries
        .iter()
        .map(|entry| {
            (
                entry.path.as_slice(),
                (entry.new_mode.as_str(), entry.new_oid.as_str()),
            )
        })
        .collect();

    let mut modified_text_entries = Vec::new();
    for entry in &fix_entries {
        let Some(&(head_mode, head_oid)) = head_by_path.get(entry.path.as_slice()) else {
            // Tree entry at `head` is identical to `fix_commit`.
            continue;
        };
        if head_mode == entry.old_mode && head_oid == entry.old_oid {
            return false;
        }
        if entry.old_mode != "000000"
            && entry.new_mode != "000000"
            && entry.old_mode != entry.new_mode
            && head_mode == entry.old_mode
        {
            return false;
        }
        if is_regular_blob_mode(&entry.old_mode)
            && is_regular_blob_mode(&entry.new_mode)
            && is_regular_blob_mode(head_mode)
            && entry.old_oid != entry.new_oid
            && head_oid != entry.new_oid
        {
            modified_text_entries.push((entry, head_oid));
        }
    }
    if modified_text_entries.is_empty() {
        return true;
    }

    let Ok(commits) = gitio::range_commits_with_parents(cwd, fix_commit, head) else {
        return false;
    };
    let mut edge_diffs: std::collections::HashMap<(String, String), Vec<gitio::RawTreeDiffEntry>> =
        std::collections::HashMap::new();
    for (entry, head_oid) in modified_text_entries {
        if !text_blob_survives_along_history(
            cwd,
            fix_commit,
            head,
            entry,
            head_oid,
            &commits,
            &mut edge_diffs,
        ) {
            return false;
        }
    }
    true
}

fn is_regular_blob_mode(mode: &str) -> bool {
    matches!(mode, "100644" | "100755")
}

/// Zero-context diff hunk with 0-based half-open line ranges in `old` and `new`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct U0Hunk {
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
}

fn parse_u0_hunks(diff: &[u8]) -> Option<Vec<U0Hunk>> {
    let mut hunks = Vec::new();
    for line in split_lines(diff) {
        if line.starts_with(b"Binary files ") {
            return None;
        }
        let Some(rest) = line.strip_prefix(b"@@ -") else {
            continue;
        };
        let rest = std::str::from_utf8(rest).ok()?;
        let header = rest.split(" @@").next()?;
        let (old_part, new_part) = header.split_once(" +")?;
        let (old_start, old_end) = parse_u0_range(old_part)?;
        let (new_start, new_end) = parse_u0_range(new_part)?;
        hunks.push(U0Hunk {
            old_start,
            old_end,
            new_start,
            new_end,
        });
    }
    Some(hunks)
}

fn parse_u0_range(part: &str) -> Option<(usize, usize)> {
    let (start_s, count) = match part.split_once(',') {
        Some((s, c)) => (s, c.parse::<usize>().ok()?),
        None => (part, 1usize),
    };
    let start = start_s.parse::<usize>().ok()?;
    if count == 0 {
        // In `git diff -U0`, `start,0` names the insertion/deletion boundary
        // immediately after 1-based line `start`, which is 0-based index `start`.
        Some((start, start))
    } else {
        let zero_start = start.checked_sub(1)?;
        Some((zero_start, zero_start + count))
    }
}

/// Reconstruct 0-based matched line pairs `(old_i, new_i)` from `old_len`,
/// `new_len`, and the sorted non-overlapping zero-context hunks between them.
fn matched_pairs_from_u0(old_len: usize, new_len: usize, hunks: &[U0Hunk]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut oi = 0usize;
    let mut ni = 0usize;
    for hunk in hunks {
        while oi < hunk.old_start && ni < hunk.new_start {
            pairs.push((oi, ni));
            oi += 1;
            ni += 1;
        }
        oi = hunk.old_end;
        ni = hunk.new_end;
    }
    while oi < old_len && ni < new_len {
        pairs.push((oi, ni));
        oi += 1;
        ni += 1;
    }
    pairs
}

/// Relax matched pairs leftwards to undo `git diff`'s hunk compaction toward
/// EOF when an identical line exists closer to the expected local position.
fn tighten_matches_left(old_lines: &[&[u8]], new_lines: &[&[u8]], matches: &mut [(usize, usize)]) {
    for idx in 0..matches.len() {
        let (oi, ni) = matches[idx];
        let lo = if idx == 0 { 0 } else { matches[idx - 1].1 + 1 };
        let expected = if idx == 0 {
            oi
        } else {
            matches[idx - 1].1 + oi.saturating_sub(matches[idx - 1].0)
        };
        let mut best_ni = ni;
        let mut best_key = (ni.abs_diff(expected), ni);
        for (cand, line) in new_lines.iter().enumerate().take(ni + 1).skip(lo) {
            if *line == old_lines[oi] {
                let key = (cand.abs_diff(expected), cand);
                if key < best_key {
                    best_key = key;
                    best_ni = cand;
                }
            }
        }
        matches[idx].1 = best_ni;
    }
}

/// Convert sorted matched line pairs `(old_i, new_i)` back into sorted
/// non-overlapping zero-context hunks.
fn u0_hunks_from_matches(
    old_len: usize,
    new_len: usize,
    matches: &[(usize, usize)],
) -> Vec<U0Hunk> {
    let mut hunks = Vec::new();
    let mut oi = 0usize;
    let mut ni = 0usize;
    for &(mo, mn) in matches {
        if oi < mo || ni < mn {
            hunks.push(U0Hunk {
                old_start: oi,
                old_end: mo,
                new_start: ni,
                new_end: mn,
            });
        }
        oi = mo + 1;
        ni = mn + 1;
    }
    if oi < old_len || ni < new_len {
        hunks.push(U0Hunk {
            old_start: oi,
            old_end: old_len,
            new_start: ni,
            new_end: new_len,
        });
    }
    hunks
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedHunk {
    start: usize,
    end: usize,
    removed_lines: Vec<Vec<u8>>,
    removed_baseline: Vec<usize>,
    inserted_lines: Vec<Vec<u8>>,
    inserted_target: Vec<usize>,
    is_insertion: bool,
    absorbed_neighbor_count: usize,
}

/// Bound the number of distinct tracked-hunk configurations retained per commit
/// so normal merges stay exact and order-invariant while pathological diamond
/// DAGs remain bounded.
const MAX_TRACKED_CONFIGS: usize = 64;

#[derive(Debug, Clone)]
enum CommitBlobState {
    Tracked {
        oid: String,
        configs: Vec<Vec<TrackedHunk>>,
    },
    /// Opaque (binary or non-regular) intermediate state preserving every
    /// reachable parent's last usable `(oid, hunks)` text snapshot so text
    /// restoration at a merge or later commit evaluates all saved text
    /// ancestors symmetrically regardless of parent ordering.
    Unprojectable {
        saved: Vec<(String, Vec<TrackedHunk>)>,
    },
}

fn push_unique_config(dest: &mut Vec<Vec<TrackedHunk>>, hunks: Vec<TrackedHunk>) {
    if dest.len() < MAX_TRACKED_CONFIGS && !dest.contains(&hunks) {
        dest.push(hunks);
    }
}

fn push_unique_saved(
    dest: &mut Vec<(String, Vec<TrackedHunk>)>,
    oid: String,
    hunks: Vec<TrackedHunk>,
) {
    if dest.len() < MAX_TRACKED_CONFIGS && !dest.iter().any(|(o, h)| o == &oid && h == &hunks) {
        dest.push((oid, hunks));
    }
}

fn collect_saved_from_state(state: &CommitBlobState, dest: &mut Vec<(String, Vec<TrackedHunk>)>) {
    match state {
        CommitBlobState::Tracked { oid, configs } => {
            for hunks in configs {
                push_unique_saved(dest, oid.clone(), hunks.clone());
            }
        }
        CommitBlobState::Unprojectable { saved } => {
            for (oid, hunks) in saved {
                push_unique_saved(dest, oid.clone(), hunks.clone());
            }
        }
    }
}

/// Resolve all distinct tracked-hunk configurations for `target_oid` from
/// `candidates` symmetrically (parent-order invariant):
/// - If any candidate has `src_oid == target_oid`, retain all distinct exact
///   matches.
/// - Otherwise project every candidate `src_oid -> target_oid` and retain all
///   distinct projected configurations so downstream commits check survival
///   against every eligible parent's tracked spans regardless of parent order.
fn resolve_all_configs(
    cwd: &Path,
    target_oid: &str,
    candidates: &[(String, Vec<TrackedHunk>)],
) -> Option<Vec<Vec<TrackedHunk>>> {
    let target_bytes = gitio::read_blob(cwd, target_oid).ok()?;
    if target_bytes.contains(&0) {
        return None;
    }

    let mut exact = Vec::new();
    for (src_oid, src_hunks) in candidates {
        if src_oid == target_oid {
            push_unique_config(&mut exact, src_hunks.clone());
        }
    }
    if !exact.is_empty() {
        return Some(exact);
    }

    let mut projected_all = Vec::new();
    for (src_oid, src_hunks) in candidates {
        let mut projected = src_hunks.clone();
        if project_hunks_between_blobs(cwd, src_oid, target_oid, &mut projected).is_some() {
            push_unique_config(&mut projected_all, projected);
        }
    }
    (!projected_all.is_empty()).then_some(projected_all)
}

fn init_tracked_hunks(
    pre_lines: &[&[u8]],
    fix_lines: &[&[u8]],
    fix_hunks: &[U0Hunk],
) -> Vec<TrackedHunk> {
    let mut tracked = Vec::with_capacity(fix_hunks.len());
    for hunk in fix_hunks {
        let old_lines = &pre_lines[hunk.old_start..hunk.old_end];
        let new_lines = &fix_lines[hunk.new_start..hunk.new_end];
        let is_insertion = old_lines.is_empty() && !new_lines.is_empty();

        let mut removed_lines = Vec::new();
        let mut removed_baseline = Vec::new();
        if !old_lines.is_empty() {
            let mut new_counts: std::collections::HashMap<&[u8], usize> =
                std::collections::HashMap::new();
            for &line in new_lines {
                *new_counts.entry(line).or_insert(0) += 1;
            }
            let mut seen = std::collections::HashSet::new();
            for &line in old_lines {
                if let Some(count) = new_counts.get_mut(line) {
                    if *count > 0 {
                        *count -= 1;
                        continue;
                    }
                }
                if seen.insert(line) {
                    let baseline = new_lines.iter().filter(|&&l| l == line).count();
                    removed_lines.push(line.to_vec());
                    removed_baseline.push(baseline);
                }
            }
        }

        let mut inserted_lines = Vec::new();
        let mut inserted_target = Vec::new();
        if is_insertion {
            let mut seen = std::collections::HashSet::new();
            for &line in new_lines {
                if seen.insert(line) {
                    let target = new_lines.iter().filter(|&&l| l == line).count();
                    inserted_lines.push(line.to_vec());
                    inserted_target.push(target);
                }
            }
        }

        tracked.push(TrackedHunk {
            start: hunk.new_start,
            end: hunk.new_end,
            removed_lines,
            removed_baseline,
            inserted_lines,
            inserted_target,
            is_insertion,
            absorbed_neighbor_count: 0,
        });
    }
    tracked
}

fn project_tracked_hunk(
    tracked: &mut TrackedHunk,
    p_lines: &[&[u8]],
    c_lines: &[&[u8]],
    hunks: &[U0Hunk],
) {
    let s = tracked.start;
    let e = tracked.end;

    // When separate diff hunks touch both boundaries of `span` and shift it
    // across inserted/deleted lines (or insert right at `s` or `e`), include
    // both boundary hunks so relocating a copy of fixed text across edited
    // neighbors cannot detach `span` from a rollback at the original site.
    let left_touch = hunks.iter().find(|d| d.old_end == s);
    let right_touch = hunks.iter().find(|d| d.old_start == e);
    let boundary_relocation = if s < e {
        if let (Some(dl), Some(dr)) = (left_touch, right_touch) {
            let dl_delta =
                (dl.new_end - dl.new_start) as isize - (dl.old_end - dl.old_start) as isize;
            let dr_delta =
                (dr.new_end - dr.new_start) as isize - (dr.old_end - dr.old_start) as isize;
            dl.old_start == s
                || dr.old_end == e
                || (dl_delta > 0 && dr_delta < 0)
                || (dl_delta < 0 && dr_delta > 0)
        } else {
            false
        }
    } else {
        false
    };

    let mut delta_before: isize = 0;
    let mut first_overlap: Option<usize> = None;
    let mut last_overlap: Option<usize> = None;

    for (idx, d) in hunks.iter().enumerate() {
        let overlaps = if s == e {
            d.old_start <= s && d.old_end >= e
        } else if (d.old_start < e && d.old_end > s)
            || (boundary_relocation && (d.old_end == s || d.old_start == e))
        {
            true
        } else {
            tracked.is_insertion
                && tracked.absorbed_neighbor_count > 0
                && (e - s) <= tracked.absorbed_neighbor_count
                && d.old_start == d.old_end
                && (d.old_start == s || d.old_start == e)
        };

        if overlaps {
            if first_overlap.is_none() {
                first_overlap = Some(idx);
            }
            last_overlap = Some(idx);
        } else if d.old_end <= s && first_overlap.is_none() {
            delta_before += (d.new_end - d.new_start) as isize - (d.old_end - d.old_start) as isize;
        }
    }

    let (Some(first_idx), Some(last_idx)) = (first_overlap, last_overlap) else {
        tracked.start = (s as isize + delta_before) as usize;
        tracked.end = (e as isize + delta_before) as usize;
        return;
    };

    let first = hunks[first_idx];
    let last = hunks[last_idx];

    let (new_start, left_absorbed) = if first.old_start <= s {
        (first.new_start, &p_lines[first.old_start..s])
    } else {
        ((s as isize + delta_before) as usize, &p_lines[0..0])
    };
    let (new_end, right_absorbed) = if last.old_end >= e {
        (last.new_end, &p_lines[e..last.old_end])
    } else {
        (last.new_end + (e - last.old_end), &p_lines[0..0])
    };

    for &line in left_absorbed.iter().chain(right_absorbed.iter()) {
        tracked.absorbed_neighbor_count += 1;
        for (i, rem) in tracked.removed_lines.iter().enumerate() {
            if rem.as_slice() == line {
                tracked.removed_baseline[i] += 1;
            }
        }
        for (i, ins) in tracked.inserted_lines.iter().enumerate() {
            if ins.as_slice() == line {
                tracked.inserted_target[i] += 1;
            }
        }
    }

    // Clamp `removed_baseline` to the count still present in `c_lines[new_start..new_end]`
    // so an absorbed neighboring duplicate that disappears in this commit cannot
    // offset a later commit restoring the defective line at the fixed site.
    for (rem, base) in tracked
        .removed_lines
        .iter()
        .zip(&mut tracked.removed_baseline)
    {
        let present_in_new = c_lines[new_start..new_end]
            .iter()
            .filter(|&&line| line == rem.as_slice())
            .count();
        *base = (*base).min(present_in_new);
    }

    tracked.start = new_start;
    tracked.end = new_end;
}

fn tracked_hunk_survives(tracked: &TrackedHunk, head_lines: &[&[u8]]) -> bool {
    let end = tracked.end.min(head_lines.len());
    let start = tracked.start.min(end);
    let head_win = &head_lines[start..end];
    if !tracked.is_insertion {
        for (rem, &baseline) in tracked.removed_lines.iter().zip(&tracked.removed_baseline) {
            let count = head_win.iter().filter(|&&l| l == rem.as_slice()).count();
            if count > baseline {
                return false;
            }
        }
        true
    } else if !tracked.inserted_lines.is_empty() {
        let intact = tracked
            .inserted_lines
            .iter()
            .zip(&tracked.inserted_target)
            .all(|(ins, &target)| {
                head_win.iter().filter(|&&l| l == ins.as_slice()).count() >= target
            });
        intact || head_win.len() > tracked.absorbed_neighbor_count
    } else {
        true
    }
}

/// Project `initial` tracked hunks across a single blob transition `from_oid -> to_oid`.
fn project_hunks_between_blobs(
    cwd: &Path,
    from_oid: &str,
    to_oid: &str,
    hunks: &mut [TrackedHunk],
) -> Option<()> {
    let from_bytes = gitio::read_blob(cwd, from_oid).ok()?;
    let to_bytes = gitio::read_blob(cwd, to_oid).ok()?;
    if from_bytes.contains(&0) || to_bytes.contains(&0) {
        return None;
    }
    let from_lines = split_lines(&from_bytes);
    let to_lines = split_lines(&to_bytes);
    let diff = gitio::diff_blobs_u0(cwd, from_oid, to_oid).ok()?;
    let raw_hunks = parse_u0_hunks(&diff)?;
    let mut matches = matched_pairs_from_u0(from_lines.len(), to_lines.len(), &raw_hunks);
    tighten_matches_left(&from_lines, &to_lines, &mut matches);
    let tight_hunks = u0_hunks_from_matches(from_lines.len(), to_lines.len(), &matches);
    for tracked in hunks {
        project_tracked_hunk(tracked, &from_lines, &to_lines, &tight_hunks);
    }
    Some(())
}

/// Track each `fix_commit` hunk's line span commit-by-commit along
/// `fix_commit..head` so edits to neighboring lines in intermediate commits do
/// not widen the tracked span before a later rollback or refactor.
fn text_blob_survives_along_history(
    cwd: &Path,
    fix_commit: &str,
    head: &str,
    entry: &gitio::RawTreeDiffEntry,
    head_oid: &str,
    commits: &[(String, Vec<String>)],
    edge_diffs: &mut std::collections::HashMap<(String, String), Vec<gitio::RawTreeDiffEntry>>,
) -> bool {
    let (Ok(pre_bytes), Ok(fix_bytes), Ok(head_bytes)) = (
        gitio::read_blob(cwd, &entry.old_oid),
        gitio::read_blob(cwd, &entry.new_oid),
        gitio::read_blob(cwd, head_oid),
    ) else {
        return false;
    };
    if pre_bytes.contains(&0) || fix_bytes.contains(&0) || head_bytes.contains(&0) {
        return true;
    }
    // Check EOF newline termination independently on every text fix, including
    // mixed fixes that change both text and trailing newline.
    let pre_nl = pre_bytes.ends_with(b"\n");
    let fix_nl = fix_bytes.ends_with(b"\n");
    let head_nl = head_bytes.ends_with(b"\n");
    if pre_nl != fix_nl && head_nl == pre_nl {
        return false;
    }

    let pre_lines = split_lines(&pre_bytes);
    let fix_lines = split_lines(&fix_bytes);
    let head_lines = split_lines(&head_bytes);
    if pre_lines == fix_lines {
        return true;
    }

    let Ok(pf_diff) = gitio::diff_blobs_u0(cwd, &entry.old_oid, &entry.new_oid) else {
        return false;
    };
    let Some(fix_hunks) = parse_u0_hunks(&pf_diff) else {
        return true;
    };
    if fix_hunks.is_empty() {
        return true;
    }

    let initial_hunks = init_tracked_hunks(&pre_lines, &fix_lines, &fix_hunks);
    let mut states: std::collections::HashMap<&str, CommitBlobState> =
        std::collections::HashMap::new();
    states.insert(
        fix_commit,
        CommitBlobState::Tracked {
            oid: entry.new_oid.clone(),
            configs: vec![initial_hunks.clone()],
        },
    );

    for (commit, parents) in commits {
        let reachable: Vec<&str> = parents
            .iter()
            .map(String::as_str)
            .filter(|parent| states.contains_key(parent))
            .collect();
        if reachable.is_empty() {
            continue;
        }

        // Prefer unchanged `Tracked` parents (retaining every distinct config
        // across all unchanged tracked parents so `-s ours` inherits only the
        // unchanged branch while tied unchanged parents remain order-invariant).
        // When `commit` changes the entry relative to every reachable parent
        // (including mixed text + opaque merges), project EVERY reachable
        // parent's saved text states symmetrically and retain all distinct
        // resulting configs so downstream commits remain parent-order invariant.
        let mut inherited_tracked_oid: Option<String> = None;
        let mut inherited_tracked_configs: Vec<Vec<TrackedHunk>> = Vec::new();
        let mut has_inherited_unprojectable = false;
        let mut changed_entry: Option<(String, String)> = None;
        let mut all_saved: Vec<(String, Vec<TrackedHunk>)> = Vec::new();
        for &parent in &reachable {
            let key = (parent.to_string(), commit.clone());
            let entries = match edge_diffs.entry(key) {
                std::collections::hash_map::Entry::Occupied(occ) => occ.into_mut(),
                std::collections::hash_map::Entry::Vacant(vac) => {
                    let Ok(diff) = gitio::raw_tree_diff_between(cwd, parent, commit) else {
                        return false;
                    };
                    vac.insert(diff)
                }
            };
            collect_saved_from_state(&states[parent], &mut all_saved);
            match entries.iter().find(|e| e.path == entry.path) {
                None => match &states[parent] {
                    CommitBlobState::Tracked { oid, configs } => {
                        if inherited_tracked_oid.is_none() {
                            inherited_tracked_oid = Some(oid.clone());
                        }
                        for cfg in configs {
                            push_unique_config(&mut inherited_tracked_configs, cfg.clone());
                        }
                    }
                    CommitBlobState::Unprojectable { .. } => {
                        has_inherited_unprojectable = true;
                    }
                },
                Some(changed) => {
                    if changed_entry.is_none() {
                        changed_entry = Some((changed.new_mode.clone(), changed.new_oid.clone()));
                    }
                }
            }
        }

        let next_state = if let Some(oid) = inherited_tracked_oid {
            CommitBlobState::Tracked {
                oid,
                configs: inherited_tracked_configs,
            }
        } else if has_inherited_unprojectable {
            CommitBlobState::Unprojectable { saved: all_saved }
        } else if let Some((c_mode, c_oid)) = changed_entry {
            if !is_regular_blob_mode(&c_mode) {
                CommitBlobState::Unprojectable { saved: all_saved }
            } else {
                match resolve_all_configs(cwd, &c_oid, &all_saved) {
                    Some(configs) => CommitBlobState::Tracked {
                        oid: c_oid,
                        configs,
                    },
                    None => CommitBlobState::Unprojectable { saved: all_saved },
                }
            }
        } else {
            continue;
        };
        states.insert(commit.as_str(), next_state);
    }

    let final_configs = match states.get(head) {
        Some(CommitBlobState::Tracked { configs, .. }) => configs.clone(),
        Some(CommitBlobState::Unprojectable { saved }) => {
            match resolve_all_configs(cwd, head_oid, saved) {
                Some(configs) => configs,
                None => return false,
            }
        }
        None => {
            let mut projected = initial_hunks;
            if project_hunks_between_blobs(cwd, &entry.new_oid, head_oid, &mut projected).is_none()
            {
                return false;
            }
            vec![projected]
        }
    };

    final_configs.iter().all(|hunks| {
        hunks
            .iter()
            .all(|tracked| tracked_hunk_survives(tracked, &head_lines))
    })
}

fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let trimmed = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    trimmed.split(|&b| b == b'\n').collect()
}

/// Open blocking finding IDs for `state` at `current_head`, combining
/// ledger disposition state with Git resolution-commit matching.
pub(crate) fn open_blocking_finding_ids(
    cwd: Option<&Path>,
    state: &ChangeState,
    current_head: Option<&str>,
) -> Vec<String> {
    let resolution_statuses = evaluate_finding_resolutions(cwd, state, current_head);
    state
        .findings
        .values()
        .filter(|f| {
            f.blocks_integration()
                || (f.blocking
                    && resolution_statuses
                        .get(&f.id)
                        .is_some_and(ResolutionCommitStatus::leaves_open))
        })
        .map(|f| f.id.clone())
        .collect()
}

/// Non-merge commits in the change's own `base..head` range paired with their
/// stable Git patch-ids.
fn change_range_patch_ids(
    cwd: &Path,
    state: &ChangeState,
    head: &str,
) -> Result<Vec<(String, String)>> {
    let ps_base = state
        .latest_patchset()
        .map(|p| p.base.as_str())
        .filter(|base| gitio::is_ancestor(cwd, base, head).unwrap_or(false));
    let target_mb = gitio::branch_head(cwd, &state.target_branch)
        .ok()
        .and_then(|target| gitio::merge_base(cwd, &target, head).ok());
    let state_base = gitio::is_ancestor(cwd, &state.base, head)
        .unwrap_or(false)
        .then_some(state.base.as_str());
    let range_base = match (ps_base, target_mb.as_deref()) {
        (Some(pb), Some(tmb)) if gitio::is_ancestor(cwd, pb, tmb).unwrap_or(false) => Some(tmb),
        (Some(pb), _) => Some(pb),
        (None, Some(tmb)) => Some(tmb),
        (None, None) => state_base,
    };
    let Some(base) = range_base else {
        return Ok(Vec::new());
    };
    let mut entries = Vec::new();
    for commit in gitio::range_commits(cwd, base, head)? {
        if gitio::commit_parents(cwd, &commit)?.len() > 1 {
            continue;
        }
        if let Some(pid) = gitio::patch_id(cwd, &commit)? {
            entries.push((commit, pid));
        }
    }
    Ok(entries)
}

#[derive(Debug, Serialize)]
pub struct HoldSummary {
    pub active: bool,
    /// Every active hold, in event-ID order. Holds are independent, so a
    /// caller that acts on one has not cleared the others.
    pub reasons: Vec<HoldEntry>,
}

fn hold_entries(state: &ChangeState) -> Vec<HoldEntry> {
    state
        .holds
        .values()
        .map(|hold| HoldEntry {
            hold_event_id: hold.hold_event_id.clone(),
            reason: hold.reason.clone(),
            held_by: hold.held_by.clone(),
        })
        .collect()
}

/// One active hold, named by the event that set it so a release can lift
/// exactly that one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoldEntry {
    pub hold_event_id: String,
    pub reason: String,
    pub held_by: String,
}

#[derive(Debug, Serialize)]
pub struct BlockerSummary {
    pub open_findings: usize,
    pub blocking_findings: usize,
    pub gate_status: BTreeMap<String, String>,
    pub hold: HoldSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClaimStatus {
    pub claim_id: String,
    pub owner: ClaimIdentity,
    pub active: bool,
    pub expired: bool,
    pub stale: bool,
    pub ttl_seconds: u64,
    pub claimed_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub stage: String,
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<crate::model::BlockerRef>,
    pub stage_started_at: DateTime<Utc>,
    pub age_seconds: u64,
    pub budget_seconds: Option<u64>,
    pub stage_budgets: BTreeMap<crate::model::StageBudget, u64>,
    /// Why the claim this one displaced had its lease cut short, as the
    /// taker stated it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub displaced_reason: Option<String>,
    pub snapshot_author: Option<GitIdentity>,
    pub snapshot_committer: Option<GitIdentity>,
    pub snapshot_claim_actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_mismatch: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct MessageStatus {
    pub event_id: String,
    pub message_type: MessageType,
    pub severity: MessageSeverity,
    pub summary: String,
    pub detail: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub actor: String,
    pub harness: Option<String>,
    pub session: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<&crate::state::MessageEntry> for MessageStatus {
    fn from(message: &crate::state::MessageEntry) -> Self {
        Self {
            event_id: message.event_id.clone(),
            message_type: message.message_type,
            severity: message.severity,
            summary: message.summary.clone(),
            detail: message.detail.clone(),
            metadata: message.metadata.clone(),
            actor: message.actor.clone(),
            harness: message.harness.clone(),
            session: message.session.clone(),
            created_at: message.created_at,
        }
    }
}

/// One reviewer identity's coverage of the change.
///
/// "Somebody reviewed this change" and "somebody reviewed what is about to
/// ship" are different claims, and only the second one is worth anything at
/// integration time. A panel can run correctly for ten rounds and still let
/// the final corrections ship unseen, which is why coverage is measured
/// against the final patchset rather than against participation.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewerCoverage {
    /// The effective author of this reviewer's verdicts and findings.
    pub reviewer: String,
    /// The newest patchset this reviewer filed a verdict or finding against.
    pub last_patchset: String,
    pub verdicts: usize,
    pub findings: usize,
    /// This reviewer saw the patchset that is about to ship.
    pub covers_final: bool,
    /// This reviewer matches a contributor on the patchset it last saw, so its
    /// verdict carries no independence.
    pub is_author: bool,
    /// The contributor matched by this reviewer, when the review is
    /// non-independent for that patchset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_contributor: Option<String>,
    /// Whether the patchset used its compatibility attribution or an explicit
    /// contributor declaration.
    pub contributors_source: &'static str,
    /// The reviewer cannot be placed: arc invented its identity from git
    /// configuration, or it carries the snapshot actor's name with no
    /// `--on-behalf-of` recorded on either side. Reported as unknown rather
    /// than as independence or as self-review, either of which would claim
    /// more than the ledger holds. True whenever the covering verdict's
    /// identity was assumed, matching the guard that rejects such an approval.
    pub attribution_unknown: bool,
}

/// The versioned machine-readable contract agent harnesses program against.
/// Everything here is derivable from the ledger plus Git.
#[derive(Debug, Serialize)]
pub struct ReviewSubject {
    pub patchset_id: String,
    /// The identity that ran the snapshot command.
    pub invoker: String,
    /// Who the attribution policy treats the work as by: the represented
    /// subject when the snapshot was on behalf of one, else the invoker.
    pub effective_author: String,
    /// The effective contributor set the independence check compares: the
    /// explicitly declared set when the patchset carries one, else the
    /// single-element fallback of the effective author.
    pub contributors: Vec<String>,
    /// Where `contributors` came from: an explicitly recorded set, or the
    /// compatibility fallback. A synthesized fallback is not a declaration,
    /// and reporting it as one would let a lead mistake a guess for a claim.
    pub basis: &'static str,
}

/// Build the identity comparison for the patchset a report is reviewing.
/// Closed reports pass the shipped patchset here, while open reports pass the
/// current patchset; the comparison therefore follows the same subject the
/// gate and debt projection use.
pub(crate) fn review_subject_for_patchset(patchset: &state::Patchset) -> ReviewSubject {
    ReviewSubject {
        patchset_id: patchset.id.clone(),
        invoker: patchset.actor.clone(),
        effective_author: patchset.effective_author().to_string(),
        contributors: if patchset.contributors.is_empty() {
            vec![patchset.effective_author().to_string()]
        } else {
            patchset.contributors.clone()
        },
        basis: if patchset.contributors.is_empty() {
            "effective-author"
        } else {
            "explicit-contributors"
        },
    }
}

/// A change's blockers, highest precedence first. Readiness is their absence
/// and the `arc check` code is the first one's, so neither can be stated
/// apart from the list.
#[derive(Debug)]
pub struct Readiness {
    blockers: Vec<Blocker>,
}

impl Readiness {
    pub fn new(blockers: Vec<Blocker>) -> Self {
        Self { blockers }
    }

    pub fn ready(&self) -> bool {
        self.blockers.is_empty()
    }

    pub fn exit_code(&self) -> i32 {
        blockers::exit_code(&self.blockers)
    }
}

impl Serialize for Readiness {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut fields = serializer.serialize_struct("Readiness", 3)?;
        fields.serialize_field("ready_to_integrate", &self.ready())?;
        // The arc-status/1 spelling, kept for consumers that read it.
        fields.serialize_field("integrate_ready", &self.ready())?;
        fields.serialize_field("blockers", &self.blockers)?;
        fields.end()
    }
}

#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub budget_raises: Vec<crate::gate_raise::Applicability>,
    /// The budget suspensions in force on this ledger when the report was
    /// built; empty when none is.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub budget_suspensions: Vec<crate::gate_suspension::Suspension>,
    pub budget_errors: Vec<String>,
    pub budget_rules: Option<crate::gate_budget::BudgetRules>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_context: Option<crate::review_context::ReviewContext>,
    pub integration_mode: crate::integration::Mode,
    pub integration_mode_refusal: Option<String>,
    pub provisional_integrations: Vec<crate::provisional::Obligation>,
    pub unfinished_provisional_integrations: Vec<crate::commands::provisional::PendingTransition>,
    pub obsolete_provisional_ancestry: Vec<String>,
    pub provisional_prerequisites: Vec<crate::model::ProvisionalPrerequisite>,
    pub provisional_staging_destination: Option<String>,
    pub replay_snapshot_required: bool,
    /// Integration's refusal of a reserved-domain commit in the patchset that
    /// would ship, word for word, naming the commit and how to re-author it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved_identity: Option<String>,
    pub recorded_verdict_tips: Vec<String>,
    pub eligible_verdict_tips: Vec<String>,
    pub verdict_assessments: Vec<VerdictAssessment>,
    pub schema: &'static str,
    pub change_id: String,
    pub slug: String,
    pub title: String,
    pub profile: String,
    pub iterating: bool,
    pub state: String,
    pub target_branch: String,
    pub branch: String,
    pub base: String,
    /// The fork that records this change's branch, when one does. A change on
    /// fork work is unintegrable from every checkout: the boundary binds to
    /// the change record rather than to the directory the caller stands in,
    /// and this is the fork the refusal names. `None` for a branch no fork
    /// records, including every branch arc opens itself. Additive in
    /// `arc-status/20`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
    pub worktree: Option<String>,
    /// The pool slot whose checkout the change uses, and whether its lease
    /// has ended; absent for a change that never leased one. Additive in
    /// `arc-status/27`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<crate::state::SlotRecord>,
    pub opened_by: String,
    pub opened_harness: Option<String>,
    pub tags: Vec<String>,
    /// The fork `begin --from-fork` promoted this change's work from, when
    /// one did. The recorded link is the fork slug and the source base, head,
    /// and tree the work was read from; it is not review coverage and not an
    /// obligation. Additive in `arc-status/21`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_fork: Option<crate::model::ForkProvenance>,
    pub blocked_by: Vec<String>,
    pub assigned_to: Option<String>,
    pub priority: i32,
    pub messages: Vec<MessageStatus>,
    pub blocks: Vec<String>,
    /// Canonical closures; local provisional prerequisite references are separate.
    pub blocker_status: BlockerStatus,
    /// The change's must-land-together component, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lands_with: Option<crate::commands::land::LandsWithReport>,
    /// The prepared, unfinished landing attempt holding the change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_landing: Option<String>,
    pub claim: Option<ClaimStatus>,
    pub current_head: Option<String>,
    /// The destination revision used for this report's merged tree and evidence.
    #[serde(skip)]
    pub observed_target_head: Option<String>,
    pub needs_rebase: bool,
    /// The tree a merge into the target would ship, which is what the required
    /// gates have to answer for. It is the head's own tree while the change is
    /// rebased, and content neither branch committed once it is behind. `None`
    /// where no single tree exists to name: no branch, no target, a textual
    /// conflict, or a report derived from the ledger alone. Additive in
    /// `arc-status/15`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_tree: Option<String>,
    pub latest_patchset: Option<crate::state::Patchset>,
    /// The identities the independence check compares on the latest
    /// patchset: the effective author and the effective contributor set,
    /// with which of the two supplied them. Derived from the same Patchset
    /// methods the rejection check uses, so text and JSON cannot disagree
    /// about who is being compared. Absent with no patchset: an
    /// unsnapshotted change has no author set to report, and inventing one
    /// would put words in nobody's mouth. Additive in arc-status/18.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_subject: Option<ReviewSubject>,
    pub brief: Option<BriefStatus>,
    /// Literal comparison with the newest snapshot, including after closure.
    pub head_matches_latest_patchset: bool,
    /// Patchset used to describe head drift: the closure's source patchset
    /// when recorded, otherwise the newest snapshot.
    pub comparison_patchset: Option<String>,
    pub head_matches_comparison_patchset: bool,
    pub worktree_dirty: Option<bool>,
    pub verdict: Option<VerdictStatus>,
    /// External decisions remain distinct from observed local verdicts.
    /// Additive in `arc-status/24`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub external_verdicts: Vec<ExternalVerdictStatus>,
    /// Whether a local or eligible external approval covers the current head.
    /// Additive in `arc-status/24`.
    pub has_valid_approval: bool,
    /// Who reviewed what, newest coverage first. Additive in arc-status/6.
    pub review_map: Vec<ReviewerCoverage>,
    /// What a lead should know before integrating and arc will not refuse for:
    /// thin or stale review coverage, review only by the brief's author, an
    /// undischarged audit obligation. Never affects readiness or exit status.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub advisories: Vec<Advisory>,
    pub findings: Vec<FindingSummary>,
    pub open_blocking_findings: Vec<String>,
    pub holds: Vec<HoldEntry>,
    pub gates: Vec<GateStatus>,
    /// Source files for the policy rules that apply to this change.
    /// Additive in `arc-status/23`.
    pub policy_sources: BTreeMap<String, Vec<String>>,
    /// Where the declarations in play disagree: the checkout against the
    /// target, or the change's gate against the target's. Readiness answers
    /// for the target's declarations plus the gates the change adds, so
    /// `status`, `check` and `integrate` agree. Additive in `arc-status/26`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub declaration_notes: Vec<String>,
    pub probes: Vec<ProbeStatus>,
    pub blocker_summary: BlockerSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_rejection_reason: Option<String>,
    /// Several verdicts replace the same earlier verdict, so none of them is
    /// authoritative and the change carries no usable verdict until one
    /// supersedes them all. Reported because the alternative reads as though
    /// nobody reviewed the change.
    #[serde(skip_serializing_if = "is_false")]
    pub verdict_contested: bool,
    /// A declared debt supplied a missing verdict or let a self-approval
    /// stand. Only then is the waiver an authorization input.
    #[serde(skip_serializing_if = "is_false")]
    pub approval_waived_by_debt: bool,
    pub next_action: String,
    /// The allowed guidance actions for the current state, in the order a
    /// lead should weigh them. Empty where a single action is the only
    /// correct one. Guidance, not an authorization token: nothing is written
    /// by reading this list, and the user or lead executes the action
    /// themselves. Additive in arc-status/18.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub review_options: Vec<&'static str>,
    /// Facts sessions kept while working, oldest first. Carried on the report
    /// so `resume` and `status --json` hand them back without a second read.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<crate::state::KeptContext>,
    /// Whether this change is in scope for the independent-review rule, and
    /// which declaration put it there.
    pub danger: DangerScope,
    pub ready_reason: String,
    #[serde(flatten)]
    readiness: Readiness,
    pub closure: Option<crate::state::ClosureState>,
    /// The head a contribution was last found ready to send at. Additive in
    /// `arc-status/25`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_to_send: Option<crate::state::ReadyToSendEntry>,
    /// A declared review obligation with no audit answering it. Additive in
    /// arc-status/6.
    pub debt_outstanding: bool,
    /// Where the declared review obligation stands: `declared` before the
    /// change integrates, `owed` after it until a review answers it,
    /// `discharged` once one has. Absent when no debt was declared.
    /// `debt_outstanding` is true only for `owed`. Additive in `arc-status/27`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debt_standing: Option<crate::state::DebtStanding>,
    /// The change's gating approval was recorded as owed corroboration, and
    /// no audit has supplied it. Reported beside `debt_outstanding`
    /// because it is the same obligation seen one step earlier: debt says a
    /// review never happened, this says one happened and is not yet trusted.
    /// Additive in `arc-status/9`.
    pub provisional_approval_outstanding: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debt: Option<crate::state::Debt>,
    /// The dirty-tree waiver in force at the current head, if one is. A waiver
    /// naming an earlier revision is spent, and is not reported as though it
    /// still excused anything. Additive in `arc-status/10`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty_tree_waiver: Option<crate::state::DirtyTreeWaiver>,
    /// Reviews recorded after integration, never mixed into `verdict`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub audit_verdicts: Vec<crate::state::AuditVerdictEntry>,
    /// Checks observed after integration, never mixed into `gates`.
    /// Additive in `arc-status/27`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub post_integration_verifications: Vec<crate::state::PostIntegrationVerificationEntry>,
    /// Observed forge (hosted-PR) facts. Absent for changes with no forge
    /// events that are not on the `forge` profile. Additive in arc-status/5.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forge: Option<crate::forge::ForgeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forge_view: Option<serde_json::Value>,
    #[serde(skip)]
    pub provenance_check_enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct BriefStatus {
    pub version: usize,
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
    /// How far the brief's base revision sits from the change head, so a
    /// reader knows whether the brief's citations still describe the tree.
    /// Additive in `arc-status/11`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_drift: Option<BriefBaseDrift>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub acceptance_probes: Vec<crate::model::AcceptanceProbe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_slice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_source: Option<crate::model::PlanSource>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum BriefBaseDrift {
    /// The brief was checked against the revision the change now heads.
    Current,
    /// The head descends from the base by this many commits.
    Behind { commits: usize },
    /// The base is not in the change's history at all — rebased or rewound —
    /// so it describes a tree this change no longer contains.
    Detached,
}

impl BriefBaseDrift {
    /// The clause a brief-printing surface appends after the base revision.
    /// `None` when the base is the head and there is nothing to say. One
    /// definition, because four surfaces print it and a wording that lived in
    /// four places would go stale in three of them.
    pub fn annotation(&self) -> Option<String> {
        match self {
            BriefBaseDrift::Current => None,
            BriefBaseDrift::Behind { commits } => Some(format!(
                " — **{commits} commits behind the change head**; line citations may have decayed"
            )),
            BriefBaseDrift::Detached => Some(
                " — **not in this change's history** (rebased or rewound); line citations describe a tree this change no longer contains"
                    .to_string(),
            ),
        }
    }
}

pub fn brief_base_drift(
    cwd: &Path,
    base_revision: Option<&str>,
    head: Option<&str>,
) -> Option<BriefBaseDrift> {
    let (Some(base), Some(head)) = (base_revision, head) else {
        return None;
    };
    if base == head {
        return Some(BriefBaseDrift::Current);
    }
    if !gitio::is_ancestor(cwd, base, head).ok()? {
        return Some(BriefBaseDrift::Detached);
    }
    Some(BriefBaseDrift::Behind {
        commits: gitio::commit_count(cwd, base, head).ok()?,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct VerdictAssessment {
    pub event_id: String,
    pub origin_replica_id: String,
    pub origin_name: Option<String>,
    pub actor: String,
    pub on_behalf_of: Option<String>,
    pub admission_provenance: String,
    pub covers_current_head: bool,
    pub gating_eligible: bool,
    pub selected_authorizing_event: bool,
    pub rejection_reason: Option<String>,
    /// Under `review_clearance = "assessed"`, `clearing` or `findings`: whether
    /// the verdict clears the work or only records what it found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    /// Why a `findings` verdict does not clear.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings_reason: Option<&'static str>,
}

pub fn verdict_assessments(
    state: &ChangeState,
    current_head: Option<&str>,
    policy: &crate::policy::PolicyFile,
    danger: &DangerScope,
    implementers: &BTreeMap<String, Vec<String>>,
) -> Vec<VerdictAssessment> {
    let selected = state
        .latest_verdict()
        .map(|verdict| verdict.event_id.as_str());
    state
        .verdicts
        .iter()
        .map(|verdict| {
            let assessed = state.verdict_uses_assessed(verdict, policy.policy.review_clearance);
            let gap = assessed
                .then(|| state.verdict_clearance_gap(verdict))
                .flatten();
            let patchset = state
                .patchsets
                .iter()
                .find(|patchset| patchset.id == verdict.patchset_id);
            let covers = patchset
                .is_some_and(|patchset| Some(patchset.approved_head.as_str()) == current_head);
            let reason = state
                .privilege_scope
                .as_ref()
                .and_then(|scope| scope.rejection(&verdict.event_id, &verdict.origin_replica_id))
                .or_else(|| {
                    patchset
                        .filter(|_| {
                            policy.policy.forbid_self_approval
                                && danger.dangerous
                                && !state.debt_waives_latest_patchset()
                        })
                        .and_then(|patchset| {
                            crate::approval::self_approval_rejection(
                                patchset,
                                verdict.effective_author(),
                                verdict.author_assumed(),
                                &verdict.judged_models(),
                                implementers
                                    .get(&patchset.id)
                                    .map_or(&[][..], Vec::as_slice),
                                state.verdict_tiebreak(verdict).is_some(),
                            )
                        })
                })
                .or_else(|| {
                    gap.as_ref()
                        .filter(|_| {
                            verdict.verdict == Verdict::Approved
                                && !state.debt_waives_latest_patchset()
                        })
                        .map(crate::approval::findings_rejection)
                });
            let eligible = reason.is_none();
            VerdictAssessment {
                event_id: verdict.event_id.clone(),
                origin_replica_id: verdict.origin_replica_id.clone(),
                origin_name: state
                    .privilege_scope
                    .as_ref()
                    .and_then(|scope| scope.origin_names.get(&verdict.origin_replica_id))
                    .cloned(),
                actor: verdict.actor.clone(),
                on_behalf_of: verdict.on_behalf_of.clone(),
                admission_provenance: state
                    .privilege_scope
                    .as_ref()
                    .map(|scope| scope.provenance(&verdict.event_id))
                    .unwrap_or("legacy-unknown")
                    .into(),
                covers_current_head: covers,
                gating_eligible: eligible,
                selected_authorizing_event: covers
                    && state
                        .latest_patchset()
                        .is_some_and(|patchset| patchset.id == verdict.patchset_id)
                    && eligible
                    && selected == Some(verdict.event_id.as_str())
                    && verdict.verdict == Verdict::Approved,
                rejection_reason: reason,
                kind: assessed.then_some(if gap.is_some() {
                    "findings"
                } else {
                    "clearing"
                }),
                findings_reason: gap.as_ref().map(|why| why.as_str()),
            }
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct VerdictStatus {
    pub event_id: String,
    pub origin_replica_id: String,
    pub verdict: Verdict,
    pub patchset_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Why this verdict is owed corroboration, when the reviewer said it is.
    /// Additive in `arc-status/9`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provisional: Option<String>,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    /// Whether arc invented the identity this verdict is attributed to rather
    /// than someone declaring it. Additive in `arc-status/6`.
    pub author_assumed: bool,
    /// Whether this verdict applies to the newest patchset and current head,
    /// independent of whether it is an approval or satisfies review policy.
    pub matches_current_head: bool,
    pub valid_for_current_head: bool,
}

#[derive(Debug, Serialize)]
pub struct ExternalVerdictStatus {
    /// External source marker is part of the public status projection.
    /// Additive in `arc-status/24`.
    pub source: &'static str,
    pub event_id: String,
    pub revision: String,
    pub verdict: crate::model::ExternalVerdict,
    pub decided_by: String,
    pub reference: String,
    pub recorded_by: String,
    pub created_at: DateTime<Utc>,
    /// The named revision equals the current patchset head.
    pub matches_current_patchset: bool,
    /// The external approval satisfies review policy for this head.
    pub gates_current_head: bool,
    pub findings: Vec<crate::model::ExternalFinding>,
}

/// Build the status report: replayed ledger state joined with live Git
/// facts, dependency state, and the declared gate policy.
pub use crate::model::{DangerRule, DangerScope};

impl DangerScope {
    /// Independent review is owed when the repository forbids self-approval
    /// *and* this change is in scope for that rule.
    pub fn requires_independent_review(&self, policy: &PolicyFile) -> bool {
        policy.policy.forbid_self_approval && self.dangerous
    }

    /// One line naming why the gate landed where it did.
    pub fn explain(&self) -> String {
        match self.rule {
            DangerRule::NotDeclared => {
                "the project declares no dangerous surfaces, so every change needs one".into()
            }
            DangerRule::Escalated => "the change was opened --dangerous".into(),
            DangerRule::DeclaredPath => {
                format!("it touches declared surfaces: {}", self.paths.join(", "))
            }
            DangerRule::Untouched => "it touches no declared dangerous surface".into(),
            DangerRule::Undetermined => {
                "the touched paths could not be established, so it is assumed dangerous".into()
            }
        }
    }

    /// No working tree to diff. An unknown surface is the one case where
    /// guessing wrong must not lower the bar.
    pub fn undetermined(state: &ChangeState) -> Self {
        DangerScope {
            dangerous: true,
            rule: if state.dangerous {
                DangerRule::Escalated
            } else {
                DangerRule::Undetermined
            },
            paths: Vec::new(),
        }
    }

    pub(crate) fn resolve(
        state: &ChangeState,
        policy: &PolicyFile,
        cwd: &Path,
        head: Option<&str>,
    ) -> Self {
        if state.dangerous {
            return DangerScope {
                dangerous: true,
                rule: DangerRule::Escalated,
                paths: Vec::new(),
            };
        }
        // An undeclared list keeps the previous uniform behaviour, so
        // adopting the feature is opt-in rather than a silent loosening.
        if !policy.danger.is_declared() {
            return DangerScope {
                dangerous: true,
                rule: DangerRule::NotDeclared,
                paths: Vec::new(),
            };
        }
        let Some(head) = head else {
            return DangerScope::undetermined(state);
        };
        let Ok(changed) = gitio::changed_paths(cwd, &state.base, head) else {
            return DangerScope::undetermined(state);
        };
        let paths = policy.danger.matching(changed.iter().map(String::as_str));
        DangerScope {
            dangerous: !paths.is_empty(),
            rule: if paths.is_empty() {
                DangerRule::Untouched
            } else {
                DangerRule::DeclaredPath
            },
            paths,
        }
    }
}

/// The tree each revision names, for evidence recorded before arc kept the
/// tree beside it.
///
/// Resolved once per distinct revision rather than per gate, and only where a
/// tree-keyed reading is what the report needs. A revision whose commit is
/// gone is absent here, and evidence naming it then matches no tree.
pub(crate) fn legacy_evidence_trees(state: &ChangeState, cwd: &Path) -> BTreeMap<String, String> {
    state
        .verifications
        .iter()
        .filter(|entry| entry.tree.is_none())
        .map(|entry| entry.revision.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|revision| {
            Some((
                revision.to_string(),
                gitio::commit_tree(cwd, revision).ok()?,
            ))
        })
        .collect()
}

pub fn build(
    state: &ChangeState,
    cwd: &Path,
    declarations: &Declarations,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
    fork: Option<String>,
) -> Result<StatusReport> {
    let mut selected = state.clone();
    selected.set_review_context(declarations.review_context.clone());
    let state = &selected;
    let mut report = build_at(
        state,
        cwd,
        &declarations.gates,
        &declarations.policy,
        declarations.target_unreadable,
        dependency_status,
        blocks,
        Utc::now(),
        fork,
    )?;
    report.declaration_notes = declarations.notes.clone();
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
pub fn build_at(
    state: &ChangeState,
    cwd: &Path,
    gates: &GatesFile,
    policy: &PolicyFile,
    target_unreadable: bool,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
    now: DateTime<Utc>,
    fork: Option<String>,
) -> Result<StatusReport> {
    let shape = if target_unreadable || state.is_closed() {
        crate::integration::Shape::FfOnly
    } else {
        crate::style::resolved_style(
            cwd,
            &state.target_branch,
            crate::style::StyleFlags::default(),
        )?
        .shape()
    };
    build_at_with_shape(
        state,
        cwd,
        gates,
        policy,
        target_unreadable,
        dependency_status,
        blocks,
        now,
        fork,
        shape,
        None,
    )
}

/// Ask Git, in batches, for what `build_at_with_shape` reads about each open
/// change among `states`, so that a listing reporting on all of them runs the
/// same Git commands however many there are.
pub fn prefetch_listing<'a>(cwd: &Path, states: impl Iterator<Item = &'a ChangeState>) {
    if !crate::store::shares_reads() {
        return;
    }
    let mut wanted = gitio::listing::Wanted::default();
    for state in states.filter(|state| !state.is_closed()) {
        let target = gitio::branch_head(cwd, &state.target_branch).ok();
        if let (Some(head), Some(target)) = (gitio::branch_head(cwd, &state.branch).ok(), &target) {
            wanted.merges.push((target.clone(), head));
        }
        if let (Some(patchset), Some(target)) = (state.latest_patchset(), &target) {
            wanted
                .ancestry
                .push((target.clone(), patchset.head.clone()));
            wanted
                .identities
                .push((target.clone(), patchset.base.clone(), patchset.head.clone()));
        }
        if let Some(worktree) = state
            .worktree
            .as_deref()
            .map(Path::new)
            .filter(|worktree| worktree.exists())
        {
            wanted.worktrees.push(worktree.to_path_buf());
        }
    }
    gitio::listing::prefetch(cwd, &wanted);
}

#[allow(clippy::too_many_arguments)]
pub fn build_at_with_shape(
    state: &ChangeState,
    cwd: &Path,
    gates: &GatesFile,
    policy: &PolicyFile,
    target_unreadable: bool,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
    now: DateTime<Utc>,
    fork: Option<String>,
    shape: crate::integration::Shape,
    evaluate: Option<CreateEvaluationCheckout<'_>>,
) -> Result<StatusReport> {
    // A closed change is judged at the head its ledger recorded: its branch
    // may be deleted or reused, and its target is never merged into again.
    let (current_head, target_head) = if state.is_closed() {
        (state.closed_head().map(str::to_string), None)
    } else {
        (
            gitio::branch_head(cwd, &state.branch).ok(),
            gitio::branch_head(cwd, &state.target_branch).ok(),
        )
    };
    // One merge-tree run answers both questions it can answer: whether the
    // text conflicts, and what a clean merge would ship.
    let merge = match (&current_head, &target_head) {
        (Some(head), Some(target)) => Some(gitio::merge_outcome(cwd, target, head)?),
        _ => None,
    };
    let diverged = if shape == crate::integration::Shape::FfOnly && policy.contribution.is_none() {
        match (state.latest_patchset(), &target_head) {
            (Some(patchset), Some(target)) => {
                let (ancestor, contained) =
                    crate::integration::ancestry(cwd, target, &patchset.head)?;
                !crate::integration::ff_eligible(ancestor, contained)
            }
            _ => false,
        }
    } else {
        false
    };
    let needs_rebase = diverged || merge.as_ref().is_some_and(|merge| merge.conflicts);
    let merged_tree = merge.and_then(|merge| merge.tree);
    // The predicate integration refuses on, so readiness never reads ready
    // for a patchset integration would refuse. A read that fails leaves the
    // refusal to integration, which reports the failure itself.
    let reserved = match (state.is_closed(), state.latest_patchset()) {
        (false, Some(patchset)) => crate::git_health::shipped_reserved(
            cwd,
            &patchset.base,
            &patchset.head,
            target_head.as_deref(),
        )
        .ok()
        .flatten(),
        _ => None,
    };
    let worktree_dirty = if state.is_closed() {
        None
    } else {
        state
            .worktree
            .as_deref()
            .map(Path::new)
            .filter(|worktree| worktree.exists())
            .map(|worktree| gitio::is_clean(worktree).map(|clean| !clean))
            .transpose()?
    };
    let danger = DangerScope::resolve(state, policy, cwd, current_head.as_deref());
    let role_advisories = if state.is_closed() {
        Vec::new()
    } else {
        role_trailer_advisories(
            state,
            cwd,
            target_head.as_deref(),
            current_head.as_deref(),
            policy,
            &danger,
        )
    };
    let commit_destination = if state.is_closed() {
        CommitDestination::None
    } else {
        CommitDestination::Target(target_head.as_deref())
    };
    let mut report = build_report(
        state,
        gates,
        policy,
        target_unreadable,
        dependency_status,
        blocks,
        now,
        Some(cwd),
        ProbeSite::Live {
            invoking: cwd,
            create: evaluate,
        },
        current_head,
        commit_destination,
        needs_rebase,
        reserved,
        merged_tree,
        worktree_dirty,
        danger,
        fork,
    )?;
    report.observed_target_head = target_head;
    report.advisories.extend(role_advisories);
    Ok(report)
}

/// Build a report from a state replayed to a past event. Live Git facts are
/// not consulted: an open state is judged at its latest patchset's head, a
/// closed one at the head its closure recorded, and rebase state is not
/// simulated. Used by `--at`.
/// A report derived from the ledger alone, for callers with no working tree.
///
/// `repo` is any path inside the repository. It is not a working tree and is
/// not read as one: resolving the danger scope needs the recorded base and
/// patchset head plus the objects they name, which every clone already has.
/// Passing `None` leaves the scope undetermined, which assumes dangerous.
#[allow(clippy::too_many_arguments)]
pub fn build_as_of(
    state: &ChangeState,
    gates: &GatesFile,
    policy: &PolicyFile,
    target_unreadable: bool,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
    now: DateTime<Utc>,
    repo: Option<&Path>,
    fork: Option<String>,
) -> Result<StatusReport> {
    let current_head = match state.closed_head() {
        Some(head) => Some(head.to_string()),
        None => state
            .latest_patchset()
            .map(|patchset| patchset.head.clone()),
    };
    build_report(
        state,
        gates,
        policy,
        target_unreadable,
        dependency_status,
        blocks,
        now,
        repo,
        ProbeSite::Replay,
        current_head.clone(),
        CommitDestination::None,
        false,
        None,
        None,
        None,
        match repo {
            Some(repo) => DangerScope::resolve(state, policy, repo, current_head.as_deref()),
            None => DangerScope::undetermined(state),
        },
        fork,
    )
}

static REPORTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many status reports this process has built.
pub(crate) fn reports_built() -> usize {
    REPORTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// `probe_site` says where a declared environment probe may run: a live
/// report probes the content the evidence is looked up at, and a ledger
/// replay reports recorded identities without probing.
#[allow(clippy::too_many_arguments)]
fn build_report(
    state: &ChangeState,
    gates: &GatesFile,
    policy: &PolicyFile,
    target_unreadable: bool,
    dependency_status: BlockerStatus,
    blocks: Vec<String>,
    now: DateTime<Utc>,
    cwd: Option<&Path>,
    probe_site: ProbeSite<'_>,
    current_head: Option<String>,
    commit_destination: CommitDestination<'_>,
    needs_rebase: bool,
    reserved: Option<crate::git_health::ReservedCommit>,
    merged_tree: Option<String>,
    worktree_dirty: Option<bool>,
    danger: DangerScope,
    fork: Option<String>,
) -> Result<StatusReport> {
    REPORTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    crate::perf::count("status_reports", 1);
    let provenance_mode = policy.provenance.git_identity;
    let provenance_check_enabled = provenance_mode == crate::config::GitIdentityMode::PerActor;
    let mut latest_patchset = state.latest_patchset().cloned();
    if let Some(patchset) = &mut latest_patchset {
        patchset.provenance_mismatch = state::provenance_mismatch(
            provenance_mode,
            patchset.claim_actor.as_deref(),
            patchset.author.as_ref(),
            patchset.committer.as_ref(),
        );
    }
    // The review subject derives from the same methods the independence
    // check calls, so the projection cannot drift from what the gate does.
    // Once closed, the closure's source patchset is the subject that shipped;
    // latest_patchset remains the separate history projection.
    let review_subject = state
        .debt_subject_patchset()
        .map(review_subject_for_patchset);
    let head_matches = match (&current_head, &latest_patchset) {
        (Some(h), Some(p)) => *h == p.head,
        _ => false,
    };
    let comparison = match state
        .closure
        .as_ref()
        .and_then(|closure| closure.source_patchset_id.as_deref())
    {
        Some(id) => state.patchsets.iter().find(|patchset| patchset.id == id),
        None => state.latest_patchset(),
    };
    let comparison_patchset = comparison.map(|patchset| patchset.id.clone());
    let head_matches_comparison_patchset = comparison
        .zip(current_head.as_deref())
        .is_some_and(|(patchset, head)| patchset.head == head);
    // An approval stands on the commit it approved, followed only through
    // rewrites that changed its signature, appended role entries, or both.
    // The patchset's own head follows every rewrite, so gate evidence and the
    // review subject keep describing the branch; only the verdict is left
    // behind by a rewrite that changed anything else.
    let approved_head_matches = match (&current_head, &latest_patchset) {
        (Some(h), Some(p)) => *h == p.approved_head,
        _ => false,
    };
    let base_drift = cwd.and_then(|cwd| {
        state.latest_brief().and_then(|brief| {
            brief_base_drift(cwd, brief.base_revision.as_deref(), current_head.as_deref())
        })
    });

    let review_map = reviewer_coverage(state);
    let eligible_external: Vec<_> = state
        .external_verdicts
        .iter()
        .filter(|verdict| {
            state.privilege_scope.as_ref().is_none_or(|scope| {
                scope.permits(
                    &verdict.event_id,
                    &verdict.origin_replica_id,
                    crate::import_authorization::Role::ExternalDecision,
                )
            })
        })
        .cloned()
        .collect();
    let implementers = reviewed_patchset_implementers(state, policy);
    let mut approval = approval::decide(&ApprovalFacts {
        tiebreak: state
            .latest_verdict()
            .is_some_and(|verdict| state.verdict_tiebreak(verdict).is_some()),
        ladder_clearance: state
            .latest_verdict()
            .and_then(|verdict| state.verdict_ladder_clearance(verdict)),
        latest_patchset: latest_patchset.as_ref(),
        implementers: latest_patchset
            .as_ref()
            .and_then(|patchset| implementers.get(&patchset.id))
            .map_or(&[][..], Vec::as_slice),
        current_head: current_head.as_deref(),
        head_matches,
        approved_head_matches,
        latest_verdict: state.latest_verdict(),
        verdict_tips: state.eligible_verdict_tips().len(),
        external_verdicts: &eligible_external,
        debt_waives_latest_patchset: state.debt_waives_latest_patchset(),
        forbid_self_approval: policy.policy.forbid_self_approval,
        forbid_self_approval_sources: policy
            .sources
            .sources_for("policy.forbid_self_approval=true")
            .join(", "),
        danger: &danger,
        review_map: &review_map,
        provisional_approval_reason: state
            .outstanding_provisional_approval()
            .and_then(|verdict| verdict.provisional.as_deref()),
        reviewed_only_by_brief_author: latest_patchset
            .as_ref()
            .and_then(|patchset| reviewed_only_by_brief_author(state, patchset)),
        debt_outstanding: state.debt_outstanding(),
        review_clearance: policy.policy.review_clearance,
    });
    if !approval.approval_valid && approval.approval_rejection_reason.is_none() {
        let origins: BTreeSet<_> = state
            .verdicts
            .iter()
            .filter(|verdict| !state.review_eligible(&verdict.event_id, &verdict.origin_replica_id))
            .map(|verdict| verdict.origin_replica_id.as_str())
            .collect();
        if !origins.is_empty() {
            approval.approval_rejection_reason = Some(format!(
                "imported verdict origins {origins:?} are ineligible; local verdict owed"
            ));
        }
    }

    let ApprovalOutcome {
        verdict,
        external_verdicts,
        current_external_verdict,
        local_verdict_refuses_this_head,
        external_refuses_this_head,
        approval_valid,
        waiver_satisfies_approval,
        approval_waived_by_debt,
        approval_rejection_reason,
        verdict_contested,
        mut advisories,
    } = approval;
    if !state.is_closed() {
        advisories.extend(missing_changelog_advisory(state, cwd));
    }
    advisories.extend(
        gates
            .retired_bypass_keys()
            .into_iter()
            .map(|detail| Advisory {
                code: "gate-bypass-retired",
                detail,
            }),
    );
    for (event, entry) in state
        .verdicts
        .iter()
        .filter_map(|verdict| {
            state
                .verdict_tiebreak(verdict)
                .map(|entry| (&verdict.event_id, entry))
        })
        .chain(state.audit_verdicts.iter().filter_map(|audit| {
            state
                .audit_tiebreak(audit)
                .map(|entry| (&audit.event_id, entry))
        }))
    {
        advisories.push(Advisory {
            code: "tiebreak-reviewer",
            detail: format!("read {event}: {}", entry.label()),
        });
    }

    // Computed before the report takes ownership of the head it is compared
    // against. A waiver covers exactly the revision it names.
    let dirty_tree_waiver_in_force = state
        .eligible_dirty_tree_waiver()
        .cloned()
        .filter(|waiver| Some(&waiver.revision) == current_head.as_ref());

    let resolution_statuses = evaluate_finding_resolutions(cwd, state, current_head.as_deref());
    let findings: Vec<FindingSummary> = state
        .findings
        .values()
        .map(|f| {
            let resolution_commit_status = resolution_statuses.get(&f.id).cloned();
            let missing_commit = resolution_commit_status
                .as_ref()
                .is_some_and(ResolutionCommitStatus::leaves_open);
            let status = if missing_commit {
                "open".into()
            } else {
                f.effective_status()
                    .map(|s| format!("{s:?}").to_lowercase())
                    .unwrap_or_else(|| {
                        if f.contested() {
                            "contested".into()
                        } else {
                            "open".into()
                        }
                    })
            };
            FindingSummary {
                id: f.id.clone(),
                blocking: f.blocking,
                severity: f.severity,
                summary: f.summary.clone(),
                status,
                contested: f.contested(),
                body: f.body.clone(),
                anchor: f.anchor.clone(),
                patchset_id: f.patchset_id.clone(),
                reported_by: f.reported_by.clone(),
                resolution_commit_status,
            }
        })
        .collect();

    let open_blocking: Vec<String> = state
        .findings
        .values()
        .filter(|f| {
            f.blocks_integration()
                || (f.blocking
                    && resolution_statuses
                        .get(&f.id)
                        .is_some_and(ResolutionCommitStatus::leaves_open))
        })
        .map(|f| f.id.clone())
        .collect();

    // The tree the gates must answer for, named only where it is not the
    // head's own — that is, where a merge would ship content neither branch
    // committed.
    let head_tree = cwd
        .zip(current_head.as_deref())
        .and_then(|(cwd, head)| gitio::commit_tree(cwd, head).ok());
    let evaluated_tree = merged_tree
        .clone()
        .filter(|merged| head_tree.as_ref() != Some(merged));
    // A gate answers for content, so content is the key everywhere it is
    // known: the merge's tree where the change is behind its target, the
    // head's own otherwise. A rebase that moves the base without touching the
    // diff produces a new commit holding a tree some run already answered
    // for, and that answer still describes what ships. Only a head whose tree
    // cannot be read falls back to the revision.
    let lookup_tree = evaluated_tree.clone().or_else(|| head_tree.clone());
    let legacy_trees = match (&lookup_tree, cwd) {
        (Some(_), Some(cwd)) => legacy_evidence_trees(state, cwd),
        _ => BTreeMap::new(),
    };
    let resolve_tree = |revision: &str| legacy_trees.get(revision).cloned();

    let mut gate_statuses: Vec<GateStatus> = Vec::new();
    // A probe is one property of the checkout, so gates declaring the same
    // probe under the same bound share one run. The checkout is resolved at
    // the first probe that needs one, and one this report owns lives until
    // the report is built.
    let mut probe_runs: BTreeMap<(String, Option<u64>), crate::gates::ProbeRun> = BTreeMap::new();
    let mut probe_subject: Option<ProbeSubject> = None;
    let live = matches!(probe_site, ProbeSite::Live { .. });
    for (name, gate) in gates.required_for(&state.profile) {
        let _confinement = crate::confinement::Scope::new(&gate.confine);
        let bound = |entry: &state::VerificationEntry| {
            current_head.as_deref().is_some_and(|head| {
                answers_for_commit(entry, gate, head, commit_destination, &state.rewrites, cwd)
            })
        };
        let evidence_at = |matches: &dyn Fn(&state::VerificationEntry) -> bool| match (
            &lookup_tree,
            current_head.as_deref(),
        ) {
            (Some(tree), _) => state.gate_evidence_at_tree_matching(
                name,
                tree,
                &resolve_tree,
                |entry: &state::VerificationEntry| {
                    crate::state::verification_event(state, entry)
                        == crate::model::GateEvent::Integration
                        && entry
                            .base_pass
                            .as_ref()
                            .is_none_or(|base| base.target == state.target_branch)
                        && bound(entry)
                        && matches(entry)
                },
            ),
            (None, Some(head)) => {
                state.gate_evidence_at_matching(name, head, |entry: &state::VerificationEntry| {
                    crate::state::verification_event(state, entry)
                        == crate::model::GateEvent::Integration
                        && entry
                            .base_pass
                            .as_ref()
                            .is_none_or(|base| base.target == state.target_branch)
                        && matches(entry)
                })
            }
            (None, None) => None,
        };
        let newest = evidence_at(&|_| true);
        let (current, probe_failed, needs_evaluation) = match gate.environment.as_deref() {
            Some(probe) if live && evidence_at(&|e| e.environment.is_some()).is_some() => {
                if probe_subject.is_none() {
                    probe_subject = Some(ProbeSubject::resolve(
                        &probe_site,
                        lookup_tree.as_deref(),
                        state,
                    )?);
                }
                match probe_subject.as_ref().and_then(ProbeSubject::path) {
                    None => (None, None, true),
                    Some(checkout) => {
                        let key = (probe.to_owned(), gate.timeout);
                        if !probe_runs.contains_key(&key) {
                            probe_runs.insert(
                                key.clone(),
                                crate::gates::environment_probe(checkout, probe, gate.timeout)?,
                            );
                        }
                        let run = probe_runs.get(&key).expect("probe run just cached");
                        (
                            run.identity.clone(),
                            run.failure.as_ref().map(|failure| failure.describe()),
                            false,
                        )
                    }
                }
            }
            _ => (None, None, false),
        };
        let evidence = evidence_at(&|e| {
            e.bypass.is_none()
                && matches_declaration(e, gate)
                && if live {
                    matches_environment(e, gate, current.as_deref())
                } else {
                    gate.environment.is_none() || e.environment.is_some()
                }
        })
        .or(newest);
        let bypassed = evidence.is_some_and(|e| e.bypass.is_some());
        // Applicability is asked of a checkout holding the content being
        // evaluated. A ledger replay observes no environment and takes a
        // recorded identity at its word; only a live report can say whether
        // the probe answers for that content.
        let environment = match gate.environment.as_deref() {
            None => None,
            // Nothing ran, so there is no environment to compare.
            Some(_) if bypassed => None,
            Some(_) => {
                let recorded = evidence
                    .and_then(|e| e.environment.as_ref())
                    .map(|environment| environment.identity.clone());
                let inapplicable = current
                    .as_deref()
                    .zip(recorded.as_deref())
                    .is_some_and(|(current, recorded)| current != recorded);
                let unknown = evidence.is_some() && recorded.is_none();
                Some(GateEnvironmentStatus {
                    evidence: recorded,
                    current,
                    inapplicable,
                    unknown,
                    probe_failed,
                    needs_evaluation,
                })
            }
        };
        // A gate that declares an environment probe is answered only by
        // evidence whose recorded identity is the one the probe yields for
        // the evaluated content. A probe that yields none, or that no clean
        // checkout of that content can run, leaves every receipt not-green;
        // a ledger replay counts a recorded identity by its presence.
        let environment_applies = match (&environment, live) {
            (None, _) => true,
            (Some(environment), false) => environment.evidence.is_some(),
            (Some(environment), true) => {
                environment.current.is_some()
                    && environment.evidence.is_some()
                    && !environment.inapplicable
            }
        };
        let result = evidence.map(|e| e.result);
        // Discrimination is asked of the gate at this revision, not of the
        // newest run, so a rerun that appends a plain pass cannot retract
        // what an earlier run established against the same tree.
        let counted_pass = evidence.filter(|e| e.result == crate::model::VerifyResult::Pass);
        let discriminating = counted_pass.and_then(|e| match &lookup_tree {
            Some(tree) => state.gate_falsification_at_tree(name, tree, &resolve_tree),
            None => state.gate_falsification_at(name, &e.revision),
        });
        let budget =
            gate.budget_judgement(evidence.map(|e| &e.timing).unwrap_or(&Default::default()));
        let budget_ok = budget.accepted();
        gate_statuses.push(GateStatus {
            batch_acceptance_missing: evidence.is_some_and(|entry| {
                !crate::gate_concurrency::accepted_here(
                    gate,
                    entry.timing.concurrent_batch.as_ref(),
                )
            }),
            base_pass: evidence.and_then(|entry| entry.base_pass.clone()),
            budget,
            execution_host: evidence.map(|e| e.hostname.clone()),
            name: name.clone(),
            duration_ms: evidence.and_then(|e| e.duration_ms),
            timing: evidence.map(|e| e.timing.clone()).unwrap_or_default(),
            command: gate.command.clone(),
            declared_by: gate.declared_by.clone(),
            evaluated_tree: evaluated_tree.clone(),
            inherited_from: evidence
                .map(|e| {
                    if e.bypass.is_some() {
                        e.recorded_revision.clone()
                    } else {
                        e.revision.clone()
                    }
                })
                .filter(|revision| Some(revision.as_str()) != current_head.as_deref()),
            result: verification_result_label(result),
            // Evidence produced on a dirty tree describes something no
            // checkout of this revision reproduces, and evidence whose
            // tree moved mid-run describes no single tree at all. Both
            // are displayed and neither counts as green — throwing them
            // away would push loops toward not recording at all, which is
            // worse than recording them honestly.
            // Evidence is green for the command it ran, not for the
            // gate's name. A declaration edited after the run describes a
            // different check, and counting the old pass would let a gate
            // nobody has run authorize a merge.
            // The whole declaration, not just the command: a run under a
            // laxer timeout is not evidence for a stricter one, and a run
            // whose declared timeout is unknown cannot be shown to satisfy
            // a declaration that has one.
            green_at_head: evidence.is_some_and(|e| {
                e.green_at_head(state.eligible_dirty_tree_waiver())
                    && matches_declaration(e, gate)
                    && environment_applies
                    && budget_ok
            }),
            bypass: evidence.and_then(|e| e.bypass.clone()),
            declaration_changed: evidence.is_some_and(|e| {
                e.green_at_head(state.eligible_dirty_tree_waiver()) && !matches_declaration(e, gate)
            }),
            attested: evidence.is_some_and(|e| e.attested),
            tested_tree: evidence.and_then(|e| e.tested_tree.clone()),
            worktree_dirty: evidence.and_then(|e| e.worktree_dirty),
            worktree_dirty_tracked: evidence.and_then(|e| e.worktree_dirty_tracked),
            worktree_dirty_untracked: evidence.and_then(|e| e.worktree_dirty_untracked),
            tree_moved: evidence.is_some_and(|e| e.tree_moved),
            evidence_event_id: evidence.map(|e| e.event_id.clone()),
            revision: evidence.map(|e| e.revision.clone()),
            hostname: evidence.map(|e| e.hostname.clone()),
            runner: evidence.and_then(|e| e.runner.clone()),
            environment,
            output_tail: evidence
                .filter(|e| {
                    matches!(
                        e.result,
                        crate::model::VerifyResult::Fail | crate::model::VerifyResult::NotAdmitted
                    )
                })
                .and_then(|e| e.output_tail.clone()),
            output_artifact: evidence.and_then(|e| e.output_artifact.clone()),
            timed_out: evidence
                .is_some_and(|e| e.result == crate::model::VerifyResult::Fail && e.timed_out),
            discrimination: counted_pass.map(|_| match discriminating {
                Some(_) => Discrimination::Discriminating,
                None => Discrimination::Undiscriminated,
            }),
            falsification: discriminating.and_then(|e| e.falsification.clone()),
            discrimination_event_id: discriminating
                .map(|e| e.event_id.clone())
                .filter(|id| Some(id.as_str()) != counted_pass.map(|e| e.event_id.as_str())),
            falsification_inferred: counted_pass.and_then(|e| e.falsification_inferred.clone()),
        });
    }
    let probe_statuses = latest_patchset
        .as_ref()
        .and_then(|patchset| {
            let brief_ref = patchset.brief_ref.as_ref()?;
            let brief_version = patchset.brief_version?;
            let brief = state
                .briefs
                .iter()
                .find(|brief| brief.event_id == brief_ref.event_id)?;
            let baseline_revision = brief.base_revision.as_deref().unwrap_or("");
            Some(
                brief
                    .acceptance_probes
                    .iter()
                    .map(|probe| {
                        let evidence = |phase, revision: &str| {
                            state.verifications.iter().rev().find(|entry| {
                                entry.revision == revision
                                    && entry.probe.as_ref().is_some_and(|evidence| {
                                        evidence.brief_event_id == brief.event_id
                                            && evidence.name == probe.name
                                            && evidence.phase == phase
                                    })
                            })
                        };
                        let baseline = evidence(ProbePhase::Baseline, baseline_revision);
                        let final_evidence = evidence(ProbePhase::Final, &patchset.head);
                        let undischargeable = !probe_base_dischargeable(
                            brief.base_revision.as_deref(),
                            &patchset.head,
                        );
                        ProbeStatus {
                            name: probe.name.clone(),
                            command: probe.command.clone(),
                            brief_version,
                            baseline_revision: baseline_revision.to_owned(),
                            baseline_result: verification_result_label(
                                baseline.map(|entry| entry.result),
                            ),
                            baseline_attested: baseline.is_some_and(|entry| entry.attested),
                            final_revision: patchset.head.clone(),
                            final_result: verification_result_label(
                                final_evidence.map(|entry| entry.result),
                            ),
                            final_attested: final_evidence.is_some_and(|entry| entry.attested),
                            // Fail and Pass recorded at one revision is
                            // contradictory evidence, not a discharged probe,
                            // so an undischargeable probe is never green.
                            discriminating_at_head: !undischargeable
                                && baseline.is_some_and(|entry| entry.result == VerifyResult::Fail)
                                && final_evidence
                                    .is_some_and(|entry| entry.result == VerifyResult::Pass),
                            undischargeable,
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();

    let budget_errors = gates.budget_errors();
    let blockers = blockers::derive(&BlockerFacts {
        invalid_gate_budget: !budget_errors.is_empty(),
        mode_refused: false,
        closed: state.is_closed(),
        branch_missing: current_head.is_none(),
        // A closed change is judged by nothing further, so a target deleted
        // after the fact is not something it still owes.
        target_unreadable: target_unreadable && !state.is_closed(),
        // A fork's branch is where work stays unintegrated on purpose, whoever
        // asks and from wherever they ask it. The refusal reads the change, so
        // standing in the fork's worktree is neither required nor enough.
        on_fork: fork.is_some(),
        iterating: state.iterating,
        blocked_by_changes: dependency_status.blocked,
        landing_pending: state.pending_landing.is_some() && !state.is_closed(),
        lands_with: false,
        reserved_identity: reserved.is_some(),
        needs_rebase,
        merged_tree_evaluated: evaluated_tree.is_some(),
        gates: gate_statuses
            .iter()
            .map(|gate| GateFact {
                green_at_head: gate.green_at_head,
                // A gate that was not admitted ran nothing, so it evaluated
                // nothing either.
                evidence_recorded: gate.evidence_event_id.is_some()
                    && gate.result != VerifyResult::NotAdmitted.as_str(),
            })
            .collect(),
        open_blocking_findings: !open_blocking.is_empty(),
        approval_valid,
        waiver_satisfies_approval,
        probes_discriminating: probe_statuses
            .iter()
            .map(|probe| probe.discriminating_at_head)
            .collect(),
        hold_active: !state.holds.is_empty(),
    });

    let gate_summary = gate_statuses
        .iter()
        .map(|gate| (gate.name.clone(), gate.result.clone()))
        .collect();
    let open_findings = findings
        .iter()
        .filter(|finding| {
            !matches!(
                finding.status.as_str(),
                "resolved" | "acceptedrisk" | "obsolete"
            )
        })
        .count();
    let blocker_summary = BlockerSummary {
        open_findings,
        blocking_findings: open_blocking.len(),
        gate_status: gate_summary,
        hold: HoldSummary {
            active: !state.holds.is_empty(),
            reasons: hold_entries(state),
        },
        approval_reason: approval_rejection_reason.clone(),
    };

    // One approval-satisfied value for every consumer: the existing
    // valid-approval or in-force-waiver predicates, combined exactly as the
    // blocker derivation combines them. A verdict the head cannot use — a
    // changes-requested or comment-only verdict on this patchset — blocks
    // the waiver, and no Approved verdict is fabricated to align displays:
    // a missing verdict remains missing.
    let approval_satisfied = approval_valid || waiver_satisfies_approval;
    let refusing_verdict_action = verdict.as_ref().and_then(|verdict| {
        if !local_verdict_refuses_this_head {
            return None;
        }
        Some(match verdict.verdict {
            Verdict::ChangesRequested => "changes-requested",
            Verdict::CommentOnly => "comment-only",
            Verdict::Approved => unreachable!("approved verdict cannot refuse this head"),
        })
    });

    let next_action =
        if state.is_closed() {
            "none:closed".into()
        } else if let Some(attempt) = &state.pending_landing {
            format!("recover_landing:{attempt}")
        } else if current_head.is_none() {
            "restore_branch".into()
        } else if target_unreadable {
            format!("restore_target:{}", state.target_branch)
        } else if dependency_status
            .blockers_ready
            .iter()
            .any(|dependency| dependency.status == "wedged")
        {
            "repair_blockers:metadata".into()
        } else if dependency_status.blocked {
            "wait_for:blockers".into()
        } else if let Some(reserved) = &reserved {
            format!("reauthor:{}", reserved.commit)
        } else if needs_rebase {
            "rebase".into()
        } else if !head_matches || !approved_head_matches {
            "snapshot".into()
        } else if !open_blocking.is_empty() {
            "resolve_findings".into()
        } else if let Some(hold) = state.holds.values().next() {
            format!("release_hold:{}", hold.hold_event_id)
        } else if !budget_errors.is_empty() {
            "repair_gate_budgets".into()
        } else if let Some(gate) = gate_statuses.iter().find(|gate| !gate.green_at_head) {
            // A gate read at a merged tree is not repaired by running it at the
            // head: that records evidence for content the merge discards. The
            // merge is what has to be evaluated, and re-evaluated once the work
            // that makes it pass lands.
            if evaluated_tree.is_some() {
                format!("verify_against:{}", state.target_branch)
            } else {
                gate.clearing_action(worktree_dirty)
            }
        } else if let Some(probe) = probe_statuses
            .iter()
            .find(|probe| !probe.discriminating_at_head)
        {
            format!("run_probe:{}", probe.name)
        } else if state.iterating {
            // An iterating change owes declared debt rather than a verdict,
            // so it never reaches `request_review`. It reaches this arm only once
            // findings, holds, gates and probes are clear, because those are real
            // work whether or not integration is the goal.
            if state.debt.is_none() && state.latest_patchset().is_some() {
                "declare_debt".into()
            } else {
                "iterating:clear".into()
            }
        } else if external_refuses_this_head {
            match current_external_verdict {
                Some(crate::model::ExternalVerdict::ChangesRequested) => {
                    "external_changes_requested".into()
                }
                Some(crate::model::ExternalVerdict::Rejected) => "external_rejected".into(),
                _ => unreachable!("only non-approving external verdicts refuse this head"),
            }
        } else if let Some(action) = refusing_verdict_action {
            // A current refusal is an action in its own right. Debt defers a
            // missing review; it cannot route around a reviewer who read this
            // patchset and declined it.
            action.into()
        } else if let Some(reason) = approval_rejection_reason.as_ref() {
            // A rejection the waiver does not cover is a real refusal, not a
            // routing question; it keeps the policy's own reason as the action.
            reason.clone()
        } else if state.ready_to_send.as_ref().is_some_and(|ready| {
            head_matches && current_head.as_deref() == Some(ready.head.as_str())
        }) {
            // Everything a merge would need held at this head, and the merge is
            // the receiver's. What is left is sending it and recording the answer.
            "await_receiver".into()
        } else if approval_satisfied {
            // The waiver is already in force and everything above is clear, so
            // steering the caller back into a review queue would contradict the
            // readiness this same report computes. The debt record and the
            // approval_waived_by_debt flag still say what authorized this.
            "integrate".into()
        } else if danger.requires_independent_review(policy) {
            // Policy requires an independent verdict; debt is not an option the
            // lead may take from guidance alone, and unknown danger stays
            // dangerous.
            "request_review".into()
        } else {
            // Ordinary scope with no valid approval: the lead may declare debt
            // with the specific coverage and deferral, or request review. The
            // choice is the caller's; reading this list writes nothing.
            "declare_debt".into()
        };
    // The options list beside the action: an empty list says the action is
    // the only correct one, which is most states. A required-review case
    // names review alone; a satisfied approval, an iterating change, a
    // refusal the waiver does not cover, or any higher-priority work names
    // nothing at all.
    let higher_priority_work = state.is_closed()
        || current_head.is_none()
        || dependency_status.blocked
        || reserved.is_some()
        || needs_rebase
        || !head_matches
        || !approved_head_matches
        || !open_blocking.is_empty()
        || !state.holds.is_empty()
        || !budget_errors.is_empty()
        || gate_statuses.iter().any(|gate| !gate.green_at_head)
        || probe_statuses
            .iter()
            .any(|probe| !probe.discriminating_at_head);
    let review_options: Vec<&'static str> = if higher_priority_work
        || state.iterating
        || approval_satisfied
        || local_verdict_refuses_this_head
        || external_refuses_this_head
        || approval_rejection_reason.is_some()
    {
        Vec::new()
    } else if danger.requires_independent_review(policy) {
        vec!["request_review"]
    } else {
        vec!["declare_debt", "request_review"]
    };

    let forge = crate::forge::build_status(
        &state.forge,
        &state.profile,
        latest_patchset
            .as_ref()
            .map(|patchset| patchset.approved_head.as_str()),
        !state.holds.is_empty(),
    );

    let readiness = Readiness::new(blockers);
    let ready_reason = if readiness.ready() {
        // Ready with no declared gate is not a gate that passed.
        if gate_statuses.is_empty() {
            format!("no gates declared for profile {}", state.profile)
        } else {
            "all integration gates pass".into()
        }
    } else {
        readiness
            .blockers
            .iter()
            .map(|blocker| blocker.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };

    Ok(StatusReport {
        review_context: state.review_context.clone(),
        obsolete_provisional_ancestry: Vec::new(),
        provisional_prerequisites: Vec::new(),
        provisional_staging_destination: None,
        replay_snapshot_required: false,
        reserved_identity: reserved.map(|reserved| reserved.to_string()),
        integration_mode: crate::integration::Mode::Authoritative,
        integration_mode_refusal: None,
        provisional_integrations: crate::provisional::project_obligations_at(state, now),
        unfinished_provisional_integrations: Vec::new(),
        recorded_verdict_tips: state
            .verdict_tips()
            .iter()
            .map(|verdict| verdict.event_id.clone())
            .collect(),
        eligible_verdict_tips: state
            .eligible_verdict_tips()
            .iter()
            .map(|verdict| verdict.event_id.clone())
            .collect(),
        verdict_assessments: verdict_assessments(
            state,
            current_head.as_deref(),
            policy,
            &danger,
            &implementers,
        ),
        schema: STATUS_SCHEMA,
        change_id: state.change_id.clone(),
        slug: state.slug.clone(),
        title: state.title.clone(),
        profile: state.profile.clone(),
        iterating: state.iterating,
        state: if state.is_closed() {
            "closed".into()
        } else {
            "open".into()
        },
        target_branch: state.target_branch.clone(),
        branch: state.branch.clone(),
        base: state.base.clone(),
        fork,
        worktree: state.worktree.clone(),
        slot: state.slot.clone(),
        opened_by: state.opened_by.clone(),
        opened_harness: state.opened_harness.clone(),
        from_fork: state.from_fork.clone(),
        tags: state.tags.clone(),
        blocked_by: state.blocked_by.clone(),
        assigned_to: state.assigned_to.clone(),
        priority: state.priority,
        messages: state.messages.iter().map(MessageStatus::from).collect(),
        blocks,
        blocker_status: dependency_status,
        lands_with: None,
        pending_landing: state.pending_landing.clone(),
        claim: claim_status_at(state, now, provenance_mode),
        current_head,
        observed_target_head: None,
        needs_rebase,
        merged_tree,
        latest_patchset,
        review_subject,
        brief: state.latest_brief().map(|brief| BriefStatus {
            version: state.briefs.len(),
            title: brief.title.clone(),
            base_revision: brief.base_revision.clone(),
            base_drift,
            acceptance_probes: brief.acceptance_probes.clone(),
            plan_ref: brief.plan_ref.clone(),
            plan_slice: brief.plan_slice.clone(),
            plan_source: brief.plan_source.clone(),
            recorded_at: brief.ts,
        }),
        head_matches_latest_patchset: head_matches,
        comparison_patchset,
        head_matches_comparison_patchset,
        worktree_dirty,
        verdict,
        external_verdicts,
        has_valid_approval: approval_valid,
        review_map,
        advisories,
        findings,
        open_blocking_findings: open_blocking,
        holds: hold_entries(state),
        gates: gate_statuses,
        policy_sources: policy.sources.as_map(),
        budget_raises: gates.raises.clone(),
        budget_suspensions: gates.budget_suspensions.clone(),
        budget_errors,
        budget_rules: gates.budget.clone(),
        declaration_notes: Vec::new(),
        probes: probe_statuses,
        blocker_summary,
        approval_rejection_reason,
        verdict_contested,
        approval_waived_by_debt,
        next_action,
        review_options,
        kept: state.kept.clone(),
        danger,
        ready_reason,
        readiness,
        closure: state.closure.clone(),
        ready_to_send: state.ready_to_send.clone(),
        debt_outstanding: state.debt_outstanding(),
        debt_standing: state.debt_standing(),
        provisional_approval_outstanding: state.provisional_approval_outstanding(),
        debt: state.debt.clone(),
        dirty_tree_waiver: dirty_tree_waiver_in_force,
        audit_verdicts: state.audit_verdicts.clone(),
        post_integration_verifications: state.post_integration_verifications.clone(),
        forge,
        forge_view: None,
        provenance_check_enabled,
    })
}

pub fn claim_status_at(
    state: &ChangeState,
    now: DateTime<Utc>,
    provenance_mode: crate::config::GitIdentityMode,
) -> Option<ClaimStatus> {
    let claim = state.claim.as_ref()?;
    let timing = state::claim_timing_at(claim, now);
    let snapshot = state.latest_patchset().filter(|patchset| {
        timing.stage == "snapshotted"
            && patchset.claim_id.as_deref() == Some(claim.claim_id.as_str())
    });
    Some(ClaimStatus {
        claim_id: claim.claim_id.clone(),
        owner: claim.owner.clone(),
        active: timing.active,
        expired: timing.expired,
        stale: timing.stale,
        ttl_seconds: claim.ttl_seconds,
        claimed_at: claim.claimed_at,
        last_activity_at: claim.last_activity_at,
        expires_at: timing.expires_at,
        stage: timing.stage,
        note: claim
            .progress
            .as_ref()
            .and_then(|progress| progress.note.clone()),
        blocker: claim
            .progress
            .as_ref()
            .and_then(|progress| progress.blocker.clone()),
        stage_started_at: timing.stage_started_at,
        age_seconds: timing.age_seconds,
        budget_seconds: timing.budget_seconds,
        stage_budgets: claim.stage_budgets.clone(),
        displaced_reason: claim
            .displaced
            .as_ref()
            .and_then(|displaced| displaced.reason.clone()),
        snapshot_author: snapshot.and_then(|patchset| patchset.author.clone()),
        snapshot_committer: snapshot.and_then(|patchset| patchset.committer.clone()),
        snapshot_claim_actor: snapshot.and_then(|patchset| patchset.claim_actor.clone()),
        provenance_mismatch: snapshot.and_then(|patchset| {
            state::provenance_mismatch(
                provenance_mode,
                patchset.claim_actor.as_deref(),
                patchset.author.as_ref(),
                patchset.committer.as_ref(),
            )
        }),
    })
}

impl StatusReport {
    /// Attach the change's must-land-together component and raise its
    /// blocker when another member is open or closed without integrating.
    pub fn require_lands_with(&mut self, report: Option<crate::commands::land::LandsWithReport>) {
        let blocks = report
            .as_ref()
            .is_some_and(crate::commands::land::LandsWithReport::blocks)
            && !self.readiness.blockers.contains(&Blocker::Closed);
        self.lands_with = report;
        self.readiness
            .blockers
            .retain(|blocker| *blocker != Blocker::LandsWith);
        if !blocks {
            return;
        }
        self.readiness.blockers.push(Blocker::LandsWith);
        self.readiness.blockers.sort_by_key(|blocker| {
            blockers::PRIORITY
                .iter()
                .position(|candidate| candidate == blocker)
        });
        if self.readiness.blockers.first() == Some(&Blocker::LandsWith) {
            let wedged = self
                .lands_with
                .as_ref()
                .is_some_and(|report| !report.wedged_members.is_empty());
            self.next_action = if wedged {
                "withdraw_lands_with".into()
            } else {
                "land_together".into()
            };
        }
        self.ready_reason = self
            .readiness
            .blockers
            .iter()
            .map(|blocker| blocker.as_str())
            .collect::<Vec<_>>()
            .join(", ");
    }

    pub fn require_replay_snapshot(&mut self, required: bool) {
        self.replay_snapshot_required = required;
        if required {
            if !self.readiness.blockers.contains(&Blocker::NoValidApproval) {
                self.readiness.blockers.push(Blocker::NoValidApproval);
            }
            self.readiness.blockers.sort_by_key(|blocker| {
                blockers::PRIORITY
                    .iter()
                    .position(|candidate| candidate == blocker)
            });
            self.next_action = "snapshot_replayed_head".into();
            self.ready_reason = "provisional ancestry substitution requires a new patchset and fresh review or debt".into();
        }
    }
    pub fn require_provisional_replay(&mut self, commits: Vec<String>) {
        self.obsolete_provisional_ancestry = commits;
        if !self.obsolete_provisional_ancestry.is_empty() {
            self.next_action = "replay_provisional_ancestry".into();
            self.ready_reason =
                "publication requires replay onto accepted authority history".into();
        }
        if !self.obsolete_provisional_ancestry.is_empty()
            && !self.readiness.blockers.contains(&Blocker::NeedsRebase)
        {
            self.readiness.blockers.push(Blocker::NeedsRebase);
            self.readiness.blockers.sort_by_key(|blocker| {
                blockers::PRIORITY
                    .iter()
                    .position(|candidate| candidate == blocker)
            });
        }
    }
    pub fn select_mode(&mut self, mode: crate::integration::Mode, refusal: Option<String>) {
        self.integration_mode = mode;
        self.integration_mode_refusal = refusal;
        self.readiness
            .blockers
            .retain(|blocker| *blocker != Blocker::IntegrationMode);
        if let Some(reason) = &self.integration_mode_refusal {
            self.readiness.blockers.insert(0, Blocker::IntegrationMode);
            self.ready_reason = reason.clone();
            if mode == crate::integration::Mode::Authoritative {
                self.next_action = "check_provisional".into();
            }
        }
    }
    /// Every blocker on the change, highest precedence first.
    pub fn blockers(&self) -> &[Blocker] {
        &self.readiness.blockers
    }

    /// True exactly when the change carries no blocker.
    pub fn integrate_ready(&self) -> bool {
        self.readiness.ready()
    }
}

/// Exit code for `arc check`: the code of the highest-precedence blocker, or
/// 0 when there is none, which is what makes the change integrate-ready.
pub fn check_exit_code(report: &StatusReport) -> i32 {
    report.readiness.exit_code()
}

fn verification_result_label(result: Option<VerifyResult>) -> String {
    result.map_or("pending", VerifyResult::as_str).into()
}

/// Walk every verdict and finding, grouping by the identity policy attributes
/// them to, and record the newest patchset each one saw.
///
/// Ordering is by patchset position in the change's own snapshot sequence, not
/// by timestamp: a verdict filed late against an old patchset reviewed the old
/// patchset. Reviewers with no patchset attribution at all are omitted rather
/// than credited with covering something.
pub fn reviewer_coverage(state: &ChangeState) -> Vec<ReviewerCoverage> {
    let position = |patchset_id: &str| {
        state
            .patchsets
            .iter()
            .position(|patchset| patchset.id == patchset_id)
    };
    let final_patchset = state.latest_patchset();

    struct Tally {
        best: usize,
        best_id: String,
        verdicts: usize,
        findings: usize,
        attributed: bool,
        declared: bool,
    }
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let mut record =
        |reviewer: &str, patchset_id: &str, attributed: bool, declared: bool, is_verdict: bool| {
            let Some(index) = position(patchset_id) else {
                return;
            };
            let entry = tallies.entry(reviewer.to_string()).or_insert(Tally {
                best: index,
                best_id: patchset_id.to_string(),
                verdicts: 0,
                findings: 0,
                attributed,
                declared,
            });
            if index >= entry.best {
                entry.best = index;
                entry.best_id = patchset_id.to_string();
            }
            // Any explicit attribution anywhere makes this identity legible, and
            // one event that claimed the name makes it somebody's claim.
            entry.attributed |= attributed;
            entry.declared |= declared;
            if is_verdict {
                entry.verdicts += 1;
            } else {
                entry.findings += 1;
            }
        };

    for verdict in &state.verdicts {
        if !state.review_eligible(&verdict.event_id, &verdict.origin_replica_id) {
            continue;
        }
        record(
            verdict.effective_author(),
            &verdict.patchset_id,
            verdict.on_behalf_of.is_some(),
            !verdict.author_assumed(),
            true,
        );
    }
    for finding in state.findings.values() {
        if let Some(patchset_id) = finding.patchset_id.as_deref() {
            record(
                finding.effective_author(),
                patchset_id,
                finding.on_behalf_of.is_some(),
                !finding.author_assumed(),
                false,
            );
        }
    }

    let mut rows: Vec<ReviewerCoverage> = tallies
        .into_iter()
        .map(|(reviewer, tally)| {
            // Independence is a fact about the patchset this reviewer
            // actually read, not about whatever happens to be newest now.
            // Judging it against the final patchset lets a later snapshot by
            // a different author retroactively make a reviewer independent,
            // or a self-review look independent once someone else pushes.
            let reviewed = state
                .patchsets
                .iter()
                .find(|patchset| patchset.id == tally.best_id);
            let matched_contributor = reviewed
                .and_then(|patchset| patchset.contributor_match(&reviewer))
                .map(str::to_string);
            let is_author = matched_contributor.is_some();
            ReviewerCoverage {
                covers_final: final_patchset.is_some_and(|patchset| patchset.id == tally.best_id),
                // Unplaceable rather than independent, either way round: an
                // identity arc invented names nobody in particular, and one
                // carrying the snapshot actor's name with no subject declared
                // on either side may or may not be that actor.
                attribution_unknown: !tally.declared
                    || (is_author
                        && !tally.attributed
                        && reviewed.is_some_and(|patchset| {
                            patchset.on_behalf_of.is_none() && patchset.contributors.is_empty()
                        })),
                is_author,
                matched_contributor,
                contributors_source: reviewed
                    .map(crate::state::Patchset::contributors_source)
                    .unwrap_or("unknown"),
                reviewer,
                last_patchset: tally.best_id,
                verdicts: tally.verdicts,
                findings: tally.findings,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.covers_final
            .cmp(&a.covers_final)
            .then_with(|| a.reviewer.cmp(&b.reviewer))
    });
    rows
}

/// Whether recorded evidence ran the gate as it is declared now.
pub fn matches_declaration(
    evidence: &crate::state::VerificationEntry,
    gate: &crate::gates::Gate,
) -> bool {
    evidence.declaration.as_ref().map_or_else(
        || gate.confine.is_none() && !gate.coverage.enabled() && !gate.concurrent,
        |identity| identity == &crate::gates::declaration_identity(gate),
    ) && crate::gate_concurrency::accepted_here(gate, evidence.timing.concurrent_batch.as_ref())
        && evidence.command == gate.command
        && evidence.timeout_seconds == gate.timeout
        && evidence.delta.as_ref().is_none_or(|delta| {
            delta.declaration == crate::gates::declaration_identity(gate)
                && ((delta.selector == crate::gates::PATH_SELECTOR && gate.coverage.enabled())
                    || gate.selector.as_deref() == Some(delta.selector.as_str()))
        })
}

/// Destination context against which commit-bound gate evidence is judged.
#[derive(Debug, Clone, Copy)]
pub enum CommitDestination<'a> {
    /// Live open-change check or head verification against the change's target
    /// branch (`None` when the target head cannot be resolved).
    Target(Option<&'a str>),
    /// Explicit synthesized-merge verification (`verify --against <target>`),
    /// pinned to a specific target commit.
    AgainstTarget(&'a str),
    /// Closed change, ledger replay (`--at`), or target-less composed run with
    /// no separate moving target branch.
    None,
}

/// Whether `evidence` answers for `head` under `gate`'s binding.
///
/// A tree-bound gate accepts any evidence matching the evaluated tree. A
/// commit-bound one accepts:
/// - synthesized-merge evidence (`against_target` recorded) whose original
///   first parent equals the recorded and selected destination and whose
///   second (source) parent reaches `head` through `signature_only` and
///   `role_additions` rewrites;
/// - direct head evidence recorded at `head` itself (matching its recorded
///   destination when present, and for `--against`, requiring the recorded
///   destination to equal the selected target and be an ancestor of `head`);
/// - carried direct head evidence whose `approved_revision` matches `head`
///   through `signature_only` and `role_additions` rewrites, provided its
///   recorded destination equals the selected destination and is an ancestor
///   of `head` (conservatively refusing older records lacking destination
///   provenance when a live destination is selected).
pub fn answers_for_commit(
    evidence: &crate::state::VerificationEntry,
    gate: &crate::gates::Gate,
    head: &str,
    destination: CommitDestination<'_>,
    rewrites: &crate::rewrite::RewriteMap,
    cwd: Option<&Path>,
) -> bool {
    if gate.binds == crate::gates::Binding::Tree {
        return true;
    }
    if let Some(original_target) = evidence.recorded_against_target.as_deref() {
        let Some(cwd) = cwd else {
            return false;
        };
        let target_matches = match destination {
            CommitDestination::Target(Some(selected))
            | CommitDestination::AgainstTarget(selected) => selected == original_target,
            CommitDestination::Target(None) => false,
            CommitDestination::None => true,
        };
        if !target_matches {
            return false;
        }
        let Ok(parents) = gitio::commit_parents(cwd, &evidence.recorded_revision) else {
            return false;
        };
        if parents.len() != 2 || parents[0] != original_target {
            return false;
        }
        return rewrites
            .approved_successor(&parents[1])
            .is_ok_and(|approved| approved == head);
    }
    if evidence.recorded_revision == head {
        return match destination {
            CommitDestination::AgainstTarget(selected) => {
                evidence.recorded_target_revision.as_deref() == Some(selected)
                    && cwd
                        .is_some_and(|cwd| gitio::is_ancestor(cwd, selected, head).unwrap_or(false))
            }
            CommitDestination::Target(Some(selected)) => evidence
                .recorded_target_revision
                .as_deref()
                .is_none_or(|recorded| recorded == selected),
            CommitDestination::Target(None) | CommitDestination::None => true,
        };
    }
    if evidence.approved_revision != head {
        return false;
    }
    match destination {
        CommitDestination::Target(Some(selected)) | CommitDestination::AgainstTarget(selected) => {
            evidence.recorded_target_revision.as_deref() == Some(selected)
                && cwd.is_some_and(|cwd| gitio::is_ancestor(cwd, selected, head).unwrap_or(false))
        }
        CommitDestination::Target(None) => false,
        CommitDestination::None => true,
    }
}

/// Whether evidence was produced in the environment a gate declares.
///
/// A gate with no probe accepts evidence from any environment. A gate that
/// declares one accepts only evidence carrying an identity equal to what the
/// probe yields where the caller is deciding; `current` is that identity,
/// `None` where no probe was run.
pub fn matches_environment(
    evidence: &crate::state::VerificationEntry,
    gate: &crate::gates::Gate,
    current: Option<&str>,
) -> bool {
    match gate.environment.as_deref() {
        None => true,
        Some(_) => evidence
            .environment
            .as_ref()
            .is_some_and(|recorded| Some(recorded.identity.as_str()) == current),
    }
}

/// One advisory: something a lead should know before integrating, which is
/// deliberately not a blocker. An orchestrator's review is a valid review
/// unless a project's policy says otherwise, so arc reports the shape of the
/// review and lets the project judge it.
#[derive(Debug, Clone, Serialize)]
pub struct Advisory {
    /// Stable machine-readable kind, so a consumer can act on one advisory
    /// without parsing prose.
    pub code: &'static str,
    pub detail: String,
}

/// Advise a changelog record unless the change has one or the invoking
/// checkout declares that the repository keeps no changelog. An unreadable
/// declaration retains the advice and warns; advice never blocks integration.
pub fn missing_changelog_advisory(state: &ChangeState, cwd: Option<&Path>) -> Option<Advisory> {
    if state.changelog.is_some() {
        return None;
    }
    if cwd.is_some_and(|cwd| crate::commands::advisory_changelog_none(cwd).is_some()) {
        return None;
    }
    Some(Advisory {
        code: "no-changelog-entry",
        detail: format!(
            "no changelog entry on {id}; record one with `arc changelog {slug} --category CATEGORY --body-file FILE`, or that it needs none with `arc changelog {slug} --none --reason TEXT`",
            id = state.change_id,
            slug = state.slug
        ),
    })
}

/// The authoring model coordinates the ledger records for each patchset a
/// verdict names, where policy asks for an independent reviewer. Commit text
/// is not consulted: a range whose ledger records no model names nobody here,
/// and the read is judged by the identities the ledger does hold.
fn reviewed_patchset_implementers(
    state: &ChangeState,
    policy: &PolicyFile,
) -> BTreeMap<String, Vec<String>> {
    if !policy.policy.forbid_self_approval {
        return BTreeMap::new();
    }
    let mut read = BTreeMap::new();
    for patchset in state.patchsets.iter().filter(|patchset| {
        state
            .verdicts
            .iter()
            .any(|verdict| verdict.patchset_id == patchset.id)
    }) {
        let production = state
            .debt
            .as_ref()
            .and_then(|debt| debt.production.as_ref());
        let ledger = state.authoring_model_values(patchset, production);
        if !ledger.is_empty() {
            read.insert(patchset.id.clone(), ledger);
        }
    }
    read
}

/// The brief's author, when every verdict on `final_patchset` came from it.
///
/// Verdicts on the patchset that is shipping — not the review map, whose rows
/// count a reviewer's whole history. A reviewer who approved an earlier
/// patchset and only filed a finding on this one has approved nothing here,
/// and one who filed only findings has approved nothing at all; counting
/// either would answer a question about the wrong window.
fn reviewed_only_by_brief_author<'a>(
    state: &'a ChangeState,
    final_patchset: &state::Patchset,
) -> Option<&'a str> {
    let covering_reviewers = state
        .verdicts
        .iter()
        .filter(|verdict| {
            verdict.patchset_id == final_patchset.id
                && state.review_eligible(&verdict.event_id, &verdict.origin_replica_id)
        })
        .map(|verdict| {
            verdict
                .on_behalf_of
                .as_deref()
                .unwrap_or(verdict.actor.as_str())
        });
    match (
        state.brief_author_for(final_patchset),
        state.reviewed_only_by_brief_author(final_patchset, covering_reviewers),
    ) {
        (Some(author), Some(true)) => Some(author),
        _ => None,
    }
}

/// Advice on the contribution trailers of the commits in `target..head`:
/// projected lines a commit lacks, and role lines whose value is no fresh
/// coordinate. Advice only; neither alters integration authorization, `ready`
/// or the exit code.
pub(crate) fn role_trailer_advisories(
    state: &ChangeState,
    cwd: &Path,
    target: Option<&str>,
    head: Option<&str>,
    policy: &PolicyFile,
    danger: &DangerScope,
) -> Vec<Advisory> {
    let (Some(target), Some(head)) = (target, head) else {
        return Vec::new();
    };
    let unreadable = |detail: String| {
        vec![Advisory {
            code: "contribution-trailers-unreadable",
            detail,
        }]
    };
    let messages = match gitio::range_messages(cwd, target, head) {
        Ok(messages) => messages,
        Err(error) => {
            return unreadable(format!(
                "could not inspect contribution trailers: {error:#}"
            ))
        }
    };
    missing_role_trailers_advisory(state, cwd, head, policy, danger, &messages)
        .into_iter()
        .chain(placeholder_role_trailers_advisory(&messages))
        .collect()
}

/// Projected contribution lines a commit lacks. Only fresh coordinates from
/// `trailers::for_change` (`Planned-by`, `Implemented-by`, `Reviewed-by`,
/// `Orchestrated-by`) are advised: unresolved or placeholder values are notes
/// in `arc trailers`, and no remedy appends a line that names nobody.
fn missing_role_trailers_advisory(
    state: &ChangeState,
    cwd: &Path,
    head: &str,
    policy: &PolicyFile,
    danger: &DangerScope,
    messages: &[(String, Vec<u8>)],
) -> Option<Advisory> {
    let projected = crate::trailers::for_change(
        state,
        &crate::trailers::ReviewProjection {
            current_head: Some(head),
            policy,
            danger,
        },
    );
    let missing = crate::trailers::missing_in_range(&projected.trailers, messages);
    if missing.is_empty() {
        return None;
    }
    let first = messages
        .iter()
        .find(|(commit, _)| missing.iter().any(|(_, commits)| commits.contains(commit)))
        .map(|(commit, _)| commit.as_str())?;
    let details = missing
        .iter()
        .map(|(line, commits)| format!("{line} (missing on {})", commits.join(", ")))
        .collect::<Vec<_>>()
        .join("; ");
    // A bulk `arc rewrite trailers --from <first> --append ...` rewrites every
    // commit returned by `gitio::commits_from(cwd, first, head)` — which on a
    // merge DAG can include sibling commits outside `first`'s ancestry. Offer
    // that alternative only when the exact rewrite set is readable, stays
    // inside `target..head`, and accepts every missing line without violating
    // the single-participant rule.
    let by_commit: BTreeMap<&str, &[u8]> = messages
        .iter()
        .map(|(commit, message)| (commit.as_str(), message.as_slice()))
        .collect();
    let missing_lines: Vec<String> = missing.iter().map(|(line, _)| line.clone()).collect();
    let bulk_safe = gitio::commits_from(cwd, first, head)
        .ok()
        .filter(|range| !range.is_empty())
        .is_some_and(|range| {
            range.iter().all(|commit| {
                by_commit.get(commit.as_str()).is_some_and(|message| {
                    let applicable =
                        crate::trailers::applicable_for_commit(message, &projected.trailers, &[]);
                    let rewritten = crate::trailers::edit(message, &[], &missing_lines);
                    missing_lines.iter().all(|line| applicable.contains(line))
                        && !crate::trailers::introduces_single_participant_issue(
                            message, &rewritten,
                        )
                })
            })
        });
    let remedy = if bulk_safe {
        let append = missing
            .iter()
            .map(|(line, _)| format!(" --append {}", crate::context::shell_quote(line)))
            .collect::<String>();
        format!("`arc rewrite trailers --sync` (or `arc rewrite trailers --from {first}{append}`)")
    } else {
        "`arc rewrite trailers --sync`".to_string()
    };
    Some(Advisory {
        code: "missing-contribution-trailers",
        detail: format!(
            "projected contribution trailers missing: {details}; before integration run \
             {remedy}; appending complete role trailers preserves approvals, debt waivers, \
             and gates"
        ),
    })
}

/// Commit role lines whose value carries a placeholder or a session id, or
/// is a person without an email: what `arc instructions git --check` reports
/// as `placeholder`.
fn placeholder_role_trailers_advisory(messages: &[(String, Vec<u8>)]) -> Option<Advisory> {
    let mut found = Vec::new();
    let mut first = None;
    for (commit, message) in messages {
        for issue in crate::trailers::check_contribution(message) {
            if issue.kind != "placeholder" {
                continue;
            }
            first.get_or_insert(commit.as_str());
            found.push(format!(
                "{commit}: {}: {} ({})",
                issue.key, issue.value, issue.detail
            ));
        }
    }
    let first = first?;
    Some(Advisory {
        code: "placeholder-contribution-trailers",
        detail: format!(
            "commit role trailers carry values that are no coordinate: {}; an unestablished \
             value is omitted, never written: before integration run `arc rewrite trailers \
             --from {first} --edits <file>`, where <file> opens with `# arc-trailer-edits/1` \
             and holds, for each line named above, one line of tab-separated fields: the \
             commit, `drop` and the exact line to omit it, or the commit, `replace`, the \
             exact line and the real coordinate that is known; the other lines under the \
             same key stay; JSON-quote a field to encode an internal tab as \\t; \
             replacing or dropping trailers with --edits requires a fresh snapshot and \
             approval or declared debt",
            found.join("; ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Codes 8, 9 and 10 are refusals that any command can raise — claim or
    /// stage conflict, execution-role refusal, and forge-link refusal — so a
    /// caller reads them the same way whichever verb produced them. Blocker
    /// codes are `check`/`integrate` verdicts and must not shadow those, or a
    /// caller branching on `check` misreads a documented meaning. Codes that
    /// are genuinely per-command, such as `watch`'s timeout, are outside this
    /// rule and deliberately not listed.
    const CROSS_COMMAND_REFUSALS: [i32; 3] = [8, 9, 10];

    #[test]
    fn blocker_exit_codes_are_distinct_and_avoid_cross_command_refusals() {
        let blockers = [
            Blocker::Closed,
            Blocker::BranchMissing,
            Blocker::TargetUnreadable,
            Blocker::Iterating,
            Blocker::BlockedByChanges,
            Blocker::LandingPending,
            Blocker::LandsWith,
            Blocker::ReservedIdentity,
            Blocker::NeedsRebase,
            Blocker::BlockingFindings,
            Blocker::NoValidApproval,
            Blocker::GatesNotGreen,
            Blocker::AcceptanceProbesNotGreen,
            Blocker::HoldActive,
        ];
        let mut seen: BTreeMap<i32, Vec<&'static str>> = BTreeMap::new();
        for blocker in blockers {
            seen.entry(blocker.exit_code())
                .or_default()
                .push(blocker.as_str());
        }
        // Closed, BranchMissing and TargetUnreadable deliberately share 6: each means
        // the change is not in a workable state and a caller acts identically on them.
        for (code, names) in &seen {
            assert!(
                names.len() == 1 || *code == 6,
                "exit {code} is shared by {names:?}; only 6 may be shared"
            );
            assert!(
                !CROSS_COMMAND_REFUSALS.contains(code),
                "blocker {names:?} took exit {code}, which is a cross-command refusal"
            );
        }
        assert_eq!(
            Blocker::AcceptanceProbesNotGreen.exit_code(),
            12,
            "the probe blocker must keep its own code"
        );
    }

    #[test]
    fn readiness_with_any_blocker_is_not_ready_and_exits_non_zero() {
        let clear = Readiness::new(Vec::new());
        assert!(clear.ready());
        assert_eq!(clear.exit_code(), 0);
        for blocker in blockers::PRIORITY {
            let readiness = Readiness::new(vec![blocker]);
            assert!(!readiness.ready(), "{}", blocker.as_str());
            assert_eq!(readiness.exit_code(), blocker.exit_code());
            assert_ne!(readiness.exit_code(), 0, "{}", blocker.as_str());
        }
        let every = Readiness::new(blockers::PRIORITY.to_vec());
        assert!(!every.ready());
        assert_eq!(every.exit_code(), blockers::PRIORITY[0].exit_code());
    }

    #[test]
    fn readiness_serializes_both_spellings_from_the_list() {
        assert_eq!(
            serde_json::to_string(&Readiness::new(Vec::new())).unwrap(),
            r#"{"ready_to_integrate":true,"integrate_ready":true,"blockers":[]}"#
        );
        assert_eq!(
            serde_json::to_string(&Readiness::new(vec![Blocker::HoldActive])).unwrap(),
            r#"{"ready_to_integrate":false,"integrate_ready":false,"blockers":["hold-active"]}"#
        );
    }

    /// The advice names placeholder role lines only: a fresh coordinate is
    /// what a line should carry, and a malformed value is `--check`'s own
    /// report, not a coordinate somebody failed to establish.
    #[test]
    fn placeholder_advice_names_each_commit_and_placeholder_line() {
        let messages = vec![
            (
                "c1".to_string(),
                b"feat: one\n\nImplemented-by: codex:gpt-6.1-sol#xhigh\nPlanned-by: codex\n"
                    .to_vec(),
            ),
            (
                "c2".to_string(),
                b"feat: two\n\nOrchestrated-by: codex:unknown\nImplemented-by: \
                  codex:019f0000-0000-7000-8000-00000000000a\n"
                    .to_vec(),
            ),
        ];
        let advisory = placeholder_role_trailers_advisory(&messages).unwrap();
        assert_eq!(advisory.code, "placeholder-contribution-trailers");
        assert!(advisory
            .detail
            .contains("c2: Orchestrated-by: codex:unknown (a placeholder is not a coordinate)"));
        assert!(advisory.detail.contains(
            "c2: Implemented-by: codex:019f0000-0000-7000-8000-00000000000a (a session id is not a model)"
        ));
        assert!(advisory.detail.contains("--from c2 --edits <file>"));
        assert!(advisory.detail.contains("arc-trailer-edits/1"));
        assert!(!advisory.detail.contains("--drop"), "{}", advisory.detail);
        assert!(!advisory.detail.contains("c1"), "{}", advisory.detail);
        assert!(placeholder_role_trailers_advisory(&messages[..1]).is_none());
    }
}
