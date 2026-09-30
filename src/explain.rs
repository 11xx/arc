//! `arc explain`: what one change knew and what it was accepted on.
//!
//! A read-only projection over the change's ledger and the journal. It
//! appends no event, moves no ref, writes nothing to the journal, and takes no
//! lock. Every row carries a standing no stronger than its source: a value an
//! event records is `recorded`, one somebody stated without arc checking it
//! is `declared`, one arc derived from other records is `inferred`, and an
//! empty slot says why it is `absent` or `unavailable` instead of vanishing.

use crate::commands::{self, Ctx};
use crate::model::{
    AuthorizationBasis, BriefCause, DebtCoverage, DebtMissing, DebtProduction, Event, Payload,
    PlannerIdentity,
};
use crate::state::{ChangeState, Patchset, VerificationEntry, VerificationRunTerminal};
use crate::store::Store;
use anyhow::{bail, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const EXPLAIN_SCHEMA: &str = "arc-explain/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StandingKind {
    Recorded,
    Declared,
    Inferred,
    Absent,
    Unavailable,
}

impl StandingKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Declared => "declared",
            Self::Inferred => "inferred",
            Self::Absent => "absent",
            Self::Unavailable => "unavailable",
        }
    }
}

/// How strongly a row is known. A reason accompanies every standing except
/// `recorded`, so a weaker standing always says what it rests on.
#[derive(Debug, Clone, Serialize)]
pub struct Standing {
    pub standing: StandingKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Standing {
    fn recorded() -> Self {
        Self {
            standing: StandingKind::Recorded,
            reason: None,
        }
    }

    fn with(standing: StandingKind, reason: impl Into<String>) -> Self {
        Self {
            standing,
            reason: Some(reason.into()),
        }
    }

    fn declared(reason: impl Into<String>) -> Self {
        Self::with(StandingKind::Declared, reason)
    }

    fn inferred(reason: impl Into<String>) -> Self {
        Self::with(StandingKind::Inferred, reason)
    }

    fn absent(reason: impl Into<String>) -> Self {
        Self::with(StandingKind::Absent, reason)
    }

    fn unavailable(reason: impl Into<String>) -> Self {
        Self::with(StandingKind::Unavailable, reason)
    }
}

/// One slot of the projection. An empty slot carries its `absence` instead of
/// being left out.
#[derive(Debug, Clone, Serialize)]
pub struct Slot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    pub rows: Vec<Row>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absence: Option<Standing>,
}

impl Slot {
    fn of(rows: Vec<Row>, when_empty: Standing) -> Self {
        let absence = rows.is_empty().then_some(when_empty);
        Self {
            basis: None,
            rows,
            absence,
        }
    }

    fn empty(absence: Standing) -> Self {
        Self::of(Vec::new(), absence)
    }

    fn with_basis(mut self, basis: impl Into<String>) -> Self {
        self.basis = Some(basis.into());
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    #[serde(flatten)]
    pub standing: Standing,
    #[serde(flatten)]
    pub detail: Detail,
}

fn row(standing: Standing, detail: Detail) -> Row {
    Row { standing, detail }
}

/// Whether a journal reference still names the body it named when recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Resolution {
    /// The current body digest equals the recorded one.
    Same,
    /// A body exists under the name and its digest differs.
    Amended,
    /// A body exists under the name; nothing recorded a digest to compare.
    Current,
    /// The name resolves to no body, hot or cold.
    Missing,
    /// The journal could not be read.
    Unavailable,
}

