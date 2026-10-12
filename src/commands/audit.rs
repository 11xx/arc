//! Post-integration audit: declaring a review obligation, and discharging it.
//!
//! A change that cannot reach an independent reviewer has two honest options
//! and one dishonest one. It can wait, it can ship carrying recorded debt, or
//! it can quietly approve itself. This module exists so the middle option is
//! available, which is what keeps the third one from being chosen.

use super::*;
use crate::state::{ChangeState, Debt, Patchset};

/// What an `ineligible-review` debt is declared with: which read it names,
/// and why routing did not count it, as the caller supplied them.
#[derive(Debug, Default, Clone)]
pub struct IneligibleInputs {
    /// The verdict event of the read. Omitted, the latest verdict on the
    /// bound patchset.
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
        let (qualifiers, route_version, assessment) = match &self.assessment {
            Some(path) => {
                let assessed = crate::assessment::read(path)?;
                if assessed.independence == Independence::Eligible {
                    bail!(
                        "the assessment says the reviewer is eligible, so there is no \
                         ineligible read to record; declare the debt with the kind its \
                         ledger shows, or discharge it with the audit"
                    );
                }
                (
                    assessed.qualifiers,
                    assessed.route_version,
                    AssessmentBasis::Assessed,
                )
            }
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
        };
        let read = match &self.read {
            Some(named) => verdicts()
                .find(|verdict| &verdict.event_id == named)
                .map(|verdict| verdict.event_id.clone())
                .with_context(|| {
                    format!(
                        "--read {named} is not a verdict on the patchset this debt covers; \
                         `arc status {} --json` lists them",
                        st.change_id
                    )
                })?,
            None => verdicts()
                .max_by_key(|verdict| verdict.created_at)
                .map(|verdict| verdict.event_id.clone())
                .with_context(|| {
                    format!(
                        "{} has no verdict on the patchset this debt covers, so there is no \
                         read to name; record the review first, or declare the kind arc \
                         derives",
                        st.change_id
                    )
                })?,
        };
        Ok(Some(IneligibleRead {
            read,
            qualifiers,
            route_version,
            assessment,
        }))
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

/// The `Implemented-by` values of the work an audit judges: the list the
/// debt's production record kept, else the shipped patchset's commits.
fn audited_implementers(ctx: &Ctx, st: &ChangeState) -> Option<Vec<String>> {
    let recorded = st
        .debt
        .as_ref()
        .and_then(|debt| debt.production.as_ref())
        .and_then(|production| production.trailer_implementers.clone());
    recorded.or_else(|| {
        st.debt_subject_patchset()
            .or_else(|| st.latest_patchset())
            .and_then(|patchset| range_implementers(ctx, patchset))
    })
}

/// Record the obligation. Allowed while open (declared at integration time)
/// and after integration (discovered later).
pub fn declare_debt(
    ctx: &Ctx,
    reference: &str,
    reason: String,
    kind: Option<DebtMissing>,
    inputs: &IneligibleInputs,
) -> Result<()> {
    let reason = reason.trim();
    if reason.is_empty() {
        bail!("--reason must say what review is owed and why it could not run");
    }
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;
    declare_debt_locked(ctx, &store, &st, reason, kind, inputs).map(|_| ())
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
        let implementers = range_implementers(ctx, subject);
        st.debt_production_for(subject, implementers)
    });
    // The caller's kind wins: arc reads the ledger, and the ledger cannot
    // distinguish a merge resolution from a repair.
    let missing = kind.unwrap_or_else(|| st.derive_debt_missing(subject));
    let ineligible = inputs.resolve(st, subject, kind)?;
    let payload = Payload::DebtDeclared {
        reason: reason.to_string(),
        patchset_id: patchset_id.clone(),
        missing,
        coverage,
        production,
        ineligible: ineligible.clone(),
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
                    let published: Event = serde_json::from_value(expected.clone())?;
                    if published.repository_id != existing.origin_replica_id
                        || published.created_at != existing.declared_at
                        || !existing.restated_by(&published)
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
}

