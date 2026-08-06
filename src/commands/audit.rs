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
    // An open change waives its self-approval only for the patchset that is
    // about to ship; a closed one has no gate left to waive, so the debt is
    // recorded as a bare obligation.
    let patchset_id = if st.is_closed() {
        None
    } else {
        st.latest_patchset().map(|patchset| patchset.id.clone())
    };
    let payload = Payload::AuditDebtDeclared {
        reason: reason.to_string(),
        patchset_id: patchset_id.clone(),
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
    if st.audit_debt.is_some() {
        println!("audit debt discharged");
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
    let Some(author) = state
        .latest_patchset()
        .map(|patchset| patchset.effective_author().to_string())
    else {
        return Ok(());
    };
    let auditor = ctx.on_behalf_of.as_deref().unwrap_or(&ctx.actor);
    if auditor == author {
        bail!(
            "{auditor} authored the audited work, so this audit would discharge its \
own obligation.\n\
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