/// Falsification of a gate's counted pass. `inferred` belongs to this domain
/// once a producer records it; nothing here derives one.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "standing", rename_all = "kebab-case")]
pub enum FalsificationRow {
    Declared {
        event_id: String,
        revision: String,
        predicted_reason: String,
    },
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct Reuse {
    pub reuse_event_id: String,
    pub run_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DebtUse {
    /// The integration's authorization names the debt.
    WaiverUsed,
    /// A debt stood on the change at integration and authorized nothing.
    RecordedUnused,
}

/// What a row is about. A row flattens its standing beside this, so no field
/// here may be named `item`, `standing`, or `reason`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "item", rename_all = "kebab-case")]
pub enum Detail {
    Brief {
        event_id: String,
        version: usize,
        body_digest: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    Planner {
        #[serde(skip_serializing_if = "Option::is_none")]
        planner: Option<PlannerIdentity>,
        #[serde(skip_serializing_if = "Option::is_none")]
        planner_status: Option<String>,
    },
    Plan {
        #[serde(skip_serializing_if = "Option::is_none")]
        plan_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        plan_slice: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        plan_sha256: Option<String>,
    },
    BaseRevision {
        #[serde(skip_serializing_if = "Option::is_none")]
        revision: Option<String>,
    },
    Cause {
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<BriefCause>,
    },
    OpeningReference {
        file: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        current_digest: Option<String>,
        resolution: Resolution,
    },
    PatchsetReference {
        patchset_id: String,
        file: String,
        recorded_digest: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        current_digest: Option<String>,
        resolution: Resolution,
    },
    Thread {
        patchset_id: String,
        scheme: String,
        id: String,
    },
    Kept {
        event_id: String,
        kind: String,
        body: String,
        /// `evidence` when the fact names its evidence, `claim` when not.
        basis: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        evidence: Option<String>,
    },
    Gate {
        gate: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        evidence_event_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        revision: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tree: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        environment: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout_seconds: Option<u64>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        reused: Vec<Reuse>,
        #[serde(skip_serializing_if = "Option::is_none")]
        falsification: Option<FalsificationRow>,
    },
    Verdict {
        #[serde(skip_serializing_if = "Option::is_none")]
        event_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        verdict: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        patchset_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reviewer: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provisional: Option<String>,
    },
    ExternalVerdict {
        event_id: String,
        verdict: String,
        revision: String,
        decided_by: String,
        reference: String,
    },
    Debt {
        #[serde(skip_serializing_if = "Option::is_none")]
        event_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        used: Option<DebtUse>,
        #[serde(skip_serializing_if = "Option::is_none")]
        debt_reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        missing: Option<DebtMissing>,
        #[serde(skip_serializing_if = "Option::is_none")]
        coverage: Option<Vec<DebtCoverage>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        production: Option<Box<DebtProduction>>,
        /// The later-knowledge event that discharged this debt after the
        /// integration. The acceptance values above are unchanged by it.
        #[serde(skip_serializing_if = "Option::is_none")]
        discharged_later_by: Option<String>,
    },
    AuditVerdict {
        event_id: String,
        verdict: String,
        revision: String,
        reviewer: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    AuditFinding {
        event_id: String,
        finding_id: String,
        severity: String,
        blocking: bool,
        summary: String,
    },
    AuditDisposition {
        event_id: String,
        finding_id: String,
        status: String,
    },
    DebtDeclared {
        event_id: String,
        debt_reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        missing: Option<DebtMissing>,
    },
    DebtDischarge {
        debt_event_id: String,
        by_event_id: String,
        verdict: String,
        reviewer: String,
        /// `approved`, or `fulfilled, not approved` when the discharging
        /// review did not approve.
        outcome: &'static str,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Explanation {
    pub schema: &'static str,
    pub change_id: String,
    pub slug: String,
    pub title: String,
    /// `open`, `integrated`, `abandoned`, or `superseded`, from the whole
    /// ledger.
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub contract: Slot,
    pub supplied_context: Slot,
    pub declared_facts: Slot,
    pub observed_reads: Slot,
    pub rejected_alternatives: Slot,
    pub evaluation: Slot,
    pub coverage_at_acceptance: Slot,
    pub later_knowledge: Slot,
}

pub fn explain(ctx: &Ctx, reference: &str, at: Option<&str>, json: bool) -> Result<()> {
    let explanation = build(ctx, reference, at)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&explanation)?);
    } else {
        print!("{}", render(&explanation));
    }
    Ok(())
}

fn build(ctx: &Ctx, reference: &str, at: Option<&str>) -> Result<Explanation> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let events = store.load_events(&change_id)?;
    let at_position = match at {
        Some(at) => match events.iter().position(|event| event.event_id == at) {
            Some(position) => Some(position),
            None => bail!("event {at} is not on change {change_id}"),
        },
        None => None,
    };
    let full = store.state(&change_id)?;
    let integration = full
        .closure
        .as_ref()
        .filter(|closure| closure.outcome == crate::model::Closure::Integrated)
        .map(|closure| closure.event_id.clone());
    let outcome = match &full.closure {
        None => "open".to_string(),
        Some(closure) => kebab(&closure.outcome),
    };

    let (subject_state, contract, evaluation, coverage, later) = match &integration {
        Some(integration_id) => {
            let integration_position = events
                .iter()
                .position(|event| &event.event_id == integration_id)
                .expect("the closure event is on the change's own ledger");
            let at_integration = commands::reduce_at(&store, &change_id, integration_id)?;
            let boundary_state = match at {
                Some(at) => commands::reduce_at(&store, &change_id, at)?,
                None => full.clone(),
            };
            let later_events = match at_position {
                Some(position) if position <= integration_position => None,
                Some(position) => Some(&events[integration_position + 1..=position]),
                None => Some(&events[integration_position + 1..]),
            };
            let at_acceptance = discharge(&at_integration, &events).map(|(found, _)| found);
            let discharge = discharge(&boundary_state, &events)
                .filter(|(_, position)| *position > integration_position)
                .map(|(found, _)| found);
            let authorization = full
                .closure
                .as_ref()
                .and_then(|closure| closure.authorization.as_ref());
            let shipped = full
                .closure
                .as_ref()
                .and_then(|closure| closure.source_patchset_id.as_deref())
                .and_then(|id| full.patchsets.iter().find(|patchset| patchset.id == id));
            let later = match later_events {
                None => Slot::empty(Standing::absent(format!(
                    "--at {} is not after the integration event {integration_id}, so no later \
                     knowledge is in view",
                    at.unwrap_or_default()
                ))),
                Some(later_events) => later_knowledge(later_events, discharge.as_ref(), at),
            };
            (
                boundary_state.clone(),
                contract_slot(&full, shipped),
                integrated_evaluation(&full, authorization, integration_id),
                coverage_slot(
                    &at_integration,
                    authorization,
                    integration_id,
                    at_acceptance.as_ref(),
                    discharge.as_ref(),
                ),
                later,
            )
        }
        None => {
            let state = match at {
                Some(at) => commands::reduce_at(&store, &change_id, at)?,
                None => full.clone(),
            };
            let not_accepted = match &full.closure {
                None => "the change is not integrated, so nothing was accepted".to_string(),
                Some(_) => format!("the change was {outcome}, so nothing was accepted"),
            };
            (
                state.clone(),
                contract_slot(&state, state.latest_patchset()),
                open_evaluation(ctx, &store, &state, at)?,
                Slot::empty(Standing::absent(not_accepted.clone())),
                Slot::empty(Standing::absent(format!(
                    "{not_accepted}; nothing is later than an acceptance"
                ))),
            )
        }
    };

    Ok(Explanation {
        schema: EXPLAIN_SCHEMA,
        change_id: full.change_id.clone(),
        slug: full.slug.clone(),
        title: full.title.clone(),
        outcome,
        integration_event_id: integration,
        at: at.map(str::to_string),
        contract,
        supplied_context: supplied_context(ctx, &subject_state),
        declared_facts: declared_facts(&subject_state),
        observed_reads: Slot::empty(Standing::absent("no tool record")),
        rejected_alternatives: rejected_alternatives(&subject_state),
        evaluation,
        coverage_at_acceptance: coverage,
        later_knowledge: later,
    })
}

fn kebab<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => "unknown".to_string(),
    }
}

