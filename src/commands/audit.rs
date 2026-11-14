//! Post-integration audit: declaring a review obligation, and discharging it.
//!
//! A change that cannot reach an independent reviewer has two honest options
//! and one dishonest one. It can wait, it can ship carrying recorded debt, or
//! it can quietly approve itself. This module exists so the middle option is
//! available, which is what keeps the third one from being chosen.

use super::*;
use crate::operation_println as println;
use crate::state::{ChangeState, Debt, Patchset};

/// What an `ineligible-review` debt is declared with: which read it names,
/// and why routing did not count it, as the caller supplied them.
#[derive(Debug, Default, Clone)]
pub struct IneligibleInputs {
    /// The verdict or audit event of the read. Omitted, the latest verdict
    /// on the bound patchset, else its latest audit.
    pub read: Option<String>,
    /// An `agent-routing assess --json` file.
    pub assessment: Option<PathBuf>,
    /// Qualifiers named by hand, which makes the record declared.
    pub qualifiers: Vec<ReviewQualifier>,
    pub route_version: Option<String>,
}

impl IneligibleInputs {
    fn is_empty(&self) -> bool {
        self.read.is_none()
            && self.assessment.is_none()
            && self.qualifiers.is_empty()
            && self.route_version.is_none()
    }

    /// The record these inputs declare, or `None` for any kind that takes no
    /// read. Everything is checked before a debt is written, so a refusal
    /// leaves the ledger as it was.
    fn resolve(
        &self,
        st: &ChangeState,
        subject: Option<&Patchset>,
        kind: Option<DebtMissing>,
        judge: &ReadJudge<'_>,
    ) -> Result<Option<IneligibleRead>> {
        if kind != Some(DebtMissing::IneligibleReview) {
            if !self.is_empty() {
                bail!(
                    "--read, --assessment, --qualifier and --route-version describe an \
                     ineligible read: they go with --kind ineligible-review"
                );
            }
            return Ok(None);
        }
        let assessed = self
            .assessment
            .as_deref()
            .map(crate::assessment::read)
            .transpose()?;
        let (qualifiers, route_version, assessment) = match &assessed {
            Some(assessed) => (
                assessed.qualifiers.clone(),
                assessed.route_version.clone(),
                AssessmentBasis::Assessed,
            ),
            None if self.qualifiers.is_empty() => bail!(
                "--kind ineligible-review needs why the read does not count: \
                 --assessment <agent-routing assess --json file>, or --qualifier <{}> \
                 (repeatable)",
                ReviewQualifier::NAMES.replace(", ", "|")
            ),
            None => (
                ReviewQualifier::normalized(self.qualifiers.iter().copied()),
                None,
                AssessmentBasis::Declared,
            ),
        };
        if let Some(version) = &self.route_version {
            crate::model::validate_route_version(version)?;
        }
        let route_version = self.route_version.clone().or(route_version);
        let verdicts = || {
            st.verdicts
                .iter()
                .filter(|verdict| subject.is_some_and(|subject| verdict.patchset_id == subject.id))
                .map(|verdict| (&verdict.event_id, verdict.created_at))
        };
        // An audit exists only once the change integrated, when the patchset
        // the debt covers is the one that shipped and every audit read it.
        let audits = || {
            st.audit_verdicts
                .iter()
                .filter(|_| subject.is_some())
                .map(|audit| (&audit.event_id, audit.created_at))
        };
        let latest = |reads: Vec<(&String, _)>| {
            reads
                .into_iter()
                .max_by_key(|(_, at)| *at)
                .map(|(id, _)| id.clone())
        };
        let read = match &self.read {
            Some(named) => verdicts()
                .chain(audits())
                .find(|(id, _)| *id == named)
                .map(|(id, _)| id.clone())
                .with_context(|| {
                    format!(
                        "--read {named} is not a verdict or audit on the patchset this debt \
                         covers; `arc status {} --json` lists them",
                        st.change_id
                    )
                })?,
            None => latest(verdicts().collect())
                .or_else(|| latest(audits().collect()))
                .with_context(|| {
                    format!(
                        "{} has no verdict or audit on the patchset this debt covers, so \
                         there is no read to name; record the review or audit first, or \
                         declare the kind arc derives",
                        st.change_id
                    )
                })?,
        };
        if let (Some(assessed), Some(subject)) = (assessed, subject) {
            if judge.would_clear(st, &read, assessed, subject) {
                bail!(
                    "under this assessment read {read} clears the review an ineligible-review \
                     debt would owe ({}), so there is no ineligible read to record; declare \
                     the debt without --kind, --read, --assessment, --qualifier and \
                     --route-version to record the kind the ledger derives",
                    judge.clearance_name()
                );
            }
        }
        Ok(Some(IneligibleRead {
            read,
            qualifiers,
            findings: None,
            route_version,
            assessment,
        }))
    }
}

/// What judges whether a named read would clear a declared debt: the
/// destination's review clearance policy, the production the declaration
/// records, and the `Implemented-by` values of the range it covers.
pub(super) struct ReadJudge<'a> {
    clearance: crate::policy::ReviewClearance,
    production: Option<&'a crate::model::DebtProduction>,
    implementers: Option<&'a [String]>,
    extent: Option<&'a crate::extent::Extent>,
}