/// Record a review performed after integration, discharging any debt.
pub fn audit(ctx: &Ctx, reference: &str, args: AuditArgs) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = store.state(&change_id)?;

    let revision = integrated_revision(&st)?;
    let implementers = audited_implementers(ctx, &st);
    refuse_self_audit(
        ctx,
        &st,
        args.verdict,
        implementers.as_deref().unwrap_or_default(),
    )?;
    let inline = super::review::parse_inline_findings(args.findings_json.as_deref())?;
    if args.verdict == Verdict::Approved && inline.iter().any(|f| f.blocking) {
        bail!("cannot approve while recording blocking findings in the same audit");
    }
    let finding_ids: Vec<String> = inline.iter().map(|f| f.finding_id.clone()).collect();
    let assessment = args
        .assessment
        .as_deref()
        .map(crate::assessment::read)
        .transpose()?
        .map(|assessed| {
            let mut binding = assessed.bind(ctx.resolve_model().model.as_deref());
            let mut uncovered_authors = Vec::new();
            if binding == AuditorBinding::Bound {
                // A list nobody could read is not an empty list: with no
                // authors established, nothing was covered.
                match implementers.as_deref() {
                    None => binding = AuditorBinding::AuthorsUnread,
                    Some(values) => {
                        uncovered_authors = assessed.uncovered(
                            values
                                .iter()
                                .filter_map(|value| crate::trailers::coordinate_model(value)),
                        );
                        if !uncovered_authors.is_empty() {
                            binding = AuditorBinding::AuthorsUncovered;
                        }
                    }
                }
            }
            AuditAssessment {
                independence: assessed.independence,
                binding: Some(binding),
                qualifiers: assessed.qualifiers,
                route_version: assessed.route_version,
                uncovered_authors,
            }
        });
    let binding = assessment.as_ref().and_then(|assessed| assessed.binding);
    let uncovered_authors = assessment
        .as_ref()
        .map(|assessed| assessed.uncovered_authors.clone())
        .unwrap_or_default();

    let payload = Payload::AuditVerdictRecorded {
        revision: revision.clone(),
        verdict: args.verdict,
        body: args.body,
        findings: inline,
        route_version: args.route_version,
        assessment,
    };
    ensure_append_allowed(&st, &payload)?;
    let mut event = ctx.event(&store, &change_id, payload)?;
    store.append_local_event(&mut event)?;
    println!("audit: {:?} at {revision}", args.verdict);
    for id in finding_ids {
        println!("finding: {id}");
    }
    println!("event: {}", event.event_id);
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
    // audit independent of the model that wrote the work.
    let named_model =
        ctx.warn_model_not_independent(implementers.as_deref().unwrap_or_default(), args.verdict);
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
        if after.debt_outstanding() {
            let owed = match after.debt.as_ref().and_then(|debt| debt.missing) {
                Some(DebtMissing::IneligibleReview) => {
                    "an eligible read: an audit with --assessment <file> saying the \
                     auditor is eligible and describing the model it recorded"
                }
                _ => "an independent review",
            };
            println!("audit recorded; the debt still owes {owed}");
        } else {
            println!("debt discharged");
        }
    }
    Ok(())
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
    implementers: &[String],
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
    if let Some((model, value)) = ctx.resolve_model().model.and_then(|model| {
        crate::trailers::naming_model(implementers, &model).map(|value| (model, value))
    }) {
        bail!(
            "model {model:?} matches `Implemented-by: {value}` on the audited work; {policy_source}, so this audit \
             would discharge its own obligation.\n\
  If another reviewer did the pass, record it as theirs:\n\
    arc audit {} --verdict approved --actor '<reviewer>' --harness '<harness>' --model '<model>'\n\
  If you are reporting problems rather than clearing the change, \
--verdict changes-requested is open to anyone.",
            state.change_id
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