fn sha256(text: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes())))
}

fn contract_slot(state: &ChangeState, subject: Option<&Patchset>) -> Slot {
    let bound = subject.and_then(|patchset| state.brief_for(patchset));
    let (brief, brief_standing) = match (bound, state.latest_brief()) {
        (Some(brief), _) => (brief, Standing::recorded()),
        (None, Some(latest)) => {
            let standing = match subject {
                Some(patchset) => Standing::inferred(format!(
                    "patchset {} names no brief; the latest brief is shown",
                    patchset.id
                )),
                None => Standing::recorded(),
            };
            (latest, standing)
        }
        (None, None) => return Slot::empty(Standing::absent("no brief recorded")),
    };
    let version = state
        .briefs
        .iter()
        .position(|candidate| candidate.event_id == brief.event_id)
        .map_or(0, |index| index + 1);
    let mut rows = vec![row(
        brief_standing,
        Detail::Brief {
            event_id: brief.event_id.clone(),
            version,
            body_digest: sha256(&brief.body),
            title: brief.title.clone(),
        },
    )];

    match &brief.plan_source {
        Some(source) if source.planners.is_empty() => rows.push(row(
            Standing::absent(format!(
                "the plan source names no planner (planner status {})",
                source.planner_status
            )),
            Detail::Planner {
                planner: None,
                planner_status: Some(source.planner_status.clone()),
            },
        )),
        Some(source) => {
            for planner in &source.planners {
                rows.push(row(
                    Standing::declared(format!(
                        "planner coordinates the plan header states (planner status {})",
                        source.planner_status
                    )),
                    Detail::Planner {
                        planner: Some(planner.clone()),
                        planner_status: Some(source.planner_status.clone()),
                    },
                ));
            }
        }
        None => rows.push(row(
            Standing::absent("the brief carries no plan source"),
            Detail::Planner {
                planner: None,
                planner_status: None,
            },
        )),
    }

    let plan = Detail::Plan {
        plan_ref: brief.plan_ref.clone(),
        plan_slice: brief.plan_slice.clone(),
        plan_sha256: brief
            .plan_source
            .as_ref()
            .map(|source| source.sha256.clone()),
    };
    rows.push(match &brief.plan_ref {
        Some(_) => row(Standing::recorded(), plan),
        None => row(Standing::absent("the brief names no plan"), plan),
    });

    let base = Detail::BaseRevision {
        revision: brief.base_revision.clone(),
    };
    rows.push(match &brief.base_revision {
        Some(_) => row(Standing::recorded(), base),
        None => row(Standing::absent("the brief records no base revision"), base),
    });

    if brief.caused_by.is_empty() {
        rows.push(row(
            Standing::absent("the brief names no cause"),
            Detail::Cause { cause: None },
        ));
    }
    for cause in &brief.caused_by {
        let standing = match cause {
            BriefCause::External { .. } => {
                Standing::declared("an external cause is a note arc cannot check")
            }
            _ => Standing::recorded(),
        };
        rows.push(row(
            standing,
            Detail::Cause {
                cause: Some(cause.clone()),
            },
        ));
    }
    Slot::of(rows, Standing::absent("no brief recorded"))
}

/// The body digest a name resolves to now, compared the way `snapshot
/// --journal-ref` computed the recorded one.
fn resolve_reference(ctx: &Ctx, file: &str) -> std::result::Result<String, (Resolution, String)> {
    match crate::journal::artifact_digest(ctx, file) {
        Ok(digest) => Ok(digest),
        Err(error) => {
            let hot = match crate::journal::resolve_dir(&ctx.cwd) {
                Ok(hot) => hot,
                Err(resolve) => {
                    return Err((
                        Resolution::Unavailable,
                        format!("the journal cannot be resolved: {resolve:#}"),
                    ))
                }
            };
            let cold = crate::journal::archive_dir(&hot);
            let exists = |path: std::path::PathBuf| path.try_exists().unwrap_or(true);
            if !exists(hot.join(file)) && !exists(cold.join(file)) {
                Err((
                    Resolution::Missing,
                    format!("{file} resolves to nothing in the journal or its cold archive"),
                ))
            } else {
                Err((
                    Resolution::Unavailable,
                    format!("{file} cannot be read: {error:#}"),
                ))
            }
        }
    }
}

