//! Post-integration audit: declaring a review obligation, and discharging it.
//!
//! A change that cannot reach an independent reviewer has two honest options
//! and one dishonest one. It can wait, it can ship carrying recorded debt, or
//! it can quietly approve itself. This module exists so the middle option is
//! available, which is what keeps the third one from being chosen.

use super::*;
use crate::state::ChangeState;

/// Record the obligation. Allowed while open (declared at integration time)
/// and after integration (discovered later).
pub fn declare_audit_debt(ctx: &Ctx, reference: &str, reason: String) -> Result<()> {
    let reason = reason.trim();
    if reason.is_empty() {
        bail!("--reason must say what review is owed and why it could not run");
    }
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = state::reduce(&store.load_events(&change_id)?)?;
    // An open change records the missing independent verdict for the patchset
    // that is about to ship. This can supply an absent verdict or rescue a
    // self-approval rejected by policy. A closed change has no gate left to
    // satisfy, so its debt is recorded as a bare obligation.
    let patchset_id = if st.is_closed() {
        None
    } else {
        st.latest_patchset().map(|patchset| patchset.id.clone())
    };
    let coverage = patchset_id
        .as_deref()
        .map(|patchset_id| {
            st.verdicts
                .iter()
                .filter(|verdict| verdict.patchset_id == patchset_id)
                .map(|verdict| DebtCoverage {
                    reviewer: verdict.effective_author().to_string(),
                    model: verdict.model.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let payload = Payload::DebtDeclared {
        reason: reason.to_string(),
        patchset_id: patchset_id.clone(),
        missing: DebtMissing::IndependentReview,
        coverage,
    };
    ensure_append_allowed(&st, &payload)?;
    let event = ctx.event(&store, &change_id, payload);
    store.append_event(&event)?;
    match &patchset_id {
        Some(id) => println!("audit debt declared for {id}: {reason}"),
        None => println!("audit debt declared: {reason}"),
    }
    println!("event: {}", event.event_id);
    Ok(())
}

pub struct AuditArgs {
    pub verdict: Verdict,
    pub body: Option<String>,
    pub findings_json: Option<String>,
}

/// Record a review performed after integration, discharging any debt.
pub fn audit(ctx: &Ctx, reference: &str, args: AuditArgs) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&change_id)?;
    let st = state::reduce(&store.load_events(&change_id)?)?;

    let revision = integrated_revision(&st)?;
    refuse_self_audit(ctx, &st, args.verdict)?;
    let inline = super::review::parse_inline_findings(args.findings_json.as_deref())?;
    if args.verdict == Verdict::Approved && inline.iter().any(|f| f.blocking) {
        bail!("cannot approve while recording blocking findings in the same audit");
    }
    let finding_ids: Vec<String> = inline.iter().map(|f| f.finding_id.clone()).collect();

    let payload = Payload::AuditVerdictRecorded {
        revision: revision.clone(),
        verdict: args.verdict,
        body: args.body,
        findings: inline,
    };
    ensure_append_allowed(&st, &payload)?;
    let event = ctx.event(&store, &change_id, payload);
    store.append_event(&event)?;
    println!("audit: {:?} at {revision}", args.verdict);
    for id in finding_ids {
        println!("finding: {id}");
    }
    println!("event: {}", event.event_id);
    // Said once the audit exists, and only then: an assumed authoring identity
    // is not refused, because it cannot be corrected after integration and
    // refusing would leave the debt undischargeable. But it is said out loud,
    // or audit debt would look like a way around the independence rule rather
    // than a way of carrying it.
    if args.verdict == Verdict::Approved && st.latest_patchset().is_some_and(|p| p.author_assumed())
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
    if st.audit_debt.is_some() {
        let (_, after) = ctx.load_state(&store, &change_id)?;
        if after.audit_debt_outstanding() {
            println!("audit recorded; the debt still owes an independent review");
        } else {
            println!("audit debt discharged");
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
fn refuse_self_audit(ctx: &Ctx, state: &ChangeState, verdict: Verdict) -> Result<()> {
    if verdict != Verdict::Approved {
        return Ok(());
    }
    let policy = crate::policy::load(&gitio::toplevel(&ctx.cwd)?)?;
    if !policy.policy.forbid_self_approval {
        return Ok(());
    }
    let Some(patchset) = state.latest_patchset() else {
        return Ok(());
    };
    let auditor = ctx.on_behalf_of.as_deref().unwrap_or(&ctx.actor);
    // An identity arc invented names nobody in particular, so two of them that
    // happen to differ do not show that two people acted. The same rule the
    // pre-integration guard applies, applied to the review that discharges the
    // obligation left behind.
    let auditor_assumed =
        ctx.on_behalf_of.is_none() && ctx.actor_source == crate::model::ActorSource::GitFallback;
    // Only the auditor's identity is refused, because only it can be
    // corrected: declaring yourself is a flag away. The author's identity is
    // already on the ledger and the ledger is append-only, and an audit exists
    // precisely to answer for work that shipped — refusing every audit of a
    // change snapshotted under an assumed identity would leave its debt
    // permanently undischargeable. What the audit is worth in that case is a
    // question the recorded provenance answers for a reader.
    if auditor_assumed {
        bail!(
            "arc assumed the auditing identity from git config, so this audit cannot show that \
anyone independent looked at {}.\n\
  Declare who is auditing: arc audit {} --verdict approved --actor '<reviewer>'",
            state.change_id,
            state.change_id
        );
    }
    if let Some(contributor) = patchset.contributor_match(auditor) {
        bail!(
            "{auditor} matches contributor {contributor} on the audited work, so this audit \
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