impl ReadJudge<'_> {
    /// Whether `read`, under `assessed` bound to its own observed or
    /// recorded model and to the range's authors, discharges an
    /// `ineligible-review` debt on `subject`. An undetermined judgment
    /// answers `false`, which records the debt.
    fn would_clear(
        &self,
        st: &ChangeState,
        read: &str,
        assessed: crate::assessment::Assessment,
        subject: &Patchset,
    ) -> bool {
        let model = st
            .verdicts
            .iter()
            .find(|verdict| verdict.event_id == read)
            .map(|verdict| verdict.observed_model.clone().or(verdict.model.clone()))
            .or_else(|| {
                st.audit_verdicts
                    .iter()
                    .find(|audit| audit.event_id == read)
                    .map(|audit| audit.observed_model.clone().or(audit.model.clone()))
            })
            .flatten();
        let bound = bind_assessment_to(model.as_deref(), assessed, self.implementers);
        st.read_would_clear(
            read,
            &bound,
            subject,
            self.production,
            self.extent,
            self.clearance,
        )
    }

    fn clearance_name(&self) -> &'static str {
        if self.clearance.is_assessed() {
            "review_clearance = \"assessed\""
        } else {
            "independence decides without review_clearance"
        }
    }
}

/// The `Implemented-by` values of the commits a patchset ships, or `None`
/// when the range cannot be read, which is said once and never a refusal.
pub(super) fn range_implementers(ctx: &Ctx, patchset: &Patchset) -> Option<Vec<String>> {
    match crate::trailers::range_implementers(&ctx.cwd, &patchset.base, &patchset.head) {
        Ok(values) => Some(values),
        Err(error) => {
            eprintln!(
                "warning: could not read the Implemented-by trailers of {}: {error:#}",
                patchset.id
            );
            None
        }
    }
}

/// The `Implemented-by` values of the commits a change shipped: its landed
/// range when a landing closed it (for a member landed by replay, the
/// commits it landed as), else `patchset`'s.
fn shipped_implementers(ctx: &Ctx, st: &ChangeState, patchset: &Patchset) -> Option<Vec<String>> {
    let Some((base, tip)) = st.landed_range() else {
        return range_implementers(ctx, patchset);
    };
    match crate::trailers::range_implementers(&ctx.cwd, base, tip) {
        Ok(values) => Some(values),
        Err(error) => {
            eprintln!(
                "warning: could not read the Implemented-by trailers of {}'s landed range: \
                 {error:#}",
                st.change_id
            );
            None
        }
    }
}

/// The `Implemented-by` values of the work an audit judges: the list the
/// debt's production record kept, else the shipped commits'.
fn audited_implementers(ctx: &Ctx, st: &ChangeState) -> Option<Vec<String>> {
    let recorded = st
        .debt
        .as_ref()
        .and_then(|debt| debt.production.as_ref())
        .and_then(|production| production.trailer_implementers.clone());
    recorded.or_else(|| {
        st.debt_subject_patchset()
            .or_else(|| st.latest_patchset())
            .and_then(|patchset| shipped_implementers(ctx, st, patchset))
    })
}

/// The assessment at `path`, kept as the read the invoking context records
/// will keep it; see [`bind_assessment`].
fn read_assessment(
    ctx: &Ctx,
    path: &Path,
    implementers: Option<&[String]>,
) -> Result<AuditAssessment> {
    Ok(bind_assessment(
        ctx,
        crate::assessment::read(path)?,
        implementers,
    ))
}

/// `assessed` as a read keeps it: bound to the reader's model (see
/// [`binding_model`]), and to every author coordinate of `implementers`, the
/// range's `Implemented-by` values, or `None` when they could not be read.
pub(super) fn bind_assessment(
    ctx: &Ctx,
    assessed: crate::assessment::Assessment,
    implementers: Option<&[String]>,
) -> AuditAssessment {
    bind_assessment_to(binding_model(ctx).as_deref(), assessed, implementers)
}

/// The model a read binds to: the one the reader's session store observed
/// wherever the store has one, else the declared model. A declared model the
/// store contradicts is recorded beside the observed one and binds nothing.
pub(super) fn binding_model(ctx: &Ctx) -> Option<String> {
    let attribution = ctx.resolve_model();
    match attribution.provenance.model_disagreement {
        Some(disagreement) => Some(disagreement.observed),
        None => attribution.model,
    }
}

/// Say that the reader's session store contradicts the declared model, which
/// the read records and its assessment does not bind.
pub(super) fn warn_model_disagreement(ctx: &Ctx, declaration: &str) {
    if let Some(disagreement) = ctx.resolve_model().provenance.model_disagreement {
        eprintln!(
            "warning: the reader's session store records model {}, {declaration} declares {}; \
             the read records the disagreement, and an assessment binds the observed model",
            disagreement.observed, disagreement.declared
        );
    }
}