fn supplied_context(ctx: &Ctx, state: &ChangeState) -> Slot {
    let mut rows = Vec::new();
    if let Some(file) = &state.journal_ref {
        let (standing, current_digest, resolution) = match resolve_reference(ctx, file) {
            Ok(digest) => (
                Standing::declared(
                    "the change was opened from this artifact and no digest was recorded; the \
                     digest shown is the body's now",
                ),
                Some(digest),
                Resolution::Current,
            ),
            Err((resolution, why)) => (
                Standing::declared(format!(
                    "the change was opened from this artifact and no digest was recorded; {why}"
                )),
                None,
                resolution,
            ),
        };
        rows.push(row(
            standing,
            Detail::OpeningReference {
                file: file.clone(),
                current_digest,
                resolution,
            },
        ));
    }
    for patchset in &state.patchsets {
        for reference in &patchset.journal_refs {
            let (standing, current_digest, resolution) =
                match resolve_reference(ctx, &reference.file) {
                    Ok(digest) if digest == reference.digest => {
                        (Standing::recorded(), Some(digest), Resolution::Same)
                    }
                    Ok(digest) => (Standing::recorded(), Some(digest), Resolution::Amended),
                    Err((Resolution::Missing, _)) => {
                        (Standing::recorded(), None, Resolution::Missing)
                    }
                    Err((resolution, why)) => (Standing::unavailable(why), None, resolution),
                };
            rows.push(row(
                standing,
                Detail::PatchsetReference {
                    patchset_id: patchset.id.clone(),
                    file: reference.file.clone(),
                    recorded_digest: reference.digest.clone(),
                    current_digest,
                    resolution,
                },
            ));
        }
        if let Some(thread) = &patchset.thread {
            rows.push(row(
                Standing::recorded(),
                Detail::Thread {
                    patchset_id: patchset.id.clone(),
                    scheme: thread.scheme.clone(),
                    id: thread.id.clone(),
                },
            ));
        }
    }
    Slot::of(
        rows,
        Standing::absent("no journal reference or thread was recorded on the change"),
    )
}

fn kept_row(kept: &crate::state::KeptContext) -> Row {
    let reason = match &kept.evidence {
        Some(_) => "kept by a session with the evidence it names; arc did not check it",
        None => "kept by a session as a claim, with no evidence named",
    };
    row(
        Standing::declared(reason),
        Detail::Kept {
            event_id: kept.event_id.clone(),
            kind: kept.kind.as_str().to_string(),
            body: kept.body.clone(),
            basis: if kept.evidence.is_some() {
                "evidence"
            } else {
                "claim"
            },
            evidence: kept.evidence.clone(),
        },
    )
}

fn declared_facts(state: &ChangeState) -> Slot {
    let mut kept: Vec<_> = state.kept.iter().collect();
    kept.sort_by_key(|kept| kept.kind.as_str());
    Slot::of(
        kept.into_iter().map(kept_row).collect(),
        Standing::absent("no fact was kept on the change"),
    )
}

fn rejected_alternatives(state: &ChangeState) -> Slot {
    Slot::of(
        state
            .kept
            .iter()
            .filter(|kept| kept.kind == crate::model::KeptKind::Rejected)
            .map(kept_row)
            .collect(),
        Standing::absent("no rejected alternative was kept on the change"),
    )
}

fn gate_row(
    state: &ChangeState,
    gate: &str,
    evidence_event_id: &str,
    declared_timeout: Option<u64>,
) -> Row {
    let Some(entry) = state
        .verifications
        .iter()
        .find(|entry| entry.event_id == evidence_event_id)
    else {
        return row(
            Standing::unavailable(format!(
                "evidence {evidence_event_id} is not on this change's ledger"
            )),
            Detail::Gate {
                gate: gate.to_string(),
                evidence_event_id: Some(evidence_event_id.to_string()),
                result: None,
                revision: None,
                tree: None,
                environment: None,
                timeout_seconds: declared_timeout,
                reused: Vec::new(),
                falsification: None,
            },
        );
    };
    row(Standing::recorded(), gate_detail(state, gate, entry))
}

fn gate_detail(state: &ChangeState, gate: &str, entry: &VerificationEntry) -> Detail {
    let reused = state
        .verification_runs
        .iter()
        .flat_map(|run| {
            run.terminals
                .iter()
                .filter_map(move |terminal| match terminal {
                    VerificationRunTerminal::Reused {
                        gate: reused_gate,
                        evidence_event_id,
                        reuse_event_id,
                    } if reused_gate == gate && evidence_event_id == &entry.event_id => {
                        Some(Reuse {
                            reuse_event_id: reuse_event_id.clone(),
                            run_id: run.run_id.clone(),
                        })
                    }
                    _ => None,
                })
        })
        .collect();
    Detail::Gate {
        gate: gate.to_string(),
        evidence_event_id: Some(entry.event_id.clone()),
        result: Some(kebab(&entry.result)),
        revision: Some(entry.revision.clone()),
        tree: entry.tree.clone().or_else(|| entry.tested_tree.clone()),
        environment: entry
            .environment
            .as_ref()
            .map(|environment| environment.identity.clone()),
        timeout_seconds: entry.timeout_seconds,
        reused,
        falsification: Some(match &entry.falsification {
            Some(falsification) => FalsificationRow::Declared {
                event_id: falsification.event_id.clone(),
                revision: falsification.revision.clone(),
                predicted_reason: falsification.predicted_reason.clone(),
            },
            None => FalsificationRow::None,
        }),
    }
}

fn integrated_evaluation(
    state: &ChangeState,
    authorization: Option<&AuthorizationBasis>,
    integration_id: &str,
) -> Slot {
    let Some(authorization) = authorization else {
        return Slot::empty(Standing::unavailable(no_basis(state)));
    };
    let rows = authorization
        .gate_evidence
        .iter()
        .map(|(gate, evidence)| {
            let timeout = authorization
                .gates
                .get(gate)
                .and_then(|declared| declared.timeout);
            gate_row(state, gate, evidence, timeout)
        })
        .collect();
    Slot::of(
        rows,
        Standing::absent("the integration's authorization required no gate"),
    )
    .with_basis(format!(
        "gates the authorization on {integration_id} counted"
    ))
}

fn open_evaluation(
    ctx: &Ctx,
    store: &Store,
    state: &ChangeState,
    at: Option<&str>,
) -> Result<Slot> {
    let Some(patchset) = state.latest_patchset() else {
        return Ok(Slot::empty(Standing::absent(
            "no patchset recorded, so no tree to evaluate",
        )));
    };
    let report = ctx.report_as_of(store, state)?;
    let rows = report
        .gates
        .iter()
        .map(|gate| match gate.evidence_event_id.as_deref() {
            Some(evidence) => gate_row(state, &gate.name, evidence, None),
            None => row(
                Standing::absent(format!(
                    "no evidence for gate {} at patchset {}",
                    gate.name, patchset.id
                )),
                Detail::Gate {
                    gate: gate.name.clone(),
                    evidence_event_id: None,
                    result: None,
                    revision: None,
                    tree: None,
                    environment: None,
                    timeout_seconds: None,
                    reused: Vec::new(),
                    falsification: None,
                },
            ),
        })
        .collect();
    let basis = match at {
        Some(at) => format!(
            "gates required at patchset {} as of {at}, replayed from the ledger",
            patchset.id
        ),
        None => format!(
            "gates required at patchset {}, replayed from the ledger",
            patchset.id
        ),
    };
    Ok(Slot::of(rows, Standing::absent("no gate is declared")).with_basis(basis))
}

fn no_basis(state: &ChangeState) -> String {
    match state
        .closure
        .as_ref()
        .and_then(|closure| closure.integration)
    {
        Some(crate::state::IntegrationKind::Asserted) => {
            "the integration was asserted, not performed by arc, so no authorization basis was \
             recorded"
                .to_string()
        }
        _ => "the integration event recorded no authorization basis".to_string(),
    }
}

/// A debt discharge arc derives from a review. No event records one.
struct Discharge {
    debt_event_id: String,
    by_event_id: String,
    verdict: crate::model::Verdict,
    reviewer: String,
}

/// Name the review behind `debt.discharged_by`, with its position on the
/// ledger. The state already decided the discharge; this finds the event whose
/// identity it copied, audits before verdicts, the order the discharge rule
/// reads them in.
fn discharge(state: &ChangeState, events: &[Event]) -> Option<(Discharge, usize)> {
    let debt = state.debt.as_ref()?;
    let coverage = debt.discharged_by.as_ref()?;
    let (by_event_id, verdict) = state
        .audit_verdicts
        .iter()
        .find(|audit| {
            audit.created_at >= debt.declared_at
                && audit.effective_author() == coverage.reviewer
                && audit.model == coverage.model
        })
        .map(|audit| (audit.event_id.clone(), audit.verdict))
        .or_else(|| {
            state
                .verdicts
                .iter()
                .find(|verdict| {
                    verdict.created_at >= debt.declared_at
                        && verdict.effective_author() == coverage.reviewer
                        && verdict.model == coverage.model
                })
                .map(|verdict| (verdict.event_id.clone(), verdict.verdict))
        })?;
    let position = events
        .iter()
        .position(|event| event.event_id == by_event_id)?;
    Some((
        Discharge {
            debt_event_id: debt.event_id.clone(),
            by_event_id,
            verdict,
            reviewer: coverage.reviewer.clone(),
        },
        position,
    ))
}