/// `assessed` bound to the reader's `model` and to every author coordinate
/// of `implementers`; see [`bind_assessment`].
fn bind_assessment_to(
    model: Option<&str>,
    assessed: crate::assessment::Assessment,
    implementers: Option<&[String]>,
) -> AuditAssessment {
    let mut binding = assessed.bind(model);
    let mut uncovered_authors = Vec::new();
    if binding == AuditorBinding::Bound {
        // A list nobody could read is not an empty list: with no authors
        // established, nothing was covered.
        match implementers {
            None => binding = AuditorBinding::AuthorsUnread,
            Some(values) => {
                uncovered_authors = assessed.uncovered(
                    values
                        .iter()
                        .filter_map(|value| crate::trailers::coordinate_model_effort(value)),
                );
                if !uncovered_authors.is_empty() {
                    binding = AuditorBinding::AuthorsUncovered;
                }
            }
        }
    }
    AuditAssessment {
        independence: assessed.independence,
        findings: assessed.findings,
        clears: assessed.clears,
        binding: Some(binding),
        qualifiers: assessed.qualifiers,
        route_version: assessed.route_version,
        uncovered_authors,
    }
}

/// Record the obligation. Allowed while open (declared at integration time)
/// and after integration (discovered later).
pub fn declare_debt(
    ctx: &Ctx,
    reference: &str,
    reason: String,
    kind: Option<DebtMissing>,
    inputs: &IneligibleInputs,
    extent: Option<&crate::extent::ExtentInput>,
) -> Result<()> {
    let reason = reason.trim();
    if reason.is_empty() {
        bail!("--reason must say what review is owed and why it could not run");
    }
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;
    plan_debt_with(ctx, &store, &st, reason, kind, inputs, extent)?
        .declare(&store)
        .map(|_| ())
}

/// What a debt declaration observed about its event: the event, and whether
/// it is durably in the ledger.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct DebtReceipt {
    pub event_id: String,
    /// `written` or `reused` once the event is durable; `attempted` for a new
    /// event whose publication did not complete.
    pub disposition: &'static str,
    /// `durable` when the backing event file or import batch and its directory
    /// entry were synced;
    /// `visible-unconfirmed` when the event is readable but the sync failed;
    /// `absent` when it is not in the ledger; `unknown` when the ledger could
    /// not be read to tell.
    pub publication: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication_error: Option<String>,
    pub reason: String,
    pub patchset_id: Option<String>,
    pub origin_replica_id: String,
    pub declared_by: String,
}

impl DebtReceipt {
    /// One line naming the event and what is known of its publication.
    pub fn summary(&self) -> String {
        format!(
            "debt event {} {} publication={}",
            self.event_id, self.disposition, self.publication
        )
    }
}

/// A declaration whose publication did not complete, carrying what was
/// observed of the event so a caller can report it.
#[derive(Debug)]
pub(super) struct DebtPublicationFailure(pub DebtReceipt);

impl std::fmt::Display for DebtPublicationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "debt event {} publication={}: {}",
            self.0.event_id,
            self.0.publication,
            self.0
                .publication_error
                .as_deref()
                .unwrap_or("not completed")
        )
    }
}

impl std::error::Error for DebtPublicationFailure {}

impl DebtReceipt {
    /// The event this declaration wrote, when the change held no debt for the
    /// same patchset beforehand. A plan that said it would declare a debt
    /// answers for exactly this event; another declaration it never saw, or a
    /// reused one, is not what it described.
    pub(super) fn event_written_over(&self, before: &ChangeState) -> Option<&str> {
        let held = before
            .debt
            .as_ref()
            .is_some_and(|debt| debt.patchset_id == self.patchset_id);
        (self.disposition == "written" && !held).then_some(self.event_id.as_str())
    }
}

/// A debt declaration judged against the change as it stands, with every
/// refusal already made and nothing written. `declare` writes it; `projected`
/// shows the change as it would read once it was.
pub(super) struct DebtPlan {
    event: crate::store::LocalEvent,
    reason: String,
    patchset_id: Option<String>,
    missing: DebtMissing,
    ineligible: Option<IneligibleRead>,
    /// The debt already on the change that this declaration restates.
    restated: Option<Debt>,
}

/// The caller holds the change transition lock across observation and
/// declaration. A refusal here leaves the ledger as it was.
pub(super) fn plan_debt(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    reason: &str,
    kind: Option<DebtMissing>,
    inputs: &IneligibleInputs,
) -> Result<DebtPlan> {
    plan_debt_with(ctx, store, st, reason, kind, inputs, None)
}

/// [`plan_debt`] for an obligation that names what the owed review must
/// cover: `extent` resolved against the patchset the debt binds to.
fn plan_debt_with(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    reason: &str,
    kind: Option<DebtMissing>,
    inputs: &IneligibleInputs,
    extent: Option<&crate::extent::ExtentInput>,
) -> Result<DebtPlan> {
    if kind == Some(DebtMissing::ReplayedUnread) {
        bail!(
            "replayed-unread is derived, never declared: `arc land --debt` records it for an \
             approved member it replayed onto a base the approval did not read"
        );
    }
    plan_debt_of(ctx, store, st, reason, kind, inputs, None, extent)
}