fn coverage_slot(
    at_integration: &ChangeState,
    authorization: Option<&AuthorizationBasis>,
    integration_id: &str,
    at_acceptance: Option<&Discharge>,
    later: Option<&Discharge>,
) -> Slot {
    let Some(authorization) = authorization else {
        return Slot::empty(Standing::unavailable(no_basis(at_integration)));
    };
    let mut rows = Vec::new();
    match &authorization.verdict_event_id {
        Some(event_id) => {
            let verdict = at_integration
                .verdicts
                .iter()
                .find(|verdict| &verdict.event_id == event_id);
            rows.push(match verdict {
                Some(verdict) => row(
                    Standing::recorded(),
                    Detail::Verdict {
                        event_id: Some(event_id.clone()),
                        verdict: Some(kebab(&verdict.verdict)),
                        patchset_id: Some(verdict.patchset_id.clone()),
                        reviewer: Some(verdict.effective_author().to_string()),
                        model: verdict.model.clone(),
                        provisional: authorization.verdict_provisional.clone(),
                    },
                ),
                None => row(
                    Standing::unavailable(format!(
                        "verdict {event_id} is not on this change's ledger"
                    )),
                    Detail::Verdict {
                        event_id: Some(event_id.clone()),
                        verdict: None,
                        patchset_id: None,
                        reviewer: None,
                        model: None,
                        provisional: authorization.verdict_provisional.clone(),
                    },
                ),
            });
        }
        None if authorization.external_verdict.is_none() => rows.push(row(
            Standing::absent("no verdict authorized the integration"),
            Detail::Verdict {
                event_id: None,
                verdict: None,
                patchset_id: None,
                reviewer: None,
                model: None,
                provisional: None,
            },
        )),
        None => {}
    }
    if let Some(external) = &authorization.external_verdict {
        rows.push(row(
            Standing::recorded(),
            Detail::ExternalVerdict {
                event_id: external.event_id.clone(),
                verdict: kebab(&external.verdict),
                revision: external.revision.clone(),
                decided_by: external.decided_by.clone(),
                reference: external.reference.clone(),
            },
        ));
    }
    let debt = match (&authorization.audit_debt_event_id, &at_integration.debt) {
        (Some(event_id), Some(debt)) if &debt.event_id == event_id => {
            Some((DebtUse::WaiverUsed, Some(debt)))
        }
        (Some(_), _) => Some((DebtUse::WaiverUsed, None)),
        (None, Some(debt)) => Some((DebtUse::RecordedUnused, Some(debt))),
        (None, None) => None,
    };
    match debt {
        Some((used, Some(debt))) => {
            let discharged_later_by = later
                .filter(|discharge| discharge.debt_event_id == debt.event_id)
                .map(|discharge| discharge.by_event_id.clone());
            rows.push(row(
                Standing::recorded(),
                Detail::Debt {
                    event_id: Some(debt.event_id.clone()),
                    used: Some(used),
                    debt_reason: Some(debt.reason.clone()),
                    missing: debt.missing,
                    coverage: debt.coverage.clone(),
                    production: debt.production.clone().map(Box::new),
                    discharged_later_by,
                },
            ));
            if let Some(discharge) =
                at_acceptance.filter(|discharge| discharge.debt_event_id == debt.event_id)
            {
                rows.push(discharge_row(discharge));
            }
        }
        Some((used, None)) => rows.push(row(
            Standing::unavailable(
                "the authorization names a debt the replayed state does not hold",
            ),
            Detail::Debt {
                event_id: authorization.audit_debt_event_id.clone(),
                used: Some(used),
                debt_reason: None,
                missing: None,
                coverage: None,
                production: None,
                discharged_later_by: None,
            },
        )),
        None => rows.push(row(
            Standing::absent("no debt was declared by the integration"),
            Detail::Debt {
                event_id: None,
                used: None,
                debt_reason: None,
                missing: None,
                coverage: None,
                production: None,
                discharged_later_by: None,
            },
        )),
    }
    Slot::of(rows, Standing::absent("the authorization names nothing"))
        .with_basis(format!("as of the integration event {integration_id}"))
}

fn later_knowledge(events: &[Event], discharge: Option<&Discharge>, at: Option<&str>) -> Slot {
    let mut rows = Vec::new();
    for event in events {
        let reviewer = event
            .on_behalf_of
            .clone()
            .unwrap_or_else(|| event.actor.clone());
        match &event.payload {
            Payload::AuditVerdictRecorded {
                revision,
                verdict,
                findings,
                ..
            } => {
                rows.push(row(
                    Standing::recorded(),
                    Detail::AuditVerdict {
                        event_id: event.event_id.clone(),
                        verdict: kebab(verdict),
                        revision: revision.clone(),
                        reviewer,
                        model: event.model.clone(),
                    },
                ));
                for finding in findings {
                    rows.push(row(
                        Standing::recorded(),
                        Detail::AuditFinding {
                            event_id: event.event_id.clone(),
                            finding_id: finding.finding_id.clone(),
                            severity: kebab(&finding.severity),
                            blocking: finding.blocking,
                            summary: finding.summary.clone(),
                        },
                    ));
                }
                if let Some(discharge) =
                    discharge.filter(|discharge| discharge.by_event_id == event.event_id)
                {
                    rows.push(discharge_row(discharge));
                }
            }
            Payload::AuditFindingAdded {
                finding_id,
                blocking,
                severity,
                summary,
                ..
            } => rows.push(row(
                Standing::recorded(),
                Detail::AuditFinding {
                    event_id: event.event_id.clone(),
                    finding_id: finding_id.clone(),
                    severity: kebab(severity),
                    blocking: *blocking,
                    summary: summary.clone(),
                },
            )),
            Payload::AuditDispositionRecorded {
                finding_id, status, ..
            } => rows.push(row(
                Standing::recorded(),
                Detail::AuditDisposition {
                    event_id: event.event_id.clone(),
                    finding_id: finding_id.clone(),
                    status: kebab(status),
                },
            )),
            Payload::DebtDeclared {
                reason, missing, ..
            } => rows.push(row(
                Standing::recorded(),
                Detail::DebtDeclared {
                    event_id: event.event_id.clone(),
                    debt_reason: reason.clone(),
                    missing: Some(*missing),
                },
            )),
            Payload::AuditDebtDeclared { reason, .. } => rows.push(row(
                Standing::recorded(),
                Detail::DebtDeclared {
                    event_id: event.event_id.clone(),
                    debt_reason: reason.clone(),
                    missing: None,
                },
            )),
            _ => {}
        }
    }
    let absence = match at {
        Some(at) => format!("nothing was recorded after the integration by {at}"),
        None => "nothing was recorded after the integration".to_string(),
    };
    Slot::of(rows, Standing::absent(absence))
}

fn discharge_row(discharge: &Discharge) -> Row {
    row(
        Standing::inferred(format!(
            "no event records a discharge; arc's discharge rule reads one from {}",
            discharge.by_event_id
        )),
        Detail::DebtDischarge {
            debt_event_id: discharge.debt_event_id.clone(),
            by_event_id: discharge.by_event_id.clone(),
            verdict: kebab(&discharge.verdict),
            reviewer: discharge.reviewer.clone(),
            outcome: if discharge.verdict == crate::model::Verdict::Approved {
                "approved"
            } else {
                "fulfilled, not approved"
            },
        },
    )
}

fn render(explanation: &Explanation) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# {} ({}): {}",
        explanation.slug, explanation.change_id, explanation.title
    );
    let _ = writeln!(out);
    match &explanation.integration_event_id {
        Some(event) => {
            let _ = writeln!(out, "- Outcome: integrated at `{event}`");
        }
        None => {
            let _ = writeln!(out, "- Outcome: {}", explanation.outcome);
        }
    }
    if let Some(at) = &explanation.at {
        let _ = writeln!(out, "- As of: `{at}`");
    }
    for (title, slot) in [
        ("Contract", &explanation.contract),
        ("Supplied context", &explanation.supplied_context),
        ("Declared facts", &explanation.declared_facts),
        ("Observed reads", &explanation.observed_reads),
        ("Rejected alternatives", &explanation.rejected_alternatives),
        ("Evaluation", &explanation.evaluation),
        (
            "Coverage at acceptance",
            &explanation.coverage_at_acceptance,
        ),
        ("Later knowledge", &explanation.later_knowledge),
    ] {
        let _ = writeln!(out, "\n## {title}\n");
        if let Some(basis) = &slot.basis {
            let _ = writeln!(out, "Basis: {basis}\n");
        }
        for row in &slot.rows {
            let _ = writeln!(
                out,
                "- {}{}",
                standing_label(&row.standing),
                describe(&row.detail)
            );
        }
        if let Some(absence) = &slot.absence {
            let _ = writeln!(out, "- {}", standing_label(absence).trim_end());
        }
    }
    out
}

fn standing_label(standing: &Standing) -> String {
    match &standing.reason {
        Some(reason) => format!("[{}: {reason}] ", standing.standing.as_str()),
        None => format!("[{}] ", standing.standing.as_str()),
    }
}

fn opt(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("none")
}