/// The `replayed-unread` debt a landing derives for a member whose exact
/// approved patchset it replayed without conflict onto another base.
pub(super) fn plan_replayed_debt(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    reason: &str,
    replayed: crate::model::ReplayedRead,
) -> Result<DebtPlan> {
    plan_debt_of(
        ctx,
        store,
        st,
        reason,
        Some(DebtMissing::ReplayedUnread),
        &IneligibleInputs::default(),
        Some(replayed),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn plan_debt_of(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    reason: &str,
    kind: Option<DebtMissing>,
    inputs: &IneligibleInputs,
    replayed: Option<crate::model::ReplayedRead>,
    extent: Option<&crate::extent::ExtentInput>,
) -> Result<DebtPlan> {
    let reason = reason.trim();
    if reason.is_empty() {
        bail!("--reason must say what review is owed and why it could not run");
    }
    let change_id = &st.change_id;
    // An open change records the deficit for the patchset that is about to
    // ship. This can supply an absent verdict or rescue a self-approval
    // rejected by policy. A closed change has no gate left to satisfy, so its
    // debt binds to no patchset — but what it says was missing is still read
    // off the work that shipped rather than off nothing.
    let subject = st.debt_subject_patchset();
    let patchset_id = if st.is_closed() {
        None
    } else {
        subject.map(|patchset| patchset.id.clone())
    };
    let coverage = subject
        .map(|subject| st.debt_coverage_for(subject))
        .unwrap_or_default();
    let production = subject.map(|subject| {
        let implementers = shipped_implementers(ctx, st, subject);
        st.debt_production_for(subject, implementers)
    });
    let extent = match extent {
        None => None,
        Some(input) => {
            let (base, revision) = st.extent_subject().with_context(|| {
                format!(
                    "{change_id} has no patchset, so there is no subject to resolve the extent \
                     against; run `arc snapshot` first"
                )
            })?;
            Some(crate::extent::resolve(&ctx.cwd, input, base, revision)?)
        }
    };
    // The caller's kind wins: arc reads the ledger, and the ledger cannot
    // distinguish a merge resolution from a repair.
    // A judgment answers to the destination: its policy, and its review
    // context, which `--into` may make another branch's than the one the
    // change recorded. Both are read only where a judgment needs them.
    let destination;
    let (clearance, judged) =
        if (kind.is_none() && subject.is_some()) || inputs.assessment.is_some() {
            let declarations = ctx.declarations(st)?;
            let mut state = st.clone();
            state.set_review_context(declarations.review_context);
            destination = state;
            (declarations.policy.policy.review_clearance, &destination)
        } else {
            (crate::policy::ReviewClearance::default(), st)
        };
    let ineligible = inputs.resolve(
        judged,
        subject,
        kind,
        &ReadJudge {
            clearance,
            production: production.as_ref(),
            implementers: production
                .as_ref()
                .and_then(|production| production.trailer_implementers.as_deref()),
            extent: extent.as_ref(),
        },
    )?;
    // Under the clearance policy an approval that only finds is the read the
    // obligation names, so the review it did not supply is owed as
    // `ineligible-review`.
    let findings = match (kind, subject) {
        (None, Some(subject)) if clearance.is_assessed() => judged.findings_approval(subject),
        _ => None,
    };
    let (missing, ineligible) = match findings {
        Some(read) => (DebtMissing::IneligibleReview, Some(read)),
        None => (
            kind.unwrap_or_else(|| judged.derive_debt_missing(subject)),
            ineligible,
        ),
    };
    let payload = Payload::DebtDeclared {
        reason: reason.to_string(),
        patchset_id: patchset_id.clone(),
        missing,
        coverage,
        production,
        ineligible: ineligible.clone(),
        replayed,
        extent,
    };
    ensure_append_allowed(st, &payload)?;
    let event = ctx.event(store, change_id, payload)?;
    // One judgment is one obligation: an `arc integrate --debt` retried after
    // a refusal answers with the debt its first attempt recorded.
    let restated = st
        .debt
        .as_ref()
        .filter(|debt| debt.restated_by(&event))
        .cloned();
    Ok(DebtPlan {
        event,
        reason: reason.to_string(),
        patchset_id,
        missing,
        ineligible,
        restated,
    })
}

impl DebtPlan {
    /// The declaration this plan would write.
    pub(super) fn payload(&self) -> &Payload {
        &self.event.payload
    }

    /// The debt already declared that this plan restates, when there is one.
    pub(super) fn restated_event_id(&self) -> Option<&str> {
        self.restated.as_ref().map(|debt| debt.event_id.as_str())
    }

    /// The debt as a plan reports it, whose event is the one already in the
    /// ledger or none until `declare` writes it.
    pub fn planned(&self) -> crate::integration::PlannedDebt {
        crate::integration::PlannedDebt {
            disposition: if self.restated.is_some() {
                crate::integration::DebtDisposition::AlreadyDeclared
            } else {
                crate::integration::DebtDisposition::WouldDeclare
            },
            kind: self.missing,
            reason: self.reason.clone(),
            patchset_id: self.patchset_id.clone(),
            event_id: self.restated.as_ref().map(|debt| debt.event_id.clone()),
            ineligible: self.ineligible.clone(),
        }
    }

    /// The change as it reads once this declaration is in its ledger, without
    /// writing it. A declaration the change already holds changes nothing.
    pub fn projected(&self, store: &Store, st: &ChangeState) -> Result<ChangeState> {
        if self.restated.is_some() {
            return Ok(st.clone());
        }
        let mut events = store.load_events(&st.change_id)?;
        events.push(self.event.preview_after(&events)?);
        store
            .project_state(&events, &store.rewrites()?)
            .context("the declaration would make the change unreadable; nothing was written")
    }

    /// Write the declaration, or confirm the one it restates.
    pub fn declare(self, store: &Store) -> Result<DebtReceipt> {
        let DebtPlan {
            event,
            reason,
            patchset_id,
            missing,
            restated,
            ..
        } = self;
        let mut event = event;
        let change_id = event.change_id.clone();
        let origin_replica_id = event.repository_id.clone();
        let declared_by = event.actor.clone();
        let receipt =
            |event_id: &str, disposition, publication, error: Option<String>| DebtReceipt {
                event_id: event_id.to_string(),
                disposition,
                publication,
                publication_error: error,
                reason: reason.clone(),
                patchset_id: patchset_id.clone(),
                origin_replica_id: origin_replica_id.clone(),
                declared_by: declared_by.clone(),
            };
        let (verb, receipt) = match restated {
            Some(existing) => {
                // An earlier attempt may have left its event visible without a
                // confirmed sync; a retry confirms its exact backing publication,
                // including the batch that holds an imported obligation.
                let mut receipt = receipt(&existing.event_id, "reused", "durable", None);
                receipt.origin_replica_id = existing.origin_replica_id.clone();
                receipt.declared_by = existing.actor.clone();
                let confirmation = (|| -> Result<()> {
                    let expected = store
                        .raw_events(&change_id)?
                        .into_iter()
                        .find(|(id, _)| id == &existing.event_id)
                        .map(|(_, value)| value)
                        .context("debt event has no backing publication")?;
                    let mut projected: Event = serde_json::from_value(expected.clone())?;
                    if let Payload::DebtDeclared {
                        extent: Some(extent),
                        ..
                    } = &mut projected.payload
                    {
                        let rewrites = store.rewrites()?;
                        for revision in extent.subject.revisions_mut() {
                            *revision = rewrites.approved_successor(revision)?;
                        }
                    }
                    if projected.repository_id != existing.origin_replica_id
                        || projected.created_at != existing.declared_at
                        || !existing.restated_by(&projected)
                    {
                        bail!("debt event changed before durability confirmation");
                    }
                    store.sync_published_event(&expected)
                })();
                if let Err(error) = confirmation {
                    receipt.publication = "visible-unconfirmed";
                    receipt.publication_error = Some(format!("{error:#}"));
                    return Err(DebtPublicationFailure(receipt).into());
                }
                ("already declared", receipt)
            }
            None => {
                if let Err(error) = store.append_local_event(&mut event) {
                    let publication = store.publication_after_failed_append(&event);
                    let message = Some(format!("{error:#}"));
                    return Err(DebtPublicationFailure(receipt(
                        &event.event_id,
                        "attempted",
                        publication,
                        message,
                    ))
                    .into());
                }
                (
                    "declared",
                    receipt(&event.event_id, "written", "durable", None),
                )
            }
        };
        let event_id = receipt.event_id.as_str();
        match &receipt.patchset_id {
            Some(id) => {
                crate::operation_println!(
                    "debt {verb} for {id} ({}): {}",
                    missing.as_str(),
                    receipt.reason
                )
            }
            None => {
                crate::operation_println!("debt {verb} ({}): {}", missing.as_str(), receipt.reason)
            }
        }
        crate::operation_println!("event: {event_id}");
        Ok(receipt)
    }
}

/// The caller holds the change transition lock across observation and declaration.
pub(super) fn declare_debt_locked(
    ctx: &Ctx,
    store: &Store,
    st: &ChangeState,
    reason: &str,
    kind: Option<DebtMissing>,
    inputs: &IneligibleInputs,
) -> Result<DebtReceipt> {
    plan_debt(ctx, store, st, reason, kind, inputs)?.declare(store)
}

pub struct AuditArgs {
    pub verdict: Verdict,
    pub body: Option<String>,
    pub findings_json: Option<String>,
    /// The routing version that selected the auditor, as the caller declared
    /// it. `None` records an unrouted audit.
    pub route_version: Option<String>,
    /// An `agent-routing assess --json` file for the auditor. An
    /// `ineligible-review` debt is discharged only by an audit whose
    /// assessment is eligible.
    pub assessment: Option<PathBuf>,
    /// An assessment already read, bound as `assessment` would be.
    pub assessed: Option<crate::assessment::Assessment>,
    /// Findings already read, recorded when `findings_json` names none.
    pub findings: Vec<InlineFinding>,
    /// What the auditor read, resolved against the shipped patchset before
    /// anything is written. `None` records an audit whose extent is not
    /// established.
    pub extent: Option<crate::extent::ExtentInput>,
}

/// Record a review performed after integration, discharging any debt.
pub fn audit(ctx: &Ctx, reference: &str, args: AuditArgs) -> Result<()> {
    record_audit(ctx, reference, args, None).map(drop)
}

/// Record an audit. A receipt item's audit passes its guard under the
/// transition lock, once the change's state is read, as
/// [`super::review::record_verdict`] describes.
pub(super) fn record_audit(
    ctx: &Ctx,
    reference: &str,
    args: AuditArgs,
    receipt: Option<&super::receipt::ItemGuard>,
) -> Result<super::review::Recorded> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;
    let admitted = match receipt {
        None => {
            warn_model_disagreement(ctx, "the audit");
            None
        }
        Some(guard) => {
            let events = store.load_events(&change_id)?;
            match guard.admit(ctx, &store, &events, &st)? {
                super::receipt::Admission::Published(event_id) => {
                    println!("event: {event_id} (already recorded from this item)");
                    return Ok(super::review::Recorded {
                        event_id,
                        existing: true,
                    });
                }
                super::receipt::Admission::Fresh(admitted) => Some(admitted),
            }
        }
    };

    let revision = integrated_revision(&st)?;
    let extent = match &args.extent {
        None => None,
        Some(input) => {
            let (base, revision) = st.extent_subject().with_context(|| {
                format!(
                    "{change_id} records no shipped range, so there is no subject to resolve \
                     the extent against"
                )
            })?;
            let resolved = crate::extent::resolve(&ctx.cwd, input, base, revision);
            Some(match receipt {
                Some(_) => resolved.map_err(super::receipt::extent_refusal)?,
                None => resolved?,
            })
        }
    };
    let implementers = audited_implementers(ctx, &st);
    let recorded_at = chrono::Utc::now();
    let tiebreak_lookup = st
        .debt_subject_patchset()
        .or_else(|| st.latest_patchset())
        .map_or(Ok(None), |subject| {
            ctx.tiebreak_reviewer(&st, subject, recorded_at)
        });
    let tiebreak_error = tiebreak_lookup
        .as_ref()
        .err()
        .map(|error| format!("{error:#}"));
    let tiebreak_reviewer = tiebreak_lookup.unwrap_or_default();
    refuse_self_audit(ctx, &st, args.verdict, tiebreak_reviewer.is_some()).map_err(|error| {
        match &tiebreak_error {
            Some(reason) => error.context(format!("tie-break election unavailable: {reason}")),
            None => error,
        }
    })?;
    let inline = match args.findings_json.as_deref() {
        Some(src) => super::review::parse_inline_findings(Some(src))?,
        None => args.findings,
    };
    if args.verdict == Verdict::Approved && inline.iter().any(|f| f.blocking) {
        bail!("cannot approve while recording blocking findings in the same audit");
    }
    let finding_ids: Vec<String> = inline.iter().map(|f| f.finding_id.clone()).collect();
    let assessment = match (args.assessment.as_deref(), args.assessed) {
        (Some(path), _) => Some(read_assessment(ctx, path, implementers.as_deref())?),
        (None, Some(assessed)) => Some(bind_assessment(ctx, assessed, implementers.as_deref())),
        (None, None) => None,
    };
    let binding = assessment.as_ref().and_then(|assessed| assessed.binding);
    let uncovered_authors = assessment
        .as_ref()
        .map(|assessed| assessed.uncovered_authors.clone())
        .unwrap_or_default();

    let shipped = st.debt_subject_patchset();
    let production = st.debt.as_ref().and_then(|debt| debt.production.as_ref());
    // A production summary is not the only ledger authorship a debt can have:
    // a debt recorded before the summary existed still names the patchset its
    // work shipped as. Only a debt whose range has no ledger record at all
    // reads as declared evidence.
    if production.is_none() && shipped.is_none() {
        eprintln!(
            "warning: the audited range has no ledger authorship record; its \
             `Implemented-by` trailers are declared evidence, and on their own they neither \
             discharge nor refuse a review."
        );
    }
    if args.verdict == Verdict::Approved {
        let implementer = production
            .map(|production| &production.implementer)
            .cloned()
            .or_else(|| shipped.map(|patchset| st.implementer_identity(patchset)));
        if let Some(implementer) = implementer.filter(|identity| identity.model.is_none()) {
            eprintln!(
                "warning: the implementer's model is not recorded by the ledger for {} of the \
                 audited range; independence is judged by the actor, session and harness it \
                 records.",
                implementer.effective_actor()
            );
            if let Some(note) = production.and_then(|production| production.trailer_note()) {
                eprintln!("warning: {note} is declared text, not ledger authorship.");
            }
        }
    }
    let auditor = ctx.on_behalf_of.as_deref().unwrap_or(&ctx.actor);
    let auditor_models: Vec<String> = ctx
        .reader_models()
        .into_iter()
        .map(|reader| reader.model)
        .collect();
    let auditor_models: Vec<&str> = auditor_models.iter().map(String::as_str).collect();
    let held_roles = st
        .debt_subject_patchset()
        .or_else(|| st.latest_patchset())
        .map(|patchset| st.held_review_roles(patchset, auditor, &auditor_models))
        .unwrap_or_default();
    let basis = crate::review_clearance::record_basis(ctx, &st, true, recorded_at, receipt)?;
    let payload = Payload::AuditVerdictRecorded {
        basis,
        tiebreak_reviewer: tiebreak_reviewer.clone(),
        tiebreak_target: tiebreak_reviewer
            .as_ref()
            .map(|_| crate::declarations::accepted_target(&st).to_string()),
        revision: revision.clone(),
        verdict: args.verdict,
        body: args.body,
        findings: inline,
        route_version: args.route_version,
        assessment,
        held_roles,
        dispositions: admitted
            .as_ref()
            .map(|admitted| admitted.dispositions.clone())
            .unwrap_or_default(),
        receipt: admitted.map(|admitted| admitted.reference),
        extent,
    };
    ensure_append_allowed(&st, &payload)?;
    crate::review_clearance::enforce_coordinate(ctx, &st, true, recorded_at)?;
    let mut event = ctx.event_at(&store, &change_id, recorded_at, payload)?;
    store.append_local_event(&mut event)?;
    println!("audit: {:?} at {revision}", args.verdict);
    for id in finding_ids {
        println!("finding: {id}");
    }
    println!("event: {}", event.event_id);
    if let Some(error) = tiebreak_error {
        eprintln!("warning: audit {} is recorded; tie-break election unavailable, so no exception applies: {error}", event.event_id);
    }
    if let Some(note) = binding.and_then(|binding| binding.failure_note(&uncovered_authors)) {
        println!("assessment {note}");
    }
    // Said once the audit exists, and only then: an assumed identity is not
    // refused where policy permits self-approval, because it cannot be
    // corrected after integration and refusing would leave the debt
    // undischargeable. But it is said out loud, or debt would look like a way
    // around the independence rule rather than a way of carrying it.
    let named_auditor = st
        .latest_patchset()
        .is_some_and(|patchset| ctx.warn_verdict_not_independent(patchset, args.verdict));
    // The model is a second identity: another actor name does not make an
    // audit independent of the model the ledger credits with the work.
    // A tie-break election waives only this model match. Authoring models
    // come from the ledger alone; commit trailers are kept
    // only as declared comparison evidence and never supply one.
    let authoring_models = st
        .debt_subject_patchset()
        .or_else(|| st.latest_patchset())
        .map(|patchset| {
            st.authoring_model_values(
                patchset,
                st.debt.as_ref().and_then(|debt| debt.production.as_ref()),
            )
        })
        .unwrap_or_default();
    let named_model = ctx.warn_model_not_independent(
        &authoring_models,
        args.verdict,
        tiebreak_reviewer.is_some(),
    );
    let named_auditor = named_auditor || named_model;
    // The auditor's own identity is the stronger fact and subsumes this one.
    // An audit by a declared reviewer of work whose author arc invented is
    // still worth saying: the pass was independent of a name nobody claimed.
    if !named_auditor
        && args.verdict == Verdict::Approved
        && st.latest_patchset().is_some_and(|p| p.author_assumed())
    {
        eprintln!(
            "warning: arc assumed the authoring identity of the audited work, so this audit \
             shows that a review happened and not that it was independent of whoever wrote it."
        );
    }
    // Recomputed from the ledger this audit just joined, rather than assumed
    // from the debt merely existing: an audit by somebody who wrote the work
    // is a legitimate record and does not settle a debt owed an independent
    // review, and saying otherwise would be the one claim a reader acts on.
    if st.debt.is_some() {
        let (_, after) = ctx.load_state(&store, &change_id)?;
        let discharger = after
            .debt
            .as_ref()
            .and_then(|debt| debt.discharged_by_event_id.as_deref());
        if after.debt_outstanding() {
            let owed = match after.debt.as_ref().and_then(|debt| debt.missing) {
                _ if after
                    .review_context
                    .as_ref()
                    .is_some_and(|context| context.registry(true).assessed() == Some(true)) =>
                {
                    "a clearing read under the applicable review ladder"
                }
                _ if after.has_assessed_clearance() => {
                    "a clearing read: an audit by a person, or by an agent with --assessment \
                     <file> from `agent-routing assess --purpose clearance --json` that says \
                     it clears and describes the model and effort it recorded"
                }
                Some(DebtMissing::IneligibleReview) => {
                    "an eligible read: an audit that approves or requests changes with \
                     --assessment <file> saying the auditor is eligible and describing the \
                     model it recorded"
                }
                _ => "an independent review",
            };
            match after.audit_discharge_bar(&event.event_id) {
                Some(bar) => println!(
                    "audit recorded; the debt still owes {owed}; this read {}",
                    bar.reason()
                ),
                None => println!("audit recorded; the debt still owes {owed}"),
            }
        } else if discharger == Some(event.event_id.as_str()) {
            println!("debt discharged");
        } else if let Some(discharger) = discharger {
            println!("debt already discharged by {discharger}");
        }
    }
    Ok(super::review::Recorded {
        event_id: event.event_id.clone(),
        existing: false,
    })
}

/// Refuse an approving audit from the identity that wrote the work.
///
/// The debt exists because an independent verdict was unavailable. If the
/// author can discharge it, the obligation is decorative: the change ships on
/// a self-approval and then clears its own record. A repository that does not
/// forbid self-approval has already opted out of this and is left alone.
///
/// Only approval is restricted. Raising problems needs no independence, so an
/// author auditing its own work into `changes-requested` is useful and allowed.
fn refuse_self_audit(
    ctx: &Ctx,
    state: &ChangeState,
    verdict: Verdict,
    tiebreak: bool,
) -> Result<()> {
    if verdict != Verdict::Approved {
        return Ok(());
    }
    let policy = ctx.declarations(state)?.policy;
    if !policy.policy.forbid_self_approval {
        return Ok(());
    }
    let policy_sources = policy
        .sources
        .sources_for("policy.forbid_self_approval=true");
    let policy_source = if policy_sources.is_empty() {
        "policy source unavailable".to_string()
    } else {
        format!("policy declared by {}", policy_sources.join(", "))
    };
    let Some(patchset) = state.latest_patchset() else {
        return Ok(());
    };
    let auditor = ctx.on_behalf_of.as_deref().unwrap_or(&ctx.actor);
    let production = state
        .debt
        .as_ref()
        .and_then(|debt| debt.production.as_ref());
    // An identity arc invented names nobody in particular, so two of them that
    // happen to differ do not show that two people acted. The same rule the
    // pre-integration guard applies, applied to the review that discharges the
    // obligation left behind.
    let auditor_assumed = ctx
        .on_behalf_of
        .is_none()
        .then(|| ctx.actor_source.assumed_from())
        .flatten();
    // Only the auditor's identity is refused, because only it can be
    // corrected: declaring yourself is a flag away. The author's identity is
    // already on the ledger and the ledger is append-only, and an audit exists
    // precisely to answer for work that shipped — refusing every audit of a
    // change snapshotted under an assumed identity would leave its debt
    // permanently undischargeable. What the audit is worth in that case is a
    // question the recorded provenance answers for a reader.
    if let Some(source) = auditor_assumed {
        bail!(
            "arc assumed the auditing identity from {source}; {policy_source}, so this audit cannot show that \
anyone independent looked at {}.\n\
  Declare who is auditing: arc audit {} --verdict approved --actor '<reviewer>'",
            state.change_id,
            state.change_id
        );
    }
    if let Some(contributor) = patchset.contributor_match(auditor) {
        bail!(
            "{auditor} matches contributor {contributor} on the audited work; {policy_source}, so this audit \
             would discharge its own obligation.\n\
  If another reviewer did the pass, record it as theirs:\n\
    arc audit {} --verdict approved --actor '<reviewer>' --harness '<harness>' --model '<model>'\n\
  If you are reporting problems rather than clearing the change, \
--verdict changes-requested is open to anyone.",
            state.change_id
        );
    }
    if state
        .authorship_match(patchset, production, auditor, &[])
        .is_some()
    {
        bail!(
            "{auditor} matches the ledger implementer of the audited work; {policy_source}, so this audit \
             would discharge its own obligation.\n\
  If another reviewer did the pass, record it as theirs:\n\
    arc audit {} --verdict approved --actor '<reviewer>' --harness '<harness>' --model '<model>'\n\
  If you are reporting problems rather than clearing the change, \
--verdict changes-requested is open to anyone.",
            state.change_id
        );
    }
    let authoring_models = state.authoring_model_values(patchset, production);
    if let Some((reader, value)) = ctx
        .reader_models()
        .into_iter()
        .filter(|_| !tiebreak)
        .find_map(|reader| {
            crate::trailers::naming_model(&authoring_models, &reader.model)
                .map(|value| (reader, value))
        })
    {
        let remedy = crate::review_independence::remedy(&authoring_models, &reader.model);
        bail!(
            "{} matches the ledger implementer's model {value} on the audited work; {policy_source}, so this audit \
             would discharge its own obligation.{remedy}\n\
  If another reviewer did the pass, record it as theirs:\n\
    arc audit {} --verdict approved --actor '<reviewer>' --harness '<harness>' --model '<model>'\n\
  If you are reporting problems rather than clearing the change, \
--verdict changes-requested is open to anyone.",
            reader.described(),
            state.change_id,
        );
    }
    Ok(())
}

/// The revision an audit reviews: what actually integrated.
fn integrated_revision(state: &ChangeState) -> Result<String> {
    let closure = state
        .closure
        .as_ref()
        .context("change is not closed; audits review an integrated revision")?;
    closure
        .integrated_commit
        .clone()
        .context("change closed without an integration commit; nothing to audit")
}