fn describe(detail: &Detail) -> String {
    match detail {
        Detail::Brief {
            event_id,
            version,
            body_digest,
            title,
        } => format!(
            "brief v{version} `{event_id}`{}, body {body_digest}",
            title
                .as_deref()
                .map(|title| format!(" \"{title}\""))
                .unwrap_or_default()
        ),
        Detail::Planner {
            planner,
            planner_status,
        } => match planner {
            Some(planner) => format!(
                "planner {} (harness {}, model {}), plan status {}",
                opt(&planner.actor),
                opt(&planner.harness),
                opt(&planner.model),
                opt(planner_status)
            ),
            None => "planner".to_string(),
        },
        Detail::Plan {
            plan_ref,
            plan_slice,
            plan_sha256,
        } => match plan_ref {
            Some(plan_ref) => format!(
                "plan {plan_ref} slice {} (plan sha256 {})",
                opt(plan_slice),
                plan_sha256.as_deref().unwrap_or("unrecorded")
            ),
            None => "plan".to_string(),
        },
        Detail::BaseRevision { revision } => format!("base revision {}", opt(revision)),
        Detail::Cause { cause } => match cause {
            Some(BriefCause::Finding { finding_id }) => format!("caused by finding {finding_id}"),
            Some(BriefCause::Verdict { event_id }) => format!("caused by verdict {event_id}"),
            Some(BriefCause::BlockedOnStage { event_id }) => {
                format!("caused by blocked-on stage {event_id}")
            }
            Some(BriefCause::External { summary }) => format!("caused by: {summary}"),
            None => "cause".to_string(),
        },
        Detail::OpeningReference {
            file,
            current_digest,
            resolution,
        } => format!(
            "opened from {file}: {}, current digest {}",
            kebab(resolution),
            opt(current_digest)
        ),
        Detail::PatchsetReference {
            patchset_id,
            file,
            recorded_digest,
            current_digest,
            resolution,
        } => match resolution {
            Resolution::Same => {
                format!("{patchset_id} journal ref {file}: same, {recorded_digest}")
            }
            Resolution::Amended => format!(
                "{patchset_id} journal ref {file}: amended, recorded {recorded_digest}, current {}",
                opt(current_digest)
            ),
            other => format!(
                "{patchset_id} journal ref {file}: {}, recorded {recorded_digest}",
                kebab(other)
            ),
        },
        Detail::Thread {
            patchset_id,
            scheme,
            id,
        } => format!("{patchset_id} thread {scheme}:{id}"),
        Detail::Kept {
            event_id,
            kind,
            body,
            basis,
            evidence,
        } => {
            let first = body.lines().next().unwrap_or_default();
            match evidence {
                Some(evidence) => {
                    format!("{kind} `{event_id}`: {first} (evidence: {evidence})")
                }
                None => format!("{kind} `{event_id}`: {first} ({basis})"),
            }
        }
        Detail::Gate {
            gate,
            evidence_event_id,
            result,
            revision,
            tree,
            environment,
            timeout_seconds,
            reused,
            falsification,
        } => {
            let Some(evidence) = evidence_event_id else {
                return format!("gate {gate}");
            };
            let mut text = format!(
                "gate {gate}: {} by `{evidence}` at {}, tree {}, environment {}, timeout {}",
                opt(result),
                opt(revision),
                tree.as_deref().unwrap_or("unrecorded"),
                environment.as_deref().unwrap_or("unrecorded"),
                timeout_seconds
                    .map(|seconds| format!("{seconds}s"))
                    .unwrap_or_else(|| "unrecorded".to_string())
            );
            for reuse in reused {
                let _ = std::fmt::Write::write_fmt(
                    &mut text,
                    format_args!(
                        ", reused from `{evidence}` by `{}` (run {})",
                        reuse.reuse_event_id, reuse.run_id
                    ),
                );
            }
            match falsification {
                Some(FalsificationRow::Declared {
                    event_id,
                    predicted_reason,
                    ..
                }) => text.push_str(&format!(
                    ", falsification declared: {predicted_reason} (failing `{event_id}`)"
                )),
                Some(FalsificationRow::None) => text.push_str(", falsification none"),
                None => {}
            }
            text
        }
        Detail::Verdict {
            event_id,
            verdict,
            patchset_id,
            reviewer,
            model,
            provisional,
        } => match event_id {
            Some(event_id) => format!(
                "verdict {} `{event_id}` on {} by {}{}{}",
                opt(verdict),
                opt(patchset_id),
                opt(reviewer),
                model
                    .as_deref()
                    .map(|model| format!(" ({model})"))
                    .unwrap_or_default(),
                provisional
                    .as_deref()
                    .map(|why| format!(", provisional: {why}"))
                    .unwrap_or_default()
            ),
            None => "verdict".to_string(),
        },
        Detail::ExternalVerdict {
            event_id,
            verdict,
            revision,
            decided_by,
            reference,
        } => format!(
            "external verdict {verdict} `{event_id}` at {revision} by {decided_by} ({reference})"
        ),
        Detail::Debt {
            event_id,
            used,
            debt_reason,
            missing,
            coverage,
            production,
            discharged_later_by,
        } => {
            let Some(event_id) = event_id else {
                return "debt".to_string();
            };
            let used = match used {
                Some(DebtUse::WaiverUsed) => "waiver used",
                Some(DebtUse::RecordedUnused) => "debt recorded, unused",
                None => "debt",
            };
            let mut text = format!("{used} `{event_id}`: {}", opt(debt_reason));
            text.push_str(&format!(
                "; missing {}",
                missing
                    .map(|missing| missing.as_str().to_string())
                    .unwrap_or_else(|| "not recorded (legacy debt)".to_string())
            ));
            text.push_str(&format!(
                "; coverage {}",
                match coverage {
                    None => "not recorded (legacy debt)".to_string(),
                    Some(coverage) if coverage.is_empty() => "none".to_string(),
                    Some(coverage) => coverage
                        .iter()
                        .map(coverage_label)
                        .collect::<Vec<_>>()
                        .join(", "),
                }
            ));
            text.push_str(&format!(
                "; production {}",
                production
                    .as_ref()
                    .map(|production| production_label(production))
                    .unwrap_or_else(|| "not recorded".to_string())
            ));
            if let Some(later) = discharged_later_by {
                text.push_str(&format!(
                    "; discharged later by `{later}`, see later knowledge"
                ));
            }
            text
        }
        Detail::AuditVerdict {
            event_id,
            verdict,
            revision,
            reviewer,
            model,
        } => format!(
            "audit {verdict} `{event_id}` at {revision} by {reviewer}{}",
            model
                .as_deref()
                .map(|model| format!(" ({model})"))
                .unwrap_or_default()
        ),
        Detail::AuditFinding {
            event_id,
            finding_id,
            severity,
            blocking,
            summary,
        } => format!(
            "audit finding {finding_id} ({severity}{}) `{event_id}`: {summary}",
            if *blocking { ", blocking" } else { "" }
        ),
        Detail::AuditDisposition {
            event_id,
            finding_id,
            status,
        } => format!("audit disposition {status} on {finding_id} `{event_id}`"),
        Detail::DebtDeclared {
            event_id,
            debt_reason,
            missing,
        } => format!(
            "debt declared `{event_id}`: {debt_reason}{}",
            missing
                .map(|missing| format!(" (missing {})", missing.as_str()))
                .unwrap_or_default()
        ),
        Detail::DebtDischarge {
            debt_event_id,
            by_event_id,
            verdict,
            reviewer,
            outcome,
        } => format!(
            "debt `{debt_event_id}` {outcome}: discharged by {verdict} `{by_event_id}` from \
             {reviewer}"
        ),
    }
}

fn coverage_label(coverage: &DebtCoverage) -> String {
    match &coverage.model {
        Some(model) => format!("{} ({model})", coverage.reviewer),
        None => format!("{} (model unrecorded)", coverage.reviewer),
    }
}

fn production_label(production: &DebtProduction) -> String {
    let identity = |identity: &crate::model::DebtIdentity| match &identity.effort {
        Some(effort) => format!("{}@{effort}", identity.effective_actor()),
        None => identity.effective_actor().to_string(),
    };
    match &production.planner {
        Some(planner) => format!(
            "planned by {}{}, implemented by {}",
            identity(planner),
            production
                .brief_version
                .map(|version| format!(" (brief v{version})"))
                .unwrap_or_default(),
            identity(&production.implementer)
        ),
        None => format!("implemented by {}", identity(&production.implementer)),
    }
}
